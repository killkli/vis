//! Minimal local-time conversion for the history state notification.
//!
//! The native editor formats `:earlier`/`:later` notifications with
//! `localtime_r`. This module keeps the core dependency-free by reading a
//! POSIX `TZ` specification or a TZif zone file such as `/etc/localtime`.

use std::path::Path;

/// Format the `HH:MM` local wall clock of a Unix timestamp, like
/// `strftime("%H:%M", localtime_r(&state, &tm))`. Unparsable zones fall
/// back to UTC.
pub(crate) fn local_hm(epoch: i64) -> String {
    let offset = configured_zone().map_or(0, |zone| zone.offset(epoch));
    let seconds = (i128::from(epoch) + i128::from(offset)).rem_euclid(86_400);
    format!("{:02}:{:02}", seconds / 3_600, (seconds % 3_600) / 60)
}

#[derive(Debug)]
enum Zone {
    Posix(Posix),
    Tzif(Tzif),
}

impl Zone {
    fn offset(&self, epoch: i64) -> i32 {
        match self {
            Self::Posix(zone) => zone.offset(epoch),
            Self::Tzif(zone) => zone.offset(epoch),
        }
    }
}

fn configured_zone() -> Option<Zone> {
    match std::env::var("TZ") {
        Ok(value) => zone_from_tz(&value),
        Err(_) => std::fs::read("/etc/localtime")
            .ok()
            .and_then(|bytes| parse_tzif(&bytes))
            .map(Zone::Tzif),
    }
}

fn zone_from_tz(value: &str) -> Option<Zone> {
    let value = value.strip_prefix(':').unwrap_or(value);
    if value.is_empty() {
        return Some(Zone::Posix(Posix { std: 0, dst: None }));
    }
    if value.starts_with('/') || value.contains('/') {
        return read_zone_file(value)
            .and_then(|bytes| parse_tzif(&bytes))
            .map(Zone::Tzif);
    }
    parse_posix(value.as_bytes()).map(Zone::Posix)
}

fn read_zone_file(name: &str) -> Option<Vec<u8>> {
    if Path::new(name).is_absolute() {
        return std::fs::read(name).ok();
    }
    ["/usr/share/zoneinfo", "/etc/zoneinfo", "/usr/lib/zoneinfo"]
        .iter()
        .find_map(|root| std::fs::read(format!("{root}/{name}")).ok())
}

#[derive(Debug, Clone, Copy)]
struct Posix {
    /// Seconds east of UTC.
    std: i32,
    dst: Option<(i32, Rule, Rule)>,
}

impl Posix {
    fn offset(self, epoch: i64) -> i32 {
        let Some((dst, start, end)) = self.dst else {
            return self.std;
        };
        let year = civil_year(epoch);
        // POSIX transition times use the wall clock in effect before the
        // transition: standard time at spring-forward, daylight time at fall-back.
        let start_utc = rule_utc(start, year, self.std);
        let end_utc = rule_utc(end, year, dst);
        if start_utc <= end_utc {
            if epoch >= start_utc && epoch < end_utc {
                dst
            } else {
                self.std
            }
        } else if epoch >= start_utc || epoch < end_utc {
            // Southern-hemisphere daylight time spans the new year.
            dst
        } else {
            self.std
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Rule {
    date: RuleDate,
    /// Seconds after local midnight; POSIX permits values outside one day.
    time: i64,
}

#[derive(Debug, Clone, Copy)]
enum RuleDate {
    MonthWeekday { month: u32, week: u32, weekday: u32 },
    JulianNoLeap(u32),
    Julian(u32),
}

fn rule_utc(rule: Rule, year: i64, wall_offset: i32) -> i64 {
    let day = match rule.date {
        RuleDate::MonthWeekday {
            month,
            week,
            weekday,
        } => month_weekday(year, month, week, weekday),
        RuleDate::JulianNoLeap(day) => {
            let leap_day = is_leap_year(year) && day >= 60;
            i64::from(day - 1 + u32::from(leap_day))
        }
        RuleDate::Julian(day) => i64::from(day),
    };
    days_from_civil(year, 1, 1)
        .saturating_mul(86_400)
        .saturating_add(day.saturating_mul(86_400))
        .saturating_add(rule.time)
        .saturating_sub(i64::from(wall_offset))
}

fn month_weekday(year: i64, month: u32, week: u32, weekday: u32) -> i64 {
    let month = i64::from(month);
    let first_day = days_from_civil(year, month, 1);
    let first_weekday = (first_day + 4).rem_euclid(7); // 1970-01-01 was Thursday.
    let first_match = (i64::from(weekday) + 7 - first_weekday) % 7;
    let mut day = 1 + first_match + i64::from(week - 1) * 7;
    let next_month = if month == 12 { 1 } else { month + 1 };
    let next_year = if month == 12 { year + 1 } else { year };
    let month_days = days_from_civil(next_year, next_month, 1) - first_day;
    if day > month_days {
        day -= 7;
    }
    first_day + day - 1 - days_from_civil(year, 1, 1)
}

fn is_leap_year(year: i64) -> bool {
    year.rem_euclid(4) == 0 && (year.rem_euclid(100) != 0 || year.rem_euclid(400) == 0)
}

fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let adjusted_year = if month <= 2 { year - 1 } else { year };
    let era = if adjusted_year >= 0 {
        adjusted_year
    } else {
        adjusted_year - 399
    } / 400;
    let year_of_era = adjusted_year - era * 400;
    let shifted_month = if month > 2 { month - 3 } else { month + 9 };
    let day_of_year = (153 * shifted_month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

fn civil_year(epoch: i64) -> i64 {
    let z = epoch.div_euclid(86_400) + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_part = (5 * day_of_year + 2) / 153;
    year + i64::from(month_part >= 10)
}

/// Parse a POSIX `TZ` string, for example `CET-1CEST,M3.5.0,M10.5.0/3`.
/// POSIX offsets count hours west of Greenwich; stored offsets are seconds east.
fn parse_posix(bytes: &[u8]) -> Option<Posix> {
    let (_, rest) = split_name(bytes)?;
    let (standard_west, rest) = parse_hms(rest, 24)?;
    let standard = -standard_west;
    if rest.is_empty() {
        return Some(Posix {
            std: standard,
            dst: None,
        });
    }

    let (_, rest) = split_name(rest)?;
    let (daylight, rest) = if rest.is_empty() || rest.first() == Some(&b',') {
        (standard.checked_add(3_600)?, rest)
    } else {
        let (daylight_west, rest) = parse_hms(rest, 24)?;
        (-daylight_west, rest)
    };

    let (start, end) = if rest.is_empty() {
        (default_start_rule(), default_end_rule())
    } else {
        let rules = rest.strip_prefix(b",")?;
        let (start, rest) = parse_rule(rules)?;
        let (end, rest) = parse_rule(rest.strip_prefix(b",")?)?;
        if !rest.is_empty() {
            return None;
        }
        (start, end)
    };
    Some(Posix {
        std: standard,
        dst: Some((daylight, start, end)),
    })
}

fn default_start_rule() -> Rule {
    Rule {
        date: RuleDate::MonthWeekday {
            month: 3,
            week: 2,
            weekday: 0,
        },
        time: 2 * 3_600,
    }
}

fn default_end_rule() -> Rule {
    Rule {
        date: RuleDate::MonthWeekday {
            month: 11,
            week: 1,
            weekday: 0,
        },
        time: 2 * 3_600,
    }
}

fn split_name(bytes: &[u8]) -> Option<(&[u8], &[u8])> {
    if bytes.first() == Some(&b'<') {
        let end = bytes.iter().position(|byte| *byte == b'>')?;
        (end >= 4).then_some((&bytes[..=end], &bytes[end + 1..]))
    } else {
        let end = bytes
            .iter()
            .position(|byte| !byte.is_ascii_alphabetic())
            .unwrap_or(bytes.len());
        (end >= 3).then_some((&bytes[..end], &bytes[end..]))
    }
}

/// Parse signed `hh[:mm[:ss]]`; POSIX TZ offsets limit hours to 24, while
/// transition times may be signed and extend beyond a day.
fn parse_hms(bytes: &[u8], max_hours: u32) -> Option<(i32, &[u8])> {
    let (negative, rest) = match bytes.first() {
        Some(b'+') => (false, &bytes[1..]),
        Some(b'-') => (true, &bytes[1..]),
        _ => (false, bytes),
    };
    let (hours, rest) = parse_number(rest)?;
    if hours > max_hours {
        return None;
    }
    let mut seconds = i64::from(hours) * 3_600;
    let mut rest = rest;
    for factor in [60i64, 1] {
        if rest.first() == Some(&b':') {
            let (value, next) = parse_number(&rest[1..])?;
            if value > 59 {
                return None;
            }
            seconds += i64::from(value) * factor;
            rest = next;
        }
    }
    let seconds = i32::try_from(seconds).ok()?;
    Some((if negative { -seconds } else { seconds }, rest))
}

fn parse_number(bytes: &[u8]) -> Option<(u32, &[u8])> {
    let end = bytes
        .iter()
        .position(|byte| !byte.is_ascii_digit())
        .unwrap_or(bytes.len());
    if end == 0 {
        return None;
    }
    let mut value = 0u32;
    for byte in &bytes[..end] {
        value = value.checked_mul(10)?.checked_add(u32::from(byte - b'0'))?;
    }
    Some((value, &bytes[end..]))
}

fn parse_rule(bytes: &[u8]) -> Option<(Rule, &[u8])> {
    let (date, rest) = match bytes.first()? {
        b'M' => {
            let (month, rest) = parse_number(&bytes[1..])?;
            let (week, rest) = parse_number(rest.strip_prefix(b".")?)?;
            let (weekday, rest) = parse_number(rest.strip_prefix(b".")?)?;
            if !(1..=12).contains(&month) || !(1..=5).contains(&week) || weekday > 6 {
                return None;
            }
            (
                RuleDate::MonthWeekday {
                    month,
                    week,
                    weekday,
                },
                rest,
            )
        }
        b'J' => {
            let (day, rest) = parse_number(&bytes[1..])?;
            if !(1..=365).contains(&day) {
                return None;
            }
            (RuleDate::JulianNoLeap(day), rest)
        }
        b'0'..=b'9' => {
            let (day, rest) = parse_number(bytes)?;
            if day > 365 {
                return None;
            }
            (RuleDate::Julian(day), rest)
        }
        _ => return None,
    };
    let (time, rest) = if let Some(rest) = rest.strip_prefix(b"/") {
        parse_hms(rest, 167)?
    } else {
        (2 * 3_600, rest)
    };
    Some((
        Rule {
            date,
            time: i64::from(time),
        },
        rest,
    ))
}

#[derive(Debug)]
struct Tzif {
    transitions: Vec<i64>,
    indices: Vec<u8>,
    /// `(seconds east of UTC, is_dst)` for each local-time type.
    types: Vec<(i32, bool)>,
    footer: Option<Posix>,
}

impl Tzif {
    fn offset(&self, epoch: i64) -> i32 {
        let Some(last) = self.transitions.last().copied() else {
            return self.types.first().map_or(0, |(offset, _)| *offset);
        };
        if epoch > last {
            if let Some(rule) = self.footer {
                return rule.offset(epoch);
            }
        }
        let index = self.transitions.partition_point(|time| *time <= epoch);
        if index == 0 {
            self.types.first().map_or(0, |(offset, _)| *offset)
        } else {
            self.indices
                .get(index - 1)
                .and_then(|ty| self.types.get(usize::from(*ty)))
                .map_or(0, |(offset, _)| *offset)
        }
    }
}

fn parse_tzif(bytes: &[u8]) -> Option<Tzif> {
    let version = *bytes.get(4)?;
    let (first, rest) = parse_tzif_block(bytes, false)?;
    if matches!(version, b'2' | b'3' | b'4') {
        let (wide, rest) = parse_tzif_block(rest, true)?;
        Some(Tzif {
            transitions: wide.transitions,
            indices: wide.indices,
            types: wide.types,
            footer: parse_footer(rest),
        })
    } else if version == 0 {
        Some(Tzif {
            transitions: first.transitions,
            indices: first.indices,
            types: first.types,
            footer: None,
        })
    } else {
        None
    }
}

struct TzifBlock {
    transitions: Vec<i64>,
    indices: Vec<u8>,
    types: Vec<(i32, bool)>,
}

fn parse_tzif_block(bytes: &[u8], wide: bool) -> Option<(TzifBlock, &[u8])> {
    if bytes.get(..4)? != b"TZif" {
        return None;
    }
    let counts = bytes.get(20..44)?;
    let read_u32 =
        |slice: &[u8]| -> Option<u32> { Some(u32::from_be_bytes(slice.try_into().ok()?)) };
    let isutcnt = read_u32(&counts[0..4])?;
    let isstdcnt = read_u32(&counts[4..8])?;
    let leapcnt = read_u32(&counts[8..12])?;
    let timecnt = read_u32(&counts[12..16])?;
    let typecnt = read_u32(&counts[16..20])?;
    let charcnt = read_u32(&counts[20..24])?;
    if typecnt == 0 || typecnt > 256 || timecnt > 1_000_000 {
        return None;
    }
    let time_size = if wide { 8usize } else { 4 };
    let mut cursor = 44usize;
    let mut transitions = Vec::with_capacity(timecnt as usize);
    for _ in 0..timecnt {
        let raw = bytes.get(cursor..cursor.checked_add(time_size)?)?;
        let time = if wide {
            i64::from_be_bytes(raw.try_into().ok()?)
        } else {
            i64::from(i32::from_be_bytes(raw.try_into().ok()?))
        };
        transitions.push(time);
        cursor += time_size;
    }
    let index_count = usize::try_from(timecnt).ok()?;
    let indices = bytes.get(cursor..cursor.checked_add(index_count)?)?;
    cursor += index_count;
    let mut types = Vec::with_capacity(typecnt as usize);
    for _ in 0..typecnt {
        let raw = bytes.get(cursor..cursor.checked_add(6)?)?;
        let offset = i32::from_be_bytes(raw[..4].try_into().ok()?);
        if raw[4] > 1 {
            return None;
        }
        types.push((offset, raw[4] != 0));
        cursor += 6;
    }
    if indices.iter().any(|index| u32::from(*index) >= typecnt) {
        return None;
    }
    cursor = cursor.checked_add(usize::try_from(charcnt).ok()?)?;
    cursor = cursor.checked_add(
        usize::try_from(leapcnt)
            .ok()?
            .checked_mul(time_size.checked_add(4)?)?,
    )?;
    cursor = cursor.checked_add(usize::try_from(isstdcnt).ok()?)?;
    cursor = cursor.checked_add(usize::try_from(isutcnt).ok()?)?;
    Some((
        TzifBlock {
            transitions,
            indices: indices.to_vec(),
            types,
        },
        bytes.get(cursor..)?,
    ))
}

fn parse_footer(bytes: &[u8]) -> Option<Posix> {
    let rest = bytes.strip_prefix(b"\n")?;
    let end = rest.iter().position(|byte| *byte == b'\n')?;
    if end == 0 {
        return None;
    }
    parse_posix(&rest[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn epoch(year: i64, month: i64, day: i64, hour: i64, minute: i64) -> i64 {
        days_from_civil(year, month, day) * 86_400 + hour * 3_600 + minute * 60
    }

    #[test]
    fn fixed_offsets_render_known_clocks() {
        assert_eq!(local_hm_with_offset(1_700_000_000, 0), "22:13");
        assert_eq!(local_hm_with_offset(1_700_000_000, 9 * 3_600), "07:13");
        assert_eq!(local_hm_with_offset(1_700_000_000, -5 * 3_600), "17:13");
        assert_eq!(local_hm_with_offset(0, 86_400), "00:00");
    }

    #[test]
    fn posix_fixed_zone_signs_follow_posix_convention() {
        assert_eq!(
            parse_posix(b"JST-9").unwrap().offset(1_700_000_000),
            9 * 3_600
        );
        assert_eq!(
            parse_posix(b"EST5").unwrap().offset(1_700_000_000),
            -5 * 3_600
        );
        assert_eq!(parse_posix(b"UTC0").unwrap().offset(0), 0);
    }

    #[test]
    fn posix_northern_dst_switches_at_wall_time_boundaries() {
        let zone = parse_posix(b"EST5EDT,M3.2.0,M11.1.0").expect("parsed");
        assert_eq!(zone.offset(epoch(2023, 3, 12, 6, 59)), -5 * 3_600);
        assert_eq!(zone.offset(epoch(2023, 3, 12, 7, 0)), -4 * 3_600);
        assert_eq!(zone.offset(epoch(2023, 11, 5, 5, 59)), -4 * 3_600);
        assert_eq!(zone.offset(epoch(2023, 11, 5, 6, 0)), -5 * 3_600);
    }

    #[test]
    fn posix_southern_dst_spans_new_year() {
        let zone = parse_posix(b"AEST-10AEDT,M10.1.0,M4.1.0/3").expect("parsed");
        assert_eq!(zone.offset(epoch(2024, 1, 15, 0, 0)), 11 * 3_600);
        assert_eq!(zone.offset(epoch(2024, 7, 1, 12, 0)), 10 * 3_600);
    }

    #[test]
    fn posix_julian_rules_and_explicit_dst_offsets_parse() {
        let zone = parse_posix(b"<+03>-3<+02>-2,J1,J365").expect("parsed");
        assert_eq!(zone.std, 3 * 3_600);
        assert_eq!(zone.dst.unwrap().0, 2 * 3_600);
        assert_eq!(zone.offset(epoch(2024, 1, 15, 12, 0)), 2 * 3_600);
        assert_eq!(zone.offset(epoch(2024, 12, 31, 12, 0)), 3 * 3_600);
    }

    #[test]
    fn tzif_v1_uses_transition_offsets() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"TZif\0");
        bytes.extend_from_slice(&[0; 15]);
        for count in [0u32, 0, 0, 2, 2, 8] {
            bytes.extend_from_slice(&count.to_be_bytes());
        }
        for time in [1_000_000i32, 2_000_000] {
            bytes.extend_from_slice(&time.to_be_bytes());
        }
        bytes.extend_from_slice(&[0, 1]);
        bytes.extend_from_slice(&7_200i32.to_be_bytes());
        bytes.extend_from_slice(&[0, 0]);
        bytes.extend_from_slice(&10_800i32.to_be_bytes());
        bytes.extend_from_slice(&[1, 4]);
        bytes.extend_from_slice(b"AB\0CDEFGH");
        let zone = parse_tzif(&bytes).expect("parsed");
        assert_eq!(zone.offset(500_000), 7_200);
        assert_eq!(zone.offset(1_500_000), 7_200);
        assert_eq!(zone.offset(2_500_000), 10_800);
    }

    #[test]
    fn tzif_v2_uses_wide_transitions_and_posix_footer() {
        let first = test_tzif_block(false);
        let second = test_tzif_block(true);
        let mut bytes = first;
        bytes.extend_from_slice(&second);
        bytes.extend_from_slice(b"\nEST5EDT,M3.2.0,M11.1.0\n");
        let zone = parse_tzif(&bytes).expect("parsed");
        assert_eq!(zone.offset(epoch(2023, 7, 1, 12, 0)), -4 * 3_600);
    }

    fn test_tzif_block(wide: bool) -> Vec<u8> {
        let mut block = Vec::new();
        block.extend_from_slice(b"TZif2");
        block.extend_from_slice(&[0; 15]);
        for count in [0u32, 0, 0, 1, 1, 4] {
            block.extend_from_slice(&count.to_be_bytes());
        }
        let transition = 946_684_800i64;
        if wide {
            block.extend_from_slice(&transition.to_be_bytes());
        } else {
            block.extend_from_slice(&i32::try_from(transition).unwrap().to_be_bytes());
        }
        block.push(0); // Transition selects type 0.
        block.extend_from_slice(&0i32.to_be_bytes());
        block.extend_from_slice(&[0, 0]);
        block.extend_from_slice(b"STD\0");
        block
    }

    #[test]
    fn named_zoneinfo_paths_use_tzif_rules() {
        let Some(Zone::Tzif(zone)) = zone_from_tz("Asia/Taipei") else {
            return; // Zoneinfo databases are optional on some targets.
        };
        assert_eq!(
            local_hm_with_offset(1_700_000_000, zone.offset(1_700_000_000)),
            "06:13"
        );
    }

    #[test]
    fn system_tzif_matches_date_oracle() {
        let Ok(bytes) = std::fs::read("/etc/localtime") else {
            return;
        };
        let Some(zone) = parse_tzif(&bytes) else {
            return;
        };
        let timestamp = 1_700_000_000i64;
        let output = std::process::Command::new("date")
            .env_remove("TZ")
            .arg("-r")
            .arg(timestamp.to_string())
            .arg("+%H:%M")
            .output()
            .or_else(|_| {
                std::process::Command::new("date")
                    .env_remove("TZ")
                    .arg("-d")
                    .arg(format!("@{timestamp}"))
                    .arg("+%H:%M")
                    .output()
            });
        let Ok(output) = output else { return };
        if !output.status.success() {
            return;
        }
        let expected = String::from_utf8_lossy(&output.stdout).trim().to_string();
        assert_eq!(
            local_hm_with_offset(timestamp, zone.offset(timestamp)),
            expected
        );
    }

    fn local_hm_with_offset(epoch: i64, offset: i32) -> String {
        let seconds = (i128::from(epoch) + i128::from(offset)).rem_euclid(86_400);
        format!("{:02}:{:02}", seconds / 3_600, (seconds % 3_600) / 60)
    }
}
