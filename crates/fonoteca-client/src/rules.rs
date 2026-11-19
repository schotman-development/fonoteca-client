//! What the Focus filters and the smart playlists' rules share: the
//! operators and values a condition is made of, the groups that combine
//! conditions (all of them or any, nested), and the test of one value against
//! one condition. A field that has no value fails every condition on it but
//! the negative ones.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::browse::matching::fold_words as fold;

pub mod words;

/// The most groups inside one another, the first group counting as one.
pub const MOST_DEPTH: usize = 4;
const SECONDS_PER_DAY: i64 = 24 * 60 * 60;

/// The days from 1970-01-01 to the date an ISO 8601 date or time starts with.
pub fn day_number(date: &str) -> Option<i64> {
    let mut parts = date.get(..10)?.split('-');
    let mut next = || parts.next()?.parse::<i64>().ok();
    let (year, month, day) = (next()?, next()?, next()?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    // Counts the days of whole years from March, when the leap day ends the year.
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let day_of_year = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    Some(era * 146_097 + day_of_era - 719_468)
}

/// The day number of the day `seconds` after 1970-01-01 falls on.
pub fn today(seconds: u64) -> i64 {
    seconds as i64 / SECONDS_PER_DAY
}

/// The year, month and day of a day number, the days from 1970-01-01.
pub fn civil_date(day_number: i64) -> (i64, i64, i64) {
    let days = day_number + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// A day number as "2026-03-31".
pub fn iso_date(day_number: i64) -> String {
    let (year, month, day) = civil_date(day_number);
    format!("{year:04}-{month:02}-{day:02}")
}

fn days_in_month(year: i64, month: i64) -> i64 {
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    match month {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Operator {
    Is,
    IsNot,
    Contains,
    NotContains,
    StartsWith,
    GreaterThan,
    LessThan,
    Between,
    InTheLast,
    NotInTheLast,
    Before,
    After,
    Never,
    /// A filter's: at least one of the values listed.
    AnyOf,
    /// A filter's: every one of the values listed.
    AllOf,
    /// A filter's: none of the values listed.
    NoneOf,
    /// A filter's: no value at all, or any.
    IsEmpty,
    IsNotEmpty,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Unit {
    Days,
    Weeks,
    Months,
    Years,
}

/// A stretch of time back from today.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    pub amount: u32,
    pub unit: Unit,
}

/// What a condition compares with; which shape it has depends on the operator.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Value {
    Flag(bool),
    Number(i64),
    Text(String),
    /// The two ends, both included: numbers, or for a date the day numbers.
    Range([i64; 2]),
    Within(Span),
    List(Vec<String>),
}

/// The first day of the stretch that ends today, a month or a year back
/// ending on the same day of the month, or the month's last where it has no
/// such day.
pub fn first_day_of(span: Span, today: i64) -> i64 {
    let amount = i64::from(span.amount);
    let months = match span.unit {
        Unit::Days => return today - amount,
        Unit::Weeks => return today - 7 * amount,
        Unit::Months => amount,
        Unit::Years => 12 * amount,
    };
    let (year, month, day) = civil_date(today);
    let back = year * 12 + (month - 1) - months;
    let (year, month) = (back.div_euclid(12), back.rem_euclid(12) + 1);
    let day = day.min(days_in_month(year, month));
    day_number(&format!("{year:04}-{month:02}-{day:02}")).unwrap_or(today)
}

/// What an item has in a field, as a test needs it.
#[derive(Debug, Clone, PartialEq)]
pub enum FieldValue {
    /// Each value the item has, such as its genres; none where it has none.
    Texts(Vec<String>),
    Number(Option<i64>),
    /// A date as `day_number` counts.
    Day(Option<i64>),
    Flag(bool),
}

/// Whether what an item has in a field passes a condition on it.
pub fn passes(have: &FieldValue, operator: Operator, value: &Option<Value>, today: i64) -> bool {
    match have {
        FieldValue::Texts(values) => text_passes(values, operator, value),
        FieldValue::Number(have) => number_passes(*have, operator, value),
        FieldValue::Day(day) => date_passes(*day, operator, value, today),
        FieldValue::Flag(have) => flag_passes(*have, operator, value),
    }
}

fn text_passes(values: &[String], operator: Operator, value: &Option<Value>) -> bool {
    // A blank value is none, as the server counts it.
    let values: Vec<&String> = values
        .iter()
        .filter(|value| !value.trim().is_empty())
        .collect();
    let listed = || match value {
        Some(Value::List(listed)) => Some(listed),
        _ => None,
    };
    let has = |wanted: &String| values.iter().any(|value| value.trim() == wanted.trim());
    match operator {
        Operator::IsEmpty => values.is_empty(),
        Operator::IsNotEmpty => !values.is_empty(),
        Operator::AnyOf => listed().is_some_and(|listed| listed.iter().any(has)),
        Operator::AllOf => listed().is_some_and(|listed| listed.iter().all(has)),
        Operator::NoneOf => listed().is_some_and(|listed| !listed.iter().any(has)),
        _ => {
            let Some(Value::Text(wanted)) = value else {
                return false;
            };
            let wanted = fold(wanted);
            let any = |test: &dyn Fn(&str) -> bool| values.iter().any(|value| test(&fold(value)));
            match operator {
                Operator::Is => any(&|value| value == wanted),
                Operator::IsNot => !any(&|value| value == wanted),
                Operator::Contains => any(&|value| value.contains(&wanted)),
                Operator::NotContains => !any(&|value| value.contains(&wanted)),
                Operator::StartsWith => any(&|value| value.starts_with(&wanted)),
                _ => false,
            }
        }
    }
}

fn number_passes(have: Option<i64>, operator: Operator, value: &Option<Value>) -> bool {
    match (operator, value) {
        (Operator::IsNot, Some(Value::Number(not))) => have != Some(*not),
        (operator, value) => {
            let Some(have) = have else { return false };
            match (operator, value) {
                (Operator::Is, Some(Value::Number(is))) => have == *is,
                (Operator::GreaterThan, Some(Value::Number(over))) => have > *over,
                (Operator::LessThan, Some(Value::Number(under))) => have < *under,
                (Operator::Between, Some(Value::Range([from, to]))) => {
                    (*from..=*to).contains(&have)
                }
                _ => false,
            }
        }
    }
}

fn date_passes(day: Option<i64>, operator: Operator, value: &Option<Value>, today: i64) -> bool {
    match (operator, value) {
        (Operator::Never, _) => day.is_none(),
        (Operator::NotInTheLast, Some(Value::Within(span))) => {
            day.is_none_or(|day| day < first_day_of(*span, today))
        }
        (Operator::InTheLast, Some(Value::Within(span))) => {
            day.is_some_and(|day| day >= first_day_of(*span, today))
        }
        (Operator::Before, Some(Value::Text(date))) => day
            .zip(day_number(date))
            .is_some_and(|(day, limit)| day < limit),
        (Operator::After, Some(Value::Text(date))) => day
            .zip(day_number(date))
            .is_some_and(|(day, limit)| day > limit),
        (Operator::Between, Some(Value::Range([from, to]))) => {
            day.is_some_and(|day| (*from..=*to).contains(&day))
        }
        _ => false,
    }
}

fn flag_passes(have: bool, operator: Operator, value: &Option<Value>) -> bool {
    match (operator, value) {
        (Operator::Is, Some(Value::Flag(wanted))) => have == *wanted,
        _ => false,
    }
}

/// The wire says "all" or "any" where the model has a yes or no.
mod match_all {
    use serde::{Deserialize, Deserializer, Serializer, de::Error};

    pub fn serialize<S: Serializer>(all: &bool, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(if *all { "all" } else { "any" })
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<bool, D::Error> {
        match String::deserialize(deserializer)?.as_str() {
            "all" => Ok(true),
            "any" => Ok(false),
            other => Err(D::Error::custom(format!("unknown match \"{other}\""))),
        }
    }
}

/// Conditions combined, every one of them or any one. `L` is what a
/// condition is: a smart playlist's rule, or a filter of the Focus.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Group<L> {
    #[serde(rename = "match", with = "match_all")]
    pub match_all: bool,
    pub rules: Vec<Entry<L>>,
}

/// An entry of a group: a condition, or a group of its own. The wire tells
/// them apart by a group's `rules`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Entry<L> {
    Group(Group<L>),
    Leaf(L),
}

impl<L> Group<L> {
    pub fn new(match_all: bool) -> Group<L> {
        Group {
            match_all,
            rules: Vec::new(),
        }
    }

    /// A group of these conditions and no other.
    pub fn of(match_all: bool, leaves: Vec<L>) -> Group<L> {
        Group {
            match_all,
            rules: leaves.into_iter().map(Entry::Leaf).collect(),
        }
    }

    /// The conditions in the group and the groups in it, all the way down.
    pub fn leaf_count(&self) -> usize {
        self.rules.iter().map(Entry::leaf_count).sum()
    }

    /// How many groups are inside one another here, a group of conditions
    /// only being one.
    pub fn depth(&self) -> usize {
        let deepest = self.rules.iter().filter_map(|entry| match entry {
            Entry::Group(group) => Some(group.depth()),
            Entry::Leaf(_) => None,
        });
        1 + deepest.max().unwrap_or(0)
    }

    /// Whether it holds conditions only, which the first version of the
    /// rules' wire format can say.
    pub fn is_flat(&self) -> bool {
        self.rules
            .iter()
            .all(|entry| matches!(entry, Entry::Leaf(_)))
    }

    /// The conditions in the order a walk down the groups meets them.
    pub fn leaves(&self) -> Vec<&L> {
        let mut found = Vec::new();
        self.collect_leaves(&mut found);
        found
    }

    fn collect_leaves<'a>(&'a self, found: &mut Vec<&'a L>) {
        for entry in &self.rules {
            match entry {
                Entry::Leaf(leaf) => found.push(leaf),
                Entry::Group(group) => group.collect_leaves(found),
            }
        }
    }

    /// The group at `path`, a list of the places taken down the groups.
    pub fn group_at_mut(&mut self, path: &[usize]) -> Option<&mut Group<L>> {
        let Some((&first, rest)) = path.split_first() else {
            return Some(self);
        };
        match self.rules.get_mut(first)? {
            Entry::Group(group) => group.group_at_mut(rest),
            Entry::Leaf(_) => None,
        }
    }

    /// The entry at `path`, which is not the group itself.
    pub fn entry_at_mut(&mut self, path: &[usize]) -> Option<&mut Entry<L>> {
        let (&last, parent) = path.split_last()?;
        self.group_at_mut(parent)?.rules.get_mut(last)
    }

    /// Takes the entry at `path` out; the group itself can't be.
    pub fn remove_at(&mut self, path: &[usize]) -> Option<Entry<L>> {
        let (&last, parent) = path.split_last()?;
        let group = self.group_at_mut(parent)?;
        (last < group.rules.len()).then(|| group.rules.remove(last))
    }

    /// Whether the group passes where `leaf` says what each condition does,
    /// given its place among the conditions: `None` for one that doesn't
    /// count. A group none of whose conditions count wants everything.
    pub fn passes(&self, leaf: &impl Fn(usize, &L) -> Option<bool>) -> bool {
        self.verdict(0, leaf).unwrap_or(true)
    }

    fn verdict(&self, first: usize, leaf: &impl Fn(usize, &L) -> Option<bool>) -> Option<bool> {
        let mut at = first;
        let mut verdicts = Vec::new();
        for entry in &self.rules {
            verdicts.push(entry.verdict(at, leaf));
            at += entry.leaf_count();
        }
        let mut counted = verdicts.into_iter().flatten().peekable();
        counted.peek()?;
        Some(match self.match_all {
            true => counted.all(|verdict| verdict),
            false => counted.any(|verdict| verdict),
        })
    }
}

impl<L> Entry<L> {
    pub fn leaf_count(&self) -> usize {
        match self {
            Entry::Leaf(_) => 1,
            Entry::Group(group) => group.leaf_count(),
        }
    }

    fn verdict(&self, at: usize, leaf: &impl Fn(usize, &L) -> Option<bool>) -> Option<bool> {
        match self {
            Entry::Leaf(condition) => leaf(at, condition),
            // A group of "any" with nothing in it has nothing to be true by.
            Entry::Group(group) if group.rules.is_empty() && !group.match_all => Some(false),
            Entry::Group(group) => group.verdict(at, leaf),
        }
    }
}

/// The tags of the user, as the names a song under each has: the tag's own
/// and those of the tags above it, folded.
pub type TagNames = HashMap<String, Vec<String>>;

/// What conditions are judged against besides the item: the day it is, as
/// `today` counts, and the tags' names.
pub struct Now<'a> {
    pub today: i64,
    pub tags: &'a TagNames,
}

#[cfg(test)]
mod tests;
