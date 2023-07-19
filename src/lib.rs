#![cfg_attr(unstable, feature(const_type_id))]

//! Crate docs

/*#![deny(warnings)]*/
#![allow(unused)]
#![warn(missing_docs)]
#![warn(clippy::missing_docs_in_private_items)]

/// Provides the ability to create lists at compile time.
mod const_list;

/// Provides the ability to generate and compare type IDs in a `const` context.
#[cfg_attr(unstable, path = "const_type_id/compiled.rs")]
#[cfg_attr(not(unstable), path = "const_type_id/runtime.rs")]
mod const_type_id;

/// Provides methods for evaluating `const` code with generics at compilation and at runtime.
mod static_eval;

/// Declares cell types that may be used for lock-free cross-thread resource sharing.
mod rw_cell;

/// Defines the core traits used to create Geese systems.
mod traits;

use bitvec::access::*;
use bitvec::prelude::*;
use crate::const_list::*;
use crate::rw_cell::*;
use crate::static_eval::*;
pub use crate::traits::*;
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
use std::sync::*;
use std::thread::*;
use topological_sort::*;

/// Represents a system-specific handle to a Geese context.
#[allow(unused_variables)]
pub struct GeeseContextHandle<S: GeeseSystem> {
    /// The handle data used to access the Geese context.
    inner: Arc<ContextHandleInner>,
    /// Marks the system argument as being used.
    data: PhantomData<fn(S)>
}

impl<S: GeeseSystem> GeeseContextHandle<S> {
    /// Creates a new handle from an inner context reference.
    fn new(inner: Arc<ContextHandleInner>) -> Self {
        Self {
            inner,
            data: PhantomData
        }
    }

    /// Raises the specified dynamically-typed event.
    pub fn raise_event_boxed(&self, event: Box<dyn Any + Send + Sync>) {
        todo!()
    }

    /// Raises the specified event.
    pub fn raise_event<T: 'static + Send + Sync>(&self, event: T) {
        todo!()
    }

    /// Obtains the specified system dependency.
    pub fn get<T: GeeseSystem>(&self) -> SystemRef<T> {
        unsafe {
            let index = static_eval!(if let Some(index) = S::DEPENDENCIES.index_of::<T>() { index } else { GeeseContextHandle::<S>::panic_on_invalid_dependency() }, usize, S, T);
            let ctx = (*self.inner.context).borrow();
            let global_index = self.inner.dependency_id(index as u16) as usize;
            assert!(*ctx.sync_systems.get_unchecked(global_index) || ctx.owning_thread == current().id(), "Attempted a cross-thread borrow of a system that did not implement Sync.");
            let guard = ctx.systems.get_unchecked(global_index).value.borrow().detach();
            SystemRef::new(RwCellGuard::map(guard, |system| transmute::<_, &(&T, *const ())>(system).0))
        }
    }

    /// Mutably obtains the specified system dependency.
    pub fn get_mut<T: GeeseSystem>(&mut self) -> SystemRefMut<T> {
        unsafe {
            let index = static_eval!({
                if let Some(index) = S::DEPENDENCIES.index_of::<T>() {
                    assert!(const_unwrap(S::DEPENDENCIES.as_inner().get(index)).mutable(), "Attempted to mutably access an immutable dependency.");
                    index
                }
                else {
                    GeeseContextHandle::<S>::panic_on_invalid_dependency()
                }
            }, usize, S, T);
            let ctx = (*self.inner.context).borrow();
            let global_index = self.inner.dependency_id(index as u16) as usize;
            assert!(*ctx.sync_systems.get_unchecked(global_index) || ctx.owning_thread == current().id(), "Attempted a cross-thread borrow of a system that did not implement Sync.");
            let guard = ctx.systems.get_unchecked(global_index).value.borrow_mut().detach();
            SystemRefMut::new(RwCellGuardMut::map(guard, |system| transmute::<_, &mut (&mut T, *const ())>(system).0))
        }
    }

    /// Panics when the user attempts to reference an undeclared dependency.
    const fn panic_on_invalid_dependency() -> ! {
        panic!("The specified system was not a dependency of this one.");
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
    /// This function may only be invoked by one thread at a time.
    unsafe fn id(&self) -> u16 {
        self.id.get()
    }

    /// Sets the current ID of this context handle.
    /// 
    /// # Safety
    /// 
    /// This function may only be invoked by one thread at a time.
    unsafe fn set_id(&self, value: u16) {
        self.id.set(value);
    }

    /// Gets the number of dependencies that this system has.
    /// 
    /// # Safety
    /// 
    /// This function may only be invoked by one thread at a time.
    unsafe fn dependency_len(&self) -> u16 {
        self.dependency_ids.len() as u16
    }

    /// Gets the global ID of the dependency with the provided local index.
    /// 
    /// # Safety
    /// 
    /// This function may only be invoked by one thread at a time.
    /// The index must be less than the total number of system dependencies.
    unsafe fn dependency_id(&self, index: u16) -> u16 {
        self.dependency_ids.get_unchecked(index as usize).get()
    }

    /// Sets the global ID of the dependency with the provided local index.
    /// 
    /// # Safety
    /// 
    /// This function may only be invoked by one thread at a time.
    /// The index must be less than the total number of system dependencies.
    unsafe fn set_dependency_id(&self, index: u16, value: u16) {
        *self.dependency_ids.get_unchecked(index as usize).as_ptr() = value;
    }
}

/// Represents a collection of systems that can create and respond to events.
pub struct GeeseContext(Pin<Box<RwCell<ContextInner>>>);

impl GeeseContext {
    /// Obtains a reference to the given system.
    pub fn system<S: GeeseSystem>(&self) -> SystemRef<S> {
        unsafe {
            let inner = self.0.borrow();
            let index = inner.system_initializers.get(&TypeId::of::<S>()).expect("System not found.").id();
            let guard = inner.systems.get_unchecked(index as usize).value.borrow().detach();
            SystemRef::new(RwCellGuard::map(guard, |system| transmute::<_, &(&S, *const ())>(system).0))
        }
    }

    /// Mutably obtains a reference to the given system.
    pub fn system_mut<S: GeeseSystem>(&mut self) -> SystemRefMut<S> {
        unsafe {
            let inner = self.0.borrow();
            let index = inner.system_initializers.get(&TypeId::of::<S>()).expect("System not found.").id();
            let guard = inner.systems.get_unchecked(index as usize).value.borrow_mut().detach();
            SystemRefMut::new(RwCellGuardMut::map(guard, |system| transmute::<_, &mut (&mut S, *const ())>(system).0))
        }
    }

    /// Adds a system to the context.
    pub fn add_system<S: GeeseSystem>(&mut self) {
        unsafe {
            let mut inner = self.0.borrow_mut();
            if let Some(value) = inner.system_initializers.get(&TypeId::of::<S>()) {
                assert!(!value.top_level(), "Cannot add duplicate dependencies.");
                value.set_top_level(true);
            }
            else {
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
    pub fn remove_system<S: GeeseSystem>(&mut self) {
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
    pub fn reset_system<S: GeeseSystem>(&mut self) {
        self.0.borrow().reset_system::<S>();
    }

    /// Initializes all systems in the given iterator.
    /// 
    /// # Safety
    /// 
    /// The systems in the iterator must have valid IDs referring to systems in the inner context.
    unsafe fn initialize_systems<'a>(&self, systems: impl Iterator<Item = &'a SystemInitializer>) {
        let inner = self.0.borrow();
        for system in systems {
            let holder = inner.system_holder(system.id() as usize);
            holder.value.borrow_mut().write(system.descriptor.create(holder.handle.clone()));
        }
    }

    /// Drops all systems in the inner context which do not exist in the bitmap.
    /// 
    /// # Safety
    /// 
    /// For this function call to be sound, all of the zeroed bits in the systems map must correspond
    /// to valid, initialized systems in the inner context.
    unsafe fn drop_systems(&self, systems: &BitVec, initializers: &mut FxHashMap<TypeId, SystemInitializer>) {
        let inner = self.0.borrow();
        for system in systems.iter_zeros().rev() {
            let holder = inner.system_holder(system);
            holder.drop();
            initializers.remove(&holder.system_id);
        }
    }

    /// Gets a fixed pointer to the inner context.
    fn as_ptr(&self) -> *const RwCell<ContextInner> {
        &*self.0
    }
}

impl Default for GeeseContext {
    fn default() -> Self {
        Self(Box::pin(RwCell::default()))
    }
}

impl Drop for GeeseContext {
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
    /// The thread on which the context was created.
    owning_thread: ThreadId,
    /// The set of systems that implement the `Send` trait.
    send_systems: BitVec,
    /// The set of systems that implement the `Sync` trait.
    sync_systems: BitVec,
    /// The set of currently-loaded system initializers.
    system_initializers: FxHashMap<TypeId, SystemInitializer>,
    /// The set of loaded systems.
    systems: Vec<SystemHolder>,
    /// The transitive dependency lists for each system.
    transitive_dependencies: SystemFlagsList,
    /// The lists of mutable transitive dependencies for each system.
    transitive_dependencies_mut: SystemFlagsList
}

impl ContextInner {
    /// The default amount of space to allocate for processing new systems during creation.
    const DEFAULT_SYSTEM_PROCESSING_SIZE: usize = 8;

    /// Drops all systems from the systems list. No context datastructures are modified.
    /// 
    /// # Safety
    /// 
    /// All systems in the list be valid, initialized objects.
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
    pub unsafe fn system_holder(&self, index: usize) -> &SystemHolder {
        self.systems.get_unchecked(index)
    }

    /// Adds a new system and all of its dependencies to the set of initializers.
    pub fn add_new_system<S: GeeseSystem>(&mut self) {
        let mut to_process = SmallVec::new();
        self.add_system_and_load_dependencies::<true>(Box::<TypedSystemDescriptor::<S>>::default(), &mut to_process);

        while let Some(system) = to_process.pop() {
            self.add_system_and_load_dependencies::<false>(system, &mut to_process);
        }
    }

    /// Adds the provided system descriptor to the set of initializers, and pushes all of the dependencies
    /// to process into the queue.
    #[inline(always)]
    fn add_system_and_load_dependencies<const TOP_LEVEL: bool>(&mut self, system: Box<dyn SystemDescriptor>, to_process: &mut SmallVec<[Box<dyn SystemDescriptor>; Self::DEFAULT_SYSTEM_PROCESSING_SIZE]>) {
        if TOP_LEVEL {
            for dependency in system.dependencies().as_inner() {
                to_process.push(dependency.descriptor());
            }
            self.system_initializers.insert(system.system_id(), SystemInitializer::new(system, true));
        }
        else if let Entry::Vacant(entry) = self.system_initializers.entry(system.system_id()) {
            for dependency in system.dependencies().as_inner() {
                to_process.push(dependency.descriptor());
            }

            entry.insert(SystemInitializer::new(system, false));
        }
    }

    /// Instantiates all new systems added to the initializer map, appropriately resizing the context resources.
    pub fn instantiate_added_systems<'a>(&mut self, initializers: &'a FxHashMap<TypeId, SystemInitializer>, ctx: *const RwCell<ContextInner>) -> SmallVec<[&'a SystemInitializer; Self::DEFAULT_SYSTEM_PROCESSING_SIZE]> {
        unsafe {
            assert!(initializers.len() < u16::MAX as usize, "Maximum number of supported systems exceeded.");
            let mut old_systems = take(&mut self.systems);
            old_systems.set_len(0);
            let mut old_system_view = old_systems.spare_capacity_mut();
            let mut to_initialize = SmallVec::new();

            self.systems.reserve(initializers.len());
            self.event_handlers.clear();
            self.send_systems = BitVec::repeat(false, initializers.len());
            self.sync_systems = BitVec::repeat(false, initializers.len());
            self.transitive_dependencies = SystemFlagsList::new(false, initializers.len());
            self.transitive_dependencies_mut = SystemFlagsList::new(false, initializers.len());

            let mut default_holder = MaybeUninit::uninit();
    
            for system in Self::topological_sort_systems(initializers) {
                let old_id = system.id();
                system.set_id(self.systems.len() as u16);
                
                let holder = if old_id < u16::MAX {
                    let res = old_system_view.get_unchecked_mut(system.id() as usize);
                    assert!(res.assume_init_mut().value.free(), "Attempted to borrow system while the context was moving it.");
                    res
                }
                else {
                    default_holder = MaybeUninit::new(SystemHolder::new(system.descriptor().system_id(), system.descriptor().dependency_len(), ctx));
                    to_initialize.push(system.into_inner());
                    &mut default_holder
                };

                Self::update_holder_data(holder.assume_init_mut(), initializers, &system);
                self.load_transitive_dependencies(holder.assume_init_mut(), &system);
                self.systems.push(replace(holder, MaybeUninit::uninit()).assume_init());

                self.event_handlers.add_handlers(system.id(), system.descriptor().event_handlers());
                self.send_systems.set_unchecked(system.id() as usize, system.descriptor().is_send());
                self.sync_systems.set_unchecked(system.id() as usize, system.descriptor().is_sync());
            }

            to_initialize
        }
    }

    /// Removes a system from being top-level in the dependency graph.
    fn remove_top_level_system<S: GeeseSystem>(&mut self) {
        let system = self.system_initializers.get(&TypeId::of::<S>()).expect("System was not loaded.");
        assert!(system.top_level(), "System {:?} was not previously added.", type_name::<S>());
        system.set_top_level(false);
    }

    /// Computes the set of systems that are reachable from the top levels of the dependency graph.
    fn determine_connected_systems(&self) -> BitVec {
        unsafe {
            let mut connected_systems = BitVec::repeat(false, self.systems.len());
    
            for system in self.system_initializers.values() {
                if system.top_level() {
                    connected_systems.set_unchecked(system.id() as usize, true);
                    connected_systems[..] |= self.transitive_dependencies.get_unchecked(system.id() as usize);
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
    unsafe fn load_transitive_dependencies(&mut self, holder: &SystemHolder, initializer: &SystemInitializer) {
        let mut edit = self.transitive_dependencies.edit_unchecked(holder.handle.id() as usize);
        let mut edit_mut = self.transitive_dependencies_mut.edit_unchecked(holder.handle.id() as usize);

        for i in 0..holder.handle.dependency_len() {
            let global_index = holder.handle.dependency_id(i);
            edit.set(global_index as usize, true);
            edit_mut.set(global_index as usize, initializer.descriptor().dependencies().as_inner().get(i as usize).unwrap_unchecked().mutable());
    
            edit.or_with_unchecked(global_index as usize);
            edit_mut.or_with_unchecked(global_index as usize);
        }
    }
    
    /// Compacts the set of remaining systems into a new systems array after some systems have been removed.
    /// 
    /// # Safety
    /// 
    /// For this function call to be sound, the connected bitmap must correspond exactly to which systems
    /// are initialized in the context.
    pub unsafe fn compact_remaining_systems(&mut self, connected: &BitVec) {
        self.event_handlers.clear();
        self.systems.set_len(0);
        let old_systems = self.systems.spare_capacity_mut();

        let mut new_systems = Vec::with_capacity(self.system_initializers.len());
        let system_view = new_systems.spare_capacity_mut();

        self.send_systems = BitVec::repeat(false, self.system_initializers.len());
        self.sync_systems = BitVec::repeat(false, self.system_initializers.len());
        let mut new_transitive_dependencies = SystemFlagsList::new(false, self.system_initializers.len());
        let mut new_transitive_dependencies_mut = SystemFlagsList::new(false, self.system_initializers.len());

        for initializer in self.system_initializers.values() {
            let old_id = initializer.id();
            initializer.set_id(Self::compact_system_id(old_id, connected));
            let system_holder = old_systems.get_unchecked_mut(old_id as usize);
            let new_system = system_view.get_unchecked_mut(initializer.id() as usize).write(replace(system_holder, MaybeUninit::uninit()).assume_init());
            new_system.handle.set_id(initializer.id());
            Self::compact_transitive_dependencies(new_system, connected, self.transitive_dependencies_mut.get_unchecked(old_id as usize), &mut new_transitive_dependencies, &mut new_transitive_dependencies_mut);
            self.event_handlers.add_handlers(initializer.id(), initializer.descriptor().event_handlers());
            self.send_systems.set_unchecked(initializer.id() as usize, initializer.descriptor().is_send());
            self.sync_systems.set_unchecked(initializer.id() as usize, initializer.descriptor().is_sync());
        }

        Self::drop_dead_holders(connected, old_systems);

        new_systems.set_len(self.system_initializers.len());
        self.systems = new_systems;
        self.transitive_dependencies = new_transitive_dependencies;
        self.transitive_dependencies_mut = new_transitive_dependencies_mut;
    }

    /// Reloads all of the system instances that depend upon the given system.
    pub fn reset_system<S: GeeseSystem>(&self) {
        unsafe {
            let id = self.system_initializers.get(&TypeId::of::<S>()).expect("Attempted to reset nonexistant system.").id();
            let to_load = self.unload_dependents(id);
            self.load_dependents(to_load);
        }
    }

    /// Loads all systems that depend upon the system with the given ID in topological order.
    unsafe fn load_dependents(&self, to_load: SmallVec<[u16; Self::DEFAULT_SYSTEM_PROCESSING_SIZE]>) {
        for i in to_load.into_iter().rev() {
            let holder = self.systems.get_unchecked(i as usize);
            let descriptor = self.system_initializers.get(&holder.system_id).unwrap_unchecked().descriptor();
            holder.value.borrow_mut().write(descriptor.create(holder.handle.clone()));
        }
    }

    /// Unloads all systems that depend upon the system with the given ID in reverse topological order.
    unsafe fn unload_dependents(&self, id: u16) -> SmallVec<[u16; Self::DEFAULT_SYSTEM_PROCESSING_SIZE]> {
        let mut dropped = SmallVec::new();

        for i in ((id as usize + 1)..self.systems.len()).rev() {
            if *self.transitive_dependencies.get_unchecked(i).get_unchecked(id as usize) {
                self.systems.get_unchecked(i).drop();
                dropped.push(i as u16);
            }
        }

        self.systems.get_unchecked(id as usize).drop();
        dropped.push(id);

        dropped
    }

    /// Topologically sorts all systems in the map, producing a vector sorted such that all dependencies come before
    /// their dependents.
    fn topological_sort_systems(descriptors: &FxHashMap<TypeId, SystemInitializer>) -> Vec<SystemStateRef<'_>> {
        unsafe {
            let mut sort: TopologicalSort<SystemStateRef<'_>> = TopologicalSort::new();
            
            for (id, state) in descriptors {
                sort.insert(SystemStateRef(state));
                for dependency in state.descriptor().dependencies().as_inner() {
                    sort.add_dependency(SystemStateRef(descriptors.get(&dependency.dependency_id().into()).unwrap_unchecked()), SystemStateRef(state));
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
    unsafe fn update_holder_data(data: &mut SystemHolder, descriptors: &FxHashMap<TypeId, SystemInitializer>, state: &SystemInitializer) {
        data.handle.set_id(state.id());
        for (index, dependency) in state.descriptor().dependencies().as_inner().into_iter().enumerate() {
            data.handle.set_dependency_id(index as u16, descriptors.get(&dependency.dependency_id().into()).unwrap_unchecked().id());
        }
    }
    
    /// Computes the new system ID from the set of remaining systems in the bitmap.
    /// 
    /// # Safety
    /// 
    /// For this function to be sound, the system ID must be less than the bitmap length.
    unsafe fn compact_system_id(system: u16, remaining_systems: &BitVec) -> u16 {
        (*bitvec::ptr::bitslice_from_raw_parts(remaining_systems.as_bitptr(), system as usize)).count_ones() as u16
    }

    /// Updates the system handle's dependency maps using the connectivity bitmap after some systems have been dropped.
    /// 
    /// # Safety
    /// 
    /// For this function call to be defined, all of the holder's current dependency values
    /// must be less than the connected bitmap and transitive dependency list lengths.
    unsafe fn compact_transitive_dependencies(holder: &SystemHolder, connected: &BitVec, old_transitive_dependencies_mut: &BitSlice, transitive_dependencies: &mut SystemFlagsList, transitive_dependencies_mut: &mut SystemFlagsList) {
        let mut edit = transitive_dependencies.edit_unchecked(holder.handle.id() as usize);
        let mut edit_mut = transitive_dependencies_mut.edit_unchecked(holder.handle.id() as usize);

        for i in 0..holder.handle.dependency_len() {
            let old_global_index = holder.handle.dependency_id(i);
            let global_index = Self::compact_system_id(old_global_index, connected);
            edit.set(global_index as usize, true);
            edit_mut.set(global_index as usize, *old_transitive_dependencies_mut.get_unchecked(old_global_index as usize));

            edit.or_with_unchecked(global_index as usize);
            edit_mut.or_with_unchecked(global_index as usize);
            holder.handle.set_dependency_id(i, global_index);
        }
    }

    /// Drops all dead system holders from the systems array.
    /// 
    /// # Safety
    /// 
    /// For this call to be defined, all of the zeroes in the connected bitmap must correspond to
    /// valid, initialized system holders in the old systems list.
    unsafe fn drop_dead_holders(connected: &BitVec, old_systems: &mut [MaybeUninit<SystemHolder>]) {
        for dead_system in connected.iter_zeros() {
            old_systems.get_unchecked_mut(dead_system).assume_init_drop();
        }
    }
}

impl Default for ContextInner {
    fn default() -> Self {
        Self {
            event_handlers: EventMap::default(),
            owning_thread: current().id(),
            send_systems: BitVec::default(),
            sync_systems: BitVec::default(),
            system_initializers: FxHashMap::default(),
            systems: Vec::default(),
            transitive_dependencies: SystemFlagsList::default(),
            transitive_dependencies_mut: SystemFlagsList::default()
        }
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
    pub fn new(descriptor: Box<dyn SystemDescriptor>, top_level: bool) -> Self {
        Self {
            descriptor,
            id: Cell::new(u16::MAX),
            top_level: Cell::new(top_level)
        }
    }

    /// Gets the descriptor associated with this initializer.
    pub fn descriptor(&self) -> &dyn SystemDescriptor {
        &*self.descriptor
    }

    /// Gets the current ID for this system type.
    pub fn id(&self) -> u16 {
        self.id.get()
    }

    /// Sets the current ID for this system type.
    pub fn set_id(&self, id: u16) {
        self.id.set(id);
    }

    /// Gets whether this is a top-level system.
    pub fn top_level(&self) -> bool {
        self.top_level.get()
    }

    /// Sets whether this is a top-level system.
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
    pub fn into_inner(self) -> &'a SystemInitializer {
        self.0
    }
}

impl<'a> Deref for SystemStateRef<'a> {
    type Target = SystemInitializer;

    fn deref(&self) -> &Self::Target {
        self.0
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

/// Stores the current state of a system.
struct SystemHolder {
    /// The handle used to manage system state.
    pub handle: Arc<ContextHandleInner>,
    /// The type ID of the system.
    pub system_id: TypeId,
    /// The loaded system itself.
    pub value: RwCell<MaybeUninit<Box<dyn Any>>>
}

impl SystemHolder {
    /// Creates a new holder with the provided information. The context handle's ID and dependency map
    /// initially have undefined contents.
    /// 
    /// # Safety
    /// 
    /// The initial contents of the context handle must not affect the program's execution in any fashion.
    pub unsafe fn new(system_id: TypeId, dependency_len: usize, context: *const RwCell<ContextInner>) -> Self {
        let mut dependency_ids = SmallVec::with_capacity(dependency_len);
        dependency_ids.set_len(dependency_len);
        Self {
            handle: Arc::new(ContextHandleInner { context, dependency_ids, id: Cell::new(0) }),
            system_id,
            value: RwCell::new(MaybeUninit::uninit())
        }
    }

    /// Drops the system in-place and ensures that the context handle is no longer alive.
    /// 
    /// # Safety
    /// 
    /// The system value must refer to a valid, initialized object.
    pub unsafe fn drop(&self) {
        let mut value = self.value.borrow_mut();
        value.assume_init_drop();
        assert!(Arc::strong_count(&self.handle) == 1, "Attempted to retain context handle beyond system lifetime.");
    }
}

/// Represents a densely-packed list of lists of bitflags used to store
/// per-system information about other systems.
#[derive(Clone, Debug, Default)]
struct SystemFlagsList {
    /// The underlying data buffer.
    data: BitVec,
    /// The amount of bits per system.
    stride: usize
}

impl SystemFlagsList {
    /// Creates a new list of system flags, initialized to the given value.
    pub fn new(bit: bool, size: usize) -> Self {
        Self {
            data: BitVec::repeat(bit, size * size),
            stride: size
        }
    }

    /// Initializes an edit of the flags for the given system, allowing for the
    /// simultaneous immutable use of flags with smaller indices during the edit.
    /// 
    /// # Safety
    /// 
    /// For this function to be sound, index must be less than the total number of systems.
    pub unsafe fn edit_unchecked(&mut self, index: usize) -> SystemFlagsListEdit {
        let (rest, first) = self.data.split_at_unchecked_mut(index * self.stride);

        SystemFlagsListEdit {
            editable: &mut *bitvec::ptr::bitslice_from_raw_parts_mut(first.as_mut_bitptr(), self.stride),
            rest,
            stride: self.stride
        }
    }

    /// Gets a slice associated with the given system at the provided index.
    /// 
    /// # Safety
    /// 
    /// For this function to be sound, index must be less than the total number of systems.
    pub unsafe fn get_unchecked(&self, index: usize) -> &BitSlice {
        &*bitvec::ptr::bitslice_from_raw_parts(self.data.as_bitptr().add(index * self.stride), self.stride)
    }

    /// Gets a mutable slice associated with the given system at the provided index.
    /// 
    /// # Safety
    /// 
    /// For this function to be sound, index must be less than the total number of systems.
    pub unsafe fn get_unchecked_mut(&mut self, index: usize) -> &mut BitSlice {
        &mut *bitvec::ptr::bitslice_from_raw_parts_mut(self.data.as_mut_bitptr().add(index * self.stride), self.stride)
    }
}

/// Represents an ongoing edit operation to a system flags list. This allows
/// for editing one part of the list while referencing other parts.
struct SystemFlagsListEdit<'a> {
    /// The part of the bit vector that is being edited.
    editable: &'a mut BitSlice<BitSafeUsize>,
    /// Everything prior to the editable part which may be immutably referenced.
    rest: &'a BitSlice<BitSafeUsize>,
    /// The number of bits per system.
    stride: usize
}

impl<'a> SystemFlagsListEdit<'a> {
    /// Computes the bitwise or of the editable region with the system at the given index.
    /// 
    /// # Safety
    /// 
    /// For this function call to be defined, the index must be smaller than that of the system
    /// to edit.
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

/// Represents an immutable reference to a system.
pub struct SystemRef<'a, T: ?Sized> {
    /// The backing guard for the system.
    inner: RwCellGuard<'a, T>
}

impl<'a, T: ?Sized> SystemRef<'a, T> {
    /// Creates a new immutable reference to a system from a
    /// `RwCell` borrow.
    fn new(inner: RwCellGuard<'a, T>) -> Self {
        Self { inner }
    }

    /// Creates a reference to a specific borrowed component of a system.
    pub fn map<U, F>(orig: SystemRef<'a, T>, f: F) -> SystemRef<'a, U> where F: FnOnce(&T) -> &U, U: ?Sized {
        SystemRef::new(RwCellGuard::map(orig.inner, f))
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
    /// The backing guard for the system.
    inner: RwCellGuardMut<'a, T>
}

impl<'a, T: ?Sized> SystemRefMut<'a, T> {
    /// Creates a new mutable reference to a system from a mutable
    /// `RwCell` borrow.
    fn new(inner: RwCellGuardMut<'a, T>) -> Self {
        Self { inner }
    }

    /// Creates a reference to a specific borrowed component of a system.
    pub fn map<U, F>(orig: SystemRefMut<'a, T>, f: F) -> SystemRefMut<'a, U> where F: FnOnce(&mut T) -> &mut U, U: ?Sized {
        SystemRefMut::new(RwCellGuardMut::map(orig.inner, f))
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

/// Stores a mapping from event type IDs to groups of event handlers.
#[derive(Clone, Debug, Default)]
struct EventMap {
    /// The inner mapping from type IDs to event handlers.
    handlers: FxHashMap<TypeId, SmallVec<[EventHandlerEntry; Self::DEFAULT_HANDLER_BUFFER_SIZE]>>
}

impl EventMap {
    /// The default amount of space to allocate for event handler lists.
    const DEFAULT_HANDLER_BUFFER_SIZE: usize = 4;

    /// Adds all of the event handlers in the given list to this map, associating them with
    /// the provided system ID.
    pub fn add_handlers(&mut self, system_id: u16, handlers: &ConstList<'_, EventHandler>) {
        for entry in handlers {
            self.handlers.entry(entry.event_id()).or_default().push(EventHandlerEntry {
                system_id,
                handler: *entry.handler()
            });
        }
    }

    /// Clears the event map of all handlers.
    pub fn clear(&mut self) {
        self.handlers.clear();
    }

    /// Gets the event handlers that respond to the provided event type.
    pub fn handlers(&self, event: TypeId) -> &[EventHandlerEntry] {
        self.handlers.get(&event).map(|x| &x[..]).unwrap_or_default()
    }
}

/// Describes a single event handler.
#[derive(Copy, Clone, Debug)]
struct EventHandlerEntry {
    /// The invoker that may be used to dispatch events.
    pub handler: EventInvoker,
    /// The ID of the system with which this event handler is associated.
    pub system_id: u16,
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
        let mut mgr = GeeseContext::default();
        mgr.add_system::<D>();
        println!("in between");
        mgr.add_system::<A>();
        println!("death");
        mgr.reset_system::<D>();
        mgr.remove_system::<A>();
    }
}