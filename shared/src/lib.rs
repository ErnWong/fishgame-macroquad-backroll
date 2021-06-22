use nanoserde::{DeBin, SerBin};

#[derive(Debug, Clone, SerBin, DeBin, PartialEq)]
pub struct Join(pub u16);

#[derive(Debug, Clone, SerBin, DeBin, PartialEq)]
pub struct Start(pub Vec<(u16, (u16, u8))>);
