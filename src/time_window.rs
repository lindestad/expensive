//! Time-window calculations for usage aggregation.
//!
//! The dashboard supports daily, weekly, monthly, and all-time views. Daily,
//! weekly, and monthly windows all honor the configured daily start time, so a
//! `04:00` start means the "day" runs from 04:00 local time to the next 04:00.

use std::{fmt, str::FromStr};

use anyhow::{anyhow, bail, Result};
use chrono::{
    DateTime, Datelike, Duration, Local, LocalResult, NaiveDate, NaiveDateTime, NaiveTime, TimeZone,
};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Mode {
    Daily,
    Weekly,
    Monthly,
    AllTime,
}

impl Mode {
    pub const ALL: [Mode; 4] = [Mode::Daily, Mode::Weekly, Mode::Monthly, Mode::AllTime];

    pub fn title(self) -> &'static str {
        match self {
            Mode::Daily => "Daily",
            Mode::Weekly => "Weekly",
            Mode::Monthly => "Monthly",
            Mode::AllTime => "All Time",
        }
    }

    pub fn next(self) -> Self {
        let idx = self.index();
        Self::ALL[(idx + 1) % Self::ALL.len()]
    }

    pub fn previous(self) -> Self {
        let idx = self.index();
        Self::ALL[(idx + Self::ALL.len() - 1) % Self::ALL.len()]
    }

    pub fn index(self) -> usize {
        match self {
            Mode::Daily => 0,
            Mode::Weekly => 1,
            Mode::Monthly => 2,
            Mode::AllTime => 3,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DailyStart {
    pub hour: u32,
    pub minute: u32,
}

impl Default for DailyStart {
    fn default() -> Self {
        Self { hour: 4, minute: 0 }
    }
}

impl DailyStart {
    pub fn as_time(self) -> NaiveTime {
        NaiveTime::from_hms_opt(self.hour, self.minute, 0)
            .expect("DailyStart is validated during parsing")
    }
}

impl fmt::Display for DailyStart {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:02}:{:02}", self.hour, self.minute)
    }
}

impl FromStr for DailyStart {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        let (hour, minute) = value
            .split_once(':')
            .ok_or_else(|| anyhow!("expected HH:MM"))?;
        let hour: u32 = hour.parse()?;
        let minute: u32 = minute.parse()?;

        if hour > 23 || minute > 59 {
            bail!("daily start must be between 00:00 and 23:59");
        }

        Ok(Self { hour, minute })
    }
}

pub fn cutoff_millis(
    mode: Mode,
    now: DateTime<Local>,
    daily_start: DailyStart,
) -> Result<Option<i64>> {
    let Some(local_cutoff) = cutoff_naive(mode, now.naive_local(), daily_start) else {
        return Ok(None);
    };

    let resolved = match Local.from_local_datetime(&local_cutoff) {
        LocalResult::Single(value) => value,
        LocalResult::Ambiguous(first, second) => {
            if first <= second {
                first
            } else {
                second
            }
        }
        LocalResult::None => bail!("configured cutoff time does not exist in local timezone"),
    };

    Ok(Some(resolved.timestamp_millis()))
}

pub fn cutoff_naive(
    mode: Mode,
    now: NaiveDateTime,
    daily_start: DailyStart,
) -> Option<NaiveDateTime> {
    match mode {
        Mode::Daily => Some(daily_cutoff(now, daily_start)),
        Mode::Weekly => Some(weekly_cutoff(now, daily_start)),
        Mode::Monthly => Some(monthly_cutoff(now, daily_start)),
        Mode::AllTime => None,
    }
}

fn daily_cutoff(now: NaiveDateTime, daily_start: DailyStart) -> NaiveDateTime {
    let today_start = now.date().and_time(daily_start.as_time());
    if now < today_start {
        today_start - Duration::days(1)
    } else {
        today_start
    }
}

fn weekly_cutoff(now: NaiveDateTime, daily_start: DailyStart) -> NaiveDateTime {
    let days_from_monday = now.weekday().num_days_from_monday() as i64;
    let monday = now.date() - Duration::days(days_from_monday);
    let cutoff = monday.and_time(daily_start.as_time());
    if now < cutoff {
        cutoff - Duration::days(7)
    } else {
        cutoff
    }
}

fn monthly_cutoff(now: NaiveDateTime, daily_start: DailyStart) -> NaiveDateTime {
    let first = NaiveDate::from_ymd_opt(now.year(), now.month(), 1)
        .expect("current year and month are valid")
        .and_time(daily_start.as_time());

    if now >= first {
        return first;
    }

    let (year, month) = if now.month() == 1 {
        (now.year() - 1, 12)
    } else {
        (now.year(), now.month() - 1)
    };

    NaiveDate::from_ymd_opt(year, month, 1)
        .expect("previous year and month are valid")
        .and_time(daily_start.as_time())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dt(value: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M").unwrap()
    }

    #[test]
    fn daily_cutoff_uses_today_after_start() {
        assert_eq!(
            cutoff_naive(Mode::Daily, dt("2026-06-15 10:30"), DailyStart::default()),
            Some(dt("2026-06-15 04:00"))
        );
    }

    #[test]
    fn daily_cutoff_uses_yesterday_before_start() {
        assert_eq!(
            cutoff_naive(Mode::Daily, dt("2026-06-15 03:30"), DailyStart::default()),
            Some(dt("2026-06-14 04:00"))
        );
    }

    #[test]
    fn weekly_cutoff_starts_on_monday() {
        assert_eq!(
            cutoff_naive(Mode::Weekly, dt("2026-06-18 12:00"), DailyStart::default()),
            Some(dt("2026-06-15 04:00"))
        );
    }

    #[test]
    fn weekly_cutoff_uses_previous_week_before_monday_start() {
        assert_eq!(
            cutoff_naive(Mode::Weekly, dt("2026-06-15 03:30"), DailyStart::default()),
            Some(dt("2026-06-08 04:00"))
        );
    }

    #[test]
    fn monthly_cutoff_starts_on_first_day() {
        assert_eq!(
            cutoff_naive(Mode::Monthly, dt("2026-06-15 12:00"), DailyStart::default()),
            Some(dt("2026-06-01 04:00"))
        );
    }

    #[test]
    fn monthly_cutoff_uses_previous_month_before_first_day_start() {
        assert_eq!(
            cutoff_naive(Mode::Monthly, dt("2026-06-01 03:30"), DailyStart::default()),
            Some(dt("2026-05-01 04:00"))
        );
    }

    #[test]
    fn all_time_has_no_cutoff() {
        assert_eq!(
            cutoff_naive(Mode::AllTime, dt("2026-06-15 10:30"), DailyStart::default()),
            None
        );
    }

    #[test]
    fn parses_daily_start() {
        assert_eq!(
            "4:30".parse::<DailyStart>().unwrap(),
            DailyStart {
                hour: 4,
                minute: 30
            }
        );
        assert!("24:00".parse::<DailyStart>().is_err());
    }
}
