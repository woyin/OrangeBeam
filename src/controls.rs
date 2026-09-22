//! REPROG_CONTROLS_V4 top-button notification and temporary configuration.
//! See docs/PROTOCOL.md. Hardware I/O is supplied by the caller.
use crate::Report;
use std::error::Error;

pub type Result<T> = std::result::Result<T, Box<dyn Error>>;
pub const TOP_HOLD: u16 = 0x00d8;

/// Bluetooth requests on the tested first-generation unit use slot 1, while
/// unsolicited notifications use the directly connected device address 0xff.
/// Receiver notifications stay scoped to the selected receiver slot.
pub fn notification_address(bluetooth: bool, receiver_slot: u8) -> u8 {
    if bluetooth {
        0xff
    } else {
        receiver_slot
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reporting {
    pub flags: u16,
    pub mapped_to: u16,
}

impl Reporting {
    fn parse(reply: &Report) -> Result<Self> {
        let p = reply.payload();
        if p.len() < 5 || p[..2] != TOP_HOLD.to_be_bytes() {
            return Err("Truncated or mismatched top-button reporting reply".into());
        }
        let mapped = u16::from_be_bytes([p[3], p[4]]);
        Ok(Self {
            flags: u16::from_le_bytes([p[2], p.get(5).copied().unwrap_or(0)]),
            mapped_to: if mapped == 0 { TOP_HOLD } else { mapped },
        })
    }
}

/// None means unrelated traffic. Some(false) is an explicit state snapshot
/// without the top control, never an inference from a gap in mouse movement.
pub fn top_button_state(report: &Report, notification_index: u8, feature: u8) -> Option<bool> {
    if report.device() != notification_index
        || report.feature() != feature
        || report.software_id() != 0
        || report.function() != 0
        || report.payload().len() < 8
    {
        return None;
    }
    Some(
        report.payload()[..8]
            .as_chunks::<2>()
            .0
            .iter()
            .any(|p| u16::from_be_bytes([p[0], p[1]]) == TOP_HOLD),
    )
}

/// Owns only the temporary divert bit of a previously undiverted top control.
/// Ordinary return paths must call finish to observe restoration errors. Drop
/// attempts the same restoration during unwinding, but cannot handle SIGKILL,
/// process abort, power loss, disconnection, or an inaccessible HID interface.
pub struct TemporaryTopButton<T: FnMut(&Report) -> Result<Report>> {
    transport: T,
    index: u8,
    feature: u8,
    original: Reporting,
    armed: bool,
}

impl<T: FnMut(&Report) -> Result<Report>> TemporaryTopButton<T> {
    pub fn start(mut transport: T, index: u8, feature: u8) -> Result<Self> {
        // Verify the control and its capability before writing anything.
        let count = transport(&Report::request(index, feature, 0, 0x0a, &[])?)?.payload()[0];
        if count > 32 {
            return Err("Unexpected Spotlight control count".into());
        }
        let mut supported = false;
        for position in 0..count {
            let reply = transport(&Report::request(index, feature, 1, 0x0a, &[position])?)?;
            let p = reply.payload();
            if p.len() < 9 {
                return Err("Truncated control capability reply".into());
            }
            if p[..2] == TOP_HOLD.to_be_bytes() {
                supported = p[4] & 0x20 != 0;
                break;
            }
        }
        if !supported {
            return Err(
                "This device does not report a divertible Spotlight top-hold control".into(),
            );
        }
        let original = Reporting::parse(&transport(&Report::request(
            index,
            feature,
            2,
            0x0a,
            &TOP_HOLD.to_be_bytes(),
        )?)?)?;
        if original.flags & 0x15 != 0 {
            return Err(format!(
                "Top control is already diverted or using raw XY (flags={:04x}); another client or an unfinished session may own it. No settings changed.",
                original.flags
            ).into());
        }
        let mut lease = Self {
            transport,
            index,
            feature,
            original,
            armed: true,
        };
        // Arm before writing: a timeout does not mean the device rejected a write.
        let configured = lease.set_divert(true, 0x0a).and_then(|()| {
            let actual = lease.read_reporting(0x0a)?;
            if actual
                != (Reporting {
                    flags: original.flags | 1,
                    ..original
                })
            {
                return Err(format!("Top configuration readback mismatch: {actual:?}").into());
            }
            Ok(())
        });
        if let Err(error) = configured {
            return match lease.restore() {
                Ok(()) => {
                    Err(format!("{error}; original top setting restored and verified").into())
                }
                Err(restore) => Err(format!("{error}; RESTORE FAILED: {restore}").into()),
            };
        }
        Ok(lease)
    }

    pub fn original(&self) -> Reporting {
        self.original
    }

    /// Explicit recovery only when a saved pre-session query proves default top
    /// settings. Never called by normal connection or automatic cleanup.
    pub fn recover_default(mut transport: T, index: u8, feature: u8) -> Result<()> {
        let current = Reporting::parse(&transport(&Report::request(
            index,
            feature,
            2,
            0x0a,
            &TOP_HOLD.to_be_bytes(),
        )?)?)?;
        if current.flags & !1 != 0 || current.mapped_to != TOP_HOLD {
            return Err(
                "Recovery refused: top settings differ from an interrupted default session".into(),
            );
        }
        if current.flags == 0 {
            return Ok(());
        }
        let lease = Self {
            transport,
            index,
            feature,
            original: Reporting {
                flags: 0,
                mapped_to: TOP_HOLD,
            },
            armed: true,
        };
        lease.finish()
    }

    fn set_divert(&mut self, enabled: bool, software_id: u8) -> Result<()> {
        // DVALID only. PVALID/RVALID are zero; remap zero means unchanged.
        let payload = [0x00, 0xd8, 0x02 | u8::from(enabled), 0x00, 0x00];
        let request = Report::request(self.index, self.feature, 3, software_id, &payload)?;
        let reply = (self.transport)(&request)?;
        // The tested Spotlight (feature version 3) acknowledges with a zero
        // payload instead of the v2 spec's echo. Neither form proves application;
        // start and restore always follow it with getCidReporting verification.
        if !reply.payload().starts_with(&payload) && reply.payload().iter().any(|b| *b != 0) {
            return Err("Unexpected setCidReporting acknowledgement".into());
        }
        Ok(())
    }

    fn read_reporting(&mut self, software_id: u8) -> Result<Reporting> {
        Reporting::parse(&(self.transport)(&Report::request(
            self.index,
            self.feature,
            2,
            software_id,
            &TOP_HOLD.to_be_bytes(),
        )?)?)
    }

    fn restore(&mut self) -> Result<()> {
        if !self.armed {
            return Ok(());
        }
        self.armed = false;
        // Different software ID prevents a delayed enable reply matching cleanup.
        self.set_divert(self.original.flags & 1 != 0, 0x0b)?;
        let actual = self.read_reporting(0x0b)?;
        if actual != self.original {
            return Err(format!(
                "Top restoration readback differs: original={:?}, actual={actual:?}; unrelated settings were not overwritten",
                self.original
            ).into());
        }
        Ok(())
    }

    pub fn finish(mut self) -> Result<()> {
        self.restore()
    }
}

impl<T: FnMut(&Report) -> Result<Report>> Drop for TemporaryTopButton<T> {
    fn drop(&mut self) {
        if let Err(error) = self.restore() {
            eprintln!("RESTORE FAILED for Spotlight top control: {error}");
        }
    }
}
