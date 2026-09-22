use spotlight_rs::controls::notification_address;
use spotlight_rs::controls::{top_button_state, Result, TemporaryTopButton};
use spotlight_rs::presentation::Presentation;
use spotlight_rs::Report;

// A simulated device applies writes before replying, as real hardware can do
// when an acknowledgement is lost. These are synthetic, not captured packets.
#[derive(Default)]
struct Device {
    flags: u16,
    mapped: u16,
    writes: Vec<Report>,
    lose_enable_ack: bool,
    reject_restore: bool,
    corrupt_readback: bool,
    empty_ack: bool,
}

impl Device {
    fn exchange(&mut self, request: &Report) -> Result<Report> {
        let payload = match request.function() {
            0 => vec![1],
            1 => vec![0, 0xd8, 0, 0xb7, 0x34, 0, 0, 0, 1],
            2 => {
                let flags = if self.corrupt_readback && self.flags & 1 != 0 {
                    self.flags | 0x10
                } else {
                    self.flags
                };
                vec![
                    0,
                    0xd8,
                    flags as u8,
                    (self.mapped >> 8) as u8,
                    self.mapped as u8,
                    (flags >> 8) as u8,
                ]
            }
            3 => {
                self.writes.push(request.clone());
                let p = request.payload();
                assert_eq!(&p[..2], &[0, 0xd8]);
                assert_eq!(p[2] & !1, 2, "only DVALID may be set");
                assert_eq!(&p[3..5], &[0, 0], "mapping must stay unchanged");
                let enabling = p[2] & 1 != 0;
                if !enabling && self.reject_restore {
                    return Err("disconnected during restoration".into());
                }
                self.flags = (self.flags & !1) | u16::from(enabling);
                if enabling && self.lose_enable_ack {
                    return Err("enable acknowledgement lost".into());
                }
                if self.empty_ack {
                    vec![0; 16]
                } else {
                    p.to_vec()
                }
            }
            _ => panic!("unexpected function"),
        };
        Ok(Report::request(
            request.device(),
            request.feature(),
            request.function(),
            request.software_id(),
            &payload,
        )?)
    }
}

#[test]
fn restores_original_mapping_and_unrelated_flags_on_normal_finish() {
    let mut device = Device {
        flags: 0x0200,
        mapped: 0x00df,
        ..Default::default()
    };
    let lease = TemporaryTopButton::start(|r| device.exchange(r), 1, 7).unwrap();
    assert_eq!(lease.original().flags, 0x0200);
    lease.finish().unwrap();
    assert_eq!(device.flags, 0x0200);
    assert_eq!(device.mapped, 0x00df);
    assert_eq!(device.writes.len(), 2);
    assert_ne!(
        device.writes[0].software_id(),
        device.writes[1].software_id()
    );
}

#[test]
fn lost_enable_ack_still_restores_applied_configuration() {
    let mut device = Device {
        lose_enable_ack: true,
        ..Default::default()
    };
    let error = match TemporaryTopButton::start(|r| device.exchange(r), 1, 7) {
        Ok(_) => panic!("lost acknowledgement must fail initialization"),
        Err(e) => e.to_string(),
    };
    assert!(error.contains("original top setting restored and verified"));
    assert_eq!(device.flags, 0);
    assert_eq!(device.writes.len(), 2);
}

#[test]
fn empty_firmware_ack_requires_successful_readback() {
    let mut device = Device {
        empty_ack: true,
        ..Default::default()
    };
    TemporaryTopButton::start(|r| device.exchange(r), 1, 7)
        .unwrap()
        .finish()
        .unwrap();
    assert_eq!(device.flags, 0);
    device.corrupt_readback = true;
    assert!(TemporaryTopButton::start(|r| device.exchange(r), 1, 7).is_err());
    assert_eq!(device.flags, 0);
}

#[test]
fn readback_mismatch_rolls_back_instead_of_claiming_ready() {
    let mut device = Device {
        corrupt_readback: true,
        ..Default::default()
    };
    assert!(TemporaryTopButton::start(|r| device.exchange(r), 1, 7).is_err());
    assert_eq!(device.flags, 0);
    assert_eq!(device.writes.len(), 2);
}

#[test]
fn unwind_restores_configuration() {
    let mut device = Device::default();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _lease = TemporaryTopButton::start(|r| device.exchange(r), 1, 7).unwrap();
        panic!("simulated capture panic");
    }));
    assert!(result.is_err());
    assert_eq!(device.flags, 0);
    assert_eq!(device.writes.len(), 2);
}

#[test]
fn restoration_failure_is_observable() {
    let mut device = Device {
        reject_restore: true,
        ..Default::default()
    };
    let lease = TemporaryTopButton::start(|r| device.exchange(r), 1, 7).unwrap();
    assert!(lease
        .finish()
        .unwrap_err()
        .to_string()
        .contains("disconnected"));
    assert_eq!(device.flags, 1);
    assert_eq!(device.writes.len(), 2);
}

#[test]
fn preexisting_diversion_or_raw_xy_is_never_overwritten() {
    for flags in [1, 4, 0x10, 0x11] {
        let mut device = Device {
            flags,
            ..Default::default()
        };
        assert!(TemporaryTopButton::start(|r| device.exchange(r), 1, 7).is_err());
        assert_eq!(device.flags, flags);
        assert!(device.writes.is_empty());
    }
}

#[test]
fn explicit_recovery_only_accepts_interrupted_default_state() {
    let mut device = Device {
        flags: 1,
        empty_ack: true,
        ..Default::default()
    };
    TemporaryTopButton::recover_default(|r| device.exchange(r), 1, 7).unwrap();
    assert_eq!(device.flags, 0);
    assert_eq!(device.writes.len(), 1);
    TemporaryTopButton::recover_default(|r| device.exchange(r), 1, 7).unwrap();
    assert_eq!(device.writes.len(), 1, "already restored: no extra write");
    for (flags, mapped) in [(0x11, 0), (5, 0), (1, 0xdf)] {
        let mut device = Device {
            flags,
            mapped,
            ..Default::default()
        };
        assert!(TemporaryTopButton::recover_default(|r| device.exchange(r), 1, 7).is_err());
        assert!(device.writes.is_empty());
    }
}

#[test]
fn only_explicit_button_notifications_update_hold_state() {
    let mut bytes = [0; 20];
    bytes[..4].copy_from_slice(&[0x11, 1, 7, 0]);
    assert_eq!(
        top_button_state(&Report::parse(&bytes).unwrap(), 1, 7),
        Some(false)
    );
    // The position can move when another diverted button is released.
    for position in 0..4 {
        let mut pressed = bytes;
        pressed[5 + position * 2] = 0xd8;
        assert_eq!(
            top_button_state(&Report::parse(&pressed).unwrap(), 1, 7),
            Some(true)
        );
    }
    bytes[5] = 0xd9;
    assert_eq!(
        top_button_state(&Report::parse(&bytes).unwrap(), 1, 7),
        Some(false)
    );
    for (offset, value) in [(1, 2), (2, 8), (3, 0x0a), (3, 0x10)] {
        let mut unrelated = bytes;
        unrelated[offset] = value;
        assert_eq!(
            top_button_state(&Report::parse(&unrelated).unwrap(), 1, 7),
            None
        );
    }
    assert_eq!(
        top_button_state(&Report::parse(&[0x10, 1, 7, 0, 0, 0, 0]).unwrap(), 1, 7),
        None
    );
    assert!(Report::parse(&[2, 0, 0, 0, 0, 0, 0, 0]).is_err());
}

#[test]
fn recorded_bluetooth_hold_persists_through_silence_until_explicit_release() {
    let frames: Vec<_> = include_str!("../docs/qa/bluetooth-top-diverted-hold.hex")
        .lines()
        .map(|line| {
            let bytes: Vec<_> = line
                .split_whitespace()
                .map(|s| u8::from_str_radix(s, 16).unwrap())
                .collect();
            Report::parse(&bytes).unwrap()
        })
        .collect();
    assert_eq!(frames.len(), 2);
    let address = notification_address(true, 1);
    assert_eq!(address, 0xff);
    let mut state = Presentation::default();
    state.set_remote_held(top_button_state(&frames[0], address, 7).unwrap());
    assert!(state.active(49.183));
    assert!(
        state.active(73.736),
        "hold must survive over ten seconds of no movement"
    );
    state.set_remote_held(top_button_state(&frames[1], address, 7).unwrap());
    assert!(!state.active(73.737));
    assert_eq!(
        top_button_state(&frames[0], notification_address(false, 1), 7),
        None
    );
    assert_eq!(top_button_state(&frames[0], address, 8), None);
}

#[test]
fn emergency_hide_latches_until_release_and_disconnect_preserves_manual_mode() {
    let mut state = Presentation::default();
    state.set_remote_held(true);
    state.hide();
    state.set_remote_held(true); // duplicate notification must not undo emergency hide
    assert!(!state.active(100.0));
    state.set_remote_held(false);
    state.set_remote_held(true);
    assert!(state.active(101.0));
    state.set_remote_held(false);
    assert!(!state.active(101.0));
    state.toggle(102.0);
    state.set_remote_held(true);
    state.set_remote_held(false);
    assert!(
        state.active(103.0),
        "release/disconnect must not cancel a manual activation"
    );
}
