//! CPU- or filesystem-bound work off the async threads.
//!
//! The async workers are few (one per core); a regex-heavy rendering or a
//! `statvfs` on a hung network mount parked there would stall every other
//! request. Such work runs on tokio's blocking pool instead.

/// Run `f` on a blocking worker and await its result.
pub(crate) async fn run<T, F>(f: F) -> anyhow::Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    Ok(tokio::task::spawn_blocking(f).await?)
}
