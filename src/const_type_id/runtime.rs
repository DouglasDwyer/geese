use std::any::*;
use std::mem::*;

#[derive(Copy, Clone, Debug)]
pub struct ConstTypeId(fn() -> TypeId);

impl ConstTypeId {
    pub const fn of<T: 'static>() -> Self {
        Self(TypeId::of::<T>)
    }

    pub const fn eq(&self, other: &Self) -> bool {
        unsafe {
            transmute::<_, usize>(self.0) == transmute::<_, usize>(other.0)
        }
    }
}

impl From<ConstTypeId> for TypeId {
    fn from(value: ConstTypeId) -> Self {
        (value.0)()
    }
}