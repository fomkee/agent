use std::{ops::Div, time::Duration};

use chrono::{DateTime, SecondsFormat, Utc};

use crate::model::{CollectionInterval, FactValue, Unit};

const KIBIBYTE: f64 = 1_024.0;
const MEBIBYTE: f64 = 1_048_576.0;
const GIBIBYTE: f64 = 1_073_741_824.0;
const TEBIBYTE: f64 = 1_099_511_627_776.0;

pub(super) fn measurement(value: f64, unit: Unit) -> String {
    match unit {
        Unit::Bytes => bytes(value),
        Unit::Seconds => seconds(value),
        Unit::Ratio => format!("{}%", decimal(value.mul_add(100.0, 0.0), 1)),
        Unit::Count => count(value),
        Unit::None => decimal(value, 2),
    }
}

pub(super) fn fact(value: &FactValue) -> String {
    match value {
        FactValue::Boolean(value) => value.to_string(),
        FactValue::Text(value) => value.clone(),
        FactValue::Timestamp(value) => timestamp(*value),
    }
}

pub(super) fn timestamp(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Secs, true)
}

pub(super) fn collection_duration(milliseconds: u64) -> String {
    if milliseconds < 1_000 {
        format!("{milliseconds} ms")
    } else {
        let seconds = Duration::from_millis(milliseconds).as_secs_f64();
        format!("{} s", decimal(seconds, 2))
    }
}

pub(super) fn freshness(
    collected_at: DateTime<Utc>,
    interval: CollectionInterval,
    now: DateTime<Utc>,
) -> (String, bool) {
    let cadence = measurement(interval.duration().as_secs_f64(), Unit::Seconds);
    let Ok(age) = now.signed_duration_since(collected_at).to_std() else {
        return (format!("clock skew · every {cadence}"), true);
    };
    let overdue = interval
        .duration()
        .checked_mul(2)
        .is_some_and(|limit| age > limit);
    let age = measurement(age.as_secs_f64(), Unit::Seconds);
    let suffix = if overdue { " · overdue" } else { "" };
    (format!("{age} ago · every {cadence}{suffix}"), overdue)
}

pub(super) fn age(collected_at: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let Ok(age) = now.signed_duration_since(collected_at).to_std() else {
        return "clock skew".to_owned();
    };
    format!("{} ago", measurement(age.as_secs_f64(), Unit::Seconds))
}

pub(super) fn truncate(value: &str, width: usize) -> String {
    let length = value.chars().count();
    if length <= width {
        return value.to_owned();
    }
    let retained = width.saturating_sub(1);
    let mut truncated = value.chars().take(retained).collect::<String>();
    truncated.push('…');
    truncated
}

fn bytes(value: f64) -> String {
    let absolute = value.abs();
    if absolute >= TEBIBYTE {
        format!("{} TiB", decimal(value.div(TEBIBYTE), 2))
    } else if absolute >= GIBIBYTE {
        format!("{} GiB", decimal(value.div(GIBIBYTE), 2))
    } else if absolute >= MEBIBYTE {
        format!("{} MiB", decimal(value.div(MEBIBYTE), 2))
    } else if absolute >= KIBIBYTE {
        format!("{} KiB", decimal(value.div(KIBIBYTE), 2))
    } else {
        format!("{} B", decimal(value, 0))
    }
}

fn seconds(value: f64) -> String {
    let rounded = value.round();
    let Ok(duration) = Duration::try_from_secs_f64(rounded) else {
        return format!("{} s", decimal(value, 2));
    };
    let total = duration.as_secs();
    let days = total.div(86_400);
    let after_days = total % 86_400;
    let hours = after_days.div(3_600);
    let after_hours = after_days % 3_600;
    let minutes = after_hours.div(60);
    let remaining_seconds = after_hours % 60;
    let mut parts = Vec::new();
    if days > 0 {
        parts.push(format!("{days}d"));
    }
    if hours > 0 {
        parts.push(format!("{hours}h"));
    }
    if minutes > 0 {
        parts.push(format!("{minutes}m"));
    }
    if remaining_seconds > 0 || parts.is_empty() {
        parts.push(format!("{remaining_seconds}s"));
    }
    parts.join(" ")
}

fn count(value: f64) -> String {
    if value.fract() == 0.0 {
        grouped(&format!("{value:.0}"))
    } else {
        decimal(value, 2)
    }
}

fn grouped(value: &str) -> String {
    let (sign, digits) = value
        .strip_prefix('-')
        .map_or(("", value), |digits| ("-", digits));
    let mut reversed = String::with_capacity(value.len());
    for (index, character) in digits.chars().rev().enumerate() {
        if index != 0 && index % 3 == 0 {
            reversed.push(',');
        }
        reversed.push(character);
    }
    let grouped = reversed.chars().rev().collect::<String>();
    format!("{sign}{grouped}")
}

fn decimal(value: f64, precision: usize) -> String {
    let formatted = format!("{value:.precision$}");
    if formatted.contains('.') {
        formatted
            .trim_end_matches('0')
            .trim_end_matches('.')
            .to_owned()
    } else {
        formatted
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    #[test]
    fn test_formats_binary_byte_units_without_raw_value() {
        // Arrange
        let bytes = 1_610_612_736.0;

        // Act
        let formatted = measurement(bytes, Unit::Bytes);

        // Assert
        assert_eq!(formatted, "1.5 GiB");
    }

    #[test]
    fn test_formats_ratio_as_percentage() {
        // Arrange
        let ratio = 0.427;

        // Act
        let formatted = measurement(ratio, Unit::Ratio);

        // Assert
        assert_eq!(formatted, "42.7%");
    }

    #[test]
    fn test_formats_seconds_as_compact_duration() {
        // Arrange
        let duration = 93_784.0;

        // Act
        let formatted = measurement(duration, Unit::Seconds);

        // Assert
        assert_eq!(formatted, "1d 2h 3m 4s");
    }

    #[test]
    fn test_formats_integral_count_with_grouping() {
        // Arrange
        let count = 12_438.0;

        // Act
        let formatted = measurement(count, Unit::Count);

        // Assert
        assert_eq!(formatted, "12,438");
    }

    #[test]
    fn test_formats_timestamp_to_second_precision() {
        // Arrange
        let timestamp = Utc
            .with_ymd_and_hms(2026, 10, 20, 16, 20, 33)
            .single()
            .expect("valid fixture");

        // Act
        let formatted = fact(&FactValue::Timestamp(timestamp));

        // Assert
        assert_eq!(formatted, "2026-10-20T16:20:33Z");
    }

    #[test]
    fn test_marks_observation_overdue_after_two_cadences() {
        // Arrange
        let collected_at = Utc
            .with_ymd_and_hms(2026, 10, 21, 10, 0, 0)
            .single()
            .expect("valid fixture");
        let now = Utc
            .with_ymd_and_hms(2026, 10, 21, 10, 2, 1)
            .single()
            .expect("valid fixture");
        let interval = CollectionInterval::from_secs(60).expect("valid fixture");

        // Act
        let (text, overdue) = freshness(collected_at, interval, now);

        // Assert
        assert!(overdue);
        assert_eq!(text, "2m 1s ago · every 1m · overdue");
    }
}
