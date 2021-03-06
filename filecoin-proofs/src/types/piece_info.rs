use std::fmt;

use anyhow::{ensure, Result};

use crate::types::{Commitment, UnpaddedBytesAmount};

#[derive(Clone, Default, PartialEq, Eq)]
pub struct PieceInfo {
    pub commitment: Commitment,
    pub size: UnpaddedBytesAmount,
}

impl fmt::Debug for PieceInfo {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        fmt.debug_struct("PieceInfo")
            .field("commitment", &hex::encode(&self.commitment))
            .field("size", &self.size)
            .finish()
    }
}

impl PieceInfo {
    pub fn new(commitment: Commitment, size: UnpaddedBytesAmount) -> Result<Self> {
        ensure!(commitment != [0; 32], "Invalid all zero commitment");
        Ok(PieceInfo { commitment, size })
    }
}
