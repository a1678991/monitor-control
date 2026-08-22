//! The DDC/CI protocol and its Linux transport.

pub mod linux;
pub mod protocol;

pub use linux::{DdcConfig, I2cDdc};
pub use protocol::{DDC_SLAVE_ADDR, VcpValue};
