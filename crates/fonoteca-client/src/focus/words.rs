//! Filters in words: what the boxes of a filter show for its value, how
//! what is typed in them reads, and the sentence a screen reader says.

use super::*;
use crate::rules::iso_date;
use crate::rules::words::{operator_label, parse_date, parse_span, span_text};

/// What the one or two boxes of a filter show: what was typed there, else
/// its value as typed.
pub fn typed(kind: Kind, condition: &Condition) -> [String; 2] {
    if let Some(typed) = &condition.typed {
        return typed.clone();
    }
    let number = |at: i64| match kind {
        Kind::Number(measure) => measure.show(at),
        _ => iso_date(at),
    };
    match &condition.value {
        Some(Value::Text(text)) => [text.clone(), String::new()],
        Some(Value::Number(at)) => [number(*at), String::new()],
        Some(Value::Range([from, to])) => [number(*from), number(*to)],
        Some(Value::Within(span)) => [span_text(*span), String::new()],
        _ => [String::new(), String::new()],
    }
}

/// The value the boxes read as for a filter of `kind` with `operator`; none
/// while they don't read as one.
pub fn read(kind: Kind, operator: Operator, [first, second]: &[String; 2]) -> Option<Value> {
    let (first, second) = (first.trim(), second.trim());
    match (kind, operator) {
        (_, Operator::Never | Operator::IsEmpty | Operator::IsNotEmpty) => None,
        (Kind::Number(measure), Operator::Between) => {
            ordered([measure.parse(first)?, measure.parse(second)?])
        }
        (Kind::Number(measure), _) => Some(Value::Number(measure.parse(first)?)),
        (Kind::Date, Operator::InTheLast | Operator::NotInTheLast) => {
            Some(Value::Within(parse_span(first)?))
        }
        (Kind::Date, Operator::Between) => ordered([parse_date(first)?, parse_date(second)?]),
        (Kind::Date, _) => parse_date(first).map(|_| Value::Text(first.to_owned())),
        (_, _) => (!first.is_empty()).then(|| Value::Text(first.to_owned())),
    }
}

fn ordered([from, to]: [i64; 2]) -> Option<Value> {
    (from <= to).then_some(Value::Range([from, to]))
}

/// What to type in a box for a value, for the box to say while empty.
pub fn hint(kind: Kind, operator: Operator, second: bool) -> &'static str {
    match (kind, operator, second) {
        (Kind::Number(measure), Operator::Between, _) => match measure.unit() {
            "min:sec" => "3:00",
            _ if second => "to",
            _ => "from",
        },
        (Kind::Number(measure), _, _) if measure.unit() == "min:sec" => "3:30",
        (Kind::Number(_), _, _) => "number",
        (Kind::Date, Operator::InTheLast | Operator::NotInTheLast, _) => "90 days",
        (Kind::Date, Operator::Between, true) => "to, 2026-12-31",
        (Kind::Date, Operator::Between, false) => "from, 2026-01-01",
        (Kind::Date, _, _) => "2026-01-31",
        _ => "text",
    }
}

/// Whether a filter with this operator has a value to type or pick.
pub fn takes_value(operator: Operator) -> bool {
    !matches!(
        operator,
        Operator::Never | Operator::IsEmpty | Operator::IsNotEmpty
    )
}

/// Whether the operator takes two ends.
pub fn is_range(operator: Operator) -> bool {
    operator == Operator::Between
}

/// Whether the operator picks from the values a field lists.
pub fn picks_values(operator: Operator) -> bool {
    matches!(
        operator,
        Operator::AnyOf | Operator::AllOf | Operator::NoneOf
    )
}

/// A picked value as its chip says it: bare where the filter wants any of
/// the picks, as the panel says it where it wants all or none of them, so
/// an exclusion never reads as an inclusion.
pub fn pick_chip(operator: Operator, picked: &str) -> String {
    match operator {
        Operator::AllOf | Operator::NoneOf => format!("{} {picked}", operator_label(operator)),
        _ => picked.to_owned(),
    }
}

/// The filter said as a screen reader should: "Year, between 1970 and 1979".
pub fn describe(title: &str, kind: Kind, condition: &Condition) -> String {
    let said = operator_label(condition.operator);
    let [first, second] = typed(kind, condition);
    let unit = match kind {
        Kind::Number(measure) if !measure.unit().is_empty() && measure.unit() != "min:sec" => {
            format!(" {}", measure.unit())
        }
        _ => String::new(),
    };
    let what = match (kind, &condition.value, condition.operator) {
        (Kind::Flag, Some(Value::Flag(yes)), _) => if *yes { "yes" } else { "no" }.to_owned(),
        (Kind::Flag, _, _) => "either".to_owned(),
        (_, _, Operator::Between) if !second.is_empty() => {
            format!("between {first} and {second}{unit}")
        }
        (_, _, operator) if !takes_value(operator) => said.to_owned(),
        (_, _, operator) if picks_values(operator) => match condition.picked() {
            [] => "no picks".to_owned(),
            picked => format!("{said} {}", picked.join(", ")),
        },
        _ if first.is_empty() => format!("{said}, no value"),
        _ => format!("{said} {first}{unit}"),
    };
    format!("{title}, {what}")
}
