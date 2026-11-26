//! Calendar dates without a date crate: days since 1970-01-01 (UTC) and
//! back (Howard Hinnant's civil-date algorithms).

/// Days since 1970-01-01 for `YYYY-MM-DD`, or the date part of an ISO
/// timestamp (`2026-11-20T17:59:59.000Z`). `None` for anything else.
pub fn days_from_iso(s: &str) -> Option<i64> {
    let date = s.get(..10)?;
    let mut parts = date.split('-');
    let (y, m, d) = (parts.next()?, parts.next()?, parts.next()?);
    if y.len() != 4 || m.len() != 2 || d.len() != 2 {
        return None;
    }
    let (y, m, d): (i64, i64, i64) = (y.parse().ok()?, m.parse().ok()?, d.parse().ok()?);
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146_097 + doe - 719_468)
}

/// `(year, month, day)` for days since 1970-01-01.
pub fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

/// Today, in days since 1970-01-01 (UTC).
pub fn today() -> i64 {
    crate::state::now_ms().div_euclid(86_400_000)
}

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

/// "20 November 2026".
pub fn long(days: i64) -> String {
    let (y, m, d) = civil(days);
    format!("{d} {} {y}", MONTHS[(m - 1) as usize])
}

/// "version 1, reviewed 20 November 2026": where a data file's words come
/// from, shown under the part that uses it.
pub fn about(version: u32, reviewed: &str) -> String {
    match days_from_iso(reviewed) {
        Some(d) => format!("version {version}, reviewed {}", long(d)),
        None => format!("version {version}"),
    }
}

/// "November 2026".
pub fn month(days: i64) -> String {
    let (y, m, _) = civil(days);
    format!("{} {y}", MONTHS[(m - 1) as usize])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_round_trip() {
        assert_eq!(days_from_iso("1970-01-01"), Some(0));
        assert_eq!(days_from_iso("2000-03-01"), Some(11_017));
        let d = days_from_iso("2026-11-20T17:59:59.000Z").unwrap();
        assert_eq!(civil(d), (2026, 11, 20));
        assert_eq!(long(d), "20 November 2026");
        assert_eq!(month(d), "November 2026");
        for day in [-1, 0, 59, 365, 11_016, 20_000, 30_000] {
            let (y, m, dd) = civil(day);
            assert_eq!(days_from_iso(&format!("{y:04}-{m:02}-{dd:02}")), Some(day));
        }
        assert_eq!(days_from_iso("2026-13-01"), None);
        assert_eq!(days_from_iso("26-11-20"), None);
        assert_eq!(days_from_iso("soon"), None);
    }
}
