//! The users' accounts (ADR-0110, ADR-0115): each a name, and a password
//! kept only as an Argon2id hash (RFC 9106) with its salt and parameters;
//! and the wait after wrong passwords.
//!
//! Oceans Core keeps the [`Users`] in `/system/user` and checks passwords
//! for the lock screen; nothing else ever sees a hash.

#![no_std]

extern crate alloc;

pub mod controls;

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;

use argon2::{Algorithm, Argon2, Version};

/// Argon2id's costs: memory in KiB, passes, lanes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Params {
    pub memory_kib: u32,
    pub passes: u32,
    pub lanes: u32,
}

/// What new passwords are hashed with: 7 MiB and five passes, one of the
/// settings OWASP gives as equal to 19 MiB and two (the machine may have
/// little memory free). Kept with each hash, so it can grow later.
pub const DEFAULT: Params = Params {
    memory_kib: 7168,
    passes: 5,
    lanes: 1,
};

pub const SALT_LEN: usize = 16;
pub const HASH_LEN: usize = 32;
/// Passwords: 4 to 64 bytes, no control characters.
pub const MIN_PASSWORD: usize = 4;
pub const MAX_PASSWORD: usize = 64;
/// Names: 1 to 32 bytes, no control characters.
pub const MAX_NAME: usize = 32;

/// A password, as kept: never the password itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Secret {
    pub params: Params,
    pub salt: [u8; SALT_LEN],
    pub hash: [u8; HASH_LEN],
}

impl Secret {
    /// `password` hashed with a new `salt` (from the system's randomness).
    pub fn new(password: &str, salt: [u8; SALT_LEN], params: Params) -> Option<Self> {
        let hash = hash(password.as_bytes(), &salt, params)?;
        Some(Self { params, salt, hash })
    }

    /// Whether `password` is the one kept, compared in constant time.
    pub fn verify(&self, password: &str) -> bool {
        let Some(hash) = hash(password.as_bytes(), &self.salt, self.params) else {
            return false;
        };
        hash.iter()
            .zip(&self.hash)
            .fold(0u8, |diff, (a, b)| diff | (a ^ b))
            == 0
    }
}

/// Argon2id of `password` with `salt`; `None` if the parameters are out of
/// Argon2's bounds or memory runs out.
pub fn hash(password: &[u8], salt: &[u8], params: Params) -> Option<[u8; HASH_LEN]> {
    let params = argon2::Params::new(
        params.memory_kib,
        params.passes,
        params.lanes,
        Some(HASH_LEN),
    )
    .ok()?;
    let mut out = [0u8; HASH_LEN];
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into(password, salt, &mut out)
        .ok()?;
    Some(out)
}

fn printable(text: &str) -> bool {
    !text.chars().any(char::is_control)
}

pub fn valid_password(password: &str) -> bool {
    (MIN_PASSWORD..=MAX_PASSWORD).contains(&password.len()) && printable(password)
}

pub fn valid_name(name: &str) -> bool {
    (1..=MAX_NAME).contains(&name.len()) && printable(name) && name.trim() == name
}

/// The account, as Core keeps it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    /// Its short name (ADR-0115): [`OWNER`] for the first account, which
    /// administers the others; else a login of [`valid_id`].
    pub id: String,
    pub name: String,
    /// `None`: no password; the screen is never locked.
    pub secret: Option<Secret>,
    /// Minutes without input before the screen locks (ADR-0112), one of
    /// [`LOCK_CHOICES`]; 0: never. Only with a password.
    pub lock_after: u8,
}

/// The name of an account no one has named.
pub const DEFAULT_NAME: &str = "Oceans user";

/// The choices for locking after a while (ADR-0112), in minutes; 0 is
/// never.
pub const LOCK_CHOICES: [u8; 8] = [0, 1, 2, 5, 10, 15, 30, 60];
/// Until the user chooses: ten minutes, as other systems roughly do.
pub const DEFAULT_LOCK_AFTER: u8 = 10;

impl Default for Record {
    fn default() -> Self {
        Self {
            id: String::from(OWNER),
            name: String::from(DEFAULT_NAME),
            secret: None,
            lock_after: DEFAULT_LOCK_AFTER,
        }
    }
}

/// Whether the screen locks now (ADR-0112): a password is set, the user
/// chose a time, and there has been no input for that long.
pub fn lock_due(password_set: bool, lock_after: u8, last_input_ms: u64, now_ms: u64) -> bool {
    password_set
        && lock_after != 0
        && now_ms.saturating_sub(last_input_ms) >= u64::from(lock_after) * 60_000
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::new();
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

fn unhex<const N: usize>(text: &str) -> Option<[u8; N]> {
    if text.len() != 2 * N {
        return None;
    }
    let mut out = [0u8; N];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(text.get(2 * i..2 * i + 2)?, 16).ok()?;
    }
    Some(out)
}

impl Record {
    /// The text kept in `/system/user`:
    /// `name NAME` and `password argon2id m=KIB t=PASSES p=LANES SALT HASH`.
    pub fn encode(&self) -> String {
        let mut out = alloc::format!("name {}\n", self.name);
        if self.lock_after != DEFAULT_LOCK_AFTER {
            let _ = writeln!(out, "lock-after {}", self.lock_after);
        }
        if let Some(s) = &self.secret {
            let _ = writeln!(
                out,
                "password argon2id m={} t={} p={} {} {}",
                s.params.memory_kib,
                s.params.passes,
                s.params.lanes,
                hex(&s.salt),
                hex(&s.hash)
            );
        }
        out
    }

    /// From [`Record::encode`]'s text; `None` if it is damaged. A damaged
    /// password must not read as no password: Core then keeps the screen
    /// locked rather than open it.
    pub fn decode(text: &str) -> Option<Self> {
        let mut record = Self::default();
        for line in text.lines() {
            if let Some(name) = line.strip_prefix("name ") {
                if !valid_name(name) {
                    return None;
                }
                record.name = String::from(name);
            } else if let Some(rest) = line.strip_prefix("password ") {
                record.secret = Some(secret_of(rest)?);
            } else if let Some(minutes) = line.strip_prefix("lock-after ") {
                let minutes = minutes.parse::<u8>().ok()?;
                if !LOCK_CHOICES.contains(&minutes) {
                    return None;
                }
                record.lock_after = minutes;
            } else if !line.trim().is_empty() {
                return None;
            }
        }
        Some(record)
    }
}

/// The first account's id (ADR-0115): the one there always is, whose Home
/// is `/home` as before there were others.
pub const OWNER: &str = "owner";
/// The most accounts, the first included.
pub const MAX_USERS: usize = 8;
/// Logins: 1 to 16 of `a`–`z` and `0`–`9`, a letter first.
pub const MAX_ID: usize = 16;

/// Whether `id` may name an account added after the first: it becomes a
/// folder's name, so only lowercase letters and digits.
pub fn valid_id(id: &str) -> bool {
    (1..=MAX_ID).contains(&id.len())
        && id.starts_with(|c: char| c.is_ascii_lowercase())
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && id != OWNER
}

impl Record {
    /// Whether it administers the others: adds and removes accounts.
    pub fn admin(&self) -> bool {
        self.id == OWNER
    }

    /// Its Home, from the root: `home` for the first account, else
    /// `users/ID`.
    pub fn home(&self) -> String {
        if self.admin() {
            String::from("home")
        } else {
            alloc::format!("users/{}", self.id)
        }
    }
}

/// Every account (ADR-0115), the first account first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Users {
    pub accounts: Vec<Record>,
}

impl Default for Users {
    fn default() -> Self {
        Self {
            accounts: alloc::vec![Record::default()],
        }
    }
}

impl Users {
    pub fn get(&self, id: &str) -> Option<&Record> {
        self.accounts.iter().find(|a| a.id == id)
    }

    pub fn index(&self, id: &str) -> Option<usize> {
        self.accounts.iter().position(|a| a.id == id)
    }

    /// The first account's lines as before (an older Oceans reads them),
    /// then each other's after its `user ID`.
    pub fn encode(&self) -> String {
        let mut out = String::new();
        for account in &self.accounts {
            if !account.admin() {
                let _ = writeln!(out, "user {}", account.id);
            }
            out.push_str(&account.encode());
        }
        out
    }

    /// From [`Users::encode`]'s text (or one account's, as kept before
    /// ADR-0115); `None` if any of it is damaged.
    pub fn decode(text: &str) -> Option<Self> {
        let mut blocks: Vec<(String, String)> = alloc::vec![(String::from(OWNER), String::new())];
        for line in text.lines() {
            if let Some(id) = line.strip_prefix("user ") {
                if !valid_id(id) || blocks.iter().any(|(other, _)| other == id) {
                    return None;
                }
                blocks.push((String::from(id), String::new()));
            } else if let Some((_, block)) = blocks.last_mut() {
                block.push_str(line);
                block.push('\n');
            }
        }
        if blocks.len() > MAX_USERS {
            return None;
        }
        let accounts = blocks
            .into_iter()
            .map(|(id, block)| {
                Some(Record {
                    id,
                    ..Record::decode(&block)?
                })
            })
            .collect::<Option<Vec<_>>>()?;
        Some(Self { accounts })
    }
}

fn secret_of(text: &str) -> Option<Secret> {
    let words: Vec<&str> = text.split(' ').collect();
    let [kind, m, t, p, salt, hash] = words.as_slice() else {
        return None;
    };
    if *kind != "argon2id" {
        return None;
    }
    let number = |word: &str, key: &str| word.strip_prefix(key)?.parse::<u32>().ok();
    Some(Secret {
        params: Params {
            memory_kib: number(m, "m=")?,
            passes: number(t, "t=")?,
            lanes: number(p, "p=")?,
        },
        salt: unhex(salt)?,
        hash: unhex(hash)?,
    })
}

/// Wrong passwords tried in a row before the first wait.
pub const FREE_TRIES: u32 = 4;
/// The first wait, in milliseconds; each further wrong one doubles it, up
/// to [`MAX_WAIT_MS`].
pub const FIRST_WAIT_MS: u64 = 30_000;
pub const MAX_WAIT_MS: u64 = 300_000;

/// The wait after wrong passwords: a few tries free, then a longer wait
/// after each wrong one, as other systems do.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Throttle {
    failures: u32,
    until_ms: u64,
}

impl Throttle {
    /// `Err(ms)` while a try must wait that much longer.
    pub fn check(&self, now_ms: u64) -> Result<(), u64> {
        if now_ms < self.until_ms {
            Err(self.until_ms - now_ms)
        } else {
            Ok(())
        }
    }

    /// A wrong password at `now_ms`.
    pub fn failed(&mut self, now_ms: u64) {
        self.failures = self.failures.saturating_add(1);
        if self.failures > FREE_TRIES {
            let doublings = (self.failures - FREE_TRIES - 1).min(16);
            let wait = (FIRST_WAIT_MS << doublings).min(MAX_WAIT_MS);
            self.until_ms = now_ms + wait;
        }
    }

    /// The right password: the count starts again.
    pub fn succeeded(&mut self) {
        *self = Self::default();
    }

    pub fn failures(&self) -> u32 {
        self.failures
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Small costs: the tests check the logic, not the strength.
    const QUICK: Params = Params {
        memory_kib: 64,
        passes: 1,
        lanes: 1,
    };

    #[test]
    fn a_password_is_checked_against_its_hash_only() {
        let secret = Secret::new("sea shells", [7; SALT_LEN], QUICK).unwrap();
        assert!(secret.verify("sea shells"));
        assert!(!secret.verify("sea shell"));
        assert!(!secret.verify(""));
        // Another salt, another hash.
        let other = Secret::new("sea shells", [8; SALT_LEN], QUICK).unwrap();
        assert_ne!(secret.hash, other.hash);
        assert_eq!(
            hash(b"x", &[0; 4], QUICK),
            None,
            "a salt of 4 bytes is too short"
        );
    }

    #[test]
    fn records_round_trip_and_refuse_damage() {
        let secret = Secret::new("oceans123", [1; SALT_LEN], QUICK).unwrap();
        let record = Record {
            id: String::from(OWNER),
            name: String::from("Kai"),
            secret: Some(secret),
            lock_after: DEFAULT_LOCK_AFTER,
        };
        let text = record.encode();
        assert!(text.starts_with("name Kai\npassword argon2id m=64 t=1 p=1 0101"));
        assert_eq!(Record::decode(&text), Some(record.clone()));
        assert_eq!(Record::decode(""), Some(Record::default()));
        // Another time to lock after: kept; one not among the choices: damage.
        let five = Record {
            lock_after: 5,
            ..record
        };
        assert!(five.encode().contains("\nlock-after 5\n"));
        assert_eq!(Record::decode(&five.encode()), Some(five));
        assert_eq!(Record::decode("lock-after 7\n"), None);
        assert_eq!(
            Record::decode("name Kai\npassword argon2id m=64 t=1 p=1 zz yy\n"),
            None
        );
        assert_eq!(Record::decode("password scrypt a b c d e\n"), None);
        assert_eq!(Record::decode("what is this\n"), None);
        assert_eq!(Record::decode("name \n"), None);
    }

    #[test]
    fn several_accounts_are_kept_after_the_first() {
        let secret = Secret::new("oceans123", [1; SALT_LEN], QUICK).unwrap();
        let mut users = Users::default();
        users.accounts[0].name = String::from("Kai");
        users.accounts.push(Record {
            id: String::from("kim"),
            name: String::from("Kim"),
            secret: Some(secret),
            lock_after: 5,
        });
        let text = users.encode();
        assert!(text.starts_with(
            "name Kai
user kim
name Kim
lock-after 5
password "
        ));
        assert_eq!(Users::decode(&text), Some(users.clone()));
        // The one account kept before ADR-0115 is the first.
        assert_eq!(
            Users::decode(
                "name Kai
"
            )
            .map(|u| u.accounts[0].clone()),
            Some(users.accounts[0].clone())
        );
        assert_eq!(Users::decode(""), Some(Users::default()));
        // Their Homes, and who administers.
        assert_eq!(users.accounts[0].home(), "home");
        assert_eq!(
            users.get("kim").map(Record::home).as_deref(),
            Some("users/kim")
        );
        assert!(users.accounts[0].admin() && !users.accounts[1].admin());
        assert_eq!(users.index("kim"), Some(1));
        // Damage anywhere is damage.
        assert_eq!(
            Users::decode(
                "user kim
user kim
"
            ),
            None
        );
        assert_eq!(
            Users::decode(
                "user Kim
"
            ),
            None
        );
        assert_eq!(
            Users::decode(
                "user kim
what
"
            ),
            None
        );
        let many: String = (0..MAX_USERS)
            .map(|i| {
                alloc::format!(
                    "user u{i}
"
                )
            })
            .collect();
        assert_eq!(Users::decode(&many), None, "too many");
    }

    #[test]
    fn logins_are_folder_names() {
        assert!(valid_id("kim") && valid_id("k2"));
        assert!(!valid_id("") && !valid_id("2k") && !valid_id("Kim"));
        assert!(!valid_id("../x") && !valid_id("a b") && !valid_id(OWNER));
        assert!(!valid_id(&"k".repeat(MAX_ID + 1)));
    }

    #[test]
    fn passwords_and_names_are_bounded() {
        assert!(valid_password("abcd"));
        assert!(!valid_password("abc"));
        assert!(!valid_password(&"a".repeat(65)));
        assert!(!valid_password("tab\there"));
        assert!(valid_password("รหัสผ่าน"));
        assert!(valid_name("Kai"));
        assert!(!valid_name(""));
        assert!(!valid_name(" Kai"));
        assert!(!valid_name(&"n".repeat(33)));
    }

    #[test]
    fn the_screen_locks_after_the_time_without_input() {
        let minute = 60_000;
        assert!(!lock_due(true, 5, 0, 5 * minute - 1));
        assert!(lock_due(true, 5, 0, 5 * minute));
        assert!(lock_due(true, 1, 10 * minute, 11 * minute));
        assert!(!lock_due(true, 0, 0, 1000 * minute), "never");
        assert!(
            !lock_due(false, 1, 0, 1000 * minute),
            "no password, no lock"
        );
        assert!(!lock_due(true, 1, 5 * minute, 0), "input after now");
    }

    #[test]
    fn wrong_passwords_bring_longer_waits() {
        let mut t = Throttle::default();
        for _ in 0..FREE_TRIES {
            t.failed(1000);
            assert_eq!(t.check(1000), Ok(()));
        }
        t.failed(1000);
        assert_eq!(t.check(1000), Err(30_000));
        assert_eq!(t.check(31_000), Ok(()));
        t.failed(31_000);
        assert_eq!(t.check(31_000), Err(60_000));
        for _ in 0..20 {
            t.failed(100_000);
        }
        assert_eq!(t.check(100_000), Err(MAX_WAIT_MS));
        t.succeeded();
        assert_eq!((t.failures(), t.check(0)), (0, Ok(())));
    }
}
