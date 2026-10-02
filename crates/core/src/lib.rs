pub mod crdt;
pub mod editor;
pub mod markdown;
pub mod model;
pub mod storage;
pub mod sync;

/// A small deterministic generator for randomized tests, so failures
/// reproduce from the seed.
#[cfg(test)]
pub(crate) struct Rng(pub(crate) u64);

#[cfg(test)]
impl Rng {
    pub(crate) fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    pub(crate) fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}
