#[derive(Copy, Clone, Debug)]
pub struct ConstList<'a, T: 'a>(Option<ConstListItem<'a, T>>);

impl<'a, T: 'a> ConstList<'a, T> {
    pub const fn new() -> Self {
        Self(None)
    }

    pub const fn get(&self, index: usize) -> Option<&T> {
        if let Some(value) = &self.0 {
            if index == 0 {
                Some(&value.first)
            }
            else {
                value.rest.get(index - 1)
            }
        }
        else {
            None
        }
    }

    pub const fn len(&self) -> usize {
        if let Some(value) = &self.0 {
            value.rest.len() + 1
        }
        else {
            0
        }
    }

    pub const fn push(&'a self, value: T) -> Self {
        ConstList(Some(ConstListItem { first: value, rest: self }))
    }

    pub const fn pop(&'a self) -> (Option<&T>, &'a Self) {
        if let Some(value) = &self.0 {
            (Some(&value.first), value.rest)
        }
        else {
            (None, self)
        }
    }
}

impl<'a, T> IntoIterator for &'a ConstList<'a, T> {
    type Item = &'a T;

    type IntoIter = ConstListIterator<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        ConstListIterator { target: self }
    }
}

#[derive(Copy, Clone, Debug)]
struct ConstListItem<'a, T: 'a> {
    first: T,
    rest: &'a ConstList<'a, T>
}

pub struct ConstListIterator<'a, T> {
    target: &'a ConstList<'a, T>
}

impl<'a, T> Iterator for ConstListIterator<'a, T> {
    type Item = &'a T;

    fn next(&mut self) -> Option<Self::Item> {
        let (first, rest) = self.target.pop();
        self.target = rest;
        first
    }
}