use crate::*;
use crate::const_list::*;
use crate::const_type_id::*;
use std::any::*;
use std::mem::*;
use private::*;

/// Dynamically determines whether a given type implements the provided trait.
macro_rules! implements {
    ($name: ident, $trait: ident) => {
        {
            use std::cell::*;

            struct TraitTest<'a, T: ?Sized> {
                is_trait: &'a Cell<bool>,
                data: PhantomData<T>,
            }
    
            impl<T: ?Sized> Clone for TraitTest<'_, T> {
                #[inline(always)]
                fn clone(&self) -> Self {
                    self.is_trait.set(false);
                    TraitTest {
                        is_trait: self.is_trait,
                        data: PhantomData,
                    }
                }
            }

            impl<T: ?Sized + $trait> Copy for TraitTest<'_, T> {}
    
            let is_trait = Cell::new(true);
    
            _ = [TraitTest::<$name> {
                is_trait: &is_trait,
                data: PhantomData,
            }]
            .clone();
    
            is_trait.get()
        }
    };
}

/// Represents a collection of event handlers with internal state.
pub trait GeeseSystem: 'static + Sized {
    /// The set of dependencies that this system has.
    const DEPENDENCIES: Dependencies = Dependencies::new();
    
    /// The set of events to which this system responds.
    const EVENT_HANDLERS: EventHandlers<Self> = EventHandlers::new();

    /// Creates a new instance of the system for the given system handle.
    fn new(ctx: GeeseContextHandle<Self>) -> Self;
}

/// Denotes a list of system dependencies.
#[derive(Copy, Clone, Debug)]
pub struct Dependencies {
    /// The inner list of dependencies.
    inner: ConstList<'static, DependencyHolder>
}

impl Dependencies {
    /// Creates a new, empty list of dependencies.
    pub const fn new() -> Self {
        Self {
            inner: ConstList::new()
        }
    }

    /// Adds the given type to the dependency list, returning the modified list.
    pub const fn with<S: Dependency>(&'static self) -> Self {
        Self {
            inner: self.inner.push(DependencyHolder::new::<S>())
        }
    }

    /// Gets a reference to the inner list of dependency holders.
    pub(crate) const fn as_inner(&self) -> &ConstList<'static, DependencyHolder> {
        &self.inner
    }

    /// Determines the local index in this dependency list of the provided system.
    pub(crate) const fn index_of<S: GeeseSystem>(&self) -> Option<usize> {
        let mut i = 0;
        while i < self.inner.len() {
            if const_unwrap(self.inner.get(i)).dependency_id().eq(&ConstTypeId::of::<S>()) {
                return Some(i);
            }
            i += 1;
        }
        None
    }
}

/// Describes a system dependency.
#[derive(Copy, Clone, Debug)]
pub(crate) struct DependencyHolder {
    /// A function which retrieves a descriptor at runtime.
    descriptor_getter: fn() -> Box<dyn SystemDescriptor>,
    /// The list of this dependency's subdependencies.
    dependencies: &'static Dependencies,
    /// Whether this dependency may be mutably borrowed.
    mutable: bool,
    /// The type ID of the system.
    type_id: ConstTypeId,
}

impl DependencyHolder {
    /// Creates a holder for the provided dependency.
    pub const fn new<S: Dependency>() -> Self {
        Self {
            descriptor_getter: Self::get_descriptor::<S::System>,
            dependencies: &S::System::DEPENDENCIES,
            mutable: S::MUTABLE,
            type_id: ConstTypeId::of::<S::System>()
        }
    }

    /// Gets the type ID of this dependency.
    pub const fn dependency_id(&self) -> ConstTypeId {
        self.type_id
    }

    /// Determines whether this dependency may be mutably borrowed.
    pub const fn mutable(&self) -> bool {
        self.mutable
    }

    /// Gets a descriptor for use with system instantiation.
    pub fn descriptor(&self) -> Box<dyn SystemDescriptor> {
        (self.descriptor_getter)()
    }

    /// Creates a descriptor for instantiation with the given system.
    fn get_descriptor<S: GeeseSystem>() -> Box<dyn SystemDescriptor> {
        Box::<TypedSystemDescriptor<S>>::default()
    }
}

/// Denotes a list of system methods that respond to events.
#[allow(unused_variables)]
pub struct EventHandlers<S: GeeseSystem> {
    /// The inner list of event handlers.
    inner: ConstList<'static, EventHandler>,
    /// Phantom data to mark the system as used.
    data: PhantomData<fn(S)>
}

impl<S: GeeseSystem> EventHandlers<S> {
    /// Creates a new, empty list of event handlers.
    pub const fn new() -> Self {
        Self {
            inner: ConstList::new(),
            data: PhantomData
        }
    }

    /// Adds the given event handler to the list, returning the modified list.
    pub const fn with<Q: MutableRef<S>, T: 'static + Send + Sync>(&'static self, handler: fn(Q, &T)) -> Self {
        Self {
            inner: self.inner.push(EventHandler::new(handler)),
            data: PhantomData
        }
    }

    /// Gets a reference to the inner list of event handlers.
    fn as_inner(&self) -> &ConstList<'_, EventHandler> {
        &self.inner
    }
}

/// Describes an event handler for a system.
#[derive(Copy, Clone, Debug)]
pub(crate) struct EventHandler {
    /// A function that retrieves the type ID of the event.
    event_id: fn() -> TypeId,
    /// A reference to the event handler function.
    handler: EventInvoker,
}

impl EventHandler {
    /// Creates a new event handler to wrap the given function pointer.
    pub const fn new<S: GeeseSystem, Q: MutableRef<S>, T: 'static + Send + Sync>(handler: fn(Q, &T)) -> Self {
        Self {
            event_id: TypeId::of::<T>,
            handler: EventInvoker::new(handler)
        }
    }

    /// Gets the type ID of the event to which this handler responds.
    pub fn event_id(&self) -> TypeId {
        (self.event_id)()
    }

    /// Obtains a reference to the event handler function.
    pub fn handler(&self) -> &EventInvoker {
        &self.handler
    }
}

/// Provides the ability to invoke an event handler method.
#[derive(Copy, Clone, Debug)]
pub(crate) struct EventInvoker {
    /// A function that casts the event and handler function to a concrete type,
    /// and then invokes the handler.
    pointer_flattener: unsafe fn(*mut (), &dyn Any, *const ()),
    /// The handler associated with this event invoker.
    handler: *const ()
}

impl EventInvoker {
    /// Creates a new event invoker to wrap the given function pointer.
    pub const fn new<S: GeeseSystem, Q: MutableRef<S>, T: 'static + Send + Sync>(handler: fn(Q, &T)) -> Self {
        unsafe {
            Self {
                pointer_flattener: Self::pointer_flattener::<T>,
                handler: handler as *const ()
            }
        }
    }

    /// Invokes the event using the given system pointer and event value.
    /// 
    /// # Safety
    /// 
    /// For this function to be sound, the system pointer must reference a valid
    /// instance of the system type associated with this event handler. No other references
    /// to the system must exist. Further, the provided value must be of the event type
    /// associated with this event handler.
    pub unsafe fn invoke(&self, system: *mut (), value: &dyn Any) {
        (self.pointer_flattener)(system, value, self.handler);
    }

    /// Invokes the provided pointer as a function handle with the given system and value as arguments.
    /// 
    /// # Safety
    /// 
    /// The pointer to run must be a valid event handler function that accepts the system and value
    /// as arguments. These must both refer to valid objects of the correct system and event type.
    unsafe fn pointer_flattener<T: 'static + Send + Sync>(system: *mut (), value: &dyn Any, to_run: *const ()) {
        transmute::<_, fn(*mut (), &T)>(to_run)(system, value.downcast_ref().unwrap_unchecked())
    }
}

/// Describes a system's properties and allows it to be instantiated.
pub(super) trait SystemDescriptor: 'static + Send + Sync {
    /// Creates a new system instance for the provided handle.
    fn create(&self, handle: Arc<ContextHandleInner>) -> Box<dyn Any>;

    /// The set of dependencies that this system has.
    fn dependencies(&self) -> &'static Dependencies;

    /// The number of dependencies that this system has.
    fn dependency_len(&self) -> usize;

    /// The event handlers associated with this system.
    fn event_handlers(&self) -> &'static ConstList<'static, EventHandler>;

    /// Whether this type may be safely sent across threads.
    fn is_send(&self) -> bool;

    /// Whether references to this type may be safely shared across threads.
    fn is_sync(&self) -> bool;

    /// Gets the type ID associated with the given system.
    fn system_id(&self) -> TypeId;
}

/// Describes a certain system type's properties and allows it to be instantiated.
pub(crate) struct TypedSystemDescriptor<S: GeeseSystem>(PhantomData<fn(S)>);

impl<S: GeeseSystem> Default for TypedSystemDescriptor<S> {
    fn default() -> Self {
        Self(PhantomData)
    }
}

impl<S: GeeseSystem> SystemDescriptor for TypedSystemDescriptor<S> {
    fn create(&self, handle: Arc<ContextHandleInner>) -> Box<dyn Any> {
        Box::new(S::new(GeeseContextHandle::new(handle)))
    }

    fn dependencies(&self) -> &'static Dependencies {
        &S::DEPENDENCIES
    }

    fn dependency_len(&self) -> usize {
        const_eval!(S::DEPENDENCIES.as_inner().len(), usize, S)
    }

    fn event_handlers(&self) -> &'static ConstList<'static, EventHandler> {
        S::EVENT_HANDLERS.as_inner()
    }

    fn is_send(&self) -> bool {
        implements!(S, Send)
    }

    fn is_sync(&self) -> bool {
        implements!(S, Sync)
    }

    fn system_id(&self) -> TypeId {
        TypeId::of::<S>()
    }
}

/// Marks that a dependency may be mutably borrowed.
pub struct Mut<S: GeeseSystem>(PhantomData<fn(S)>);

/// Determines whether the given list of dependencies, or any subdependency lists,
/// have unnecessary duplicates.
pub(crate) const fn has_duplicate_dependencies(dependencies: &Dependencies) -> bool {
    let inner_deps = dependencies.as_inner();

    let mut i = 0;

    while i < inner_deps.len() {
        if has_duplicate_dependencies(const_unwrap(inner_deps.get(i)).dependencies) {
            return true;
        }

        let mut j = i + 1;

        while j < inner_deps.len() {
            if const_unwrap(inner_deps.get(i)).dependency_id().eq(&const_unwrap(inner_deps.get(j)).dependency_id()) {
                return true;
            }

            j += 1;
        }

        i += 1;
    }

    false
}

/// Hides traits from being externally visible.
mod private {
    use super::*;

    /// Describes one system's dependency on another.
    pub trait Dependency {
        /// The underlying type of the dependency.
        type System: GeeseSystem;

        /// Whether the dependency may be mutably borrowed.
        const MUTABLE: bool;
    }

    impl<S: GeeseSystem> Dependency for S {
        type System = S;

        const MUTABLE: bool = false;
    }

    impl<S: GeeseSystem> Dependency for Mut<S> {
        type System = S;

        const MUTABLE: bool = true;
    }

    /// Trait that marks a type as a mutable reference. This is used to
    /// hide mutable references from `const` functions, so that they may
    /// be manipulated in a `const` context.
    pub trait MutableRef<T> {}

    impl<'a, T> MutableRef<T> for &'a mut T {}

}