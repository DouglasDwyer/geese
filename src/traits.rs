use crate::*;
use crate::const_list::*;
use private::*;

pub trait GeeseSystem: 'static + Sized {
    const DEPENDENCIES: Dependencies;
    const EVENT_HANDLERS: EventHandlers<Self>;

    fn new(ctx: GeeseContextHandle<Self>) -> Self;
}

pub(super) struct SystemValidator {
    dependencies: &'static Dependencies
}

impl SystemValidator {
    pub const fn validate<S: GeeseSystem>() {
        Self { dependencies: &S::DEPENDENCIES }.validate_list();
    }

    const fn validate_list(&self) {
        let mut i = 0;
        while i < self.dependencies.len() {
            Self { dependencies: self.dependencies.get(i).dependencies }.validate_list();
            i += 1;
        }

        assert!(!self.has_duplicate_dependencies(), "System had one or more duplicate dependencies.");
        //assert!(!self.has_conflicting_mutable_paths(), "System dependencies had conflicting borrows: cannot borrow system as mutable more than once in the dependency graph.");
    }

    const fn has_conflicting_mutable_paths(&self) -> bool {
        Self::conflicting_dependency_paths(&ConstList::new().push(self.dependencies.0), &ConstList::new())
    }

    const fn conflicting_dependency_paths<'a>(dependencies: &'a ConstList<'a, ConstList<'a, DependencyHolder>>, already_seen: &'a ConstList<'a, DependencyHolder>) -> bool {
        let (begin, rest) = dependencies.pop();
        if let Some(list) = begin {
            let (dependency, other_dependencies) = list.pop();
            
            if let Some(first) = dependency {
                if Self::has_conflicting_dependency(first, already_seen) {
                    true
                }
                else {
                    Self::conflicting_dependency_paths(&rest.push(*other_dependencies).push(first.dependencies.0), &already_seen.push(*first))
                }
            }
            else {
                Self::conflicting_dependency_paths(rest, already_seen)
            }
        }
        else {
            false
        }
    }

    const fn has_duplicate_dependencies(&self) -> bool {
        let mut i = 0;
    
        while i < self.dependencies.len() {
            let mut j = i + 1;
    
            while j < self.dependencies.len() {
                if self.dependencies.get(i).dependency_id().eq(&self.dependencies.get(j).dependency_id()) {
                    return true;
                }
    
                j += 1;
            }
    
            i += 1;
        }
    
        false
    }

    const fn has_conflicting_dependency(holder: &DependencyHolder, list: &ConstList<'_, DependencyHolder>) -> bool {
        let mut i = 0;

        while i < list.len() {
            if let Some(other) = list.get(i) {
                if holder.dependency_id().eq(&other.dependency_id()) && (holder.mutable || other.mutable) {
                    return true;
                }
            }
            else {
                panic!("Index was out-of-bounds.");
            }

            i += 1;
        }

        false
    }
}

pub struct Dependencies(pub(super) ConstList<'static, DependencyHolder>);

impl Dependencies {
    pub const fn new() -> Self {
        Self(ConstList::new())
    }

    pub const fn with<S: Dependency>(&'static self) -> Self {
        Self(self.0.push(DependencyHolder::new::<S>()))
    }

    pub(super) const fn get(&self, index: usize) -> &DependencyHolder {
        if let Some(value) = self.0.get(index) {
            value
        }
        else {
            panic!("Index out-of-range.");
        }
    }

    pub(super) const fn index_of(&self, id: ConstTypeId) -> Option<usize> {
        let mut i = 0;
        while i < self.0.len() {
            if let Some(value) = self.0.get(i) {
                if value.dependency_id().eq(&id) {
                    return Some(i);
                }
            }
            else {
                panic!("Index was out-of-range.");
            }

            i += 1;
        }

        None
    }

    pub(super) const fn len(&self) -> usize {
        self.0.len()
    }
}

#[derive(Copy, Clone)]
pub(crate) struct DependencyHolder {
    descriptor_getter: fn() -> Box<dyn SystemDescriptor>,
    dependencies: &'static Dependencies,
    mutable: bool,
    type_id: ConstTypeId,
}

impl DependencyHolder {
    pub const fn new<S: Dependency>() -> Self {
        Self {
            descriptor_getter: Self::get_descriptor::<S::System>,
            dependencies: &S::System::DEPENDENCIES,
            mutable: S::MUTABLE,
            type_id: ConstTypeId::of::<S::System>()
        }
    }

    pub const fn dependency_id(&self) -> ConstTypeId {
        self.type_id
    }

    pub const fn mutable(&self) -> bool {
        self.mutable
    }

    pub fn descriptor(&self) -> Box<dyn SystemDescriptor> {
        (self.descriptor_getter)()
    }

    fn get_descriptor<S: GeeseSystem>() -> Box<dyn SystemDescriptor> {
        Box::new(TypedSystemDescriptor::<S>::default())
    }
}

pub(super) trait SystemDescriptor: 'static + Send + Sync {
    fn create(&self, inner: Arc<ContextHandleInner>) -> Box<dyn Any>;
    fn create_handle_data(&self, context_id: u16) -> Arc<ContextHandleInner>;
    fn dependencies(&self) -> &'static Dependencies;
    fn system_id(&self) -> TypeId;
}

pub struct TypedSystemDescriptor<S: GeeseSystem>(PhantomData<fn(S)>);

impl<S: GeeseSystem> Default for TypedSystemDescriptor<S> {
    fn default() -> Self {
        Self(PhantomData)
    }
}

impl<S: GeeseSystem> SystemDescriptor for TypedSystemDescriptor<S> {
    fn create(&self, inner: Arc<ContextHandleInner>) -> Box<dyn Any> {
        Box::new(S::new(GeeseContextHandle::new(inner)))
    }

    fn create_handle_data(&self, context_id: u16) -> Arc<ContextHandleInner> {
        let dependencies = static_eval!(S::DEPENDENCIES.len(), usize, S);
        Arc::new(ContextHandleInner { context_id, id: Cell::new(context_id), dependency_ids: smallvec!(Cell::default(); dependencies) })
    }

    fn dependencies(&self) -> &'static Dependencies {
        &S::DEPENDENCIES
    }

    fn system_id(&self) -> TypeId {
        TypeId::of::<S>()
    }
}

pub struct EventHandlers<S: GeeseSystem>(ConstList<'static, EventHandler<S>>);

impl<S: GeeseSystem> EventHandlers<S> {
    pub const fn new() -> Self {
        Self(ConstList::new())
    }

    pub const fn with<Q: MutableRef<S>, T: 'static + Send + Sync>(&'static self, handler: fn(Q, &T)) -> Self {
        Self(self.0.push(EventHandler::new(handler)))
    }
}

struct EventHandler<S: GeeseSystem> {
    event_id: fn() -> TypeId,
    handler: fn(*mut S, *const ()),
}

impl<S: GeeseSystem> EventHandler<S> {
    pub const fn new<Q: MutableRef<S>, T: 'static + Send + Sync>(handler: fn(Q, &T)) -> Self {
        unsafe {
            Self {
                event_id: TypeId::of::<T>,
                handler: transmute(handler)
            }
        }
    }

    pub fn event_id(&self) -> TypeId {
        (self.event_id)()
    }

    pub fn handle(&self, system: &mut S, event: *const ()) {
        unsafe {
            (transmute::<_, fn(&mut S, *const ())>(self.handler))(system, event)
        }
    }
}

pub struct Mut<S: GeeseSystem>(PhantomData<fn(S)>);

impl<S: GeeseSystem> Dependency for Mut<S> {
    type System = S;

    const MUTABLE: bool = true;
}

impl<S: GeeseSystem> Dependency for S {
    type System = S;

    const MUTABLE: bool = false;
}

mod private {
    use super::*;

    pub trait Dependency {
        type System: GeeseSystem;
        const MUTABLE: bool;
    }

    pub trait MutableRef<T> {}
    impl<'a, T> MutableRef<T> for &'a mut T {}
}