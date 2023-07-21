use crate::*;

#[cfg(not(target_arch = "wasm32"))]
use std::thread as thread;
#[cfg(target_arch = "wasm32")]
use wasm_thread as thread;

#[derive(Debug)]
pub struct HardwareThreadPool {
    inner: Arc<HardwareThreadPoolInner>
}

impl HardwareThreadPool {
    #[inline(always)]
    pub fn new(background_threads: usize) -> Self {
        let inner = Arc::new(HardwareThreadPoolInner { handle_count: AtomicUsize::new(1), ..Default::default() });
        Self::spawn_workers(&inner, background_threads);
        Self { inner }
    }

    #[inline(always)]
    fn spawn_workers(inner: &Arc<HardwareThreadPoolInner>, background_threads: usize) {
        for i in 0..background_threads {
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
    fn join(&self) {
        self.inner.join();
    }

    fn set_callback(&self, callback: Option<Arc<dyn Fn() + Send + Sync>>) {
        self.inner.set_callback(callback);
    }
}

#[derive(Default)]
struct HardwareThreadPoolInner {
    callback: wasm_sync::Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    handle_count: AtomicUsize,
    on_changed: wasm_sync::Condvar
}

impl HardwareThreadPoolInner {
    #[inline(always)]
    pub fn join(&self) {
        let guard = self.callback.lock().expect("Could not acquire callback lock.");
        if let Some(callback) = &*guard {
            let to_run = callback.clone();
            drop(callback);
            drop(guard);
            to_run();
        }
        else {
            self.on_changed.wait(guard);
        }
    }

    #[inline(always)]
    pub fn run(&self) {
        while self.handle_count.load(Ordering::Acquire) > 0 {
            self.join();
        }
    }

    #[inline(always)]
    pub fn set_callback(&self, callback: Option<Arc<dyn Fn() + Send + Sync>>) {
        *self.callback.lock().expect("Could not acquire callback lock.") = callback;
        self.on_changed.notify_all();
    }

    #[inline(always)]
    pub fn increment_counter(&self) {
        self.handle_count.fetch_add(1, Ordering::Release);
    }

    #[inline(always)]
    pub fn decrement_counter(&self) {
        self.handle_count.fetch_sub(1, Ordering::Release);
        self.on_changed.notify_all();
    }
}

impl std::fmt::Debug for HardwareThreadPoolInner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HardwareThreadPoolInner").field("handle_count", &self.handle_count).field("on_changed", &self.on_changed).finish()
    }
}