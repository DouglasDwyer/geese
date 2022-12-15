use crate::*;
use std::cell::*;
use std::marker::*;

/// Provides the ability to store data of an arbitrary type as a system.
pub struct Store<T: 'static + Default + Send + Sync> {
    ctx: GeeseContextHandle,
    inner: RefCell<T>
}

impl<T: Default + Send + Sync> Store<T> {
    /// Obtains an immutable reference to the underlying data.
    pub fn read(&self) -> StoreRef<T> {
        StoreRef::new(self.inner.borrow())
    }

    /// Obtains a mutable reference to the underlying data.
    pub fn write(&self) -> StoreRefMut<T> {
        self.ctx.raise_event(on::Changed::<T>::new());
        StoreRefMut::new(self.inner.borrow_mut())
    }
}

impl<T: Default + Send + Sync> GeeseSystem for Store<T> {
    fn new(ctx: GeeseContextHandle) -> Self {
        let inner = RefCell::default();
        Self { ctx, inner }
    }
}

/// Represents an immutable reference to a store.
pub struct StoreRef<'a, T: ?Sized> {
    inner: Ref<'a, T>
}

impl<'a, T: ?Sized> StoreRef<'a, T> {
    /// Creates a new immutable reference to a store from a
    /// `std::cell::RefCell` borrow.
    fn new(inner: Ref<'a, T>) -> Self {
        Self { inner }
    }

    /// Creates a reference to a specific borrowed component of a store.
    pub fn map<U, F>(orig: StoreRef<'a, T>, f: F) -> StoreRef<'a, U> where F: FnOnce(&T) -> &U, U: ?Sized {
        StoreRef::new(Ref::map(orig.inner, f))
    }
}

impl<'a, T: ?Sized> Deref for StoreRef<'a, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

/// Represents a mutable reference to a store.
pub struct StoreRefMut<'a, T: ?Sized> {
    inner: RefMut<'a, T>
}

impl<'a, T: ?Sized> StoreRefMut<'a, T> {
    /// Creates a new mutable reference to a store from a
    /// `std::cell::RefCell` borrow.
    fn new(inner: RefMut<'a, T>) -> Self {
        Self { inner }
    }

    /// Creates a reference to a specific borrowed component of a store.
    pub fn map<U, F>(orig: StoreRefMut<'a, T>, f: F) -> StoreRefMut<'a, U> where F: FnOnce(&mut T) -> &mut U, U: ?Sized {
        StoreRefMut::new(RefMut::map(orig.inner, f))
    }
}

impl<'a, T: ?Sized> Deref for StoreRefMut<'a, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl<'a, T: ?Sized> DerefMut for StoreRefMut<'a, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

/// The set of events which this module raises.
pub mod on {
    use super::*;

    /// Raised whenever a store is mutably accessed.
    pub struct Changed<T: 'static>(PhantomData<T>);

    impl<T: 'static> Changed<T> {
        /// Creates a new changed event for the given store type.
        pub(super) fn new() -> Self {
            Self(PhantomData::default())
        }
    }
}