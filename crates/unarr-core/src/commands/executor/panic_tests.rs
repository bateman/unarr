//! A command that panics ends failed and takes nothing with it: the worker
//! that ran it takes the next one.

use std::sync::Arc;

use chrono::Utc;

use super::Runtime;
use crate::commands::{CommandBody, CommandModel, CommandPriority, CommandStatus, CommandTrigger, Event};
use crate::db::Db;
use crate::downloadclients::DownloadClientService;
use crate::downloads::Downloads;
use crate::panics::switch;

async fn started() -> Arc<Runtime> {
    crate::panics::install_hook();
    let db = Db::open_in_memory().unwrap();
    let http = crate::http::build(std::time::Duration::from_secs(10));
    let downloads = Arc::new(Downloads::new(
        db.clone(),
        DownloadClientService::with_client(db.clone(), http, crate::downloadclients::SONARR),
    ));
    let runtime = Runtime::new(db, downloads, Arc::new(Utc::now), Default::default())
        .await
        .unwrap();
    runtime.start();
    runtime
}

/// Queue a command as the API does, wait for it to end, and read it back.
async fn run(runtime: &Runtime, body: CommandBody) -> CommandModel {
    let pushed = runtime
        .queue()
        .push_with(
            body,
            CommandPriority::Normal,
            CommandTrigger::Manual,
            runtime.now(),
            |m| m.send_updates_to_client = true,
        )
        .await
        .unwrap();
    runtime.queue().wait_idle().await;
    runtime.queue().get(pushed.id).await.unwrap().unwrap()
}

/// The states of command `id` the hub is told, up to its end. The end is
/// broadcast after the queue has it, so it is waited for.
async fn told(events: &mut tokio::sync::broadcast::Receiver<Event>, id: i64) -> Vec<CommandStatus> {
    let mut states = Vec::new();
    let read = async {
        loop {
            if let Ok(Event::CommandUpdated(c)) = events.recv().await {
                if c.id == id {
                    states.push(c.status);
                    if c.ended_at.is_some() {
                        break;
                    }
                }
            }
        }
    };
    tokio::time::timeout(std::time::Duration::from_secs(10), read)
        .await
        .expect("the hub is told how the command ended");
    states
}

// One test: the switch is the process's, and two tests arming the same
// command would take each other's panic.
#[tokio::test]
async fn a_command_that_panics_fails_and_the_next_one_runs() {
    let runtime = started().await;
    let mut events = runtime.subscribe();

    // In the command's body, once for each worker and once more: none of
    // them is lost.
    for _ in 0..super::WORKERS + 1 {
        switch::arm("command:MessagingCleanup");
        let failed = run(&runtime, CommandBody::MessagingCleanup).await;
        assert_eq!(failed.status, CommandStatus::Failed);
        assert_eq!(failed.message.as_deref(), Some("Failed"));
        let exception = failed.exception.clone().unwrap();
        assert!(
            exception.starts_with("panicked at ")
                && exception.ends_with("the panic switch was set for command:MessagingCleanup"),
            "{exception}"
        );
        assert!(failed.ended_at.is_some());
        assert_eq!(
            told(&mut events, failed.id).await,
            [CommandStatus::Started, CommandStatus::Failed]
        );
        // As stored: a start that finds it does not take it for a command
        // a crash interrupted.
        let stored: i64 = runtime
            .queue()
            .db()
            .call(move |conn| {
                Ok(
                    conn.query_row("SELECT status FROM commands WHERE id = ?1", [failed.id], |r| {
                        r.get(0)
                    })?,
                )
            })
            .await
            .unwrap();
        assert_eq!(stored, CommandStatus::Failed.as_i64());
    }
    let done = run(&runtime, CommandBody::MessagingCleanup).await;
    assert_eq!(done.status, CommandStatus::Completed);

    // Around the body, in the executor's own bookkeeping: the worker ends
    // the command it had started, and starts again.
    for _ in 0..super::WORKERS + 1 {
        switch::arm("record:MessagingCleanup");
        let failed = run(&runtime, CommandBody::MessagingCleanup).await;
        assert_eq!(failed.status, CommandStatus::Failed);
        assert!(
            failed.exception.as_deref().unwrap().starts_with("panicked at "),
            "{failed:?}"
        );
        assert_eq!(told(&mut events, failed.id).await, [CommandStatus::Failed]);
    }
    let done = run(&runtime, CommandBody::MessagingCleanup).await;
    assert_eq!(done.status, CommandStatus::Completed);
    assert!(!runtime.queue().has_running());
}
