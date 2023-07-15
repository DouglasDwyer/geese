use std::any::*;
use std::mem::*;

#[derive(Copy, Clone, Debug)]
pub struct ConstTypeId(TypeId);

impl ConstTypeId {
    pub const fn of<T: 'static>() -> Self {
        Self(TypeId::of::<T>())
    }

    pub const fn eq(&self, other: &Self) -> bool {
        unsafe {
            transmute::<_, u64>(self.0) == transmute::<_, u64>(other.0)
        }
    }
}

impl From<ConstTypeId> for TypeId {
    fn from(value: ConstTypeId) -> Self {
        value.0
    }
}