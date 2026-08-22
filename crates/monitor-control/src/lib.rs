//! Control monitor brightness and input source over DDC/CI.
//!
//! # Multiple paths to one monitor
//!
//! A monitor plugged into a machine twice — say HDMI and DisplayPort — shows up as two DRM
//! connectors and two I2C buses, and DDC/CI often works over only one of them. Worse, a
//! single DisplayPort connector exposes *two* candidate buses in sysfs: a physical I2C
//! adapter and an AUX-channel adapter, and only the AUX one carries DDC/CI.
//!
//! This crate models that directly. Displays are grouped by EDID identity into
//! [`Monitor`]s, each holding every [`MonitorPath`] that reaches it. Paths are probed to
//! find which ones answer, and commands automatically fail over when the path in use stops
//! working — as it does when a monitor sleeps or switches input.
//!
//! Probing tells three failure modes apart, because they call for different responses:
//! [`PathStatus::Unresponsive`] means nothing is there, [`PathStatus::DdcDisabled`] means
//! the display answered with a null message and needs DDC/CI enabled in its OSD menu, and
//! [`PathStatus::Unavailable`] means the bus could not be opened at all, usually a
//! permissions problem.
//!
//! # Example
//!
//! ```no_run
//! use monitor_control::{DdcConfig, MonitorManager, ValueChange};
//!
//! let mut manager = MonitorManager::discover(DdcConfig::default())?;
//! for monitor in manager.monitors_mut() {
//!     // Finds a working path on demand, probing only as far as it needs to.
//!     match monitor.set_brightness(ValueChange::Absolute(50)) {
//!         Ok(value) => println!("{}: brightness {value}", monitor.name()),
//!         Err(e) => eprintln!("{}: {e}", monitor.name()),
//!     }
//! }
//! # Ok::<(), monitor_control::Error>(())
//! ```
//!
//! # Permissions
//!
//! Talking to a display needs read/write access to its `/dev/i2c-*` node. Load the
//! `i2c-dev` module and either join the `i2c` group or install a udev rule granting your
//! user an ACL on those nodes; running as root also works but is rarely necessary.

#![warn(missing_docs)]

pub mod capabilities;
pub mod ddc;
pub mod discovery;
pub mod edid;
pub mod error;
pub mod monitor;
pub mod vcp;

pub use capabilities::Capabilities;
pub use ddc::{DdcConfig, I2cDdc, VcpValue};
pub use discovery::{ConnectorKind, PathSource};
pub use edid::{Edid, EdidId};
pub use error::{DdcError, Error, Result};
pub use monitor::{Monitor, MonitorManager, MonitorPath, PathStatus, Selector};
pub use vcp::{InputSource, ValueChange, code};
