//! Control panel: status first, then effects, buttons, timers and permissions.
//! Built with stack views so rows size to their content (no fixed frames).
use super::*;
use objc2::Message;
use objc2_core_graphics::CGRequestPostEventAccess;

const CONTENT_WIDTH: f64 = 440.0;
const LABEL_WIDTH: f64 = 76.0;

/// Connection state shown in the status line.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum RemoteStatus {
    Connecting,
    Connected(&'static str),
    Reconnecting,
    Waiting,
    Disconnected,
}

pub(super) struct Panel {
    pub window: Retained<NSWindow>,
    status_dot: Retained<NSTextField>,
    status_text: Retained<NSTextField>,
    effect_label: Retained<NSTextField>,
    message: Retained<NSTextField>,
    pub timer_fields: Vec<Retained<NSTextField>>,
    pub radius: Retained<NSSlider>,
    elapsed: Retained<NSTextField>,
    timer_button: Retained<NSButton>,
    permissions: Vec<(Retained<NSTextField>, Retained<NSButton>)>,
    remote_button: Retained<NSButton>,
}

/// (name, why it is needed, System Settings anchor)
const PERMISSIONS: [(&str, &str, &str); 3] = [
    ("输入监控", "读取遥控器按键", "Privacy_ListenEvent"),
    (
        "辅助功能",
        "长按翻页发送播放快捷键",
        "Privacy_Accessibility",
    ),
    ("屏幕录制", "实时放大", "Privacy_ScreenCapture"),
];

fn permission_granted(slot: usize) -> bool {
    match slot {
        0 => super::input_access::input_monitoring_granted(),
        1 => CGPreflightPostEventAccess(),
        _ => CGPreflightScreenCaptureAccess(),
    }
}

fn label(text: &str, mtm: MainThreadMarker) -> Retained<NSTextField> {
    NSTextField::labelWithString(&NSString::from_str(text), mtm)
}

fn secondary(text: &str, mtm: MainThreadMarker) -> Retained<NSTextField> {
    let field = label(text, mtm);
    field.setTextColor(Some(&NSColor::secondaryLabelColor()));
    field.setFont(Some(&NSFont::systemFontOfSize(12.0)));
    field
}

fn heading(text: &str, mtm: MainThreadMarker) -> Retained<NSTextField> {
    let field = label(text, mtm);
    field.setFont(Some(&NSFont::boldSystemFontOfSize(13.0)));
    field
}

fn width(view: &NSView, value: f64) {
    view.widthAnchor()
        .constraintEqualToConstant(value)
        .setActive(true);
}

fn stack(
    orientation: NSUserInterfaceLayoutOrientation,
    mtm: MainThreadMarker,
) -> Retained<NSStackView> {
    let stack = NSStackView::stackViewWithViews(&NSArray::new(), mtm);
    stack.setOrientation(orientation);
    stack
}

/// Full-width row: `leading` views from the left, `trailing` from the right.
fn row(leading: &[&NSView], trailing: &[&NSView], mtm: MainThreadMarker) -> Retained<NSStackView> {
    let row = stack(NSUserInterfaceLayoutOrientation::Horizontal, mtm);
    row.setAlignment(NSLayoutAttribute::CenterY);
    row.setSpacing(8.0);
    for view in leading {
        row.addView_inGravity(view, NSStackViewGravity::Leading);
    }
    for view in trailing {
        row.addView_inGravity(view, NSStackViewGravity::Trailing);
    }
    width(&row, CONTENT_WIDTH);
    row
}

/// Fixed-width caption so controls in a section line up.
fn caption(text: &str, mtm: MainThreadMarker) -> Retained<NSTextField> {
    let field = label(text, mtm);
    width(&field, LABEL_WIDTH);
    field
}

fn separator(mtm: MainThreadMarker) -> Retained<NSBox> {
    let line = NSBox::initWithFrame(NSBox::alloc(mtm), NSRect::ZERO);
    line.setBoxType(NSBoxType::Separator);
    width(&line, CONTENT_WIDTH);
    line
}

fn popup(items: &[&str], selected: usize, mtm: MainThreadMarker) -> Retained<NSPopUpButton> {
    let popup =
        NSPopUpButton::initWithFrame_pullsDown(NSPopUpButton::alloc(mtm), NSRect::ZERO, false);
    for item in items {
        popup.addItemWithTitle(&NSString::from_str(item));
    }
    popup.selectItemAtIndex(selected as isize);
    popup
}

impl Delegate {
    pub(super) fn show_controls(&self) {
        if self.ivars().panel.borrow().is_none() {
            let panel = self.build_panel();
            self.ivars().panel.replace(Some(panel));
        }
        self.refresh_panel(true);
        if let Some(panel) = self.ivars().panel.borrow().as_ref() {
            panel.window.makeKeyAndOrderFront(None);
        }
        #[allow(deprecated)]
        NSApplication::sharedApplication(self.mtm()).activateIgnoringOtherApps(true);
    }

    fn target(&self) -> Option<&AnyObject> {
        Some(self)
    }

    fn build_panel(&self) -> Panel {
        let mtm = self.mtm();
        let settings = self.ivars().settings.borrow().clone();
        let mut views: Vec<Retained<NSView>> = Vec::new();
        let push = |views: &mut Vec<Retained<NSView>>, view: &NSView| {
            views.push(view.retain());
        };

        // Status: connection, battery, current effect, latest message.
        let status_dot = label("●", mtm);
        let status_text = label("", mtm);
        let effect_label = secondary("", mtm);
        push(
            &mut views,
            &row(&[&status_dot, &status_text], &[&effect_label], mtm),
        );
        let message = NSTextField::wrappingLabelWithString(
            &NSString::from_str(&self.ivars().message.borrow()),
            mtm,
        );
        message.setTextColor(Some(&NSColor::secondaryLabelColor()));
        message.setFont(Some(&NSFont::systemFontOfSize(12.0)));
        width(&message, CONTENT_WIDTH);
        // Two lines reserved so longer messages never resize the window.
        message
            .heightAnchor()
            .constraintEqualToConstant(32.0)
            .setActive(true);
        push(&mut views, &message);
        push(&mut views, &separator(mtm));

        // Effects.
        push(&mut views, &heading("特效", mtm));
        let mut cycle: Vec<Retained<NSView>> =
            vec![caption("双击切换", mtm).into_super().into_super()];
        for (slot, effect) in EFFECTS.iter().enumerate() {
            // SAFETY: target/action refer to this main-thread delegate's selector.
            let button = unsafe {
                NSButton::checkboxWithTitle_target_action(
                    &NSString::from_str(Self::effect_name(*effect)),
                    self.target(),
                    Some(sel!(cycleChanged:)),
                    mtm,
                )
            };
            button.setTag(slot as isize);
            button.setState(if settings.cycle[slot] { 1 } else { 0 });
            cycle.push(button.into_super().into_super());
        }
        let cycle_refs: Vec<&NSView> = cycle.iter().map(|v| &**v).collect();
        push(&mut views, &row(&cycle_refs, &[], mtm));
        // SAFETY: target/action refer to this main-thread delegate's selectors.
        let (radius, shade) = unsafe {
            (
                NSSlider::sliderWithValue_minValue_maxValue_target_action(
                    settings.radius,
                    orange_beam::presentation::MIN_RADIUS,
                    orange_beam::presentation::MAX_RADIUS,
                    self.target(),
                    Some(sel!(radiusChanged:)),
                    mtm,
                ),
                NSSlider::sliderWithValue_minValue_maxValue_target_action(
                    settings.shade,
                    0.2,
                    0.9,
                    self.target(),
                    Some(sel!(shadeChanged:)),
                    mtm,
                ),
            )
        };
        width(&radius, 140.0);
        width(&shade, 110.0);
        let shade_label = label("暗度", mtm);
        push(
            &mut views,
            &row(
                &[&caption("大小", mtm), &radius],
                &[&shade_label, &shade],
                mtm,
            ),
        );
        let zoom_titles: Vec<String> = orange_beam::settings::ZOOM_LEVELS
            .iter()
            .map(|z| format!("{z}×"))
            .collect();
        let zoom_refs: Vec<&str> = zoom_titles.iter().map(String::as_str).collect();
        let zoom_index = orange_beam::settings::ZOOM_LEVELS
            .iter()
            .position(|z| *z == settings.zoom)
            .unwrap_or(1);
        let zoom = popup(&zoom_refs, zoom_index, mtm);
        // SAFETY: target/action refer to this main-thread delegate's selectors.
        let preview = unsafe {
            zoom.setTarget(self.target());
            zoom.setAction(Some(sel!(zoomChanged:)));
            NSButton::buttonWithTitle_target_action(
                ns_string!("预览当前特效"),
                self.target(),
                Some(sel!(previewCurrent:)),
                mtm,
            )
        };
        push(
            &mut views,
            &row(&[&caption("放大倍数", mtm), &zoom], &[&preview], mtm),
        );
        push(&mut views, &separator(mtm));

        // Remote buttons.
        push(&mut views, &heading("按键", mtm));
        let hold_titles: Vec<&str> = HoldAction::ALL.iter().map(|a| a.label()).collect();
        for (tag, (text, action)) in [
            ("长按下一页", settings.next_hold),
            ("长按上一页", settings.back_hold),
        ]
        .into_iter()
        .enumerate()
        {
            let selected = HoldAction::ALL
                .iter()
                .position(|a| *a == action)
                .unwrap_or(0);
            let choice = popup(&hold_titles, selected, mtm);
            choice.setTag(tag as isize);
            // SAFETY: target/action refer to this main-thread delegate's selector.
            unsafe {
                choice.setTarget(self.target());
                choice.setAction(Some(sel!(holdChanged:)));
            }
            width(&choice, 200.0);
            push(&mut views, &row(&[&caption(text, mtm), &choice], &[], mtm));
        }
        push(&mut views, &separator(mtm));

        // Timers.
        let elapsed = secondary("", mtm);
        push(
            &mut views,
            &row(&[&heading("计时器", mtm)], &[&elapsed], mtm),
        );
        let mut timer_fields = Vec::new();
        for (slot, minutes) in settings.timers.iter().enumerate() {
            let field = NSTextField::textFieldWithString(
                &NSString::from_str(&minutes.map(|m| m.to_string()).unwrap_or_default()),
                mtm,
            );
            width(&field, 56.0);
            let unit = label("分钟", mtm);
            let hint = secondary(
                if slot == 0 {
                    "到点遥控器振动；留空表示不用"
                } else {
                    ""
                },
                mtm,
            );
            push(
                &mut views,
                &row(
                    &[&caption(&format!("定时 {}", slot + 1), mtm), &field, &unit],
                    &[&hint],
                    mtm,
                ),
            );
            timer_fields.push(field);
        }
        let reminder_index = ScreenReminder::ALL
            .iter()
            .position(|m| *m == settings.screen_reminder)
            .unwrap_or(1);
        let reminder_titles: Vec<&str> = ScreenReminder::ALL.iter().map(|m| m.label()).collect();
        let reminder = popup(&reminder_titles, reminder_index, mtm);
        // SAFETY: target/action refer to this main-thread delegate's selectors.
        let timer_button = unsafe {
            reminder.setTarget(self.target());
            reminder.setAction(Some(sel!(reminderChanged:)));
            NSButton::buttonWithTitle_target_action(
                ns_string!("开始计时"),
                self.target(),
                Some(sel!(toggleTimer:)),
                mtm,
            )
        };
        push(
            &mut views,
            &row(
                &[&caption("屏幕提醒", mtm), &reminder],
                &[&timer_button],
                mtm,
            ),
        );
        push(&mut views, &separator(mtm));

        // Permissions, each with a shortcut to its System Settings page.
        push(&mut views, &heading("权限", mtm));
        let mut permissions = Vec::new();
        for (slot, (name, purpose, _)) in PERMISSIONS.iter().enumerate() {
            let state = label(name, mtm);
            width(&state, 110.0);
            let why = secondary(purpose, mtm);
            // SAFETY: target/action refer to this main-thread delegate's selector.
            let open = unsafe {
                NSButton::buttonWithTitle_target_action(
                    ns_string!("去设置"),
                    self.target(),
                    Some(sel!(openPrivacy:)),
                    mtm,
                )
            };
            open.setTag(slot as isize);
            push(&mut views, &row(&[&state, &why], &[&open], mtm));
            permissions.push((state, open));
        }
        push(&mut views, &separator(mtm));

        // Footer: emergency shortcut and connection control.
        // SAFETY: target/action refer to this main-thread delegate's selector.
        let remote_button = unsafe {
            NSButton::buttonWithTitle_target_action(
                ns_string!("断开遥控器"),
                self.target(),
                Some(sel!(toggleRemote:)),
                mtm,
            )
        };
        push(
            &mut views,
            &row(
                &[&secondary("⌃⌥⌘H 立即隐藏（含黑屏）", mtm)],
                &[&remote_button],
                mtm,
            ),
        );

        let refs: Vec<&NSView> = views.iter().map(|v| &**v).collect();
        let root = NSStackView::stackViewWithViews(&NSArray::from_slice(&refs), mtm);
        root.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
        root.setAlignment(NSLayoutAttribute::Leading);
        root.setSpacing(10.0);
        root.setEdgeInsets(NSEdgeInsets {
            top: 16.0,
            left: 20.0,
            bottom: 16.0,
            right: 20.0,
        });
        // SAFETY: AppKit window on main. The panel retains it, so automatic
        // release-on-close is disabled before it can be shown or closed.
        let window = unsafe {
            let window = NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                NSRect::new(NSPoint::ZERO, NSSize::new(CONTENT_WIDTH + 40.0, 400.0)),
                // No miniaturize: a minimized window would add a Dock tile to
                // this menu-bar-only app. Closing hides it; the menu bar icon reopens it.
                NSWindowStyleMask::Titled | NSWindowStyleMask::Closable,
                NSBackingStoreType::Buffered,
                false,
            );
            window.setReleasedWhenClosed(false);
            window
        };
        window.setTitle(&NSString::from_str(app_name()));
        let fitting = root.fittingSize();
        window.setContentView(Some(&root));
        window.setContentSize(fitting);
        window.center();
        Panel {
            window,
            status_dot,
            status_text,
            effect_label,
            message,
            timer_fields,
            radius,
            elapsed,
            timer_button,
            permissions,
            remote_button,
        }
    }

    pub(super) fn set_message(&self, text: &str) {
        if *self.ivars().message.borrow() == text {
            return;
        }
        self.ivars().message.replace(text.to_string());
        if let Some(panel) = self.ivars().panel.borrow().as_ref() {
            panel.message.setStringValue(&NSString::from_str(text));
        }
    }

    pub(super) fn set_remote_status(&self, status: RemoteStatus) {
        self.ivars().remote_status.set(status);
        if !matches!(status, RemoteStatus::Connected(_)) {
            self.ivars().battery.set(None);
        }
        let title = NSString::from_str(if self.ivars().remote.borrow().is_some() {
            "断开遥控器"
        } else {
            "连接遥控器"
        });
        if let Some(item) = self.ivars().remote_item.borrow().as_ref() {
            item.setTitle(&title);
        }
        self.refresh_panel(false);
    }

    /// Cheap text updates; `permissions` also re-queries the privacy state.
    pub(super) fn refresh_panel(&self, permissions: bool) {
        let panel = self.ivars().panel.borrow();
        let Some(panel) = panel.as_ref() else {
            return;
        };
        let set = |field: &NSTextField, text: &str| {
            if field.stringValue().to_string() != text {
                field.setStringValue(&NSString::from_str(text));
            }
        };
        let (color, text) = match self.ivars().remote_status.get() {
            RemoteStatus::Connected(transport) => {
                let battery = match self.ivars().battery.get() {
                    Some((level, 1..=3)) => format!(" · 电量 {level}%（充电中）"),
                    Some((level, _)) => format!(" · 电量 {level}%"),
                    None => String::new(),
                };
                (
                    NSColor::systemGreenColor(),
                    format!("{transport}已连接{battery}"),
                )
            }
            RemoteStatus::Connecting => (NSColor::systemOrangeColor(), "正在连接…".into()),
            RemoteStatus::Reconnecting => (
                NSColor::systemOrangeColor(),
                "连接中断，正在自动重连…".into(),
            ),
            RemoteStatus::Waiting => (
                NSColor::systemOrangeColor(),
                "未找到遥控器，自动重试中".into(),
            ),
            RemoteStatus::Disconnected => (
                NSColor::secondaryLabelColor(),
                "已断开（不自动重连）".into(),
            ),
        };
        panel.status_dot.setTextColor(Some(&color));
        set(&panel.status_text, &text);
        let effect = self.ivars().presentation.borrow().effect;
        set(
            &panel.effect_label,
            &format!("当前特效：{}", Self::effect_name(effect)),
        );
        let connected = self.ivars().remote.borrow().is_some();
        let remote_title = if connected {
            "断开遥控器"
        } else {
            "连接遥控器"
        };
        if panel.remote_button.title().to_string() != remote_title {
            panel
                .remote_button
                .setTitle(&NSString::from_str(remote_title));
        }
        let elapsed = self.ivars().talk_timer.borrow().elapsed(self.now());
        set(
            &panel.elapsed,
            &elapsed
                .map(|s| format!("已用 {:02}:{:02}", s as u64 / 60, s as u64 % 60))
                .unwrap_or_default(),
        );
        let timer_title = if elapsed.is_some() {
            "停止计时"
        } else {
            "开始计时"
        };
        if panel.timer_button.title().to_string() != timer_title {
            panel
                .timer_button
                .setTitle(&NSString::from_str(timer_title));
        }
        if permissions {
            for (slot, (state, open)) in panel.permissions.iter().enumerate() {
                let granted = permission_granted(slot);
                let name = PERMISSIONS[slot].0;
                set(
                    state,
                    &format!("{} {name}", if granted { "✓" } else { "✗" }),
                );
                let color = if granted {
                    NSColor::labelColor()
                } else {
                    NSColor::systemRedColor()
                };
                state.setTextColor(Some(&color));
                open.setHidden(granted);
            }
        }
    }

    /// Asks once (the system prompt appears only the first time), then opens
    /// the matching System Settings page for a later manual grant.
    pub(super) fn open_privacy(&self, slot: usize) {
        match slot {
            0 => {
                super::input_access::request_input_monitoring();
            }
            1 => {
                CGRequestPostEventAccess();
            }
            _ => {
                CGRequestScreenCaptureAccess();
            }
        }
        let anchor = PERMISSIONS[slot.min(2)].2;
        let url = format!("x-apple.systempreferences:com.apple.preference.security?{anchor}");
        if let Some(url) = NSURL::URLWithString(&NSString::from_str(&url)) {
            NSWorkspace::sharedWorkspace().openURL(&url);
        }
        self.set_message(&format!("授权后如状态未更新，请重新打开{}。", app_name()));
        self.refresh_panel(true);
    }
}
