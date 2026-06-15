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

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CalendarScale {
    Day,
    Week,
    Month,
}

impl CalendarScale {
    pub const ALL: [CalendarScale; 3] = [
        CalendarScale::Day,
        CalendarScale::Week,
        CalendarScale::Month,
    ];

    pub fn title(self) -> &'static str {
        match self {
            CalendarScale::Day => "Day",
            CalendarScale::Week => "Week",
            CalendarScale::Month => "Month",
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
            CalendarScale::Day => 0,
            CalendarScale::Week => 1,
            CalendarScale::Month => 2,
        }
    }

    pub fn mode(self) -> Mode {
        match self {
            CalendarScale::Day => Mode::Daily,
            CalendarScale::Week => Mode::Weekly,
            CalendarScale::Month => Mode::Monthly,
        }
    }

    pub fn from_mode(mode: Mode) -> Option<Self> {
        match mode {
            Mode::Daily => Some(CalendarScale::Day),
            Mode::Weekly => Some(CalendarScale::Week),
            Mode::Monthly => Some(CalendarScale::Month),
            Mode::AllTime => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct PeriodKey {
    pub scale: CalendarScale,
    pub start_millis: i64,
    pub end_millis: i64,
}

impl PeriodKey {
    pub fn contains(self, millis: i64) -> bool {
        millis >= self.start_millis && millis < self.end_millis
    }

    pub fn mode(self) -> Mode {
        self.scale.mode()
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

pub fn current_period(
    scale: CalendarScale,
    now: DateTime<Local>,
    daily_start: DailyStart,
) -> Result<PeriodKey> {
    let start_millis = cutoff_millis(scale.mode(), now, daily_start)?
        .expect("calendar scales always have a finite cutoff");
    let start = local_from_millis(start_millis)?;
    period_from_start(scale, start)
}

pub fn shift_period(period: PeriodKey, steps: i32) -> Result<PeriodKey> {
    let start = local_from_millis(period.start_millis)?;
    let shifted = shift_start(start, period.scale, steps)?;
    period_from_start(period.scale, shifted)
}

pub fn visible_periods(period: PeriodKey, daily_start: DailyStart) -> Result<Vec<PeriodKey>> {
    match period.scale {
        CalendarScale::Day => visible_days(period, daily_start),
        CalendarScale::Week => visible_weeks(period),
        CalendarScale::Month => visible_months(period, daily_start),
    }
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

fn visible_days(period: PeriodKey, daily_start: DailyStart) -> Result<Vec<PeriodKey>> {
    let selected = local_from_millis(period.start_millis)?.naive_local();
    let first_of_month = NaiveDate::from_ymd_opt(selected.year(), selected.month(), 1)
        .expect("selected year and month are valid")
        .and_time(daily_start.as_time());
    let offset = first_of_month.weekday().num_days_from_monday() as i64;
    let grid_start = resolve_local(first_of_month - Duration::days(offset))?;

    (0..42)
        .map(|idx| {
            let start = shift_start(grid_start, CalendarScale::Day, idx)?;
            period_from_start(CalendarScale::Day, start)
        })
        .collect()
}

fn visible_weeks(period: PeriodKey) -> Result<Vec<PeriodKey>> {
    let selected = local_from_millis(period.start_millis)?;
    let week_index = selected.iso_week().week() as i32 - 1;
    let page_offset = week_index.rem_euclid(12);
    let grid_start = shift_start(selected, CalendarScale::Week, -page_offset)?;

    (0..12)
        .map(|idx| {
            let start = shift_start(grid_start, CalendarScale::Week, idx)?;
            period_from_start(CalendarScale::Week, start)
        })
        .collect()
}

fn visible_months(period: PeriodKey, daily_start: DailyStart) -> Result<Vec<PeriodKey>> {
    let selected = local_from_millis(period.start_millis)?.naive_local();
    let first_month = NaiveDate::from_ymd_opt(selected.year(), 1, 1)
        .expect("selected year is valid")
        .and_time(daily_start.as_time());
    let grid_start = resolve_local(first_month)?;

    (0..12)
        .map(|idx| {
            let start = shift_start(grid_start, CalendarScale::Month, idx)?;
            period_from_start(CalendarScale::Month, start)
        })
        .collect()
}

fn period_from_start(scale: CalendarScale, start: DateTime<Local>) -> Result<PeriodKey> {
    let end = shift_start(start, scale, 1)?;
    Ok(PeriodKey {
        scale,
        start_millis: start.timestamp_millis(),
        end_millis: end.timestamp_millis(),
    })
}

fn daily_cutoff(now: NaiveDateTime, daily_start: DailyStart) -> NaiveDateTime {
    let today_start = now.date().and_time(daily_start.as_time());
    if now < today_start {
        today_start - Duration::days(1)
    } else {
        today_start
    }
}

fn shift_start(
    start: DateTime<Local>,
    scale: CalendarScale,
    steps: i32,
) -> Result<DateTime<Local>> {
    let naive = start.naive_local();
    let shifted = match scale {
        CalendarScale::Day => naive + Duration::days(i64::from(steps)),
        CalendarScale::Week => naive + Duration::days(i64::from(steps) * 7),
        CalendarScale::Month => add_months(naive, steps),
    };
    resolve_local(shifted)
}

fn add_months(value: NaiveDateTime, months: i32) -> NaiveDateTime {
    let month_index = value.year() * 12 + value.month0() as i32 + months;
    let year = month_index.div_euclid(12);
    let month0 = month_index.rem_euclid(12) as u32;
    let month = month0 + 1;
    let day = value.day().min(days_in_month(year, month));

    NaiveDate::from_ymd_opt(year, month, day)
        .expect("calculated year, month, and day are valid")
        .and_time(value.time())
}

fn days_in_month(year: i32, month: u32) -> u32 {
    let (next_year, next_month) = if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };
    let next_month_start =
        NaiveDate::from_ymd_opt(next_year, next_month, 1).expect("next month is valid");
    (next_month_start - Duration::days(1)).day()
}

fn local_from_millis(millis: i64) -> Result<DateTime<Local>> {
    DateTime::from_timestamp_millis(millis)
        .map(|value| value.with_timezone(&Local))
        .ok_or_else(|| anyhow!("timestamp is outside the supported range"))
}

fn resolve_local(value: NaiveDateTime) -> Result<DateTime<Local>> {
    match Local.from_local_datetime(&value) {
        LocalResult::Single(value) => Ok(value),
        LocalResult::Ambiguous(first, second) => {
            if first <= second {
                Ok(first)
            } else {
                Ok(second)
            }
        }
        LocalResult::None => bail!("configured cutoff time does not exist in local timezone"),
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

    fn local_millis(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> i64 {
        Local
            .with_ymd_and_hms(year, month, day, hour, minute, 0)
            .unwrap()
            .timestamp_millis()
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
    fn current_period_has_bounded_day_range() {
        let period = current_period(
            CalendarScale::Day,
            Local.with_ymd_and_hms(2026, 6, 15, 10, 30, 0).unwrap(),
            DailyStart::default(),
        )
        .unwrap();

        assert_eq!(period.scale, CalendarScale::Day);
        assert_eq!(period.start_millis, local_millis(2026, 6, 15, 4, 0));
        assert_eq!(period.end_millis, local_millis(2026, 6, 16, 4, 0));
    }

    #[test]
    fn visible_days_returns_six_week_month_grid() {
        let period = current_period(
            CalendarScale::Day,
            Local.with_ymd_and_hms(2026, 6, 15, 10, 30, 0).unwrap(),
            DailyStart::default(),
        )
        .unwrap();
        let days = visible_periods(period, DailyStart::default()).unwrap();

        assert_eq!(days.len(), 42);
        assert_eq!(days[0].start_millis, local_millis(2026, 6, 1, 4, 0));
        assert_eq!(days[41].start_millis, local_millis(2026, 7, 12, 4, 0));
    }

    #[test]
    fn visible_weeks_uses_stable_twelve_week_page() {
        let period = current_period(
            CalendarScale::Week,
            Local.with_ymd_and_hms(2026, 6, 18, 10, 30, 0).unwrap(),
            DailyStart::default(),
        )
        .unwrap();
        let next = shift_period(period, 1).unwrap();
        let weeks = visible_periods(period, DailyStart::default()).unwrap();
        let next_weeks = visible_periods(next, DailyStart::default()).unwrap();

        assert_eq!(weeks.len(), 12);
        assert_eq!(weeks[0].start_millis, next_weeks[0].start_millis);
    }

    #[test]
    fn shift_period_moves_by_calendar_scale() {
        let period = current_period(
            CalendarScale::Month,
            Local.with_ymd_and_hms(2026, 6, 15, 10, 30, 0).unwrap(),
            DailyStart::default(),
        )
        .unwrap();
        let previous = shift_period(period, -1).unwrap();
        let next = shift_period(period, 1).unwrap();

        assert_eq!(previous.start_millis, local_millis(2026, 5, 1, 4, 0));
        assert_eq!(next.start_millis, local_millis(2026, 7, 1, 4, 0));
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
