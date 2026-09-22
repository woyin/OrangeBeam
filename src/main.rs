use hidapi::{DeviceInfo, HidApi, HidDevice};
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
mod macos;
#[cfg(unix)]
mod stop_signals;
use spotlight_rs::controls::{notification_address, top_button_state, TemporaryTopButton};
use spotlight_rs::presentation::Effect;
use spotlight_rs::{
    feature_index, feature_request, is_spotlight, Report, ResponseKind, SOFTWARE_ID,
};
use std::{
    error::Error,
    time::{Duration, Instant},
};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

const HELP: &str = "spotlight-rs — native macOS Spotlight companion

USAGE:
  spotlight-rs app
  spotlight-rs demo [spotlight|laser] [SECONDS]
  spotlight-rs render-fixtures DIRECTORY
  spotlight-rs list
  spotlight-rs probe DEVICE [INDEX]
  spotlight-rs inspect DEVICE [INDEX]
  spotlight-rs capture DEVICE [SECONDS]
  spotlight-rs watch-top DEVICE [SECONDS] [INDEX]
  spotlight-rs recover-top-default DEVICE [INDEX]

DEVICE is an interface number from 'list', not a receiver slot.
INDEX is the HID++ receiver slot (default 1, allowed 1..6).
SECONDS defaults to 15 (allowed 1..300).

list: enumerate only, without opening devices.
probe: query feature availability; does not configure or remap the device.
inspect: read battery and control mappings; does not change device settings.
capture: read raw input from one Spotlight interface; does not write reports.
watch-top: temporarily enable top-hold notifications, then restore and verify.
It ends after one press/release cycle, a signal, or SECONDS (default 30).
Raw XY, persistent settings, mappings and other controls are not modified.
recover-top-default: recover an interrupted session only when its saved original
top flags were 0000, mapping 00d8, and other device-control apps are stopped.
On macOS, unlock the desktop and leave password fields before diagnostics.
Secure Input blocks diagnostics; capture stops if it becomes active.
Interface numbers can change after reconnecting. Run list again first.
For probe choose a vendor usage page (usually ff00); for capture also try
the mouse/keyboard interface. Spotlight 2 is not supported in this prototype.
";

#[derive(Debug, PartialEq, Eq)]
enum Command {
    App,
    RenderQa {
        directory: std::path::PathBuf,
    },
    Demo {
        effect: Effect,
        seconds: u64,
    },
    Help,
    List,
    Probe {
        device: usize,
        index: u8,
    },
    Inspect {
        device: usize,
        index: u8,
    },
    RecoverTop {
        device: usize,
        index: u8,
    },
    Capture {
        device: usize,
        seconds: u64,
    },
    WatchTop {
        device: usize,
        seconds: u64,
        index: u8,
    },
}

fn parse_args(args: &[String]) -> Result<Command> {
    match args.first().map(String::as_str) {
        None => Ok(Command::App),
        Some("app") if args.len() == 1 => Ok(Command::App),
        Some("render-fixtures") if args.len() == 2 => Ok(Command::RenderQa {
            directory: args[1].clone().into(),
        }),
        Some("demo") if args.len() <= 3 => {
            let effect = match args.get(1).map(String::as_str).unwrap_or("spotlight") {
                "spotlight" => Effect::Spotlight,
                "laser" => Effect::Laser,
                _ => return Err("Demo effect must be spotlight or laser".into()),
            };
            let seconds = args
                .get(2)
                .map(|s| s.parse::<u64>())
                .transpose()?
                .unwrap_or(10);
            if !(1..=300).contains(&seconds) {
                return Err("SECONDS must be 1..300".into());
            }
            Ok(Command::Demo { effect, seconds })
        }
        Some("--help" | "-h" | "help") if args.len() == 1 => Ok(Command::Help),
        Some("list") if args.len() == 1 => Ok(Command::List),
        Some("probe" | "inspect" | "recover-top-default") if (2..=3).contains(&args.len()) => {
            let device = args[1].parse()?;
            let index = args
                .get(2)
                .map(|s| s.parse::<u8>())
                .transpose()?
                .unwrap_or(1);
            if !(1..=6).contains(&index) {
                return Err("INDEX must be 1..6".into());
            }
            Ok(if args[0] == "recover-top-default" {
                Command::RecoverTop { device, index }
            } else if args[0] == "inspect" {
                Command::Inspect { device, index }
            } else {
                Command::Probe { device, index }
            })
        }
        Some("capture" | "watch-top")
            if (2..=if args[0] == "watch-top" { 4 } else { 3 }).contains(&args.len()) =>
        {
            let device = args[1].parse()?;
            let seconds = args
                .get(2)
                .map(|s| s.parse::<u64>())
                .transpose()?
                .unwrap_or(if args[0] == "watch-top" { 30 } else { 15 });
            if !(1..=300).contains(&seconds) {
                return Err("SECONDS must be 1..300".into());
            }
            if args[0] == "watch-top" {
                let index = args
                    .get(3)
                    .map(|s| s.parse::<u8>())
                    .transpose()?
                    .unwrap_or(1);
                if !(1..=6).contains(&index) {
                    return Err("INDEX must be 1..6".into());
                }
                Ok(Command::WatchTop {
                    device,
                    seconds,
                    index,
                })
            } else {
                Ok(Command::Capture { device, seconds })
            }
        }
        _ => Err("Invalid arguments. Run spotlight-rs --help".into()),
    }
}

fn interfaces(api: &HidApi) -> Vec<&DeviceInfo> {
    let mut devices: Vec<_> = api
        .device_list()
        .filter(|d| is_spotlight(d.vendor_id(), d.product_id()))
        .collect();
    devices.sort_by(|a, b| {
        a.path()
            .to_bytes()
            .cmp(b.path().to_bytes())
            .then(a.usage_page().cmp(&b.usage_page()))
            .then(a.usage().cmp(&b.usage()))
    });
    devices
}

fn ensure_input_available() -> Result<()> {
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    if macos::input_access::secure_input_enabled() {
        return Err("macOS Secure Input is active; Spotlight diagnostics cannot read reports. Unlock this Mac and leave password fields, then retry. Input Monitoring permission alone does not resolve this state.".into());
    }
    Ok(())
}

fn transact(device: &HidDevice, request: &Report) -> Result<Report> {
    ensure_input_available()?;
    // A single request is outstanding. A hard deadline still applies if input is busy.
    let deadline = Instant::now() + Duration::from_millis(1200);
    let written = device.write(request.bytes())?;
    if written != request.bytes().len() {
        return Err("Incomplete HID write".into());
    }
    let mut buffer = [0; 64];
    loop {
        ensure_input_available()?;
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(
                "HID++ request timed out; wake the remote, check interface/slot and permissions"
                    .into(),
            );
        }
        let timeout = remaining.as_millis().clamp(1, 100) as i32;
        let count = device.read_timeout(&mut buffer, timeout)?;
        if count == 0 {
            continue;
        }
        let Ok(report) = Report::parse(&buffer[..count]) else {
            continue;
        };
        match report.response_to(request) {
            ResponseKind::Reply => return Ok(report),
            ResponseKind::Error { legacy, code } => {
                return Err(format!(
                    "Device returned {} error 0x{code:02x}",
                    if legacy { "HID++ 1.0" } else { "HID++ 2.0" }
                )
                .into())
            }
            ResponseKind::Unrelated => continue,
        }
    }
}

fn probe(device: &HidDevice, index: u8) -> Result<()> {
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

fn capture(device: &HidDevice, seconds: u64) -> Result<()> {
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

fn resolve_feature(device: &HidDevice, index: u8, code: u16) -> Result<Option<u8>> {
    let request = feature_request(index, code);
    Ok(feature_index(&transact(device, &request)?, &request)?)
}

#[cfg(unix)]
fn watch_top(device: &HidDevice, index: u8, notification_index: u8, seconds: u64) -> Result<()> {
    use std::io::Write;
    let stop = stop_signals::StopSignals::install()?;
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
fn watch_top(
    _device: &HidDevice,
    _index: u8,
    _notification_index: u8,
    _seconds: u64,
) -> Result<()> {
    Err("watch-top currently requires Unix signal handling".into())
}

fn inspect(device: &HidDevice, index: u8) -> Result<()> {
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

fn run() -> Result<()> {
    let command = parse_args(&std::env::args().skip(1).collect::<Vec<_>>())?;
    if command == Command::Help {
        print!("{HELP}");
        return Ok(());
    }
    if matches!(command, Command::App | Command::Demo { .. }) {
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        return macos::run(match command {
            Command::Demo { effect, seconds } => Some((effect, seconds as f64)),
            _ => None,
        });
        #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
        return Err("The native app currently requires macOS Apple Silicon".into());
    }
    if let Command::RenderQa { directory } = &command {
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        return macos::render_qa::run(directory);
        #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
        return Err("Render fixtures require macOS Apple Silicon".into());
    }
    if matches!(
        command,
        Command::Probe { .. }
            | Command::Inspect { .. }
            | Command::Capture { .. }
            | Command::WatchTop { .. }
            | Command::RecoverTop { .. }
    ) {
        ensure_input_available()?;
    }
    let api = HidApi::new()?;
    // hidapi defaults to seizing devices unless macos-shared-device is enabled.
    // Keep normal slide navigation available while observing Spotlight reports.
    #[cfg(target_os = "macos")]
    api.set_open_exclusive(false);
    let devices = interfaces(&api);
    if command == Command::List {
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
        return Ok(());
    }
    let number = match command {
        Command::Probe { device, .. }
        | Command::Inspect { device, .. }
        | Command::WatchTop { device, .. }
        | Command::RecoverTop { device, .. }
        | Command::Capture { device, .. } => device,
        _ => unreachable!(),
    };
    let info = devices
        .get(number)
        .ok_or("No such Spotlight interface. Connect the device and run list again.")?;
    if matches!(
        command,
        Command::Probe { .. }
            | Command::Inspect { .. }
            | Command::WatchTop { .. }
            | Command::RecoverTop { .. }
    ) && info.usage_page() < 0xff00
    {
        return Err(
            "probe/inspect/watch-top requires a vendor-defined HID interface; choose its DEVICE number from list"
                .into(),
        );
    }
    let device = info.open_device(&api).map_err(|e| format!("Cannot open Spotlight: {e}. Check Input Monitoring permission on macOS or hidraw access on Linux."))?;
    match command {
        Command::Probe { index, .. } => probe(&device, index),
        Command::Inspect { index, .. } => inspect(&device, index),
        Command::Capture { seconds, .. } => capture(&device, seconds),
        Command::WatchTop { seconds, index, .. } => watch_top(
            &device,
            index,
            notification_address(matches!(info.bus_type(), hidapi::BusType::Bluetooth), index),
            seconds,
        ),
        Command::RecoverTop { index, .. } => {
            let feature =
                resolve_feature(&device, index, 0x1b04)?.ok_or("REPROG_CONTROLS_V4 unavailable")?;
            TemporaryTopButton::recover_default(|r| transact(&device, r), index, feature)?;
            println!("RECOVERED and verified top flags=0000 mapped_to=00d8");
            Ok(())
        }
        _ => unreachable!(),
    }
}

fn main() {
    if let Err(error) = run() {
        eprintln!("spotlight-rs: {error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn parse(args: &[&str]) -> Result<Command> {
        parse_args(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }
    #[test]
    fn input_limits_prevent_accidental_unbounded_capture_or_wrong_slots() {
        for args in [
            vec!["capture", "0", "0"],
            vec!["capture", "0", "301"],
            vec!["probe", "0", "0"],
            vec!["probe", "0", "255"],
            vec!["inspect", "0", "0"],
            vec!["inspect", "0", "255"],
            vec!["inspect", "-1"],
            vec!["inspect", "0", "1", "extra"],
            vec!["list", "extra"],
            vec!["probe"],
            vec!["capture", "-1"],
            vec!["watch-top", "4", "0"],
            vec!["watch-top", "4", "301"],
            vec!["watch-top", "4", "30", "0"],
            vec!["watch-top", "4", "30", "7"],
            vec!["watch-top", "4", "30", "1", "extra"],
            vec!["unknown"],
        ] {
            assert!(parse(&args).is_err(), "{args:?}");
        }
        assert_eq!(
            parse(&["probe", "2"]).unwrap(),
            Command::Probe {
                device: 2,
                index: 1
            }
        );
        assert_eq!(
            parse(&["capture", "1"]).unwrap(),
            Command::Capture {
                device: 1,
                seconds: 15
            }
        );
        assert_eq!(parse(&[]).unwrap(), Command::App);
        assert_eq!(
            parse(&["watch-top", "4"]).unwrap(),
            Command::WatchTop {
                device: 4,
                seconds: 30,
                index: 1
            }
        );
        assert_eq!(
            parse(&["inspect", "4"]).unwrap(),
            Command::Inspect {
                device: 4,
                index: 1
            }
        );
    }
}
