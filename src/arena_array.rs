use bitvec::prelude::*;
use bitvec::view::*;
use crate::*;
use std::mem::*;

pub struct ArenaArray<T, const ITEM_COUNT: usize, const BLOCK_COUNT: usize> {
    allocation: [MaybeUninit<T>; ITEM_COUNT],
    bits: BitArray<[u64; BLOCK_COUNT]>
}

impl<T, const ITEM_COUNT: usize, const BLOCK_COUNT: usize> ArenaArray<T, ITEM_COUNT, BLOCK_COUNT> {
    const ASSERT_ITEM_COUNT_VALID: () = assert!(ITEM_COUNT == size_of::<u64>() * BLOCK_COUNT, "Item count was not equivalent to block count times block size.");

    pub fn new() -> Self {
        unsafe {
            assert!(Self::ASSERT_ITEM_COUNT_VALID == ());
            
            Self {
                allocation: MaybeUninit::uninit().assume_init(),
                bits: BitArray::new([0; BLOCK_COUNT])
            }
        }
    }

    pub fn spare_capacity(&self) -> usize {
        self.bits.count_zeros()
    }

    pub unsafe fn alloc_unchecked(&mut self, value: T) -> (usize, &mut T) {
        let bit = self.bits.first_zero().unwrap_unchecked();
        let res = self.allocation.get_unchecked_mut(bit).write(value);
        *self.bits.get_unchecked_mut(bit) = true;
        (bit, res)
    }

    pub unsafe fn get_unchecked(&self, index: usize) -> &T {
        self.allocation.get_unchecked(index).assume_init_ref()
    }

    pub unsafe fn get_unchecked_mut(&mut self, index: usize) -> &mut T {
        self.allocation.get_unchecked_mut(index).assume_init_mut()
    }

    pub unsafe fn free_unchecked(&mut self, index: usize) {
        self.allocation.get_unchecked_mut(index).assume_init_drop();
        *self.bits.get_unchecked_mut(index) = false;
    }
}

impl<T, const ITEM_COUNT: usize, const BLOCK_COUNT: usize> Default for ArenaArray<T, ITEM_COUNT, BLOCK_COUNT> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T, const ITEM_COUNT: usize, const BLOCK_COUNT: usize> Drop for ArenaArray<T, ITEM_COUNT, BLOCK_COUNT> {
    fn drop(&mut self) {
        unsafe {
            for bit in self.bits.iter_ones() {
                self.allocation.get_unchecked_mut(bit).assume_init_drop();
            }
        }
    }
}