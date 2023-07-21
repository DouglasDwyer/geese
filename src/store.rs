use crate::*;

/// Provides the ability to store data of an arbitrary type as a system.
pub struct Store<T: 'static + Default> {
    /// A reference to the Geese context.
    ctx: GeeseContextHandle<Self>,
    /// Whether this value has already been modified, emitting a changed event as a result.
    dirty: bool,
    /// The value held by the store.
    value: T
}

impl<T: Default> Store<T> {
    /// Resets this system's dirty flag, allowing it to raise more changed events upon mutable access.
    fn reset_dirty(&mut self, _: &on::Changed<T>) {
        self.dirty = false;
    }
}

impl<T: Default> GeeseSystem for Store<T> {
    const EVENT_HANDLERS: EventHandlers<Self> = event_handlers().with(Self::reset_dirty);

    fn new(ctx: GeeseContextHandle<Self>) -> Self {
        let dirty = false;
        let value = T::default();
        Self { ctx, dirty, value }
    }
}

impl<T: Default> Deref for Store<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.value
    }
}

impl<T: Default> DerefMut for Store<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        if !self.dirty {
            self.dirty = true;
            self.ctx.raise_event(on::Changed::<T>::new());
        }

        &mut self.value
    }
}

/// The set of events which this module raises.
pub mod on {
    use super::*;

    /// Raised whenever a store is mutably accessed.
    pub struct Changed<T: 'static>(PhantomData<fn(T)>);

    impl<T: 'static> Changed<T> {
        /// Creates a new changed event for the given store type.
        pub(super) fn new() -> Self {
            Self(PhantomData)
        }
    }
}