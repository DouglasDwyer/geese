use crate::*;

#[cfg(not(target_arch = "wasm32"))]
use std::thread;
#[cfg(target_arch = "wasm32")]
use wasm_thread as thread;

/// A threadpool which executes Geese work on both the main thread and background
/// threads. This threadpool controls the lifetime of its background threads, and they
/// are cancelled upon drop.
#[derive(Debug)]
pub struct HardwareThreadPool {
    /// The inner threadpool information.
    inner: Arc<HardwareThreadPoolInner>,
}

impl HardwareThreadPool {
    /// Creates a new hardware threadpool and spawns the specified number of background threads. If `0` is specified,
    /// then this acts as a single-threaded (main thread only) threadpool.
    #[inline(always)]
    pub fn new(background_threads: usize) -> Self {
        let inner = Arc::new(HardwareThreadPoolInner {
            handle_count: AtomicUsize::new(1),
            ..Default::default()
        });
        Self::spawn_workers(&inner, background_threads);
        Self { inner }
    }

    /// Spawns background worker threads which repeatedly join on the inner context until it is destroyed.
    #[inline(always)]
    fn spawn_workers(inner: &Arc<HardwareThreadPoolInner>, background_threads: usize) {
        for _ in 0..background_threads {
            let inner_clone = inner.clone();
            thread::spawn(move || inner_clone.run());
        }
    }
}

impl Default for HardwareThreadPool {
    #[inline(always)]
    fn default() -> Self {
        Self::new(0)
    }
}

impl Drop for HardwareThreadPool {
    #[inline(always)]
    fn drop(&mut self) {
        self.inner.decrement_counter();
    }
}

impl GeeseThreadPool for HardwareThreadPool {
    fn set_callback(&self, callback: Option<Arc<dyn Fn() + Send + Sync>>) {
        self.inner.set_callback(callback);
    }
}

/// Stores the inner synchronization state for a threadpool.
#[derive(Default)]
struct HardwareThreadPoolInner {
    /// The callback that available threads should invoke.
    callback: wasm_sync::Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    /// The number of extant threadpool handles. When this number reaches zero, worker threads cancel themselves.
    handle_count: AtomicUsize,
    /// A condition variable that is signaled when the callback or handle count changes.
    on_changed: wasm_sync::Condvar,
}

impl HardwareThreadPoolInner {
    /// Joins this threadpool, attempting to complete any available work or waiting until available work changes.
    #[inline(always)]
    pub fn join(&self) {
        let guard = self
            .callback
            .lock()
            .expect("Could not acquire callback lock.");
        if let Some(callback) = &*guard {
            let to_run = callback.clone();
            drop(guard);
            to_run();
        } else {
            drop(self.on_changed.wait(guard));
        }
    }

    /// Dedicates the caller thread to this threadpool, repeatedly joining with the pool until it is destroyed.
    #[inline(always)]
    pub fn run(&self) {
        while self.handle_count.load(Ordering::Acquire) > 0 {
            self.join();
        }
    }

    /// Sets the work callback and notifies all waiting threads.
    #[inline(always)]
    pub fn set_callback(&self, callback: Option<Arc<dyn Fn() + Send + Sync>>) {
        *self
            .callback
            .lock()
            .expect("Could not acquire callback lock.") = callback;
        self.on_changed.notify_all();
    }

    /// Decrements the handle counter and notifies all waiting threads.
    #[inline(always)]
    pub fn decrement_counter(&self) {
        self.handle_count.fetch_sub(1, Ordering::Release);
        self.on_changed.notify_all();
    }
}

impl std::fmt::Debug for HardwareThreadPoolInner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HardwareThreadPoolInner")
            .field("handle_count", &self.handle_count)
            .field("on_changed", &self.on_changed)
            .finish()
    }
}
