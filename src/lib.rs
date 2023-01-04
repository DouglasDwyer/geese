#![deny(warnings)]

//! Geese is a game event system for Rust, built to allow modular game engine design.
//!
//! In Geese, a system is a struct with internal state and a collection of associated
//! event handlers. Systems can raise events and react to events raised by other
//! systems. Systems may also declare dependencies on other systems, which allow
//! them to immutably borrow those systems during event processing. Geese automatically
//! loads all system dependencies. Any struct can act as an event type, and any struct
//! that implements `GeeseSystem` can act as a system type.
//! 
//! The following is an example of how to use Geese to load multiple dependent systems,
//! and propogate events between them. The example creates a Geese context,
//! and requests that system `B` be loaded. When `flush_events` is called,
//! system `A` is loaded first (beause it is a dependency of `B`), and then
//! system `B` is loaded. `B` receives the typed event, and responds by querying
//! system `A` for some information.
//! 
//! ```
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
//!     fn new(_: GeeseContextHandle) -> Self {
//!         Self
//!     }
//! }
//! 
//! struct B {
//!     ctx: GeeseContextHandle
//! }
//! 
//! impl B {
//!     fn test_answer(&mut self, event: &Arc<AtomicBool>) {
//!         event.store(self.ctx.system::<A>().answer(), Ordering::Relaxed);
//!     }
//! }
//! 
//! impl GeeseSystem for B {
//!     fn new(ctx: GeeseContextHandle) -> Self {
//!         Self { ctx }
//!     }
//! 
//!     fn register(entry: &mut GeeseSystemData<Self>) {
//!         entry.dependency::<A>();
//! 
//!         entry.event(Self::test_answer);
//!     }
//! }
//! 
//! fn run() {
//!     let ab = Arc::new(AtomicBool::new(false));
//!     let mut ctx = GeeseContext::default();
//!     ctx.raise_event(notify::AddSystem::new::<B>());
//!     ctx.raise_event(ab.clone());
//!     ctx.flush_events();
//!     assert!(ab.load(Ordering::Relaxed));
//! }
//! ```

mod store;

pub use crate::store::*;

use fxhash::*;
use std::any::*;
use std::cell::*;
use std::collections::*;
use std::fmt::*;
use std::hash::*;
use std::marker::*;
use std::sync::*;
use std::ops::*;

use topological_sort::*;

/// Represents a mapping between type IDs and a value.
type TypeMap<T> = FxHashMap<TypeId, T>;

/// Represents a collection of event handlers with internal state.
pub trait GeeseSystem: 'static {
    /// Creates a new instance of the system for the given system handle.
    fn new(ctx: GeeseContextHandle) -> Self;
    /// Registers the event handlers and dependencies of this system.
    fn register(_: &mut GeeseSystemData<Self>) {}
}

/// Stores event handler data for a system.
pub struct GeeseSystemData<S: GeeseSystem + ?Sized> {
    dependencies: TypeMap<Box<dyn SystemBuilder>>,
    events: Vec<Arc<dyn EventHandler>>,
    phantom: PhantomData<S>
}

impl<S: GeeseSystem> GeeseSystemData<S> {    
    /// Registers an event handler for the given system.
    pub fn event<E: 'static, F: 'static + Fn(&mut S, &E)>(&mut self, handler: F) {
        self.events.push(Arc::new(TypedEventHandler::new(handler)));
    }

    /// Sets the given system as a dependency of this one.
    pub fn dependency<Q: GeeseSystem>(&mut self) {
        let builder = Box::new(TypedSystemBuilder::<Q>::new());
        assert!(self.dependencies.insert(builder.system_id(), builder).is_none(), "Cannot add duplicate dependencies.");
    }

    /// Creates a new, empty system data holder.
    fn new() -> Self {
        Self {
            dependencies: Default::default(),
            events: Default::default(),
            phantom: Default::default()
        }
    }
}

/// Represents a system-specific handle to a Geese context.
pub struct GeeseContextHandle {
    inner: InnerContext,
    system: Arc<dyn SystemBuilder>
}

impl GeeseContextHandle {
    /// Creates a new context handle.
    fn new(inner: InnerContext, system: Arc<dyn SystemBuilder>) -> Self {
        Self {
            inner,
            system
        }
    }

    /// Raises the specified dynamically-typed event.
    pub fn raise_boxed_event(&self, event: Box<dyn Any>) {
        self.inner.events_mut().push_back(event);
    }

    /// Raises the specified event.
    pub fn raise_event<T: 'static>(&self, event: T) {
        self.inner.events_mut().push_back(Box::new(event));
    }

    /// Obtains the specified system dependency.
    pub fn system<T: GeeseSystem>(&self) -> SystemRef<'_, T> {
        assert!(self.system.dependencies().iter().any(|x| x.system_id() == TypeId::of::<T>()), "The specified system was not a declared dependency of this one.");
        SystemRef::new(Ref::map(self.inner.systems(), |x| {
            let holder = x.get(&TypeId::of::<T>()).expect("System was not loaded.").as_ref().expect("Cannot obtain a reference to the current system.");
            holder.system().downcast_ref::<T>().expect("System was not of the correct type.")
        }))
    }
}

impl Debug for GeeseContextHandle {
    fn fmt(&self, _: &mut Formatter<'_>) -> Result {
        Ok(())
    }
}

/// Represents a collection of systems that can create and respond to events.
#[derive(Default)]
pub struct GeeseContext {
    active_systems: TypeMap<Arc<dyn SystemBuilder>>,
    event_map: TypeMap<Vec<Arc<dyn EventHandler>>>,
    inner: InnerContext,
}

impl GeeseContext {
    /// Places the given dynamically-typed event into the system event queue.
    pub fn raise_boxed_event(&mut self, event: Box<dyn Any>) {
        self.inner.events.borrow_mut().push_back(event);
    }

    /// Places the given event into the system event queue.
    pub fn raise_event<T: 'static>(&mut self, event: T) {
        self.inner.events.borrow_mut().push_back(Box::new(event));
    }

    /// Causes an event cycle to complete by running systems until the event queue is empty.
    pub fn flush_events(&mut self) {
        while self.events_in_queue() {
            let mut inner_events = self.inner.events.borrow_mut();
            let mut prior_queue = std::mem::take(&mut *inner_events);
            drop(inner_events);

            while let Some(event) = prior_queue.pop_front() {
                if event.downcast_ref::<notify::Flush>().is_some() && self.events_in_queue() {
                    prior_queue.push_front(Box::new(notify::Flush));
                    
                    for new_event in self.inner.events.borrow_mut().drain(..).rev() {
                        prior_queue.push_front(new_event);
                    }
                }

                self.handle_event(&*event);
            }
        }
    }

    /// Obtains a reference to the given system.
    pub fn system<S: GeeseSystem>(&mut self) -> SystemRef<'_, S> {
        self.flush_events();
        SystemRef::new(Ref::map(self.inner.systems(), |x|
            x.get(&TypeId::of::<S>()).expect("System not found.")
                .as_ref().expect("System was currently borrowed.")
                .system().downcast_ref().expect("System was not of the correct type.")))
    }

    /// Mutably obtains a reference to the given system.
    pub fn system_mut<S: GeeseSystem>(&mut self) -> SystemRefMut<'_, S> {
        self.flush_events();
        SystemRefMut::new(RefMut::map(self.inner.systems_mut(), |x|
            x.get_mut(&TypeId::of::<S>()).expect("System not found.")
                .as_mut().expect("System was currently borrowed.")
                .system_mut().downcast_mut().expect("System was not of the correct type.")))
    }

    /// Determines whether there are any events yet to be processed
    /// in the main event queue.
    fn events_in_queue(&self) -> bool {
        !self.inner.events.borrow().is_empty()
    }

    /// Handles an event by broadcasting it to all systems.
    fn handle_event(&mut self, event: &dyn Any) {
        self.handle_geese_event(event);
        for handler in self.event_map.get(&event.type_id()).unwrap_or(&Vec::new()) {
            let mut system = std::mem::replace(self.inner.systems_mut().get_mut(&handler.system_id()).expect("Key was not found in map."), None);
            handler.respond(system.as_mut().expect("Attempted to send events to a borrowed system.").system_mut(), event);
            self.inner.systems_mut().insert(handler.system_id(), system);
        }
    }

    /// Processes the logic for builtin Geese events.
    fn handle_geese_event(&mut self, event: &dyn Any) {
        if event.type_id() == TypeId::of::<notify::AddSystem>() {
            self.add_system(event.downcast_ref::<notify::AddSystem>().expect("Event type ID did not match type").builder.clone());
        }
        else if event.type_id() == TypeId::of::<notify::RemoveSystem>() {
            self.remove_system(event.downcast_ref::<notify::RemoveSystem>().expect("Event type ID did not match type").type_id);
        }
        else if event.type_id() == TypeId::of::<notify::ResetSystem>() {
            self.reload_system(event.downcast_ref::<notify::ResetSystem>().expect("Event type ID did not match type").type_id);
        }
        else if event.type_id() == TypeId::of::<notify::Delayed>() {
            self.inner.events_mut().push_back(event.downcast_ref::<notify::Delayed>().expect("Event type ID did not match type").0.replace(None).expect("Delayed event was already taken."));
        }
    }
    
    /// Activates and deactivates the appropriate systems if they have changed.
    fn reload_systems(&mut self) {
        let activation_order = Self::determine_system_order(self.active_systems.values().cloned()).expect("Cannot instantiate systems due to a cyclic dependency.");
        let deactivation_order =
        Self::determine_system_order(self.inner.systems().values().map(|x| x.as_ref().expect("Could not borrow system, as it was already borrowed.").builder()))
            .expect("Cannot deactive systems due to a cyclic dependency.");

        for system in &activation_order {
            // We need to re-borrow the system here, because otherwise we would have a mutable reference out to the
            // systems map while a new system is being created (during which it may reference the system map).
            #[allow(clippy::map_entry)]
            if !self.inner.systems_mut().contains_key(&system.system_id()) {
                let holder = SystemHolder::new(self.inner.clone(), system.clone());
                self.inner.systems_mut().insert(system.system_id(), Some(holder));
            }
        }

        for system in deactivation_order.iter().rev() {
            if !activation_order.iter().any(|x| x.system_id() == system.system_id()) {
                let removed = self.inner.systems_mut().remove(&system.system_id());
                drop(removed);
            }
        }

        self.update_event_map(&activation_order[..]);
    }

    /// Reloads the specified system and all of its dependencies.
    fn reload_system(&mut self, system_id: TypeId) {
        let system_reload_order = self.determine_dependent_system_reload_order(system_id);
        for system in system_reload_order.iter().rev() {
            let removed = self.inner.systems_mut().insert(system.system_id(), None).expect("Replaced empty system.");
            drop(removed);
        }

        for system in &system_reload_order {
            let holder = SystemHolder::new(self.inner.clone(), system.clone());
            self.inner.systems_mut().insert(system.system_id(), Some(holder));
        }
    }

    /// Adds a system for use in the current Geese context.
    fn add_system(&mut self, builder: Arc<dyn SystemBuilder>) {
        if self.active_systems.contains_key(&builder.system_id()) {
            panic!("Attempted to add duplicate system.");
        }
        self.active_systems.insert(builder.system_id(), builder);
        self.reload_systems();
    }

    /// Removes a system from use in the current Geese context.
    fn remove_system(&mut self, type_id: TypeId) {
        self.active_systems.remove(&type_id).expect("Attempted to remove unknown system.");
        self.reload_systems();
    }

    /// Determines the order in which dependent systems should be reloaded.
    fn determine_dependent_system_reload_order(&mut self, system_id: TypeId) -> Vec<Arc<dyn SystemBuilder>> {
        let systems = self.inner.systems();

        let mut reload_order = Vec::new();
        reload_order.push(systems[&system_id].as_ref().expect("System was already borrowed.").builder());

        let builders = systems.values().map(|x| x.as_ref().expect("System was already borrowed.").builder()).collect::<Vec<_>>();
        let mut finished = false;
        while !finished {
            finished = true;
            for system in &builders {
                if !reload_order.iter().any(|x| x.system_id() == system.system_id()) {
                    for dependent in system.dependencies() {
                        if reload_order.iter().any(|x| x.system_id() == dependent.system_id()) {
                            finished = false;
                            reload_order.push(system.clone());
                            break;
                        }
                    }
                }
            }
        }

        reload_order
    }

    /// Updates the event-dispatching acceleration structure with the event handlers
    /// for all currently-loaded systems.
    fn update_event_map(&mut self, systems: &[Arc<dyn SystemBuilder>]) {
        self.event_map = TypeMap::default();

        for system in systems {
            for event in system.event_responders() {
                self.event_map.entry(event.event_id()).or_default().push(event.clone());
            }
        }
    }

    /// Determines the topological load ordering for the set of systems and all of their dependencies. Returns
    /// none if a cyclic dependency is found.
    fn determine_system_order(systems: impl Iterator<Item = Arc<dyn SystemBuilder>>) -> Option<Vec<Arc<dyn SystemBuilder>>> {
        let mut ts = TopologicalSort::new();
        let mut processed = HashSet::new();
        let mut to_be_processed = systems.collect::<VecDeque<_>>();
        while let Some(x) = to_be_processed.pop_front() {
            if !processed.contains(&x.system_id()) {
                processed.insert(x.system_id());
                ts.insert(x.clone());
                for dependency in x.dependencies() {
                    ts.add_dependency(dependency.clone(), x.clone());
                    to_be_processed.push_back(dependency.clone());
                }
            }            
        }

        let count = ts.len();
        let output = ts.into_iter().collect::<Vec<_>>();
        (count == output.len()).then_some(output)
    }
}

impl Debug for GeeseContext {
    fn fmt(&self, _: &mut Formatter<'_>) -> Result {
        Ok(())
    }
}

/// Stores shared information about a Geese context.
#[derive(Clone, Default)]
struct InnerContext {
    events: Arc<RefCell<VecDeque<Box<dyn Any>>>>,
    systems: Arc<RefCell<TypeMap<Option<SystemHolder>>>>
}

impl InnerContext {
    /// Mutably obtains the event queue. This will panic if it was already borrowed.
    pub fn events_mut(&self) -> RefMut<'_, VecDeque<Box<dyn Any>>> {
        self.events.borrow_mut()
    }

    /// Obtains the system list. This will panic if it was already mutably borrowed.
    pub fn systems(&self) -> Ref<'_, TypeMap<Option<SystemHolder>>> {
        self.systems.borrow()
    }

    /// Mutably obtains the system list. This will panic if it was already borrowed.
    pub fn systems_mut(&self) -> RefMut<'_, TypeMap<Option<SystemHolder>>> {
        self.systems.borrow_mut()
    }
}

/// Stores a system instance and construction data.
struct SystemHolder {
    builder: Arc<dyn SystemBuilder>,
    system: Box<dyn Any>
}

impl SystemHolder {
    /// Creates a new system holder for the specified inner context and builder.
    pub fn new(inner: InnerContext, builder: Arc<dyn SystemBuilder>) -> Self {
        let system = builder.build(GeeseContextHandle::new(inner, builder.clone()));
        Self { builder, system }
    }

    /// Obtains a reference to the system builder.
    pub fn builder(&self) -> Arc<dyn SystemBuilder> {
        self.builder.clone()
    }

    /// Obtains a reference to the system instance.
    pub fn system(&self) -> &dyn Any {
        &*self.system
    }
    
    /// Mutably obtains a reference to the system instance.
    pub fn system_mut(&mut self) -> &mut dyn Any {
        &mut *self.system
    }
}

/// Stores information about system construction, dependencies, and event handlers.
trait SystemBuilder {
    /// Obtains the type ID of this system.
    fn system_id(&self) -> TypeId;
    /// Obtains builders for any dependencies of this system.
    fn dependencies(&self) -> &[Arc<dyn SystemBuilder>];
    /// Obtains all of the event handlers for the given event ID associated with this system.
    fn event_responders(&self) -> &[Arc<dyn EventHandler>];
    /// Creates a new instance of this system for the given context handle.
    fn build(&self, ctx: GeeseContextHandle) -> Box<dyn Any>;
}

impl Eq for dyn SystemBuilder {}
impl PartialEq for dyn SystemBuilder {
    fn eq(&self, other: &Self) -> bool {
        self.system_id() == other.system_id()
    }
}

impl Hash for dyn SystemBuilder {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.system_id().hash(state)
    }
}

/// Stores typed information for system construction and dependencies.
struct TypedSystemBuilder<S: GeeseSystem> {
    dependencies: Vec<Arc<dyn SystemBuilder>>,
    event_handlers: Vec<Arc<dyn EventHandler>>,
    phantom: PhantomData<S>
}

impl<S: GeeseSystem> TypedSystemBuilder<S> {
    /// Creates a new typed builder for the current system.
    pub fn new() -> Self {
        let mut entry = GeeseSystemData::new();
        S::register(&mut entry);
        let dependencies = entry.dependencies.into_iter().map(|(_, x)| Arc::from(x)).collect();

        Self {
            dependencies,
            event_handlers: entry.events,
            phantom: Default::default()
        }
    }
}

impl<S: GeeseSystem> SystemBuilder for TypedSystemBuilder<S> {
    fn system_id(&self) -> TypeId {
        TypeId::of::<S>()
    }
    
    fn dependencies(&self) -> &[Arc<dyn SystemBuilder>] {
        &self.dependencies[..]
    }

    fn event_responders(&self) -> &[Arc<dyn EventHandler>] {
        &self.event_handlers[..]
    }

    fn build(&self, ctx: GeeseContextHandle) -> Box<dyn Any> {
        Box::new(S::new(ctx))
    }
}

/// Provides the ability to handle a strongly-typed event for a system.
trait EventHandler {
    /// The system ID of the event.
    fn system_id(&self) -> TypeId;
    /// The type ID of the event.
    fn event_id(&self) -> TypeId;
    /// Invokes the event handler for the given system and event. Panics if the system or event is not of the correct type.
    fn respond(&self, system: &mut dyn Any, event: &dyn Any);
}

/// Provides the ability to call a function in response to an event.
struct TypedEventHandler<S: GeeseSystem, E: 'static, F>(F, PhantomData<(S, E)>) where F : Fn(&mut S, &E);

impl<S: GeeseSystem, E: 'static, F: Fn(&mut S, &E)> TypedEventHandler<S, E, F> {
    /// Creates a new event handler for the specified function.
    pub fn new(f: F) -> Self {
        Self(f, PhantomData::default())
    }
}

impl<S: GeeseSystem, E: 'static, F: Fn(&mut S, &E)> EventHandler for TypedEventHandler<S, E, F> {
    fn system_id(&self) -> TypeId {
        TypeId::of::<S>()
    }

    fn event_id(&self) -> TypeId {
        TypeId::of::<E>()
    }

    fn respond(&self, system: &mut dyn Any, event: &dyn Any) {
        (self.0)(system.downcast_mut().expect("System was of the incorrect type."), event.downcast_ref().expect("Event was of the incorrect type."));
    }
}

/// Represents an immutable reference to a system.
pub struct SystemRef<'a, T: ?Sized> {
    inner: Ref<'a, T>
}

impl<'a, T: ?Sized> SystemRef<'a, T> {
    /// Creates a new immutable reference to a system from a
    /// `std::cell::RefCell` borrow.
    fn new(inner: Ref<'a, T>) -> Self {
        Self { inner }
    }

    /// Creates a reference to a specific borrowed component of a system.
    pub fn map<U, F>(orig: SystemRef<'a, T>, f: F) -> SystemRef<'a, U> where F: FnOnce(&T) -> &U, U: ?Sized {
        SystemRef::new(Ref::map(orig.inner, f))
    }
}

impl<'a, T: ?Sized> Deref for SystemRef<'a, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

/// Represents a mutable reference to a system.
pub struct SystemRefMut<'a, T: ?Sized> {
    inner: RefMut<'a, T>
}

impl<'a, T: ?Sized> SystemRefMut<'a, T> {
    /// Creates a new mutable reference to a system from a
    /// `std::cell::RefCell` borrow.
    fn new(inner: RefMut<'a, T>) -> Self {
        Self { inner }
    }

    /// Creates a reference to a specific borrowed component of a system.
    pub fn map<U, F>(orig: SystemRefMut<'a, T>, f: F) -> SystemRefMut<'a, U> where F: FnOnce(&mut T) -> &mut U, U: ?Sized {
        SystemRefMut::new(RefMut::map(orig.inner, f))
    }
}

impl<'a, T: ?Sized> Deref for SystemRefMut<'a, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl<'a, T: ?Sized> DerefMut for SystemRefMut<'a, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

/// Provides events to which the Geese context responds.
pub mod notify {
    use super::*;

    /// Causes Geese to load a system during the next event cycle. The context will panic if the system was already present.
    pub struct AddSystem {
        pub(super) builder: Arc<dyn SystemBuilder>
    }

    impl AddSystem {
        /// Tells Geese to load the specified system when this event triggers.
        pub fn new<S: GeeseSystem>() -> Self {
            let builder = Arc::new(TypedSystemBuilder::<S>::new());
            Self { builder }
        }
    }

    /// Causes Geese to remove a system during the next event cycle. The context will panic if the system was not present.
    pub struct RemoveSystem {
        pub(super) type_id: TypeId
    }

    impl RemoveSystem {
        /// Tells Geese to unload the specified system when this event triggers.
        pub fn new<S: GeeseSystem>() -> Self {
            let type_id = TypeId::of::<S>();
            Self { type_id }
        }
    }

    /// Causes Geese to reload a system during the next event cycle. The context will panic if the system was not present.
    pub struct ResetSystem {
        pub(super) type_id: TypeId
    }

    impl ResetSystem {
        /// Tells Geese to reset the specified system when this event triggers.
        pub fn new<S: GeeseSystem>() -> Self {
            let type_id = TypeId::of::<S>();
            Self { type_id }
        }
    }

    /// Causes the context to delay processing the given event during
    /// the current flush cycle.
    pub struct Delayed(pub(super) Cell<Option<Box<dyn Any>>>);

    impl Delayed {
        /// Tells the context to delay processing this event until all of the other events
        /// placed into the queue before the next flush call have been processed.
        pub fn new<T: 'static>(event: T) -> Self {
            Self(Cell::new(Some(Box::new(event))))
        }
    }

    /// Instructs the Geese context to process all events that were
    /// raised until this point in the queue before moving on.
    pub struct Flush;
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::atomic::*;

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
        fn new(_: GeeseContextHandle) -> Self {
            Self
        }

        fn register(entry: &mut GeeseSystemData<Self>) {
            entry.event(Self::increment);
        }
    }

    struct B {
        ctx: GeeseContextHandle
    }

    impl B {
        fn test_answer(&mut self, event: &Arc<AtomicBool>) {
            event.store(self.ctx.system::<A>().answer(), Ordering::Relaxed);
        }
    }

    impl GeeseSystem for B {
        fn new(ctx: GeeseContextHandle) -> Self {
            ctx.raise_event(());
            Self { ctx }
        }

        fn register(entry: &mut GeeseSystemData<Self>) {
            entry.dependency::<A>();

            entry.event(Self::test_answer);
        }
    }

    struct C {
        counter: usize
    }

    impl C {
        pub fn counter(&self) -> usize {
            self.counter
        }

        fn increment_counter(&mut self, _: &()) {
            self.counter += 1;
        }
    }

    impl GeeseSystem for C {
        fn new(_: GeeseContextHandle) -> Self {
            Self { counter: 0 }
        }

        fn register(with: &mut GeeseSystemData<Self>) {
            with.event(Self::increment_counter);
        }
    }

    #[test]
    fn test_single_system() {
        let ab = Arc::new(AtomicUsize::new(0));
        let mut ctx = GeeseContext::default();
        ctx.raise_event(notify::AddSystem::new::<A>());
        ctx.raise_event(ab.clone());
        ctx.flush_events();
        assert!(ab.load(Ordering::Relaxed) == 1);
    }

    #[test]
    fn test_dependent_system() {
        let ab = Arc::new(AtomicBool::new(false));
        let mut ctx = GeeseContext::default();
        ctx.raise_event(notify::AddSystem::new::<B>());
        ctx.raise_event(ab.clone());
        ctx.flush_events();
        assert!(ab.load(Ordering::Relaxed));
    }

    #[test]
    fn test_system_reload() {
        let mut ctx = GeeseContext::default();
        ctx.raise_event(notify::AddSystem::new::<C>());
        ctx.raise_event(notify::AddSystem::new::<B>());
        assert_eq!(ctx.system::<C>().counter(), 1);
        ctx.raise_event(notify::ResetSystem::new::<A>());
        assert_eq!(ctx.system::<C>().counter(), 2);
    }

    #[test]
    #[should_panic]
    fn test_add_system_event_twice_panic() {
        let mut ctx = GeeseContext::default();
        ctx.raise_event(notify::AddSystem::new::<B>());
        ctx.raise_event(notify::AddSystem::new::<B>());
        ctx.flush_events();
    }

    #[test]
    #[should_panic]
    fn test_remove_system_event_unknown_panic() {
        let mut ctx = GeeseContext::default();
        ctx.raise_event(notify::RemoveSystem::new::<B>());
        ctx.flush_events();
    }
}