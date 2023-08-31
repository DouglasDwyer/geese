#![cfg_attr(unstable, feature(const_format_args))]
#![cfg_attr(unstable, feature(const_type_id))]

//! Geese is a game event system for Rust, built to allow modular game engine design.
//!
//! In Geese, a system is a struct with internal state and a collection of associated
//! event handlers. Systems can raise events and react to events raised by other
//! systems. Systems may also declare dependencies on other systems, which allow
//! them to borrow those systems during event processing. Geese automatically
//! loads all system dependencies. Any struct can act as an event type, and any struct
//! that implements [`GeeseSystem`](crate::GeeseSystem) can act as a system type.
//!
//! The following is an example of how to use Geese to load multiple dependent systems,
//! and propogate events between them. The example creates a Geese context,
//! and requests that system `B` be loaded. When [`flush`](crate::GeeseContext::flush) is called,
//! system `A` is loaded first (because it is a dependency of `B`), and then
//! system `B` is loaded. `B` receives the typed event, and responds by querying
//! system `A` for some information.
//!
//! ```rust
//! # use geese::*;
//! # use std::sync::*;
//! # use std::sync::atomic::*;
//! #
//! struct A;
//!
//! impl A {
//!     pub fn answer(&self) -> bool {
//!         true
//!     }
//! }
//!
//! impl GeeseSystem for A {
//!     fn new(_: GeeseContextHandle<Self>) -> Self {
//!         Self
//!     }
//! }
//!
//! struct B {
//!     ctx: GeeseContextHandle<Self>
//! }
//!
//! impl B {
//!     fn test_answer(&mut self, event: &Arc<AtomicBool>) {
//!         event.store(self.ctx.get::<A>().answer(), Ordering::Relaxed);
//!     }
//! }
//!
//! impl GeeseSystem for B {
//!     const DEPENDENCIES: Dependencies = dependencies()
//!         .with::<A>();
//!
//!     const EVENT_HANDLERS: EventHandlers<Self> = event_handlers()
//!         .with(Self::test_answer);
//!
//!     fn new(ctx: GeeseContextHandle<Self>) -> Self {
//!         Self { ctx }
//!     }
//! }
//!
//! let ab = Arc::new(AtomicBool::new(false));
//! let mut ctx = GeeseContext::default();
//! ctx.flush()
//!     .with(notify::add_system::<B>())
//!     .with(ab.clone());
//! assert!(ab.load(Ordering::Relaxed));
//! ```
//!
//! A working game of Pong using `geese` can be found in [the examples folder](/examples/).
//! 
//! ### Event processing
//!
//! The following invariants are always upheld during event processing, making it easy to reason about order of execution:
//!
//! - If multiple events are raised, they are processed in first-in-first-out (FIFO) order. The `notify::flush` command
//! can be used for fine-grained control over ordering by starting embedded event cycles.
//! - Multiple handlers for the same event on the same system are invoked in the order that they appear in the handlers list.
//! - When processing a single event, dependencies' event handlers are always invoked before those of dependents.
//!
//! ### Concurrency
//!
//! Geese can use multithreading to parallelize over work, allowing independent systems to execute event handlers in tandem.
//! Even during multithreading, all invariants of event processing are upheld - from the perspective of a single system, events still
//! execute serially. The more systems one defines, the more parallelism is achieved.
//!
//! To use Geese with multithreading, employ either the builtin [`HardwareThreadPool`](crate::HardwareThreadPool) or implement a custom threadpool with the [`GeeseThreadPool`](crate::GeeseThreadPool) trait.

#![deny(warnings)]
#![warn(missing_docs)]
#![warn(clippy::missing_docs_in_private_items)]

/// Provides the ability to generate and compare type IDs in a `const` context.
#[cfg_attr(unstable, path = "const_type_id/compiled.rs")]
#[cfg_attr(not(unstable), path = "const_type_id/runtime.rs")]
mod const_type_id;

/// Carries out event processing tasks.
mod event_manager;

/// Declares cell types that may be used for lock-free cross-thread resource sharing.
mod rw_cell;

/// Provides methods for evaluating `const` code with generics at compilation and at runtime.
mod static_eval;

/// Offers a means for easily sharing data between systems.
mod store;

/// Implements a threadpool for context multitasking.
mod thread_pool;

/// Defines the core traits used to create Geese systems.
mod traits;

use crate::event_manager::*;
use crate::rw_cell::*;
use crate::static_eval::*;
pub use crate::store::*;
pub use crate::thread_pool::*;
pub use crate::traits::*;
use bitvec::access::*;
use bitvec::prelude::*;
use const_list::*;
use fxhash::*;
use smallvec::*;
use std::any::*;
use std::cell::*;
use std::collections::hash_map::*;
use std::hash::*;
use std::marker::*;
use std::mem::*;
use std::ops::*;
use std::pin::*;
use std::sync::atomic::*;
use std::sync::*;
use topological_sort::*;

/// Represents a system-specific handle to a Geese context.
#[allow(unused_variables)]
pub struct GeeseContextHandle<S: GeeseSystem> {
    /// The handle data used to access the Geese context.
    inner: Arc<ContextHandleInner>,
    /// Marks the system argument as being used.
    data: PhantomData<fn(S)>,
}

impl<S: GeeseSystem> GeeseContextHandle<S> {
    /// Creates a new handle from an inner context reference.
    #[inline(always)]
    fn new(inner: Arc<ContextHandleInner>) -> Self {
        Self {
            inner,
            data: PhantomData,
        }
    }

    /// Raises the specified dynamically-typed event.
    #[inline(always)]
    pub fn raise_event_boxed(&self, event: Box<dyn Any + Send + Sync>) {
        unsafe {
            drop(
                self.inner
                    .event_sender
                    .lock()
                    .unwrap_unchecked()
                    .send(Event::new(event)),
            );
        }
    }

    /// Raises the specified event.
    #[inline(always)]
    pub fn raise_event<T: 'static + Send + Sync>(&self, event: T) {
        self.raise_event_boxed(Box::new(event));
    }

    /// Obtains the specified system dependency.
    #[inline(always)]
    pub fn get<T: GeeseSystem>(&self) -> SystemRef<T> {
        unsafe {
            let index = static_eval!(
                if let Some(index) = S::DEPENDENCIES.index_of::<T>() {
                    index
                } else {
                    GeeseContextHandle::<S>::panic_on_invalid_dependency()
                },
                usize,
                S,
                T
            );
            let ctx = (*self.inner.context).borrow();
            let global_index = self.inner.dependency_id(index as u16) as usize;
            assert!(
                *ctx.sync_systems.get_unchecked(global_index)
                    || ctx.owning_thread == std::thread::current().id(),
                "Attempted a cross-thread borrow of a system that did not implement Sync."
            );
            let guard = ctx
                .systems
                .get_unchecked(global_index)
                .value
                .borrow()
                .detach();
            SystemRef::new(RwCellGuard::map(guard, |system| {
                transmute::<_, &(&T, *const ())>(system).0
            }))
        }
    }

    /// Mutably obtains the specified system dependency.
    #[inline(always)]
    pub fn get_mut<T: GeeseSystem>(&mut self) -> SystemRefMut<T> {
        unsafe {
            let index = static_eval!(
                {
                    if let Some(index) = S::DEPENDENCIES.index_of::<T>() {
                        assert!(
                            const_unwrap(S::DEPENDENCIES.as_inner().get(index)).mutable(),
                            "Attempted to mutably access an immutable dependency."
                        );
                        index
                    } else {
                        GeeseContextHandle::<S>::panic_on_invalid_dependency()
                    }
                },
                usize,
                S,
                T
            );
            let ctx = (*self.inner.context).borrow();
            let global_index = self.inner.dependency_id(index as u16) as usize;
            assert!(
                *ctx.sync_systems.get_unchecked(global_index)
                    || ctx.owning_thread == std::thread::current().id(),
                "Attempted a cross-thread borrow of a system that did not implement Sync."
            );
            let guard = ctx
                .systems
                .get_unchecked(global_index)
                .value
                .borrow_mut()
                .detach();
            SystemRefMut::new(RwCellGuardMut::map(guard, |system| {
                transmute::<_, &mut (&mut T, *const ())>(system).0
            }))
        }
    }

    /// Panics when the user attempts to reference an undeclared dependency.
    #[inline(always)]
    const fn panic_on_invalid_dependency() -> ! {
        panic!("The specified system was not a dependency of this one.");
    }
}

impl<S: GeeseSystem> std::fmt::Debug for GeeseContextHandle<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GeeseContextHandle")
            .field("type", &type_name::<S>())
            .finish()
    }
}

unsafe impl<S: GeeseSystem> Send for GeeseContextHandle<S> {}
unsafe impl<S: GeeseSystem> Sync for GeeseContextHandle<S> {}

/// Stores the inner data about a context handle.
struct ContextHandleInner {
    /// The context with which this handle is associated.
    context: *const RwCell<ContextInner>,
    /// A mapping from local to global IDs of system dependencies.
    dependency_ids: SmallVec<[Cell<u16>; Self::DEFAULT_DEPENDENCY_BUFFER_SIZE]>,
    /// A sender that may be used to raise events within the context.
    event_sender: wasm_sync::Mutex<std::sync::mpsc::Sender<Event>>,
    /// The current ID of this system.
    id: Cell<u16>,
}

impl ContextHandleInner {
    /// The default size of the dependency ID buffer.
    const DEFAULT_DEPENDENCY_BUFFER_SIZE: usize = 4;

    /// Gets the current ID of this context handle.
    ///
    /// # Safety
    ///
    /// This function may only be invoked by while the ID is not being edited.
    #[inline(always)]
    unsafe fn id(&self) -> u16 {
        self.id.get()
    }

    /// Sets the current ID of this context handle.
    ///
    /// # Safety
    ///
    /// This function may only be invoked by one thread at a time.
    #[inline(always)]
    unsafe fn set_id(&self, value: u16) {
        self.id.set(value);
    }

    /// Gets the number of dependencies that this system has.
    ///
    /// # Safety
    ///
    /// This function may only be invoked by while the dependency IDs are not being edited.
    #[inline(always)]
    unsafe fn dependency_len(&self) -> u16 {
        self.dependency_ids.len() as u16
    }

    /// Gets the global ID of the dependency with the provided local index.
    ///
    /// # Safety
    ///
    /// This function may only be invoked by while the dependency IDs are not being edited.
    /// The index must be less than the total number of system dependencies.
    #[inline(always)]
    unsafe fn dependency_id(&self, index: u16) -> u16 {
        self.dependency_ids.get_unchecked(index as usize).get()
    }

    /// Sets the global ID of the dependency with the provided local index.
    ///
    /// # Safety
    ///
    /// This function may only be invoked by one thread at a time.
    /// The index must be less than the total number of system dependencies.
    #[inline(always)]
    unsafe fn set_dependency_id(&self, index: u16, value: u16) {
        *self.dependency_ids.get_unchecked(index as usize).as_ptr() = value;
    }
}

/// Represents a collection of systems that can create and respond to events.
pub struct GeeseContext(Pin<Box<RwCell<ContextInner>>>);

impl GeeseContext {
    /// Creates a new context that uses the given threadpool to complete event cycles.
    #[inline(always)]
    pub fn with_threadpool(pool: impl GeeseThreadPool) -> Self {
        Self(Box::pin(RwCell::new(ContextInner::with_threadpool(pool))))
    }

    /// Specifies the threadpool that the context will use when completing event cycles.
    #[inline(always)]
    pub fn set_threadpool(&mut self, pool: impl GeeseThreadPool) {
        self.0.borrow_mut().thread_pool = Arc::new(pool);
    }

    /// Causes an event cycle to complete. Returns a builder to which events can be added,
    /// all of which are processed (along with any spawned subevents) when the builder is dropped.
    #[inline(always)]
    pub fn flush(&mut self) -> impl '_ + EventQueue {
        ContextEventQueue {
            ctx: self,
            queue: EventBuffer::default(),
        }
    }

    /// Obtains a reference to the given system.
    #[inline(always)]
    pub fn get<S: GeeseSystem>(&self) -> SystemRef<S> {
        unsafe {
            let inner = self.0.borrow();
            let index = inner
                .system_initializers
                .get(&TypeId::of::<S>())
                .expect("System not found.")
                .id();
            let guard = inner
                .systems
                .get_unchecked(index as usize)
                .value
                .borrow()
                .detach();
            SystemRef::new(RwCellGuard::map(guard, |system| {
                transmute::<_, &(&S, *const ())>(system).0
            }))
        }
    }

    /// Mutably obtains a reference to the given system.
    #[inline(always)]
    pub fn get_mut<S: GeeseSystem>(&mut self) -> SystemRefMut<S> {
        unsafe {
            let inner = self.0.borrow();
            let index = inner
                .system_initializers
                .get(&TypeId::of::<S>())
                .expect("System not found.")
                .id();
            let guard = inner
                .systems
                .get_unchecked(index as usize)
                .value
                .borrow_mut()
                .detach();
            SystemRefMut::new(RwCellGuardMut::map(guard, |system| {
                transmute::<_, &mut (&mut S, *const ())>(system).0
            }))
        }
    }

    /// Causes an event cycle to complete by running systems until the specified event queue and
    /// all follow-up events have been processed.
    #[inline(always)]
    fn flush_buffer(&mut self, buffer: EventBuffer) {
        unsafe {
            let inner = self.0.borrow();

            for event in buffer.events {
                drop(inner.event_sender.send(Event::new(event)));
            }

            let pool = inner.thread_pool.clone();
            let inner_mgr = inner.event_manager.get();
            (*inner_mgr).push_external_event_cycle();
            (*inner_mgr).gather_external_events();
            drop(inner);

            let ctx_ptr = &*self.0 as *const RwCell<ContextInner> as usize;

            let mgr = Arc::new(EventManagerWrapper(
                wasm_sync::Mutex::new(inner_mgr),
                wasm_sync::Condvar::new(),
            ));
            let mgr_clone = mgr.clone();
            let callback = Arc::new_cyclic(move |weak| {
                let weak_clone = weak.clone() as Weak<dyn Fn() + Send + Sync>;
                move || {
                    Self::process_events(
                        ctx_ptr as *const _,
                        &mgr_clone.1,
                        &mgr_clone.0,
                        &weak_clone,
                    )
                }
            });

            let mut lock_guard = MaybeUninit::new(mgr.0.lock().unwrap_unchecked());
            loop {
                pool.set_callback(Some(callback.clone()));
                while let Some(event_mgr) = lock_guard.assume_init_mut().as_mut() {
                    let guard = (*(ctx_ptr as *const RwCell<ContextInner>)).borrow();
                    match event_mgr.next_job(&guard) {
                        Ok(to_run) => {
                            lock_guard.assume_init_drop();

                            to_run.execute(&guard);
                            (***lock_guard.write(mgr.0.lock().unwrap_unchecked()))
                                .complete_job(&to_run);
                            guard.thread_pool.set_callback(Some(callback.clone()));
                        }
                        Err(EventJobError::Complete) => {
                            **lock_guard.assume_init_mut() = std::ptr::null_mut();
                            guard.thread_pool.set_callback(None);
                            break;
                        }
                        _ => {
                            guard.thread_pool.set_callback(None);
                            lock_guard.write(
                                mgr.1.wait(lock_guard.assume_init_read()).unwrap_unchecked(),
                            );
                        }
                    }
                }

                let state = (*inner_mgr).update_state();

                match state {
                    EventManagerState::Complete() => break,
                    EventManagerState::CycleClogged(event) => {
                        self.run_clogged_event(event, inner_mgr);
                        (*inner_mgr).gather_external_events();
                    }
                };

                **lock_guard.assume_init_mut() = inner_mgr;
            }
            lock_guard.assume_init_drop();
        }
    }

    /// Adds a system to the context.
    fn add_system<S: GeeseSystem>(&mut self) {
        unsafe {
            static_eval!(
                assert!(
                    !has_duplicate_dependencies(&S::DEPENDENCIES),
                    "System had duplicate dependencies."
                ),
                (),
                S
            );
            let mut inner = self.0.borrow_mut();
            if let Some(value) = inner.system_initializers.get(&TypeId::of::<S>()) {
                assert!(!value.top_level(), "Cannot add duplicate dependencies.");
                value.set_top_level(true);
            } else {
                inner.add_new_system::<S>();
                let initializers = take(&mut inner.system_initializers);
                let to_initialize = inner.instantiate_added_systems(&initializers, self.as_ptr());
                drop(inner);
                self.initialize_systems(to_initialize.into_iter());
                self.0.borrow_mut().system_initializers = initializers;
            }
        }
    }

    /// Removes a system from the context, unloading it if it becomes disconnected from the dependency graph.
    fn remove_system<S: GeeseSystem>(&mut self) {
        unsafe {
            let mut inner = self.0.borrow_mut();
            inner.remove_top_level_system::<S>();
            let connected = inner.determine_connected_systems();
            if connected.first_zero().is_some() {
                let mut initializers = take(&mut inner.system_initializers);
                drop(inner);
                self.drop_systems(&connected, &mut initializers);
                let mut inner = self.0.borrow_mut();
                inner.system_initializers = initializers;
                inner.compact_remaining_systems(&connected);
            }
        }
    }

    /// Reloads a system and all systems that depend upon it.
    fn reset_system<S: GeeseSystem>(&mut self) {
        self.0.borrow().reset_system::<S>();
    }

    /// Initializes all systems in the given iterator.
    ///
    /// # Safety
    ///
    /// The systems in the iterator must have valid IDs referring to systems in the inner context.
    #[inline(always)]
    unsafe fn initialize_systems<'a>(&self, systems: impl Iterator<Item = &'a SystemInitializer>) {
        let inner = self.0.borrow();
        for system in systems {
            let holder = inner.system_holder(system.id() as usize);
            holder
                .value
                .borrow_mut()
                .write(system.descriptor.create(holder.handle.clone()));
        }
    }

    /// Drops all systems in the inner context which do not exist in the bitmap.
    ///
    /// # Safety
    ///
    /// For this function call to be sound, all of the zeroed bits in the systems map must correspond
    /// to valid, initialized systems in the inner context.
    #[inline(always)]
    unsafe fn drop_systems(
        &self,
        systems: &BitVec,
        initializers: &mut FxHashMap<TypeId, SystemInitializer>,
    ) {
        let inner = self.0.borrow();
        for system in systems.iter_zeros().rev() {
            let holder = inner.system_holder(system);
            holder.drop();
            initializers.remove(&holder.system_id);
        }
    }

    /// Gets a fixed pointer to the inner context.
    #[inline(always)]
    fn as_ptr(&self) -> *const RwCell<ContextInner> {
        &*self.0
    }

    /// Completes the execution of a clogged event based upon its type.
    ///
    /// # Safety
    ///
    /// The inner context must be valid and initialized, and the inner manager must be valid,
    /// with no other outstanding mutable references.
    #[inline(always)]
    unsafe fn run_clogged_event(
        &mut self,
        event: Box<dyn Any + Send + Sync>,
        inner_mgr: *mut EventManager,
    ) {
        if let Some(ev) = event.downcast_ref::<notify::AddSystem>() {
            (ev.executor)(self);
        }
        if let Some(ev) = event.downcast_ref::<notify::RemoveSystem>() {
            (ev.executor)(self);
        }
        if let Some(ev) = event.downcast_ref::<notify::ResetSystem>() {
            (ev.executor)(self);
        }
        if let Ok(ev) = event.downcast::<notify::Flush>() {
            (*inner_mgr).push_event_cycle(ev.0.events.into_iter())
        }
    }

    /// Loads and executes events from the event manager until no new events are available.
    ///
    /// # Safety
    ///
    /// Context and state must refer to the same valid inner context, and must remain valid for this function's duration.
    /// No other mutable references must exist to the event manager.
    #[inline(always)]
    unsafe fn process_events(
        ctx: *const RwCell<ContextInner>,
        on_new_work: &wasm_sync::Condvar,
        state: &wasm_sync::Mutex<*mut EventManager>,
        callback: &Weak<dyn Fn() + Send + Sync>,
    ) {
        let mut lock_guard = MaybeUninit::new(state.lock().unwrap_unchecked());
        while let Some(mgr) = lock_guard.assume_init_mut().as_mut() {
            let guard = (*ctx).borrow();
            match mgr.next_job(&guard) {
                Ok(to_run) => {
                    lock_guard.assume_init_drop();

                    to_run.execute(&guard);

                    (***lock_guard.write(state.lock().unwrap_unchecked())).complete_job(&to_run);
                    guard
                        .thread_pool
                        .set_callback(Some(callback.upgrade().unwrap_unchecked()));
                    on_new_work.notify_all();
                    drop(guard);
                }
                Err(EventJobError::Complete) => {
                    **lock_guard.assume_init_mut() = std::ptr::null_mut();
                    guard.thread_pool.set_callback(None);
                    on_new_work.notify_all();
                    break;
                }
                Err(EventJobError::MainThreadRequired) => {
                    guard.thread_pool.set_callback(None);
                    on_new_work.notify_all();
                    break;
                }
                _ => {
                    guard.thread_pool.set_callback(None);
                    break;
                }
            }
        }
        lock_guard.assume_init_drop();
    }
}

impl std::fmt::Debug for GeeseContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("GeeseContext")
            .field(&(&*self.0 as *const RwCell<ContextInner> as *const ()))
            .finish()
    }
}

impl Default for GeeseContext {
    #[inline(always)]
    fn default() -> Self {
        Self(Box::pin(RwCell::default()))
    }
}

impl Drop for GeeseContext {
    #[inline(always)]
    fn drop(&mut self) {
        unsafe {
            self.0.borrow().clear_all_systems();
        }
    }
}

/// The backing for a Geese context. The inner context must be pinned
/// in memory so that pointers to it remain valid.
struct ContextInner {
    /// The set of events to which systems respond.
    event_handlers: EventMap,
    /// The manager that schedules and runs event cycles.
    event_manager: UnsafeCell<EventManager>,
    /// A sender that may be used to raise events within the context.
    event_sender: std::sync::mpsc::Sender<Event>,
    /// The thread on which the context was created.
    owning_thread: std::thread::ThreadId,
    /// The set of systems which (including all dependencies) implement the `Sync` trait.
    sync_systems: BitVec,
    /// The set of currently-loaded system initializers.
    system_initializers: FxHashMap<TypeId, SystemInitializer>,
    /// The set of loaded systems.
    systems: Vec<SystemHolder>,
    /// The threadpool with which to asynchronously coordinate context work.
    thread_pool: Arc<dyn GeeseThreadPool>,
    /// The transitive dependency lists for each system.
    transitive_dependencies: SystemFlagsList,
    /// The lists of bidirectional dependencies for each system: all systems that can reach, or are reachable from, a single system.
    transitive_dependencies_bi: SystemFlagsList,
    /// The lists of mutable transitive dependencies for each system.
    transitive_dependencies_mut: SystemFlagsList,
}

impl ContextInner {
    /// The default amount of space to allocate for processing new systems during creation.
    const DEFAULT_SYSTEM_PROCESSING_SIZE: usize = 8;

    /// Creates a new inner context that uses the given threadpool to complete event cycles.
    #[inline(always)]
    pub fn with_threadpool(pool: impl GeeseThreadPool) -> Self {
        let (mgr, event_sender) = EventManager::new();

        Self {
            event_handlers: EventMap::default(),
            event_manager: UnsafeCell::new(mgr),
            event_sender,
            owning_thread: std::thread::current().id(),
            sync_systems: BitVec::default(),
            system_initializers: FxHashMap::default(),
            systems: Vec::default(),
            thread_pool: Arc::new(pool),
            transitive_dependencies: SystemFlagsList::default(),
            transitive_dependencies_bi: SystemFlagsList::default(),
            transitive_dependencies_mut: SystemFlagsList::default(),
        }
    }

    /// Drops all systems from the systems list. No context datastructures are modified.
    ///
    /// # Safety
    ///
    /// All systems in the list be valid, initialized objects.
    #[inline(always)]
    pub unsafe fn clear_all_systems(&self) {
        for holder in self.systems.iter().rev() {
            holder.drop();
        }
    }

    /// Gets the holder for the system at the given index.
    ///
    /// # Safety
    ///
    /// The index must be less than the total number of loaded systems.
    #[inline(always)]
    pub unsafe fn system_holder(&self, index: usize) -> &SystemHolder {
        self.systems.get_unchecked(index)
    }

    /// Adds a new system and all of its dependencies to the set of initializers.
    #[inline(always)]
    pub fn add_new_system<S: GeeseSystem>(&mut self) {
        let mut to_process = SmallVec::new();
        self.add_system_and_load_dependencies::<true>(
            Box::<TypedSystemDescriptor<S>>::default(),
            &mut to_process,
        );

        while let Some(system) = to_process.pop() {
            self.add_system_and_load_dependencies::<false>(system, &mut to_process);
        }
    }

    /// Adds the provided system descriptor to the set of initializers, and pushes all of the dependencies
    /// to process into the queue.
    #[inline(always)]
    fn add_system_and_load_dependencies<const TOP_LEVEL: bool>(
        &mut self,
        system: Box<dyn SystemDescriptor>,
        to_process: &mut SmallVec<
            [Box<dyn SystemDescriptor>; Self::DEFAULT_SYSTEM_PROCESSING_SIZE],
        >,
    ) {
        if TOP_LEVEL {
            for dependency in system.dependencies().as_inner() {
                to_process.push(dependency.descriptor());
            }
            self.system_initializers
                .insert(system.system_id(), SystemInitializer::new(system, true));
        } else if let Entry::Vacant(entry) = self.system_initializers.entry(system.system_id()) {
            for dependency in system.dependencies().as_inner() {
                to_process.push(dependency.descriptor());
            }

            entry.insert(SystemInitializer::new(system, false));
        }
    }

    /// Instantiates all new systems added to the initializer map, appropriately resizing the context resources.
    #[inline(always)]
    pub fn instantiate_added_systems<'a>(
        &mut self,
        initializers: &'a FxHashMap<TypeId, SystemInitializer>,
        ctx: *const RwCell<ContextInner>,
    ) -> SmallVec<[&'a SystemInitializer; Self::DEFAULT_SYSTEM_PROCESSING_SIZE]> {
        unsafe {
            assert!(
                initializers.len() < u16::MAX as usize,
                "Maximum number of supported systems exceeded."
            );
            let mut old_systems = take(&mut self.systems);
            old_systems.set_len(0);
            let old_system_view = old_systems.spare_capacity_mut();
            let mut to_initialize = SmallVec::new();

            self.systems.reserve(initializers.len());
            self.event_handlers.clear();
            self.sync_systems = BitVec::repeat(false, initializers.len());
            self.transitive_dependencies = SystemFlagsList::new(false, initializers.len());
            self.transitive_dependencies_mut = SystemFlagsList::new(false, initializers.len());

            let mut default_holder;

            for system in Self::topological_sort_systems(initializers) {
                let old_id = system.id();
                system.set_id(self.systems.len() as u16);

                let holder = if old_id < u16::MAX {
                    let res = old_system_view.get_unchecked_mut(old_id as usize);
                    assert!(
                        res.assume_init_mut().value.free(),
                        "Attempted to borrow system while the context was moving it."
                    );
                    res
                } else {
                    default_holder = MaybeUninit::new(SystemHolder::new(
                        system.descriptor().system_id(),
                        system.descriptor().dependency_len(),
                        self.event_sender.clone(),
                        ctx,
                    ));
                    to_initialize.push(system.into_inner());
                    &mut default_holder
                };

                Self::update_holder_data(holder.assume_init_mut(), initializers, &system);
                self.load_transitive_dependencies(holder.assume_init_mut(), &system);
                self.systems.push(holder.assume_init_read());

                self.event_handlers
                    .add_handlers(system.id(), system.descriptor().event_handlers());
                self.sync_systems
                    .set_unchecked(system.id() as usize, system.descriptor().is_sync());
            }

            self.compute_sync_transitive_dependencies();
            self.compute_transitive_dependencies_bi();
            (*self.event_manager.get()).configure(self);

            to_initialize
        }
    }

    /// Removes a system from being top-level in the dependency graph.
    #[inline(always)]
    fn remove_top_level_system<S: GeeseSystem>(&mut self) {
        let system = self
            .system_initializers
            .get(&TypeId::of::<S>())
            .expect("System was not loaded.");
        assert!(
            system.top_level(),
            "System {:?} was not previously added.",
            type_name::<S>()
        );
        system.set_top_level(false);
    }

    /// Computes the set of systems that are reachable from the top levels of the dependency graph.
    #[inline(always)]
    fn determine_connected_systems(&self) -> BitVec {
        unsafe {
            let mut connected_systems = BitVec::repeat(false, self.systems.len());

            for system in self.system_initializers.values() {
                if system.top_level() {
                    connected_systems[..] |= self
                        .transitive_dependencies
                        .get_unchecked(system.id() as usize);
                }
            }

            connected_systems
        }
    }

    /// Loads all transitive dependencies of the given system into the transitive dependency lists. All dependencies
    /// of this system must have had their transitive dependencies loaded prior to calling this method.
    ///
    /// # Safety
    ///
    /// The system holder and initializer IDs must all refer to valid systems within the context.
    #[inline(always)]
    unsafe fn load_transitive_dependencies(
        &mut self,
        holder: &SystemHolder,
        initializer: &SystemInitializer,
    ) {
        let mut edit = self
            .transitive_dependencies
            .edit_unchecked(holder.handle.id() as usize);
        let mut edit_mut = self
            .transitive_dependencies_mut
            .edit_unchecked(holder.handle.id() as usize);

        edit.set_unchecked(holder.handle.id() as usize, true);
        edit_mut.set_unchecked(holder.handle.id() as usize, true);

        for i in 0..holder.handle.dependency_len() {
            let global_index = holder.handle.dependency_id(i);
            edit.or_with_unchecked(global_index as usize);
            if initializer
                .descriptor()
                .dependencies()
                .as_inner()
                .get(i as usize)
                .unwrap_unchecked()
                .mutable()
            {
                edit_mut.or_with_unchecked(global_index as usize);
            }
        }
    }

    /// Compacts the set of remaining systems into a new systems array after some systems have been removed.
    ///
    /// # Safety
    ///
    /// For this function call to be sound, the connected bitmap must correspond exactly to which systems
    /// are initialized in the context.
    #[inline(always)]
    pub unsafe fn compact_remaining_systems(&mut self, connected: &BitVec) {
        self.event_handlers.clear();
        self.systems.set_len(0);
        let old_systems = self.systems.spare_capacity_mut();

        let mut new_systems = Vec::with_capacity(self.system_initializers.len());
        let system_view = new_systems.spare_capacity_mut();

        self.sync_systems = BitVec::repeat(false, self.system_initializers.len());
        let mut new_transitive_dependencies =
            SystemFlagsList::new(false, self.system_initializers.len());
        let mut new_transitive_dependencies_mut =
            SystemFlagsList::new(false, self.system_initializers.len());

        for old_id in connected.iter_ones() {
            let system_holder = old_systems.get_unchecked_mut(old_id);
            let initializer = self
                .system_initializers
                .get(&system_holder.assume_init_ref().system_id)
                .unwrap_unchecked();
            initializer.set_id(Self::compact_system_id(old_id as u16, connected));

            let new_system = system_view
                .get_unchecked_mut(initializer.id() as usize)
                .write(system_holder.assume_init_read());
            new_system.handle.set_id(initializer.id());
            Self::compact_transitive_dependencies(
                new_system,
                connected,
                self.transitive_dependencies_mut.get_unchecked(old_id),
                &mut new_transitive_dependencies,
                &mut new_transitive_dependencies_mut,
            );
            self.event_handlers
                .add_handlers(initializer.id(), initializer.descriptor().event_handlers());
            self.sync_systems.set_unchecked(
                initializer.id() as usize,
                initializer.descriptor().is_sync(),
            );
        }

        Self::drop_dead_holders(connected, old_systems);

        new_systems.set_len(self.system_initializers.len());
        self.systems = new_systems;
        self.transitive_dependencies = new_transitive_dependencies;
        self.transitive_dependencies_mut = new_transitive_dependencies_mut;
        self.compute_sync_transitive_dependencies();
        self.compute_transitive_dependencies_bi();
        (*self.event_manager.get()).configure(self);
    }

    /// Reloads all of the system instances that depend upon the given system.
    #[inline(always)]
    pub fn reset_system<S: GeeseSystem>(&self) {
        unsafe {
            let id = self
                .system_initializers
                .get(&TypeId::of::<S>())
                .expect("Attempted to reset nonexistant system.")
                .id();
            let to_load = self.unload_dependents(id);
            self.load_dependents(to_load);
        }
    }

    /// Loads all systems that depend upon the system with the given ID in topological order.
    ///
    /// # Safety
    ///
    /// For the result of this method to be defined, the list of systems to load must contain IDs
    /// strictly less than the total number of systems loaded in the context.
    #[inline(always)]
    unsafe fn load_dependents(
        &self,
        to_load: SmallVec<[u16; Self::DEFAULT_SYSTEM_PROCESSING_SIZE]>,
    ) {
        for i in to_load.into_iter().rev() {
            let holder = self.systems.get_unchecked(i as usize);
            let descriptor = self
                .system_initializers
                .get(&holder.system_id)
                .unwrap_unchecked()
                .descriptor();
            holder
                .value
                .borrow_mut()
                .write(descriptor.create(holder.handle.clone()));
        }
    }

    /// Unloads all systems that depend upon the system with the given ID in reverse topological order.
    ///
    /// # Safety
    ///
    /// For this function call to be sound, the context's systems must be valid and initialized, and the
    /// given ID must correspond to a valid system.
    #[inline(always)]
    unsafe fn unload_dependents(
        &self,
        id: u16,
    ) -> SmallVec<[u16; Self::DEFAULT_SYSTEM_PROCESSING_SIZE]> {
        let mut dropped = SmallVec::new();

        for i in ((id as usize + 1)..self.systems.len()).rev() {
            if *self
                .transitive_dependencies
                .get_unchecked(i)
                .get_unchecked(id as usize)
            {
                self.systems.get_unchecked(i).drop();
                dropped.push(i as u16);
            }
        }

        self.systems.get_unchecked(id as usize).drop();
        dropped.push(id);

        dropped
    }

    /// Calculates which systems and their transitive dependencies all implement `Sync`, and stores the result.
    ///
    /// # Safety
    ///
    /// For this function call to be sound, the size of the systems vector must exactly match the size of
    /// the dependencies and sync bitmaps.
    #[inline(always)]
    unsafe fn compute_sync_transitive_dependencies(&mut self) {
        let mut syncs = BitVec::repeat(false, self.systems.len());
        let mut working_memory = BitVec::repeat(false, self.systems.len());

        for i in 0..self.systems.len() {
            working_memory.clone_from_bitslice(self.transitive_dependencies.get_unchecked(i));
            *working_memory[..].not() |= &self.sync_systems;
            if working_memory.all() {
                syncs.set_unchecked(i, true);
            }
        }

        self.sync_systems = syncs;
    }

    /// Calculates the set of bidirectional dependencies, and stores it.
    ///
    /// # Safety
    ///
    /// For this call to be defined, the set of systems must be valid and equal in
    /// length to the dependency lists.
    #[inline(always)]
    unsafe fn compute_transitive_dependencies_bi(&mut self) {
        self.transitive_dependencies_bi = self.compute_transitive_dependencies_inverse();
        self.transitive_dependencies_bi |= &self.transitive_dependencies;
    }

    /// For each system, calculates the set of dependents.
    ///
    /// # Safety
    ///
    /// For this call to be defined, the set of systems must be valid and equal in
    /// length to the dependency lists.
    #[inline(always)]
    unsafe fn compute_transitive_dependencies_inverse(&mut self) -> SystemFlagsList {
        let mut inverse = SystemFlagsList::new(false, self.systems.len());

        for i in (0..self.systems.len()).rev() {
            let mut edit = inverse.edit_unchecked(i);
            edit.set_unchecked(i, true);
            let holder = self.systems.get_unchecked(i);
            for i in (0..holder.handle.dependency_len()).map(|x| holder.handle.dependency_id(x)) {
                edit.or_into_unchecked(i as usize);
            }
        }

        inverse
    }

    /// Topologically sorts all systems in the map, producing a vector sorted such that all dependencies come before
    /// their dependents.
    #[inline(always)]
    fn topological_sort_systems(
        descriptors: &FxHashMap<TypeId, SystemInitializer>,
    ) -> Vec<SystemStateRef<'_>> {
        unsafe {
            let mut sort: TopologicalSort<SystemStateRef<'_>> = TopologicalSort::new();

            for state in descriptors.values() {
                sort.insert(SystemStateRef(state));
                for dependency in state.descriptor().dependencies().as_inner() {
                    sort.add_dependency(
                        SystemStateRef(
                            descriptors
                                .get(&dependency.dependency_id().into())
                                .unwrap_unchecked(),
                        ),
                        SystemStateRef(state),
                    );
                }
            }

            sort.collect::<Vec<_>>()
        }
    }

    /// Updates the system holder's ID and dependency map.
    ///
    /// # Safety
    ///
    /// This function must not be called while the system handle is simultaneously being used elsewhere.
    #[inline(always)]
    unsafe fn update_holder_data(
        data: &mut SystemHolder,
        descriptors: &FxHashMap<TypeId, SystemInitializer>,
        state: &SystemInitializer,
    ) {
        data.handle.set_id(state.id());
        for (index, dependency) in state
            .descriptor()
            .dependencies()
            .as_inner()
            .into_iter()
            .enumerate()
        {
            data.handle.set_dependency_id(
                index as u16,
                descriptors
                    .get(&dependency.dependency_id().into())
                    .unwrap_unchecked()
                    .id(),
            );
        }
    }

    /// Computes the new system ID from the set of remaining systems in the bitmap.
    ///
    /// # Safety
    ///
    /// For this function to be sound, the system ID must be less than the bitmap length.
    #[inline(always)]
    unsafe fn compact_system_id(system: u16, remaining_systems: &BitVec) -> u16 {
        (*bitvec::ptr::bitslice_from_raw_parts(remaining_systems.as_bitptr(), system as usize))
            .count_ones() as u16
    }

    /// Updates the system handle's dependency maps using the connectivity bitmap after some systems have been dropped.
    ///
    /// # Safety
    ///
    /// For this function call to be defined, all of the holder's current dependency values
    /// must be less than the connected bitmap and transitive dependency list lengths.
    #[inline(always)]
    unsafe fn compact_transitive_dependencies(
        holder: &SystemHolder,
        connected: &BitVec,
        old_transitive_dependencies_mut: &BitSlice,
        transitive_dependencies: &mut SystemFlagsList,
        transitive_dependencies_mut: &mut SystemFlagsList,
    ) {
        let mut edit = transitive_dependencies.edit_unchecked(holder.handle.id() as usize);
        let mut edit_mut = transitive_dependencies_mut.edit_unchecked(holder.handle.id() as usize);

        edit.set(holder.handle.id() as usize, true);
        edit_mut.set(holder.handle.id() as usize, true);

        for i in 0..holder.handle.dependency_len() {
            let old_global_index = holder.handle.dependency_id(i);
            let global_index = Self::compact_system_id(old_global_index, connected);

            edit.or_with_unchecked(global_index as usize);

            if *old_transitive_dependencies_mut.get_unchecked(old_global_index as usize) {
                edit_mut.or_with_unchecked(global_index as usize);
            }

            holder.handle.set_dependency_id(i, global_index);
        }
    }

    /// Drops all dead system holders from the systems array.
    ///
    /// # Safety
    ///
    /// For this call to be defined, all of the zeroes in the connected bitmap must correspond to
    /// valid, initialized system holders in the old systems list.
    #[inline(always)]
    unsafe fn drop_dead_holders(connected: &BitVec, old_systems: &mut [MaybeUninit<SystemHolder>]) {
        for dead_system in connected.iter_zeros() {
            old_systems
                .get_unchecked_mut(dead_system)
                .assume_init_drop();
        }
    }
}

impl Default for ContextInner {
    #[inline(always)]
    fn default() -> Self {
        Self::with_threadpool(HardwareThreadPool::default())
    }
}

/// Tracks the ID and initialization information of loaded systems.
struct SystemInitializer {
    /// The system descriptor.
    descriptor: Box<dyn SystemDescriptor>,
    /// The ID assigned to the system.
    id: Cell<u16>,
    /// Whether this is a top-level system in the dependency graph.
    top_level: Cell<bool>,
}

impl SystemInitializer {
    /// Creates a new system initializer.
    #[inline(always)]
    pub fn new(descriptor: Box<dyn SystemDescriptor>, top_level: bool) -> Self {
        Self {
            descriptor,
            id: Cell::new(u16::MAX),
            top_level: Cell::new(top_level),
        }
    }

    /// Gets the descriptor associated with this initializer.
    #[inline(always)]
    pub fn descriptor(&self) -> &dyn SystemDescriptor {
        &*self.descriptor
    }

    /// Gets the current ID for this system type.
    #[inline(always)]
    pub fn id(&self) -> u16 {
        self.id.get()
    }

    /// Sets the current ID for this system type.
    #[inline(always)]
    pub fn set_id(&self, id: u16) {
        self.id.set(id);
    }

    /// Gets whether this is a top-level system.
    #[inline(always)]
    pub fn top_level(&self) -> bool {
        self.top_level.get()
    }

    /// Sets whether this is a top-level system.
    #[inline(always)]
    pub fn set_top_level(&self, top_level: bool) {
        self.top_level.set(top_level);
    }
}

/// Provides the ability to hash system initializer references and
/// compare them for reference equality.
#[derive(Copy, Clone)]
struct SystemStateRef<'a>(&'a SystemInitializer);

impl<'a> SystemStateRef<'a> {
    /// Obtains the inner reference that this value hold.
    #[inline(always)]
    pub fn into_inner(self) -> &'a SystemInitializer {
        self.0
    }
}

impl<'a> Deref for SystemStateRef<'a> {
    type Target = SystemInitializer;

    #[inline(always)]
    fn deref(&self) -> &Self::Target {
        self.0
    }
}

impl<'a> Hash for SystemStateRef<'a> {
    #[inline(always)]
    fn hash<H: Hasher>(&self, state: &mut H) {
        (self.0 as *const _ as usize).hash(state);
    }
}

impl<'a> PartialEq for SystemStateRef<'a> {
    #[inline(always)]
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self.0 as *const _, other.0 as *const _)
    }
}

impl<'a> Eq for SystemStateRef<'a> {}

/// Stores the current state of a system.
struct SystemHolder {
    /// The handle used to manage system state.
    pub handle: Arc<ContextHandleInner>,
    /// The type ID of the system.
    pub system_id: TypeId,
    /// The loaded system itself.
    pub value: RwCell<MaybeUninit<Box<dyn Any>>>,
}

impl SystemHolder {
    /// Creates a new holder with the provided information. The context handle's ID and dependency map
    /// initially have undefined contents.
    ///
    /// # Safety
    ///
    /// The initial contents of the context handle must not affect the program's execution in any fashion.
    #[inline(always)]
    pub unsafe fn new(
        system_id: TypeId,
        dependency_len: usize,
        event_sender: std::sync::mpsc::Sender<Event>,
        context: *const RwCell<ContextInner>,
    ) -> Self {
        let mut dependency_ids = SmallVec::with_capacity(dependency_len);
        dependency_ids.set_len(dependency_len);
        Self {
            handle: Arc::new(ContextHandleInner {
                context,
                event_sender: wasm_sync::Mutex::new(event_sender),
                dependency_ids,
                id: Cell::new(0),
            }),
            system_id,
            value: RwCell::new(MaybeUninit::uninit()),
        }
    }

    /// Drops the system in-place and ensures that the context handle is no longer alive.
    ///
    /// # Safety
    ///
    /// The system value must refer to a valid, initialized object.
    #[inline(always)]
    pub unsafe fn drop(&self) {
        let mut value = self.value.borrow_mut();
        value.assume_init_drop();
        assert!(
            Arc::strong_count(&self.handle) == 1,
            "Attempted to retain context handle beyond system lifetime."
        );
    }
}

/// Represents a densely-packed list of lists of bitflags used to store
/// per-system information about other systems.
#[derive(Clone, Debug, Default)]
struct SystemFlagsList {
    /// The underlying data buffer.
    data: BitVec,
    /// The amount of bits per system.
    stride: usize,
}

impl SystemFlagsList {
    /// Creates a new list of system flags, initialized to the given value.
    #[inline(always)]
    pub fn new(bit: bool, size: usize) -> Self {
        Self {
            data: BitVec::repeat(bit, size * size),
            stride: size,
        }
    }

    /// Initializes an edit of the flags for the given system, allowing for the
    /// simultaneous immutable use of flags with smaller indices during the edit.
    ///
    /// # Safety
    ///
    /// For this function to be sound, index must be less than the total number of systems.
    #[inline(always)]
    pub unsafe fn edit_unchecked(&mut self, index: usize) -> SystemFlagsListEdit {
        let (rest, first) = self.data.split_at_unchecked_mut(index * self.stride);

        SystemFlagsListEdit {
            editable: &mut *bitvec::ptr::bitslice_from_raw_parts_mut(
                first.as_mut_bitptr(),
                self.stride,
            ),
            rest,
            stride: self.stride,
        }
    }

    /// Gets a slice associated with the given system at the provided index.
    ///
    /// # Safety
    ///
    /// For this function to be sound, index must be less than the total number of systems.
    #[inline(always)]
    pub unsafe fn get_unchecked(&self, index: usize) -> &BitSlice {
        &*bitvec::ptr::bitslice_from_raw_parts(
            self.data.as_bitptr().add(index * self.stride),
            self.stride,
        )
    }
}

impl BitOrAssign<&SystemFlagsList> for SystemFlagsList {
    #[inline(always)]
    fn bitor_assign(&mut self, rhs: &SystemFlagsList) {
        self.data |= &rhs.data;
    }
}

/// Represents an ongoing edit operation to a system flags list. This allows
/// for editing one part of the list while referencing other parts.
struct SystemFlagsListEdit<'a> {
    /// The part of the bit vector that is being edited.
    editable: &'a mut BitSlice<BitSafeUsize>,
    /// Everything prior to the editable part which may be immutably referenced.
    rest: &'a mut BitSlice<BitSafeUsize>,
    /// The number of bits per system.
    stride: usize,
}

impl<'a> SystemFlagsListEdit<'a> {
    /// Computes the bitwise or into the editable region with the system at the given index.
    ///
    /// # Safety
    ///
    /// For this function call to be defined, the index must be smaller than that of the system
    /// to edit.
    #[inline(always)]
    pub unsafe fn or_with_unchecked(&mut self, index: usize) {
        *self.editable |= &*bitvec::ptr::bitslice_from_raw_parts(
            self.rest.as_bitptr().add(index * self.stride),
            self.stride,
        );
    }

    /// Computes the bitwise or of the editable region with the system into the given index.
    ///
    /// # Safety
    ///
    /// For this function call to be defined, the index must be smaller than that of the system
    /// to edit.
    #[inline(always)]
    pub unsafe fn or_into_unchecked(&mut self, index: usize) {
        *bitvec::ptr::bitslice_from_raw_parts_mut(
            self.rest.as_mut_bitptr().add(index * self.stride),
            self.stride,
        ) |= &*self.editable;
    }
}

impl<'a> Deref for SystemFlagsListEdit<'a> {
    type Target = BitSlice<BitSafeUsize>;

    #[inline(always)]
    fn deref(&self) -> &Self::Target {
        self.editable
    }
}

impl<'a> DerefMut for SystemFlagsListEdit<'a> {
    #[inline(always)]
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.editable
    }
}

/// Represents an immutable reference to a system.
#[derive(Debug)]
pub struct SystemRef<'a, T: ?Sized> {
    /// The backing guard for the system.
    inner: RwCellGuard<'a, T>,
}

impl<'a, T: ?Sized> SystemRef<'a, T> {
    /// Creates a new immutable reference to a system from a
    /// `RwCell` borrow.
    #[inline(always)]
    fn new(inner: RwCellGuard<'a, T>) -> Self {
        Self { inner }
    }

    /// Creates a reference to a specific borrowed component of a system.
    #[inline(always)]
    pub fn map<U, F>(orig: SystemRef<'a, T>, f: F) -> SystemRef<'a, U>
    where
        F: FnOnce(&T) -> &U,
        U: ?Sized,
    {
        SystemRef::new(RwCellGuard::map(orig.inner, f))
    }
}

impl<'a, T: ?Sized> Deref for SystemRef<'a, T> {
    type Target = T;

    #[inline(always)]
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

/// Represents a mutable reference to a system.
#[derive(Debug)]
pub struct SystemRefMut<'a, T: ?Sized> {
    /// The backing guard for the system.
    inner: RwCellGuardMut<'a, T>,
}

impl<'a, T: ?Sized> SystemRefMut<'a, T> {
    /// Creates a new mutable reference to a system from a mutable
    /// `RwCell` borrow.
    #[inline(always)]
    fn new(inner: RwCellGuardMut<'a, T>) -> Self {
        Self { inner }
    }

    /// Creates a reference to a specific borrowed component of a system.
    #[inline(always)]
    pub fn map<U, F>(orig: SystemRefMut<'a, T>, f: F) -> SystemRefMut<'a, U>
    where
        F: FnOnce(&mut T) -> &mut U,
        U: ?Sized,
    {
        SystemRefMut::new(RwCellGuardMut::map(orig.inner, f))
    }
}

impl<'a, T: ?Sized> Deref for SystemRefMut<'a, T> {
    type Target = T;

    #[inline(always)]
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl<'a, T: ?Sized> DerefMut for SystemRefMut<'a, T> {
    #[inline(always)]
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

/// Stores a mapping from event type IDs to groups of event handlers.
#[derive(Clone, Debug, Default)]
struct EventMap {
    /// The inner mapping from type IDs to event handlers.
    handlers: FxHashMap<TypeId, SmallVec<[EventHandlerEntry; Self::DEFAULT_HANDLER_BUFFER_SIZE]>>,
}

impl EventMap {
    /// The default amount of space to allocate for event handler lists.
    const DEFAULT_HANDLER_BUFFER_SIZE: usize = 4;

    /// Adds all of the event handlers in the given list to this map, associating them with
    /// the provided system ID.
    #[inline(always)]
    pub fn add_handlers(&mut self, system_id: u16, handlers: &ConstList<'_, EventHandlerRaw>) {
        for entry in handlers {
            self.handlers
                .entry(entry.event_id())
                .or_default()
                .push(EventHandlerEntry {
                    system_id,
                    handler: *entry.handler(),
                });
        }
    }

    /// Clears the event map of all handlers.
    #[inline(always)]
    pub fn clear(&mut self) {
        self.handlers.clear();
    }

    /// Gets the event handlers that respond to the provided event type.
    #[inline(always)]
    pub fn handlers(&self, event: TypeId) -> &[EventHandlerEntry] {
        self.handlers
            .get(&event)
            .map(|x| &x[..])
            .unwrap_or_default()
    }
}

/// Describes a single event handler.
#[derive(Copy, Clone, Debug, Default)]
struct EventHandlerEntry {
    /// The invoker that may be used to dispatch events.
    pub handler: EventInvoker,
    /// The ID of the system with which this event handler is associated.
    pub system_id: u16,
}

/// Holds a series of events that the context should execute.
#[derive(Default)]
pub struct EventBuffer {
    /// The events contained in this queue.
    events: SmallVec<[Box<dyn Any + Send + Sync>; Self::DEFAULT_EVENT_BUFFER_SIZE]>,
}

impl EventBuffer {
    /// The default amount of space to allocate for items in an event queue.
    const DEFAULT_EVENT_BUFFER_SIZE: usize = 4;
}

impl EventQueue for EventBuffer {
    fn with_many_boxed(
        mut self,
        events: impl IntoIterator<Item = Box<dyn Any + Send + Sync>>,
    ) -> Self {
        self.events.extend(events);
        self
    }
}

/// Denotes an event sequence that will be flushed through a context upon drop.
pub struct ContextEventQueue<'a> {
    /// The context to which events should be flushed.
    ctx: &'a mut GeeseContext,
    /// The internal queue of events.
    queue: EventBuffer,
}

impl<'a> EventQueue for ContextEventQueue<'a> {
    fn with_many_boxed(
        mut self,
        events: impl IntoIterator<Item = Box<dyn Any + Send + Sync>>,
    ) -> Self {
        self.queue.events.extend(events);
        self
    }
}

impl<'a> Drop for ContextEventQueue<'a> {
    fn drop(&mut self) {
        self.ctx.flush_buffer(take(&mut self.queue));
    }
}

/// Provides events to which the Geese context responds.
pub mod notify {
    use super::*;

    /// Tells Geese to load the specified system when this event triggers.
    pub(crate) struct AddSystem {
        /// The function that executes the system addition.
        pub(super) executor: fn(&mut GeeseContext),
    }

    /// Causes Geese to load a system during the next event cycle. The context will panic if the system was already present.
    #[inline(always)]
    pub fn add_system<S: GeeseSystem>() -> impl Send + Sync {
        AddSystem {
            executor: GeeseContext::add_system::<S>,
        }
    }

    /// Tells Geese to unload the specified system when this event triggers.
    pub(crate) struct RemoveSystem {
        /// The function that executes the system removal.
        pub(super) executor: fn(&mut GeeseContext),
    }

    /// Causes Geese to remove a system during the next event cycle. The context will panic if the system was not present.
    #[inline(always)]
    pub fn remove_system<S: GeeseSystem>() -> impl Send + Sync {
        RemoveSystem {
            executor: GeeseContext::remove_system::<S>,
        }
    }

    /// Tells Geese to reset the specified system when this event triggers.
    pub(crate) struct ResetSystem {
        /// The function that executes the system reset.
        pub(super) executor: fn(&mut GeeseContext),
    }

    /// Causes Geese to reload a system during the next event cycle. The context will panic if the system was not present.
    #[inline(always)]
    pub fn reset_system<S: GeeseSystem>() -> impl Send + Sync {
        ResetSystem {
            executor: GeeseContext::reset_system::<S>,
        }
    }

    /// Causes the context to delay processing the given event during
    /// the current cycle.
    pub(crate) struct Delayed(pub(crate) Box<dyn Any + Send + Sync>);

    /// Tells the context to delay processing this event until all of the other events
    /// placed into the queue have been processed.
    #[inline(always)]
    pub fn delayed<T: 'static + Send + Sync>(event: T) -> impl Send + Sync {
        Delayed(Box::new(event))
    }

    /// Tells the context to process the events in the queue as a separate, isolated event cycle.
    pub(crate) struct Flush(pub EventBuffer);

    impl EventQueue for Flush {
        fn with_many_boxed(
            mut self,
            events: impl IntoIterator<Item = Box<dyn Any + Send + Sync>>,
        ) -> Self {
            self.0.events.extend(events.into_iter());
            self
        }
    }

    /// Instructs the Geese context to process a series of events, and all subevents spawned from them, before moving to other items in the queue.
    #[inline(always)]
    pub fn flush() -> impl EventQueue + Send + Sync {
        Flush(EventBuffer::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct A;

    impl A {
        fn increment(&mut self, event: &Arc<AtomicUsize>) {
            event.fetch_add(1, Ordering::Relaxed);
        }

        pub fn answer(&self) -> bool {
            true
        }
    }

    impl GeeseSystem for A {
        const EVENT_HANDLERS: EventHandlers<Self> = event_handlers().with(Self::increment);

        fn new(_: GeeseContextHandle<Self>) -> Self {
            Self
        }
    }

    struct B {
        ctx: GeeseContextHandle<Self>,
    }

    impl B {
        fn test_answer(&mut self, event: &Arc<AtomicBool>) {
            event.store(self.ctx.get::<A>().answer(), Ordering::Relaxed);
        }
    }

    impl GeeseSystem for B {
        const DEPENDENCIES: Dependencies = dependencies().with::<A>();

        const EVENT_HANDLERS: EventHandlers<Self> = event_handlers().with(Self::test_answer);

        fn new(ctx: GeeseContextHandle<Self>) -> Self {
            println!("made new b");
            ctx.raise_event(());
            Self { ctx }
        }
    }

    struct C {
        counter: AtomicUsize,
    }

    impl C {
        pub fn counter(&self) -> usize {
            self.counter.load(Ordering::Acquire)
        }

        fn increment_counter(&mut self, _: &()) {
            self.counter.fetch_add(1, Ordering::AcqRel);
        }
    }

    impl GeeseSystem for C {
        const EVENT_HANDLERS: EventHandlers<Self> = event_handlers().with(Self::increment_counter);

        fn new(_: GeeseContextHandle<Self>) -> Self {
            Self {
                counter: AtomicUsize::new(0),
            }
        }
    }

    struct D;

    impl GeeseSystem for D {
        const DEPENDENCIES: Dependencies = dependencies().with::<A>().with::<C>();

        fn new(ctx: GeeseContextHandle<Self>) -> Self {
            ctx.get::<C>().counter.store(4, Ordering::Release);
            Self
        }
    }

    struct E {
        value: i32,
    }

    impl GeeseSystem for E {
        fn new(_: GeeseContextHandle<Self>) -> Self {
            Self { value: 0 }
        }
    }

    struct F {
        ctx: GeeseContextHandle<Self>,
    }

    impl F {
        fn increment_value(&mut self, _: &()) {
            self.ctx.get_mut::<E>().value += 1;
        }
    }

    impl GeeseSystem for F {
        const DEPENDENCIES: Dependencies = dependencies().with::<Mut<E>>();

        const EVENT_HANDLERS: EventHandlers<Self> = event_handlers().with(Self::increment_value);

        fn new(ctx: GeeseContextHandle<Self>) -> Self {
            Self { ctx }
        }
    }

    struct G {
        ctx: GeeseContextHandle<Self>,
    }

    impl G {
        fn negate_value(&mut self, _: &()) {
            self.ctx.get_mut::<E>().value *= -1;
        }
    }

    impl GeeseSystem for G {
        const DEPENDENCIES: Dependencies = dependencies().with::<Mut<E>>().with::<F>();

        const EVENT_HANDLERS: EventHandlers<Self> = event_handlers().with(Self::negate_value);

        fn new(ctx: GeeseContextHandle<Self>) -> Self {
            ctx.raise_event(());
            Self { ctx }
        }
    }

    struct H {
        ctx: GeeseContextHandle<Self>,
        not_safe: PhantomData<*const u8>,
    }

    impl H {
        fn decrement(&mut self, event: &isize) {
            if *event > 0 {
                self.ctx.raise_event(*event - 1);
                println!(
                    "J on thread {:?} count {event:?}",
                    std::thread::current().id()
                );

                if *event == 25 {
                    self.ctx.raise_event(());
                }
            }
        }
    }

    impl GeeseSystem for H {
        const EVENT_HANDLERS: EventHandlers<Self> = event_handlers().with(Self::decrement);

        fn new(ctx: GeeseContextHandle<Self>) -> Self {
            Self {
                ctx,
                not_safe: PhantomData,
            }
        }
    }

    struct I {
        ctx: GeeseContextHandle<Self>,
        last_value: usize,
    }

    impl I {
        fn decrement(&mut self, event: &usize) {
            if *event > 0 {
                println!(
                    "I on thread {:?} count {event:?}",
                    std::thread::current().id()
                );
                self.last_value = *event;
                self.ctx.raise_event(*event - 1);
            }
        }

        fn hit_it(&mut self, _: &()) {
            println!("Other hit it at {:?}", self.last_value);
        }
    }

    impl GeeseSystem for I {
        const EVENT_HANDLERS: EventHandlers<Self> =
            event_handlers().with(Self::decrement).with(Self::hit_it);

        fn new(ctx: GeeseContextHandle<Self>) -> Self {
            Self { ctx, last_value: 0 }
        }
    }

    struct J;

    impl GeeseSystem for J {
        const DEPENDENCIES: Dependencies = dependencies().with::<H>().with::<I>();

        fn new(_: GeeseContextHandle<Self>) -> Self {
            Self
        }
    }

    struct K {
        value: i32,
    }

    impl K {
        fn increase(&mut self, value: &i32) {
            assert!(*value > self.value);
            self.value = *value;
        }
    }

    impl GeeseSystem for K {
        const EVENT_HANDLERS: EventHandlers<Self> = event_handlers().with(Self::increase);

        fn new(_: GeeseContextHandle<Self>) -> Self {
            Self { value: 0 }
        }
    }

    #[test]
    fn test_single_system() {
        let ab = Arc::new(AtomicUsize::new(0));
        let mut ctx = GeeseContext::default();
        ctx.flush().with(notify::add_system::<A>()).with(ab.clone());
        assert!(ab.load(Ordering::Relaxed) == 1);
    }

    #[test]
    fn test_dependent_system() {
        let ab = Arc::new(AtomicBool::new(false));
        let mut ctx = GeeseContext::default();
        ctx.flush().with(notify::add_system::<B>()).with(ab.clone());
        assert!(ab.load(Ordering::Relaxed));
    }

    #[test]
    fn test_system_reload_one() {
        let mut ctx = GeeseContext::default();
        ctx.flush()
            .with(notify::add_system::<C>())
            .with(notify::add_system::<B>());
        assert_eq!(ctx.get::<C>().counter(), 1);
        ctx.flush().with(notify::reset_system::<A>());
        assert_eq!(ctx.get::<C>().counter(), 2);
    }

    #[test]
    fn test_system_reload_order() {
        let mut ctx = GeeseContext::default();
        ctx.flush()
            .with(notify::add_system::<D>())
            .with(notify::add_system::<B>());
        assert_eq!(ctx.get::<C>().counter(), 5);
        ctx.flush().with(notify::reset_system::<A>());
        assert_eq!(ctx.get::<C>().counter(), 5);
    }

    #[test]
    #[should_panic]
    fn test_add_system_event_twice_panic() {
        let mut ctx = GeeseContext::default();
        ctx.flush()
            .with(notify::add_system::<B>())
            .with(notify::add_system::<B>());
    }

    #[test]
    #[should_panic]
    fn test_remove_system_event_unknown_panic() {
        let mut ctx = GeeseContext::default();
        ctx.flush().with(notify::remove_system::<B>());
    }

    #[test]
    fn test_mut_dependency() {
        let mut ctx = GeeseContext::default();
        ctx.flush().with(notify::add_system::<G>());
        assert_eq!(ctx.get::<E>().value, -1);
    }

    #[test]
    fn test_multiple_threads() {
        let mut ctx = GeeseContext::with_threadpool(HardwareThreadPool::new(2));
        ctx.flush()
            .with(notify::add_system::<J>())
            .with(50usize)
            .with(50isize);
    }

    #[test]
    fn test_flush() {
        let mut ctx = GeeseContext::default();
        ctx.flush()
            .with(notify::add_system::<K>())
            .with(1i32)
            .with(notify::flush().with_many([2i32, 3]))
            .with(4i32);
        assert!(ctx.get::<K>().value == 4);
    }

    #[test]
    #[should_panic]
    fn test_no_flush() {
        let mut ctx = GeeseContext::default();
        ctx.flush()
            .with(notify::add_system::<K>())
            .with(1i32)
            .with(4i32)
            .with(2i32)
            .with(3i32);
    }
}
