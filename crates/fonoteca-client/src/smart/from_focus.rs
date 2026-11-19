//! The Focus of the tracks page as rules, so that "save this filter as a
//! smart playlist" fills the editor, and rules as a Focus, so that a smart
//! playlist's rules open in the Focus. Both are one tree of conditions; only
//! what the server's rules can't say, or the Focus can't show, is refused.

use crate::focus::{Condition, Focus};
use crate::rules::iso_date;
use crate::rules::words::operator_label;

use super::*;

/// The rules a Focus is: the filters that say something, in the groups they
/// are in; those a group has none of are left out with it. Where a filter
/// has no rule, why not, naming its field as `title_of` does.
pub fn from_focus(focus: &Focus, title_of: &dyn Fn(&str) -> String) -> Result<Group<Rule>, String> {
    rules_of(&focus.filters, title_of)
}

fn rules_of(
    group: &Group<Condition>,
    title_of: &dyn Fn(&str) -> String,
) -> Result<Group<Rule>, String> {
    let mut rules = Vec::new();
    for entry in &group.rules {
        match entry {
            Entry::Group(inner) => {
                let inner = rules_of(inner, title_of)?;
                if inner.rules.is_empty() && !inner.match_all {
                    return Err("A group of any with no filters matches nothing.".into());
                }
                if !inner.rules.is_empty() {
                    rules.push(Entry::Group(inner));
                }
            }
            Entry::Leaf(condition) if condition.is_active() => {
                rules.push(entry_of(condition, title_of)?)
            }
            Entry::Leaf(_) => {}
        }
    }
    Ok(Group {
        match_all: group.match_all,
        rules,
    })
}

/// The rule a filter is, or the group of rules where it needs several.
fn entry_of(
    condition: &Condition,
    title_of: &dyn Fn(&str) -> String,
) -> Result<Entry<Rule>, String> {
    let title = title_of(&condition.field);
    if condition.field == DECADE {
        return decades(condition, &title);
    }
    let field = Field::from_key(&condition.field)
        .ok_or_else(|| format!("{title} has no smart playlist rule."))?;
    let rule = |operator, value| Rule {
        field,
        operator,
        value: Some(value),
    };
    let listed = |operator, all: bool| match &condition.value {
        Some(Value::List(values)) if values.len() == 1 => {
            Entry::Leaf(rule(operator, Value::Text(values[0].clone())))
        }
        Some(Value::List(values)) => Entry::Group(Group::of(
            all,
            (values.iter())
                .map(|value| rule(operator, Value::Text(value.clone())))
                .collect(),
        )),
        _ => Entry::Group(Group::new(all)),
    };
    let entry = match (condition.operator, &condition.value) {
        (Operator::AnyOf, _) => listed(Operator::Is, false),
        (Operator::AllOf, _) => listed(Operator::Is, true),
        (Operator::NoneOf, _) => listed(Operator::IsNot, true),
        (Operator::Between, Some(Value::Range([from, to]))) if field.kind() == Kind::Date => {
            let after = Value::Text(iso_date(from - 1));
            let before = Value::Text(iso_date(to + 1));
            Entry::Group(Group::of(
                true,
                vec![rule(Operator::After, after), rule(Operator::Before, before)],
            ))
        }
        (operator, value) if field.operators().contains(&operator) => Entry::Leaf(Rule {
            field,
            operator,
            value: value.clone(),
        }),
        (operator, _) => {
            return Err(format!(
                "{title} \"{}\" has no smart playlist rule.",
                operator_label(operator)
            ));
        }
    };
    check_entry(&entry, &title).map(|()| entry)
}

/// The key of the Focus filter on the decade of the release year.
const DECADE: &str = "decade";

/// A pick of decades, which the rules say as the years from a decade's first
/// to its last.
fn decades(condition: &Condition, title: &str) -> Result<Entry<Rule>, String> {
    let (Operator::AnyOf, Some(Value::List(picked))) = (condition.operator, &condition.value)
    else {
        return Err(format!("{title} has no smart playlist rule."));
    };
    let years = |label: &String| {
        let first: i64 = label.strip_suffix('s')?.parse().ok()?;
        Some(Rule {
            field: Field::Year,
            operator: Operator::Between,
            value: Some(Value::Range([first, first + 9])),
        })
    };
    let rules: Option<Vec<Rule>> = picked.iter().map(years).collect();
    let rules = rules.ok_or_else(|| format!("{title} has no smart playlist rule."))?;
    Ok(match <[Rule; 1]>::try_from(rules) {
        Ok([only]) => Entry::Leaf(only),
        Err(rules) => Entry::Group(Group::of(false, rules)),
    })
}

fn check_entry(entry: &Entry<Rule>, title: &str) -> Result<(), String> {
    let rules = match entry {
        Entry::Leaf(rule) => vec![rule],
        Entry::Group(group) => group.leaves(),
    };
    (rules.into_iter())
        .try_for_each(Rule::check)
        .map_err(|why| format!("{title}: {why}"))
}

/// The Focus the rules are: each rule a filter on the field of the same
/// name, in the same groups.
pub fn into_focus(rules: &Group<Rule>) -> Focus {
    Focus {
        filters: condition_group(rules),
    }
}

fn condition_group(group: &Group<Rule>) -> Group<Condition> {
    Group {
        match_all: group.match_all,
        rules: (group.rules.iter())
            .map(|entry| match entry {
                Entry::Group(inner) => Entry::Group(condition_group(inner)),
                Entry::Leaf(rule) => Entry::Leaf(Condition {
                    field: rule.field.key(),
                    operator: rule.operator,
                    value: rule.value.clone(),
                    enabled: true,
                    typed: None,
                }),
            })
            .collect(),
    }
}
