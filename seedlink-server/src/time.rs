//! Timestamp parsing and time window filtering for SeedLink TIME command.
//!
//! Handles two timestamp formats:
//! - TIME command: `"YYYY,M,D,h,m,s"` (month/day based)
//! - miniSEED v2 BTime: binary day-of-year based (payload bytes 20..30)

/// Comparable timestamp represented as seconds since Unix epoch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Timestamp {
    seconds: i64,
}

impl Timestamp {
    /// Parse TIME command format: `"2024,1,15,10,30,45"`.
    ///
    /// Fields: year, month, day, hour, minute, second.
    pub fn from_time_command(s: &str) -> Option<Self> {
        let parts: Vec<&str> = s.split(',').collect();
        if parts.len() != 6 {
            return None;
        }
        let year: i64 = parts[0].parse().ok()?;
        let month: u32 = parts[1].parse().ok()?;
        let day: u32 = parts[2].parse().ok()?;
        let hour: u32 = parts[3].parse().ok()?;
        let minute: u32 = parts[4].parse().ok()?;
        let second: u32 = parts[5].parse().ok()?;

        if !(1..=12).contains(&month)
            || !(1..=31).contains(&day)
            || hour > 23
            || minute > 59
            || second > 59
        {
            return None;
        }

        let doy = month_day_to_doy(year, month, day)?;
        Some(Self::from_components(year, doy, hour, minute, second))
    }

    /// Parse miniSEED v2 BTime from payload bytes 20..30.
    ///
    /// BTime layout (big-endian):
    /// - bytes 20..22: year (u16)
    /// - bytes 22..24: day-of-year (u16)
    /// - byte 24: hour (u8)
    /// - byte 25: minute (u8)
    /// - byte 26: second (u8)
    /// - byte 27: unused
    /// - bytes 28..30: ticks/10000ths of second (u16, ignored for comparison)
    pub fn from_mseed_payload(payload: &[u8]) -> Option<Self> {
        if payload.len() < 30 {
            return None;
        }
        let year = u16::from_be_bytes([payload[20], payload[21]]) as i64;
        let doy = u16::from_be_bytes([payload[22], payload[23]]) as u32;
        let hour = payload[24] as u32;
        let minute = payload[25] as u32;
        let second = payload[26] as u32;

        if year == 0 || doy == 0 || doy > 366 || hour > 23 || minute > 59 || second > 59 {
            return None;
        }

        Some(Self::from_components(year, doy, hour, minute, second))
    }

    /// Build a timestamp from year, day-of-year, and time components.
    fn from_components(year: i64, doy: u32, hour: u32, minute: u32, second: u32) -> Self {
        // Days from Unix epoch (1970-01-01) to start of `year`
        let mut days: i64 = 0;
        if year >= 1970 {
            for y in 1970..year {
                days += if is_leap(y) { 366 } else { 365 };
            }
        } else {
            for y in year..1970 {
                days -= if is_leap(y) { 366 } else { 365 };
            }
        }
        // Add day-of-year (1-based)
        days += (doy as i64) - 1;

        let seconds = days * 86400 + (hour as i64) * 3600 + (minute as i64) * 60 + (second as i64);
        Self { seconds }
    }
}

fn is_leap(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

/// Convert month (1-12) and day (1-31) to day-of-year (1-366).
fn month_day_to_doy(year: i64, month: u32, day: u32) -> Option<u32> {
    let leap = is_leap(year);
    let month_days: [u32; 12] = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];

    if !(1..=12).contains(&month) {
        return None;
    }
    let max_day = month_days[(month - 1) as usize];
    if day < 1 || day > max_day {
        return None;
    }

    let mut doy = day;
    for &md in month_days.iter().take((month - 1) as usize) {
        doy += md;
    }
    Some(doy)
}

/// Time window filter attached to a subscription.
#[derive(Debug, Clone)]
pub(crate) struct TimeWindow {
    pub start: Timestamp,
    pub end: Option<Timestamp>,
}

impl TimeWindow {
    /// Parse TIME command arguments into a TimeWindow.
    pub fn parse(start: &str, end: Option<&str>) -> Option<Self> {
        let start_ts = Timestamp::from_time_command(start)?;
        let end_ts = match end {
            Some(e) => Some(Timestamp::from_time_command(e)?),
            None => None,
        };
        Some(Self {
            start: start_ts,
            end: end_ts,
        })
    }

    /// Check if a timestamp falls within this window.
    ///
    /// - `start <= ts` is always required
    /// - If `end` is `Some`, `ts <= end` is also required
    pub fn contains(&self, ts: Timestamp) -> bool {
        if ts < self.start {
            return false;
        }
        if let Some(end) = self.end
            && ts > end
        {
            return false;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_time_command_valid() {
        let ts = Timestamp::from_time_command("2024,1,15,10,30,45").unwrap();
        // 2024-01-15 10:30:45 UTC
        // Days from 1970 to 2024: 54 years
        // DOY for Jan 15 = 15
        assert!(ts.seconds > 0);
    }

    #[test]
    fn parse_time_command_invalid() {
        assert!(Timestamp::from_time_command("").is_none());
        assert!(Timestamp::from_time_command("2024,13,1,0,0,0").is_none()); // month 13
        assert!(Timestamp::from_time_command("2024,0,1,0,0,0").is_none()); // month 0
        assert!(Timestamp::from_time_command("2024,1,32,0,0,0").is_none()); // day 32
        assert!(Timestamp::from_time_command("2024,2,30,0,0,0").is_none()); // Feb 30 (leap)
        assert!(Timestamp::from_time_command("2023,2,29,0,0,0").is_none()); // Feb 29 non-leap
        assert!(Timestamp::from_time_command("2024,1,1,24,0,0").is_none()); // hour 24
        assert!(Timestamp::from_time_command("not,a,time,at,all,x").is_none());
    }

    #[test]
    fn month_day_to_doy_regular() {
        // Jan 1 = DOY 1
        assert_eq!(month_day_to_doy(2023, 1, 1), Some(1));
        // Jan 31 = DOY 31
        assert_eq!(month_day_to_doy(2023, 1, 31), Some(31));
        // Feb 1 = DOY 32 (non-leap)
        assert_eq!(month_day_to_doy(2023, 2, 1), Some(32));
        // Feb 28 = DOY 59 (non-leap)
        assert_eq!(month_day_to_doy(2023, 2, 28), Some(59));
        // Mar 1 = DOY 60 (non-leap)
        assert_eq!(month_day_to_doy(2023, 3, 1), Some(60));
        // Dec 31 = DOY 365 (non-leap)
        assert_eq!(month_day_to_doy(2023, 12, 31), Some(365));
    }

    #[test]
    fn month_day_to_doy_leap() {
        // Feb 29 valid in leap year
        assert_eq!(month_day_to_doy(2024, 2, 29), Some(60));
        // Mar 1 = DOY 61 in leap year
        assert_eq!(month_day_to_doy(2024, 3, 1), Some(61));
        // Dec 31 = DOY 366 in leap year
        assert_eq!(month_day_to_doy(2024, 12, 31), Some(366));
    }

    #[test]
    fn parse_mseed_btime() {
        let mut payload = vec![0u8; 512];
        // Year 2024 = 0x07E8
        payload[20] = 0x07;
        payload[21] = 0xE8;
        // DOY 15 = 0x000F
        payload[22] = 0x00;
        payload[23] = 0x0F;
        // hour=10, minute=30, second=45
        payload[24] = 10;
        payload[25] = 30;
        payload[26] = 45;

        let ts = Timestamp::from_mseed_payload(&payload).unwrap();
        let expected = Timestamp::from_time_command("2024,1,15,10,30,45").unwrap();
        assert_eq!(ts, expected);
    }

    #[test]
    fn parse_mseed_btime_invalid() {
        // Too short
        assert!(Timestamp::from_mseed_payload(&[0u8; 20]).is_none());
        // Year 0
        assert!(Timestamp::from_mseed_payload(&[0u8; 512]).is_none());
    }

    #[test]
    fn time_window_contains() {
        let tw = TimeWindow::parse("2024,1,1,0,0,0", Some("2024,1,31,23,59,59")).unwrap();

        // Within range
        let mid = Timestamp::from_time_command("2024,1,15,12,0,0").unwrap();
        assert!(tw.contains(mid));

        // At start boundary
        assert!(tw.contains(tw.start));

        // At end boundary
        assert!(tw.contains(tw.end.unwrap()));

        // Before start
        let before = Timestamp::from_time_command("2023,12,31,23,59,59").unwrap();
        assert!(!tw.contains(before));

        // After end
        let after = Timestamp::from_time_command("2024,2,1,0,0,0").unwrap();
        assert!(!tw.contains(after));
    }

    #[test]
    fn time_window_open_ended() {
        let tw = TimeWindow::parse("2024,1,1,0,0,0", None).unwrap();

        // Start boundary passes
        assert!(tw.contains(tw.start));

        // Way in the future passes
        let future = Timestamp::from_time_command("2030,12,31,23,59,59").unwrap();
        assert!(tw.contains(future));

        // Before start fails
        let before = Timestamp::from_time_command("2023,12,31,23,59,59").unwrap();
        assert!(!tw.contains(before));
    }

    #[test]
    fn timestamp_ordering() {
        let t1 = Timestamp::from_time_command("2024,1,1,0,0,0").unwrap();
        let t2 = Timestamp::from_time_command("2024,1,1,0,0,1").unwrap();
        let t3 = Timestamp::from_time_command("2024,6,15,12,0,0").unwrap();
        let t4 = Timestamp::from_time_command("2025,1,1,0,0,0").unwrap();

        assert!(t1 < t2);
        assert!(t2 < t3);
        assert!(t3 < t4);
        assert_eq!(t1, t1);
    }
}
