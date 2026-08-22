//! DDC/CI frame encoding and decoding, independent of any transport.
//!
//! Frames follow the DDC/CI 1.1 / MCCS layout. A request written to the display's
//! I2C slave address looks like:
//!
//! ```text
//! 0x51  0x80|len  <payload...>  <checksum>
//! ```
//!
//! and a reply read back from it looks like:
//!
//! ```text
//! 0x6E  0x80|len  <payload...>  <checksum>
//! ```
//!
//! Both checksums are a running XOR, but they are seeded differently: a request folds in
//! the display's 8-bit write address (`0x6E`), while a reply folds in the host's receive
//! address (`0x50`).

use crate::error::DdcError;

/// 7-bit I2C slave address the display exposes DDC/CI on.
pub const DDC_SLAVE_ADDR: u16 = 0x37;

/// Source address a host puts in the frames it sends.
const HOST_ADDR: u8 = 0x51;
/// The display's write address on the wire, used to seed the request checksum.
const DISPLAY_WRITE_ADDR: u8 = 0x6E;
/// The host's receive address, used to seed the reply checksum.
const HOST_RECV_ADDR: u8 = 0x50;

/// DDC/CI opcodes used by this crate.
pub mod op {
    /// Get VCP Feature request.
    pub const GET_VCP: u8 = 0x01;
    /// Get VCP Feature reply.
    pub const GET_VCP_REPLY: u8 = 0x02;
    /// Set VCP Feature request. The display sends no reply.
    pub const SET_VCP: u8 = 0x03;
    /// Capabilities Request.
    pub const CAPS_REQUEST: u8 = 0xF3;
    /// Capabilities Reply.
    pub const CAPS_REPLY: u8 = 0xE3;
}

/// Longest payload a DDC/CI frame can carry, from the 5-bit length field.
pub const MAX_PAYLOAD: usize = 32;

/// Total bytes in a reply to Get VCP Feature: address, length, 8 payload bytes, checksum.
pub const GET_VCP_REPLY_LEN: usize = 11;

/// Build a complete request frame, including the trailing checksum.
pub fn encode_request(payload: &[u8]) -> Vec<u8> {
    debug_assert!(payload.len() <= MAX_PAYLOAD, "payload exceeds DDC/CI frame limit");
    let mut frame = Vec::with_capacity(payload.len() + 3);
    frame.push(HOST_ADDR);
    frame.push(0x80 | payload.len() as u8);
    frame.extend_from_slice(payload);
    let checksum = frame.iter().fold(DISPLAY_WRITE_ADDR, |acc, b| acc ^ b);
    frame.push(checksum);
    frame
}

/// A reply frame whose framing and checksum have been validated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    /// The payload with framing bytes and checksum stripped.
    pub payload: Vec<u8>,
}

/// Extract the payload from a raw reply, validating framing and checksum.
///
/// `buf` may be longer than the frame; trailing bytes are ignored, because reading a
/// fixed-size buffer from an I2C bus routinely picks up idle-line noise past the frame.
///
/// A zero-length payload is reported as [`DdcError::NullMessage`] rather than an empty
/// success, since that frame is the display's way of declining the request.
pub fn decode_reply(buf: &[u8], strict_checksum: bool) -> Result<Reply, DdcError> {
    if buf.len() < 3 {
        return Err(DdcError::Malformed(format!(
            "reply is {} bytes, need at least 3",
            buf.len()
        )));
    }
    if buf[0] != DISPLAY_WRITE_ADDR {
        // An all-zero or all-0xFF first byte means nothing drove the bus at all.
        if buf.iter().all(|&b| b == 0x00) || buf.iter().all(|&b| b == 0xFF) {
            return Err(DdcError::NoResponse);
        }
        return Err(DdcError::Malformed(format!(
            "source address {:#04x}, expected {DISPLAY_WRITE_ADDR:#04x}",
            buf[0]
        )));
    }
    if buf[1] & 0x80 == 0 {
        return Err(DdcError::Malformed(format!(
            "length byte {:#04x} has its high bit clear",
            buf[1]
        )));
    }

    let len = usize::from(buf[1] & 0x7F);
    if len > MAX_PAYLOAD {
        return Err(DdcError::Malformed(format!(
            "declared payload length {len} exceeds the {MAX_PAYLOAD}-byte maximum"
        )));
    }
    let total = len + 3;
    if buf.len() < total {
        return Err(DdcError::Malformed(format!(
            "declared payload length {len} needs {total} bytes, got {}",
            buf.len()
        )));
    }

    let computed = buf[..total - 1].iter().fold(HOST_RECV_ADDR, |acc, b| acc ^ b);
    let received = buf[total - 1];
    if computed != received {
        // Some displays seed the reply checksum with their own address instead of the
        // host's. Accept that variant unless the caller asked for strict checking.
        let lenient = buf[..total - 1]
            .iter()
            .fold(DISPLAY_WRITE_ADDR, |acc, b| acc ^ b);
        if strict_checksum || lenient != received {
            return Err(DdcError::Checksum { computed, received });
        }
    }

    if len == 0 {
        return Err(DdcError::NullMessage);
    }
    Ok(Reply { payload: buf[2..total - 1].to_vec() })
}

/// The value of a single VCP feature, as reported by Get VCP Feature Reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct VcpValue {
    /// The VCP feature code this value belongs to.
    pub code: u8,
    /// Feature type byte: `0x00` for continuous, `0x01` for non-continuous.
    pub kind: u8,
    /// Maximum value the display advertises for this feature.
    pub maximum: u16,
    /// Current value of the feature.
    pub current: u16,
}

impl VcpValue {
    /// The low byte of the current value.
    ///
    /// Non-continuous features such as Input Source (`0x60`) encode their meaning here;
    /// the high byte carries vendor-specific data that must be ignored.
    pub fn low_byte(&self) -> u8 {
        self.current as u8
    }

    /// Whether the display advertises this feature as continuous.
    pub fn is_continuous(&self) -> bool {
        self.kind == 0x00
    }

    /// The current value as a percentage of the advertised maximum.
    pub fn percent(&self) -> Option<f64> {
        (self.maximum > 0).then(|| f64::from(self.current) * 100.0 / f64::from(self.maximum))
    }
}

/// Build a Get VCP Feature request for `code`.
pub fn get_vcp_request(code: u8) -> Vec<u8> {
    encode_request(&[op::GET_VCP, code])
}

/// Build a Set VCP Feature request writing `value` to `code`.
pub fn set_vcp_request(code: u8, value: u16) -> Vec<u8> {
    let [hi, lo] = value.to_be_bytes();
    encode_request(&[op::SET_VCP, code, hi, lo])
}

/// Build a Capabilities Request for the fragment starting at `offset`.
pub fn capabilities_request(offset: u16) -> Vec<u8> {
    let [hi, lo] = offset.to_be_bytes();
    encode_request(&[op::CAPS_REQUEST, hi, lo])
}

/// Interpret the payload of a Get VCP Feature Reply.
pub fn parse_get_vcp_reply(payload: &[u8], expected: u8) -> Result<VcpValue, DdcError> {
    if payload.len() < 8 {
        return Err(DdcError::Malformed(format!(
            "Get VCP reply payload is {} bytes, need 8",
            payload.len()
        )));
    }
    if payload[0] != op::GET_VCP_REPLY {
        return Err(DdcError::Malformed(format!(
            "opcode {:#04x}, expected {:#04x}",
            payload[0],
            op::GET_VCP_REPLY
        )));
    }
    // Result code 0x01 is the display telling us the feature does not exist.
    if payload[1] == 0x01 {
        return Err(DdcError::UnsupportedFeature { code: expected });
    }
    if payload[1] != 0x00 {
        return Err(DdcError::Malformed(format!(
            "result code {:#04x}",
            payload[1]
        )));
    }
    if payload[2] != expected {
        return Err(DdcError::FeatureMismatch { expected, got: payload[2] });
    }
    Ok(VcpValue {
        code: payload[2],
        kind: payload[3],
        maximum: u16::from_be_bytes([payload[4], payload[5]]),
        current: u16::from_be_bytes([payload[6], payload[7]]),
    })
}

/// Interpret the payload of a Capabilities Reply, returning its offset and text fragment.
pub fn parse_capabilities_reply(payload: &[u8]) -> Result<(u16, &[u8]), DdcError> {
    if payload.len() < 3 {
        return Err(DdcError::Malformed(format!(
            "capabilities reply payload is {} bytes, need at least 3",
            payload.len()
        )));
    }
    if payload[0] != op::CAPS_REPLY {
        return Err(DdcError::Malformed(format!(
            "opcode {:#04x}, expected {:#04x}",
            payload[0],
            op::CAPS_REPLY
        )));
    }
    Ok((u16::from_be_bytes([payload[1], payload[2]]), &payload[3..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Frames captured from a DELL G3223Q and cross-checked against `ddcutil`.
    const REAL_BRIGHTNESS_REQUEST: [u8; 5] = [0x51, 0x82, 0x01, 0x10, 0xAC];
    const REAL_BRIGHTNESS_REPLY: [u8; 11] =
        [0x6E, 0x88, 0x02, 0x00, 0x10, 0x00, 0x00, 0x64, 0x00, 0x64, 0xA4];

    #[test]
    fn request_matches_hardware_capture() {
        assert_eq!(get_vcp_request(0x10), REAL_BRIGHTNESS_REQUEST);
    }

    #[test]
    fn reply_matches_hardware_capture() {
        let reply = decode_reply(&REAL_BRIGHTNESS_REPLY, true).expect("valid frame");
        let value = parse_get_vcp_reply(&reply.payload, 0x10).expect("valid reply");
        assert_eq!(value, VcpValue { code: 0x10, kind: 0x00, maximum: 100, current: 100 });
        assert_eq!(value.percent(), Some(100.0));
    }

    #[test]
    fn trailing_bus_noise_is_ignored() {
        // Reading a fixed-size buffer picks up whatever idles on the line afterwards.
        let mut buf = REAL_BRIGHTNESS_REPLY.to_vec();
        buf.push(0x6E);
        let reply = decode_reply(&buf, true).expect("frame despite trailing noise");
        assert_eq!(parse_get_vcp_reply(&reply.payload, 0x10).unwrap().current, 100);
    }

    #[test]
    fn input_source_reply_uses_low_byte() {
        // Captured from a DELL G3223Q reporting DisplayPort-1: the meaning is in `sl`.
        let raw = [0x6E, 0x88, 0x02, 0x00, 0x60, 0x00, 0x12, 0x12, 0x0F, 0x0F, 0xD4];
        let reply = decode_reply(&raw, true).expect("valid frame");
        let value = parse_get_vcp_reply(&reply.payload, 0x60).expect("valid reply");
        assert_eq!(value.current, 0x0F0F);
        assert_eq!(value.low_byte(), 0x0F);
    }

    #[test]
    fn null_message_is_recognised() {
        // Captured from a 27M2V that answers on the bus but refuses to serve requests.
        let raw = [0x6E, 0x80, 0xBE];
        assert_eq!(decode_reply(&raw, true), Err(DdcError::NullMessage));
    }

    #[test]
    fn null_message_survives_trailing_noise() {
        let raw = [0x6E, 0x80, 0xBE, 0x6E, 0xA3, 0x4E, 0x58, 0xEF];
        assert_eq!(decode_reply(&raw, true), Err(DdcError::NullMessage));
    }

    #[test]
    fn silent_bus_reports_no_response() {
        assert_eq!(decode_reply(&[0x00; 11], true), Err(DdcError::NoResponse));
        assert_eq!(decode_reply(&[0xFF; 11], true), Err(DdcError::NoResponse));
    }

    #[test]
    fn corrupt_checksum_is_rejected_in_strict_mode() {
        let mut raw = REAL_BRIGHTNESS_REPLY;
        raw[10] ^= 0xFF;
        assert!(matches!(
            decode_reply(&raw, true),
            Err(DdcError::Checksum { .. })
        ));
    }

    #[test]
    fn lenient_mode_accepts_display_seeded_checksum() {
        // Some displays seed the reply checksum with 0x6E instead of 0x50; the two seeds
        // differ by 0x3E, so a conforming frame's checksum shifts by exactly that.
        let mut raw = REAL_BRIGHTNESS_REPLY;
        raw[10] ^= 0x50 ^ 0x6E;
        assert!(matches!(
            decode_reply(&raw, true),
            Err(DdcError::Checksum { .. })
        ));
        assert!(decode_reply(&raw, false).is_ok());
    }

    #[test]
    fn unsupported_feature_is_distinguished_from_noise() {
        let payload = [0x02, 0x01, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00];
        assert_eq!(
            parse_get_vcp_reply(&payload, 0x10),
            Err(DdcError::UnsupportedFeature { code: 0x10 })
        );
        // A definitive refusal must not be retried.
        assert!(!DdcError::UnsupportedFeature { code: 0x10 }.is_retryable());
    }

    #[test]
    fn reply_for_the_wrong_feature_is_caught() {
        let payload = [0x02, 0x00, 0x12, 0x00, 0x00, 0x64, 0x00, 0x32];
        assert_eq!(
            parse_get_vcp_reply(&payload, 0x10),
            Err(DdcError::FeatureMismatch { expected: 0x10, got: 0x12 })
        );
    }

    #[test]
    fn set_request_round_trips_through_the_checksum() {
        let frame = set_vcp_request(0x60, 0x11);
        assert_eq!(frame[..6], [0x51, 0x84, 0x03, 0x60, 0x00, 0x11]);
        let checksum = frame[..6].iter().fold(0x6Eu8, |a, b| a ^ b);
        assert_eq!(*frame.last().unwrap(), checksum);
    }

    #[test]
    fn declared_length_longer_than_buffer_is_rejected() {
        let raw = [0x6E, 0x88, 0x02, 0x00];
        assert!(matches!(decode_reply(&raw, true), Err(DdcError::Malformed(_))));
    }
}
