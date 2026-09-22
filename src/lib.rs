//! Platform-independent HID++ codec and control lifecycle; callers supply I/O.
//!
//! Wire-format references and limitations are recorded in docs/PROTOCOL.md.
use std::fmt;

pub mod controls;
pub mod presentation;

pub const LOGITECH_VENDOR: u16 = 0x046d;
pub const SOFTWARE_ID: u8 = 0x0a;

/// Only first-generation Spotlight IDs are eligible for device access.
/// Shared Bolt receiver IDs do not identify a Spotlight and are deliberately excluded.
pub fn is_spotlight(vendor: u16, product: u16) -> bool {
    vendor == LOGITECH_VENDOR && matches!(product, 0xc53e | 0xb503)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtocolError {
    InvalidReport,
    InvalidFunction,
    InvalidSoftwareId,
    PayloadTooLong,
    UnrelatedResponse,
    Device { legacy: bool, code: u8 },
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for ProtocolError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    bytes: [u8; 20],
    len: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResponseKind {
    Reply,
    Error { legacy: bool, code: u8 },
    Unrelated,
}

impl Report {
    /// Incoming buffers include the report ID. Reject truncated and overlong reports.
    pub fn parse(data: &[u8]) -> Result<Self, ProtocolError> {
        if !matches!(
            (data.first(), data.len()),
            (Some(0x10), 7) | (Some(0x11), 20)
        ) {
            return Err(ProtocolError::InvalidReport);
        }
        let mut bytes = [0; 20];
        bytes[..data.len()].copy_from_slice(data);
        Ok(Self {
            bytes,
            len: data.len(),
        })
    }

    /// Always send long reports: first-generation Bluetooth requires this format.
    pub fn request(
        device: u8,
        feature: u8,
        function: u8,
        software_id: u8,
        payload: &[u8],
    ) -> Result<Self, ProtocolError> {
        if function > 15 {
            return Err(ProtocolError::InvalidFunction);
        }
        if !(1..=15).contains(&software_id) {
            return Err(ProtocolError::InvalidSoftwareId);
        }
        if payload.len() > 16 {
            return Err(ProtocolError::PayloadTooLong);
        }
        let mut bytes = [0; 20];
        bytes[..4].copy_from_slice(&[0x11, device, feature, (function << 4) | software_id]);
        bytes[4..4 + payload.len()].copy_from_slice(payload);
        Ok(Self { bytes, len: 20 })
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes[..self.len]
    }
    pub fn payload(&self) -> &[u8] {
        &self.bytes[4..self.len]
    }
    pub fn device(&self) -> u8 {
        self.bytes[1]
    }
    pub fn feature(&self) -> u8 {
        self.bytes[2]
    }
    pub fn function(&self) -> u8 {
        self.bytes[3] >> 4
    }
    pub fn software_id(&self) -> u8 {
        self.bytes[3] & 15
    }

    /// Distinguish notifications, other applications' replies, and matching errors.
    pub fn response_to(&self, request: &Self) -> ResponseKind {
        if self.device() != request.device() {
            return ResponseKind::Unrelated;
        }
        if matches!(self.feature(), 0x8f | 0xfe) {
            if self.bytes[3] == request.feature() && self.bytes[4] == request.bytes[3] {
                return ResponseKind::Error {
                    legacy: self.feature() == 0x8f,
                    code: self.bytes[5],
                };
            }
            return ResponseKind::Unrelated;
        }
        if self.feature() == request.feature() && self.bytes[3] == request.bytes[3] {
            ResponseKind::Reply
        } else {
            ResponseKind::Unrelated
        }
    }
}

pub fn feature_request(device: u8, feature_code: u16) -> Report {
    let [high, low] = feature_code.to_be_bytes();
    Report::request(device, 0, 0, SOFTWARE_ID, &[high, low, 0])
        .expect("fixed getFeature request is valid")
}

/// Resolve a feature code through ROOT; index zero means unsupported.
pub fn feature_index(reply: &Report, request: &Report) -> Result<Option<u8>, ProtocolError> {
    if request.feature() != 0 || request.function() != 0 {
        return Err(ProtocolError::UnrelatedResponse);
    }
    match reply.response_to(request) {
        ResponseKind::Reply => Ok(match reply.payload()[0] {
            0 => None,
            n => Some(n),
        }),
        ResponseKind::Error { legacy, code } => Err(ProtocolError::Device { legacy, code }),
        ResponseKind::Unrelated => Err(ProtocolError::UnrelatedResponse),
    }
}
