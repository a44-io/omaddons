//! The clock M3's time rules read: the doorbell's `at`, and later automatic-backup days,
//! `captured_at`, error ages and `acked_at`. In debug builds `OMADDONS_NOW` (RFC 3339)
//! freezes it, so tests can pin time (DEC-7: test hooks exist in debug builds only);
//! release builds ignore it, and then [`now`] is exactly `Timestamp::now()`.

use jiff::Timestamp;

/// Now, or the debug build's frozen `OMADDONS_NOW`.
pub fn now() -> Timestamp {
    frozen(std::env::var_os("OMADDONS_NOW"), cfg!(debug_assertions)).unwrap_or_else(Timestamp::now)
}

/// The time `OMADDONS_NOW` freezes the clock at, in a debug build. Empty or not RFC 3339
/// freezes nothing.
fn frozen(var: Option<std::ffi::OsString>, debug: bool) -> Option<Timestamp> {
    var.filter(|_| debug)?.to_str()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn var(value: &str) -> Option<std::ffi::OsString> {
        Some(value.into())
    }

    #[test]
    fn omaddons_now_freezes_debug_builds_only() {
        let at: Timestamp = "2026-10-21T12:00:00Z".parse().unwrap();
        assert_eq!(frozen(var("2026-10-21T12:00:00Z"), true), Some(at));
        assert_eq!(
            frozen(var("2026-10-21T14:00:00+02:00"), true),
            Some(at),
            "any RFC 3339 offset"
        );
        assert_eq!(
            frozen(var("2026-10-21T12:00:00Z"), false),
            None,
            "release builds ignore it"
        );
        assert_eq!(frozen(None, true), None);
        assert_eq!(frozen(var(""), true), None, "empty");
        assert_eq!(frozen(var("yesterday"), true), None, "not RFC 3339");
    }

    #[test]
    fn unfrozen_now_is_the_real_clock() {
        if std::env::var_os("OMADDONS_NOW").is_some() {
            return;
        }
        let before = Timestamp::now();
        let now = now();
        assert!(before <= now && now <= Timestamp::now());
    }
}
