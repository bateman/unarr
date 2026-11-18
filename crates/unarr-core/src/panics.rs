//! What a panic costs: the request, the command or the task it happened in,
//! and nothing else. One process holds every personality, so a bug in one
//! must not end the others (`docs/DESIGN.md`, "Surviving a panic").
//!
//! The release profile unwinds. What is caught here, at the places a unit of
//! work starts, is turned into that unit's failure: [`catch`] around a
//! request or a command, [`supervised`] around a task that must keep running,
//! [`spawn_logged`] around one whose loss is accepted. Out of memory, a stack
//! overflow and a panic raised while another unwinds still end the process.

use std::any::Any;
use std::cell::RefCell;
use std::future::Future;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::task::Poll;

thread_local! {
    /// Where the last panic of this thread was raised, from the hook: the
    /// payload a catcher receives holds the message only.
    static LOCATION: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// A panic that was caught.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Panicked {
    pub message: String,
    /// `file:line:column`, when the hook of [`install_hook`] saw it.
    pub location: Option<String>,
}

impl Panicked {
    fn from_payload(payload: Box<dyn Any + Send>) -> Panicked {
        let message = match payload.downcast::<String>() {
            Ok(text) => *text,
            Err(payload) => match payload.downcast::<&'static str>() {
                Ok(text) => (*text).to_owned(),
                Err(_) => "a panic with no message".to_owned(),
            },
        };
        Panicked {
            message,
            location: LOCATION.with(|l| l.borrow_mut().take()),
        }
    }
}

impl std::fmt::Display for Panicked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.location {
            Some(location) => write!(f, "panicked at {location}: {}", self.message),
            None => write!(f, "panicked: {}", self.message),
        }
    }
}

impl std::error::Error for Panicked {}

/// Keep where each panic was raised for the catcher of its thread, then let
/// the hook that was there write its line to the standard error, as before.
///
/// The hook itself logs nothing through `tracing`: a panic raised inside a
/// log layer would find the layer's lock held by its own thread. The
/// catcher logs, once the stack is unwound.
pub fn install_hook() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let location = info.location().map(|l| l.to_string());
            // Not while the thread is being torn down: nothing reads it then.
            let _ = LOCATION.try_with(|l| *l.borrow_mut() = location);
            previous(info);
        }));
    });
}

/// Run `f`; a panic inside it is its error.
pub fn catch_sync<T>(f: impl FnOnce() -> T) -> Result<T, Panicked> {
    catch_unwind(AssertUnwindSafe(f)).map_err(Panicked::from_payload)
}

/// Await `future`; a panic in any of its polls is its error. The future is
/// dropped where it stopped: what it held is released as the stack unwound,
/// and what it shares with others is theirs to find in order (see
/// [`crate::sync`] for the locks).
pub async fn catch<F: Future>(future: F) -> Result<F::Output, Panicked> {
    let mut future = std::pin::pin!(future);
    std::future::poll_fn(
        move |cx| match catch_unwind(AssertUnwindSafe(|| future.as_mut().poll(cx))) {
            Ok(Poll::Pending) => Poll::Pending,
            Ok(Poll::Ready(value)) => Poll::Ready(Ok(value)),
            Err(payload) => Poll::Ready(Err(Panicked::from_payload(payload))),
        },
    )
    .await
}

/// How long a supervised task waits before it starts again: a panic that
/// repeats at once must not spin a core.
#[cfg(not(test))]
const RESTART_DELAY: std::time::Duration = std::time::Duration::from_secs(1);
#[cfg(test)]
const RESTART_DELAY: std::time::Duration = std::time::Duration::from_millis(5);

/// Spawn a task that must keep running: when it panics, the panic is logged
/// and the task made again by `make` after a moment. It ends when the
/// future it runs ends by itself.
pub fn supervised<F, Fut>(name: &'static str, mut make: F) -> tokio::task::JoinHandle<()>
where
    F: FnMut() -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    tokio::spawn(async move {
        loop {
            match catch(make()).await {
                Ok(()) => return,
                Err(panic) => {
                    tracing::error!("The {name} {panic}; it starts again");
                    tokio::time::sleep(RESTART_DELAY).await;
                }
            }
        }
    })
}

/// Spawn a task whose loss is accepted: a panic ends it, and is logged
/// where the personality logs rather than on the standard error alone.
pub fn spawn_logged<Fut>(name: &'static str, future: Fut) -> tokio::task::JoinHandle<()>
where
    Fut: Future<Output = ()> + Send + 'static,
{
    tokio::spawn(async move {
        if let Err(panic) = catch(future).await {
            tracing::error!("The {name} {panic}; it is not run again");
        }
    })
}

/// Panics on purpose, for the tests of all the above. It exists in the
/// crate's own tests and in a build with the `panic-switch` feature, which
/// no published binary has: without the feature there is no such code, no
/// route to it and nothing to set that would bring it back.
#[cfg(any(test, feature = "panic-switch"))]
pub mod switch {
    use crate::sync::Recover;
    use std::sync::Mutex;

    /// The places armed to panic, each for its next visit.
    static ARMED: Mutex<Vec<String>> = Mutex::new(Vec::new());

    /// The next [`fire`] named `place` panics.
    pub fn arm(place: &str) {
        ARMED.lock().recover().push(place.to_owned());
    }

    /// Panic if `place` was armed; once for each time it was.
    pub fn fire(place: &str) {
        let armed = {
            let mut armed = ARMED.lock().recover();
            armed.iter().position(|p| p == place).map(|at| armed.remove(at))
        };
        if armed.is_some() {
            panic!("the panic switch was set for {place}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    #[test]
    fn a_caught_panic_has_its_message_and_its_place() {
        install_hook();
        let caught = catch_sync(|| panic!("the {} went wrong", "parser")).unwrap_err();
        assert_eq!(caught.message, "the parser went wrong");
        let location = caught.location.clone().expect("the hook saw it");
        assert!(location.contains("panics.rs:"), "{location}");
        assert_eq!(
            caught.to_string(),
            format!("panicked at {location}: the parser went wrong")
        );
        // A literal message and a payload that is no text.
        assert_eq!(catch_sync(|| panic!("plain")).unwrap_err().message, "plain");
        let odd = catch_sync(|| std::panic::panic_any(7_u8)).unwrap_err();
        assert_eq!(odd.message, "a panic with no message");
        assert_eq!(catch_sync(|| 3), Ok(3));
    }

    #[tokio::test]
    async fn a_future_that_panics_after_a_wait_is_an_error() {
        install_hook();
        let caught = catch(async {
            tokio::task::yield_now().await;
            let empty: Vec<u8> = Vec::new();
            empty.first().copied().expect("nothing there")
        })
        .await
        .unwrap_err();
        assert_eq!(caught.message, "nothing there");
        assert_eq!(catch(async { 5 }).await, Ok(5));
    }

    #[tokio::test]
    async fn a_supervised_task_starts_again_and_ends_by_itself() {
        install_hook();
        let runs = Arc::new(AtomicUsize::new(0));
        let counted = runs.clone();
        supervised("test task", move || {
            let runs = counted.clone();
            async move {
                if runs.fetch_add(1, Ordering::SeqCst) < 2 {
                    panic!("again");
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(runs.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn a_logged_task_ends_without_failing_its_handle() {
        install_hook();
        assert!(spawn_logged("test task", async { panic!("once") }).await.is_ok());
    }
}
