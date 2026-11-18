//! Read-only queries, bounded input captures and temporary control diagnostics.
use crate::{ensure_input_available, resolve_feature, transact, Result};
use hidapi::{DeviceInfo, HidDevice};
use orange_beam::controls::{
    active_controls, lease_controls, top_button_state, TemporaryTopButton, BACK_HOLD, BACK_SHORT,
    NEXT_HOLD, NEXT_SHORT, SWITCH_HIGHLIGHT, TOP_HOLD,
};
use orange_beam::{feature_index, feature_request, Report, SOFTWARE_ID};
use std::time::{Duration, Instant};

pub fn list(devices: &[&DeviceInfo]) {
    if devices.is_empty() {
        println!("No first-generation Spotlight interfaces found (046d:c53e / 046d:b503).");
    }
    for (number, d) in devices.iter().enumerate() {
        println!(
            "{number}: {:04x}:{:04x} usage={:04x}:{:04x} interface={} bus={:?} {}",
            d.vendor_id(),
            d.product_id(),
            d.usage_page(),
            d.usage(),
            d.interface_number(),
            d.bus_type(),
            d.product_string().unwrap_or("Spotlight")
        );
    }
}

pub fn probe(device: &HidDevice, index: u8) -> Result<()> {
    for (code, name) in [
        (0x0001, "FEATURE_SET"),
        (0x1000, "BATTERY_STATUS"),
        (0x1004, "UNIFIED_BATTERY"),
        (0x1a00, "PRESENTER_CONTROL"),
        (0x1a01, "SENSOR_3D"),
        (0x1b04, "REPROG_CONTROLS_V4"),
        (0x2205, "POINTER_SPEED"),
    ] {
        let request = feature_request(index, code);
        let reply = transact(device, &request)?;
        match feature_index(&reply, &request)? {
            Some(feature) => println!("{code:04x} {name:22} index={feature:02x}"),
            None => println!("{code:04x} {name:22} unavailable"),
        }
    }
    Ok(())
}

pub fn capture(device: &HidDevice, seconds: u64) -> Result<()> {
    let start = Instant::now();
    let duration = Duration::from_secs(seconds);
    let mut buffer = [0; 64];
    let mut reports = 0;
    eprintln!("Reading this Spotlight interface for {seconds}s. Press/move/release its buttons.");
    while start.elapsed() < duration {
        ensure_input_available()?;
        let timeout = duration
            .saturating_sub(start.elapsed())
            .as_millis()
            .clamp(1, 100) as i32;
        let count = device.read_timeout(&mut buffer, timeout)?;
        if count == 0 {
            continue;
        }
        ensure_input_available()?;
        reports += 1;
        print!("{:8}ms", start.elapsed().as_millis());
        for byte in &buffer[..count] {
            print!(" {byte:02x}");
        }
        println!();
    }
    eprintln!("Captured {reports} reports. No reports may mean the wrong interface, a sleeping device, or missing input permissions.");
    Ok(())
}

#[cfg(unix)]
pub fn watch_top(
    device: &HidDevice,
    index: u8,
    notification_index: u8,
    seconds: u64,
) -> Result<()> {
    use std::io::Write;
    let stop = crate::stop_signals::StopSignals::install()?;
    let feature =
        resolve_feature(device, index, 0x1b04)?.ok_or("REPROG_CONTROLS_V4 is unavailable")?;
    if stop.received() != 0 {
        return Err("Stopped before configuring the top button".into());
    }
    let lease = TemporaryTopButton::start(|request| transact(device, request), index, feature)?;
    let original = lease.original();
    eprintln!("READY top-hold notifications enabled; index={index} feature={feature:02x} original_flags={:04x} original_mapping={:04x}. Hold/move/keep still/release the top button within {seconds}s.", original.flags, original.mapped_to);
    let captured: Result<()> = (|| {
        let start = Instant::now();
        let deadline = start + Duration::from_secs(seconds);
        let mut buffer = [0; 64];
        let mut pressed = false;
        let mut reports = 0;
        let stdout = std::io::stdout();
        let mut output = stdout.lock();
        while Instant::now() < deadline && stop.received() == 0 {
            ensure_input_available()?;
            let count = device.read_timeout(&mut buffer, 100)?;
            if count == 0 {
                continue;
            }
            ensure_input_available()?;
            reports += 1;
            write!(output, "{:8}ms", start.elapsed().as_millis())?;
            for byte in &buffer[..count] {
                write!(output, " {byte:02x}")?;
            }
            writeln!(output)?;
            output.flush()?;
            if let Some(down) = Report::parse(&buffer[..count])
                .ok()
                .and_then(|r| top_button_state(&r, notification_index, feature))
            {
                eprintln!(
                    "{:8}ms TOP {}",
                    start.elapsed().as_millis(),
                    if down { "pressed" } else { "released" }
                );
                if pressed && !down {
                    break;
                }
                pressed |= down;
            }
        }
        eprintln!(
            "Captured {reports} reports; signal={}. Restoring top setting.",
            stop.received()
        );
        Ok(())
    })();
    let restored = lease.finish();
    match (captured, restored) {
        (Ok(()), Ok(())) => {
            eprintln!(
                "RESTORED and verified top flags={:04x} mapped_to={:04x}",
                original.flags, original.mapped_to
            );
            Ok(())
        }
        (Err(error), Ok(())) => Err(format!("{error}; top setting restored and verified").into()),
        (Ok(()), Err(error)) => Err(format!("RESTORE FAILED: {error}").into()),
        (Err(error), Err(restore)) => Err(format!("{error}; RESTORE FAILED: {restore}").into()),
    }
}

#[cfg(not(unix))]
pub fn watch_top(
    _device: &HidDevice,
    _index: u8,
    _notification_index: u8,
    _seconds: u64,
) -> Result<()> {
    Err("watch-top currently requires Unix signal handling".into())
}

fn control_name(cid: u16) -> &'static str {
    match cid {
        TOP_HOLD => "top-hold",
        SWITCH_HIGHLIGHT => "double-click",
        NEXT_HOLD => "next-hold",
        BACK_HOLD => "back-hold",
        0x0050 => "top-click",
        NEXT_SHORT => "next-short",
        BACK_SHORT => "back-short",
        _ => "unknown",
    }
}

/// Diagnostic only: diverts each gesture control separately, so the app keeps
/// native cursor motion, clicks and short-press page turns. Every lease is
/// restored in reverse order and verified by readback, as in watch-top.
#[cfg(unix)]
pub fn watch_controls(
    device: &HidDevice,
    index: u8,
    notification_index: u8,
    seconds: u64,
) -> Result<()> {
    use std::io::Write;
    let stop = crate::stop_signals::StopSignals::install()?;
    let feature =
        resolve_feature(device, index, 0x1b04)?.ok_or("REPROG_CONTROLS_V4 is unavailable")?;
    let mut leases = Vec::new();
    let mut started: Result<()> = Ok(());
    for cid in [TOP_HOLD, SWITCH_HIGHLIGHT, NEXT_HOLD, BACK_HOLD] {
        if stop.received() != 0 {
            started = Err("Stopped before configuring every control".into());
            break;
        }
        match TemporaryTopButton::start_control(|r| transact(device, r), index, feature, cid) {
            Ok(lease) => {
                let o = lease.original();
                eprintln!(
                    "DIVERTED {cid:04x} {} original_flags={:04x} original_mapping={:04x}",
                    control_name(cid),
                    o.flags,
                    o.mapped_to
                );
                leases.push(lease);
            }
            Err(error) => {
                started = Err(format!("{cid:04x}: {error}").into());
                break;
            }
        }
    }
    let captured: Result<()> = started.and_then(|()| {
        eprintln!("READY notifications={notification_index:02x} feature={feature:02x}. For {seconds}s: double-click top, hold next, hold back, short-press next/back, hold top. Ctrl-C ends early.");
        let start = Instant::now();
        let deadline = start + Duration::from_secs(seconds);
        let mut buffer = [0; 64];
        let mut held: Vec<u16> = Vec::new();
        let mut mouse_buttons: Option<Vec<u8>> = None;
        let mut motion = 0u64;
        let mut reports = 0u64;
        let stdout = std::io::stdout();
        let mut output = stdout.lock();
        while Instant::now() < deadline && stop.received() == 0 {
            ensure_input_available()?;
            let count = device.read_timeout(&mut buffer, 100)?;
            if count == 0 {
                continue;
            }
            ensure_input_available()?;
            reports += 1;
            let ms = start.elapsed().as_millis();
            let data = &buffer[..count];
            write!(output, "{ms:8}ms")?;
            for byte in data {
                write!(output, " {byte:02x}")?;
            }
            writeln!(output)?;
            output.flush()?;
            match data[0] {
                // Keyboard: modifier, then up to six usage codes.
                0x01 => {
                    let keys: Vec<_> = data[2.min(count)..]
                        .iter()
                        .filter(|k| **k != 0)
                        .map(|k| match k {
                            0x4f => "RightArrow".to_string(),
                            0x50 => "LeftArrow".to_string(),
                            other => format!("key-{other:02x}"),
                        })
                        .collect();
                    eprintln!("{ms:8}ms KEYBOARD {}", if keys.is_empty() { "released".into() } else { keys.join("+") });
                }
                // Mouse: button mask precedes motion; log button changes only.
                0x02 => {
                    motion += 1;
                    let buttons = data[1..3.min(count)].to_vec();
                    if mouse_buttons.as_ref() != Some(&buttons) {
                        eprintln!("{ms:8}ms MOUSE buttons={}", buttons.iter().map(|b| format!("{b:02x}")).collect::<String>());
                        mouse_buttons = Some(buttons);
                    }
                }
                0x10 | 0x11 => {
                    let Ok(report) = Report::parse(data) else { continue };
                    if let Some(now) = active_controls(&report, notification_index, feature) {
                        for cid in now.iter().filter(|c| !held.contains(c)) {
                            eprintln!("{ms:8}ms PRESS   {cid:04x} {}", control_name(*cid));
                        }
                        for cid in held.iter().filter(|c| !now.contains(c)) {
                            eprintln!("{ms:8}ms RELEASE {cid:04x} {}", control_name(*cid));
                        }
                        held = now;
                    } else {
                        eprintln!("{ms:8}ms HID++ other (device={:02x} feature={:02x} function={} swid={})", report.device(), report.feature(), report.function(), report.software_id());
                    }
                }
                other => eprintln!("{ms:8}ms REPORT id={other:02x} len={count}"),
            }
        }
        eprintln!(
            "Captured {reports} reports ({motion} mouse); signal={}. Restoring controls.",
            stop.received()
        );
        Ok(())
    });
    let mut restore_errors = Vec::new();
    while let Some(lease) = leases.pop() {
        let cid = lease.control();
        match lease.finish() {
            Ok(()) => eprintln!("RESTORED and verified {cid:04x}"),
            Err(error) => restore_errors.push(format!("{cid:04x}: {error}")),
        }
    }
    match (captured, restore_errors.is_empty()) {
        (Ok(()), true) => Ok(()),
        (Err(error), true) => {
            Err(format!("{error}; diverted controls restored and verified").into())
        }
        (Ok(()), false) => Err(format!("RESTORE FAILED: {}", restore_errors.join("; ")).into()),
        (Err(error), false) => {
            Err(format!("{error}; RESTORE FAILED: {}", restore_errors.join("; ")).into())
        }
    }
}

#[cfg(not(unix))]
pub fn watch_controls(_: &HidDevice, _: u8, _: u8, _: u64) -> Result<()> {
    Err("watch-controls currently requires Unix signal handling".into())
}

/// Diagnostic for the countdown takeover: diverts only the two short page keys,
/// so a native page turn and a takeover notification are distinguishable. Every
/// lease is restored and verified, as in watch-controls.
#[cfg(unix)]
pub fn watch_page_keys(
    device: &HidDevice,
    index: u8,
    notification_index: u8,
    seconds: u64,
) -> Result<()> {
    use std::io::Write;
    let stop = crate::stop_signals::StopSignals::install()?;
    let feature =
        resolve_feature(device, index, 0x1b04)?.ok_or("REPROG_CONTROLS_V4 is unavailable")?;
    if stop.received() != 0 {
        return Err("Stopped before configuring the page keys".into());
    }
    // V7 needs the takeover duration, so the clock starts before the first
    // transaction rather than after the last one.
    let takeover_started = Instant::now();
    let mut leases = lease_controls(
        |request: &Report| transact(device, request),
        index,
        feature,
        &[NEXT_SHORT, BACK_SHORT],
        |_| None,
    )?;
    let takeover_ms = takeover_started.elapsed().as_millis();
    eprintln!("TAKEOVER ms={takeover_ms}");
    for lease in &leases {
        let original = lease.original();
        eprintln!(
            "DIVERTED {:04x} {} original_flags={:04x} original_mapping={:04x}",
            lease.control(),
            control_name(lease.control()),
            original.flags,
            original.mapped_to
        );
    }
    let captured: Result<()> = (|| {
        eprintln!("READY notifications={notification_index:02x} feature={feature:02x}. For {seconds}s: short-press next and back. A KEYBOARD line means the device still sends arrow keys. Ctrl-C ends early.");
        let start = Instant::now();
        let deadline = start + Duration::from_secs(seconds);
        let mut buffer = [0; 64];
        let mut held: Vec<u16> = Vec::new();
        let mut reports = 0u64;
        let stdout = std::io::stdout();
        let mut output = stdout.lock();
        while Instant::now() < deadline && stop.received() == 0 {
            ensure_input_available()?;
            let count = device.read_timeout(&mut buffer, 100)?;
            if count == 0 {
                continue;
            }
            ensure_input_available()?;
            reports += 1;
            let ms = start.elapsed().as_millis();
            let data = &buffer[..count];
            write!(output, "{ms:8}ms")?;
            for byte in data {
                write!(output, " {byte:02x}")?;
            }
            writeln!(output)?;
            output.flush()?;
            match data[0] {
                // Keyboard page turn: the diversion did not suppress the arrow key.
                0x01 => {
                    let keys: Vec<_> = data[2.min(count)..]
                        .iter()
                        .filter(|k| **k != 0)
                        .map(|k| match k {
                            0x4f => "RightArrow".to_string(),
                            0x50 => "LeftArrow".to_string(),
                            other => format!("key-{other:02x}"),
                        })
                        .collect();
                    eprintln!(
                        "{ms:8}ms KEYBOARD {}",
                        if keys.is_empty() {
                            "released".into()
                        } else {
                            keys.join("+")
                        }
                    );
                }
                0x10 | 0x11 => {
                    let Ok(report) = Report::parse(data) else {
                        continue;
                    };
                    let Some(now) = active_controls(&report, notification_index, feature) else {
                        eprintln!("{ms:8}ms HID++ other (device={:02x} feature={:02x} function={} swid={})", report.device(), report.feature(), report.function(), report.software_id());
                        continue;
                    };
                    for cid in now.iter().filter(|c| !held.contains(c)) {
                        eprintln!("{ms:8}ms PRESS   {cid:04x} {}", control_name(*cid));
                    }
                    for cid in held.iter().filter(|c| !now.contains(c)) {
                        eprintln!("{ms:8}ms RELEASE {cid:04x} {}", control_name(*cid));
                    }
                    held = now;
                }
                other => eprintln!("{ms:8}ms REPORT id={other:02x} len={count}"),
            }
        }
        eprintln!(
            "Captured {reports} reports; signal={}. Restoring page keys.",
            stop.received()
        );
        Ok(())
    })();
    let release_started = Instant::now();
    let mut restore_errors = Vec::new();
    while let Some(lease) = leases.pop() {
        let cid = lease.control();
        match lease.finish() {
            Ok(()) => eprintln!("RESTORED and verified {cid:04x}"),
            Err(error) => restore_errors.push(format!("{cid:04x}: {error}")),
        }
    }
    eprintln!("RELEASE ms={}", release_started.elapsed().as_millis());
    match (captured, restore_errors.is_empty()) {
        (Ok(()), true) => Ok(()),
        (Err(error), true) => Err(format!("{error}; page keys restored and verified").into()),
        (Ok(()), false) => Err(format!("RESTORE FAILED: {}", restore_errors.join("; ")).into()),
        (Err(error), false) => {
            Err(format!("{error}; RESTORE FAILED: {}", restore_errors.join("; ")).into())
        }
    }
}

#[cfg(not(unix))]
pub fn watch_page_keys(_: &HidDevice, _: u8, _: u8, _: u64) -> Result<()> {
    Err("watch-page-keys currently requires Unix signal handling".into())
}

/// PRESENTER_CONTROL (0x1a00) function 1 {length, 0xe8, intensity}, as used
/// by Projecteur for the first-generation Spotlight. Not a configuration write.
pub fn vibrate(device: &HidDevice, index: u8, intensity: u8, length: u8) -> Result<()> {
    let feature =
        resolve_feature(device, index, 0x1a00)?.ok_or("PRESENTER_CONTROL is unavailable")?;
    let reply = transact(
        device,
        &Report::request(index, feature, 1, SOFTWARE_ID, &[length, 0xe8, intensity])?,
    )?;
    println!(
        "VIBRATE sent feature={feature:02x} intensity={intensity} length={length}; reply payload={}",
        reply.payload().iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(" ")
    );
    Ok(())
}

pub fn inspect(device: &HidDevice, index: u8) -> Result<()> {
    if let Some(feature) = resolve_feature(device, index, 0x1000)? {
        let reply = transact(
            device,
            &Report::request(index, feature, 0, SOFTWARE_ID, &[])?,
        )?;
        let p = reply.payload();
        println!(
            "BATTERY_STATUS current={} next={} status=0x{:02x}",
            p[0], p[1], p[2]
        );
    } else {
        println!("BATTERY_STATUS unavailable");
    }
    let Some(feature) = resolve_feature(device, index, 0x1b04)? else {
        println!("REPROG_CONTROLS_V4 unavailable");
        return Ok(());
    };
    let count = transact(
        device,
        &Report::request(index, feature, 0, SOFTWARE_ID, &[])?,
    )?
    .payload()[0];
    if count > 32 {
        return Err(
            "Unexpected control count for a first-generation Spotlight (maximum 32)".into(),
        );
    }
    println!("REPROG_CONTROLS_V4 controls={count}");
    for position in 0..count {
        let info = transact(
            device,
            &Report::request(index, feature, 1, SOFTWARE_ID, &[position])?,
        )?;
        let p = info.payload();
        if p.len() < 9 {
            return Err("Truncated getControlInfo response".into());
        }
        let cid = u16::from_be_bytes([p[0], p[1]]);
        let task = u16::from_be_bytes([p[2], p[3]]);
        let capabilities = u16::from_le_bytes([p[4], p[8]]);
        let reporting = transact(
            device,
            &Report::request(index, feature, 2, SOFTWARE_ID, &cid.to_be_bytes())?,
        )?;
        let q = reporting.payload();
        if q.len() < 6 || q[..2] != cid.to_be_bytes() {
            return Err("Truncated or mismatched getCidReporting response".into());
        }
        let flags = u16::from_le_bytes([q[2], q[5]]);
        let mapped = match u16::from_be_bytes([q[3], q[4]]) {
            0 => cid,
            value => value,
        };
        println!("control={position} cid={cid:04x} task={task:04x} capabilities={capabilities:04x} reporting={flags:04x} mapped_to={mapped:04x}");
    }
    Ok(())
}

pub fn recover_controls(device: &HidDevice, index: u8) -> Result<()> {
    let feature =
        resolve_feature(device, index, 0x1b04)?.ok_or("REPROG_CONTROLS_V4 unavailable")?;
    let mut failures = Vec::new();
    for cid in [
        TOP_HOLD,
        SWITCH_HIGHLIGHT,
        NEXT_HOLD,
        BACK_HOLD,
        NEXT_SHORT,
        BACK_SHORT,
    ] {
        match TemporaryTopButton::recover_default_control(
            |r| transact(device, r),
            index,
            feature,
            cid,
        ) {
            Ok(()) => {
                println!("RECOVERED and verified {cid:04x} flags=0000 mapped_to={cid:04x}")
            }
            Err(error) => failures.push(format!("{cid:04x}: {error}")),
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; ").into())
    }
}

pub fn recover_top(device: &HidDevice, index: u8) -> Result<()> {
    let feature =
        resolve_feature(device, index, 0x1b04)?.ok_or("REPROG_CONTROLS_V4 unavailable")?;
    TemporaryTopButton::recover_default(|r| transact(device, r), index, feature)?;
    println!("RECOVERED and verified top flags=0000 mapped_to=00d8");
    Ok(())
}
