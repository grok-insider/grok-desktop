use std::{fmt, str::FromStr};

use chrono::{DateTime, Datelike, LocalResult, NaiveDate, TimeZone, Utc, Weekday};
use chrono_tz::Tz;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    AutomationId, AutomationOccurrenceId, AutomationSchedulerOwnerId, MissedRunPolicy,
    OverlapPolicy, ProjectId, RunId, UnixMillis,
};

/// Version of the schedule-to-occurrence rules persisted beside every cursor and occurrence.
///
/// This must advance whenever canonical parsing, calendar recurrence, timezone gap/fold, or
/// monthly-date behavior changes. Persisted UTC occurrence instants are never rewritten.
pub const AUTOMATION_SCHEDULE_CALCULATOR_VERSION: u32 = 1;
/// Maximum UTF-8 byte length accepted by any schedule parser.
pub const MAX_AUTOMATION_SCHEDULE_BYTES: usize = 256;
/// Maximum UTC window accepted by one calendar evaluation.
pub const MAX_AUTOMATION_SCHEDULE_WINDOW_DAYS: u64 = 370;
/// Maximum decisions returned by one calendar evaluation.
pub const MAX_AUTOMATION_SCHEDULE_DECISIONS: usize = 370;
/// Maximum lifetime of one durable daemon scheduler lease.
pub const MAX_AUTOMATION_SCHEDULER_LEASE_MS: u64 = 60_000;
/// Maximum number of volatile claims before an occurrence requires explicit review.
pub const MAX_AUTOMATION_OCCURRENCE_CLAIM_ATTEMPTS: u32 = 16;

const MILLIS_PER_DAY: u64 = 86_400_000;
const MAX_AUTOMATION_TITLE_BYTES: usize = 200;
const MAX_AUTOMATION_PROMPT_BYTES: usize = 64 * 1024;
const RENDERER_JSON_FIELDS: [&str; 6] = [
    "frequency",
    "localTime",
    "weekday",
    "dayOfMonth",
    "timeZoneIana",
    "timeZoneWindows",
];

/// Closed recurrence cadence supported by the daemon-owned v1 calculator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AutomationCadence {
    /// One local occurrence every calendar day.
    Daily,
    /// One local occurrence on Monday through Friday.
    Weekdays,
    /// One local occurrence on the selected weekday, where Sunday is zero.
    Weekly {
        /// Sunday-based weekday in the inclusive range zero through six.
        weekday: u8,
    },
    /// One local occurrence on the selected day when that date exists in the month.
    Monthly {
        /// Calendar day in the inclusive range one through 31.
        day_of_month: u8,
    },
}

/// Canonical daemon-owned local recurring schedule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AutomationSchedule {
    /// Calendar recurrence rule.
    pub cadence: AutomationCadence,
    /// Local wall-clock hour in the inclusive range zero through 23.
    pub hour: u8,
    /// Local wall-clock minute in the inclusive range zero through 59.
    pub minute: u8,
}

/// SHA-256 binding of calculator version, canonical schedule, and canonical timezone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AutomationScheduleFingerprint([u8; 32]);

impl AutomationScheduleFingerprint {
    /// Wraps an already-computed fingerprint for persistence rehydration.
    #[must_use]
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Borrows the fixed-width fingerprint.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Copies the fixed-width fingerprint.
    #[must_use]
    pub const fn to_bytes(self) -> [u8; 32] {
        self.0
    }
}

/// Validated nominal local calendar slot, including slots absent during a DST gap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AutomationLocalDateTime {
    /// Gregorian year.
    pub year: i32,
    /// Calendar month in the inclusive range one through 12.
    pub month: u8,
    /// Calendar day valid for the selected year and month.
    pub day: u8,
    /// Local hour in the inclusive range zero through 23.
    pub hour: u8,
    /// Local minute in the inclusive range zero through 59.
    pub minute: u8,
}

impl AutomationLocalDateTime {
    /// Creates a validated minute-precision local calendar slot.
    ///
    /// # Errors
    ///
    /// Returns [`AutomationScheduleError`] when the date or time is invalid.
    pub fn new(
        year: i32,
        month: u8,
        day: u8,
        hour: u8,
        minute: u8,
    ) -> Result<Self, AutomationScheduleError> {
        let value = Self {
            year,
            month,
            day,
            hour,
            minute,
        };
        value.to_naive()?;
        Ok(value)
    }

    fn from_date(date: NaiveDate, hour: u8, minute: u8) -> Self {
        Self {
            year: date.year(),
            month: u8::try_from(date.month()).expect("month is bounded"),
            day: u8::try_from(date.day()).expect("day is bounded"),
            hour,
            minute,
        }
    }

    fn to_naive(self) -> Result<chrono::NaiveDateTime, AutomationScheduleError> {
        NaiveDate::from_ymd_opt(self.year, u32::from(self.month), u32::from(self.day))
            .and_then(|date| date.and_hms_opt(u32::from(self.hour), u32::from(self.minute), 0))
            .ok_or(AutomationScheduleError::InvalidLocalDateTime)
    }
}

/// One deterministic calendar decision produced by the v1 calculator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutomationScheduleDecision {
    /// A valid local slot resolved to one frozen UTC Unix timestamp.
    Due {
        /// Intended local calendar slot.
        nominal_local: AutomationLocalDateTime,
        /// Frozen UTC timestamp. In a DST fold this is the earlier UTC instant.
        scheduled_for: UnixMillis,
    },
    /// The intended local slot did not exist during a forward DST transition.
    ///
    /// The calculator never shifts this slot. Callers can durably record the explicit skip and
    /// advance to the following calendar slot.
    SkippedNonexistentLocalTime {
        /// Missing nominal local slot used as the durable logical identity.
        nominal_local: AutomationLocalDateTime,
    },
}

impl AutomationScheduleDecision {
    /// Returns the immutable nominal local identity for this decision.
    #[must_use]
    pub const fn nominal_local(self) -> AutomationLocalDateTime {
        match self {
            Self::Due { nominal_local, .. }
            | Self::SkippedNonexistentLocalTime { nominal_local } => nominal_local,
        }
    }

    /// Returns the resolved UTC timestamp, absent only for a DST gap.
    #[must_use]
    pub const fn scheduled_for(self) -> Option<UnixMillis> {
        match self {
            Self::Due { scheduled_for, .. } => Some(scheduled_for),
            Self::SkippedNonexistentLocalTime { .. } => None,
        }
    }
}

/// Bounded calendar evaluation result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutomationScheduleEvaluation {
    /// Ordered local decisions after the exclusive lower bound.
    pub decisions: Vec<AutomationScheduleDecision>,
    /// True when at least one additional decision existed inside the requested window.
    pub truncated: bool,
}

/// Invalid schedule syntax, timezone, timestamp, or evaluation bound.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AutomationScheduleError {
    /// Schedule text exceeded the domain boundary.
    #[error("automation schedule exceeds {MAX_AUTOMATION_SCHEDULE_BYTES} bytes")]
    TooLong,
    /// Canonical v1 text was malformed or non-canonical.
    #[error("automation schedule is not canonical v1")]
    InvalidCanonical,
    /// Legacy normalization input was outside the one supported compatibility subset.
    #[error("automation schedule cannot be normalized")]
    InvalidNormalization,
    /// The timezone was not a recognized IANA timezone.
    #[error("automation timezone is invalid")]
    InvalidTimezone,
    /// A persisted or requested local calendar slot was invalid.
    #[error("automation local date and time are invalid")]
    InvalidLocalDateTime,
    /// A Unix timestamp could not be represented by the calendar implementation.
    #[error("automation timestamp is outside the supported calendar range")]
    TimestampOutOfRange,
    /// Evaluation bounds were reversed or exceeded the maximum window.
    #[error("automation schedule evaluation window is invalid")]
    InvalidWindow,
    /// The requested decision limit was zero or exceeded its bound.
    #[error("automation schedule decision limit is invalid")]
    InvalidLimit,
}

impl AutomationSchedule {
    /// Creates a validated schedule from typed fields.
    ///
    /// # Errors
    ///
    /// Returns [`AutomationScheduleError`] for an invalid weekday, month day, hour, or minute.
    pub fn new(
        cadence: AutomationCadence,
        hour: u8,
        minute: u8,
    ) -> Result<Self, AutomationScheduleError> {
        if hour > 23
            || minute > 59
            || matches!(cadence, AutomationCadence::Weekly { weekday } if weekday > 6)
            || matches!(cadence, AutomationCadence::Monthly { day_of_month } if !(1..=31).contains(&day_of_month))
        {
            return Err(AutomationScheduleError::InvalidCanonical);
        }
        Ok(Self {
            cadence,
            hour,
            minute,
        })
    }

    /// Parses only the exact canonical v1 representation.
    ///
    /// # Errors
    ///
    /// Returns [`AutomationScheduleError`] for aliases, whitespace, padding differences, unknown
    /// cadence, or values outside their bounds.
    pub fn parse_canonical(value: &str) -> Result<Self, AutomationScheduleError> {
        validate_schedule_length(value)?;
        let fields = value.split(';').collect::<Vec<_>>();
        let schedule = match fields.as_slice() {
            ["v1", "daily", time] => {
                let (hour, minute) = parse_time(time)?;
                Self::new(AutomationCadence::Daily, hour, minute)?
            }
            ["v1", "weekdays", time] => {
                let (hour, minute) = parse_time(time)?;
                Self::new(AutomationCadence::Weekdays, hour, minute)?
            }
            ["v1", "weekly", weekday, time] => {
                let (hour, minute) = parse_time(time)?;
                Self::new(
                    AutomationCadence::Weekly {
                        weekday: parse_canonical_u8(weekday)?,
                    },
                    hour,
                    minute,
                )?
            }
            ["v1", "monthly", day_of_month, time] => {
                let (hour, minute) = parse_time(time)?;
                Self::new(
                    AutomationCadence::Monthly {
                        day_of_month: parse_canonical_u8(day_of_month)?,
                    },
                    hour,
                    minute,
                )?
            }
            _ => return Err(AutomationScheduleError::InvalidCanonical),
        };
        if schedule.to_canonical_string() != value {
            return Err(AutomationScheduleError::InvalidCanonical);
        }
        Ok(schedule)
    }

    /// Parses canonical v1 or one narrowly bounded legacy format for normalization.
    ///
    /// Current renderer JSON and limited five-field cron are accepted only here. The result must
    /// be persisted through [`Self::to_canonical_string`]; persisted restore paths must call
    /// [`Self::parse_canonical`] instead.
    ///
    /// # Errors
    ///
    /// Returns [`AutomationScheduleError`] for unsupported JSON, cron, timezone disagreement, or
    /// invalid typed fields.
    pub fn parse_for_normalization(
        value: &str,
        timezone: &str,
    ) -> Result<Self, AutomationScheduleError> {
        validate_schedule_length(value)?;
        if value.chars().any(char::is_control) {
            return Err(AutomationScheduleError::InvalidNormalization);
        }
        parse_timezone(timezone)?;
        if value.starts_with("v1;") {
            Self::parse_canonical(value)
        } else if value.starts_with('{') {
            parse_renderer_json(value, timezone)
        } else {
            parse_limited_cron(value)
        }
    }

    /// Encodes the only representation accepted by [`Self::parse_canonical`].
    #[must_use]
    pub fn to_canonical_string(self) -> String {
        match self.cadence {
            AutomationCadence::Daily => format!("v1;daily;{:02}:{:02}", self.hour, self.minute),
            AutomationCadence::Weekdays => {
                format!("v1;weekdays;{:02}:{:02}", self.hour, self.minute)
            }
            AutomationCadence::Weekly { weekday } => {
                format!("v1;weekly;{weekday};{:02}:{:02}", self.hour, self.minute)
            }
            AutomationCadence::Monthly { day_of_month } => format!(
                "v1;monthly;{day_of_month};{:02}:{:02}",
                self.hour, self.minute
            ),
        }
    }

    /// Binds this schedule to its timezone and calculator rules.
    ///
    /// # Errors
    ///
    /// Returns [`AutomationScheduleError`] unless `timezone` is recognized.
    pub fn fingerprint(
        self,
        timezone: &str,
    ) -> Result<AutomationScheduleFingerprint, AutomationScheduleError> {
        let timezone = parse_timezone(timezone)?;
        let mut hasher = Sha256::new();
        hasher.update(b"grok-desktop/automation-schedule\0");
        hasher.update(AUTOMATION_SCHEDULE_CALCULATOR_VERSION.to_be_bytes());
        hasher.update([0]);
        hasher.update(self.to_canonical_string().as_bytes());
        hasher.update([0]);
        hasher.update(timezone.name().as_bytes());
        Ok(AutomationScheduleFingerprint(hasher.finalize().into()))
    }

    /// Returns the first calendar decision strictly after one UTC timestamp.
    ///
    /// # Errors
    ///
    /// Returns [`AutomationScheduleError`] for invalid timezone or timestamp bounds.
    pub fn next_decision_after(
        self,
        timezone: &str,
        after: UnixMillis,
    ) -> Result<AutomationScheduleDecision, AutomationScheduleError> {
        let through = after
            .checked_add(MAX_AUTOMATION_SCHEDULE_WINDOW_DAYS * MILLIS_PER_DAY)
            .ok_or(AutomationScheduleError::TimestampOutOfRange)?;
        self.decisions_between(timezone, after, through, 1)?
            .decisions
            .into_iter()
            .next()
            .ok_or(AutomationScheduleError::TimestampOutOfRange)
    }

    /// Calculates ordered decisions inside one bounded UTC window.
    ///
    /// The lower bound is exclusive and the upper bound inclusive. Monthly dates absent from a
    /// month produce no decision for that month. DST gaps produce an explicit skipped decision;
    /// folds resolve once to the earlier UTC instant.
    ///
    /// # Errors
    ///
    /// Returns [`AutomationScheduleError`] for invalid bounds, timezone, timestamps, or limit.
    pub fn decisions_between(
        self,
        timezone: &str,
        after_exclusive: UnixMillis,
        through_inclusive: UnixMillis,
        limit: usize,
    ) -> Result<AutomationScheduleEvaluation, AutomationScheduleError> {
        if !(1..=MAX_AUTOMATION_SCHEDULE_DECISIONS).contains(&limit) {
            return Err(AutomationScheduleError::InvalidLimit);
        }
        let span = through_inclusive
            .checked_sub(after_exclusive)
            .ok_or(AutomationScheduleError::InvalidWindow)?;
        if span > MAX_AUTOMATION_SCHEDULE_WINDOW_DAYS * MILLIS_PER_DAY {
            return Err(AutomationScheduleError::InvalidWindow);
        }
        let timezone = parse_timezone(timezone)?;
        let after = utc_from_millis(after_exclusive)?;
        let through = utc_from_millis(through_inclusive)?;
        let after_local = after.with_timezone(&timezone).naive_local();
        let through_local = through.with_timezone(&timezone).naive_local();
        let mut date = after_local.date();
        let end_date = through_local.date();
        let mut decisions = Vec::with_capacity(limit.min(32).saturating_add(1));

        while date <= end_date {
            if self.matches(date) {
                let nominal = AutomationLocalDateTime::from_date(date, self.hour, self.minute);
                let local = nominal.to_naive()?;
                match timezone.from_local_datetime(&local) {
                    LocalResult::Single(value) => push_resolved_decision(
                        &mut decisions,
                        nominal,
                        value,
                        after_exclusive,
                        through_inclusive,
                    )?,
                    LocalResult::Ambiguous(first, second) => {
                        let value = if first.timestamp_millis() <= second.timestamp_millis() {
                            first
                        } else {
                            second
                        };
                        push_resolved_decision(
                            &mut decisions,
                            nominal,
                            value,
                            after_exclusive,
                            through_inclusive,
                        )?;
                    }
                    LocalResult::None if local > after_local && local <= through_local => {
                        decisions.push(AutomationScheduleDecision::SkippedNonexistentLocalTime {
                            nominal_local: nominal,
                        });
                    }
                    LocalResult::None => {}
                }
                if decisions.len() > limit {
                    decisions.truncate(limit);
                    return Ok(AutomationScheduleEvaluation {
                        decisions,
                        truncated: true,
                    });
                }
            }
            date = date
                .succ_opt()
                .ok_or(AutomationScheduleError::TimestampOutOfRange)?;
        }

        Ok(AutomationScheduleEvaluation {
            decisions,
            truncated: false,
        })
    }

    /// Resolves one exact nominal local slot under this schedule and timezone.
    ///
    /// This is used when rehydrating occurrences so persisted UTC instants cannot disagree with
    /// their immutable schedule. A DST fold resolves to the earlier UTC instant and a gap remains
    /// an explicit skipped decision.
    ///
    /// # Errors
    ///
    /// Returns [`AutomationScheduleError`] unless the nominal slot belongs to this cadence and
    /// has the exact configured hour and minute.
    pub fn decision_for_nominal(
        self,
        timezone: &str,
        nominal_local: AutomationLocalDateTime,
    ) -> Result<AutomationScheduleDecision, AutomationScheduleError> {
        let local = nominal_local.to_naive()?;
        if nominal_local.hour != self.hour
            || nominal_local.minute != self.minute
            || !self.matches(local.date())
        {
            return Err(AutomationScheduleError::InvalidLocalDateTime);
        }
        let timezone = parse_timezone(timezone)?;
        match timezone.from_local_datetime(&local) {
            LocalResult::Single(value) => Ok(AutomationScheduleDecision::Due {
                nominal_local,
                scheduled_for: positive_timestamp(value)?,
            }),
            LocalResult::Ambiguous(first, second) => {
                let value = if first.timestamp_millis() <= second.timestamp_millis() {
                    first
                } else {
                    second
                };
                Ok(AutomationScheduleDecision::Due {
                    nominal_local,
                    scheduled_for: positive_timestamp(value)?,
                })
            }
            LocalResult::None => {
                Ok(AutomationScheduleDecision::SkippedNonexistentLocalTime { nominal_local })
            }
        }
    }

    fn matches(self, date: NaiveDate) -> bool {
        match self.cadence {
            AutomationCadence::Daily => true,
            AutomationCadence::Weekdays => !matches!(date.weekday(), Weekday::Sat | Weekday::Sun),
            AutomationCadence::Weekly { weekday } => {
                u8::try_from(date.weekday().num_days_from_sunday()).expect("weekday is bounded")
                    == weekday
            }
            AutomationCadence::Monthly { day_of_month } => date.day() == u32::from(day_of_month),
        }
    }
}

impl fmt::Display for AutomationSchedule {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.to_canonical_string())
    }
}

impl FromStr for AutomationSchedule {
    type Err = AutomationScheduleError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse_canonical(value)
    }
}

fn validate_schedule_length(value: &str) -> Result<(), AutomationScheduleError> {
    if value.is_empty() {
        return Err(AutomationScheduleError::InvalidCanonical);
    }
    if value.len() > MAX_AUTOMATION_SCHEDULE_BYTES {
        return Err(AutomationScheduleError::TooLong);
    }
    Ok(())
}

fn parse_time(value: &str) -> Result<(u8, u8), AutomationScheduleError> {
    let bytes = value.as_bytes();
    if bytes.len() != 5
        || bytes[2] != b':'
        || !bytes[..2].iter().all(u8::is_ascii_digit)
        || !bytes[3..].iter().all(u8::is_ascii_digit)
    {
        return Err(AutomationScheduleError::InvalidCanonical);
    }
    let hour = value[..2]
        .parse()
        .map_err(|_| AutomationScheduleError::InvalidCanonical)?;
    let minute = value[3..]
        .parse()
        .map_err(|_| AutomationScheduleError::InvalidCanonical)?;
    Ok((hour, minute))
}

fn parse_canonical_u8(value: &str) -> Result<u8, AutomationScheduleError> {
    let parsed = value
        .parse::<u8>()
        .map_err(|_| AutomationScheduleError::InvalidCanonical)?;
    if parsed.to_string() != value {
        return Err(AutomationScheduleError::InvalidCanonical);
    }
    Ok(parsed)
}

fn parse_timezone(value: &str) -> Result<Tz, AutomationScheduleError> {
    value
        .parse()
        .map_err(|_| AutomationScheduleError::InvalidTimezone)
}

fn parse_renderer_json(
    value: &str,
    timezone: &str,
) -> Result<AutomationSchedule, AutomationScheduleError> {
    let value: Value =
        serde_json::from_str(value).map_err(|_| AutomationScheduleError::InvalidNormalization)?;
    let object = value
        .as_object()
        .ok_or(AutomationScheduleError::InvalidNormalization)?;
    if object
        .keys()
        .any(|key| !RENDERER_JSON_FIELDS.contains(&key.as_str()))
    {
        return Err(AutomationScheduleError::InvalidNormalization);
    }
    let json_timezone = required_string(object, "timeZoneIana")?;
    if parse_timezone(json_timezone)?.name() != parse_timezone(timezone)?.name() {
        return Err(AutomationScheduleError::InvalidNormalization);
    }
    if let Some(value) = object.get("timeZoneWindows") {
        let value = value
            .as_str()
            .ok_or(AutomationScheduleError::InvalidNormalization)?;
        if value.trim().is_empty() || value.len() > 128 || value.chars().any(char::is_control) {
            return Err(AutomationScheduleError::InvalidNormalization);
        }
    }
    let (hour, minute) = parse_time(required_string(object, "localTime")?)
        .map_err(|_| AutomationScheduleError::InvalidNormalization)?;
    let weekday = optional_json_u8(object, "weekday")?;
    let day_of_month = optional_json_u8(object, "dayOfMonth")?;
    let cadence = match (required_string(object, "frequency")?, weekday, day_of_month) {
        ("daily", None, None) => AutomationCadence::Daily,
        ("weekdays", None, None) => AutomationCadence::Weekdays,
        ("weekly", Some(weekday), None) => AutomationCadence::Weekly { weekday },
        ("monthly", None, Some(day_of_month)) => AutomationCadence::Monthly { day_of_month },
        _ => return Err(AutomationScheduleError::InvalidNormalization),
    };
    AutomationSchedule::new(cadence, hour, minute)
        .map_err(|_| AutomationScheduleError::InvalidNormalization)
}

fn required_string<'a>(
    object: &'a Map<String, Value>,
    key: &str,
) -> Result<&'a str, AutomationScheduleError> {
    object
        .get(key)
        .and_then(Value::as_str)
        .ok_or(AutomationScheduleError::InvalidNormalization)
}

fn optional_json_u8(
    object: &Map<String, Value>,
    key: &str,
) -> Result<Option<u8>, AutomationScheduleError> {
    object
        .get(key)
        .map(|value| {
            value
                .as_u64()
                .and_then(|value| u8::try_from(value).ok())
                .ok_or(AutomationScheduleError::InvalidNormalization)
        })
        .transpose()
}

fn parse_limited_cron(value: &str) -> Result<AutomationSchedule, AutomationScheduleError> {
    let fields = value.split_ascii_whitespace().collect::<Vec<_>>();
    let [minute, hour, day_of_month, month, weekday] = fields.as_slice() else {
        return Err(AutomationScheduleError::InvalidNormalization);
    };
    let minute = parse_cron_number(minute, 0, 59)?;
    let hour = parse_cron_number(hour, 0, 23)?;
    if *month != "*" {
        return Err(AutomationScheduleError::InvalidNormalization);
    }
    let cadence = match (*day_of_month, *weekday) {
        ("*", "*") => AutomationCadence::Daily,
        ("*", "1-5") => AutomationCadence::Weekdays,
        ("*", value) => AutomationCadence::Weekly {
            weekday: parse_cron_number(value, 0, 6)?,
        },
        (value, "*") => AutomationCadence::Monthly {
            day_of_month: parse_cron_number(value, 1, 31)?,
        },
        _ => return Err(AutomationScheduleError::InvalidNormalization),
    };
    AutomationSchedule::new(cadence, hour, minute)
        .map_err(|_| AutomationScheduleError::InvalidNormalization)
}

fn parse_cron_number(value: &str, minimum: u8, maximum: u8) -> Result<u8, AutomationScheduleError> {
    let parsed = value
        .parse::<u8>()
        .map_err(|_| AutomationScheduleError::InvalidNormalization)?;
    if !(minimum..=maximum).contains(&parsed) || parsed.to_string() != value {
        return Err(AutomationScheduleError::InvalidNormalization);
    }
    Ok(parsed)
}

fn utc_from_millis(value: UnixMillis) -> Result<DateTime<Utc>, AutomationScheduleError> {
    let value = i64::try_from(value).map_err(|_| AutomationScheduleError::TimestampOutOfRange)?;
    DateTime::from_timestamp_millis(value).ok_or(AutomationScheduleError::TimestampOutOfRange)
}

fn push_resolved_decision(
    decisions: &mut Vec<AutomationScheduleDecision>,
    nominal_local: AutomationLocalDateTime,
    value: DateTime<Tz>,
    after_exclusive: UnixMillis,
    through_inclusive: UnixMillis,
) -> Result<(), AutomationScheduleError> {
    let scheduled_for = positive_timestamp(value)?;
    if scheduled_for > after_exclusive && scheduled_for <= through_inclusive {
        decisions.push(AutomationScheduleDecision::Due {
            nominal_local,
            scheduled_for,
        });
    }
    Ok(())
}

fn positive_timestamp(value: DateTime<Tz>) -> Result<UnixMillis, AutomationScheduleError> {
    u64::try_from(value.timestamp_millis())
        .map_err(|_| AutomationScheduleError::TimestampOutOfRange)
}

/// Immutable automation definition material owned by one durable occurrence.
///
/// A future executor consumes this snapshot and never reloads mutable title, prompt, schedule,
/// timezone, or policy fields from the automation definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutomationExecutionSnapshot {
    /// Automation revision captured when the occurrence was materialized.
    pub definition_revision: u64,
    /// Immutable owning project used by future run creation.
    pub project_id: ProjectId,
    /// User-visible title frozen for this occurrence.
    pub title: String,
    /// Exact bounded prompt frozen for this occurrence.
    pub prompt: String,
    /// Parsed canonical schedule.
    pub schedule: AutomationSchedule,
    /// Exact canonical v1 representation retained for persistence and audit.
    pub canonical_schedule: String,
    /// Canonical IANA timezone name.
    pub timezone: String,
    /// Frozen missed-run behavior.
    pub missed_run_policy: MissedRunPolicy,
    /// Frozen overlap behavior.
    pub overlap_policy: OverlapPolicy,
    /// Binding of calculator version, schedule, and timezone.
    pub schedule_fingerprint: AutomationScheduleFingerprint,
    /// Calculator semantics used to materialize this snapshot.
    pub calculator_version: u32,
}

impl AutomationExecutionSnapshot {
    /// Creates an immutable execution snapshot from canonical schedule text.
    ///
    /// # Errors
    ///
    /// Returns [`AutomationSchedulerError`] for invalid text, schedule, or timezone.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        definition_revision: u64,
        project_id: ProjectId,
        title: String,
        prompt: String,
        canonical_schedule: String,
        timezone: String,
        missed_run_policy: MissedRunPolicy,
        overlap_policy: OverlapPolicy,
    ) -> Result<Self, AutomationSchedulerError> {
        validate_execution_text(&title, &prompt)?;
        let schedule = AutomationSchedule::parse_canonical(&canonical_schedule)?;
        let timezone = canonicalize_timezone(timezone)?;
        let schedule_fingerprint = schedule.fingerprint(&timezone)?;
        Ok(Self {
            definition_revision,
            project_id,
            title,
            prompt,
            schedule,
            canonical_schedule,
            timezone,
            missed_run_policy,
            overlap_policy,
            schedule_fingerprint,
            calculator_version: AUTOMATION_SCHEDULE_CALCULATOR_VERSION,
        })
    }

    /// Rehydrates a snapshot after validating all redundant immutable bindings.
    ///
    /// # Errors
    ///
    /// Returns [`AutomationSchedulerError::InvalidPersistedState`] when any field is malformed or
    /// disagrees with the canonical schedule, timezone, fingerprint, or calculator version.
    pub fn restore(snapshot: Self) -> Result<Self, AutomationSchedulerError> {
        let valid = validate_execution_text(&snapshot.title, &snapshot.prompt).is_ok()
            && AutomationSchedule::parse_canonical(&snapshot.canonical_schedule)
                .is_ok_and(|schedule| schedule == snapshot.schedule)
            && parse_timezone(&snapshot.timezone)
                .is_ok_and(|timezone| timezone.name() == snapshot.timezone)
            && snapshot.calculator_version == AUTOMATION_SCHEDULE_CALCULATOR_VERSION
            && snapshot
                .schedule
                .fingerprint(&snapshot.timezone)
                .is_ok_and(|fingerprint| fingerprint == snapshot.schedule_fingerprint);
        if !valid {
            return Err(AutomationSchedulerError::InvalidPersistedState);
        }
        Ok(snapshot)
    }
}

/// Durable scheduler cursor and next calendar decision for one definition revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutomationScheduleCursor {
    /// Owning automation.
    pub automation_id: AutomationId,
    /// Definition revision this cursor interprets.
    pub definition_revision: u64,
    /// Exact schedule/timezone binding.
    pub schedule_fingerprint: AutomationScheduleFingerprint,
    /// Calculator rules used for the cursor.
    pub calculator_version: u32,
    /// Greatest wall-clock timestamp durably evaluated by a tick.
    pub evaluated_through: UnixMillis,
    /// Next local decision, absent only at the supported calendar boundary.
    pub next_decision: Option<AutomationScheduleDecision>,
    /// Optimistic cursor revision.
    pub revision: u64,
    /// Cursor creation time.
    pub created_at: UnixMillis,
    /// Last durable advancement time.
    pub updated_at: UnixMillis,
}

impl AutomationScheduleCursor {
    /// Creates the first cursor for one immutable execution snapshot.
    ///
    /// # Errors
    ///
    /// Returns [`AutomationSchedulerError`] when the next due instant is not after the evaluated
    /// watermark or the snapshot is corrupt.
    pub fn new(
        automation_id: AutomationId,
        snapshot: &AutomationExecutionSnapshot,
        evaluated_through: UnixMillis,
        next_decision: Option<AutomationScheduleDecision>,
        now: UnixMillis,
    ) -> Result<Self, AutomationSchedulerError> {
        AutomationExecutionSnapshot::restore(snapshot.clone())?;
        if now < evaluated_through {
            return Err(AutomationSchedulerError::ClockRegression);
        }
        validate_next_decision(next_decision, evaluated_through)?;
        Ok(Self {
            automation_id,
            definition_revision: snapshot.definition_revision,
            schedule_fingerprint: snapshot.schedule_fingerprint,
            calculator_version: snapshot.calculator_version,
            evaluated_through,
            next_decision,
            revision: 0,
            created_at: now,
            updated_at: now,
        })
    }

    /// Rehydrates a persisted cursor after validating its complete shape.
    ///
    /// # Errors
    ///
    /// Returns [`AutomationSchedulerError::InvalidPersistedState`] for inconsistent fields.
    pub fn restore(cursor: Self) -> Result<Self, AutomationSchedulerError> {
        if cursor.calculator_version == 0
            || cursor.schedule_fingerprint.as_bytes() == &[0; 32]
            || cursor.updated_at < cursor.created_at
            || cursor.updated_at < cursor.evaluated_through
            || (cursor.revision == 0 && cursor.updated_at != cursor.created_at)
            || validate_next_decision(cursor.next_decision, cursor.evaluated_through).is_err()
        {
            return Err(AutomationSchedulerError::InvalidPersistedState);
        }
        Ok(cursor)
    }

    /// Advances the durable evaluation watermark and next decision.
    ///
    /// # Errors
    ///
    /// Returns [`AutomationSchedulerError`] for clock regression, an invalid next decision, or
    /// exhausted revision.
    pub fn advance(
        &mut self,
        evaluated_through: UnixMillis,
        next_decision: Option<AutomationScheduleDecision>,
        now: UnixMillis,
    ) -> Result<(), AutomationSchedulerError> {
        if evaluated_through < self.evaluated_through
            || now < self.updated_at
            || now < evaluated_through
        {
            return Err(AutomationSchedulerError::ClockRegression);
        }
        validate_next_decision(next_decision, evaluated_through)?;
        advance_revision(&mut self.revision)?;
        self.evaluated_through = evaluated_through;
        self.next_decision = next_decision;
        self.updated_at = now;
        Ok(())
    }
}

/// Durable lifecycle of one logical scheduled local slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AutomationOccurrenceState {
    /// Eligible work exists but has not been claimed by a daemon owner.
    Pending,
    /// One overlap is retained behind the active occurrence.
    QueuedOverlap,
    /// A fenced daemon owner holds a bounded volatile claim; no run exists yet.
    Claimed,
    /// A durable run is linked. Lease expiry must never cause another run dispatch.
    RunLinked,
    /// The linked run completed successfully.
    Succeeded,
    /// The linked run completed with a known failure.
    Failed,
    /// Missed-run policy explicitly skipped this logical slot or collapsed window.
    SkippedMissed,
    /// Overlap policy explicitly skipped this logical slot.
    SkippedOverlap,
    /// The nominal local slot did not exist during a DST gap and was never shifted.
    SkippedInvalidLocalTime,
    /// Claim exhaustion or an uncertain linked run requires explicit review.
    InterruptedNeedsReview,
    /// The occurrence was cancelled before successful completion.
    Cancelled,
}

impl AutomationOccurrenceState {
    /// Returns whether the occurrence accepts no automatic transition.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded
                | Self::Failed
                | Self::SkippedMissed
                | Self::SkippedOverlap
                | Self::SkippedInvalidLocalTime
                | Self::InterruptedNeedsReview
                | Self::Cancelled
        )
    }
}

/// Store-level uniqueness identity for one nominal local slot and definition revision.
///
/// DST-gap skips use this key directly and never invent a UTC timestamp.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AutomationOccurrenceSlot {
    /// Definition revision whose immutable schedule produced the slot.
    pub definition_revision: u64,
    /// Exact nominal local calendar slot.
    pub nominal_local: AutomationLocalDateTime,
}

/// Exact fenced volatile claim retained on a claimed or run-linked occurrence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutomationOccurrenceClaim {
    /// Daemon process that acquired the claim.
    pub owner_id: AutomationSchedulerOwnerId,
    /// Global scheduler fencing generation.
    pub fence: u64,
    /// Claim acquisition time.
    pub claimed_at: UnixMillis,
    /// Exclusive wall-clock expiry for an unlinked claim.
    pub expires_at: UnixMillis,
}

/// Durable source of truth for one scheduled automation occurrence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutomationOccurrence {
    /// Stable occurrence journal identifier.
    pub id: AutomationOccurrenceId,
    /// Owning automation.
    pub automation_id: AutomationId,
    /// Immutable definition material; execution never reloads mutable definition fields.
    pub snapshot: AutomationExecutionSnapshot,
    /// Logical local slot used for uniqueness, including DST gaps.
    pub nominal_local: AutomationLocalDateTime,
    /// Frozen UTC instant, absent only for a nonexistent DST local time.
    pub scheduled_for: Option<UnixMillis>,
    /// Number of missed slots represented by a bounded collapsed decision.
    pub occurrence_count: u32,
    /// Current journal lifecycle state.
    pub state: AutomationOccurrenceState,
    /// Current volatile claim, retained through run linking but cleared at terminalization.
    pub claim: Option<AutomationOccurrenceClaim>,
    /// Durable linked run, absent before exact run creation.
    pub run_id: Option<RunId>,
    /// Number of claims ever acquired, bounded before wrap or infinite retry.
    pub claim_attempt_count: u32,
    /// Optimistic journal revision.
    pub revision: u64,
    /// Materialization time.
    pub created_at: UnixMillis,
    /// Last successful lifecycle transition time.
    pub updated_at: UnixMillis,
}

impl AutomationOccurrence {
    /// Materializes a due occurrence in the pending state.
    ///
    /// # Errors
    ///
    /// Returns [`AutomationSchedulerError`] unless the decision has a resolved UTC instant, the
    /// snapshot is valid, and `occurrence_count` is positive.
    pub fn pending(
        id: AutomationOccurrenceId,
        automation_id: AutomationId,
        snapshot: AutomationExecutionSnapshot,
        decision: AutomationScheduleDecision,
        occurrence_count: u32,
        now: UnixMillis,
    ) -> Result<Self, AutomationSchedulerError> {
        let AutomationScheduleDecision::Due {
            nominal_local,
            scheduled_for,
        } = decision
        else {
            return Err(AutomationSchedulerError::InvalidDecision);
        };
        Self::materialize(
            id,
            automation_id,
            snapshot,
            nominal_local,
            Some(scheduled_for),
            occurrence_count,
            AutomationOccurrenceState::Pending,
            now,
        )
    }

    /// Materializes an explicit terminal DST-gap skip with no synthetic UTC timestamp.
    ///
    /// # Errors
    ///
    /// Returns [`AutomationSchedulerError`] unless the decision is a nonexistent local time and
    /// the snapshot is valid.
    pub fn skipped_invalid_local_time(
        id: AutomationOccurrenceId,
        automation_id: AutomationId,
        snapshot: AutomationExecutionSnapshot,
        decision: AutomationScheduleDecision,
        now: UnixMillis,
    ) -> Result<Self, AutomationSchedulerError> {
        let AutomationScheduleDecision::SkippedNonexistentLocalTime { nominal_local } = decision
        else {
            return Err(AutomationSchedulerError::InvalidDecision);
        };
        Self::materialize(
            id,
            automation_id,
            snapshot,
            nominal_local,
            None,
            1,
            AutomationOccurrenceState::SkippedInvalidLocalTime,
            now,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn materialize(
        id: AutomationOccurrenceId,
        automation_id: AutomationId,
        snapshot: AutomationExecutionSnapshot,
        nominal_local: AutomationLocalDateTime,
        scheduled_for: Option<UnixMillis>,
        occurrence_count: u32,
        state: AutomationOccurrenceState,
        now: UnixMillis,
    ) -> Result<Self, AutomationSchedulerError> {
        AutomationExecutionSnapshot::restore(snapshot.clone())?;
        nominal_local.to_naive()?;
        if occurrence_count == 0 {
            return Err(AutomationSchedulerError::InvalidDecision);
        }
        let occurrence = Self {
            id,
            automation_id,
            snapshot,
            nominal_local,
            scheduled_for,
            occurrence_count,
            state,
            claim: None,
            run_id: None,
            claim_attempt_count: 0,
            revision: 0,
            created_at: now,
            updated_at: now,
        };
        Self::restore(occurrence)
    }

    /// Rehydrates a persisted occurrence after checking lifecycle and immutable bindings.
    ///
    /// # Errors
    ///
    /// Returns [`AutomationSchedulerError::InvalidPersistedState`] for an unreachable shape.
    pub fn restore(occurrence: Self) -> Result<Self, AutomationSchedulerError> {
        if AutomationExecutionSnapshot::restore(occurrence.snapshot.clone()).is_err()
            || occurrence.nominal_local.to_naive().is_err()
            || occurrence.occurrence_count == 0
            || occurrence.claim_attempt_count > MAX_AUTOMATION_OCCURRENCE_CLAIM_ATTEMPTS
            || occurrence.updated_at < occurrence.created_at
            || !occurrence.reachable_shape()
        {
            return Err(AutomationSchedulerError::InvalidPersistedState);
        }
        Ok(occurrence)
    }

    /// Returns the exact logical slot key required for durable uniqueness.
    #[must_use]
    pub const fn slot(&self) -> AutomationOccurrenceSlot {
        AutomationOccurrenceSlot {
            definition_revision: self.snapshot.definition_revision,
            nominal_local: self.nominal_local,
        }
    }

    /// Retains this pending occurrence as the one allowed overlap.
    ///
    /// # Errors
    ///
    /// Returns [`AutomationSchedulerError`] unless the occurrence is pending and time is monotonic.
    pub fn queue_overlap(&mut self, now: UnixMillis) -> Result<(), AutomationSchedulerError> {
        if self.snapshot.overlap_policy != OverlapPolicy::QueueOne {
            return Err(self.invalid_transition(AutomationOccurrenceState::QueuedOverlap));
        }
        self.move_from(
            AutomationOccurrenceState::Pending,
            AutomationOccurrenceState::QueuedOverlap,
            now,
        )
    }

    /// Promotes the retained overlap after the active occurrence terminalizes.
    ///
    /// # Errors
    ///
    /// Returns [`AutomationSchedulerError`] unless this is the queued occurrence.
    pub fn promote_queued(&mut self, now: UnixMillis) -> Result<(), AutomationSchedulerError> {
        self.move_from(
            AutomationOccurrenceState::QueuedOverlap,
            AutomationOccurrenceState::Pending,
            now,
        )
    }

    /// Terminalizes a pending decision under missed-run Skip policy.
    ///
    /// # Errors
    ///
    /// Returns [`AutomationSchedulerError`] unless no run or claim exists.
    pub fn skip_missed(&mut self, now: UnixMillis) -> Result<(), AutomationSchedulerError> {
        if self.snapshot.missed_run_policy != MissedRunPolicy::Skip {
            return Err(self.invalid_transition(AutomationOccurrenceState::SkippedMissed));
        }
        self.move_unclaimed_to(AutomationOccurrenceState::SkippedMissed, now)
    }

    /// Terminalizes a pending or queued decision under overlap Skip policy.
    ///
    /// # Errors
    ///
    /// Returns [`AutomationSchedulerError`] unless no run or claim exists.
    pub fn skip_overlap(&mut self, now: UnixMillis) -> Result<(), AutomationSchedulerError> {
        self.move_unclaimed_to(AutomationOccurrenceState::SkippedOverlap, now)
    }

    /// Acquires a bounded fenced volatile claim and increments its attempt counter.
    ///
    /// # Errors
    ///
    /// Returns [`AutomationSchedulerError`] unless pending, the token is nonzero, the claim has a
    /// positive lifetime, time is monotonic, and attempts remain below the bound.
    pub fn claim(
        &mut self,
        token: &AutomationSchedulerLeaseToken,
        claimed_at: UnixMillis,
        expires_at: UnixMillis,
    ) -> Result<(), AutomationSchedulerError> {
        if self.state != AutomationOccurrenceState::Pending {
            return Err(self.invalid_transition(AutomationOccurrenceState::Claimed));
        }
        if self
            .scheduled_for
            .is_some_and(|scheduled_for| claimed_at < scheduled_for)
        {
            return Err(AutomationSchedulerError::InvalidDecision);
        }
        if token.fence == 0
            || expires_at <= claimed_at
            || expires_at - claimed_at > MAX_AUTOMATION_SCHEDULER_LEASE_MS
        {
            return Err(AutomationSchedulerError::InvalidLease);
        }
        if self.claim_attempt_count >= MAX_AUTOMATION_OCCURRENCE_CLAIM_ATTEMPTS {
            return Err(AutomationSchedulerError::ClaimAttemptsExhausted);
        }
        self.ensure_time(claimed_at)?;
        let next_attempt = self
            .claim_attempt_count
            .checked_add(1)
            .ok_or(AutomationSchedulerError::ClaimAttemptsExhausted)?;
        advance_revision(&mut self.revision)?;
        self.claim_attempt_count = next_attempt;
        self.claim = Some(AutomationOccurrenceClaim {
            owner_id: token.owner_id.clone(),
            fence: token.fence,
            claimed_at,
            expires_at,
        });
        self.state = AutomationOccurrenceState::Claimed;
        self.updated_at = claimed_at;
        Ok(())
    }

    /// Returns an expired unlinked claim to pending without dispatching work.
    ///
    /// # Errors
    ///
    /// Returns [`AutomationSchedulerError`] unless an unlinked claim is expired.
    pub fn release_expired_claim(
        &mut self,
        now: UnixMillis,
    ) -> Result<(), AutomationSchedulerError> {
        let claim = self
            .claim
            .as_ref()
            .ok_or(AutomationSchedulerError::InvalidPersistedState)?;
        if self.state != AutomationOccurrenceState::Claimed
            || self.run_id.is_some()
            || now < claim.expires_at
        {
            return Err(self.invalid_transition(AutomationOccurrenceState::Pending));
        }
        self.ensure_time(now)?;
        advance_revision(&mut self.revision)?;
        self.claim = None;
        self.state = AutomationOccurrenceState::Pending;
        self.updated_at = now;
        Ok(())
    }

    /// Links one exact durable run while the matching fenced claim remains live.
    ///
    /// # Errors
    ///
    /// Returns [`AutomationSchedulerError`] for a stale token, expired claim, wrong state, or
    /// clock/revision failure.
    pub fn link_run(
        &mut self,
        token: &AutomationSchedulerLeaseToken,
        run_id: RunId,
        now: UnixMillis,
    ) -> Result<(), AutomationSchedulerError> {
        let claim = self
            .claim
            .as_ref()
            .ok_or(AutomationSchedulerError::InvalidPersistedState)?;
        if self.state != AutomationOccurrenceState::Claimed
            || claim.owner_id != token.owner_id
            || claim.fence != token.fence
            || now >= claim.expires_at
        {
            return Err(AutomationSchedulerError::StaleFence);
        }
        self.ensure_time(now)?;
        advance_revision(&mut self.revision)?;
        self.run_id = Some(run_id);
        self.state = AutomationOccurrenceState::RunLinked;
        self.updated_at = now;
        Ok(())
    }

    /// Records known successful completion of the exact linked run.
    ///
    /// # Errors
    ///
    /// Returns [`AutomationSchedulerError`] unless `run_id` is the linked run.
    pub fn succeed(
        &mut self,
        run_id: &RunId,
        now: UnixMillis,
    ) -> Result<(), AutomationSchedulerError> {
        self.finish_linked(run_id, AutomationOccurrenceState::Succeeded, now)
    }

    /// Records known failure of the exact linked run.
    ///
    /// # Errors
    ///
    /// Returns [`AutomationSchedulerError`] unless `run_id` is the linked run.
    pub fn fail(
        &mut self,
        run_id: &RunId,
        now: UnixMillis,
    ) -> Result<(), AutomationSchedulerError> {
        self.finish_linked(run_id, AutomationOccurrenceState::Failed, now)
    }

    /// Records that the exact linked run requires explicit review and must not be replayed.
    ///
    /// # Errors
    ///
    /// Returns [`AutomationSchedulerError`] unless `run_id` is the linked run.
    pub fn interrupt(
        &mut self,
        run_id: &RunId,
        now: UnixMillis,
    ) -> Result<(), AutomationSchedulerError> {
        self.finish_linked(
            run_id,
            AutomationOccurrenceState::InterruptedNeedsReview,
            now,
        )
    }

    /// Terminalizes a repeatedly reclaimed pending occurrence for explicit review.
    ///
    /// # Errors
    ///
    /// Returns [`AutomationSchedulerError`] unless the claim-attempt bound is exhausted.
    pub fn mark_claims_exhausted(
        &mut self,
        now: UnixMillis,
    ) -> Result<(), AutomationSchedulerError> {
        if self.state != AutomationOccurrenceState::Pending
            || self.claim.is_some()
            || self.run_id.is_some()
            || self.claim_attempt_count < MAX_AUTOMATION_OCCURRENCE_CLAIM_ATTEMPTS
        {
            return Err(self.invalid_transition(AutomationOccurrenceState::InterruptedNeedsReview));
        }
        self.ensure_time(now)?;
        advance_revision(&mut self.revision)?;
        self.state = AutomationOccurrenceState::InterruptedNeedsReview;
        self.updated_at = now;
        Ok(())
    }

    /// Cancels a nonterminal occurrence without ever replaying a linked run.
    ///
    /// # Errors
    ///
    /// Returns [`AutomationSchedulerError`] for a terminal state or clock/revision failure.
    pub fn cancel(&mut self, now: UnixMillis) -> Result<(), AutomationSchedulerError> {
        if self.state.is_terminal() {
            return Err(self.invalid_transition(AutomationOccurrenceState::Cancelled));
        }
        self.ensure_time(now)?;
        advance_revision(&mut self.revision)?;
        self.claim = None;
        self.state = AutomationOccurrenceState::Cancelled;
        self.updated_at = now;
        Ok(())
    }

    fn finish_linked(
        &mut self,
        run_id: &RunId,
        next: AutomationOccurrenceState,
        now: UnixMillis,
    ) -> Result<(), AutomationSchedulerError> {
        if self.state != AutomationOccurrenceState::RunLinked
            || self.run_id.as_ref() != Some(run_id)
        {
            return Err(self.invalid_transition(next));
        }
        self.ensure_time(now)?;
        advance_revision(&mut self.revision)?;
        self.claim = None;
        self.state = next;
        self.updated_at = now;
        Ok(())
    }

    fn move_unclaimed_to(
        &mut self,
        next: AutomationOccurrenceState,
        now: UnixMillis,
    ) -> Result<(), AutomationSchedulerError> {
        if !matches!(
            self.state,
            AutomationOccurrenceState::Pending | AutomationOccurrenceState::QueuedOverlap
        ) || self.claim.is_some()
            || self.run_id.is_some()
        {
            return Err(self.invalid_transition(next));
        }
        self.ensure_time(now)?;
        advance_revision(&mut self.revision)?;
        self.state = next;
        self.updated_at = now;
        Ok(())
    }

    fn move_from(
        &mut self,
        from: AutomationOccurrenceState,
        next: AutomationOccurrenceState,
        now: UnixMillis,
    ) -> Result<(), AutomationSchedulerError> {
        if self.state != from || self.claim.is_some() || self.run_id.is_some() {
            return Err(self.invalid_transition(next));
        }
        self.ensure_time(now)?;
        advance_revision(&mut self.revision)?;
        self.state = next;
        self.updated_at = now;
        Ok(())
    }

    fn ensure_time(&self, now: UnixMillis) -> Result<(), AutomationSchedulerError> {
        if now < self.updated_at {
            return Err(AutomationSchedulerError::ClockRegression);
        }
        Ok(())
    }

    const fn invalid_transition(&self, to: AutomationOccurrenceState) -> AutomationSchedulerError {
        AutomationSchedulerError::InvalidOccurrenceTransition {
            from: self.state,
            to,
        }
    }

    fn reachable_shape(&self) -> bool {
        let exact_decision = self
            .snapshot
            .schedule
            .decision_for_nominal(&self.snapshot.timezone, self.nominal_local)
            .is_ok_and(|decision| decision.scheduled_for() == self.scheduled_for);
        let scheduled_shape = match self.state {
            AutomationOccurrenceState::SkippedInvalidLocalTime => self.scheduled_for.is_none(),
            _ => self.scheduled_for.is_some(),
        };
        let lifecycle_shape = match self.state {
            AutomationOccurrenceState::Pending
            | AutomationOccurrenceState::QueuedOverlap
            | AutomationOccurrenceState::SkippedMissed
            | AutomationOccurrenceState::SkippedOverlap
            | AutomationOccurrenceState::SkippedInvalidLocalTime => {
                self.claim.is_none() && self.run_id.is_none()
            }
            AutomationOccurrenceState::Claimed => {
                self.claim.is_some() && self.run_id.is_none() && self.claim_attempt_count > 0
            }
            AutomationOccurrenceState::RunLinked => {
                self.claim.is_some() && self.run_id.is_some() && self.claim_attempt_count > 0
            }
            AutomationOccurrenceState::Succeeded | AutomationOccurrenceState::Failed => {
                self.claim.is_none() && self.run_id.is_some() && self.claim_attempt_count > 0
            }
            AutomationOccurrenceState::InterruptedNeedsReview => {
                self.claim.is_none()
                    && ((self.run_id.is_some() && self.claim_attempt_count > 0)
                        || (self.run_id.is_none()
                            && self.claim_attempt_count
                                == MAX_AUTOMATION_OCCURRENCE_CLAIM_ATTEMPTS))
            }
            AutomationOccurrenceState::Cancelled => self.claim.is_none(),
        };
        let attempts = u64::from(self.claim_attempt_count);
        let revision_shape = match self.state {
            AutomationOccurrenceState::Pending => {
                self.revision >= attempts.saturating_mul(2) && self.revision.is_multiple_of(2)
            }
            AutomationOccurrenceState::QueuedOverlap
            | AutomationOccurrenceState::SkippedMissed
            | AutomationOccurrenceState::SkippedOverlap => {
                self.revision >= attempts.saturating_mul(2).saturating_add(1)
            }
            AutomationOccurrenceState::Claimed => {
                attempts > 0
                    && self.revision >= attempts.saturating_mul(2).saturating_sub(1)
                    && !self.revision.is_multiple_of(2)
            }
            AutomationOccurrenceState::RunLinked => {
                attempts > 0
                    && self.revision >= attempts.saturating_mul(2)
                    && self.revision.is_multiple_of(2)
            }
            AutomationOccurrenceState::Succeeded | AutomationOccurrenceState::Failed => {
                attempts > 0
                    && self.revision >= attempts.saturating_mul(2).saturating_add(1)
                    && !self.revision.is_multiple_of(2)
            }
            AutomationOccurrenceState::Cancelled if self.run_id.is_some() => {
                attempts > 0 && self.revision >= attempts.saturating_mul(2).saturating_add(1)
            }
            AutomationOccurrenceState::Cancelled if attempts > 0 => {
                self.revision >= attempts.saturating_mul(2)
            }
            AutomationOccurrenceState::Cancelled => self.revision >= 1,
            AutomationOccurrenceState::SkippedInvalidLocalTime => {
                attempts == 0 && self.revision == 0
            }
            AutomationOccurrenceState::InterruptedNeedsReview if self.run_id.is_some() => {
                attempts > 0
                    && self.revision >= attempts.saturating_mul(2).saturating_add(1)
                    && !self.revision.is_multiple_of(2)
            }
            AutomationOccurrenceState::InterruptedNeedsReview => {
                self.claim_attempt_count == MAX_AUTOMATION_OCCURRENCE_CLAIM_ATTEMPTS
                    && self.revision >= attempts.saturating_mul(2).saturating_add(1)
                    && !self.revision.is_multiple_of(2)
            }
        };
        let claim_shape = self.claim.as_ref().is_none_or(|claim| {
            claim.fence > 0
                && claim.expires_at > claim.claimed_at
                && claim.expires_at - claim.claimed_at <= MAX_AUTOMATION_SCHEDULER_LEASE_MS
                && claim.claimed_at >= self.created_at
                && claim.claimed_at <= self.updated_at
        });
        exact_decision && scheduled_shape && lifecycle_shape && revision_shape && claim_shape
    }
}

/// Exact owner and fencing generation required by scheduler store mutations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutomationSchedulerLeaseToken {
    /// Daemon process identity.
    pub owner_id: AutomationSchedulerOwnerId,
    /// Positive monotonically increasing fencing generation.
    pub fence: u64,
}

/// Durable singleton scheduler lease.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutomationSchedulerLease {
    /// Current daemon owner.
    pub owner_id: AutomationSchedulerOwnerId,
    /// Positive fencing generation, strictly increased on takeover.
    pub fence: u64,
    /// Time this ownership generation began.
    pub acquired_at: UnixMillis,
    /// Most recent same-owner renewal time.
    pub renewed_at: UnixMillis,
    /// Exclusive lease expiry.
    pub expires_at: UnixMillis,
}

impl AutomationSchedulerLease {
    /// Acquires the first scheduler lease generation.
    ///
    /// # Errors
    ///
    /// Returns [`AutomationSchedulerError`] for zero fence, zero/oversized lifetime, or overflow.
    pub fn acquire(
        owner_id: AutomationSchedulerOwnerId,
        fence: u64,
        now: UnixMillis,
        ttl_ms: u64,
    ) -> Result<Self, AutomationSchedulerError> {
        validate_lease_ttl(ttl_ms)?;
        if fence == 0 {
            return Err(AutomationSchedulerError::InvalidLease);
        }
        let expires_at = now
            .checked_add(ttl_ms)
            .ok_or(AutomationSchedulerError::InvalidLease)?;
        Ok(Self {
            owner_id,
            fence,
            acquired_at: now,
            renewed_at: now,
            expires_at,
        })
    }

    /// Rehydrates a persisted lease after validating its exact timestamps and fence.
    ///
    /// # Errors
    ///
    /// Returns [`AutomationSchedulerError::InvalidPersistedState`] for an impossible lease.
    pub fn restore(lease: Self) -> Result<Self, AutomationSchedulerError> {
        if lease.fence == 0
            || lease.renewed_at < lease.acquired_at
            || lease.expires_at <= lease.renewed_at
            || lease.expires_at - lease.renewed_at > MAX_AUTOMATION_SCHEDULER_LEASE_MS
        {
            return Err(AutomationSchedulerError::InvalidPersistedState);
        }
        Ok(lease)
    }

    /// Returns the exact token required by fenced store mutations.
    #[must_use]
    pub fn token(&self) -> AutomationSchedulerLeaseToken {
        AutomationSchedulerLeaseToken {
            owner_id: self.owner_id.clone(),
            fence: self.fence,
        }
    }

    /// Returns whether this generation owns the instant. Expiry is exclusive.
    #[must_use]
    pub const fn is_valid_at(&self, now: UnixMillis) -> bool {
        now >= self.acquired_at && now < self.expires_at
    }

    /// Renews an unexpired lease under the exact same token.
    ///
    /// # Errors
    ///
    /// Returns [`AutomationSchedulerError`] for stale ownership, expiry, clock regression, invalid
    /// lifetime, or overflow.
    pub fn renew(
        &mut self,
        token: &AutomationSchedulerLeaseToken,
        now: UnixMillis,
        ttl_ms: u64,
    ) -> Result<(), AutomationSchedulerError> {
        validate_lease_ttl(ttl_ms)?;
        if token.owner_id != self.owner_id || token.fence != self.fence {
            return Err(AutomationSchedulerError::StaleFence);
        }
        if now < self.renewed_at {
            return Err(AutomationSchedulerError::ClockRegression);
        }
        if now >= self.expires_at {
            return Err(AutomationSchedulerError::LeaseExpired);
        }
        self.expires_at = now
            .checked_add(ttl_ms)
            .ok_or(AutomationSchedulerError::InvalidLease)?;
        self.renewed_at = now;
        Ok(())
    }

    /// Transfers an expired lease to a new owner under a strictly larger fence.
    ///
    /// # Errors
    ///
    /// Returns [`AutomationSchedulerError`] unless the prior lease is expired and all new lease
    /// fields are valid.
    pub fn take_over(
        &mut self,
        owner_id: AutomationSchedulerOwnerId,
        fence: u64,
        now: UnixMillis,
        ttl_ms: u64,
    ) -> Result<(), AutomationSchedulerError> {
        if now < self.expires_at {
            return Err(AutomationSchedulerError::LeaseStillHeld);
        }
        if fence <= self.fence {
            return Err(AutomationSchedulerError::StaleFence);
        }
        *self = Self::acquire(owner_id, fence, now, ttl_ms)?;
        Ok(())
    }
}

/// Invalid scheduler aggregate construction or lifecycle transition.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AutomationSchedulerError {
    /// Typed schedule or timezone validation failed.
    #[error(transparent)]
    Schedule(#[from] AutomationScheduleError),
    /// Immutable title or prompt input was malformed or oversized.
    #[error("automation execution snapshot text is invalid")]
    InvalidExecutionText,
    /// A schedule decision did not match the requested occurrence constructor.
    #[error("automation schedule decision is incompatible with the occurrence")]
    InvalidDecision,
    /// A timestamp moved behind durable scheduler state.
    #[error("automation scheduler clock regressed")]
    ClockRegression,
    /// The occurrence lifecycle edge is not allowed.
    #[error("invalid automation occurrence transition from {from:?} to {to:?}")]
    InvalidOccurrenceTransition {
        /// Existing state.
        from: AutomationOccurrenceState,
        /// Requested state.
        to: AutomationOccurrenceState,
    },
    /// An optimistic revision could not advance.
    #[error("automation scheduler revision is exhausted")]
    RevisionExhausted,
    /// A lease or claim had zero, oversized, inverted, or overflowing fields.
    #[error("automation scheduler lease is invalid")]
    InvalidLease,
    /// The supplied owner/fence no longer owns scheduler mutation authority.
    #[error("automation scheduler fence is stale")]
    StaleFence,
    /// Same-owner renewal was attempted after exclusive expiry.
    #[error("automation scheduler lease expired")]
    LeaseExpired,
    /// Takeover was attempted while the prior lease remained live.
    #[error("automation scheduler lease is still held")]
    LeaseStillHeld,
    /// The bounded claim-attempt count is exhausted.
    #[error("automation occurrence claim attempts are exhausted")]
    ClaimAttemptsExhausted,
    /// Persisted fields do not form a reachable scheduler aggregate.
    #[error("persisted automation scheduler state is internally inconsistent")]
    InvalidPersistedState,
}

fn validate_execution_text(title: &str, prompt: &str) -> Result<(), AutomationSchedulerError> {
    let title_valid = !title.trim().is_empty()
        && title.len() <= MAX_AUTOMATION_TITLE_BYTES
        && !title.chars().any(char::is_control);
    let prompt_valid = !prompt.trim().is_empty()
        && prompt.len() <= MAX_AUTOMATION_PROMPT_BYTES
        && !prompt.chars().any(|character| {
            character == '\0'
                || (character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
        });
    if !title_valid || !prompt_valid {
        return Err(AutomationSchedulerError::InvalidExecutionText);
    }
    Ok(())
}

fn canonicalize_timezone(value: String) -> Result<String, AutomationScheduleError> {
    let canonical = parse_timezone(&value)?.name();
    if canonical == value {
        Ok(value)
    } else {
        Ok(canonical.to_owned())
    }
}

fn validate_next_decision(
    next: Option<AutomationScheduleDecision>,
    evaluated_through: UnixMillis,
) -> Result<(), AutomationSchedulerError> {
    if let Some(decision) = next {
        decision.nominal_local().to_naive()?;
        if decision
            .scheduled_for()
            .is_some_and(|scheduled_for| scheduled_for <= evaluated_through)
        {
            return Err(AutomationSchedulerError::InvalidDecision);
        }
    }
    Ok(())
}

fn advance_revision(revision: &mut u64) -> Result<(), AutomationSchedulerError> {
    *revision = revision
        .checked_add(1)
        .ok_or(AutomationSchedulerError::RevisionExhausted)?;
    Ok(())
}

fn validate_lease_ttl(ttl_ms: u64) -> Result<(), AutomationSchedulerError> {
    if ttl_ms == 0 || ttl_ms > MAX_AUTOMATION_SCHEDULER_LEASE_MS {
        return Err(AutomationSchedulerError::InvalidLease);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use chrono::DateTime;

    use super::*;

    fn timestamp(value: &str) -> UnixMillis {
        u64::try_from(
            DateTime::parse_from_rfc3339(value)
                .expect("RFC 3339 test timestamp")
                .timestamp_millis(),
        )
        .expect("positive test timestamp")
    }

    fn automation_id() -> AutomationId {
        AutomationId::new("automation-1").expect("automation id")
    }

    fn occurrence_id() -> AutomationOccurrenceId {
        AutomationOccurrenceId::new("automation-occurrence-1").expect("occurrence id")
    }

    fn owner(value: &str) -> AutomationSchedulerOwnerId {
        AutomationSchedulerOwnerId::new(value).expect("owner id")
    }

    fn run_id() -> RunId {
        RunId::new("run-1").expect("run id")
    }

    fn snapshot() -> AutomationExecutionSnapshot {
        AutomationExecutionSnapshot::new(
            7,
            ProjectId::new("project-1").expect("project id"),
            "Daily brief".into(),
            "Summarize the project status.".into(),
            "v1;daily;09:30".into(),
            "Europe/Paris".into(),
            MissedRunPolicy::RunOnce,
            OverlapPolicy::QueueOne,
        )
        .expect("execution snapshot")
    }

    fn due_decision() -> AutomationScheduleDecision {
        AutomationScheduleDecision::Due {
            nominal_local: AutomationLocalDateTime::new(2026, 7, 12, 9, 30).expect("local slot"),
            scheduled_for: timestamp("2026-07-12T07:30:00Z"),
        }
    }

    fn pending_occurrence() -> AutomationOccurrence {
        pending_occurrence_with(snapshot())
    }

    fn pending_occurrence_with(snapshot: AutomationExecutionSnapshot) -> AutomationOccurrence {
        AutomationOccurrence::pending(
            occurrence_id(),
            automation_id(),
            snapshot,
            due_decision(),
            1,
            timestamp("2026-07-12T07:30:00Z"),
        )
        .expect("pending occurrence")
    }

    fn gap_snapshot() -> AutomationExecutionSnapshot {
        AutomationExecutionSnapshot::new(
            8,
            ProjectId::new("project-1").expect("project id"),
            "Gap brief".into(),
            "Summarize after the DST transition.".into(),
            "v1;daily;02:30".into(),
            "Europe/Paris".into(),
            MissedRunPolicy::Skip,
            OverlapPolicy::Skip,
        )
        .expect("gap snapshot")
    }

    fn skip_snapshot() -> AutomationExecutionSnapshot {
        AutomationExecutionSnapshot::new(
            9,
            ProjectId::new("project-1").expect("project id"),
            "Skipped brief".into(),
            "Summarize only when policy permits.".into(),
            "v1;daily;09:30".into(),
            "Europe/Paris".into(),
            MissedRunPolicy::Skip,
            OverlapPolicy::Skip,
        )
        .expect("skip snapshot")
    }

    #[test]
    fn canonical_v1_round_trips_exactly_and_rejects_aliases() {
        let schedules = [
            (
                "v1;daily;00:00",
                AutomationSchedule::new(AutomationCadence::Daily, 0, 0).expect("daily"),
            ),
            (
                "v1;weekdays;23:59",
                AutomationSchedule::new(AutomationCadence::Weekdays, 23, 59).expect("weekdays"),
            ),
            (
                "v1;weekly;0;08:05",
                AutomationSchedule::new(AutomationCadence::Weekly { weekday: 0 }, 8, 5)
                    .expect("weekly"),
            ),
            (
                "v1;monthly;31;12:15",
                AutomationSchedule::new(AutomationCadence::Monthly { day_of_month: 31 }, 12, 15)
                    .expect("monthly"),
            ),
        ];
        for (encoded, schedule) in schedules {
            assert_eq!(AutomationSchedule::parse_canonical(encoded), Ok(schedule));
            assert_eq!(schedule.to_canonical_string(), encoded);
            assert_eq!(encoded.parse::<AutomationSchedule>(), Ok(schedule));
        }

        for invalid in [
            "",
            " v1;daily;09:00",
            "v1;daily;9:00",
            "v1;daily;09:0",
            "v1;daily;24:00",
            "v1;daily;09:60",
            "v1;weekly;00;09:00",
            "v1;weekly;7;09:00",
            "v1;monthly;0;09:00",
            "v1;monthly;32;09:00",
            "v1;daily;09:00;extra",
            "v2;daily;09:00",
        ] {
            assert!(
                AutomationSchedule::parse_canonical(invalid).is_err(),
                "{invalid}"
            );
        }
        assert_eq!(
            AutomationSchedule::parse_canonical(&"x".repeat(MAX_AUTOMATION_SCHEDULE_BYTES + 1)),
            Err(AutomationScheduleError::TooLong)
        );
    }

    #[test]
    fn normalization_accepts_only_current_applicable_json_and_limited_cron() {
        let cases = [
            (
                r#"{"frequency":"daily","localTime":"09:05","timeZoneIana":"UTC"}"#,
                "v1;daily;09:05",
            ),
            (
                r#"{"frequency":"weekdays","localTime":"08:30","timeZoneIana":"Europe/Paris","timeZoneWindows":"Romance Standard Time"}"#,
                "v1;weekdays;08:30",
            ),
            (
                r#"{"frequency":"weekly","localTime":"18:00","weekday":0,"timeZoneIana":"UTC"}"#,
                "v1;weekly;0;18:00",
            ),
            (
                r#"{"frequency":"monthly","localTime":"07:45","dayOfMonth":31,"timeZoneIana":"UTC"}"#,
                "v1;monthly;31;07:45",
            ),
            ("5 9 * * *", "v1;daily;09:05"),
            ("30 8 * * 1-5", "v1;weekdays;08:30"),
            ("0 18 * * 0", "v1;weekly;0;18:00"),
            ("45 7 31 * *", "v1;monthly;31;07:45"),
        ];
        for (input, expected) in cases {
            let timezone = if input.contains("Europe/Paris") {
                "Europe/Paris"
            } else {
                "UTC"
            };
            assert_eq!(
                AutomationSchedule::parse_for_normalization(input, timezone)
                    .expect("normalization")
                    .to_canonical_string(),
                expected
            );
        }

        for invalid in [
            r#"{"frequency":"daily","localTime":"09:00","weekday":1,"timeZoneIana":"UTC"}"#,
            r#"{"frequency":"weekdays","localTime":"09:00","dayOfMonth":1,"timeZoneIana":"UTC"}"#,
            r#"{"frequency":"weekly","localTime":"09:00","weekday":1,"dayOfMonth":1,"timeZoneIana":"UTC"}"#,
            r#"{"frequency":"monthly","localTime":"09:00","weekday":1,"dayOfMonth":1,"timeZoneIana":"UTC"}"#,
            r#"{"frequency":"weekly","localTime":"09:00","weekday":null,"timeZoneIana":"UTC"}"#,
            r#"{"frequency":"daily","localTime":"09:00","timeZoneIana":"UTC","timeZoneWindows":""}"#,
            r#"{"frequency":"daily","localTime":"09:00","timeZoneIana":"UTC","timeZoneWindows":"   "}"#,
            r#"{"frequency":"daily","localTime":"09:00","timeZoneIana":"UTC","unknown":1}"#,
            "*/5 9 * * *",
            "0 9 * 1 *",
            "0 9 1 * 1",
            "0 9 * * 7",
            "00 9 * * *",
            "0 9 * * *\n",
        ] {
            assert!(
                AutomationSchedule::parse_for_normalization(invalid, "UTC").is_err(),
                "{invalid}"
            );
        }
        assert!(
            AutomationSchedule::parse_for_normalization(
                r#"{"frequency":"daily","localTime":"09:00","timeZoneIana":"Europe/Paris"}"#,
                "UTC"
            )
            .is_err()
        );
    }

    #[test]
    fn fingerprints_bind_canonical_semantics_timezone_and_calculator_version() {
        let daily = AutomationSchedule::parse_canonical("v1;daily;09:00").expect("daily");
        let weekly = AutomationSchedule::parse_canonical("v1;weekly;1;09:00").expect("weekly");
        assert_eq!(daily.fingerprint("UTC"), daily.fingerprint("UTC"));
        assert_ne!(
            daily.fingerprint("UTC").expect("UTC fingerprint"),
            daily
                .fingerprint("Europe/Paris")
                .expect("Paris fingerprint")
        );
        assert_ne!(
            daily.fingerprint("UTC").expect("daily fingerprint"),
            weekly.fingerprint("UTC").expect("weekly fingerprint")
        );
        assert_eq!(AUTOMATION_SCHEDULE_CALCULATOR_VERSION, 1);
    }

    #[test]
    fn bounded_calculation_obeys_cadence_and_reports_truncation() {
        let daily = AutomationSchedule::parse_canonical("v1;daily;09:00").expect("daily");
        let after = timestamp("2026-01-01T00:00:00Z");
        let through = timestamp("2026-01-04T23:00:00Z");
        let page = daily
            .decisions_between("UTC", after, through, 2)
            .expect("daily page");
        assert_eq!(page.decisions.len(), 2);
        assert!(page.truncated);
        assert_eq!(
            page.decisions[0].scheduled_for(),
            Some(timestamp("2026-01-01T09:00:00Z"))
        );

        let weekdays = AutomationSchedule::parse_canonical("v1;weekdays;09:00").expect("weekdays");
        let weekend = weekdays
            .decisions_between(
                "UTC",
                timestamp("2026-07-11T00:00:00Z"),
                timestamp("2026-07-12T23:59:00Z"),
                10,
            )
            .expect("weekend");
        assert!(weekend.decisions.is_empty());

        let monthly = AutomationSchedule::parse_canonical("v1;monthly;31;12:00").expect("monthly");
        let months = monthly
            .decisions_between(
                "UTC",
                timestamp("2026-02-01T00:00:00Z"),
                timestamp("2026-04-01T00:00:00Z"),
                10,
            )
            .expect("monthly window");
        assert_eq!(months.decisions.len(), 1);
        assert_eq!(
            months.decisions[0].scheduled_for(),
            Some(timestamp("2026-03-31T12:00:00Z"))
        );

        assert_eq!(
            daily.decisions_between("UTC", through, after, 1),
            Err(AutomationScheduleError::InvalidWindow)
        );
        assert_eq!(
            daily.decisions_between("UTC", after, through, 0),
            Err(AutomationScheduleError::InvalidLimit)
        );
        let oversized = after + (MAX_AUTOMATION_SCHEDULE_WINDOW_DAYS + 1) * MILLIS_PER_DAY;
        assert_eq!(
            daily.decisions_between("UTC", after, oversized, 1),
            Err(AutomationScheduleError::InvalidWindow)
        );
    }

    #[test]
    fn dst_gap_is_explicit_and_fold_uses_the_earlier_utc_instant() {
        let schedule =
            AutomationSchedule::parse_canonical("v1;daily;02:30").expect("daily schedule");
        let gap = schedule
            .decisions_between(
                "Europe/Paris",
                timestamp("2026-03-28T23:00:00Z"),
                timestamp("2026-03-29T04:00:00Z"),
                10,
            )
            .expect("spring gap");
        assert_eq!(
            gap.decisions,
            vec![AutomationScheduleDecision::SkippedNonexistentLocalTime {
                nominal_local: AutomationLocalDateTime::new(2026, 3, 29, 2, 30).expect("gap slot")
            }]
        );

        let fold = schedule
            .decisions_between(
                "Europe/Paris",
                timestamp("2026-10-24T22:00:00Z"),
                timestamp("2026-10-25T04:00:00Z"),
                10,
            )
            .expect("fall fold");
        assert_eq!(fold.decisions.len(), 1);
        assert_eq!(
            fold.decisions[0].scheduled_for(),
            Some(timestamp("2026-10-25T00:30:00Z"))
        );
        assert_eq!(
            fold.decisions[0].nominal_local(),
            AutomationLocalDateTime::new(2026, 10, 25, 2, 30).expect("fold slot")
        );
    }

    #[test]
    fn next_decision_returns_the_gap_instead_of_silently_shifting_it() {
        let schedule =
            AutomationSchedule::parse_canonical("v1;daily;02:30").expect("daily schedule");
        assert_eq!(
            schedule
                .next_decision_after("Europe/Paris", timestamp("2026-03-28T23:00:00Z"))
                .expect("next decision"),
            AutomationScheduleDecision::SkippedNonexistentLocalTime {
                nominal_local: AutomationLocalDateTime::new(2026, 3, 29, 2, 30).expect("gap slot")
            }
        );
    }

    #[test]
    fn execution_snapshot_revalidates_every_redundant_binding() {
        let snapshot = snapshot();
        assert_eq!(
            AutomationExecutionSnapshot::restore(snapshot.clone()),
            Ok(snapshot.clone())
        );
        assert_eq!(snapshot.canonical_schedule, "v1;daily;09:30");
        assert_eq!(snapshot.timezone, "Europe/Paris");

        let mut corrupt = snapshot.clone();
        corrupt.schedule_fingerprint = AutomationScheduleFingerprint::new([9; 32]);
        assert_eq!(
            AutomationExecutionSnapshot::restore(corrupt),
            Err(AutomationSchedulerError::InvalidPersistedState)
        );
        let mut corrupt = snapshot.clone();
        corrupt.canonical_schedule = "0 9 * * *".into();
        assert!(AutomationExecutionSnapshot::restore(corrupt).is_err());
        let mut corrupt = snapshot;
        corrupt.calculator_version += 1;
        assert!(AutomationExecutionSnapshot::restore(corrupt).is_err());
        assert!(
            AutomationExecutionSnapshot::new(
                0,
                ProjectId::new("project-1").expect("project id"),
                String::new(),
                "prompt".into(),
                "v1;daily;09:00".into(),
                "UTC".into(),
                MissedRunPolicy::Skip,
                OverlapPolicy::Skip,
            )
            .is_err()
        );
    }

    #[test]
    fn cursor_requires_monotonic_watermarks_and_future_due_instants() {
        let next = AutomationScheduleDecision::Due {
            nominal_local: AutomationLocalDateTime::new(2026, 7, 13, 9, 30).expect("next slot"),
            scheduled_for: 200,
        };
        let mut cursor =
            AutomationScheduleCursor::new(automation_id(), &snapshot(), 100, Some(next), 100)
                .expect("cursor");
        assert_eq!(
            AutomationScheduleCursor::restore(cursor.clone()),
            Ok(cursor.clone())
        );
        cursor.advance(150, Some(next), 150).expect("advance");
        assert_eq!(cursor.revision, 1);
        assert_eq!(
            cursor.advance(149, Some(next), 151),
            Err(AutomationSchedulerError::ClockRegression)
        );
        let stale_next = AutomationScheduleDecision::Due {
            nominal_local: next.nominal_local(),
            scheduled_for: 150,
        };
        assert_eq!(
            cursor.advance(150, Some(stale_next), 151),
            Err(AutomationSchedulerError::InvalidDecision)
        );
    }

    #[test]
    fn occurrence_lifecycle_is_fenced_and_linked_runs_are_never_reclaimed() {
        let mut occurrence = pending_occurrence();
        let start = occurrence.updated_at;
        assert_eq!(occurrence.slot().definition_revision, 7);
        occurrence.queue_overlap(start + 1).expect("queue overlap");
        occurrence
            .promote_queued(start + 2)
            .expect("promote overlap");

        let lease =
            AutomationSchedulerLease::acquire(owner("daemon-a"), 1, start + 2, 20).expect("lease");
        let token = lease.token();
        occurrence
            .claim(&token, start + 3, start + 20)
            .expect("claim");
        assert_eq!(occurrence.claim_attempt_count, 1);
        let stale = AutomationSchedulerLeaseToken {
            owner_id: owner("daemon-b"),
            fence: 2,
        };
        assert_eq!(
            occurrence.link_run(&stale, run_id(), start + 4),
            Err(AutomationSchedulerError::StaleFence)
        );
        occurrence
            .link_run(&token, run_id(), start + 4)
            .expect("link run");
        assert_eq!(
            occurrence.release_expired_claim(start + 21),
            Err(AutomationSchedulerError::InvalidOccurrenceTransition {
                from: AutomationOccurrenceState::RunLinked,
                to: AutomationOccurrenceState::Pending,
            })
        );
        occurrence.succeed(&run_id(), start + 21).expect("succeed");
        assert_eq!(occurrence.state, AutomationOccurrenceState::Succeeded);
        assert!(occurrence.claim.is_none());
        assert_eq!(
            AutomationOccurrence::restore(occurrence.clone()),
            Ok(occurrence)
        );
    }

    #[test]
    fn expired_unlinked_claims_are_bounded_then_require_review() {
        let mut occurrence = pending_occurrence();
        let token = AutomationSchedulerLeaseToken {
            owner_id: owner("daemon-a"),
            fence: 1,
        };
        let mut now = occurrence.updated_at + 1;
        for _ in 0..MAX_AUTOMATION_OCCURRENCE_CLAIM_ATTEMPTS {
            occurrence
                .claim(&token, now, now + 1)
                .expect("bounded claim");
            now += 1;
            occurrence
                .release_expired_claim(now)
                .expect("expired claim release");
            now += 1;
        }
        assert_eq!(
            occurrence.claim(&token, now, now + 1),
            Err(AutomationSchedulerError::ClaimAttemptsExhausted)
        );
        occurrence
            .mark_claims_exhausted(now)
            .expect("review terminalization");
        assert_eq!(
            occurrence.state,
            AutomationOccurrenceState::InterruptedNeedsReview
        );
        assert!(AutomationOccurrence::restore(occurrence).is_ok());
    }

    #[test]
    fn claims_cannot_start_early_and_claimed_cancellation_restores() {
        let mut occurrence = pending_occurrence();
        let scheduled_for = occurrence.scheduled_for.expect("scheduled timestamp");
        let token = AutomationSchedulerLeaseToken {
            owner_id: owner("daemon-a"),
            fence: 1,
        };
        assert_eq!(
            occurrence.claim(&token, scheduled_for - 1, scheduled_for + 10),
            Err(AutomationSchedulerError::InvalidDecision)
        );
        occurrence
            .claim(&token, scheduled_for, scheduled_for + 10)
            .expect("on-time claim");
        occurrence
            .cancel(scheduled_for + 1)
            .expect("claimed cancellation");
        assert_eq!(occurrence.state, AutomationOccurrenceState::Cancelled);
        assert!(AutomationOccurrence::restore(occurrence).is_ok());
    }

    #[test]
    fn gap_occurrences_use_only_the_logical_local_slot_key() {
        let local = AutomationLocalDateTime::new(2026, 3, 29, 2, 30).expect("gap slot");
        let occurrence = AutomationOccurrence::skipped_invalid_local_time(
            occurrence_id(),
            automation_id(),
            gap_snapshot(),
            AutomationScheduleDecision::SkippedNonexistentLocalTime {
                nominal_local: local,
            },
            timestamp("2026-03-29T04:00:00Z"),
        )
        .expect("gap occurrence");
        assert_eq!(occurrence.scheduled_for, None);
        assert_eq!(occurrence.slot().nominal_local, local);
        assert_eq!(occurrence.slot().definition_revision, 8);
        assert_eq!(occurrence.revision, 0);
        assert!(AutomationOccurrence::restore(occurrence).is_ok());
    }

    #[test]
    fn occurrence_materialization_recomputes_exact_cadence_time_utc_and_gap_shape() {
        let forged_time = AutomationScheduleDecision::Due {
            nominal_local: AutomationLocalDateTime::new(2026, 7, 12, 10, 30).expect("forged time"),
            scheduled_for: timestamp("2026-07-12T08:30:00Z"),
        };
        assert!(
            AutomationOccurrence::pending(
                occurrence_id(),
                automation_id(),
                snapshot(),
                forged_time,
                1,
                timestamp("2026-07-12T09:00:00Z"),
            )
            .is_err()
        );

        let forged_utc = AutomationScheduleDecision::Due {
            nominal_local: due_decision().nominal_local(),
            scheduled_for: timestamp("2026-07-12T07:30:01Z"),
        };
        assert!(
            AutomationOccurrence::pending(
                occurrence_id(),
                automation_id(),
                snapshot(),
                forged_utc,
                1,
                timestamp("2026-07-12T09:00:00Z"),
            )
            .is_err()
        );

        let weekly = AutomationExecutionSnapshot::new(
            10,
            ProjectId::new("project-1").expect("project id"),
            "Weekly brief".into(),
            "Summarize weekly.".into(),
            "v1;weekly;1;09:30".into(),
            "Europe/Paris".into(),
            MissedRunPolicy::RunOnce,
            OverlapPolicy::Skip,
        )
        .expect("weekly snapshot");
        assert!(
            AutomationOccurrence::pending(
                occurrence_id(),
                automation_id(),
                weekly,
                due_decision(),
                1,
                timestamp("2026-07-12T09:00:00Z"),
            )
            .is_err()
        );

        let gap_local = AutomationLocalDateTime::new(2026, 3, 29, 2, 30).expect("DST gap slot");
        assert!(
            AutomationOccurrence::pending(
                occurrence_id(),
                automation_id(),
                gap_snapshot(),
                AutomationScheduleDecision::Due {
                    nominal_local: gap_local,
                    scheduled_for: timestamp("2026-03-29T01:30:00Z"),
                },
                1,
                timestamp("2026-03-29T04:00:00Z"),
            )
            .is_err()
        );
        assert!(
            AutomationOccurrence::skipped_invalid_local_time(
                occurrence_id(),
                automation_id(),
                snapshot(),
                AutomationScheduleDecision::SkippedNonexistentLocalTime {
                    nominal_local: due_decision().nominal_local(),
                },
                timestamp("2026-07-12T09:00:00Z"),
            )
            .is_err()
        );
    }

    #[test]
    fn skipped_and_corrupt_occurrence_shapes_fail_closed() {
        let mut missed = pending_occurrence_with(skip_snapshot());
        let start = missed.updated_at;
        missed.skip_missed(start + 1).expect("missed skip");
        assert_eq!(missed.state, AutomationOccurrenceState::SkippedMissed);
        let mut overlap = pending_occurrence();
        overlap.queue_overlap(start + 1).expect("queued overlap");
        overlap.skip_overlap(start + 2).expect("overlap skip");
        assert_eq!(overlap.state, AutomationOccurrenceState::SkippedOverlap);

        let mut wrong_policy = pending_occurrence();
        assert!(wrong_policy.skip_missed(start + 1).is_err());
        let mut wrong_policy = pending_occurrence_with(skip_snapshot());
        assert!(wrong_policy.queue_overlap(start + 1).is_err());

        let mut corrupt = pending_occurrence();
        corrupt.state = AutomationOccurrenceState::RunLinked;
        assert_eq!(
            AutomationOccurrence::restore(corrupt),
            Err(AutomationSchedulerError::InvalidPersistedState)
        );
        let mut corrupt = pending_occurrence();
        corrupt.scheduled_for = None;
        assert!(AutomationOccurrence::restore(corrupt).is_err());
        let mut corrupt = pending_occurrence();
        corrupt.occurrence_count = 0;
        assert!(AutomationOccurrence::restore(corrupt).is_err());
    }

    #[test]
    fn lease_renewal_and_takeover_enforce_exclusive_expiry_and_fences() {
        let mut lease =
            AutomationSchedulerLease::acquire(owner("daemon-a"), 1, 100, 20).expect("lease");
        assert!(lease.is_valid_at(119));
        assert!(!lease.is_valid_at(120));
        let token = lease.token();
        lease.renew(&token, 110, 20).expect("renew");
        assert_eq!(lease.expires_at, 130);
        assert_eq!(
            lease.renew(&token, 109, 20),
            Err(AutomationSchedulerError::ClockRegression)
        );
        let stale = AutomationSchedulerLeaseToken {
            owner_id: owner("daemon-b"),
            fence: 2,
        };
        assert_eq!(
            lease.renew(&stale, 111, 20),
            Err(AutomationSchedulerError::StaleFence)
        );
        assert_eq!(
            lease.take_over(owner("daemon-b"), 2, 129, 20),
            Err(AutomationSchedulerError::LeaseStillHeld)
        );
        lease
            .take_over(owner("daemon-b"), 2, 130, 20)
            .expect("takeover");
        assert_eq!(lease.fence, 2);
        assert_eq!(lease.owner_id, owner("daemon-b"));
        assert_eq!(AutomationSchedulerLease::restore(lease.clone()), Ok(lease));
    }

    #[test]
    fn invalid_lease_and_restore_shapes_are_rejected() {
        assert_eq!(
            AutomationSchedulerLease::acquire(owner("daemon-a"), 0, 100, 20),
            Err(AutomationSchedulerError::InvalidLease)
        );
        assert_eq!(
            AutomationSchedulerLease::acquire(owner("daemon-a"), 1, 100, 0),
            Err(AutomationSchedulerError::InvalidLease)
        );
        assert_eq!(
            AutomationSchedulerLease::acquire(
                owner("daemon-a"),
                1,
                100,
                MAX_AUTOMATION_SCHEDULER_LEASE_MS + 1,
            ),
            Err(AutomationSchedulerError::InvalidLease)
        );
        let mut corrupt =
            AutomationSchedulerLease::acquire(owner("daemon-a"), 1, 100, 20).expect("lease");
        corrupt.renewed_at = 121;
        assert_eq!(
            AutomationSchedulerLease::restore(corrupt),
            Err(AutomationSchedulerError::InvalidPersistedState)
        );
    }
}
