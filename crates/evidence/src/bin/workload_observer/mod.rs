//! Parsing and identity logic; records never retain the process command name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Identity {
    pub pid: u32,
    pub start_jiffies: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stat {
    pub identity: Identity,
    pub exited: bool,
    pub ppid: u32,
    pub user_ticks: u64,
    pub system_ticks: u64,
}

pub fn parse_stat(text: &str) -> Option<Stat> {
    // comm is parenthesized but can itself contain spaces and parentheses.
    // All fields after its final ')' have the fixed kernel stat layout.
    let open = text.find('(')?;
    let close = text.rfind(')')?;
    if close <= open {
        return None;
    }
    let pid = text[..open].trim().parse::<u32>().ok()?;
    if pid == 0 {
        return None;
    }
    let fields: Vec<_> = text[close + 1..].split_whitespace().collect();
    if fields.first()?.len() != 1 {
        return None;
    }
    Some(Stat {
        identity: Identity {
            pid,
            start_jiffies: fields.get(19)?.parse().ok()?,
        },
        exited: matches!(*fields.first()?, "Z" | "X" | "x"),
        ppid: fields.get(1)?.parse().ok()?,
        user_ticks: fields.get(11)?.parse().ok()?,
        system_ticks: fields.get(12)?.parse().ok()?,
    })
}

pub fn normalized_sha256(value: &str) -> Option<String> {
    (value.len() == 64 && value.bytes().all(|c| c.is_ascii_hexdigit()))
        .then(|| value.to_ascii_lowercase())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RootState {
    Same,
    Exited,
    Reused,
}

pub fn root_state(expected: Identity, observed: Option<Identity>) -> RootState {
    match observed {
        None => RootState::Exited,
        Some(actual) if actual == expected => RootState::Same,
        Some(_) => RootState::Reused,
    }
}

pub fn valid_bounds(seconds: u64, interval_ms: u64) -> bool {
    (1..=600).contains(&seconds) && (25..=50).contains(&interval_ms)
}

/// Absolute elapsed time of the next sample, or None after the hard deadline.
/// Missed periods are skipped instead of causing a catch-up burst.
pub fn next_sample_at(
    now: std::time::Duration,
    prior_due: std::time::Duration,
    period: std::time::Duration,
    limit: std::time::Duration,
) -> Option<std::time::Duration> {
    if now >= limit {
        return None;
    }
    let due = prior_due.saturating_add(period);
    Some(
        if due <= now {
            now.saturating_add(period)
        } else {
            due
        }
        .min(limit),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn row(comm: &str, start: u64) -> String {
        format!("123 ({comm}) S 7 0 0 0 0 0 0 0 0 0 17 19 0 0 0 0 0 0 {start} 0")
    }
    #[test]
    fn stat_comm_spaces_nested_parentheses_and_newline() {
        let s = parse_stat(&row("a (tricky) rustc)\nname", 99)).unwrap();
        assert_eq!(
            s,
            Stat {
                identity: Identity {
                    pid: 123,
                    start_jiffies: 99
                },
                exited: false,
                ppid: 7,
                user_ticks: 17,
                system_ticks: 19
            }
        );
    }
    #[test]
    fn stat_truncation_and_bad_numeric_fields_rejected() {
        assert!(parse_stat("123 (rustc) S 7").is_none());
        assert!(parse_stat(&row("rustc", 3).replace("17 19", "no 19")).is_none());
        assert!(parse_stat("123 rustc S 7").is_none());
    }
    #[test]
    fn pid_reuse_is_distinct_and_stops_root_tracking() {
        let old = parse_stat(&row("cargo", 10)).unwrap().identity;
        let new = parse_stat(&row("cargo", 11)).unwrap().identity;
        assert_ne!(old, new);
        assert_eq!(root_state(old, Some(new)), RootState::Reused);
        assert_eq!(root_state(old, None), RootState::Exited);
        assert_eq!(root_state(old, Some(old)), RootState::Same);
    }
    #[test]
    fn bounded_configuration_and_declared_hash() {
        assert!(valid_bounds(600, 25));
        for bounds in [(0, 25), (601, 25), (10, 24), (10, 51)] {
            assert!(!valid_bounds(bounds.0, bounds.1));
        }
        assert_eq!(normalized_sha256(&"A".repeat(64)), Some("a".repeat(64)));
        assert!(normalized_sha256(&"z".repeat(64)).is_none());
        assert!(normalized_sha256("abc").is_none());
    }
    #[test]
    fn root_zombie_is_exited_before_pid_disappears() {
        assert!(
            parse_stat(&row("cargo", 10).replace(") S ", ") Z "))
                .unwrap()
                .exited
        );
        assert!(
            parse_stat(&row("cargo", 10).replace(") S ", ") X "))
                .unwrap()
                .exited
        );
    }
    #[test]
    fn scheduling_skips_missed_samples_and_caps_at_deadline() {
        use std::time::Duration as D;
        let ms = D::from_millis;
        assert_eq!(
            next_sample_at(ms(92), ms(25), ms(25), ms(600)),
            Some(ms(117))
        );
        assert_eq!(
            next_sample_at(ms(590), ms(575), ms(50), ms(600)),
            Some(ms(600))
        );
        assert_eq!(next_sample_at(ms(600), ms(575), ms(25), ms(600)), None);
    }
}
