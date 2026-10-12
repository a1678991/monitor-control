//! Error types for DDC/CI monitor control.

use std::path::PathBuf;

/// Result alias used throughout the crate.
pub type Result<T> = std::result::Result<T, Error>;

/// Why a DDC/CI exchange did not produce a usable answer.
///
/// The distinction matters: [`DdcError::NoResponse`] means nothing is listening on
/// the DDC/CI slave address, so the path is dead. [`DdcError::NullMessage`] means the
/// display *is* on the bus but declined to answer, which almost always means DDC/CI is
/// switched off in the monitor's on-screen menu.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DdcError {
    /// The display did not acknowledge the DDC/CI slave address at all.
    #[error("no response from display (I2C slave 0x37 unresponsive)")]
    NoResponse,

    /// The display replied with a well-formed DDC/CI Null Message.
    ///
    /// The link works but the display refuses to serve requests. On most monitors this
    /// means DDC/CI is disabled in the OSD menu.
    #[error("display returned a DDC/CI null message (DDC/CI is likely disabled in the monitor's OSD menu)")]
    NullMessage,

    /// The reply did not carry the length or opcode the request called for.
    #[error("malformed reply: {0}")]
    Malformed(String),

    /// The reply's checksum did not match the bytes received.
    #[error("checksum mismatch: computed {computed:#04x}, received {received:#04x}")]
    Checksum {
        /// The checksum this end calculated over the received bytes.
        computed: u8,
        /// The checksum byte the display actually sent.
        received: u8,
    },

    /// The display reported that it does not implement the requested VCP feature.
    #[error("display does not support VCP feature {code:#04x}")]
    UnsupportedFeature {
        /// The VCP feature the display rejected.
        code: u8,
    },

    /// The reply concerned a different VCP feature than the one requested.
    #[error("reply was for VCP feature {got:#04x}, expected {expected:#04x}")]
    FeatureMismatch {
        /// The VCP feature that was requested.
        expected: u8,
        /// The VCP feature the reply actually carried.
        got: u8,
    },
}

impl DdcError {
    /// How much this failure reveals about the far end of the link.
    ///
    /// Used to pick which of several failed attempts to report. A null message proves
    /// something is listening on the DDC/CI address, and a corrupt frame proves something
    /// drove the bus; silence proves nothing. Reporting the most informative failure keeps
    /// the diagnosis stable when a flaky display fails differently on each retry.
    pub(crate) fn evidence(&self) -> u8 {
        match self {
            Self::UnsupportedFeature { .. } | Self::FeatureMismatch { .. } => 4,
            Self::NullMessage => 3,
            Self::Checksum { .. } | Self::Malformed(_) => 2,
            Self::NoResponse => 1,
        }
    }

    /// Whether retrying the same request has a realistic chance of succeeding.
    ///
    /// Transport-level noise is worth retrying; a definitive "I don't support that"
    /// is not.
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::NoResponse | Self::Malformed(_) | Self::Checksum { .. } => true,
            // A null message is the display asking us to back off, so a retry is
            // worthwhile, but callers treat a persistent one as a distinct state.
            Self::NullMessage => true,
            Self::UnsupportedFeature { .. } | Self::FeatureMismatch { .. } => false,
        }
    }
}

/// Anything that can go wrong while discovering or driving a monitor.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// An I2C device node could not be opened or used.
    #[error("i2c device {path}: {source}")]
    I2c {
        /// The device node involved.
        path: PathBuf,
        /// The underlying OS error.
        #[source]
        source: std::io::Error,
    },

    /// A filesystem read failed, usually somewhere under `/sys`.
    #[error("{context}: {source}")]
    Io {
        /// What was being attempted.
        context: String,
        /// The underlying OS error.
        #[source]
        source: std::io::Error,
    },

    /// The DDC/CI exchange failed after all retries were used up.
    #[error("DDC/CI on {path}: {source}")]
    Ddc {
        /// The device node the exchange ran over.
        path: PathBuf,
        /// Why the exchange failed.
        #[source]
        source: DdcError,
    },

    /// The bytes claiming to be an EDID could not be parsed.
    #[error("invalid EDID: {0}")]
    Edid(String),

    /// No monitor matched the requested selector.
    #[error("no monitor matched selector `{0}`")]
    NoMatch(String),

    /// Firmware returns a stale input value even after the visible input changes.
    #[error("monitor `{name}` does not reliably report its current input; input switching is still available")]
    InputReadbackUnavailable {
        /// The monitor's display name.
        name: String,
    },

    /// The monitor was found but none of its connection paths can carry DDC/CI.
    #[error("monitor `{name}` has no working DDC/CI path ({detail})")]
    NoWorkingPath {
        /// The monitor's display name.
        name: String,
        /// Per-path status, to show why each candidate was ruled out.
        detail: String,
    },

    /// A VCP value was outside the range the display advertises.
    #[error("value {value} is out of range for VCP {code:#04x} (max {max})")]
    ValueOutOfRange {
        /// The VCP feature being written.
        code: u8,
        /// The value that was rejected.
        value: u16,
        /// The maximum the display advertises.
        max: u16,
    },

    /// A user-supplied string could not be understood.
    #[error("{0}")]
    Parse(String),
}

impl Error {
    pub(crate) fn io(context: impl Into<String>, source: std::io::Error) -> Self {
        Self::Io { context: context.into(), source }
    }
}
