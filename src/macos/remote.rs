//! Enumeration stays on AppKit's enduring main run loop; device I/O runs in a
//! worker. Only semantic state changes reach the UI. macOS handles cursor motion.
use hidapi::{BusType, DeviceInfo, HidApi};
use objc2::MainThreadMarker;
use spotlight_rs::controls::{
    active_controls, notification_address, Reporting, TemporaryTopButton, BACK_HOLD, NEXT_HOLD,
    SWITCH_HIGHLIGHT, TOP_HOLD,
};
use spotlight_rs::{Report, SOFTWARE_ID};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc,
};
use std::thread::{self, JoinHandle};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gesture {
    DoubleClick,
    NextHold,
    BackHold,
}

pub enum Event {
    /// Transport name, a warning when optional gestures are unavailable, and
    /// every control now diverted with its original (persisted in case the
    /// process dies before restoring them).
    Connected(&'static str, Option<String>, Unrestored),
    Hold(bool),
    Gesture(Gesture),
    /// A native page-turn key (short next/back) was pressed on the remote.
    PageTurn,
    /// The vibration carrying this reminder text could not be delivered.
    VibrationFailed(String),
    Message(String),
    /// Battery percentage and BATTERY_STATUS state byte (1–3 = charging).
    Battery(u8, u8),
    /// The worker ended; its result and leftovers come from `Remote::stop`.
    Stopped,
}

/// Controls this process diverted but could not restore (usually because the
/// remote disconnected), keyed by CID with the original reporting state.
pub type Unrestored = Vec<(u16, Reporting)>;

enum Command {
    /// Reminder text travels with the request so a failure can show it.
    Vibrate(String),
}

/// Intensity and length confirmed perceptible on the user's first-generation unit.
const VIBRATION: [u8; 3] = [3, 0xe8, 200];

pub struct Remote {
    stop: Arc<AtomicBool>,
    commands: mpsc::Sender<Command>,
    worker: Option<JoinHandle<Outcome>>,
}

/// Worker result plus controls whose restoration failed. Returned through
/// join so it also survives a user-initiated stop (whose event is discarded).
pub type Outcome = (std::result::Result<(), String>, Unrestored);

impl Remote {
    pub fn start(
        _mtm: MainThreadMarker,
        unrestored: Unrestored,
        notify: impl Fn(Event) + Send + 'static,
    ) -> crate::Result<Self> {
        crate::ensure_input_available()?;
        // hidapi never deinitializes its process-global IOHIDManager. Its first
        // CFRunLoop must outlive every reconnect, so initialize/enumerate on main.
        // Initializing it on a disposable worker leaves a dangling run-loop target.
        let api = HidApi::new()?;
        api.set_open_exclusive(false);
        let interfaces = crate::interfaces(&api);
        let info = interfaces
            .iter()
            .copied()
            .filter(|d| d.usage_page() >= 0xff00)
            .min_by_key(|d| {
                if matches!(d.bus_type(), BusType::Bluetooth) {
                    0
                } else {
                    1
                }
            })
            .ok_or("未找到第一代 Spotlight；连接蓝牙或 USB 接收器后，点击连接遥控器。")?
            .clone();
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let (commands, inbox) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("spotlight-hid".into())
            .spawn(move || {
                // Leases' Drop attempts restoration during unwinding. Report a
                // failure and clear UI hold even if transport code unexpectedly panics.
                let mut unrestored = unrestored;
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run(api, info, &stopping, &inbox, &mut unrestored, &notify)
                        .map_err(|e| e.to_string())
                }))
                .unwrap_or_else(|_| Err("遥控器读取线程异常退出；请检查恢复日志。".into()));
                notify(Event::Hold(false));
                notify(Event::Stopped);
                (result, unrestored)
            })?;
        Ok(Self {
            stop,
            commands,
            worker: Some(worker),
        })
    }

    /// Queues one vibration; false when the worker has already exited.
    /// Asynchronous failures arrive as `Event::VibrationFailed(reminder)`.
    pub fn vibrate(&self, reminder: String) -> bool {
        self.commands.send(Command::Vibrate(reminder)).is_ok()
    }

    pub fn stop(&mut self) -> Outcome {
        self.stop.store(true, Ordering::Relaxed);
        match self.worker.take() {
            Some(worker) => worker
                .join()
                .unwrap_or_else(|_| (Err("遥控器线程未正常退出。".into()), Vec::new())),
            None => (Ok(()), Vec::new()),
        }
    }
}

impl Drop for Remote {
    fn drop(&mut self) {
        if let (Err(error), _) = self.stop() {
            eprintln!("REMOTE cleanup: {error}");
        }
    }
}

/// Turns explicit held-control snapshots into UI events. Gestures fire on the
/// press edge: the firmware reports 00da/00dc only once its hold threshold is
/// reached, and 00df as a short pulse after a completed double click.
struct Buttons<'a, F: Fn(Event)> {
    address: u8,
    feature: u8,
    held: Vec<u16>,
    notify: &'a F,
}

impl<F: Fn(Event)> Buttons<'_, F> {
    fn handle(&mut self, report: &Report) {
        let Some(now) = active_controls(report, self.address, self.feature) else {
            return;
        };
        for cid in now.iter().filter(|c| !self.held.contains(c)) {
            match *cid {
                TOP_HOLD => (self.notify)(Event::Hold(true)),
                SWITCH_HIGHLIGHT => (self.notify)(Event::Gesture(Gesture::DoubleClick)),
                NEXT_HOLD => (self.notify)(Event::Gesture(Gesture::NextHold)),
                BACK_HOLD => (self.notify)(Event::Gesture(Gesture::BackHold)),
                _ => {}
            }
        }
        if self.held.contains(&TOP_HOLD) && !now.contains(&TOP_HOLD) {
            (self.notify)(Event::Hold(false));
        }
        self.held = now;
    }
}

fn run(
    api: HidApi,
    info: DeviceInfo,
    stop: &AtomicBool,
    inbox: &mpsc::Receiver<Command>,
    unrestored: &mut Unrestored,
    notify: &impl Fn(Event),
) -> crate::Result<()> {
    crate::ensure_input_available()?;
    let bluetooth = matches!(info.bus_type(), BusType::Bluetooth);
    let device = info.open_device(&api)?;
    let index = 1;
    let feature =
        crate::resolve_feature(&device, index, 0x1b04)?.ok_or("遥控器未提供顶键控制功能。")?;
    // Vibration and battery are optional; a failed lookup only hides them.
    let presenter = crate::resolve_feature(&device, index, 0x1a00)
        .ok()
        .flatten();
    let battery = crate::resolve_feature(&device, index, 0x1000)
        .ok()
        .flatten();
    if stop.load(Ordering::Relaxed) {
        return Ok(());
    }
    // The top hold is required. Gesture controls are optional: one that is
    // already owned by another client is left untouched and reported.
    // One Copy closure type for every lease, so they can share a list.
    let transport = |r: &Report| crate::transact(&device, r);
    let leftover = |cid: u16| unrestored.iter().find(|(c, _)| *c == cid).map(|(_, r)| *r);
    let top = TemporaryTopButton::start_or_adopt(
        transport,
        index,
        feature,
        TOP_HOLD,
        leftover(TOP_HOLD),
    )?;
    let mut leased = vec![(TOP_HOLD, top.original())];
    let mut gestures = Vec::new();
    let mut unavailable = Vec::new();
    for (cid, name) in [
        (SWITCH_HIGHLIGHT, "双击切换"),
        (NEXT_HOLD, "长按下一页"),
        (BACK_HOLD, "长按上一页"),
    ] {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        match TemporaryTopButton::start_or_adopt(transport, index, feature, cid, leftover(cid)) {
            Ok(lease) => {
                leased.push((cid, lease.original()));
                gestures.push(lease);
            }
            Err(error) => {
                eprintln!("REMOTE gesture {cid:04x} unavailable: {error}");
                unavailable.push(name);
            }
        }
    }
    // Every leased control is now owned (and will be restored) by this session.
    unrestored.retain(|(cid, _)| !leased.iter().any(|(c, _)| c == cid));
    let address = notification_address(bluetooth, index);
    eprintln!(
        "REMOTE READY transport={} query={index:02x} notifications={address:02x} feature={feature:02x} gestures={} presenter={presenter:?}",
        if bluetooth { "Bluetooth" } else { "USB" },
        gestures.len()
    );
    let mut warning = (!unavailable.is_empty())
        .then(|| format!("{}不可用（可能被其他软件占用）。", unavailable.join("、")));
    if presenter.is_none() {
        warning = Some(format!(
            "{}设备未提供振动功能，计时器只在屏幕上提示。",
            warning.unwrap_or_default()
        ));
    }
    notify(Event::Connected(
        if bluetooth { "蓝牙" } else { "USB" },
        warning,
        leased,
    ));
    let mut buttons = Buttons {
        address,
        feature,
        held: Vec::new(),
        notify,
    };
    let captured: crate::Result<()> = (|| {
        let mut buffer = [0; 64];
        let mut paused = false;
        // Read at once, then every five minutes; the device reports coarse steps.
        let mut battery_due = std::time::Instant::now();
        let mut keys_down = false;
        while !stop.load(Ordering::Relaxed) {
            while let Ok(command) = inbox.try_recv() {
                match command {
                    Command::Vibrate(reminder) => {
                        let Some(presenter) = presenter else {
                            notify(Event::VibrationFailed(reminder));
                            continue;
                        };
                        let request =
                            Report::request(index, presenter, 1, SOFTWARE_ID, &VIBRATION)?;
                        // Button notifications read while waiting are still handled.
                        if let Err(error) =
                            crate::transact_observing(&device, &request, &mut |r| buttons.handle(r))
                        {
                            eprintln!("REMOTE vibration failed: {error}");
                            notify(Event::VibrationFailed(reminder));
                        }
                    }
                }
            }
            if let Some(battery) = battery.filter(|_| std::time::Instant::now() >= battery_due) {
                battery_due = std::time::Instant::now() + std::time::Duration::from_secs(300);
                let request = Report::request(index, battery, 0, SOFTWARE_ID, &[])?;
                match crate::transact_observing(&device, &request, &mut |r| buttons.handle(r)) {
                    Ok(reply) if reply.payload().len() >= 3 => {
                        let p = reply.payload();
                        notify(Event::Battery(p[0].min(100), p[2]));
                    }
                    Ok(_) => {}
                    Err(error) => eprintln!("REMOTE battery read failed: {error}"),
                }
            }
            // Secure Input (a focused password field) blocks HID reads. Pause
            // with the leases kept; restoring now would fail and strand them.
            if super::input_access::secure_input_enabled() {
                if !paused {
                    paused = true;
                    notify(Event::Hold(false));
                    buttons.held.clear();
                    notify(Event::Message(
                        "安全输入已开启（例如密码框获得焦点），遥控器暂停；离开密码框后自动恢复。"
                            .into(),
                    ));
                }
                thread::sleep(std::time::Duration::from_millis(200));
                continue;
            }
            if paused {
                paused = false;
                notify(Event::Message("安全输入已关闭，遥控器已恢复。".into()));
            }
            let count = device.read_timeout(&mut buffer, 100)?;
            if count == 0 {
                continue;
            }
            // Keyboard report 1: modifier, reserved/keys. Page turns arrive
            // here natively (RightArrow/LeftArrow); only the press edge counts.
            if buffer[0] == 0x01 && count >= 3 {
                let down = buffer[2..count].iter().any(|key| *key != 0);
                if down && !keys_down {
                    notify(Event::PageTurn);
                }
                keys_down = down;
            }
            if let Ok(report) = Report::parse(&buffer[..count]) {
                buttons.handle(&report);
            }
        }
        Ok(())
    })();
    // Hide promptly on read/disconnection failure, before cleanup transactions.
    notify(Event::Hold(false));
    let mut restore_errors = Vec::new();
    gestures.insert(0, top);
    while let Some(lease) = gestures.pop() {
        let cid = lease.control();
        let original = lease.original();
        if let Err(error) = lease.finish() {
            restore_errors.push(format!("{cid:04x}: {error}"));
            unrestored.push((cid, original));
        }
    }
    if restore_errors.is_empty() {
        eprintln!("REMOTE RESTORED and verified every diverted control");
    }
    let restore = restore_errors.join("; ");
    match (captured, restore_errors.is_empty()) {
        (Ok(()), true) => Ok(()),
        (Err(error), true) => Err(format!("{error}；按键原值已恢复并校验。请重新连接。").into()),
        (Ok(()), false) => Err(format!("RESTORE FAILED: {restore}").into()),
        (Err(error), false) => Err(format!("{error}; RESTORE FAILED: {restore}").into()),
    }
}
