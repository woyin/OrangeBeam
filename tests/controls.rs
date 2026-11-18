use orange_beam::controls::{
    active_controls, lease_controls, restore_leftovers, start_record, top_button_state, Reporting,
    Result, TemporaryTopButton, BACK_HOLD, BACK_SHORT, NEXT_HOLD, NEXT_SHORT, SWITCH_HIGHLIGHT,
    TOP_HOLD,
};
use orange_beam::controls::{format_unrestored, notification_address, parse_unrestored};
use orange_beam::presentation::Presentation;
use orange_beam::Report;

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
    /// Control under test; zero means the top hold (0x00d8).
    cid: u16,
}

impl Device {
    fn exchange(&mut self, request: &Report) -> Result<Report> {
        let [high, low] = if self.cid == 0 { TOP_HOLD } else { self.cid }.to_be_bytes();
        let payload = match request.function() {
            0 => vec![2],
            1 => {
                // Position 0 is an unrelated control; the target is at position 1.
                if request.payload()[0] == 0 {
                    vec![0, 0x50, 0, 0x38, 0x34, 0, 0, 0, 0]
                } else {
                    vec![high, low, 0, 0xb7, 0x34, 0, 0, 0, 1]
                }
            }
            2 => {
                let flags = if self.corrupt_readback && self.flags & 1 != 0 {
                    self.flags | 0x10
                } else {
                    self.flags
                };
                vec![
                    high,
                    low,
                    flags as u8,
                    (self.mapped >> 8) as u8,
                    self.mapped as u8,
                    (flags >> 8) as u8,
                ]
            }
            3 => {
                self.writes.push(request.clone());
                let p = request.payload();
                assert_eq!(&p[..2], &[high, low]);
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
    assert!(error.contains("original setting of control 00d8 restored and verified"));
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

#[test]
fn other_divertible_controls_use_their_own_cid_and_restore() {
    for cid in [SWITCH_HIGHLIGHT, NEXT_HOLD] {
        let mut device = Device {
            cid,
            empty_ack: true,
            ..Default::default()
        };
        let lease = TemporaryTopButton::start_control(|r| device.exchange(r), 1, 7, cid).unwrap();
        assert_eq!(lease.control(), cid);
        assert_eq!(lease.original().mapped_to, cid);
        lease.finish().unwrap();
        assert_eq!(device.flags, 0);
        assert_eq!(device.writes.len(), 2);
        // A device that only exposes 0x00df must not be treated as having 0x00d8.
        let mut other = Device {
            cid,
            ..Default::default()
        };
        assert!(TemporaryTopButton::start(|r| other.exchange(r), 1, 7).is_err());
        assert!(other.writes.is_empty());
    }
}

#[test]
fn notifications_report_every_held_control() {
    let mut bytes = [0; 20];
    bytes[..8].copy_from_slice(&[0x11, 0xff, 7, 0, 0x00, 0xd8, 0x00, 0xda]);
    let report = Report::parse(&bytes).unwrap();
    assert_eq!(
        active_controls(&report, 0xff, 7),
        Some(vec![TOP_HOLD, NEXT_HOLD])
    );
    assert_eq!(top_button_state(&report, 0xff, 7), Some(true));
    bytes[4..8].fill(0);
    assert_eq!(
        active_controls(&Report::parse(&bytes).unwrap(), 0xff, 7),
        Some(vec![])
    );
    bytes[3] = 0x10; // event 1 is raw motion, not button state
    assert_eq!(
        active_controls(&Report::parse(&bytes).unwrap(), 0xff, 7),
        None
    );
}

#[test]
fn recorded_bluetooth_gestures_decode_to_each_diverted_control() {
    // docs/qa/bluetooth-gesture-controls.hex: first-generation Spotlight,
    // Bluetooth, macOS 27, with 00d8/00df/00da/00dc temporarily diverted.
    // Sequence: double click, next hold, back hold, top hold (press/release each).
    let frames: Vec<_> = include_str!("../docs/qa/bluetooth-gesture-controls.hex")
        .lines()
        .map(|line| {
            let bytes: Vec<_> = line
                .split_whitespace()
                .map(|s| u8::from_str_radix(s, 16).unwrap())
                .collect();
            Report::parse(&bytes).unwrap()
        })
        .collect();
    let decoded: Vec<_> = frames
        .iter()
        .map(|f| active_controls(f, notification_address(true, 1), 7).unwrap())
        .collect();
    let expected: Vec<Vec<u16>> = vec![
        vec![SWITCH_HIGHLIGHT],
        vec![],
        vec![NEXT_HOLD],
        vec![],
        vec![BACK_HOLD],
        vec![],
        vec![TOP_HOLD],
        vec![],
    ];
    assert_eq!(decoded, expected);
}

#[test]
fn only_this_process_leftover_diversion_is_adopted_after_failed_restore() {
    let previous = Reporting {
        flags: 0,
        mapped_to: TOP_HOLD,
    };
    // Exactly our leftover: adopted without writing, restored on finish.
    let mut device = Device {
        flags: 1,
        empty_ack: true,
        ..Default::default()
    };
    let lease =
        TemporaryTopButton::start_or_adopt(|r| device.exchange(r), 1, 7, TOP_HOLD, Some(previous))
            .unwrap();
    lease.finish().unwrap();
    assert_eq!(
        device.writes.len(),
        1,
        "adoption writes nothing; only restore"
    );
    assert_eq!(device.flags, 0);
    // Without a recorded leftover, or with any other state, nothing is touched.
    for (flags, mapped, leftover) in [
        (1, 0, None),
        (0x11, 0, Some(previous)),
        (1, 0xdf, Some(previous)),
    ] {
        let mut device = Device {
            flags,
            mapped,
            ..Default::default()
        };
        assert!(TemporaryTopButton::start_or_adopt(
            |r| device.exchange(r),
            1,
            7,
            TOP_HOLD,
            leftover
        )
        .is_err());
        assert!(device.writes.is_empty());
        assert_eq!(device.flags, flags);
    }
    // A clean device is configured normally even if a leftover was recorded.
    let mut device = Device::default();
    TemporaryTopButton::start_or_adopt(|r| device.exchange(r), 1, 7, TOP_HOLD, Some(previous))
        .unwrap()
        .finish()
        .unwrap();
    assert_eq!(device.writes.len(), 2);
}

#[test]
fn leftover_records_survive_a_restart_and_reject_garbage() {
    let items = vec![
        (
            TOP_HOLD,
            Reporting {
                flags: 0,
                mapped_to: TOP_HOLD,
            },
        ),
        (
            NEXT_HOLD,
            Reporting {
                flags: 0x0200,
                mapped_to: NEXT_HOLD,
            },
        ),
    ];
    assert_eq!(parse_unrestored(&format_unrestored(&items)), items);
    assert!(parse_unrestored("zz 0 0\n00d8 0000\n00d8 0 0 extra\n").is_empty());
}

// Two short-press page keys on one simulated device: the countdown diverts
// both or neither, and each lease restores its own control.
struct PageKeys {
    flags: [u16; 2],
    divertible: [bool; 2],
    writes: Vec<u16>,
}

const SHORT_KEYS: [u16; 2] = [NEXT_SHORT, BACK_SHORT];

impl PageKeys {
    fn new(flags: [u16; 2], divertible: [bool; 2]) -> std::rc::Rc<std::cell::RefCell<Self>> {
        std::rc::Rc::new(std::cell::RefCell::new(Self {
            flags,
            divertible,
            writes: Vec::new(),
        }))
    }
    fn slot(cid: u16) -> usize {
        usize::from(cid == BACK_SHORT)
    }
    fn exchange(&mut self, request: &Report) -> Result<Report> {
        let payload = match request.function() {
            0 => vec![2],
            1 => {
                let slot = usize::from(request.payload()[0]);
                let [high, low] = SHORT_KEYS[slot].to_be_bytes();
                // Capability byte 4 carries the divert bit; byte 8 is the high half.
                let divert = if self.divertible[slot] { 0x34 } else { 0x14 };
                vec![high, low, 0, 0xb6, divert, 0, 0, 0, 1]
            }
            2 => {
                let cid = u16::from_be_bytes([request.payload()[0], request.payload()[1]]);
                let flags = self.flags[Self::slot(cid)];
                let [high, low] = cid.to_be_bytes();
                vec![high, low, flags as u8, high, low, (flags >> 8) as u8]
            }
            3 => {
                let p = request.payload();
                let cid = u16::from_be_bytes([p[0], p[1]]);
                let slot = Self::slot(cid);
                self.writes.push(cid);
                self.flags[slot] = (self.flags[slot] & !1) | u16::from(p[2] & 1);
                p.to_vec()
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

/// One cloneable transport per device, as the application builds it.
fn shared(
    device: &std::rc::Rc<std::cell::RefCell<PageKeys>>,
) -> impl FnMut(&Report) -> Result<Report> + Clone {
    let device = device.clone();
    move |request: &Report| device.borrow_mut().exchange(request)
}

fn refusal(device: &std::rc::Rc<std::cell::RefCell<PageKeys>>) -> String {
    match lease_controls(shared(device), 1, 7, &SHORT_KEYS, |_| None) {
        Ok(_) => panic!("takeover must be refused"),
        Err(error) => error.to_string(),
    }
}

#[test]
fn both_short_page_keys_take_over_and_restore_their_originals() {
    let device = PageKeys::new([0, 0], [true, true]);
    let leases = lease_controls(shared(&device), 1, 7, &SHORT_KEYS, |_| None).unwrap();
    assert_eq!(device.borrow().writes, vec![NEXT_SHORT, BACK_SHORT]);
    assert_eq!(device.borrow().flags, [1, 1]);
    for lease in leases {
        lease.finish().unwrap();
    }
    assert_eq!(device.borrow().flags, [0, 0], "both read back as default");
    assert_eq!(
        device.borrow().writes,
        vec![NEXT_SHORT, BACK_SHORT, NEXT_SHORT, BACK_SHORT],
        "each key wrote an enable and a restore"
    );
}

#[test]
fn one_refused_short_key_restores_the_one_already_diverted() {
    // The back key is diverted by another client, so the pair must not split.
    let device = PageKeys::new([0, 1], [true, true]);
    let error = refusal(&device);
    assert!(error.contains("00db"), "{error}");
    assert_eq!(device.borrow().flags, [0, 1]);
    assert_eq!(
        device.borrow().writes,
        vec![NEXT_SHORT, NEXT_SHORT],
        "the first key was enabled, then restored, before the error returned"
    );
}

#[test]
fn short_keys_owned_elsewhere_or_unsupported_are_refused_without_writes() {
    // Another client's diversion, and a control the device cannot divert.
    for (flags, divertible) in [([1, 1], [true, true]), ([0, 0], [false, true])] {
        let device = PageKeys::new(flags, divertible);
        assert!(!refusal(&device).is_empty());
        assert!(device.borrow().writes.is_empty(), "{flags:?}");
        assert_eq!(device.borrow().flags, flags);
    }
}

/// The record a session publishes must keep the originals of every control it
/// diverts, and every earlier entry it did not resolve. A record that loses an
/// entry makes the next start treat that control as another client's.
#[test]
fn the_start_record_keeps_earlier_entries_and_this_sessions_leases() {
    let original = |flags: u16| Reporting {
        flags,
        mapped_to: TOP_HOLD,
    };
    let stale = vec![
        (TOP_HOLD, original(0)),
        (SWITCH_HIGHLIGHT, original(0x0200)),
        (NEXT_SHORT, original(0)),
    ];
    let leased = vec![
        (TOP_HOLD, original(0)),
        (NEXT_HOLD, original(0)),
        (BACK_SHORT, original(0)),
    ];
    let record = start_record(&stale, &leased);
    // The leases come last and win; every unrelated leftover survives, so a
    // crash record is never narrowed by starting a new session.
    assert_eq!(
        record,
        vec![
            (SWITCH_HIGHLIGHT, original(0x0200)),
            (NEXT_SHORT, original(0)),
            (TOP_HOLD, original(0)),
            (NEXT_HOLD, original(0)),
            (BACK_SHORT, original(0)),
        ]
    );
    // The page-key takeover appends to this in-memory record. Without the
    // gesture entries above, writing the record back would drop them.
    let mut after_page_keys = record.clone();
    after_page_keys.retain(|(cid, _)| !SHORT_KEYS.contains(cid));
    after_page_keys.extend([(NEXT_SHORT, original(0)), (BACK_SHORT, original(0))]);
    assert!(after_page_keys
        .iter()
        .any(|(cid, _)| *cid == SWITCH_HIGHLIGHT));
    assert_eq!(
        parse_unrestored(&format_unrestored(&after_page_keys)).len(),
        5
    );
}

#[test]
fn a_leftover_short_key_record_is_restored_at_start_and_leaves_the_record() {
    // A forced quit during setup left 00d9 diverted; the restart must restore
    // it at once, without waiting for the next countdown.
    let device = PageKeys::new([1, 0], [true, true]);
    let leftover = vec![
        (
            NEXT_SHORT,
            Reporting {
                flags: 0,
                mapped_to: NEXT_SHORT,
            },
        ),
        (
            TOP_HOLD,
            Reporting {
                flags: 0,
                mapped_to: TOP_HOLD,
            },
        ),
    ];
    let restored = restore_leftovers(shared(&device), 1, 7, &SHORT_KEYS, |cid| {
        leftover
            .iter()
            .find(|(control, _)| *control == cid)
            .map(|(_, original)| *original)
    });
    assert_eq!(restored, vec![NEXT_SHORT]);
    assert_eq!(device.borrow().flags, [0, 0], "page keys answer again");
    assert_eq!(
        device.borrow().writes,
        vec![NEXT_SHORT],
        "adoption writes nothing; only the restore does"
    );
    // The restored control leaves the record; unrelated entries stay.
    let mut record = leftover.clone();
    record.retain(|(cid, _)| !restored.contains(cid));
    assert_eq!(record.len(), 1);
    assert_eq!(record[0].0, TOP_HOLD);
    // A record the device no longer matches is not touched.
    let diverted_elsewhere = PageKeys::new([1, 0], [true, true]);
    assert!(
        restore_leftovers(shared(&diverted_elsewhere), 1, 7, &SHORT_KEYS, |cid| (cid
            == NEXT_SHORT)
            .then_some(Reporting {
                flags: 0x0200,
                mapped_to: NEXT_SHORT
            }))
        .is_empty()
    );
    assert!(diverted_elsewhere.borrow().writes.is_empty());
    assert_eq!(diverted_elsewhere.borrow().flags, [1, 0]);
}

#[test]
fn explicit_recovery_applies_to_each_gesture_control() {
    for cid in [SWITCH_HIGHLIGHT, NEXT_HOLD, BACK_HOLD] {
        let mut device = Device {
            cid,
            flags: 1,
            empty_ack: true,
            ..Default::default()
        };
        TemporaryTopButton::recover_default_control(|r| device.exchange(r), 1, 7, cid).unwrap();
        assert_eq!(device.flags, 0);
        let mut remapped = Device {
            cid,
            flags: 1,
            mapped: TOP_HOLD,
            ..Default::default()
        };
        assert!(
            TemporaryTopButton::recover_default_control(|r| remapped.exchange(r), 1, 7, cid)
                .is_err()
        );
        assert!(remapped.writes.is_empty());
    }
}
