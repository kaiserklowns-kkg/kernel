//! What Settings' Privacy page shows (ADR-0156), host-tested: when each
//! app last started holding each permission, as Core keeps it in
//! `/system/permissions-used`, and the lines Core's `PRIVACY` answers.

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use core::fmt::Write;

use crate::Permission;

/// When each app last started holding each permission: seconds since 1970
/// (UTC). Kept as lines `APP PERMISSION SECONDS`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Used {
    last: BTreeMap<(String, Permission), u64>,
}

impl Used {
    /// Reads the kept lines. A line that does not read is skipped: what is
    /// lost is only a time shown, never a decision.
    pub fn parse(text: &str) -> Self {
        let mut used = Self::default();
        for line in text.lines() {
            let mut words = line.split_whitespace();
            if let (Some(app), Some(permission), Some(seconds), None) =
                (words.next(), words.next(), words.next(), words.next())
                && let Some(permission) = Permission::from_name(permission)
                && let Ok(seconds) = seconds.parse::<u64>()
            {
                used.last.insert((app.to_string(), permission), seconds);
            }
        }
        used
    }

    /// The lines to keep.
    pub fn to_text(&self) -> String {
        let mut text = String::new();
        for ((app, permission), seconds) in &self.last {
            let _ = writeln!(text, "{app} {} {seconds}", permission.name());
        }
        text
    }

    /// `app` started holding `permission` at `seconds`.
    pub fn note(&mut self, app: &str, permission: Permission, seconds: u64) {
        self.last.insert((app.to_string(), permission), seconds);
    }

    /// When `app` last started holding `permission`, if it ever did.
    pub fn last(&self, app: &str, permission: Permission) -> Option<u64> {
        self.last.get(&(app.to_string(), permission)).copied()
    }

    /// An app removed: its times go with it.
    pub fn forget(&mut self, app: &str) {
        self.last.retain(|(id, _), _| id != app);
    }
}

/// One line of Core's `PRIVACY` answer: an app, a permission it asks for,
/// the decision (`oceans_core_proto::Decision` as a number), and when it
/// last started holding it (0: never).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry<'a> {
    pub permission: Permission,
    pub decision: u8,
    pub last: u64,
    pub app: &'a str,
}

impl<'a> Entry<'a> {
    /// `PERMISSION\tDECISION\tSECONDS\tAPP\n`.
    pub fn write_to(&self, out: &mut String) {
        let _ = writeln!(
            out,
            "{}\t{}\t{}\t{}",
            self.permission.name(),
            self.decision,
            self.last,
            self.app
        );
    }

    /// One line, without its line break.
    pub fn parse(line: &'a str) -> Option<Self> {
        let mut parts = line.split('\t');
        let entry = Self {
            permission: Permission::from_name(parts.next()?)?,
            decision: parts.next()?.parse().ok()?,
            last: parts.next()?.parse().ok()?,
            app: parts.next().filter(|app| !app.is_empty())?,
        };
        parts.next().is_none().then_some(entry)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn used_times_are_kept_and_read_back() {
        let mut used = Used::default();
        used.note("app.oceans.hello", Permission::Network, 1_760_500_000);
        used.note("app.oceans.hello", Permission::Files, 1_760_400_000);
        used.note("com.example.radio", Permission::Network, 1_760_600_000);
        // A later start replaces the time.
        used.note("app.oceans.hello", Permission::Network, 1_760_500_100);
        let read = Used::parse(&used.to_text());
        assert_eq!(read, used);
        assert_eq!(
            read.last("app.oceans.hello", Permission::Network),
            Some(1_760_500_100)
        );
        assert_eq!(read.last("app.oceans.hello", Permission::Pointer), None);
    }

    #[test]
    fn damaged_lines_are_skipped_and_removed_apps_forgotten() {
        let mut used = Used::parse(
            "app.a network 100\nnonsense\napp.b wings 5\napp.c files x\napp.d files 7 extra\napp.b files 9\n",
        );
        assert_eq!(used.last("app.a", Permission::Network), Some(100));
        assert_eq!(used.last("app.b", Permission::Files), Some(9));
        assert_eq!(used.last("app.d", Permission::Files), None);
        used.forget("app.a");
        assert_eq!(used.last("app.a", Permission::Network), None);
        assert_eq!(used.last("app.b", Permission::Files), Some(9));
    }

    #[test]
    fn privacy_lines_round_trip() {
        let entry = Entry {
            permission: Permission::Network,
            decision: 1,
            last: 1_760_500_000,
            app: "app.oceans.hello",
        };
        let mut text = String::new();
        entry.write_to(&mut text);
        assert_eq!(text, "network\t1\t1760500000\tapp.oceans.hello\n");
        assert_eq!(Entry::parse(text.trim_end()), Some(entry));
        // Wrong or missing parts are refused, not guessed.
        assert_eq!(Entry::parse("network\t1\t5"), None);
        assert_eq!(Entry::parse("wings\t1\t5\tapp.x"), None);
        assert_eq!(Entry::parse("network\t1\t5\tapp.x\tmore"), None);
        assert_eq!(Entry::parse("network\tx\t5\tapp.x"), None);
    }
}
