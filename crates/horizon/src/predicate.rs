//! Claimable-balance predicates, as Horizon renders them.
//!
//! CAP-23 stores these on the ledger and evaluates them at claim time. Horizon
//! returns the same tree in JSON, with both the historical camelCase keys and
//! the later snake_case ones. We accept either.
//!
//! Evaluation is fail-closed: a tree deeper than 4 (the protocol maximum), an
//! `and`/`or` that is not a pair, or a relative deadline we cannot place in
//! time, is not claimable. That is the side that costs the sponsor money if we
//! get it wrong — we would otherwise submit a transaction that is guaranteed
//! to fail and still consume a sequence number.

use serde::Deserialize;

/// One node of a CAP-23 predicate.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct Predicate {
    #[serde(default)]
    pub unconditional: Option<bool>,
    #[serde(default)]
    pub and: Option<Vec<Predicate>>,
    #[serde(default)]
    pub or: Option<Vec<Predicate>>,
    #[serde(default)]
    pub not: Option<Box<Predicate>>,
    #[serde(default, alias = "absBefore")]
    pub abs_before: Option<String>,
    #[serde(default, alias = "absBeforeEpoch")]
    pub abs_before_epoch: Option<String>,
    #[serde(default, alias = "relBefore")]
    pub rel_before: Option<String>,
}

impl Predicate {
    /// `true` if this clause holds at unix time `now`.
    ///
    /// `created` is the close time of the ledger that created the balance,
    /// needed only for `rel_before`. Horizon exposes it as `last_modified_time`
    /// on a balance that has not been touched since creation.
    pub fn holds(&self, now: i64, created: Option<i64>) -> bool {
        self.holds_at(now, created, 0)
    }

    fn holds_at(&self, now: i64, created: Option<i64>, depth: u8) -> bool {
        // CAP-23 rejects depth > 4 at creation. Anything deeper here is not a
        // real on-chain predicate; refuse rather than recurse forever.
        if depth > 4 {
            return false;
        }
        if let Some(parts) = &self.and {
            if parts.len() != 2 {
                return false;
            }
            return parts[0].holds_at(now, created, depth + 1)
                && parts[1].holds_at(now, created, depth + 1);
        }
        if let Some(parts) = &self.or {
            if parts.len() != 2 {
                return false;
            }
            return parts[0].holds_at(now, created, depth + 1)
                || parts[1].holds_at(now, created, depth + 1);
        }
        if let Some(inner) = &self.not {
            return !inner.holds_at(now, created, depth + 1);
        }
        if let Some(flag) = self.unconditional {
            return flag;
        }
        if let Some(epoch) = self
            .abs_before_epoch
            .as_deref()
            .and_then(|s| s.parse::<i64>().ok())
        {
            return now < epoch;
        }
        if let Some(stamp) = self.abs_before.as_deref().and_then(parse_rfc3339_utc) {
            return now < stamp;
        }
        if let Some(rel) = self
            .rel_before
            .as_deref()
            .and_then(|s| s.parse::<i64>().ok())
        {
            return created.is_some_and(|c| now < c.saturating_add(rel));
        }
        // Horizon and the SDKs render "always" as `{}` or `{unconditional:true}`.
        true
    }
}

/// Horizon's `last_modified_time` / `absBefore`: `2020-02-26T19:29:16Z`.
pub fn parse_rfc3339_utc(s: &str) -> Option<i64> {
    let s = s.trim();
    let s = s.strip_suffix('Z').or_else(|| s.strip_suffix("+00:00"))?;
    let (date, time) = s.split_once('T')?;
    let mut d = date.split('-');
    let year: i32 = d.next()?.parse().ok()?;
    let month: u32 = d.next()?.parse().ok()?;
    let day: u32 = d.next()?.parse().ok()?;
    let (hms, _) = time
        .split_once('.')
        .map(|(h, f)| (h, Some(f)))
        .unwrap_or((time, None));
    let mut t = hms.split(':');
    let hour: u32 = t.next()?.parse().ok()?;
    let minute: u32 = t.next()?.parse().ok()?;
    let second: u32 = t.next()?.parse().ok()?;
    unix_from_civil(year, month, day, hour, minute, second)
}

/// Days-from-civil (Howard Hinnant) plus the time of day, as unix seconds.
fn unix_from_civil(y: i32, m: u32, d: u32, h: u32, min: u32, s: u32) -> Option<i64> {
    if !(1..=12).contains(&m) || d == 0 || d > 31 || h > 23 || min > 59 || s > 60 {
        return None;
    }
    let y = y as i64;
    let m = m as i64;
    let d = d as i64;
    let (y, m) = if m <= 2 { (y - 1, m + 9) } else { (y, m - 3) };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let doy = (153 * m + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + i64::from(h) * 3_600 + i64::from(min) * 60 + i64::from(s))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pred(json: &str) -> Predicate {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn epoch_zero_is_the_unix_epoch() {
        assert_eq!(parse_rfc3339_utc("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(
            parse_rfc3339_utc("2020-02-26T19:29:16Z"),
            Some(1_582_745_356)
        );
    }

    #[test]
    fn unconditional_and_empty_are_always_true() {
        assert!(pred(r#"{"unconditional":true}"#).holds(0, None));
        assert!(pred("{}").holds(0, None));
        assert!(!pred(r#"{"unconditional":false}"#).holds(0, None));
    }

    #[test]
    fn abs_before_uses_the_epoch_when_horizon_sends_it() {
        let p = pred(r#"{"absBefore":"2020-08-26T11:15:39Z","absBeforeEpoch":"1598440539"}"#);
        assert!(p.holds(1_598_440_538, None));
        assert!(!p.holds(1_598_440_539, None));
    }

    #[test]
    fn abs_before_falls_back_to_the_iso_string() {
        let p = pred(r#"{"abs_before":"2020-08-26T11:15:39Z"}"#);
        assert!(p.holds(1_598_440_538, None));
        assert!(!p.holds(1_598_440_539, None));
    }

    #[test]
    fn rel_before_needs_a_creation_time() {
        let p = pred(r#"{"relBefore":"12"}"#);
        assert!(!p.holds(100, None));
        assert!(p.holds(111, Some(100)));
        assert!(!p.holds(112, Some(100)));
    }

    #[test]
    fn not_before_relative_is_the_vesting_shape() {
        // "not claimable until 2 hours after creation" — the griefing case:
        // the balance exists and names the user, but ClaimClaimableBalance
        // fails until the window opens.
        let p = pred(r#"{"not":{"rel_before":"7200"}}"#);
        assert!(!p.holds(100 + 7_199, Some(100)));
        assert!(p.holds(100 + 7_200, Some(100)));
    }

    #[test]
    fn and_and_or_require_exactly_two_children() {
        assert!(!pred(r#"{"and":[{"unconditional":true}]}"#).holds(0, None));
        assert!(pred(r#"{"and":[{"unconditional":true},{"unconditional":true}]}"#).holds(0, None));
        assert!(
            !pred(r#"{"and":[{"unconditional":true},{"unconditional":false}]}"#).holds(0, None)
        );
        assert!(pred(r#"{"or":[{"unconditional":false},{"unconditional":true}]}"#).holds(0, None));
    }

    #[test]
    fn horizon_example_tree_is_never_claimable() {
        // The docs page uses `not: {unconditional:true}` as the second arm of
        // an `and`, which is always false. Useful as a parser smoke test.
        let p = pred(
            r#"{
              "and": [
                {"or":[{"relBefore":"12"},{"absBeforeEpoch":"1598440539"}]},
                {"not":{"unconditional":true}}
              ]
            }"#,
        );
        assert!(!p.holds(0, Some(0)));
    }

    #[test]
    fn depth_past_four_is_refused() {
        // A nest of `and`s that would be true if evaluated, and is not once
        // we refuse to walk past the protocol's depth cap.
        let mut json = String::from(r#"{"unconditional":true}"#);
        for _ in 0..6 {
            json = format!(r#"{{"and":[{json},{{"unconditional":true}}]}}"#);
        }
        assert!(!pred(&json).holds(0, None));
        assert!(pred(r#"{"and":[{"unconditional":true},{"unconditional":true}]}"#).holds(0, None));
    }
}
