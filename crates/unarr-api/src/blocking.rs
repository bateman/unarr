//! CPU- or filesystem-bound work off the async threads.
//!
//! The async workers are few (one per core); a regex-heavy rendering or a
//! `statvfs` on a hung network mount parked there would stall every other
//! request. Such work runs on tokio's blocking pool instead.

/// Run `f` on a blocking worker and await its result. A panic in `f` is the
/// caller's error, with the message and the place of the panic: the request
/// that asked answers 500 instead of waiting for nothing.
pub(crate) async fn run<T, F>(f: F) -> anyhow::Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let done = tokio::task::spawn_blocking(move || unarr_core::panics::catch_sync(f)).await?;
    done.map_err(anyhow::Error::new)
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn a_panic_on_the_blocking_pool_is_the_callers_error() {
        unarr_core::panics::install_hook();
        let failed = super::run(|| -> u8 { panic!("rendering went wrong") }).await;
        let error = failed.unwrap_err().to_string();
        assert!(
            error.starts_with("panicked at ") && error.ends_with("rendering went wrong"),
            "{error}"
        );
        assert_eq!(super::run(|| 7).await.unwrap(), 7);
    }
}
