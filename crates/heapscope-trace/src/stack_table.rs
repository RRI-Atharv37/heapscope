//! De-duplicated call stacks. Id 0 means "no stack"; real ids start at 1.

use crate::event::StackId;

#[derive(Clone, Debug, Default)]
pub struct StackTable {
    frames: Vec<u64>,
    ends: Vec<usize>,
}

impl StackTable {
    pub fn len(&self) -> usize {
        self.ends.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ends.is_empty()
    }

    /// Return addresses of `id`, innermost frame first.
    pub fn get(&self, id: StackId) -> Option<&[u64]> {
        let i = (id as usize).checked_sub(1)?;
        let end = *self.ends.get(i)?;
        let start = if i == 0 { 0 } else { self.ends[i - 1] };
        Some(&self.frames[start..end])
    }

    pub(crate) fn push(&mut self, frames: &[u64]) {
        self.frames.extend_from_slice(frames);
        self.ends.push(self.frames.len());
    }
}
