use std::cell::*;
use std::mem::*;
use std::ops::*;
use std::sync::atomic::*;

/// A read-only cell that allows immutable references for the inner data to
/// be held simultaneously as mutable references to the outer data.
#[derive(Debug, Default)]
pub struct ReadCell<T> {
    /// The underlying value.
    inner: UnsafeCell<T>,
}

impl<T> ReadCell<T> {
    /// Creates a new cell with the given value.
    #[inline(always)]
    pub const fn new(value: T) -> Self {
        Self {
            inner: UnsafeCell::new(value),
        }
    }
}

impl<T> Deref for ReadCell<T> {
    type Target = T;

    #[inline(always)]
    fn deref(&self) -> &Self::Target {
        unsafe { &*self.inner.get() }
    }
}

/// A lightweight reference-counted cell. Aborts the program when borrows conflict.
#[derive(Debug, Default)]
pub struct RwCell<T> {
    /// The inner cell data.
    inner: ReadCell<RwCellInner<T>>,
}

impl<T> RwCell<T> {
    /// Creates a new cell that wraps the provided value.
    #[inline(always)]
    pub const fn new(value: T) -> Self {
        Self {
            inner: ReadCell::new(RwCellInner {
                borrow_state: AtomicU16::new(0),
                value: UnsafeCell::new(value),
            }),
        }
    }

    /// Immutably borrows the value of this cell.
    #[inline(always)]
    pub fn borrow(&self) -> RwCellGuard<T> {
        unsafe {
            Self::abort_if(
                self.inner.borrow_state.fetch_add(1, Ordering::AcqRel) >= u16::MAX - 1,
                "Attempted to immutably borrow cell while it was mutably borrowed.",
            );
            RwCellGuard {
                value: &*(self.inner.value.get() as *const T),
                borrow_state: &self.inner.borrow_state,
            }
        }
    }

    /// Mutably borrows the value of this cell.
    #[inline(always)]
    pub fn borrow_mut(&self) -> RwCellGuardMut<T> {
        unsafe {
            Self::abort_if(
                self.inner.borrow_state.swap(u16::MAX, Ordering::AcqRel) != 0,
                "Attempted to mutably borrow cell while other borrows already existed.",
            );
            RwCellGuardMut {
                value: &mut *self.inner.value.get(),
                borrow_state: &self.inner.borrow_state,
            }
        }
    }

    /// Determines whether this cell is free to be borrowed.
    #[inline(always)]
    pub fn free(&self) -> bool {
        self.inner.borrow_state.load(Ordering::Acquire) == 0
    }

    /// Aborts the program if the given condition is true.
    #[inline(always)]
    fn abort_if(condition: bool, reason: &str) {
        if condition {
            AbortPanic::abort(reason);
        }
    }
}

/// Stores the inner data for a read-write cell.
#[derive(Debug, Default)]
struct RwCellInner<T> {
    /// The value contained in the cell.
    value: UnsafeCell<T>,
    /// The borrow counter.
    borrow_state: AtomicU16,
}

/// A resource guard that dynamically controls the lifetime of an immutable read-write cell borrow.
#[derive(Debug)]
pub struct RwCellGuard<'a, T: ?Sized> {
    /// The value currently being borrowed.
    value: &'a T,
    /// The borrow counter.
    borrow_state: &'a AtomicU16,
}

impl<'a, T: ?Sized> RwCellGuard<'a, T> {
    /// Removes the lifetime from a guard, allowing it to be freely used in
    /// other parts of the program.
    ///
    /// # Safety
    ///
    /// For this function to be sound, the underlying cell must not be moved or
    /// destroyed while this guard exists.
    #[inline(always)]
    pub unsafe fn detach(self) -> RwCellGuard<'static, T> {
        transmute(self)
    }

    /// Creates a reference to a specific portion of a value.
    #[inline(always)]
    pub fn map<U, F>(orig: Self, f: F) -> RwCellGuard<'a, U>
    where
        F: FnOnce(&T) -> &U,
        U: ?Sized,
    {
        let result = RwCellGuard {
            value: f(orig.value),
            borrow_state: orig.borrow_state,
        };
        forget(orig);
        result
    }
}

impl<'a, T: ?Sized> Deref for RwCellGuard<'a, T> {
    type Target = T;

    #[inline(always)]
    fn deref(&self) -> &Self::Target {
        self.value
    }
}

impl<'a, T: ?Sized> Drop for RwCellGuard<'a, T> {
    #[inline(always)]
    fn drop(&mut self) {
        self.borrow_state.fetch_sub(1, Ordering::AcqRel);
    }
}

/// A resource guard that dynamically controls the lifetime of a mutable read-write cell borrow.
#[derive(Debug)]
pub struct RwCellGuardMut<'a, T: ?Sized> {
    /// The value currently being borrowed.
    value: &'a mut T,
    /// The borrow counter.
    borrow_state: &'a AtomicU16,
}

impl<'a, T: ?Sized> RwCellGuardMut<'a, T> {
    /// Removes the lifetime from a guard, allowing it to be freely used in
    /// other parts of the program.
    ///
    /// # Safety
    ///
    /// For this function to be sound, the underlying cell must not be moved or
    /// destroyed while this guard exists.
    #[inline(always)]
    pub unsafe fn detach(self) -> RwCellGuardMut<'static, T> {
        transmute(self)
    }

    /// Creates a reference to a specific portion of a value.
    #[inline(always)]
    pub fn map<U, F>(orig: Self, f: F) -> RwCellGuardMut<'a, U>
    where
        F: FnOnce(&mut T) -> &mut U,
        U: ?Sized,
    {
        let RwCellGuardMutDestructure {
            value,
            borrow_state,
        } = orig.into();

        RwCellGuardMut {
            value: f(value),
            borrow_state,
        }
    }
}

impl<'a, T: ?Sized> Deref for RwCellGuardMut<'a, T> {
    type Target = T;

    #[inline(always)]
    fn deref(&self) -> &Self::Target {
        self.value
    }
}

impl<'a, T: ?Sized> DerefMut for RwCellGuardMut<'a, T> {
    #[inline(always)]
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.value
    }
}

impl<'a, T: ?Sized> Drop for RwCellGuardMut<'a, T> {
    #[inline(always)]
    fn drop(&mut self) {
        self.borrow_state.store(0, Ordering::Release);
    }
}

/// Allows for destructure a mutable guard without running its destructor.
struct RwCellGuardMutDestructure<'a, T: ?Sized> {
    /// The value currently being borrowed.
    value: &'a mut T,
    /// The borrow counter.
    borrow_state: &'a AtomicU16,
}

impl<'a, T: ?Sized> From<RwCellGuardMut<'a, T>> for RwCellGuardMutDestructure<'a, T> {
    #[inline(always)]
    fn from(value: RwCellGuardMut<'a, T>) -> Self {
        unsafe {
            let mut result = MaybeUninit::uninit();
            (result.as_mut_ptr() as *mut RwCellGuardMut<T>).write(value);
            result.assume_init()
        }
    }
}

/// Implements an uncatchable panic.
struct AbortPanic(*const str);

impl AbortPanic {
    /// Immediately aborts the program with the given message.
    #[allow(unused_variables)]
    #[inline(always)]
    fn abort(message: &str) -> ! {
        let guard = Self(message);
        panic!("{:?}", message);
    }
}

impl Drop for AbortPanic {
    fn drop(&mut self) {
        unsafe {
            panic!("{:?}", &*self.0);
        }
    }
}
