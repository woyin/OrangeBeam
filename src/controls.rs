//! REPROG_CONTROLS_V4 top-button notification and temporary configuration.
//! See docs/PROTOCOL.md. Hardware I/O is supplied by the caller.
use crate::Report;
use std::error::Error;

pub type Result<T> = std::result::Result<T, Box<dyn Error>>;
pub const TOP_HOLD: u16 = 0x00d8;
/// Firmware-detected top-button double click ("Switch Highlight").
pub const SWITCH_HIGHLIGHT: u16 = 0x00df;
pub const NEXT_HOLD: u16 = 0x00da;
pub const BACK_HOLD: u16 = 0x00dc;

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
    fn parse(reply: &Report, cid: u16) -> Result<Self> {
        let p = reply.payload();
        if p.len() < 5 || p[..2] != cid.to_be_bytes() {
            return Err(
                format!("Truncated or mismatched reporting reply for control {cid:04x}").into(),
            );
        }
        let mapped = u16::from_be_bytes([p[3], p[4]]);
        Ok(Self {
            flags: u16::from_le_bytes([p[2], p.get(5).copied().unwrap_or(0)]),
            mapped_to: if mapped == 0 { cid } else { mapped },
        })
    }
}

/// Controls reported as currently held by a diverted-button notification
/// (REPROG_CONTROLS_V4 event 0). None means unrelated traffic; an empty list
/// is an explicit "nothing held" snapshot.
pub fn active_controls(report: &Report, notification_index: u8, feature: u8) -> Option<Vec<u16>> {
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
            .map(|p| u16::from_be_bytes([p[0], p[1]]))
            .filter(|cid| *cid != 0)
            .collect(),
    )
}

/// None means unrelated traffic. Some(false) is an explicit state snapshot
/// without the top control, never an inference from a gap in mouse movement.
pub fn top_button_state(report: &Report, notification_index: u8, feature: u8) -> Option<bool> {
    active_controls(report, notification_index, feature).map(|held| held.contains(&TOP_HOLD))
}

/// Owns only the temporary divert bit of one previously undiverted control
/// (the top hold by default).
/// Ordinary return paths must call finish to observe restoration errors. Drop
/// attempts the same restoration during unwinding, but cannot handle SIGKILL,
/// process abort, power loss, disconnection, or an inaccessible HID interface.
pub struct TemporaryTopButton<T: FnMut(&Report) -> Result<Report>> {
    transport: T,
    index: u8,
    feature: u8,
    cid: u16,
    original: Reporting,
    armed: bool,
}

impl<T: FnMut(&Report) -> Result<Report>> TemporaryTopButton<T> {
    pub fn start(transport: T, index: u8, feature: u8) -> Result<Self> {
        Self::start_control(transport, index, feature, TOP_HOLD)
    }

    pub fn start_control(transport: T, index: u8, feature: u8, cid: u16) -> Result<Self> {
        Self::start_or_adopt(transport, index, feature, cid, None)
    }

    /// `unrestored` is this process's own recorded original from an earlier
    /// session whose restoration failed (typically a disconnect). If the device
    /// still shows exactly that state plus the divert bit, the diversion is
    /// adopted without writing. Any other preexisting diversion is refused.
    pub fn start_or_adopt(
        mut transport: T,
        index: u8,
        feature: u8,
        cid: u16,
        unrestored: Option<Reporting>,
    ) -> Result<Self> {
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
            if p[..2] == cid.to_be_bytes() {
                supported = p[4] & 0x20 != 0;
                break;
            }
        }
        if !supported {
            return Err(format!(
                "This device does not report a divertible Spotlight control {cid:04x}"
            )
            .into());
        }
        let original = Reporting::parse(
            &transport(&Report::request(
                index,
                feature,
                2,
                0x0a,
                &cid.to_be_bytes(),
            )?)?,
            cid,
        )?;
        if original.flags & 0x15 != 0 {
            if let Some(previous) = unrestored {
                let ours = Reporting {
                    flags: previous.flags | 1,
                    ..previous
                };
                if previous.flags & 0x15 == 0 && original == ours {
                    return Ok(Self {
                        transport,
                        index,
                        feature,
                        cid,
                        original: previous,
                        armed: true,
                    });
                }
            }
            return Err(format!(
                "Control {cid:04x} is already diverted or using raw XY (flags={:04x}); another client or an unfinished session may own it. No settings changed.",
                original.flags
            ).into());
        }
        let mut lease = Self {
            transport,
            index,
            feature,
            cid,
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
                return Err(format!("Control {cid:04x} readback mismatch: {actual:?}").into());
            }
            Ok(())
        });
        if let Err(error) = configured {
            return match lease.restore() {
                Ok(()) => Err(format!(
                    "{error}; original setting of control {cid:04x} restored and verified"
                )
                .into()),
                Err(restore) => Err(format!("{error}; RESTORE FAILED: {restore}").into()),
            };
        }
        Ok(lease)
    }

    pub fn original(&self) -> Reporting {
        self.original
    }

    pub fn control(&self) -> u16 {
        self.cid
    }

    /// Explicit recovery only when a saved pre-session query proves default top
    /// settings. Never called by normal connection or automatic cleanup.
    pub fn recover_default(transport: T, index: u8, feature: u8) -> Result<()> {
        Self::recover_default_control(transport, index, feature, TOP_HOLD)
    }

    /// Same explicit recovery for any control whose documented pre-session
    /// state was flags 0000 and a mapping to itself.
    pub fn recover_default_control(
        mut transport: T,
        index: u8,
        feature: u8,
        cid: u16,
    ) -> Result<()> {
        let current = Reporting::parse(
            &transport(&Report::request(
                index,
                feature,
                2,
                0x0a,
                &cid.to_be_bytes(),
            )?)?,
            cid,
        )?;
        if current.flags & !1 != 0 || current.mapped_to != cid {
            return Err(format!(
                "Recovery refused: control {cid:04x} settings differ from an interrupted default session"
            )
            .into());
        }
        if current.flags == 0 {
            return Ok(());
        }
        let lease = Self {
            transport,
            index,
            feature,
            cid,
            original: Reporting {
                flags: 0,
                mapped_to: cid,
            },
            armed: true,
        };
        lease.finish()
    }

    fn set_divert(&mut self, enabled: bool, software_id: u8) -> Result<()> {
        // DVALID only. PVALID/RVALID are zero; remap zero means unchanged.
        let [high, low] = self.cid.to_be_bytes();
        let payload = [high, low, 0x02 | u8::from(enabled), 0x00, 0x00];
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
        Reporting::parse(
            &(self.transport)(&Report::request(
                self.index,
                self.feature,
                2,
                software_id,
                &self.cid.to_be_bytes(),
            )?)?,
            self.cid,
        )
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
                "Control {:04x} restoration readback differs: original={:?}, actual={actual:?}; unrelated settings were not overwritten",
                self.cid, self.original
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
            eprintln!(
                "RESTORE FAILED for Spotlight control {:04x}: {error}",
                self.cid
            );
        }
    }
}

/// Leftover diversions persisted between app runs: one `cid flags mapping`
/// line of hex per control. Malformed lines are ignored.
pub fn format_unrestored(items: &[(u16, Reporting)]) -> String {
    items
        .iter()
        .map(|(cid, r)| format!("{cid:04x} {:04x} {:04x}\n", r.flags, r.mapped_to))
        .collect()
}

pub fn parse_unrestored(text: &str) -> Vec<(u16, Reporting)> {
    text.lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace().map(|f| u16::from_str_radix(f, 16));
            match (fields.next(), fields.next(), fields.next(), fields.next()) {
                (Some(Ok(cid)), Some(Ok(flags)), Some(Ok(mapped_to)), None) => {
                    Some((cid, Reporting { flags, mapped_to }))
                }
                _ => None,
            }
        })
        .collect()
}
