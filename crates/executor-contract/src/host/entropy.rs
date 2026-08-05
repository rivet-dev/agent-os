use super::BoundedUsize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntropyOperation {
    pub length: BoundedUsize,
}
