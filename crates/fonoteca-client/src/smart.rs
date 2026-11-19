//! Smart playlists' rules: which songs a playlist wants, by what they are
//! (their genre, when they were added, whether they were played), how the
//! songs found are ordered and how many are kept. The JSON is the shape of
//! Fonoteca's `fonotecaSmartPlaylists` extension, which the server keeps and
//! which the editor's preview is checked against: version 1 where the rules
//! are conditions only, version 2 where they are in groups. A song that has
//! no value for a field fails every rule on it but the negative ones.

pub mod draft;
pub mod from_focus;
pub mod words;

use std::cmp::Ordering;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use opensubsonic::{Child, Tag};
use serde::{Deserialize, Serialize};

use crate::browse::matching::fold_words as fold;
pub use crate::rules::{Entry, Group, Now, Operator, TagNames, Value};
use crate::rules::{FieldValue, MOST_DEPTH, day_number, passes};

/// What the server keeps when no limit is sent.
pub const DEFAULT_LIMIT: u32 = 500;
/// The most songs a smart playlist may keep.
pub const MOST_LIMIT: u32 = 5000;
/// The most rules in all that the second version of the wire format takes.
pub const MOST_RULES: usize = 50;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Field {
    Title,
    Artist,
    AlbumArtist,
    Album,
    Genre,
    Composer,
    Mood,
    Tag,
    Format,
    Year,
    PlayCount,
    Rating,
    Duration,
    Bpm,
    BitRate,
    BitDepth,
    SamplingRate,
    Added,
    LastPlayed,
    Favourite,
}

/// What a field holds, which decides the operators it takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Text,
    Number,
    Date,
    Flag,
}

impl Field {
    pub const ALL: [Field; 20] = [
        Field::Title,
        Field::Artist,
        Field::AlbumArtist,
        Field::Album,
        Field::Genre,
        Field::Composer,
        Field::Mood,
        Field::Tag,
        Field::Format,
        Field::Year,
        Field::PlayCount,
        Field::Rating,
        Field::Duration,
        Field::Bpm,
        Field::BitRate,
        Field::BitDepth,
        Field::SamplingRate,
        Field::Added,
        Field::LastPlayed,
        Field::Favourite,
    ];

    pub fn kind(self) -> Kind {
        match self {
            Field::Title
            | Field::Artist
            | Field::AlbumArtist
            | Field::Album
            | Field::Genre
            | Field::Composer
            | Field::Mood
            | Field::Tag
            | Field::Format => Kind::Text,
            Field::Added | Field::LastPlayed => Kind::Date,
            Field::Favourite => Kind::Flag,
            _ => Kind::Number,
        }
    }

    /// The operators the field takes, the first being what a new rule has.
    pub fn operators(self) -> &'static [Operator] {
        use Operator::*;
        match (self.kind(), self) {
            (Kind::Text, _) => &[Is, IsNot, Contains, NotContains, StartsWith],
            (Kind::Number, _) => &[Is, IsNot, GreaterThan, LessThan, Between],
            (Kind::Date, Field::LastPlayed) => &[InTheLast, NotInTheLast, Before, After, Never],
            (Kind::Date, _) => &[InTheLast, NotInTheLast, Before, After],
            (Kind::Flag, _) => &[Is],
        }
    }

    /// The name the wire gives the field, which the Focus names it by too.
    pub fn key(self) -> String {
        match serde_json::to_value(self) {
            Ok(serde_json::Value::String(key)) => key,
            _ => unreachable!("a field is a plain name"),
        }
    }

    pub fn from_key(key: &str) -> Option<Field> {
        Field::ALL.into_iter().find(|field| field.key() == key)
    }

    /// What a song has in the field.
    pub fn value_of(self, song: &Child, now: &Now) -> FieldValue {
        match self.kind() {
            Kind::Text => FieldValue::Texts(texts_of(song, self, now)),
            Kind::Number => FieldValue::Number(number_of(song, self)),
            Kind::Date => FieldValue::Day(date_of(song, self)),
            Kind::Flag => FieldValue::Flag(song.starred.is_some()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Rule {
    pub field: Field,
    pub operator: Operator,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
}

impl Rule {
    /// Why the rule can't be kept, as the server would refuse it.
    pub fn check(&self) -> Result<(), String> {
        if !self.field.operators().contains(&self.operator) {
            return Err(format!(
                "{} can't be compared with \"{}\".",
                words::field_label(self.field),
                words::operator_label(self.operator)
            ));
        }
        let fits = match (self.operator, self.field.kind(), &self.value) {
            (Operator::Never, _, None) => true,
            (Operator::Is | Operator::IsNot, Kind::Number, Some(Value::Number(_))) => true,
            (Operator::Is | Operator::IsNot, Kind::Text, Some(Value::Text(_))) => true,
            (Operator::Is, Kind::Flag, Some(Value::Flag(_))) => true,
            (Operator::Contains | Operator::NotContains | Operator::StartsWith, _, value) => {
                matches!(value, Some(Value::Text(_)))
            }
            (Operator::GreaterThan | Operator::LessThan, _, Some(Value::Number(_))) => true,
            (Operator::Between, _, Some(Value::Range([from, to]))) => from <= to,
            (Operator::InTheLast | Operator::NotInTheLast, _, Some(Value::Within(span))) => {
                span.amount > 0
            }
            (Operator::Before | Operator::After, _, Some(Value::Text(date))) => {
                date.len() == 10 && day_number(date).is_some()
            }
            _ => false,
        };
        match fits {
            true => Ok(()),
            false => Err(format!(
                "{} needs another value.",
                words::field_label(self.field)
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SortKey {
    Random,
    Title,
    Artist,
    Album,
    Year,
    PlayCount,
    LastPlayed,
    Added,
    Rating,
    Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sort {
    pub by: SortKey,
    #[serde(default)]
    pub descending: bool,
}

impl Default for Sort {
    fn default() -> Self {
        Sort {
            by: SortKey::Title,
            descending: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SmartRules {
    #[serde(flatten)]
    pub root: Group<Rule>,
    #[serde(default)]
    pub sort: Sort,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

impl SmartRules {
    /// The rules as the server sends them in a playlist, refusing what it
    /// would refuse.
    pub fn from_value(json: serde_json::Value) -> Result<SmartRules, String> {
        let rules: SmartRules = serde_json::from_value(json).map_err(|e| e.to_string())?;
        rules.check().map(|()| rules)
    }

    #[cfg(test)]
    pub fn from_json(json: &str) -> Result<SmartRules, String> {
        Self::from_value(serde_json::from_str(json).map_err(|e| e.to_string())?)
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("rules are plain data")
    }

    /// The version of the extension these rules need: the first says
    /// conditions, the second groups of them too.
    pub fn version(&self) -> i64 {
        match self.root.is_flat() {
            true => 1,
            false => 2,
        }
    }

    pub fn check(&self) -> Result<(), String> {
        if self.root.rules.is_empty() {
            return Err("Add at least one rule.".to_owned());
        }
        if self
            .limit
            .is_some_and(|limit| !(1..=MOST_LIMIT).contains(&limit))
        {
            return Err(format!("The limit is from 1 to {MOST_LIMIT} songs."));
        }
        if self.root.depth() > MOST_DEPTH {
            return Err(format!("Groups go {MOST_DEPTH} deep at most."));
        }
        if self.root.leaf_count() > MOST_RULES {
            return Err(format!("There are {MOST_RULES} rules at most."));
        }
        check_group(&self.root, &mut 0)
    }
}

/// Checks the rules of a group and of the groups in it; `seen` counts the
/// rules met, as the server numbers them in its messages.
fn check_group(group: &Group<Rule>, seen: &mut usize) -> Result<(), String> {
    if group.rules.is_empty() {
        return Err("A group has no rules.".to_owned());
    }
    for entry in &group.rules {
        match entry {
            Entry::Group(inner) => check_group(inner, seen)?,
            Entry::Leaf(rule) => {
                *seen += 1;
                rule.check().map_err(|why| format!("Rule {seen}: {why}"))?;
            }
        }
    }
    Ok(())
}

/// The names each tag counts as, so that a song with "Cool" under "Jazz" is
/// a song tagged "Jazz" too.
pub fn tag_names(tags: &[Tag]) -> TagNames {
    let by_id: HashMap<&str, &Tag> = tags.iter().map(|tag| (tag.id.as_str(), tag)).collect();
    tags.iter()
        .map(|tag| {
            let mut names = Vec::new();
            let mut at = Some(tag);
            // A tag under itself would loop; no tree is deeper than the list.
            for _ in 0..tags.len() {
                let Some(tag) = at else { break };
                names.push(fold(&tag.name));
                at = tag
                    .parent_id
                    .as_deref()
                    .and_then(|id| by_id.get(id).copied());
            }
            (tag.id.clone(), names)
        })
        .collect()
}

/// Whether the song is one the rules want, not looking at the limit.
pub fn matches(song: &Child, rules: &SmartRules, now: &Now) -> bool {
    rules
        .root
        .passes(&|_, rule| Some(rule_passes(song, rule, now)))
}

/// The songs the rules give: those it matches, in its order, the first
/// `limit`. A random order is the same for the same `seed`, which the
/// server takes from the hour so that a list doesn't reshuffle each time.
pub fn select<'a>(songs: &'a [Child], rules: &SmartRules, now: &Now, seed: u64) -> Vec<&'a Child> {
    let mut found: Vec<&Child> = (songs.iter())
        .filter(|song| matches(song, rules, now))
        .collect();
    found.sort_by(|a, b| in_order(a, b, rules.sort, seed));
    found.truncate(rules.limit.unwrap_or(DEFAULT_LIMIT) as usize);
    found
}

fn rule_passes(song: &Child, rule: &Rule, now: &Now) -> bool {
    passes(
        &rule.field.value_of(song, now),
        rule.operator,
        &rule.value,
        now.today,
    )
}

/// The values a song has in a text field, none for a song that has none.
fn texts_of(song: &Child, field: Field, now: &Now) -> Vec<String> {
    let names = |people: &[opensubsonic::ArtistID3], shown: Option<&String>| -> Vec<String> {
        match people.is_empty() {
            true => shown.cloned().into_iter().collect(),
            false => people.iter().map(|person| person.name.clone()).collect(),
        }
    };
    let mut values: Vec<String> = match field {
        Field::Title => vec![song.title.clone()],
        Field::Artist => names(
            &song.artists,
            song.display_artist.as_ref().or(song.artist.as_ref()),
        ),
        Field::AlbumArtist => names(&song.album_artists, song.display_album_artist.as_ref()),
        Field::Album => song.album.iter().cloned().collect(),
        Field::Genre => match song.genres.is_empty() {
            true => song.genre.iter().cloned().collect(),
            false => song.genres.iter().map(|genre| genre.name.clone()).collect(),
        },
        Field::Composer => {
            let composers: Vec<String> = (song.contributors.iter())
                .filter(|contributor| contributor.role.eq_ignore_ascii_case("composer"))
                .map(|contributor| contributor.artist.name.clone())
                .collect();
            match composers.is_empty() {
                true => song.display_composer.iter().cloned().collect(),
                false => composers,
            }
        }
        Field::Mood => song.moods.clone(),
        Field::Tag => (song.tag_ids.iter())
            .filter_map(|id| now.tags.get(id))
            .flatten()
            .cloned()
            .collect(),
        _ => song.suffix.iter().cloned().collect(),
    };
    values.retain(|value| !value.trim().is_empty());
    values
}

/// A song's number in a field; none where it has none. A song that has not
/// been played or rated has a zero, as the server counts it, and a zero
/// year, rate or depth means the file doesn't say.
fn number_of(song: &Child, field: Field) -> Option<i64> {
    let said = |number: Option<i64>| number.filter(|&number| number > 0);
    match field {
        Field::Year => said(song.year),
        Field::PlayCount => Some(song.play_count.unwrap_or(0)),
        Field::Rating => Some(song.user_rating.unwrap_or(0)),
        Field::Duration => song.duration,
        Field::Bpm => said(song.bpm),
        Field::BitRate => song.bit_rate,
        Field::BitDepth => said(song.bit_depth),
        _ => said(song.sampling_rate),
    }
}

/// The day a song was added or last played, as `day_number` counts.
fn date_of(song: &Child, field: Field) -> Option<i64> {
    let date = match field {
        Field::Added => &song.created,
        _ => &song.played,
    };
    date.as_deref().and_then(day_number)
}

/// What songs are sorted by: a number or the words folded, so a song
/// without one can go last whichever way the order runs.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum Key {
    Number(i64),
    Text(String),
}

fn key_of(song: &Child, by: SortKey) -> Option<Key> {
    let text = |value: Option<&String>| value.map(|value| Key::Text(fold(value)));
    match by {
        SortKey::Random => None,
        SortKey::Title => Some(Key::Text(fold(&song.title))),
        SortKey::Artist => text(song.display_artist.as_ref().or(song.artist.as_ref())),
        SortKey::Album => text(song.album.as_ref()),
        SortKey::Year => song.year.map(Key::Number),
        SortKey::PlayCount => Some(Key::Number(song.play_count.unwrap_or(0))),
        SortKey::LastPlayed => song.played.clone().map(Key::Text),
        SortKey::Added => song.created.clone().map(Key::Text),
        SortKey::Rating => Some(Key::Number(song.user_rating.unwrap_or(0))),
        SortKey::Duration => song.duration.map(Key::Number),
    }
}

fn in_order(a: &Child, b: &Child, sort: Sort, seed: u64) -> Ordering {
    let by_key = match sort.by {
        SortKey::Random => shuffled(seed, a).cmp(&shuffled(seed, b)),
        by => match (key_of(a, by), key_of(b, by)) {
            (Some(a), Some(b)) if sort.descending => b.cmp(&a),
            (Some(a), Some(b)) => a.cmp(&b),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        },
    };
    by_key.then_with(|| {
        (a.album.cmp(&b.album))
            .then(a.disc_number.cmp(&b.disc_number))
            .then(a.track.cmp(&b.track))
            .then(a.id.cmp(&b.id))
    })
}

/// Where a song falls in a shuffle, the same for the same seed.
fn shuffled(seed: u64, song: &Child) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    (seed, &song.id).hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests;
