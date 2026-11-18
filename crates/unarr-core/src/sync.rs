//! Locks after a panic.
//!
//! A thread that panics while it holds a `std::sync` lock poisons it, and
//! every later `lock().unwrap()` panics in turn: one failed request would
//! become every request failing until the restart. Unarr's locks guard
//! caches, queues and settings whose critical sections replace or copy a
//! value and call nothing that fails in between, so what a panic leaves
//! behind is a value as good as before or after the write. They are
//! recovered, not treated as fatal (`docs/DESIGN.md`, "Surviving a panic",
//! lists them).

use std::sync::{LockResult, PoisonError};

/// The guard, whether or not a holder panicked.
pub trait Recover<G> {
    fn recover(self) -> G;
}

impl<G> Recover<G> for LockResult<G> {
    fn recover(self) -> G {
        self.unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::Recover;
    use std::sync::{Arc, Mutex, RwLock};

    #[test]
    fn a_poisoned_lock_gives_its_value() {
        let mutex = Arc::new(Mutex::new(1));
        let lock = Arc::new(RwLock::new(2));
        let (m, l) = (mutex.clone(), lock.clone());
        let _ = std::thread::spawn(move || {
            let _m = m.lock().recover();
            let _l = l.write().recover();
            panic!("while holding both");
        })
        .join();
        assert!(mutex.is_poisoned() && lock.is_poisoned());
        assert_eq!(*mutex.lock().recover(), 1);
        assert_eq!(*lock.read().recover(), 2);
        *lock.write().recover() = 3;
        assert_eq!(*lock.read().recover(), 3);
    }
}
