#![cfg_attr(feature = "static_check", feature(const_type_id))]

#![allow(warnings)]

/// Implements a compile-time linked list for describing variable-length data in `const` contexts.
mod const_list;

/// Provides the ability to generate and compare type IDs in a `const` context.
#[cfg_attr(static_check, path = "const_type_id/compiled.rs")]
#[cfg_attr(not(static_check), path = "const_type_id/runtime.rs")]
mod const_type_id;

/// Provides a macro for evaluating `const` code at either compile-time or runtime.
mod static_eval;

/// Provides the core traits for implementing and validating systems.
mod traits;

use bitvec::prelude::*;
use crate::const_list::*;
use crate::const_type_id::*;
use crate::static_eval::*;
pub use crate::traits::*;
use fxhash::*;
use smallvec::*;
use std::any::*;
use std::cell::*;
use std::hash::*;
use std::marker::*;
use std::mem::*;
use std::ops::*;
use std::sync::*;
use topological_sort::*;

pub struct GeeseContextHandle<S: GeeseSystem> {
    inner: Arc<ContextHandleInner>,
    data: PhantomData<fn(S)>
}

impl<S: GeeseSystem> GeeseContextHandle<S> {
    fn new(inner: Arc<ContextHandleInner>) -> Self {
        Self {
            inner,
            data: PhantomData
        }
    }

    /// Raises the specified dynamically-typed event.
    pub fn raise_event_boxed(&self, event: Box<dyn Any + Send + Sync>) {
        unsafe {
            (&mut *(*ExecutionContext::current(self.inner.context_id)).events.get()).push(event);
        }
    }

    /// Raises the specified event.
    pub fn raise_event<T: 'static + Send + Sync>(&self, event: T) {
        self.raise_event_boxed(Box::new(event));
    }

    pub fn get<T: GeeseSystem>(&self) -> SystemRef<T> {
        unsafe {
            let index = static_eval!(if let Some(index) = S::DEPENDENCIES.index_of(ConstTypeId::of::<T>()) { index } else { GeeseContextHandle::<S>::panic_on_invalid_dependency() }, usize, S, T);
            let global_index = self.inner.dependency_id(index as u16);
            SystemRef::new(self, global_index, ExecutionContext::current(self.inner.context_id))
        }
    }

    pub fn get_mut<T: GeeseSystem>(&mut self) -> SystemRefMut<T> {
        unsafe {
            let index = static_eval!({
                if let Some(index) = S::DEPENDENCIES.index_of(ConstTypeId::of::<T>()) {
                    assert!(S::DEPENDENCIES.get(index).mutable(), "Attempted to mutably access an immutable dependency.");
                    index
                }
                else {
                    GeeseContextHandle::<S>::panic_on_invalid_dependency()
                }
            }, usize, S, T);
            let global_index = self.inner.dependency_id(index as u16);
            SystemRefMut::new(self, global_index, ExecutionContext::current(self.inner.context_id))
        }
    }

    const fn panic_on_invalid_dependency() -> ! {
        panic!("The specified system was not a dependency of this one.");
    }
}

unsafe impl<S: GeeseSystem> Send for GeeseContextHandle<S> {}
unsafe impl<S: GeeseSystem> Sync for GeeseContextHandle<S> {}

/// Represents an immutable reference to a system.
pub struct SystemRef<'a, T: ?Sized> {
    context: *mut ExecutionContext,
    system: &'a T
}

impl<'a, T> SystemRef<'a, T> {
    /// Creates a new immutable reference to a system from a handle and context.
    unsafe fn new<S: GeeseSystem>(handle: &'a GeeseContextHandle<S>, index: u16, context: *mut ExecutionContext) -> Self {
        let context_value = &mut *context;
        context_value.borrow_count += 1;
        assert!((*context_value.available_systems).get_unchecked(index as usize), "Attempted to access inactive system in context.");
        let system = &*((*context_value.systems).get_unchecked(index as usize).assume_init_ref().value.get() as *const Box<dyn Any>);
        
        Self {
            context,
            system: transmute::<_, &(&'a T, *const ())>(system).0
        }
    }
}

impl<'a, T: ?Sized> SystemRef<'a, T> {
    /// Creates a reference to a specific borrowed component of a system.
    pub fn map<U, F>(orig: SystemRef<'a, T>, f: F) -> SystemRef<'a, U> where F: FnOnce(&T) -> &U, U: ?Sized {
        let result = SystemRef {
            context: orig.context,
            system: f(orig.system)
        };
        forget(orig);
        result
    }
}

impl<'a, T: ?Sized> Deref for SystemRef<'a, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.system
    }
}

impl<'a, T: ?Sized> Drop for SystemRef<'a, T> {
    fn drop(&mut self) {
        unsafe {
            (&mut *self.context).borrow_count -= 1;
        }
    }
}

/// Represents a mutable reference to a system.
pub struct SystemRefMut<'a, T: ?Sized> {
    context: *mut ExecutionContext,
    index: u16,
    system: &'a mut T
}

impl<'a, T> SystemRefMut<'a, T> {
    /// Creates a new mutable reference to a system from a handle and context.
    unsafe fn new<S: GeeseSystem>(handle: &'a mut GeeseContextHandle<S>, index: u16, context: *mut ExecutionContext) -> Self {
        let context_value = &mut *context;
        context_value.borrow_count += 1;
        assert!((*context_value.available_systems).get_unchecked(index as usize), "Attempted to access inactive system in context.");
        assert!(!context_value.mutable_borrows.replace_unchecked(index as usize, true), "Attempted to borrow the same system mutably multiple times.");
        let system = &mut *(*context_value.systems).get_unchecked(index as usize).assume_init_ref().value.get();
        
        Self {
            context,
            index,
            system: transmute::<_, &mut (&mut T, *const ())>(system).0
        }
    }
}

impl<'a, T: ?Sized> SystemRefMut<'a, T> {
    /// Creates a reference to a specific borrowed component of a system.
    pub fn map<U, F>(orig: SystemRefMut<'a, T>, f: F) -> SystemRefMut<'a, U> where F: FnOnce(&mut T) -> &mut U, U: ?Sized {
        unsafe {
            let original_reference = orig.system as *mut T;
            let context = orig.context;
            let index = orig.index;
            forget(orig);
    
            let result = SystemRefMut {
                context,
                index,
                system: f(&mut *original_reference)
            };
            result
        }
    }
}

impl<'a, T: ?Sized> Deref for SystemRefMut<'a, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.system
    }
}

impl<'a, T: ?Sized> DerefMut for SystemRefMut<'a, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.system
    }
}

impl<'a, T: ?Sized> Drop for SystemRefMut<'a, T> {
    fn drop(&mut self) {
        unsafe {
            let ctx = &mut *self.context;
            ctx.borrow_count -= 1;
            ctx.mutable_borrows.set(self.index as usize, false);
        }
    }
}

struct ContextHandleInner {
    context_id: u16,
    id: Cell<u16>,
    dependency_ids: SmallVec<[Cell<u16>; DEFAULT_DEPENDENCY_BUFFER_SIZE]>
}

impl ContextHandleInner {
    fn id(&self) -> u16 {
        self.id.get()
    }

    fn set_id(&self, value: u16) {
        self.id.set(value);
    }

    fn dependency_len(&self) -> u16 {
        self.dependency_ids.len() as u16
    }

    unsafe fn dependency_id(&self, index: u16) -> u16 {
        self.dependency_ids.get_unchecked(index as usize).get()
    }

    unsafe fn set_dependency_id(&self, index: u16, value: u16) {
        self.dependency_ids.get_unchecked(index as usize).set(value);
    }
}

const DEFAULT_CONTEXT_BUFFER_SIZE: usize = 4;
const DEFAULT_DEPENDENCY_BUFFER_SIZE: usize = 4;
const DEFAULT_EVENT_BUFFER_SIZE: usize = 8;

struct ExecutionContext {
    available_systems: *const BitVec,
    borrow_count: u16,
    context_id: u16,
    events: UnsafeCell<SmallVec<[Box<dyn Any + Send + Sync>; DEFAULT_EVENT_BUFFER_SIZE]>>,
    mutable_borrows: BitVec,
    systems: *const Vec<MaybeUninit<SystemHolder>>
}

impl ExecutionContext {
    thread_local! {
        static CONTEXTS: UnsafeCell<Vec<*mut ExecutionContext>> = const { UnsafeCell::new(Vec::new()) };
    }

    pub fn execute<T>(context_id: u16, available_systems: &BitVec, systems: *const Vec<MaybeUninit<SystemHolder>>, f: impl FnOnce() -> T) -> (T, SmallVec<[Box<dyn Any + Send + Sync>; DEFAULT_EVENT_BUFFER_SIZE]>) {
        unsafe {
            let mut ctx = ExecutionContext {
                available_systems,
                borrow_count: 0,
                context_id,
                events: UnsafeCell::default(),
                mutable_borrows: BitVec::repeat(false, (*systems).len()),
                systems
            };

            Self::add_context(&mut ctx);

            let res = f();

            Self::remove_context(context_id);
            (res, ctx.events.into_inner())
        }
    }

    pub fn current(context_id: u16) -> *mut ExecutionContext {
        unsafe {
            let contexts = &mut *Self::contexts();
            let index = contexts.iter().position(|x| (**x).context_id == context_id).expect("Attempted to access inactive context.");
            contexts.swap(0, index);
            *contexts.get_unchecked(0)
        }
    }

    unsafe fn add_context(ctx: *mut ExecutionContext) {
        let contexts = &mut *Self::contexts();
        let end = contexts.len();
        contexts.push(ctx);
        contexts.swap(0, end);
    }

    unsafe fn remove_context(context_id: u16) {
        let contexts = &mut *Self::contexts();
        let index = contexts.iter().position(|x| (**x).context_id == context_id).unwrap_unchecked();
        let final_position = contexts.len() - 1;
        contexts.swap(index, final_position);
        contexts.set_len(final_position);
    }

    fn contexts() -> *mut Vec<*mut ExecutionContext> {
        Self::CONTEXTS.with(|x| x.get())
    }
}

pub struct SystemManager {
    context_id: u16,
    system_descriptors: FxHashMap<TypeId, SystemState>,
    systems: Vec<MaybeUninit<SystemHolder>>
}

impl SystemManager {
    const DEFAULT_SYSTEM_PROCESSING_SIZE: usize = 8;

    pub fn add_system<S: GeeseSystem>(&mut self) {
        if let Some(value) = self.system_descriptors.get(&TypeId::of::<S>()) {
            assert!(!value.top_level(), "Cannot add duplicate dependencies.");
            value.set_top_level(true);
        }
        else {
            self.add_new_system::<S>();
            self.reload_systems();
        }
    }

    fn reload_systems(&mut self) {
        unsafe {
            let mut old_systems = take(&mut self.systems);
            let mut touched_systems: BitVec = BitVec::repeat(false, old_systems.len());
    
            for system in Self::topological_sort_systems(&self.system_descriptors).into_iter().rev() {
                let old_id = system.id();
                system.set_id(self.systems.len().try_into().expect("Maximum number of supported systems exceeded."));
                
                if (system.id() as usize) < touched_systems.len() {
                    touched_systems.set_unchecked(system.id() as usize, true);
                    let holder = old_systems.get_unchecked_mut(system.id() as usize);
                    holder.assume_init_ref().handle.upgrade().map(|x| Self::update_context_data(&*x, &self.system_descriptors, &system));
                    self.systems.push(replace(holder, MaybeUninit::uninit()));
                }
                else {
                    // Create new system here!
                    let handle = system.descriptor().create_handle_data(system.id());
                    Self::update_context_data(&*handle, &self.system_descriptors, &system);
                    let (system, events) = Self::create_system(self.context_id, &system, handle, &self.systems);
                    self.systems.push(MaybeUninit::new(system));
                }
            }

            for system in touched_systems.iter_zeros() {
                self.systems.get_unchecked_mut(system).assume_init_drop();
            }
        }
    }

    fn add_new_system<S: GeeseSystem>(&mut self) {
        let mut to_process = SmallVec::new();
        self.add_system_and_load_dependencies(Box::new(TypedSystemDescriptor::<S>::default()), true, &mut to_process);

        while let Some(system) = to_process.pop() {
            self.add_system_and_load_dependencies(system, false, &mut to_process);
        }
    }

    fn add_system_and_load_dependencies(&mut self, system: Box<dyn SystemDescriptor>, top_level: bool, to_process: &mut SmallVec<[Box<dyn SystemDescriptor>; Self::DEFAULT_SYSTEM_PROCESSING_SIZE]>) {
        for dependency in &system.dependencies().0 {
            if !self.system_descriptors.contains_key(&dependency.dependency_id().into()) {
                to_process.push(dependency.descriptor());
            }
        }

        self.system_descriptors.insert(system.system_id(), SystemState::new(system, top_level));
    }

    fn topological_sort_systems(descriptors: &FxHashMap<TypeId, SystemState>) -> Vec<SystemStateRef<'_>> {
        unsafe {
            let mut sort = TopologicalSort::new();
            
            for (id, state) in descriptors {
                for dependency in &state.descriptor().dependencies().0 {
                    sort.add_dependency(SystemStateRef(descriptors.get(&dependency.dependency_id().into()).unwrap_unchecked()), SystemStateRef(state));
                }
            }

            sort.collect::<Vec<_>>()
        }
    }

    unsafe fn create_system(context_id: u16, system: &SystemState, handle: Arc<ContextHandleInner>, systems: *const Vec<MaybeUninit<SystemHolder>>) -> (SystemHolder, SmallVec<[Box<dyn Any + Send + Sync>; DEFAULT_EVENT_BUFFER_SIZE]>) {
        let mut transitive_dependencies = BitVec::repeat(false, (*systems).len());
        let mut transitive_dependencies_mut = BitVec::repeat(false, (*systems).len());
        Self::load_transitive_dependencies(&handle, &mut transitive_dependencies, &mut transitive_dependencies_mut, system, systems);

        let cloned_handle = handle.clone();
        let (system, events) = ExecutionContext::execute(context_id, &transitive_dependencies, systems, move || system.descriptor().create(cloned_handle));

        (SystemHolder { handle: Arc::downgrade(&handle), transitive_dependencies, transitive_dependencies_mut, value: UnsafeCell::new(system) }, events)
    }

    unsafe fn load_transitive_dependencies(handle: &ContextHandleInner, transitive_dependencies: &mut BitVec, transitive_dependencies_mut: &mut BitVec, system: &SystemState, systems: *const Vec<MaybeUninit<SystemHolder>>) {
        for i in 0..handle.dependency_len() {
            let global_index = handle.dependency_id(i);
            transitive_dependencies.set(global_index as usize, true);
            transitive_dependencies_mut.set(global_index as usize, system.descriptor().dependencies().get(global_index as usize).mutable());
    
            transitive_dependencies[..] |= &(&*systems).get_unchecked(global_index as usize).assume_init_ref().transitive_dependencies[..];
            transitive_dependencies_mut[..] |= &(&*systems).get_unchecked(global_index as usize).assume_init_ref().transitive_dependencies_mut[..];
        }
    }

    fn update_context_data(data: &ContextHandleInner, descriptors: &FxHashMap<TypeId, SystemState>, state: &SystemState) {
        unsafe {
            data.set_id(state.id());
    
            for (index, dependency) in state.descriptor().dependencies().0.into_iter().enumerate() {
                data.set_dependency_id(index as u16, descriptors.get(&dependency.dependency_id().into()).unwrap_unchecked().id());
            }
        }
    }
}

struct SystemHolder {
    pub handle: Weak<ContextHandleInner>,
    pub transitive_dependencies: BitVec,
    pub transitive_dependencies_mut: BitVec,
    pub value: UnsafeCell<Box<dyn Any>>
}

#[derive(Copy, Clone)]
struct SystemStateRef<'a>(&'a SystemState);

impl<'a> Deref for SystemStateRef<'a> {
    type Target = SystemState;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<'a> Hash for SystemStateRef<'a> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        (self.0 as *const _ as usize).hash(state);
    }
}

impl<'a> PartialEq for SystemStateRef<'a> {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self.0 as *const _, other.0 as *const _)
    }
}

impl<'a> Eq for SystemStateRef<'a> {}

struct SystemState {
    descriptor: Box<dyn SystemDescriptor>,
    id: Cell<u16>,
    top_level: Cell<bool>,
}

impl SystemState {
    pub fn new(descriptor: Box<dyn SystemDescriptor>, top_level: bool) -> Self {
        Self {
            descriptor,
            id: Cell::new(u16::MAX),
            top_level: Cell::new(top_level)
        }
    }

    pub fn descriptor(&self) -> &dyn SystemDescriptor {
        &*self.descriptor
    }

    pub fn id(&self) -> u16 {
        self.id.get()
    }

    pub fn set_id(&self, id: u16) {
        self.id.set(id);
    }

    pub fn top_level(&self) -> bool {
        self.top_level.get()
    }

    pub fn set_top_level(&self, top_level: bool) {
        self.top_level.set(top_level);
    }
}

#[cfg(test)]
mod tests
{
    use super::*;

    struct A;
    
    impl A {
        fn handler(&mut self, eve: &i32) {
            panic!("bob");
        }
    }
    
    impl GeeseSystem for A {
        const DEPENDENCIES: Dependencies = Dependencies::new()
            .with::<B>()
            .with::<C>();
    
        const EVENT_HANDLERS: EventHandlers<Self> = EventHandlers::new()
            .with(Self::handler);
    
        fn new(ctx: GeeseContextHandle<Self>) -> Self {
            todo!()
        }
    }
    
    struct B;
    
    impl GeeseSystem for B {
        const DEPENDENCIES: Dependencies = Dependencies::new().with::<D>();
    
        const EVENT_HANDLERS: EventHandlers<Self> = EventHandlers::new();
    
        fn new(_: GeeseContextHandle<Self>) -> Self {
            Self
        }
    }

    struct C;
    
    impl GeeseSystem for C {
        const DEPENDENCIES: Dependencies = Dependencies::new().with::<Mut<D>>();
    
        const EVENT_HANDLERS: EventHandlers<Self> = EventHandlers::new();
    
        fn new(_: GeeseContextHandle<Self>) -> Self {
            Self
        }
    }

    struct D;
    
    impl GeeseSystem for D {
        const DEPENDENCIES: Dependencies = Dependencies::new();
    
        const EVENT_HANDLERS: EventHandlers<Self> = EventHandlers::new();
    
        fn new(_: GeeseContextHandle<Self>) -> Self {
            Self
        }
    }
    
    #[test]
    fn test() {
    }
}