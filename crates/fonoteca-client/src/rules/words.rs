//! Conditions in words: what each operator is called, and how the numbers,
//! ranges, spans and dates of a value are typed and shown.

use super::*;

pub fn operator_label(operator: Operator) -> &'static str {
    match operator {
        Operator::Is => "is",
        Operator::IsNot => "is not",
        Operator::Contains => "contains",
        Operator::NotContains => "does not contain",
        Operator::StartsWith => "starts with",
        Operator::GreaterThan => "more than",
        Operator::LessThan => "less than",
        Operator::Between => "between",
        Operator::InTheLast => "in the last",
        Operator::NotInTheLast => "not in the last",
        Operator::Before => "before",
        Operator::After => "after",
        Operator::Never => "never",
        Operator::AnyOf => "any of",
        Operator::AllOf => "all of",
        Operator::NoneOf => "none of",
        Operator::IsEmpty => "is empty",
        Operator::IsNotEmpty => "is not empty",
    }
}

/// What a number counts, which is how it is typed and shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Measure {
    Count,
    /// How often something was played, of which the steps of the most
    /// played are offered.
    Plays,
    /// Seconds, as minutes and seconds.
    Duration,
    KilobitsPerSecond,
    /// Hertz, as kilohertz.
    Kilohertz,
    /// Bytes, as megabytes.
    Megabytes,
}

impl Measure {
    /// The unit the numbers are in, for a label; none for a plain count.
    pub fn unit(self) -> &'static str {
        match self {
            Measure::Count | Measure::Plays => "",
            Measure::Duration => "min:sec",
            Measure::KilobitsPerSecond => "kbit/s",
            Measure::Kilohertz => "kHz",
            Measure::Megabytes => "MB",
        }
    }

    /// How many of the stored unit make one of the shown, for those shown
    /// as a decimal.
    fn scale(self) -> Option<f64> {
        match self {
            Measure::Kilohertz => Some(1_000.0),
            Measure::Megabytes => Some(1_000_000.0),
            _ => None,
        }
    }

    pub fn show(self, number: i64) -> String {
        match (self, self.scale()) {
            (Measure::Duration, _) => format!("{}:{:02}", number / 60, number % 60),
            (_, Some(scale)) => {
                let shown = format!("{:.2}", number as f64 / scale);
                shown.trim_end_matches('0').trim_end_matches('.').to_owned()
            }
            _ => number.to_string(),
        }
    }

    pub fn parse(self, text: &str) -> Option<i64> {
        let text = text.trim();
        match (self, self.scale(), text.split_once(':')) {
            (Measure::Duration, _, Some((minutes, seconds))) => Some(
                minutes.trim().parse::<i64>().ok()? * 60 + seconds.trim().parse::<i64>().ok()?,
            ),
            (_, Some(scale), _) => {
                let shown: f64 = text.parse().ok()?;
                (shown >= 0.0).then(|| (shown * scale).round() as i64)
            }
            _ => text.parse().ok(),
        }
    }
}

/// "from - to", both ends read as `measure` numbers.
pub fn parse_range(measure: Measure, text: &str) -> Option<[i64; 2]> {
    let (from, to) = text.split_once(['-', '–'])?;
    Some([measure.parse(from)?, measure.parse(to)?])
}

/// "90 days", "3 months"; a number alone is days.
pub fn parse_span(text: &str) -> Option<Span> {
    let mut words = text.split_whitespace();
    let amount = words.next()?.parse().ok()?;
    let unit = match words.next().map(|unit| unit.trim_end_matches('s')) {
        None | Some("day") => Unit::Days,
        Some("week") => Unit::Weeks,
        Some("month") => Unit::Months,
        Some("year") => Unit::Years,
        Some(_) => return None,
    };
    words.next().is_none().then_some(Span { amount, unit })
}

pub fn span_text(span: Span) -> String {
    let unit = match span.unit {
        Unit::Days => "day",
        Unit::Weeks => "week",
        Unit::Months => "month",
        Unit::Years => "year",
    };
    let plural = if span.amount == 1 { "" } else { "s" };
    format!("{} {unit}{plural}", span.amount)
}

pub fn parse_flag(text: &str) -> Option<bool> {
    match text.to_lowercase().as_str() {
        "yes" | "true" => Some(true),
        "no" | "false" => Some(false),
        _ => None,
    }
}

/// The day a date typed as "2026-01-31" is.
pub fn parse_date(text: &str) -> Option<i64> {
    let text = text.trim();
    (text.len() == 10).then(|| day_number(text)).flatten()
}
