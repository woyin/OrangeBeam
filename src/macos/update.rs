//! Update check: reads the public GitHub release list on a worker thread and
//! reports on main. It never downloads or installs anything; Homebrew
//! installs are pointed at `brew upgrade`, others at the release page.
use super::*;
use orange_beam::update::{newer_release, Release, Version};

const RELEASES_API: &str = "https://api.github.com/repos/woyin/OrangeBeam/releases?per_page=20";
const RELEASES_PAGE: &str = "https://github.com/woyin/OrangeBeam/releases";
const BREW_UPGRADE: &str = "brew upgrade --cask orange-beam";
/// After a failed automatic check (offline, rate limit), wait before retrying.
const AUTO_RETRY_SECS: f64 = 3600.0;

pub(super) fn current_version() -> Version {
    Version::parse(env!("CARGO_PKG_VERSION")).expect("Cargo package version is valid")
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn installed_with_homebrew() -> bool {
    [
        "/opt/homebrew/Caskroom/orange-beam",
        "/usr/local/Caskroom/orange-beam",
    ]
    .iter()
    .any(|path| std::path::Path::new(path).exists())
}

/// Blocking; runs on a worker thread.
fn fetch_releases() -> Result<Vec<Release>, String> {
    let output = std::process::Command::new("/usr/bin/curl")
        .args([
            "-fsSL",
            "--max-time",
            "15",
            "-H",
            "Accept: application/vnd.github+json",
            "-H",
            concat!("User-Agent: OrangeBeam/", env!("CARGO_PKG_VERSION")),
            RELEASES_API,
        ])
        .output()
        .map_err(|error| format!("无法运行 curl：{error}"))?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr);
        return Err(format!("无法访问 GitHub：{}", detail.trim()));
    }
    parse_releases(&output.stdout)
}

/// Published releases from the GitHub API response; drafts are skipped.
fn parse_releases(json: &[u8]) -> Result<Vec<Release>, String> {
    let data = NSData::with_bytes(json);
    let object =
        NSJSONSerialization::JSONObjectWithData_options_error(&data, NSJSONReadingOptions::empty())
            .map_err(|error| error.localizedDescription().to_string())?;
    let list = object
        .downcast::<NSArray>()
        .map_err(|_| "GitHub 返回的发布列表格式不对".to_string())?;
    let mut releases = Vec::new();
    for item in list.iter() {
        let Ok(entry) = item.downcast::<NSDictionary>() else {
            continue;
        };
        let value = |key: &str| entry.objectForKey(&NSString::from_str(key));
        let flag = |key: &str| {
            value(key)
                .and_then(|v| v.downcast::<NSNumber>().ok())
                .is_some_and(|n| n.boolValue())
        };
        let text = |key: &str| {
            value(key)
                .and_then(|v| v.downcast::<NSString>().ok())
                .map(|s| s.to_string())
        };
        if flag("draft") {
            continue;
        }
        if let (Some(tag), Some(url)) = (text("tag_name"), text("html_url")) {
            releases.push(Release {
                tag,
                prerelease: flag("prerelease"),
                url,
            });
        }
    }
    Ok(releases)
}

impl Delegate {
    /// Scheduled check, evaluated at most once a minute from tick.
    pub(super) fn maybe_check_for_updates(&self, now: f64) {
        if now < self.ivars().update_due_at.get() || self.ivars().initial_demo.is_some() {
            return;
        }
        self.ivars().update_due_at.set(now + 60.0);
        let due = {
            let settings = self.ivars().settings.borrow();
            settings
                .update_check
                .is_due(settings.last_update_check, unix_now())
        };
        if due {
            self.check_for_updates(false);
        }
    }

    /// `manual` checks report every outcome in an alert; scheduled ones only
    /// announce a new version (menu item and panel message).
    pub(super) fn check_for_updates(&self, manual: bool) {
        if self.ivars().update_checking.replace(true) {
            return;
        }
        if manual {
            self.set_message("正在检查更新…");
        }
        // SAFETY: the delegate lives for the app; the retain is released on
        // main because the bound value is moved into the main-queue closure.
        let retained =
            unsafe { Retained::retain(self as *const Self as *mut Self) }.expect("live delegate");
        let target = MainThreadBound::new(retained, self.mtm());
        let spawned = std::thread::Builder::new()
            .name("update-check".into())
            .spawn(move || {
                let result = fetch_releases();
                DispatchQueue::main().exec_async(move || {
                    let mtm = MainThreadMarker::new().expect("main queue");
                    target.get(mtm).finish_update_check(result, manual);
                });
            });
        if let Err(error) = spawned {
            self.ivars().update_checking.set(false);
            eprintln!("UPDATE could not start: {error}");
        }
    }

    fn finish_update_check(&self, result: Result<Vec<Release>, String>, manual: bool) {
        self.ivars().update_checking.set(false);
        let releases = match result {
            Ok(releases) => releases,
            Err(error) => {
                eprintln!("UPDATE check failed: {error}");
                self.ivars().update_due_at.set(self.now() + AUTO_RETRY_SECS);
                if manual {
                    self.alert("检查更新失败", &error, &["好"]);
                }
                return;
            }
        };
        self.ivars().settings.borrow_mut().last_update_check = Some(unix_now());
        self.save_settings();
        let current = current_version();
        let found = newer_release(&current, &releases)
            .map(|(version, release)| (version.as_str().to_string(), release.url.clone()));
        eprintln!(
            "UPDATE checked: current {} newest offered {:?}",
            current.as_str(),
            found.as_ref().map(|(v, _)| v)
        );
        self.ivars().available_update.replace(found.clone());
        self.refresh_menu();
        self.refresh_panel(false);
        match found {
            Some((version, url)) => {
                self.set_message(&format!("发现新版本 {version}，可在菜单栏中查看。"));
                if manual {
                    self.offer_update(&version, &url);
                }
            }
            None if manual => {
                self.set_message("已是最新版本。");
                self.alert(
                    "已是最新版本",
                    &format!("当前版本 {}。", current.as_str()),
                    &["好"],
                );
            }
            None => {}
        }
    }

    /// Menu entry: show a known update, otherwise check now.
    pub(super) fn update_menu_action(&self) {
        let known = self.ivars().available_update.borrow().clone();
        match known {
            Some((version, url)) => self.offer_update(&version, &url),
            None => self.check_for_updates(true),
        }
    }

    fn offer_update(&self, version: &str, url: &str) {
        let current = current_version();
        if installed_with_homebrew() {
            let choice = self.alert(
                &format!("发现新版本 {version}"),
                &format!(
                    "当前版本 {}。通过 Homebrew 安装的，请在终端运行：\n{BREW_UPGRADE}",
                    current.as_str()
                ),
                &["复制更新命令", "查看更新内容", "稍后"],
            );
            match choice {
                0 => {
                    let board = NSPasteboard::generalPasteboard();
                    board.clearContents();
                    // SAFETY: the string pasteboard type is a static constant.
                    board.setString_forType(&NSString::from_str(BREW_UPGRADE), unsafe {
                        NSPasteboardTypeString
                    });
                    self.set_message(&format!("已复制：{BREW_UPGRADE}"));
                }
                1 => open_url(url),
                _ => {}
            }
        } else {
            let choice = self.alert(
                &format!("发现新版本 {version}"),
                &format!("当前版本 {}。", current.as_str()),
                &["打开下载页", "稍后"],
            );
            if choice == 0 {
                open_url(if url.is_empty() { RELEASES_PAGE } else { url });
            }
        }
    }

    /// Modal alert; returns the index of the chosen button.
    fn alert(&self, title: &str, detail: &str, buttons: &[&str]) -> usize {
        let mtm = self.mtm();
        let alert = NSAlert::new(mtm);
        alert.setMessageText(&NSString::from_str(title));
        alert.setInformativeText(&NSString::from_str(detail));
        for button in buttons {
            alert.addButtonWithTitle(&NSString::from_str(button));
        }
        #[allow(deprecated)]
        NSApplication::sharedApplication(mtm).activateIgnoringOtherApps(true);
        // First button returns 1000 (NSAlertFirstButtonReturn), then 1001, ...
        (alert.runModal() - 1000).max(0) as usize
    }

    /// Panel line: installed version and when the last check succeeded.
    pub(super) fn update_status_line(&self) -> String {
        let current = current_version();
        let last = self.ivars().settings.borrow().last_update_check;
        let when = match last {
            None => "尚未检查".to_string(),
            Some(last) => match unix_now().saturating_sub(last) / 86_400 {
                0 => "上次检查：今天".to_string(),
                days => format!("上次检查：{days} 天前"),
            },
        };
        match self.ivars().available_update.borrow().as_ref() {
            Some((version, _)) => format!("当前 {} · 有新版本 {version}", current.as_str()),
            None => format!("当前 {} · {when}", current.as_str()),
        }
    }
}

fn open_url(url: &str) {
    if let Some(url) = NSURL::URLWithString(&NSString::from_str(url)) {
        NSWorkspace::sharedWorkspace().openURL(&url);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_list_parsing_skips_drafts_and_incomplete_entries() {
        let json = br#"[
            {"tag_name": "v1.0-beta.3", "prerelease": true, "draft": false, "html_url": "https://x/3"},
            {"tag_name": "v9.9", "prerelease": false, "draft": true, "html_url": "https://x/draft"},
            {"tag_name": "v1.0", "prerelease": false, "draft": false, "html_url": "https://x/1"},
            {"prerelease": false, "draft": false, "html_url": "https://x/no-tag"}
        ]"#;
        let releases = parse_releases(json).unwrap();
        assert_eq!(
            releases,
            vec![
                Release {
                    tag: "v1.0-beta.3".into(),
                    prerelease: true,
                    url: "https://x/3".into()
                },
                Release {
                    tag: "v1.0".into(),
                    prerelease: false,
                    url: "https://x/1".into()
                },
            ]
        );
        assert!(parse_releases(b"{\"message\": \"rate limited\"}").is_err());
        assert!(parse_releases(b"not json").is_err());
    }

    #[test]
    fn package_version_parses() {
        assert!(current_version()
            .as_str()
            .starts_with(env!("CARGO_PKG_VERSION")));
    }
}
