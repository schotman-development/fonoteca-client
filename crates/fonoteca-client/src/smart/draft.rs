//! A smart playlist as the editor holds it while it is made: the rules as
//! typed, some of which may not read yet, and what the server will be sent
//! once they all do.

use super::words::{parse_rule, sort_index, value_text};
use super::*;

/// A rule row of the editor: its field and operator, and its value as typed.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub field: Field,
    pub operator: Operator,
    pub text: String,
}

impl Row {
    fn new(field: Field) -> Row {
        Row {
            field,
            operator: field.operators()[0],
            text: match field.kind() {
                Kind::Flag => "yes".to_owned(),
                _ => String::new(),
            },
        }
    }

    fn of(rule: &Rule) -> Row {
        Row {
            field: rule.field,
            operator: rule.operator,
            text: value_text(rule),
        }
    }

    pub fn rule(&self) -> Option<Rule> {
        parse_rule(self.field, self.operator, &self.text)
    }

    /// Whether something was typed that doesn't read; a row left empty is
    /// only waiting for its value.
    pub fn is_broken(&self) -> bool {
        !self.text.trim().is_empty() && self.rule().is_none()
    }

    pub fn field_index(&self) -> usize {
        Field::ALL
            .iter()
            .position(|&field| field == self.field)
            .unwrap_or(0)
    }

    pub fn operator_index(&self) -> usize {
        (self.field.operators().iter())
            .position(|&operator| operator == self.operator)
            .unwrap_or(0)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Draft {
    /// The playlist being edited; none for a new one.
    pub playlist_id: Option<String>,
    pub name: String,
    pub match_all: bool,
    pub rows: Vec<Row>,
    /// Which of `SORTS`.
    pub sort: usize,
    /// As typed; empty for the server's own.
    pub limit: String,
}

impl Default for Draft {
    fn default() -> Self {
        Draft {
            playlist_id: None,
            name: String::new(),
            match_all: true,
            rows: vec![Row::new(Field::Genre)],
            sort: sort_index(Sort {
                by: SortKey::Added,
                descending: true,
            }),
            limit: DEFAULT_LIMIT.to_string(),
        }
    }
}

impl Draft {
    /// A new playlist of these rules, the order and limit a new one has.
    pub fn of_rules(match_all: bool, rules: &[Rule]) -> Draft {
        Draft {
            match_all,
            rows: rules.iter().map(Row::of).collect(),
            ..Draft::default()
        }
    }

    /// The playlist `id` named `name` as its rules say.
    pub fn editing(id: &str, name: &str, rules: &SmartRules) -> Draft {
        Draft {
            playlist_id: Some(id.to_owned()),
            name: name.to_owned(),
            sort: sort_index(rules.sort),
            limit: rules
                .limit
                .map(|limit| limit.to_string())
                .unwrap_or_default(),
            ..Draft::of_rules(rules.match_all, &rules.rules)
        }
    }

    pub fn add_row(&mut self) {
        self.rows.push(Row::new(Field::Genre));
    }

    pub fn remove_row(&mut self, at: usize) {
        if at < self.rows.len() {
            self.rows.remove(at);
        }
    }

    /// Has the row compare another field, with that field's first operator;
    /// what was typed stays where the kind of value is the same.
    pub fn set_field(&mut self, at: usize, field: usize) {
        let (Some(row), Some(&field)) = (self.rows.get_mut(at), Field::ALL.get(field)) else {
            return;
        };
        let new = Row::new(field);
        let text = match row.field.kind() == field.kind() {
            true => std::mem::take(&mut row.text),
            false => new.text.clone(),
        };
        *row = Row { text, ..new };
    }

    pub fn set_operator(&mut self, at: usize, operator: usize) {
        let Some(row) = self.rows.get_mut(at) else {
            return;
        };
        if let Some(&operator) = row.field.operators().get(operator) {
            row.operator = operator;
        }
    }

    pub fn set_text(&mut self, at: usize, text: &str) {
        if let Some(row) = self.rows.get_mut(at) {
            row.text = text.to_owned();
        }
    }

    pub fn sort_by(&self) -> Sort {
        words::SORTS[self.sort.min(words::SORTS.len() - 1)].1
    }

    /// The limit as typed; `Err` when it is not a number.
    fn limit(&self) -> Result<Option<u32>, String> {
        match self.limit.trim() {
            "" => Ok(None),
            typed => typed
                .parse()
                .map(Some)
                .map_err(|_| "The limit is a number of songs, or nothing.".to_owned()),
        }
    }

    /// What the server is sent: the rules once every row reads and the
    /// limit does, or why not.
    pub fn rules(&self) -> Result<SmartRules, String> {
        let rules = (self.rows.iter().map(Row::rule))
            .collect::<Option<Vec<_>>>()
            .ok_or("Fix the rules marked, or take them out.")?;
        let rules = self.rules_of(rules, self.limit()?);
        rules.check().map(|()| rules)
    }

    /// What keeps the draft from being saved, said once the user has typed
    /// something wrong; none while rows are merely still empty.
    pub fn problem(&self) -> String {
        if let Err(why) = self.limit() {
            return why;
        }
        if self.rows.iter().any(Row::is_broken) {
            return "Fix the rules marked, or take them out.".to_owned();
        }
        let waiting = self.rows.iter().any(|row| row.rule().is_none());
        match self.rules() {
            Err(why) if !waiting => why,
            _ => String::new(),
        }
    }

    /// The rules of the rows that read, for a preview that follows what is
    /// typed; none while no row does.
    pub fn readable_rules(&self) -> Option<SmartRules> {
        let rules: Vec<Rule> = self.rows.iter().filter_map(Row::rule).collect();
        let limit = self.limit().ok().flatten();
        (!rules.is_empty()).then(|| self.rules_of(rules, limit))
    }

    fn rules_of(&self, rules: Vec<Rule>, limit: Option<u32>) -> SmartRules {
        SmartRules {
            match_all: self.match_all,
            rules,
            sort: self.sort_by(),
            limit,
        }
    }
}
