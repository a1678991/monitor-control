//! DDC/CI transport over Linux `i2c-dev`.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::{AsRawFd, RawFd};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::protocol::{self, DDC_SLAVE_ADDR, GET_VCP_REPLY_LEN, MAX_PAYLOAD, VcpValue};
use crate::error::{DdcError, Error, Result};

/// `I2C_SLAVE` from `linux/i2c-dev.h`: set the slave address for subsequent transfers.
const I2C_SLAVE: libc::c_ulong = 0x0703;
/// `I2C_SLAVE_FORCE`: as above, but claim the address even if a kernel driver holds it.
const I2C_SLAVE_FORCE: libc::c_ulong = 0x0706;

/// Timing and retry policy for DDC/CI exchanges.
///
/// The defaults follow the DDC/CI specification's minimum delays. Displays vary wildly in
/// how strictly they need these honoured, so [`DdcConfig::sleep_multiplier`] scales all of
/// them at once for stubborn hardware.
#[derive(Debug, Clone)]
pub struct DdcConfig {
    /// How many times to re-send a request that failed in a retryable way.
    pub retries: u32,
    /// Delay between writing a request and reading its reply.
    pub read_delay: Duration,
    /// Delay after a Set VCP Feature, which the display needs to apply the change.
    pub post_set_delay: Duration,
    /// Minimum quiet time between consecutive operations on the same bus.
    pub min_command_interval: Duration,
    /// Scales every delay above. Raise it for displays that answer unreliably.
    pub sleep_multiplier: f64,
    /// Reject replies whose checksum does not use the specification's seed.
    pub strict_checksum: bool,
}

impl Default for DdcConfig {
    fn default() -> Self {
        Self {
            retries: 3,
            read_delay: Duration::from_millis(50),
            post_set_delay: Duration::from_millis(50),
            min_command_interval: Duration::from_millis(40),
            sleep_multiplier: 1.0,
            strict_checksum: false,
        }
    }
}

impl DdcConfig {
    fn scale(&self, d: Duration) -> Duration {
        if (self.sleep_multiplier - 1.0).abs() < f64::EPSILON {
            d
        } else {
            d.mul_f64(self.sleep_multiplier.max(0.0))
        }
    }
}

/// An open DDC/CI channel to one I2C bus.
///
/// The struct owns the file descriptor and tracks when it last drove the bus, so it can
/// enforce the specification's minimum quiet time without the caller thinking about it.
#[derive(Debug)]
pub struct I2cDdc {
    file: File,
    path: PathBuf,
    config: DdcConfig,
    last_op: Option<Instant>,
}

impl I2cDdc {
    /// Open `/dev/i2c-{bus}` and claim the DDC/CI slave address.
    pub fn open(bus: u32, config: DdcConfig) -> Result<Self> {
        Self::open_path(PathBuf::from(format!("/dev/i2c-{bus}")), config)
    }

    /// Open a specific I2C device node and claim the DDC/CI slave address.
    pub fn open_path(path: PathBuf, config: DdcConfig) -> Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|source| Error::I2c { path: path.clone(), source })?;

        set_slave_address(file.as_raw_fd(), DDC_SLAVE_ADDR)
            .map_err(|source| Error::I2c { path: path.clone(), source })?;

        Ok(Self { file, path, config, last_op: None })
    }

    /// The device node this channel drives.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Wait out the configured quiet time since the last bus operation.
    fn wait_for_bus(&mut self) {
        let interval = self.config.scale(self.config.min_command_interval);
        if let Some(last) = self.last_op {
            let elapsed = last.elapsed();
            if elapsed < interval {
                std::thread::sleep(interval - elapsed);
            }
        }
    }

    /// Write one I2C transaction.
    ///
    /// A DDC/CI frame must reach the display as a single transfer, so this deliberately
    /// avoids `write_all`: a short write is a protocol failure, not something to resume.
    fn write_frame(&mut self, frame: &[u8]) -> Result<()> {
        self.wait_for_bus();
        let n = unsafe {
            libc::write(
                self.file.as_raw_fd(),
                frame.as_ptr().cast::<libc::c_void>(),
                frame.len(),
            )
        };
        self.last_op = Some(Instant::now());
        if n < 0 {
            return Err(Error::I2c {
                path: self.path.clone(),
                source: io::Error::last_os_error(),
            });
        }
        if n as usize != frame.len() {
            return Err(Error::I2c {
                path: self.path.clone(),
                source: io::Error::other(format!(
                    "short write: {n} of {} bytes",
                    frame.len()
                )),
            });
        }
        Ok(())
    }

    /// Read one I2C transaction of up to `len` bytes.
    fn read_frame(&mut self, len: usize) -> Result<Vec<u8>> {
        let mut buf = vec![0u8; len];
        let n = unsafe {
            libc::read(
                self.file.as_raw_fd(),
                buf.as_mut_ptr().cast::<libc::c_void>(),
                buf.len(),
            )
        };
        self.last_op = Some(Instant::now());
        if n < 0 {
            return Err(Error::I2c {
                path: self.path.clone(),
                source: io::Error::last_os_error(),
            });
        }
        buf.truncate(n as usize);
        Ok(buf)
    }

    /// Send a request and read its reply, retrying transient failures.
    ///
    /// I/O errors and malformed replies are both retried, because a monitor that is busy
    /// refreshing tends to NAK the address and to answer with garbage in roughly equal
    /// measure. Retries back off linearly to give a struggling display room to recover.
    fn exchange(&mut self, request: &[u8], reply_len: usize) -> Result<Vec<u8>> {
        let mut last: Option<DdcError> = None;

        for attempt in 0..=self.config.retries {
            if attempt > 0 {
                std::thread::sleep(self.config.scale(self.config.min_command_interval) * attempt);
            }

            let failure = match self.try_exchange(request, reply_len) {
                Ok(payload) => return Ok(payload),
                Err(Error::Ddc { source, .. }) if source.is_retryable() => source,
                // An I/O-level failure means the display did not take the address.
                Err(Error::I2c { .. }) => DdcError::NoResponse,
                Err(other) => return Err(other),
            };

            // Keep the most informative failure rather than the most recent one, so a
            // display that answers with a null message once and then goes quiet is still
            // reported as present-but-refusing.
            if last.as_ref().is_none_or(|prev| failure.evidence() > prev.evidence()) {
                last = Some(failure);
            }
        }

        Err(Error::Ddc {
            path: self.path.clone(),
            source: last.unwrap_or(DdcError::NoResponse),
        })
    }

    /// One request/reply round trip, without retries.
    fn try_exchange(&mut self, request: &[u8], reply_len: usize) -> Result<Vec<u8>> {
        self.write_frame(request)?;
        std::thread::sleep(self.config.scale(self.config.read_delay));
        // Read one byte past the frame: some displays append a stop byte, and asking for
        // it costs nothing while a short read would look like truncation.
        let raw = self.read_frame(reply_len + 1)?;
        protocol::decode_reply(&raw, self.config.strict_checksum)
            .map(|r| r.payload)
            .map_err(|source| Error::Ddc { path: self.path.clone(), source })
    }

    /// Read a VCP feature.
    pub fn get_vcp(&mut self, code: u8) -> Result<VcpValue> {
        let payload = self.exchange(&protocol::get_vcp_request(code), GET_VCP_REPLY_LEN)?;
        protocol::parse_get_vcp_reply(&payload, code)
            .map_err(|source| Error::Ddc { path: self.path.clone(), source })
    }

    /// Write a VCP feature.
    ///
    /// Set VCP Feature is unacknowledged, so this reports success once the frame has gone
    /// out; verify with [`I2cDdc::get_vcp`] when confirmation matters.
    pub fn set_vcp(&mut self, code: u8, value: u16) -> Result<()> {
        self.write_frame(&protocol::set_vcp_request(code, value))?;
        std::thread::sleep(self.config.scale(self.config.post_set_delay));
        Ok(())
    }

    /// Read the display's capabilities string.
    ///
    /// The string arrives in fragments; this walks the offsets until the display returns
    /// an empty fragment. The loop is bounded so a display that keeps re-sending the same
    /// offset cannot hang the caller.
    pub fn capabilities(&mut self) -> Result<String> {
        let mut out: Vec<u8> = Vec::new();
        let mut offset: u16 = 0;

        // Capabilities strings run to a few hundred bytes; this ceiling is far above any
        // real display while still bounding a misbehaving one.
        const MAX_FRAGMENTS: usize = 256;

        for _ in 0..MAX_FRAGMENTS {
            let payload = self.exchange(
                &protocol::capabilities_request(offset),
                MAX_PAYLOAD + 3,
            )?;
            let (reply_offset, fragment) = protocol::parse_capabilities_reply(&payload)
                .map_err(|source| Error::Ddc { path: self.path.clone(), source })?;

            if fragment.is_empty() {
                break;
            }
            // A display that ignores our offset would otherwise loop forever.
            if reply_offset != offset {
                return Err(Error::Ddc {
                    path: self.path.clone(),
                    source: DdcError::Malformed(format!(
                        "capabilities fragment at offset {reply_offset}, expected {offset}"
                    )),
                });
            }
            out.extend_from_slice(fragment);
            offset = offset.saturating_add(fragment.len() as u16);
        }

        Ok(String::from_utf8_lossy(&out).trim().to_string())
    }
}

/// Point the file descriptor at a slave address, forcing past any bound kernel driver.
fn set_slave_address(fd: RawFd, addr: u16) -> io::Result<()> {
    // Try the polite request first; fall back to the forcing variant, which is what
    // matters when a driver such as `ddcci-backlight` already holds address 0x37.
    for request in [I2C_SLAVE, I2C_SLAVE_FORCE] {
        let rc = unsafe { libc::ioctl(fd, request, libc::c_ulong::from(addr)) };
        if rc >= 0 {
            return Ok(());
        }
    }
    Err(io::Error::last_os_error())
}
