//! How much is in the heap.

use super::{BLOCK_SIZE, Heap};

impl Heap {
    pub fn block_bytes(&self) -> usize {
        self.blocks.borrow().len() * BLOCK_SIZE + self.precise_bytes.get()
    }

    pub fn capacity(&self) -> usize {
        self.block_bytes().saturating_add(self.extra_memory_size())
    }

    /// The count and the bytes of what the last collection has left.
    fn survivors(&self) -> (usize, usize) {
        let (mut count, mut bytes) = (0, 0);
        self.for_each_directory(|directory| {
            let blocks = directory.blocks_with_marks();
            let marks: usize = blocks.iter().map(|block| block.counted_marks()).sum();
            count += marks;
            bytes += marks * directory.cell_size();
        });
        for allocation in self.precise.borrow().iter().filter(|it| it.is_marked()) {
            count += 1;
            bytes += allocation.cell_size();
        }
        (count, bytes)
    }

    pub fn size(&self) -> usize {
        self.survivors().1.saturating_add(self.extra_memory_size())
    }

    pub fn object_count(&self) -> usize {
        self.survivors().0
    }
}
