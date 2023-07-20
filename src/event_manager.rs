use bitvec::prelude::*;
use bitvec::ptr::*;
use crate::*;
use crate::arena_array::*;
use smallvec::*;
use std::collections::vec_deque::*;
use vecdeque_stableix::*;
use wasm_sync::*;

pub struct EventManager {
    event_sender: std::sync::mpsc::Sender<Event>,
    inner: Mutex<EventManagerInner>
}

pub struct EventManagerInner {
    borrow_manager: SystemBorrowManager,
    accepting_events: bool,
    event_list: VecDeque<Box<dyn Any + Send + Sync>>,
    receiver: std::sync::mpsc::Receiver<Event>,
}

pub(crate) struct Event {
    value: Box<dyn Any + Send + Sync>,
    sender: ThreadId
}

impl Event {
    pub fn new(value: Box<dyn Any + Send + Sync>) -> Self {
        Self {
            value,
            sender: current().id()
        }
    }
}

#[derive(Clone, Debug, Default)]
struct SystemBorrowManager {
    borrowed_systems: SmallVec<[u16; Self::DEFAULT_STACKED_BORROW_BUFFER_SIZE]>,
    system_borrows: BitVec,
    system_borrows_mut: BitVec
}

impl SystemBorrowManager {
    const DEFAULT_STACKED_BORROW_BUFFER_SIZE: usize = 4;

    pub fn configure(&mut self, ctx: &ContextInner) {
        self.system_borrows.resize(ctx.system_initializers.len(), false);
        self.system_borrows_mut.resize(ctx.system_initializers.len(), false);
    }

    pub unsafe fn borrow(&mut self, id: u16, ctx: &ContextInner) -> bool {
        if self.borrowed_systems.len() == 0
            || (current().id() == ctx.owning_thread && self.can_borrow::<true>(id, ctx))
            || self.can_borrow::<false>(id, ctx) {
            self.commit_borrow(id, ctx);
            true
        }
        else {
            false
        }
    }

    pub unsafe fn release(&mut self, id: u16, ctx: &ContextInner) {
        self.system_borrows.fill(false);
        self.system_borrows_mut.fill(false);

        let mut to_remove = 0;
        for (index, value) in self.borrowed_systems.iter().copied().enumerate() {
            if id == value {
                to_remove = index;
            }
            else {
                self.system_borrows |= ctx.transitive_dependencies.get_unchecked(value as usize);
                self.system_borrows_mut |= ctx.transitive_dependencies_mut.get_unchecked(value as usize);
            }
        }

        self.borrowed_systems.swap_remove(to_remove);
    }

    unsafe fn commit_borrow(&mut self, id: u16, ctx: &ContextInner) {
        self.borrowed_systems.push(id);
        self.system_borrows |= ctx.transitive_dependencies.get_unchecked(id as usize);
        self.system_borrows_mut |= ctx.transitive_dependencies_mut.get_unchecked(id as usize);
    }

    unsafe fn can_borrow<const OWNING_THREAD: bool>(&mut self, id: u16, ctx: &ContextInner) -> bool {
        let transitive_dependencies = ctx.transitive_dependencies.get_unchecked(id as usize);
        let transitive_dependencies_mut = ctx.transitive_dependencies_mut.get_unchecked(id as usize);

        for borrow in self.system_borrows.chunks(usize::BITS as usize) {
            let start = borrow.as_bitptr().offset_from(self.system_borrows.as_bitptr()) as usize;

            let borrow_mut = bitslice_from_start_len(&self.system_borrows_mut, start, borrow.len());
            let deps = bitslice_from_start_len(&transitive_dependencies, start, borrow.len());
            let deps_mut = bitslice_from_start_len(&transitive_dependencies_mut, start, borrow.len());
            let sync = bitslice_from_start_len(&ctx.sync_systems, start, borrow.len());
            
            let res = if OWNING_THREAD {
                Self::can_borrow_bitmap(borrow.load(), borrow_mut.load(), deps.load(), deps_mut.load(), sync.load())
            }
            else {
                Self::can_borrow_bitmap(borrow.load(), borrow_mut.load(), deps.load(), deps_mut.load(), usize::MAX)
            };

            if res.count_zeros() > 0 {
                return false;
            }
        }

        true
    }

    #[inline(always)]
    unsafe fn can_borrow_bitmap(borrow: usize, borrow_mut: usize, deps: usize, deps_mut: usize, sync: usize) -> usize {
        !deps | (sync & ((deps_mut & !borrow) | (!deps_mut & !borrow_mut)))
    }
}

unsafe fn bitslice_from_start_len(v: &BitSlice, start: usize, len: usize) -> &BitSlice {
    &*slice_from_raw_parts(v.as_bitptr().add(start), len)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::*;

    #[test]
    fn test_same_borrow() {
        unsafe {
            let mut ctx = GeeseContext::default();
            ctx.add_system::<A>();
            ctx.add_system::<E>();
    
            let inner = ctx.0.borrow();
    
            let mut mgr = SystemBorrowManager::default();
            mgr.configure(&inner);
            assert!(mgr.borrow(inner.system_initializers[&TypeId::of::<A>()].id(), &inner), "{mgr:?}");
            assert!(!mgr.borrow(inner.system_initializers[&TypeId::of::<A>()].id(), &inner), "{mgr:?}");
            assert!(!mgr.borrow(inner.system_initializers[&TypeId::of::<D>()].id(), &inner), "{mgr:?}");
            mgr.release(inner.system_initializers[&TypeId::of::<A>()].id(), &inner);

            assert!(mgr.borrow(inner.system_initializers[&TypeId::of::<B>()].id(), &inner), "{mgr:?}");
            assert!(mgr.borrow(inner.system_initializers[&TypeId::of::<E>()].id(), &inner), "{mgr:?}");
            assert!(!mgr.borrow(inner.system_initializers[&TypeId::of::<C>()].id(), &inner), "{mgr:?}");
            
            mgr.release(inner.system_initializers[&TypeId::of::<B>()].id(), &inner);
            mgr.release(inner.system_initializers[&TypeId::of::<E>()].id(), &inner);

            assert!(mgr.borrow(inner.system_initializers[&TypeId::of::<C>()].id(), &inner), "{mgr:?}");
            assert!(!mgr.borrow(inner.system_initializers[&TypeId::of::<D>()].id(), &inner), "{mgr:?}");
            assert!(!mgr.borrow(inner.system_initializers[&TypeId::of::<E>()].id(), &inner), "{mgr:?}");
        }
    }
}