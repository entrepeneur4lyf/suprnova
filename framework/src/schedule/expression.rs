//! Cron expression parsing and due-checking
//!
//! Supports standard cron syntax with 5 fields:
//! `minute hour day-of-month month day-of-week`

use chrono::{DateTime, Datelike, Local, TimeZone, Timelike};

/// Day of week enum for scheduling
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DayOfWeek {
    Sunday = 0,
    Monday = 1,
    Tuesday = 2,
    Wednesday = 3,
    Thursday = 4,
    Friday = 5,
    Saturday = 6,
}

impl DayOfWeek {
    /// Convert from chrono Weekday
    pub fn from_chrono(weekday: chrono::Weekday) -> Self {
        match weekday {
            chrono::Weekday::Sun => DayOfWeek::Sunday,
            chrono::Weekday::Mon => DayOfWeek::Monday,
            chrono::Weekday::Tue => DayOfWeek::Tuesday,
            chrono::Weekday::Wed => DayOfWeek::Wednesday,
            chrono::Weekday::Thu => DayOfWeek::Thursday,
            chrono::Weekday::Fri => DayOfWeek::Friday,
            chrono::Weekday::Sat => DayOfWeek::Saturday,
        }
    }
}

/// Cron expression for scheduling tasks
///
/// Supports standard cron syntax with 5 fields:
/// `minute hour day-of-month month day-of-week`
///
/// # Examples
///
/// ```rust,ignore
/// use suprnova::CronExpression;
///
/// // Every minute
/// let expr = CronExpression::every_minute();
///
/// // Daily at 3:00 AM
/// let expr = CronExpression::daily_at("03:00");
///
/// // Custom cron expression
/// let expr = CronExpression::parse("0 */2 * * *").unwrap(); // Every 2 hours
/// ```
#[derive(Debug, Clone)]
pub struct CronExpression {
    raw: String,
    /// Minutes (0-59)
    minute: CronField,
    /// Hours (0-23)
    hour: CronField,
    /// Day of month (1-31)
    day_of_month: CronField,
    /// Month (1-12)
    month: CronField,
    /// Day of week (0-6, Sunday=0)
    day_of_week: CronField,
}

#[derive(Debug, Clone)]
enum CronField {
    Any,                // *
    Value(u32),         // 5
    Range(u32, u32),    // 1-5
    Step(u32),          // */5
    List(Vec<u32>),     // 1,3,5
    StepFrom(u32, u32), // 5/10 (start at 5, every 10)
}

impl CronField {
    fn matches(&self, value: u32) -> bool {
        match self {
            CronField::Any => true,
            CronField::Value(v) => *v == value,
            CronField::Range(start, end) => value >= *start && value <= *end,
            CronField::Step(step) => value.is_multiple_of(*step),
            CronField::StepFrom(start, step) => {
                value >= *start && (value - start).is_multiple_of(*step)
            }
            CronField::List(values) => values.contains(&value),
        }
    }

    fn parse(s: &str) -> Result<Self, String> {
        if s == "*" {
            return Ok(CronField::Any);
        }

        // Handle */N (every N)
        if let Some(rest) = s.strip_prefix("*/") {
            let step: u32 = rest
                .parse()
                .map_err(|_| format!("Invalid step value in '{}'", s))?;
            return Ok(CronField::Step(step));
        }

        // Handle N/M (starting at N, every M)
        if s.contains('/') && !s.starts_with('*') {
            let parts: Vec<&str> = s.split('/').collect();
            if parts.len() == 2 {
                let start: u32 = parts[0]
                    .parse()
                    .map_err(|_| format!("Invalid start value in '{}'", s))?;
                let step: u32 = parts[1]
                    .parse()
                    .map_err(|_| format!("Invalid step value in '{}'", s))?;
                return Ok(CronField::StepFrom(start, step));
            }
        }

        // Handle comma-separated list (1,3,5)
        if s.contains(',') {
            let values: Result<Vec<u32>, _> = s.split(',').map(|v| v.trim().parse()).collect();
            return Ok(CronField::List(
                values.map_err(|_| format!("Invalid list value in '{}'", s))?,
            ));
        }

        // Handle range (1-5)
        if s.contains('-') {
            let parts: Vec<&str> = s.split('-').collect();
            if parts.len() == 2 {
                let start: u32 = parts[0]
                    .parse()
                    .map_err(|_| format!("Invalid range start in '{}'", s))?;
                let end: u32 = parts[1]
                    .parse()
                    .map_err(|_| format!("Invalid range end in '{}'", s))?;
                return Ok(CronField::Range(start, end));
            }
        }

        // Handle single value
        let value: u32 = s.parse().map_err(|_| format!("Invalid value in '{}'", s))?;
        Ok(CronField::Value(value))
    }
}

impl std::fmt::Display for CronField {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CronField::Any => write!(f, "*"),
            CronField::Value(v) => write!(f, "{}", v),
            CronField::Range(s, e) => write!(f, "{}-{}", s, e),
            CronField::Step(s) => write!(f, "*/{}", s),
            CronField::StepFrom(start, step) => write!(f, "{}/{}", start, step),
            CronField::List(l) => write!(
                f,
                "{}",
                l.iter()
                    .map(|v| v.to_string())
                    .collect::<Vec<_>>()
                    .join(",")
            ),
        }
    }
}

impl CronExpression {
    /// Parse a cron expression string
    ///
    /// Format: `minute hour day-of-month month day-of-week`
    ///
    /// # Examples
    ///
    /// - `* * * * *` - Every minute
    /// - `0 * * * *` - Every hour
    /// - `0 3 * * *` - Daily at 3:00 AM
    /// - `0 0 * * 0` - Weekly on Sunday
    /// - `*/5 * * * *` - Every 5 minutes
    pub fn parse(expression: &str) -> Result<Self, String> {
        let parts: Vec<&str> = expression.split_whitespace().collect();

        if parts.len() != 5 {
            return Err(format!(
                "Cron expression must have 5 fields, got {}",
                parts.len()
            ));
        }

        Ok(Self {
            raw: expression.to_string(),
            minute: CronField::parse(parts[0])?,
            hour: CronField::parse(parts[1])?,
            day_of_month: CronField::parse(parts[2])?,
            month: CronField::parse(parts[3])?,
            day_of_week: CronField::parse(parts[4])?,
        })
    }

    /// Check if this expression is due now (wall clock).
    ///
    /// Thin wrapper over [`Self::is_due_at`] that uses `Local::now()` as the
    /// clock. Production schedulers should call this; tests should prefer
    /// `is_due_at` so they can inject a synthetic clock and avoid clock-skew
    /// flakiness.
    pub fn is_due(&self) -> bool {
        self.is_due_at(Local::now())
    }

    /// Check if this expression is due for the supplied instant.
    ///
    /// Exposed so tests can drive cron evaluation against a fixed clock —
    /// the same-minute dedup test and any future timezone/DST test build a
    /// `DateTime<Local>` from a fixed `NaiveDateTime` rather than racing
    /// `tokio::time::pause()` against wall-clock advancement. Generic over
    /// `TimeZone` so callers can also pass `Utc` or a custom offset when
    /// the per-schedule timezone follow-up lands.
    pub fn is_due_at<Tz: TimeZone>(&self, now: DateTime<Tz>) -> bool {
        self.minute.matches(now.minute())
            && self.hour.matches(now.hour())
            && self.day_of_month.matches(now.day())
            && self.month.matches(now.month())
            && self
                .day_of_week
                .matches(now.weekday().num_days_from_sunday())
    }

    /// Get the raw cron expression string
    pub fn expression(&self) -> &str {
        &self.raw
    }

    /// Set the time component (modifies hour and minute)
    pub fn at(mut self, time: &str) -> Self {
        let parts: Vec<&str> = time.split(':').collect();
        if parts.len() == 2
            && let (Ok(hour), Ok(minute)) = (parts[0].parse::<u32>(), parts[1].parse::<u32>())
        {
            self.hour = CronField::Value(hour);
            self.minute = CronField::Value(minute);
            self.raw = format!(
                "{} {} {} {} {}",
                minute, hour, self.day_of_month, self.month, self.day_of_week,
            );
        }
        self
    }

    // =========================================================================
    // Factory Methods
    // =========================================================================

    /// Every minute: `* * * * *`
    pub fn every_minute() -> Self {
        Self::parse("* * * * *").unwrap()
    }

    /// Every N minutes: `*/N * * * *`
    ///
    /// # Panics
    ///
    /// Panics if `n` is outside the cron minute range `1..=59`. Cron
    /// step values must be positive and below the field width. Use a
    /// `1..=59` value, or fall back to [`Self::hourly`] / similar
    /// helpers for coarser intervals.
    pub fn every_n_minutes(n: u32) -> Self {
        Self::try_every_n_minutes(n)
            .expect("every_n_minutes: step `n` must be in the cron minute range 1..=59")
    }

    /// Fallible sibling of [`every_n_minutes`](Self::every_n_minutes): returns
    /// `Err` instead of panicking when `n` is outside `1..=59`. (The infallible
    /// helper's `# Panics` contract was previously unenforced — the cron parser
    /// accepts any `u32` without range-checking — so a bad step silently
    /// produced a never-firing schedule; this validates the contract.)
    pub fn try_every_n_minutes(n: u32) -> Result<Self, String> {
        if !(1..=59).contains(&n) {
            return Err(format!(
                "every_n_minutes: step `n` must be in 1..=59, got {n}"
            ));
        }
        Self::parse(&format!("*/{} * * * *", n))
    }

    /// Every hour at minute 0: `0 * * * *`
    pub fn hourly() -> Self {
        Self::parse("0 * * * *").unwrap()
    }

    /// Every hour at specific minute: `M * * * *`
    ///
    /// # Panics
    ///
    /// Panics if `minute` is outside `0..=59`. Cron minute field accepts
    /// 0 through 59 inclusive.
    pub fn hourly_at(minute: u32) -> Self {
        Self::try_hourly_at(minute).expect("hourly_at: `minute` must be in 0..=59")
    }

    /// Fallible sibling of [`hourly_at`](Self::hourly_at): returns `Err`
    /// instead of panicking when `minute` is outside `0..=59`.
    pub fn try_hourly_at(minute: u32) -> Result<Self, String> {
        if minute > 59 {
            return Err(format!(
                "hourly_at: `minute` must be in 0..=59, got {minute}"
            ));
        }
        Self::parse(&format!("{} * * * *", minute))
    }

    /// Daily at midnight: `0 0 * * *`
    pub fn daily() -> Self {
        Self::parse("0 0 * * *").unwrap()
    }

    /// Daily at specific time: `M H * * *`
    ///
    /// `time` is a `HH:MM` string (24-hour clock). Lenient parsing: a string
    /// that is not exactly two `:`-separated segments falls back to
    /// [`daily`](Self::daily); a non-numeric segment is treated as `0`.
    ///
    /// # Panics
    ///
    /// Panics if either numeric segment is out of cron range (hour `0..=23`,
    /// minute `0..=59`). Pass a well-formed `"HH:MM"` to avoid the panic —
    /// e.g. `"09:30"` or `"23:00"` — or use [`try_daily_at`](Self::try_daily_at).
    pub fn daily_at(time: &str) -> Self {
        Self::try_daily_at(time)
            .expect("daily_at: HH:MM segments must be in cron range (hour 0..=23, minute 0..=59)")
    }

    /// Fallible sibling of [`daily_at`](Self::daily_at): returns `Err` instead
    /// of panicking when a numeric `HH:MM` segment is out of range. Mirrors
    /// `daily_at`'s lenient parsing otherwise (non-`HH:MM` → [`daily`](Self::daily),
    /// non-numeric segment → `0`).
    pub fn try_daily_at(time: &str) -> Result<Self, String> {
        let parts: Vec<&str> = time.split(':').collect();
        if parts.len() == 2 {
            let hour: u32 = parts[0].parse().unwrap_or(0);
            let minute: u32 = parts[1].parse().unwrap_or(0);
            if hour > 23 {
                return Err(format!("daily_at: hour `{hour}` must be in 0..=23"));
            }
            if minute > 59 {
                return Err(format!("daily_at: minute `{minute}` must be in 0..=59"));
            }
            Self::parse(&format!("{} {} * * *", minute, hour))
        } else {
            Ok(Self::daily())
        }
    }

    /// Weekly on Sunday at midnight: `0 0 * * 0`
    pub fn weekly() -> Self {
        Self::parse("0 0 * * 0").unwrap()
    }

    /// Weekly on specific day at midnight: `0 0 * * D`
    pub fn weekly_on(day: DayOfWeek) -> Self {
        Self::parse(&format!("0 0 * * {}", day as u32)).unwrap()
    }

    /// On specific days of the week at midnight
    pub fn on_days(days: &[DayOfWeek]) -> Self {
        let days_str: Vec<String> = days.iter().map(|d| (*d as u32).to_string()).collect();
        Self::parse(&format!("0 0 * * {}", days_str.join(","))).unwrap()
    }

    /// Monthly on the first day at midnight: `0 0 1 * *`
    pub fn monthly() -> Self {
        Self::parse("0 0 1 * *").unwrap()
    }

    /// Monthly on specific day at midnight: `0 0 D * *`
    ///
    /// # Panics
    ///
    /// Panics if `day` is outside `1..=31`. Use a day-of-month value
    /// the calendar can hit — months without a 31st silently skip
    /// (this is cron-standard behaviour).
    pub fn monthly_on(day: u32) -> Self {
        Self::try_monthly_on(day).expect("monthly_on: `day` must be in 1..=31")
    }

    /// Fallible sibling of [`monthly_on`](Self::monthly_on): returns `Err`
    /// instead of panicking when `day` is outside `1..=31`.
    pub fn try_monthly_on(day: u32) -> Result<Self, String> {
        if !(1..=31).contains(&day) {
            return Err(format!("monthly_on: `day` must be in 1..=31, got {day}"));
        }
        Self::parse(&format!("0 0 {} * *", day))
    }

    /// Quarterly on the first day of each quarter at midnight
    pub fn quarterly() -> Self {
        Self::parse("0 0 1 1,4,7,10 *").unwrap()
    }

    /// Yearly on January 1st at midnight: `0 0 1 1 *`
    pub fn yearly() -> Self {
        Self::parse("0 0 1 1 *").unwrap()
    }

    /// On weekdays (Monday-Friday) at midnight
    pub fn weekdays() -> Self {
        Self::parse("0 0 * * 1-5").unwrap()
    }

    /// On weekends (Saturday-Sunday) at midnight
    pub fn weekends() -> Self {
        Self::parse("0 0 * * 0,6").unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_every_minute() {
        let expr = CronExpression::parse("* * * * *").unwrap();
        assert_eq!(expr.expression(), "* * * * *");
    }

    #[test]
    fn test_parse_specific_time() {
        let expr = CronExpression::parse("30 14 * * *").unwrap();
        assert_eq!(expr.expression(), "30 14 * * *");
    }

    #[test]
    fn test_parse_invalid_expression() {
        let result = CronExpression::parse("* * *");
        assert!(result.is_err());
    }

    #[test]
    fn test_factory_methods() {
        assert_eq!(CronExpression::every_minute().expression(), "* * * * *");
        assert_eq!(CronExpression::hourly().expression(), "0 * * * *");
        assert_eq!(CronExpression::daily().expression(), "0 0 * * *");
        assert_eq!(CronExpression::weekly().expression(), "0 0 * * 0");
        assert_eq!(CronExpression::monthly().expression(), "0 0 1 * *");
    }

    #[test]
    fn test_daily_at() {
        let expr = CronExpression::daily_at("03:30");
        assert_eq!(expr.expression(), "30 3 * * *");
    }

    #[test]
    fn test_at_modifier() {
        let expr = CronExpression::daily().at("14:30");
        assert_eq!(expr.expression(), "30 14 * * *");
    }

    // ---- #380c: helpers now validate ranges (contract-bug fix) ----------
    //
    // The cron parser accepts any `u32` without range-checking, so these
    // helpers' `# Panics` docs were unenforced and bad input silently became
    // a never-firing schedule. The `try_*` siblings now return descriptive
    // `Err`; the infallible variants `expect` on the same check.

    #[test]
    fn try_every_n_minutes_validates_step() {
        assert!(CronExpression::try_every_n_minutes(5).is_ok());
        let err = CronExpression::try_every_n_minutes(0).unwrap_err();
        assert!(err.contains("1..=59"), "got: {err}");
        assert!(CronExpression::try_every_n_minutes(60).is_err());
    }

    #[test]
    fn try_hourly_at_validates_minute() {
        assert!(CronExpression::try_hourly_at(30).is_ok());
        let err = CronExpression::try_hourly_at(99).unwrap_err();
        assert!(err.contains("99") && err.contains("0..=59"), "got: {err}");
    }

    #[test]
    fn try_daily_at_validates_but_mirrors_lenient_parse() {
        // Out-of-range numeric -> Err.
        assert!(CronExpression::try_daily_at("25:00").is_err());
        assert!(CronExpression::try_daily_at("09:61").is_err());
        // Well-formed -> Ok.
        assert_eq!(
            CronExpression::try_daily_at("09:30").unwrap().expression(),
            "30 9 * * *"
        );
        // Lenient (unchanged): non-HH:MM falls back to daily, non-numeric -> 0.
        assert_eq!(
            CronExpression::try_daily_at("nope").unwrap().expression(),
            "0 0 * * *"
        );
        assert_eq!(
            CronExpression::try_daily_at("ab:cd").unwrap().expression(),
            "0 0 * * *"
        );
    }

    #[test]
    fn try_monthly_on_validates_day() {
        assert!(CronExpression::try_monthly_on(15).is_ok());
        assert!(CronExpression::try_monthly_on(0).is_err());
        assert!(CronExpression::try_monthly_on(99).is_err());
    }

    #[test]
    fn infallible_factories_now_panic_on_out_of_range() {
        use std::panic::catch_unwind;
        assert!(catch_unwind(|| CronExpression::hourly_at(99)).is_err());
        assert!(catch_unwind(|| CronExpression::monthly_on(99)).is_err());
        assert!(catch_unwind(|| CronExpression::every_n_minutes(0)).is_err());
        // Sanity: valid inputs still build the expected expression.
        assert_eq!(CronExpression::hourly_at(30).expression(), "30 * * * *");
    }
}
