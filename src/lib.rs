#![cfg_attr(feature = "static_check", feature(const_type_id))]

#![allow(warnings)]

/// Implements a compile-time linked list for describing variable-length data in `const` contexts.
mod const_list;

/// Provides the ability to generate and compare type IDs in a `const` context.
#[cfg_attr(feature = "static_check", path = "const_type_id/compiled.rs")]
#[cfg_attr(not(feature = "static_check"), path = "const_type_id/runtime.rs")]
mod const_type_id;

/// Provides a macro for evaluating `const` code at either compile-time or runtime.
mod static_eval;

/// Provides the core traits for implementing and validating systems.
mod traits;

use bitvec::access::*;
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
            let ctx = ExecutionContext::current(self.inner.context_id);
            SystemRef::new(self, self.inner.dependency_id(index as u16), ctx)
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
            let ctx = ExecutionContext::current(self.inner.context_id);
            SystemRefMut::new(self, self.inner.dependency_id(index as u16), ctx)
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
    dependency_ids: SmallVec<[Cell<u16>; Self::DEFAULT_DEPENDENCY_BUFFER_SIZE]>
}

impl ContextHandleInner {
    const DEFAULT_DEPENDENCY_BUFFER_SIZE: usize = 4;

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

struct ExecutionContext {
    available_systems: *const BitSlice,
    borrow_count: u16,
    context_id: u16,
    events: UnsafeCell<SmallVec<[Box<dyn Any + Send + Sync>; Self::DEFAULT_EVENT_BUFFER_SIZE]>>,
    mutable_borrows: BitVec,
    systems: *const Vec<MaybeUninit<SystemHolder>>
}

impl ExecutionContext {
    const DEFAULT_EVENT_BUFFER_SIZE: usize = 8;

    thread_local! {
        static CONTEXTS: UnsafeCell<Vec<*mut ExecutionContext>> = const { UnsafeCell::new(Vec::new()) };
    }

    pub fn execute<T>(context_id: u16, available_systems: &BitSlice, systems: *const Vec<MaybeUninit<SystemHolder>>, f: impl FnOnce() -> T) -> (T, SmallVec<[Box<dyn Any + Send + Sync>; Self::DEFAULT_EVENT_BUFFER_SIZE]>) {
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
        assert!((*contexts.pop().unwrap_unchecked()).borrow_count == 0, "Attempted to hold dependency reference beyond event execution cycle.");
    }

    fn contexts() -> *mut Vec<*mut ExecutionContext> {
        Self::CONTEXTS.with(|x| x.get())
    }
}

#[derive(Default)]
pub struct SystemManager {
    context_id: u16,
    event_handlers: EventMap,
    transitive_dependencies: SystemFlagsList,
    transitive_dependencies_mut: SystemFlagsList,
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
            self.instantiate_added_systems();
        }
    }

    pub fn remove_system<S: GeeseSystem>(&mut self) {
        unsafe {
            self.remove_top_level_system::<S>();
            let connected = self.determine_connected_systems();
            if connected.first_zero().is_some() {
                self.unload_disconnected_systems(&connected);
                self.compact_remaining_systems(&connected);
            }
        }
    }

    pub fn reset_system<S: GeeseSystem>(&mut self) {
        unsafe {
            let id = self.system_descriptors.get(&TypeId::of::<S>()).expect("Attempted to reset inactive system.").id();
            let to_load = self.unload_dependents(id);
            self.load_dependents(to_load);
        }
    }

    unsafe fn load_dependents(&mut self, to_load: Vec<(Arc<ContextHandleInner>, TypeId)>) {
        for (handle, system_id) in to_load.into_iter().rev() {
            let descriptor = self.system_descriptors.get(&system_id).unwrap_unchecked().descriptor();
            let cloned_handle = handle.clone();
            let (system, events) = ExecutionContext::execute(self.context_id, self.transitive_dependencies.get_unchecked(handle.id() as usize), &self.systems, move || descriptor.create(cloned_handle));
            self.systems.get_unchecked_mut(handle.id() as usize).write(SystemHolder { handle, system_id, value: UnsafeCell::new(system) });
        }
    }

    unsafe fn unload_dependents(&mut self, id: u16) -> Vec<(Arc<ContextHandleInner>, TypeId)> {
        let mut dropped = Vec::new();
        let systems_ptr: *mut _ = &mut self.systems;

        for i in ((id as usize + 1)..self.systems.len()).rev() {
            if *self.transitive_dependencies.get_unchecked(i).get_unchecked(id as usize) {
                let system = (*systems_ptr).get_unchecked_mut(i);
                dropped.push((system.assume_init_ref().handle.clone(), system.assume_init_ref().system_id));
                ExecutionContext::execute(self.context_id, self.transitive_dependencies.get_unchecked_mut(i), systems_ptr, || system.assume_init_drop());
            }
        }

        let system = (*systems_ptr).get_unchecked_mut(id as usize);
        dropped.push((system.assume_init_ref().handle.clone(), system.assume_init_ref().system_id));
        ExecutionContext::execute(self.context_id, self.transitive_dependencies.get_unchecked_mut(id as usize), systems_ptr, || system.assume_init_drop());

        dropped
    }

    unsafe fn compact_remaining_systems(&mut self, connected: &BitVec) {
        self.event_handlers.clear();
        let mut new_systems = Vec::with_capacity(self.system_descriptors.len());
        let system_view = new_systems.spare_capacity_mut();
        let mut new_transitive_dependencies = SystemFlagsList::new(false, self.system_descriptors.len());
        let mut new_transitive_dependencies_mut = SystemFlagsList::new(false, self.system_descriptors.len());

        for i in 0..self.system_descriptors.len() {
            let system_holder = self.systems.get_unchecked_mut(i);
            let system = self.system_descriptors.get(&system_holder.assume_init_mut().system_id).unwrap_unchecked();
            let old_id = system.id();
            system.set_id(Self::compact_system_id(old_id, connected));
            let new_system = system_view.get_unchecked_mut(system.id() as usize).write(replace(system_holder, MaybeUninit::uninit())).assume_init_ref();
            Self::compact_system_dependencies(new_system, connected);
            Self::compact_transitive_dependencies(&new_system.handle, connected, self.transitive_dependencies_mut.get_unchecked(old_id as usize), &mut new_transitive_dependencies, &mut new_transitive_dependencies_mut);
            self.event_handlers.add_handlers(i as u16, system.descriptor().event_handlers());
        }

        new_systems.set_len(self.system_descriptors.len());
        self.systems = new_systems;
        self.transitive_dependencies = new_transitive_dependencies;
        self.transitive_dependencies_mut = new_transitive_dependencies_mut;
    }

    unsafe fn unload_disconnected_systems(&mut self, disconnected: &BitVec) {
        for system in disconnected.iter_zeros().rev() {
            let system_value: *mut _ = self.systems.get_unchecked_mut(system);
            self.system_descriptors.remove(&(*system_value).assume_init_ref().system_id);
            ExecutionContext::execute(self.context_id, self.transitive_dependencies.get_unchecked_mut(system), &self.systems, || (*system_value).assume_init_drop());
        }
    }

    fn remove_top_level_system<S: GeeseSystem>(&mut self) {
        let system = self.system_descriptors.get(&TypeId::of::<S>()).expect("System was not loaded.");
        assert!(system.top_level(), "System {:?} was not previously added.", type_name::<S>());
        system.set_top_level(false);
    }

    fn instantiate_added_systems(&mut self) {
        unsafe {
            assert!(self.system_descriptors.len() < u16::MAX as usize, "Maximum number of supported systems exceeded.");
            let mut old_systems = take(&mut self.systems);

            self.event_handlers.clear();
            self.transitive_dependencies = SystemFlagsList::new(false, self.system_descriptors.len());
            self.transitive_dependencies_mut = SystemFlagsList::new(false, self.system_descriptors.len());
    
            for system in Self::topological_sort_systems(&self.system_descriptors) {
                let old_id = system.id();
                system.set_id(self.systems.len() as u16);
                
                if old_id < u16::MAX {
                    let holder = old_systems.get_unchecked_mut(system.id() as usize);
                    Self::update_context_data(&holder.assume_init_ref().handle, &self.system_descriptors, &system);
                    self.systems.push(replace(holder, MaybeUninit::uninit()));
                }
                else {
                    let handle = system.descriptor().create_handle_data(self.context_id);
                    Self::update_context_data(&*handle, &self.system_descriptors, &system);
                    
                    let system = Self::create_system(SystemCreationParams {
                        context_id: self.context_id,
                        handle,
                        system: &system,
                        systems: &self.systems,
                        transitive_dependencies: &mut self.transitive_dependencies,
                        transitive_dependencies_mut: &mut self.transitive_dependencies_mut
                    });

                    self.systems.push(MaybeUninit::new(system));
                }

                self.event_handlers.add_handlers(system.id(), system.descriptor().event_handlers());
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

    unsafe fn compact_system_dependencies(holder: &SystemHolder, connected: &BitVec) {
        for i in 0..holder.handle.dependency_len() {
            holder.handle.set_dependency_id(i, Self::compact_system_id(holder.handle.dependency_id(i), connected));
        }
    }

    fn topological_sort_systems(descriptors: &FxHashMap<TypeId, SystemState>) -> Vec<SystemStateRef<'_>> {
        unsafe {
            let mut sort: TopologicalSort<SystemStateRef<'_>> = TopologicalSort::new();
            
            for (id, state) in descriptors {
                sort.insert(SystemStateRef(state));
                for dependency in &state.descriptor().dependencies().0 {
                    sort.add_dependency(SystemStateRef(descriptors.get(&dependency.dependency_id().into()).unwrap_unchecked()), SystemStateRef(state));
                }
            }

            sort.collect::<Vec<_>>()
        }
    }

    unsafe fn create_system(mut params: SystemCreationParams) -> SystemHolder {
        Self::load_transitive_dependencies(&mut params);

        let cloned_handle = params.handle.clone();
        let (system, events) = ExecutionContext::execute(params.context_id, params.transitive_dependencies.get_unchecked(params.system.id() as usize), params.systems, move || params.system.descriptor().create(cloned_handle));

        SystemHolder { handle: params.handle, system_id: params.system.descriptor().system_id(), value: UnsafeCell::new(system) }
    }

    unsafe fn load_transitive_dependencies(params: &mut SystemCreationParams) {
        let mut edit = params.transitive_dependencies.edit_unchecked(params.handle.id() as usize);
        let mut edit_mut = params.transitive_dependencies_mut.edit_unchecked(params.handle.id() as usize);

        for i in 0..params.handle.dependency_len() {
            let global_index = params.handle.dependency_id(i);
            edit.set(global_index as usize, true);
            edit_mut.set(global_index as usize, params.system.descriptor().dependencies().get(i as usize).mutable());
    
            edit.or_with_unchecked(global_index as usize);
            edit_mut.or_with_unchecked(global_index as usize);
        }
    }

    unsafe fn compact_transitive_dependencies(handle: &ContextHandleInner, connected: &BitVec, old_transitive_dependencies_mut: &BitSlice, transitive_dependencies: &mut SystemFlagsList, transitive_dependencies_mut: &mut SystemFlagsList) {
        let mut edit = transitive_dependencies.edit_unchecked(handle.id() as usize);
        let mut edit_mut = transitive_dependencies_mut.edit_unchecked(handle.id() as usize);

        for i in 0..handle.dependency_len() {
            let old_global_index = handle.dependency_id(i);
            let global_index = Self::compact_system_id(old_global_index, connected);
            edit.set(global_index as usize, true);
            edit_mut.set(global_index as usize, *old_transitive_dependencies_mut.get_unchecked(old_global_index as usize));

            edit.or_with_unchecked(global_index as usize);
            edit_mut.or_with_unchecked(global_index as usize);
        }
    }

    fn determine_connected_systems(&self) -> BitVec {
        unsafe {
            let mut connected_systems = BitVec::repeat(false, self.systems.len());
    
            for (_, system) in &self.system_descriptors {
                if system.top_level() {
                    connected_systems.set_unchecked(system.id() as usize, true);
                    connected_systems[..] |= self.transitive_dependencies.get_unchecked(system.id() as usize);
                }
            }

            connected_systems
        }
    }

    unsafe fn compact_system_id(system: u16, remaining_systems: &BitVec) -> u16 {
        (*bitvec::ptr::bitslice_from_raw_parts(remaining_systems.as_bitptr(), system as usize)).count_ones() as u16
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

struct SystemCreationParams<'a> {
    context_id: u16,
    handle: Arc<ContextHandleInner>,
    system: &'a SystemState,
    systems: &'a Vec<MaybeUninit<SystemHolder>>,
    transitive_dependencies: &'a mut SystemFlagsList,
    transitive_dependencies_mut: &'a mut SystemFlagsList,
}

struct SystemHolder {
    pub handle: Arc<ContextHandleInner>,
    pub system_id: TypeId,
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

#[derive(Clone, Debug, Default)]
struct SystemFlagsList {
    data: BitVec,
    stride: usize
}

impl SystemFlagsList {
    pub fn new(bit: bool, size: usize) -> Self {
        Self {
            data: BitVec::repeat(bit, size * size),
            stride: size
        }
    }

    pub unsafe fn edit_unchecked(&mut self, index: usize) -> SystemFlagsListEdit {
        let (rest, first) = self.data.split_at_unchecked_mut(index * self.stride);

        SystemFlagsListEdit {
            editable: &mut *bitvec::ptr::bitslice_from_raw_parts_mut(first.as_mut_bitptr(), self.stride),
            rest,
            stride: self.stride
        }
    }

    pub unsafe fn get_unchecked(&self, index: usize) -> &BitSlice {
        &*bitvec::ptr::bitslice_from_raw_parts(self.data.as_bitptr().add(index * self.stride), self.stride)
    }

    pub unsafe fn get_unchecked_mut(&mut self, index: usize) -> &mut BitSlice {
        &mut *bitvec::ptr::bitslice_from_raw_parts_mut(self.data.as_mut_bitptr().add(index * self.stride), self.stride)
    }
}

struct SystemFlagsListEdit<'a> {
    editable: &'a mut BitSlice<BitSafeUsize>,
    rest: &'a BitSlice<BitSafeUsize>,
    stride: usize
}

impl<'a> SystemFlagsListEdit<'a> {
    pub unsafe fn or_with_unchecked(&mut self, index: usize) {
        *self.editable |= &*bitvec::ptr::bitslice_from_raw_parts(self.rest.as_bitptr().add(index * self.stride), self.stride);
    }
}

impl<'a> Deref for SystemFlagsListEdit<'a> {
    type Target = BitSlice<BitSafeUsize>;

    fn deref(&self) -> &Self::Target {
        self.editable
    }
}

impl<'a> DerefMut for SystemFlagsListEdit<'a> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.editable
    }
}

#[derive(Clone, Debug, Default)]
struct EventMap {
    handlers: FxHashMap<TypeId, SmallVec<[EventHandlerEntry; Self::DEFAULT_HANDLER_BUFFER_SIZE]>>
}

impl EventMap {
    const DEFAULT_HANDLER_BUFFER_SIZE: usize = 4;

    pub fn add_handlers(&mut self, system_id: u16, handlers: &ConstList<'_, EventHandler>) {
        for entry in handlers {
            self.handlers.entry(entry.event_id()).or_default().push(EventHandlerEntry {
                system_id,
                handler: entry.handler()
            });
        }
    }

    pub fn clear(&mut self) {
        self.handlers.clear();
    }

    pub fn handlers(&self, event: TypeId) -> &[EventHandlerEntry] {
        self.handlers.get(&event).map(|x| &x[..]).unwrap_or_default()
    }
}

#[derive(Copy, Clone, Debug)]
struct EventHandlerEntry {
    pub system_id: u16,
    pub handler: fn(*mut (), *const ())
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
            println!("a");
            Self
        }
    }

    impl Drop for A {
        fn drop(&mut self) {
            println!("drop a");
        }
    }
    
    struct B;
    
    impl GeeseSystem for B {
        const DEPENDENCIES: Dependencies = Dependencies::new().with::<D>();
    
        const EVENT_HANDLERS: EventHandlers<Self> = EventHandlers::new();
    
        fn new(_: GeeseContextHandle<Self>) -> Self {
            println!("b");
            Self
        }
    }

    impl Drop for B {
        fn drop(&mut self) {
            println!("drop b");
        }
    }

    struct C {
        ctx: GeeseContextHandle<Self>
    }
    
    impl GeeseSystem for C {
        const DEPENDENCIES: Dependencies = Dependencies::new().with::<Mut<D>>();
    
        const EVENT_HANDLERS: EventHandlers<Self> = EventHandlers::new();
    
        fn new(mut ctx: GeeseContextHandle<Self>) -> Self {
            let mut d = ctx.get_mut::<D>();

            println!("c {:?}", d.get_it());
            d.set_it();
            drop(d);
            Self { ctx }
        }
    }

    impl Drop for C {
        fn drop(&mut self) {
            let d = self.ctx.get_mut::<D>();
            println!("drop c but {:?}", d.get_it());
        }
    }

    struct D(u32);
    
    impl D {
        pub fn get_it(&self) -> u32 {
            self.0
        }

        pub fn set_it(&mut self) {
            self.0 = 95;
        }
    }

    impl Drop for D {
        fn drop(&mut self) {
            println!("drop d");
        }
    }

    impl GeeseSystem for D {
        const DEPENDENCIES: Dependencies = Dependencies::new();
    
        const EVENT_HANDLERS: EventHandlers<Self> = EventHandlers::new();
    
        fn new(_: GeeseContextHandle<Self>) -> Self {
            println!("d");
            Self(29)
        }
    }
    
    #[test]
    fn test() {
        let mut mgr = SystemManager::default();
        mgr.add_system::<D>();
        println!("in between");
        mgr.add_system::<A>();
        println!("death");
        mgr.reset_system::<D>();
        mgr.remove_system::<A>();
    }
}