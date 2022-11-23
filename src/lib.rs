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
//!     ctx.raise_event(event::NotifyAddSystem::new::<B>());
//!     ctx.raise_event(ab.clone());
//!     ctx.flush_events();
//!     assert!(ab.load(Ordering::Relaxed));
//! }
//! ```

use std::any::*;
use std::cell::*;
use std::collections::*;
use std::hash::*;
use std::marker::*;
use std::sync::*;
use std::ops::*;

use topological_sort::*;

/// Represents a collection of event handlers with internal state.
pub trait GeeseSystem: 'static {
    /// Creates a new instance of the system for the given system handle.
    fn new(ctx: GeeseContextHandle) -> Self;
    /// Registers the event handlers and dependencies of this system.
    fn register(_: &mut GeeseSystemData<Self>) {}
}

/// Stores event handler data for a system.
pub struct GeeseSystemData<S: GeeseSystem + ?Sized> {
    dependencies: HashMap<TypeId, Box<dyn SystemBuilder>>,
    events: Vec<Box<dyn EventHandler>>,
    phantom: PhantomData<S>
}

impl<S: GeeseSystem> GeeseSystemData<S> {    
    /// Registers an event handler for the given system.
    pub fn event<E: 'static>(&mut self, handler: fn(&mut S, &E)) {
        self.events.push(Box::new(handler));
    }

    /// Sets the given system as a dependency of this one.
    pub fn dependency<Q: GeeseSystem>(&mut self) {
        let builder = Box::new(TypedSystemBuilder::<Q>::new());
        assert!(self.dependencies.insert(builder.system_id(), builder).is_none(), "Cannot add duplicate dependencies");
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

    /// Raises the specified event.
    pub fn raise_event<T: 'static>(&self, event: T) {
        self.inner.events_mut().push_back(Box::new(event));
    }

    /// Obtains the specified system dependency.
    pub fn system<'a, T: GeeseSystem>(&'a self) -> SystemRef<'a, T> {
        assert!(self.system.dependencies().iter().any(|x| x.system_id() == TypeId::of::<T>()), "The specified system was not a declared dependency of this one.");
        SystemRef::new(Ref::map(self.inner.systems(), |x| {
            let holder = x.get(&TypeId::of::<T>()).expect("System was not loaded.").as_ref().expect("Cannot obtain a reference to the current system.");
            holder.system().downcast_ref::<T>().expect("System was not of the correct type.")
        }))
    }
}

/// Represents a collection of systems that can create and respond to events.
#[derive(Default)]
pub struct GeeseContext {
    active_systems: HashMap<TypeId, Arc<dyn SystemBuilder>>,
    dirty: bool,
    inner: InnerContext,
}

impl GeeseContext {
    /// Places the given event in the system event queue.
    pub fn raise_event<T: 'static>(&mut self, event: T) {
        self.inner.events.borrow_mut().push_back(Box::new(event));
    }

    /// Causes an event cycle to complete by running systems until the event queue is empty.
    pub fn flush_events(&mut self) {
        while let Some(event) = self.next_event() {
            self.handle_event(&*event);
        }
    }

    /// Immediately adds the specified system to the context.
    pub fn add_system<S: GeeseSystem>(&mut self) {
        self.queue_add_system(Arc::new(TypedSystemBuilder::<S>::new()));
        self.reload_dirty_systems();
    }

    /// Immediately removes the specified system from the context.
    pub fn remove_system<S: GeeseSystem>(&mut self) {
        self.queue_remove_system(TypeId::of::<S>());
        self.reload_dirty_systems();
    }

    /// Obtains a reference to the given system.
    pub fn system<S: GeeseSystem>(&self) -> SystemRef<'_, S> {
        SystemRef::new(Ref::map(self.inner.systems(), |x|
            x.get(&TypeId::of::<S>()).expect("System not found.")
                .as_ref().expect("System was currently borrowed.")
                .system().downcast_ref().expect("System was not of the correct type.")))
    }

    /// Mutably obtains a reference to the given system.
    pub fn system_mut<S: GeeseSystem>(&mut self) -> SystemRefMut<'_, S> {
        SystemRefMut::new(RefMut::map(self.inner.systems_mut(), |x|
            x.get_mut(&TypeId::of::<S>()).expect("System not found.")
                .as_mut().expect("System was currently borrowed.")
                .system_mut().downcast_mut().expect("System was not of the correct type.")))
    }

    /// Gets the next event in the event queue.
    fn next_event(&self) -> Option<Box<dyn Any>> {
        self.inner.events.borrow_mut().pop_front()
    }

    /// Handles an event by broadcasting it to all systems.
    fn handle_event(&mut self, event: &dyn Any) {
        if event.type_id() == TypeId::of::<event::NotifyAddSystem>() {
            self.queue_add_system(event.downcast_ref::<event::NotifyAddSystem>().expect("Event type ID did not match type").builder.clone());
        }
        else if event.type_id() == TypeId::of::<event::NotifyRemoveSystem>() {
            self.queue_remove_system(event.downcast_ref::<event::NotifyRemoveSystem>().expect("Event type ID did not match type").type_id);
        }
        else {
            self.reload_dirty_systems();
        }

        for id in self.system_ids() {
            let mut system = std::mem::replace(self.inner.systems_mut().get_mut(&id).expect("Key was not found in map."), None);
            system.as_mut().expect("Attempted to send events to a borrowed system.").apply_event(&*event);
            self.inner.systems_mut().insert(id, system);
        }
    }

    fn system_ids(&self) -> impl Iterator<Item = TypeId> {
        self.inner.systems().keys().cloned().collect::<Vec<_>>().into_iter()
    }
    
    /// Activates and deactivates the appropriate systems if they have changed.
    fn reload_dirty_systems(&mut self) {
        if self.dirty {
            self.dirty = false;
            
            let activation_order = Self::determine_system_order(self.active_systems.values()).expect("Cannot instantiate systems due to a cyclic dependency.");
            let deactivation_order =
                Self::determine_system_order(self.inner.systems().values().map(|x| x.as_ref().expect("Could not borrow system, as it was already borrowed.").builder()).collect::<Vec<_>>().iter())
                    .expect("Cannot deactive systems due to a cyclic dependency.");

            for system in &activation_order {
                if !self.inner.systems_mut().contains_key(&system.system_id()) {
                    self.inner.systems_mut().insert(system.system_id(), Some(SystemHolder::new(self.inner.clone(), system.clone())));
                }
            }

            for system in deactivation_order.iter().rev() {
                if !activation_order.iter().map(|x| (**x).system_id()).collect::<Vec<_>>().contains(&system.system_id()) {
                    drop(self.inner.systems_mut().remove(&system.system_id()));
                }
            }
        }
    }

    /// Adds a system for use in the current Geese context.
    fn queue_add_system(&mut self, builder: Arc<dyn SystemBuilder>) {
        if self.active_systems.contains_key(&builder.system_id()) {
            panic!("Attempted to add duplicate system.");
        }
        self.active_systems.insert(builder.system_id(), builder);
        self.dirty = true;
    }

    /// Removes a system from use in the current Geese context.
    fn queue_remove_system(&mut self, type_id: TypeId) {
        self.active_systems.remove(&type_id).expect("Attempted to remove unknown system.");
        self.dirty = true;
    }

    /// Determines the topological load ordering for the set of systems and all of their dependencies. Returns
    /// none if a cyclic dependency is found.
    fn determine_system_order<'a>(systems: impl Iterator<Item = &'a Arc<dyn SystemBuilder>>) -> Option<Vec<Arc<dyn SystemBuilder>>> {
        let mut ts = TopologicalSort::new();
        let mut required_systems = HashSet::new();
        let mut to_be_processed = systems.collect::<Vec<_>>();
        while let Some(x) = to_be_processed.pop() {
            ts.insert(x.clone());
            for dependency in x.dependencies() {
                if !required_systems.contains(&dependency.system_id()) {
                    required_systems.insert(dependency.system_id());
                    ts.add_dependency(x.clone(), dependency.clone());
                }
            }
        }

        let count = ts.len();
        let output = ts.into_iter().collect::<Vec<_>>();
        (count == output.len()).then(|| output)
    }
}

/// Stores shared information about a Geese context.
#[derive(Clone, Default)]
struct InnerContext {
    events: Arc<RefCell<VecDeque<Box<dyn Any>>>>,
    systems: Arc<RefCell<HashMap<TypeId, Option<SystemHolder>>>>
}

impl InnerContext {
    /// Mutably obtains the event queue. This will panic if it was already borrowed.
    pub fn events_mut(&self) -> RefMut<'_, VecDeque<Box<dyn Any>>> {
        self.events.borrow_mut()
    }

    /// Obtains the system list. This will panic if it was already mutably borrowed.
    pub fn systems(&self) -> Ref<'_, HashMap<TypeId, Option<SystemHolder>>> {
        self.systems.borrow()
    }

    /// Mutably obtains the system list. This will panic if it was already borrowed.
    pub fn systems_mut(&self) -> RefMut<'_, HashMap<TypeId, Option<SystemHolder>>> {
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

    /// Causes the system to respond to the current event.
    pub fn apply_event(&mut self, event: &dyn Any) {
        for resp in self.builder.event_responders(event.type_id()) {
            resp.respond(&mut *self.system, event);
        }
    }
}

/// Stores information about system construction, dependencies, and event handlers.
trait SystemBuilder {
    /// Obtains the type ID of this system.
    fn system_id(&self) -> TypeId;
    /// Obtains builders for any dependencies of this system.
    fn dependencies(&self) -> &[Arc<dyn SystemBuilder>];
    /// Obtains all of the event handlers for the given event ID associated with this system.
    fn event_responders(&self, event_id: TypeId) -> &[Box<dyn EventHandler>];
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
    event_handlers: HashMap<TypeId, Vec<Box<dyn EventHandler>>>,
    phantom: PhantomData<S>
}

impl<S: GeeseSystem> TypedSystemBuilder<S> {
    /// Creates a new typed builder for the current system.
    pub fn new() -> Self {
        let mut entry = GeeseSystemData::new();
        S::register(&mut entry);
        let dependencies = entry.dependencies.into_iter().map(|(_, x)| Arc::from(x)).collect();
        let mut event_handlers: HashMap<_, Vec<Box<dyn EventHandler>>> = HashMap::new();

        for event in entry.events {
            event_handlers.entry(event.event_id()).or_default().push(event);
        }

        Self {
            dependencies,
            event_handlers,
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

    fn event_responders(&self, event_id: TypeId) -> &[Box<dyn EventHandler>] {
        println!("Eve type {:?}", event_id);
        self.event_handlers.get(&event_id).map(|x| &x[..]).unwrap_or(&[])
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

impl<S: GeeseSystem, E: 'static> EventHandler for fn(&mut S, &E) {
    fn system_id(&self) -> TypeId {
        TypeId::of::<S>()
    }

    fn event_id(&self) -> TypeId {
        TypeId::of::<E>()
    }

    fn respond(&self, system: &mut dyn Any, event: &dyn Any) {
        (self)(system.downcast_mut().expect("System was of the incorrect type."), event.downcast_ref().expect("Event was of the incorrect type."));
    }
}

/// Represents an immutable reference to a system.
pub struct SystemRef<'a, T> {
    inner: Ref<'a, T>
}

impl<'a, T> SystemRef<'a, T> {
    fn new(inner: Ref<'a, T>) -> Self {
        Self { inner }
    }
}

impl<'a, T> Deref for SystemRef<'a, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &*self.inner
    }
}

/// Represents a mutable reference to a system.
pub struct SystemRefMut<'a, T> {
    inner: RefMut<'a, T>
}

impl<'a, T> SystemRefMut<'a, T> {
    fn new(inner: RefMut<'a, T>) -> Self {
        Self { inner }
    }
}

impl<'a, T> Deref for SystemRefMut<'a, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &*self.inner
    }
}

impl<'a, T> DerefMut for SystemRefMut<'a, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut *self.inner
    }
}

/// Provides events for interacting directly with the Geese context.
pub mod event {
    use super::*;

    /// Causes Geese to load a system during the next event cycle. The context will panic if the system was already present.
    pub struct NotifyAddSystem {
        pub(super) builder: Arc<dyn SystemBuilder>
    }

    impl NotifyAddSystem {
        /// Tells Geese to load the specified system when this event triggers.
        pub fn new<S: GeeseSystem>() -> Self {
            let builder = Arc::new(TypedSystemBuilder::<S>::new());
            Self { builder }
        }
    }

    /// Causes Geese to remove a system during the next event cycle. The context will panic if the system was not present.
    pub struct NotifyRemoveSystem {
        pub(super) type_id: TypeId
    }

    impl NotifyRemoveSystem {
        /// Tells Geese to unload the specified system when this event triggers.
        pub fn new<S: GeeseSystem>() -> Self {
            let type_id = TypeId::of::<S>();
            Self { type_id }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::atomic::*;

    struct A;

    impl A {
        fn increment(&mut self, event: &Arc<AtomicUsize>) {
            event.store(1, Ordering::Relaxed);
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
            Self { ctx }
        }

        fn register(entry: &mut GeeseSystemData<Self>) {
            entry.dependency::<A>();

            entry.event(Self::test_answer);
        }
    }

    #[test]
    fn test_single_system() {
        let ab = Arc::new(AtomicUsize::new(0));
        let mut ctx = GeeseContext::default();
        ctx.add_system::<A>();
        ctx.raise_event(ab.clone());
        ctx.flush_events();
        assert!(ab.load(Ordering::Relaxed) == 1);
    }

    #[test]
    fn test_dependent_system() {
        let ab = Arc::new(AtomicBool::new(false));
        let mut ctx = GeeseContext::default();
        ctx.raise_event(event::NotifyAddSystem::new::<B>());
        ctx.raise_event(ab.clone());
        ctx.flush_events();
        assert!(ab.load(Ordering::Relaxed));
    }
}