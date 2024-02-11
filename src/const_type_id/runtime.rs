use std::any::*;
use std::mem::*;

/// A globally-unique, compile-time derivable identifier for a type.
#[derive(Copy, Clone, Debug)]
pub struct ConstTypeId(fn() -> TypeId);

impl ConstTypeId {
    /// Gets the ID associated with the given type.
    pub const fn of<T: 'static>() -> Self {
        Self(TypeId::of::<T>)
    }

    /// Determines whether this type ID matches another. This function may only be used in
    /// a `const` context on nightly.
    #[allow(clippy::transmutes_expressible_as_ptr_casts)]
    pub const fn eq(&self, other: &Self) -> bool {
        unsafe { transmute::<_, usize>(self.0) == transmute::<_, usize>(other.0) }
    }
}

impl From<ConstTypeId> for TypeId {
    fn from(value: ConstTypeId) -> Self {
        (value.0)()
    }
}
