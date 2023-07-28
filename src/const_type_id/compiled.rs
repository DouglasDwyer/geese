use std::any::*;
use std::mem::*;

/// A globally-unique, compile-time derivable identifier for a type.
#[derive(Copy, Clone, Debug)]
pub struct ConstTypeId(TypeId);

impl ConstTypeId {
    /// Gets the ID associated with the given type.
    pub const fn of<T: 'static>() -> Self {
        Self(TypeId::of::<T>())
    }

    /// Determines whether this type ID matches another. This function may only be used in
    /// a `const` context on nightly.
    pub const fn eq(&self, other: &Self) -> bool {
        unsafe {
            Self::arrays_eq(
                transmute::<_, &[u8; size_of::<TypeId>()]>(self),
                transmute(other),
            )
        }
    }

    /// Determines whether the two given arrays are equal.
    const fn arrays_eq<const N: usize>(a: &[u8; N], b: &[u8; N]) -> bool {
        let mut i = 0;
        while i < N {
            if a[i] != b[i] {
                return false;
            }

            i += 1;
        }
        true
    }
}

impl From<ConstTypeId> for TypeId {
    fn from(value: ConstTypeId) -> Self {
        value.0
    }
}
