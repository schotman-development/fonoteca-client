//! Rules in words: what the editor calls each field and operator, how a
//! value is typed and shown, and the line that sums a playlist's rules up.

use super::*;

pub fn field_label(field: Field) -> &'static str {
    match field {
        Field::Title => "Title",
        Field::Artist => "Artist",
        Field::AlbumArtist => "Album artist",
        Field::Album => "Album",
        Field::Genre => "Genre",
        Field::Composer => "Composer",
        Field::Mood => "Mood",
        Field::Tag => "Tag",
        Field::Format => "Format",
        Field::Year => "Year",
        Field::PlayCount => "Play count",
        Field::Rating => "Rating",
        Field::Duration => "Duration",
        Field::Bpm => "BPM",
        Field::BitRate => "Bit rate (kbit/s)",
        Field::BitDepth => "Bit depth",
        Field::SamplingRate => "Sample rate (Hz)",
        Field::Added => "Added",
        Field::LastPlayed => "Last played",
        Field::Favourite => "Favourite",
    }
}

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
    }
}

/// The orders the editor offers, by the name each has there: every key in
/// both directions, so a saved order always shows as itself.
pub const SORTS: [(&str, Sort); 19] = [
    ("Random", sort(SortKey::Random, false)),
    ("Most played", sort(SortKey::PlayCount, true)),
    ("Least played", sort(SortKey::PlayCount, false)),
    ("Recently added", sort(SortKey::Added, true)),
    ("Oldest added", sort(SortKey::Added, false)),
    ("Recently played", sort(SortKey::LastPlayed, true)),
    ("Longest ago played", sort(SortKey::LastPlayed, false)),
    ("Title A-Z", sort(SortKey::Title, false)),
    ("Title Z-A", sort(SortKey::Title, true)),
    ("Artist A-Z", sort(SortKey::Artist, false)),
    ("Artist Z-A", sort(SortKey::Artist, true)),
    ("Album A-Z", sort(SortKey::Album, false)),
    ("Album Z-A", sort(SortKey::Album, true)),
    ("Newest year first", sort(SortKey::Year, true)),
    ("Oldest year first", sort(SortKey::Year, false)),
    ("Highest rated", sort(SortKey::Rating, true)),
    ("Lowest rated", sort(SortKey::Rating, false)),
    ("Longest first", sort(SortKey::Duration, true)),
    ("Shortest first", sort(SortKey::Duration, false)),
];

const fn sort(by: SortKey, descending: bool) -> Sort {
    Sort { by, descending }
}

/// The place of `wanted` in the orders on offer. Random has no direction,
/// so a saved random order in either direction is the first.
pub fn sort_index(wanted: Sort) -> usize {
    let wanted = match wanted.by {
        SortKey::Random => sort(SortKey::Random, false),
        _ => wanted,
    };
    SORTS
        .iter()
        .position(|(_, sort)| *sort == wanted)
        .unwrap_or(0)
}

/// What the rule's value reads as when typed.
pub fn value_text(rule: &Rule) -> String {
    let number = |number: i64| number_text(rule.field, number);
    match &rule.value {
        None => String::new(),
        Some(Value::Flag(yes)) => if *yes { "yes" } else { "no" }.to_owned(),
        Some(Value::Number(at)) => number(*at),
        Some(Value::Text(text)) => text.clone(),
        Some(Value::Range([from, to])) => format!("{} - {}", number(*from), number(*to)),
        Some(Value::Within(span)) => {
            let unit = match span.unit {
                Unit::Days => "day",
                Unit::Weeks => "week",
                Unit::Months => "month",
                Unit::Years => "year",
            };
            let plural = if span.amount == 1 { "" } else { "s" };
            format!("{} {unit}{plural}", span.amount)
        }
    }
}

/// A duration as minutes and seconds, any other number as it is.
fn number_text(field: Field, number: i64) -> String {
    match field {
        Field::Duration => format!("{}:{:02}", number / 60, number % 60),
        _ => number.to_string(),
    }
}

/// What to type in the box for a value, for the box to say while empty.
pub fn value_hint(field: Field, operator: Operator) -> &'static str {
    match (operator, field.kind()) {
        (Operator::Never, _) => "",
        (_, Kind::Flag) => "yes or no",
        (Operator::Between, _) if field == Field::Duration => "3:00 - 5:00",
        (Operator::Between, _) => "from - to",
        (Operator::InTheLast | Operator::NotInTheLast, _) => "90 days",
        (Operator::Before | Operator::After, _) => "2026-01-31",
        (_, Kind::Number) if field == Field::Duration => "3:30 or seconds",
        (_, Kind::Number) => "number",
        _ => "text",
    }
}

/// The rule of `field` and `operator` with the value typed as `text`, if
/// that reads as one the server takes.
pub fn parse_rule(field: Field, operator: Operator, text: &str) -> Option<Rule> {
    let text = text.trim();
    if operator != Operator::Never && text.is_empty() {
        return None;
    }
    let value = match (operator, field.kind()) {
        (Operator::Never, _) => None,
        (_, Kind::Flag) => Some(Value::Flag(parse_flag(text)?)),
        (Operator::Between, _) => Some(Value::Range(parse_range(field, text)?)),
        (Operator::InTheLast | Operator::NotInTheLast, _) => Some(Value::Within(parse_span(text)?)),
        (_, Kind::Number) => Some(Value::Number(parse_number(field, text)?)),
        _ => Some(Value::Text(text.to_owned())),
    };
    let rule = Rule {
        field,
        operator,
        value,
    };
    rule.check().ok()?;
    Some(rule)
}

fn parse_flag(text: &str) -> Option<bool> {
    match text.to_lowercase().as_str() {
        "yes" | "true" => Some(true),
        "no" | "false" => Some(false),
        _ => None,
    }
}

fn parse_number(field: Field, text: &str) -> Option<i64> {
    let text = text.trim();
    match (field, text.split_once(':')) {
        (Field::Duration, Some((minutes, seconds))) => {
            Some(minutes.trim().parse::<i64>().ok()? * 60 + seconds.trim().parse::<i64>().ok()?)
        }
        _ => text.parse().ok(),
    }
}

fn parse_range(field: Field, text: &str) -> Option<[i64; 2]> {
    let (from, to) = text.split_once(['-', '–'])?;
    Some([parse_number(field, from)?, parse_number(field, to)?])
}

/// "90 days", "3 months"; a number alone is days.
fn parse_span(text: &str) -> Option<Span> {
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

/// The rules in a line: "Blues · added in the last 90 days · never played".
pub fn summary(rules: &SmartRules) -> String {
    let phrases: Vec<String> = rules.rules.iter().map(phrase).collect();
    let line = phrases.join(" · ");
    match rules.match_all || phrases.len() < 2 {
        true => line,
        false => format!("Any of: {line}"),
    }
}

fn phrase(rule: &Rule) -> String {
    let value = value_text(rule);
    match (rule.field, rule.operator, &rule.value) {
        (Field::Genre, Operator::Is, _) => value,
        (Field::Added, Operator::InTheLast, _) => format!("added in the last {value}"),
        (Field::LastPlayed, Operator::InTheLast, _) => format!("played in the last {value}"),
        (Field::LastPlayed, Operator::Never, _)
        | (Field::PlayCount, Operator::Is, Some(Value::Number(0))) => "never played".to_owned(),
        (Field::Favourite, Operator::Is, Some(Value::Flag(true))) => "favourite".to_owned(),
        (Field::Favourite, Operator::Is, Some(Value::Flag(false))) => "not a favourite".to_owned(),
        (field, operator, _) => format!(
            "{} {} {value}",
            field_label(field),
            operator_label(operator)
        )
        .trim_end()
        .to_owned(),
    }
}
