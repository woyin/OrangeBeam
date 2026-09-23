//! Update checks: version comparison, release selection and scheduling.
//! Platform-independent; the app fetches the GitHub release list itself.
use std::cmp::Ordering;

/// How often the app looks for a new release. Checks only read the public
/// GitHub release list; nothing is downloaded or installed automatically.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateCheck {
    Off,
    Daily,
    Weekly,
    Monthly,
}

impl UpdateCheck {
    pub const ALL: [UpdateCheck; 4] = [
        UpdateCheck::Off,
        UpdateCheck::Daily,
        UpdateCheck::Weekly,
        UpdateCheck::Monthly,
    ];

    pub fn label(self) -> &'static str {
        match self {
            UpdateCheck::Off => "不自动检查",
            UpdateCheck::Daily => "每天",
            UpdateCheck::Weekly => "每周",
            UpdateCheck::Monthly => "每月",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            UpdateCheck::Off => "off",
            UpdateCheck::Daily => "daily",
            UpdateCheck::Weekly => "weekly",
            UpdateCheck::Monthly => "monthly",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|mode| mode.key() == key)
    }

    fn interval_secs(self) -> Option<u64> {
        const DAY: u64 = 24 * 60 * 60;
        match self {
            UpdateCheck::Off => None,
            UpdateCheck::Daily => Some(DAY),
            UpdateCheck::Weekly => Some(7 * DAY),
            UpdateCheck::Monthly => Some(30 * DAY),
        }
    }

    /// Due when never checked, or when the interval has passed. A clock that
    /// moved backwards past the last check also counts as due.
    pub fn is_due(self, last_check: Option<u64>, now: u64) -> bool {
        match (self.interval_secs(), last_check) {
            (None, _) => false,
            (Some(_), None) => true,
            (Some(interval), Some(last)) => now < last || now - last >= interval,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Ident {
    Number(u64),
    Text(String),
}

/// A release version such as `1.0`, `1.0.1`, `1.0-beta` or `1.0.0-beta.2`
/// (a leading `v` is ignored). Missing numeric parts count as zero, so
/// `1.0` equals `1.0.0`; any pre-release sorts before its release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    numbers: Vec<u64>,
    pre: Vec<Ident>,
    text: String,
}

impl Version {
    pub fn parse(text: &str) -> Option<Self> {
        let trimmed = text.trim();
        let bare = trimmed.strip_prefix('v').unwrap_or(trimmed);
        let bare = bare.split('+').next()?;
        let (core, pre) = match bare.split_once('-') {
            Some((core, pre)) => (core, Some(pre)),
            None => (bare, None),
        };
        let numbers = core
            .split('.')
            .map(|part| part.parse::<u64>().ok())
            .collect::<Option<Vec<_>>>()?;
        let pre = match pre {
            None => Vec::new(),
            Some("") => return None,
            Some(pre) => pre
                .split('.')
                .map(|id| match id.parse::<u64>() {
                    Ok(number) => Some(Ident::Number(number)),
                    Err(_) if !id.is_empty() => Some(Ident::Text(id.to_ascii_lowercase())),
                    Err(_) => None,
                })
                .collect::<Option<Vec<_>>>()?,
        };
        Some(Self {
            numbers,
            pre,
            text: bare.to_string(),
        })
    }

    pub fn is_prerelease(&self) -> bool {
        !self.pre.is_empty()
    }

    /// As written, without a leading `v`.
    pub fn as_str(&self) -> &str {
        &self.text
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        let width = self.numbers.len().max(other.numbers.len());
        let number = |v: &Version, i: usize| v.numbers.get(i).copied().unwrap_or(0);
        for i in 0..width {
            match number(self, i).cmp(&number(other, i)) {
                Ordering::Equal => {}
                unequal => return unequal,
            }
        }
        match (self.pre.is_empty(), other.pre.is_empty()) {
            (true, true) => return Ordering::Equal,
            (true, false) => return Ordering::Greater,
            (false, true) => return Ordering::Less,
            (false, false) => {}
        }
        for (a, b) in self.pre.iter().zip(&other.pre) {
            let order = match (a, b) {
                (Ident::Number(a), Ident::Number(b)) => a.cmp(b),
                (Ident::Number(_), Ident::Text(_)) => Ordering::Less,
                (Ident::Text(_), Ident::Number(_)) => Ordering::Greater,
                (Ident::Text(a), Ident::Text(b)) => a.cmp(b),
            };
            if order != Ordering::Equal {
                return order;
            }
        }
        self.pre.len().cmp(&other.pre.len())
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// A published (non-draft) release from the GitHub API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub tag: String,
    pub prerelease: bool,
    pub url: String,
}

/// The newest release above `current`, if any. Stable installs are offered
/// stable releases only; a pre-release install is also offered newer betas.
pub fn newer_release<'a>(
    current: &Version,
    releases: &'a [Release],
) -> Option<(Version, &'a Release)> {
    releases
        .iter()
        .filter(|release| current.is_prerelease() || !release.prerelease)
        .filter_map(|release| Some((Version::parse(&release.tag)?, release)))
        .filter(|(version, _)| version > current)
        .max_by(|(a, _), (b, _)| a.cmp(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    #[test]
    fn versions_order_like_semver_with_short_forms() {
        assert_eq!(v("v1.0").cmp(&v("1.0.0")), Ordering::Equal);
        assert!(v("1.0-beta") < v("1.0-beta.2"));
        assert!(v("1.0-beta.2") < v("1.0-beta.10"), "numeric identifiers");
        assert!(v("1.0-beta.2") < v("1.0-rc.1"));
        assert!(v("1.0-rc.1") < v("1.0"));
        assert!(v("1.0") < v("1.0.1"));
        assert!(v("0.2.0") < v("1.0-beta"));
        assert!(v("1.0.0-beta.2") > v("v1.0-beta"));
        assert_eq!(
            v("v1.0-beta.2+build.5").as_str(),
            "1.0-beta.2",
            "build metadata dropped"
        );
        for bad in ["", "v", "1..0", "1.x", "1.0-", "1.0-beta..2"] {
            assert!(Version::parse(bad).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn stable_installs_ignore_betas_and_betas_see_both() {
        let releases = vec![
            Release {
                tag: "v1.0-beta".into(),
                prerelease: true,
                url: "a".into(),
            },
            Release {
                tag: "v1.0-beta.3".into(),
                prerelease: true,
                url: "b".into(),
            },
            Release {
                tag: "v0.9".into(),
                prerelease: false,
                url: "c".into(),
            },
            Release {
                tag: "not-a-version".into(),
                prerelease: false,
                url: "d".into(),
            },
        ];
        let beta = v("1.0.0-beta.2");
        assert_eq!(
            newer_release(&beta, &releases).map(|(_, r)| r.url.as_str()),
            Some("b")
        );
        assert_eq!(
            newer_release(&v("0.8"), &releases).map(|(_, r)| r.url.as_str()),
            Some("c")
        );
        assert!(
            newer_release(&v("1.0-beta.3"), &releases).is_none(),
            "already newest"
        );
        let mut with_stable = releases.clone();
        with_stable.push(Release {
            tag: "v1.0".into(),
            prerelease: false,
            url: "e".into(),
        });
        assert_eq!(
            newer_release(&beta, &with_stable).map(|(_, r)| r.url.as_str()),
            Some("e")
        );
    }

    #[test]
    fn checks_are_due_by_interval_and_off_never_runs() {
        let week = 7 * 24 * 3600;
        assert!(!UpdateCheck::Off.is_due(None, 1_000));
        assert!(UpdateCheck::Weekly.is_due(None, 1_000), "never checked");
        assert!(!UpdateCheck::Weekly.is_due(Some(1_000), 1_000 + week - 1));
        assert!(UpdateCheck::Weekly.is_due(Some(1_000), 1_000 + week));
        assert!(UpdateCheck::Daily.is_due(Some(1_000), 1_000 + 24 * 3600));
        assert!(
            UpdateCheck::Monthly.is_due(Some(5_000), 4_000),
            "clock went backwards"
        );
        for mode in UpdateCheck::ALL {
            assert_eq!(UpdateCheck::from_key(mode.key()), Some(mode));
        }
    }
}
