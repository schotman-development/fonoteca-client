//! A Focus pick of the tracks page as rules, so that "save this Focus as a
//! smart playlist" can fill the editor.

use crate::focus::Focus;

use super::*;

/// The rules a Focus is, and whether a song must pass all of them; none
/// when the Focus can't be said in rules: a facet the rules have no field
/// for, or alternatives (any of several values) beside other picks, which
/// one level of rules can't hold.
pub fn from_focus(focus: &Focus) -> Option<(bool, Vec<Rule>)> {
    let mut groups: Vec<(bool, Vec<Rule>)> = Vec::new();
    let mut facets: Vec<&str> = focus.picked().into_iter().map(|(facet, _)| facet).collect();
    facets.dedup();
    for facet in facets {
        let values: Vec<&str> = (focus.picked().into_iter())
            .filter(|(picked, _)| *picked == facet)
            .map(|(_, value)| value)
            .collect();
        let rules = values
            .iter()
            .map(|value| rule_of(facet, value))
            .collect::<Option<Vec<_>>>()?;
        let alternatives = rules.len() > 1 && !focus.is_all_of(facet);
        groups.push((alternatives, rules));
    }
    if focus.favourites {
        let favourite = Rule {
            field: Field::Favourite,
            operator: Operator::Is,
            value: Some(Value::Flag(true)),
        };
        groups.push((false, vec![favourite]));
    }
    let any_of_one_facet = matches!(groups.as_slice(), [(true, _)]);
    if !any_of_one_facet && groups.iter().any(|(alternatives, _)| *alternatives) {
        return None;
    }
    let rules = groups.into_iter().flat_map(|(_, rules)| rules).collect();
    Some((!any_of_one_facet, rules))
}

/// The rule that says a facet's picked value, by the facets of the tracks
/// page.
fn rule_of(facet: &str, value: &str) -> Option<Rule> {
    let text = |field| Rule {
        field,
        operator: Operator::Is,
        value: Some(Value::Text(value.to_owned())),
    };
    match facet {
        "Genres" => Some(text(Field::Genre)),
        "Performers" => Some(text(Field::Artist)),
        "Composers" => Some(text(Field::Composer)),
        "Moods" => Some(text(Field::Mood)),
        "Format" => Some(text(Field::Format)),
        "Release date" => {
            let from: i64 = value.strip_suffix('s')?.parse().ok()?;
            Some(Rule {
                field: Field::Year,
                operator: Operator::Between,
                value: Some(Value::Range([from, from + 9])),
            })
        }
        _ => None,
    }
}
