//! A value one thread hands to others once it has it.

use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

pub struct Promise<T>(Arc<(Mutex<Option<T>>, Condvar)>);

impl<T> Clone for Promise<T> {
    fn clone(&self) -> Self {
        Promise(self.0.clone())
    }
}

impl<T: Clone> Promise<T> {
    pub fn new() -> Promise<T> {
        Promise(Arc::new((Mutex::new(None), Condvar::new())))
    }

    pub fn set(&self, value: T) {
        *self.0 .0.lock().unwrap() = Some(value);
        self.0 .1.notify_all();
    }

    /// Blocks until the value is there; `None` once `limit` has passed without it.
    pub fn wait(&self, limit: Duration) -> Option<T> {
        let deadline = Instant::now() + limit;
        let mut state = self.0 .0.lock().unwrap();
        while state.is_none() {
            let left = deadline.checked_duration_since(Instant::now())?;
            state = self.0 .1.wait_timeout(state, left).unwrap().0;
        }
        state.clone()
    }
}
