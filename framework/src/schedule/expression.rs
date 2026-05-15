//! Cron expression parsing and due-checking
//!
//! Supports standard cron syntax with 5 fields:
//! `minute hour day-of-month month day-of-week`

use chrono::{Datelike, Local, Timelike};

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
    Any,               // *
    Value(u32),        // 5
    Range(u32, u32),   // 1-5
    Step(u32),         // */5
    List(Vec<u32>),    // 1,3,5
    StepFrom(u32, u32), // 5/10 (start at 5, every 10)
}

impl CronField {
    fn matches(&self, value: u32) -> bool {
        match self {
            CronField::Any => true,
            CronField::Value(v) => *v == value,
            CronField::Range(start, end) => value >= *start && value <= *end,
            CronField::Step(step) => value.is_multiple_of(*step),
            CronField::StepFrom(start, step) => value >= *start && (value - start).is_multiple_of(*step),
            CronField::List(values) => values.contains(&value),
        }
    }

    fn parse(s: &str) -> Result<Self, String> {
        if s == "*" {
            return Ok(CronField::Any);
        }

        // Handle */N (every N)
        if s.starts_with("*/") {
            let step: u32 = s[2..]
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
        let value: u32 = s
            .parse()
            .map_err(|_| format!("Invalid value in '{}'", s))?;
        Ok(CronField::Value(value))
    }

    fn to_string(&self) -> String {
        match self {
            CronField::Any => "*".to_string(),
            CronField::Value(v) => v.to_string(),
            CronField::Range(s, e) => format!("{}-{}", s, e),
            CronField::Step(s) => format!("*/{}", s),
            CronField::StepFrom(start, step) => format!("{}/{}", start, step),
            CronField::List(l) => l
                .iter()
                .map(|v| v.to_string())
                .collect::<Vec<_>>()
                .join(","),
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

    /// Check if this expression is due now
    pub fn is_due(&self) -> bool {
        let now = Local::now();

        self.minute.matches(now.minute())
            && self.hour.matches(now.hour())
            && self.day_of_month.matches(now.day())
            && self.month.matches(now.month())
            && self.day_of_week.matches(now.weekday().num_days_from_sunday())
    }

    /// Get the raw cron expression string
    pub fn expression(&self) -> &str {
        &self.raw
    }

    /// Set the time component (modifies hour and minute)
    pub fn at(mut self, time: &str) -> Self {
        let parts: Vec<&str> = time.split(':').collect();
        if parts.len() == 2
            && let (Ok(hour), Ok(minute)) = (parts[0].parse::<u32>(), parts[1].parse::<u32>()) {
                self.hour = CronField::Value(hour);
                self.minute = CronField::Value(minute);
                self.raw = format!(
                    "{} {} {} {} {}",
                    minute,
                    hour,
                    self.day_of_month.to_string(),
                    self.month.to_string(),
                    self.day_of_week.to_string(),
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
    pub fn every_n_minutes(n: u32) -> Self {
        Self::parse(&format!("*/{} * * * *", n)).unwrap()
    }

    /// Every hour at minute 0: `0 * * * *`
    pub fn hourly() -> Self {
        Self::parse("0 * * * *").unwrap()
    }

    /// Every hour at specific minute: `M * * * *`
    pub fn hourly_at(minute: u32) -> Self {
        Self::parse(&format!("{} * * * *", minute)).unwrap()
    }

    /// Daily at midnight: `0 0 * * *`
    pub fn daily() -> Self {
        Self::parse("0 0 * * *").unwrap()
    }

    /// Daily at specific time: `M H * * *`
    pub fn daily_at(time: &str) -> Self {
        let parts: Vec<&str> = time.split(':').collect();
        if parts.len() == 2 {
            let hour = parts[0].parse().unwrap_or(0);
            let minute = parts[1].parse().unwrap_or(0);
            Self::parse(&format!("{} {} * * *", minute, hour)).unwrap()
        } else {
            Self::daily()
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
    pub fn monthly_on(day: u32) -> Self {
        Self::parse(&format!("0 0 {} * *", day)).unwrap()
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
}
