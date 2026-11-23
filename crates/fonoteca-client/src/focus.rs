//! What the overview pages' Focus panel narrows by: filters on the fields of
//! a page's items, which the user adds and combines in groups (all of them
//! or any, nested), and how many items each value or range has under the
//! other filters. A filter is a condition of the shared rules model, so the
//! tracks page's filters and a smart playlist's rules are one thing.
//!
//! A filter that has no value yet (nothing picked, nothing typed) wants
//! every item, so a panel opens with its default filters and narrows nothing.

pub mod words;

use std::cmp::Reverse;
use std::collections::{BTreeSet, HashMap};

use serde::{Deserialize, Serialize};

pub use crate::rules::words::Measure;
use crate::rules::{Entry, FieldValue, Group, Now, Operator, Span, Unit, Value, passes};

/// The most played steps, by how many items each takes, and their names.
const TOP_STEPS: [usize; 6] = [25, 50, 100, 250, 500, 1000];
const TOP_LABELS: [&str; 6] = [
    "Top 25", "Top 50", "Top 100", "Top 250", "Top 500", "Top 1000",
];
/// The recently added steps, by how many days back each reaches, and their
/// names.
const RECENT_DAYS: [u32; 3] = [7, 31, 365];
const RECENT_LABELS: [&str; 3] = ["Last week", "Last month", "Last year"];

/// The key of the field the heart narrows by, which every page that has a
/// heart has a flag of.
pub const FAVOURITE: &str = "favourite";

/// What a field holds, which decides the operators it takes and the control
/// that sets them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Words typed in a box.
    Text,
    /// Values the items have, listed with their counts to pick from.
    Values,
    Number(Measure),
    Date,
    Flag,
}

impl Kind {
    /// The operators a field of this kind takes, the first being what a new
    /// filter has. A values field takes the text ones too, which a smart
    /// playlist's rules say.
    pub fn operators(self) -> &'static [Operator] {
        use Operator::*;
        match self {
            Kind::Text => &[
                Contains,
                Is,
                IsNot,
                StartsWith,
                NotContains,
                IsEmpty,
                IsNotEmpty,
            ],
            Kind::Values => &[
                AnyOf,
                AllOf,
                NoneOf,
                Is,
                IsNot,
                Contains,
                NotContains,
                StartsWith,
                IsEmpty,
                IsNotEmpty,
            ],
            Kind::Number(_) => &[Between, Is, IsNot, GreaterThan, LessThan],
            Kind::Date => &[InTheLast, NotInTheLast, Before, After, Between, Never],
            Kind::Flag => &[Is],
        }
    }
}

/// How a values field lists its values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Order {
    /// The most items first, ties by name.
    Frequency,
    /// By name, the last first, which puts the newest decade first.
    LabelDescending,
}

/// What an overview page's items can be told apart by.
pub struct Field<T> {
    /// What conditions and saved filters call the field; for the tracks page
    /// the name the smart playlists' wire gives it.
    pub key: &'static str,
    pub title: &'static str,
    /// The heading of the "Add filter" menu it is listed under.
    pub group: &'static str,
    pub kind: Kind,
    pub order: Order,
    pub get: fn(&T, &Now) -> FieldValue,
}

type Getter<T> = fn(&T, &Now) -> FieldValue;

const fn field<T>(
    key: &'static str,
    title: &'static str,
    group: &'static str,
    kind: Kind,
    get: Getter<T>,
) -> Field<T> {
    Field {
        key,
        title,
        group,
        kind,
        order: Order::Frequency,
        get,
    }
}

pub const fn text<T>(
    key: &'static str,
    title: &'static str,
    group: &'static str,
    get: Getter<T>,
) -> Field<T> {
    field(key, title, group, Kind::Text, get)
}

pub const fn values<T>(
    key: &'static str,
    title: &'static str,
    group: &'static str,
    get: Getter<T>,
) -> Field<T> {
    field(key, title, group, Kind::Values, get)
}

/// A values field that sorts by name, the last first, such as decades.
pub const fn descending<T>(
    key: &'static str,
    title: &'static str,
    group: &'static str,
    get: Getter<T>,
) -> Field<T> {
    Field {
        order: Order::LabelDescending,
        ..values(key, title, group, get)
    }
}

pub const fn number<T>(
    key: &'static str,
    title: &'static str,
    group: &'static str,
    measure: Measure,
    get: Getter<T>,
) -> Field<T> {
    field(key, title, group, Kind::Number(measure), get)
}

pub const fn date<T>(
    key: &'static str,
    title: &'static str,
    group: &'static str,
    get: Getter<T>,
) -> Field<T> {
    field(key, title, group, Kind::Date, get)
}

pub const fn flag<T>(
    key: &'static str,
    title: &'static str,
    group: &'static str,
    get: Getter<T>,
) -> Field<T> {
    field(key, title, group, Kind::Flag, get)
}

fn find<'a, T>(fields: &'a [Field<T>], key: &str) -> Option<&'a Field<T>> {
    fields.iter().find(|field| field.key == key)
}

/// What a page's fields are called and hold, apart from the items: what the
/// panel needs to list, add and set the filters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Info {
    pub key: &'static str,
    pub title: &'static str,
    pub group: &'static str,
    pub kind: Kind,
}

pub fn infos<T>(fields: &[Field<T>]) -> Vec<Info> {
    (fields.iter())
        .map(|field| Info {
            key: field.key,
            title: field.title,
            group: field.group,
            kind: field.kind,
        })
        .collect()
}

pub fn info_of<'a>(infos: &'a [Info], key: &str) -> Option<&'a Info> {
    infos.iter().find(|info| info.key == key)
}

fn enabled() -> bool {
    true
}

fn is_enabled(enabled: &bool) -> bool {
    *enabled
}

/// One filter: a field, how to compare it and with what, and whether it
/// counts at the moment.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Condition {
    pub field: String,
    pub operator: Operator,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
    #[serde(default = "enabled", skip_serializing_if = "is_enabled")]
    pub enabled: bool,
    /// What was typed in the boxes for the value, which may not read as one
    /// yet; it is not kept.
    #[serde(skip)]
    pub typed: Option<[String; 2]>,
}

impl Condition {
    /// A new filter on `field`, which wants everything until it is given a
    /// value.
    pub fn new(field: &Info) -> Condition {
        Condition {
            field: field.key.to_owned(),
            operator: field.kind.operators()[0],
            value: None,
            enabled: true,
            typed: None,
        }
    }

    /// Whether the filter says anything: it is on, and has what its operator
    /// compares with.
    pub fn is_active(&self) -> bool {
        self.enabled && self.is_complete()
    }

    fn is_complete(&self) -> bool {
        match (self.operator, &self.value) {
            (Operator::Never | Operator::IsEmpty | Operator::IsNotEmpty, _) => true,
            (_, None) => false,
            (_, Some(Value::Text(text))) => !text.trim().is_empty(),
            (_, Some(Value::List(listed))) => !listed.is_empty(),
            (_, Some(_)) => true,
        }
    }

    /// The values picked, where the filter lists values to pick.
    pub fn picked(&self) -> &[String] {
        match (&self.value, self.operator) {
            (Some(Value::List(listed)), Operator::AnyOf | Operator::AllOf | Operator::NoneOf) => {
                listed
            }
            _ => &[],
        }
    }
}

/// What is filtered on a page: conditions, in groups.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Focus {
    pub filters: Group<Condition>,
}

impl Default for Focus {
    fn default() -> Self {
        Focus {
            filters: Group::new(true),
        }
    }
}

impl Focus {
    /// A focus of a filter for each of these fields.
    pub fn of(keys: &[&str], fields: &[Info]) -> Focus {
        let conditions = (keys.iter())
            .filter_map(|key| info_of(fields, key))
            .map(Condition::new)
            .collect();
        Focus {
            filters: Group::of(true, conditions),
        }
    }

    pub fn condition_mut(&mut self, path: &[usize]) -> Option<&mut Condition> {
        match self.filters.entry_at_mut(path)? {
            Entry::Leaf(condition) => Some(condition),
            Entry::Group(_) => None,
        }
    }

    /// Adds `condition` to the group at `path`.
    pub fn add(&mut self, path: &[usize], condition: Condition) {
        if let Some(group) = self.filters.group_at_mut(path) {
            group.rules.push(Entry::Leaf(condition));
        }
    }

    /// Adds an empty group to the group at `path`, unless that would nest
    /// them deeper than the rules' wire format allows.
    pub fn add_group(&mut self, path: &[usize], match_all: bool) {
        let deepest = crate::rules::MOST_DEPTH;
        if path.len() + 2 > deepest {
            return;
        }
        if let Some(group) = self.filters.group_at_mut(path) {
            group.rules.push(Entry::Group(Group::new(match_all)));
        }
    }

    pub fn toggle_match_all(&mut self, path: &[usize]) {
        if let Some(group) = self.filters.group_at_mut(path) {
            group.match_all = !group.match_all;
        }
    }

    pub fn remove(&mut self, path: &[usize]) {
        self.filters.remove_at(path);
    }

    /// Picks `value` of the values filter at `path`, or lets it go where it
    /// is picked.
    pub fn toggle(&mut self, path: &[usize], value: &str) {
        let Some(condition) = self.condition_mut(path) else {
            return;
        };
        let mut picked: BTreeSet<String> = condition.picked().iter().cloned().collect();
        if !picked.remove(value) {
            picked.insert(value.to_owned());
        }
        condition.value = Some(Value::List(picked.into_iter().collect()));
    }

    /// Whether the heart is on: a filter wants the favourites alone.
    pub fn favourites(&self) -> bool {
        self.filters.rules.iter().any(|entry| match entry {
            Entry::Leaf(condition) => is_favourites(condition),
            Entry::Group(_) => false,
        })
    }

    /// Turns the heart on, or off where it is on.
    pub fn toggle_favourites(&mut self) {
        if !self.favourites() {
            let wanted = Condition {
                field: FAVOURITE.to_owned(),
                operator: Operator::Is,
                value: Some(Value::Flag(true)),
                enabled: true,
                typed: None,
            };
            return self.filters.rules.push(Entry::Leaf(wanted));
        }
        self.filters.rules.retain(|entry| match entry {
            Entry::Leaf(condition) => !is_favourites(condition),
            Entry::Group(_) => true,
        });
    }

    /// Where each group and condition is, in the order the panel lists them:
    /// a group, then what is in it.
    pub fn places(&self) -> Vec<Place> {
        let mut found = Vec::new();
        collect_places(&self.filters, &mut Vec::new(), &mut found);
        found
    }

    /// Whether anything narrows the page.
    pub fn is_narrowing(&self) -> bool {
        self.filters
            .leaves()
            .iter()
            .any(|condition| condition.is_active())
            || has_empty_any_group(&self.filters)
    }

    /// Lets every value go and wants all items. The filters stay, as do
    /// their operators and whether they are on.
    pub fn clear(&mut self) {
        clear_group(&mut self.filters);
    }

    /// Lets go of the filters on fields the page doesn't have, which a
    /// saved set of another version may hold.
    pub fn retain_known(&mut self, fields: &[Info]) {
        retain_known(&mut self.filters, fields);
    }
}

/// An empty group of "any" matches nothing, so it narrows the page by itself.
fn has_empty_any_group(group: &Group<Condition>) -> bool {
    group.rules.iter().any(|entry| match entry {
        Entry::Group(inner) => {
            (inner.rules.is_empty() && !inner.match_all) || has_empty_any_group(inner)
        }
        Entry::Leaf(_) => false,
    })
}

fn retain_known(group: &mut Group<Condition>, fields: &[Info]) {
    group.rules.retain_mut(|entry| match entry {
        Entry::Group(inner) => {
            retain_known(inner, fields);
            true
        }
        Entry::Leaf(condition) => info_of(fields, &condition.field).is_some(),
    });
}

/// Where a group or a condition is: the places taken down the groups.
#[derive(Debug, Clone, PartialEq)]
pub struct Place {
    pub path: Vec<usize>,
    pub is_group: bool,
}

fn collect_places(group: &Group<Condition>, path: &mut Vec<usize>, found: &mut Vec<Place>) {
    found.push(Place {
        path: path.clone(),
        is_group: true,
    });
    for (at, entry) in group.rules.iter().enumerate() {
        path.push(at);
        match entry {
            Entry::Group(inner) => collect_places(inner, path, found),
            Entry::Leaf(_) => found.push(Place {
                path: path.clone(),
                is_group: false,
            }),
        }
        path.pop();
    }
}

fn is_favourites(condition: &Condition) -> bool {
    condition.field == FAVOURITE
        && condition.operator == Operator::Is
        && condition.value == Some(Value::Flag(true))
        && condition.enabled
}

fn clear_group(group: &mut Group<Condition>) {
    for entry in &mut group.rules {
        match entry {
            Entry::Group(inner) => clear_group(inner),
            Entry::Leaf(condition) => {
                condition.value = None;
                condition.typed = None;
            }
        }
    }
}

/// A value of a field and how many items have it.
#[derive(Debug, Clone, PartialEq)]
pub struct Count {
    pub label: String,
    pub count: usize,
}

/// What the page's items come to under a filter.
#[derive(Debug, Clone, PartialEq)]
pub enum Tally {
    /// A filter nothing is counted for.
    Nothing,
    /// The values of a field in the order it lists them, each with the items
    /// that have it under the other filters; a value no item has under them
    /// stays listed, with none.
    Values(Vec<Count>),
    /// How many items the filter leaves of those the other filters leave,
    /// and the least and the most among all the items.
    Range {
        left: usize,
        of: usize,
        bounds: Option<(i64, i64)>,
    },
}

/// The tallies of each filter, in the order a walk down the groups meets
/// them.
pub type Counted = Vec<Tally>;

/// The days to count back for each of the recently added steps, and their
/// names.
pub fn recent_steps() -> Vec<(&'static str, Span)> {
    (RECENT_LABELS.iter().zip(RECENT_DAYS))
        .map(|(&label, days)| {
            let span = Span {
                amount: days,
                unit: Unit::Days,
            };
            (label, span)
        })
        .collect()
}

/// The names of the most played steps that narrow `len` items: a step as
/// large as the items are, or larger, holds them all.
pub fn top_labels(len: usize) -> Vec<&'static str> {
    (TOP_STEPS.iter().zip(TOP_LABELS))
        .take_while(|&(&step, _)| step < len)
        .map(|(_, label)| label)
        .collect()
}

/// The play count that takes an item into the top `label` of the items with
/// these `plays`: the count of the item that ends that step.
pub fn top_threshold(plays: &[i64], label: &str) -> Option<i64> {
    let step = TOP_STEPS[TOP_LABELS.iter().position(|&name| name == label)?];
    let mut played: Vec<i64> = plays.iter().copied().filter(|&count| count > 0).collect();
    played.sort_unstable_by_key(|&count| Reverse(count));
    played.get(step.min(played.len()).checked_sub(1)?).copied()
}

/// One filter of a focus, with the field it is on, where the field is known.
struct Leaf<'a, T> {
    condition: &'a Condition,
    field: Option<&'a Field<T>>,
}

fn leaves<'a, T>(focus: &'a Focus, fields: &'a [Field<T>]) -> Vec<Leaf<'a, T>> {
    (focus.filters.leaves().into_iter())
        .map(|condition| Leaf {
            condition,
            field: find(fields, &condition.field),
        })
        .collect()
}

impl<T> Leaf<'_, T> {
    /// The field, where the filter says something about it.
    fn active_field(&self) -> Option<&Field<T>> {
        self.field.filter(|_| self.condition.is_active())
    }

    fn passed_by(&self, item: &T, now: &Now) -> bool {
        let Some(field) = self.field else { return true };
        let have = (field.get)(item, now);
        passes(&have, self.condition.operator, &self.condition.value, now)
    }
}

/// Whether each item passes every filter in its place among the groups.
pub fn matching<T>(items: &[T], fields: &[Field<T>], focus: &Focus, now: &Now) -> Vec<bool> {
    let leaves = leaves(focus, fields);
    let passed = passed_columns(items, &leaves, now);
    (0..items.len())
        .map(|i| {
            focus
                .filters
                .passes(&|at, _| passed[at].as_ref().map(|column| column[i]))
        })
        .collect()
}

/// For each active filter, whether each item passes it.
fn passed_columns<T>(items: &[T], leaves: &[Leaf<T>], now: &Now) -> Vec<Option<Vec<bool>>> {
    (leaves.iter())
        .map(|leaf| {
            leaf.active_field()?;
            Some(items.iter().map(|item| leaf.passed_by(item, now)).collect())
        })
        .collect()
}

/// What each filter counts under the others: for a filter of values, those
/// its items have and how many items have each; for a number or a date, how
/// many it leaves and the bounds of the library. One pass over the items: an
/// item is looked at once per filter, with every other filter in place.
pub fn count<T>(items: &[T], fields: &[Field<T>], focus: &Focus, now: &Now) -> Counted {
    let leaves = leaves(focus, fields);
    let passed = passed_columns(items, &leaves, now);
    let mut tallies: Vec<Accumulator> = leaves.iter().map(Accumulator::of).collect();
    for (i, item) in items.iter().enumerate() {
        let everything = focus
            .filters
            .passes(&|at, _| passed[at].as_ref().map(|column| column[i]));
        for (c, tally) in tallies.iter_mut().enumerate() {
            let Some(field) = leaves[c].field else {
                continue;
            };
            if matches!(tally, Accumulator::Nothing) {
                continue;
            }
            let without_this = match passed[c] {
                Some(_) => focus.filters.passes(&|at, _| {
                    passed[at]
                        .as_ref()
                        .filter(|_| at != c)
                        .map(|column| column[i])
                }),
                None => everything,
            };
            let this_passes = passed[c].as_ref().is_none_or(|column| column[i]);
            tally.add(&(field.get)(item, now), without_this, this_passes);
        }
    }
    (tallies.into_iter().zip(&leaves))
        .map(|(tally, leaf)| tally.finish(leaf.field.map(|field| field.order)))
        .collect()
}

/// What a filter has counted so far.
enum Accumulator {
    Nothing,
    Values(HashMap<String, usize>),
    Range {
        left: usize,
        of: usize,
        bounds: Option<(i64, i64)>,
    },
}

impl Accumulator {
    fn of<T>(leaf: &Leaf<T>) -> Accumulator {
        match leaf.field.map(|field| field.kind) {
            Some(Kind::Values) => Accumulator::Values(HashMap::new()),
            Some(Kind::Number(_) | Kind::Date) => Accumulator::Range {
                left: 0,
                of: 0,
                bounds: None,
            },
            _ => Accumulator::Nothing,
        }
    }

    /// Takes in an item that has `have`, which passes the other filters or
    /// not, and the filter itself or not.
    fn add(&mut self, have: &FieldValue, passes_others: bool, passes_this: bool) {
        match (self, have) {
            (Accumulator::Values(tally), FieldValue::Texts(values)) => {
                let mut seen = BTreeSet::new();
                for value in values.iter().map(|value| value.trim()) {
                    if value.is_empty() || !seen.insert(value) {
                        continue;
                    }
                    let counted = tally.entry(value.to_owned()).or_default();
                    *counted += usize::from(passes_others);
                }
            }
            (Accumulator::Range { left, of, bounds }, have) => {
                *of += usize::from(passes_others);
                *left += usize::from(passes_others && passes_this);
                if let FieldValue::Number(Some(at)) | FieldValue::Day(Some(at)) = have {
                    *bounds = Some(match *bounds {
                        Some((least, most)) => (least.min(*at), most.max(*at)),
                        None => (*at, *at),
                    });
                }
            }
            _ => {}
        }
    }

    fn finish(self, order: Option<Order>) -> Tally {
        match (self, order) {
            (Accumulator::Values(tally), Some(order)) => Tally::Values(ordered(order, tally)),
            (Accumulator::Range { left, of, bounds }, _) => Tally::Range { left, of, bounds },
            _ => Tally::Nothing,
        }
    }
}

fn ordered(order: Order, tally: HashMap<String, usize>) -> Vec<Count> {
    let mut values: Vec<Count> = (tally.into_iter())
        .map(|(label, count)| Count { label, count })
        .collect();
    match order {
        Order::Frequency => {
            values.sort_by(|a, b| b.count.cmp(&a.count).then(a.label.cmp(&b.label)))
        }
        Order::LabelDescending => values.sort_by(|a, b| b.label.cmp(&a.label)),
    }
    values
}

/// The decade a year is in, as "1970s".
pub fn decade(year: i64) -> Option<String> {
    (year > 0).then(|| format!("{}s", year / 10 * 10))
}

#[cfg(test)]
mod tests;
