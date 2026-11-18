//! What the overview pages' Focus panel narrows by: the values of an item's
//! facets (its genres, its decade, its artists...) the user picked, and how
//! many items each value has under the picks of the other facets. The
//! picks of one facet are alternatives, or all required where it is
//! switched to that; the facets all are required.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// The most played steps, by how many items each takes, and their names.
const TOP_STEPS: [usize; 6] = [25, 50, 100, 250, 500, 1000];
pub const FIRST_TOP_STEP: usize = TOP_STEPS[0];
const TOP_LABELS: [&str; 6] = [
    "Top 25", "Top 50", "Top 100", "Top 250", "Top 500", "Top 1000",
];
/// The recently added steps, by how many days back each reaches, and their
/// names.
const ADDED_DAYS: [i64; 3] = [7, 31, 365];
const ADDED_LABELS: [&str; 3] = ["Last week", "Last month", "Last year"];
const SECONDS_PER_DAY: i64 = 24 * 60 * 60;

/// Where a facet gets an item's values.
pub enum Kind<T> {
    /// The values the item has, such as its genres.
    Values(fn(&T) -> Vec<String>),
    /// How often the item was played: it has the value of each step of the
    /// most played that its count reaches.
    Top(fn(&T) -> i64),
    /// The day the item was added, as `day_number` counts: it has the value
    /// of each step of the recently added that it falls in.
    Added(fn(&T) -> Option<i64>),
}

/// How a facet lists its values.
pub enum Order {
    /// The most items first, ties by name.
    Frequency,
    /// By name, the last first, which puts the newest decade first.
    LabelDescending,
    /// In the order of these names.
    Listed(&'static [&'static str]),
}

/// A way to tell an overview page's items apart, as `Column` is a way to
/// show them.
pub struct Facet<T> {
    pub title: &'static str,
    pub kind: Kind<T>,
    pub order: Order,
    /// Whether the user may switch it to want all of the values picked, not
    /// any of them.
    pub all_of_offered: bool,
}

pub const fn facet<T>(title: &'static str, values: fn(&T) -> Vec<String>) -> Facet<T> {
    Facet {
        title,
        kind: Kind::Values(values),
        order: Order::Frequency,
        all_of_offered: false,
    }
}

/// A facet whose values an item has more of, and each of which the user may
/// want all of.
pub const fn linkable<T>(title: &'static str, values: fn(&T) -> Vec<String>) -> Facet<T> {
    Facet {
        all_of_offered: true,
        ..facet(title, values)
    }
}

/// A facet of values that sort by name, the last first, such as decades.
pub const fn descending<T>(title: &'static str, values: fn(&T) -> Vec<String>) -> Facet<T> {
    Facet {
        order: Order::LabelDescending,
        ..facet(title, values)
    }
}

pub const fn most_played<T>(title: &'static str, plays: fn(&T) -> i64) -> Facet<T> {
    Facet {
        title,
        kind: Kind::Top(plays),
        order: Order::Listed(&TOP_LABELS),
        all_of_offered: false,
    }
}

pub const fn recently_added<T>(title: &'static str, day: fn(&T) -> Option<i64>) -> Facet<T> {
    Facet {
        title,
        kind: Kind::Added(day),
        order: Order::Listed(&ADDED_LABELS),
        all_of_offered: false,
    }
}

/// The values picked in one facet, and whether all of them are wanted.
#[derive(Debug, Default, Clone, PartialEq)]
struct Pick {
    values: BTreeSet<String>,
    all_of: bool,
}

impl Pick {
    /// Whether an item with `values` is one the pick wants; an empty pick
    /// wants every item.
    fn admits(&self, values: &[String]) -> bool {
        let has = |wanted: &String| values.contains(wanted);
        match self.all_of {
            true => self.values.iter().all(has),
            false => self.values.is_empty() || self.values.iter().any(has),
        }
    }
}

/// What is picked on a page: values by facet title, and whether only the
/// favourites are wanted.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Focus {
    picks: BTreeMap<String, Pick>,
    pub favourites: bool,
}

impl Focus {
    /// Picks `value` of `facet`, or lets it go where it is picked.
    pub fn toggle(&mut self, facet: &str, value: &str) {
        let values = &mut self.picks.entry(facet.to_owned()).or_default().values;
        if !values.remove(value) {
            values.insert(value.to_owned());
        }
    }

    /// Switches `facet` between wanting any of its picked values and all.
    pub fn toggle_all_of(&mut self, facet: &str) {
        let pick = self.picks.entry(facet.to_owned()).or_default();
        pick.all_of = !pick.all_of;
    }

    pub fn is_picked(&self, facet: &str, value: &str) -> bool {
        (self.picks.get(facet)).is_some_and(|pick| pick.values.contains(value))
    }

    pub fn is_all_of(&self, facet: &str) -> bool {
        self.picks.get(facet).is_some_and(|pick| pick.all_of)
    }

    /// The values picked, as the facet and the value, facet by facet.
    pub fn picked(&self) -> Vec<(&str, &str)> {
        (self.picks.iter())
            .flat_map(|(facet, pick)| {
                pick.values
                    .iter()
                    .map(|value| (facet.as_str(), value.as_str()))
            })
            .collect()
    }

    /// Whether anything narrows the page.
    pub fn is_narrowing(&self) -> bool {
        self.favourites || self.picks.values().any(|pick| !pick.values.is_empty())
    }

    /// Lets every pick go and wants all items, not just the favourites. The
    /// facets keep their any-of or all-of.
    pub fn clear(&mut self) {
        self.favourites = false;
        for pick in self.picks.values_mut() {
            pick.values.clear();
        }
    }
}

/// A value of a facet and how many items have it.
#[derive(Debug, Clone, PartialEq)]
pub struct Count {
    pub label: String,
    pub count: usize,
}

/// The values of a facet in the order it lists them.
#[derive(Debug, Clone, PartialEq)]
pub struct Counted {
    pub title: &'static str,
    pub all_of_offered: bool,
    pub values: Vec<Count>,
}

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

/// The decade a year is in, as "1970s".
pub fn decade(year: i64) -> Option<String> {
    (year > 0).then(|| format!("{}s", year / 10 * 10))
}

/// The values each item has in `facet`, in the items' order.
fn values_of<T>(items: &[T], facet: &Facet<T>, today: i64) -> Vec<Vec<String>> {
    match &facet.kind {
        Kind::Values(values) => items.iter().map(|item| unique(values(item))).collect(),
        Kind::Top(plays) => top_values(&items.iter().map(plays).collect::<Vec<_>>()),
        Kind::Added(day) => items
            .iter()
            .map(|item| added_values(day(item).map(|day| today - day)))
            .collect(),
    }
}

/// A facet's values once each, without the blank ones some servers send.
fn unique(mut values: Vec<String>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    values.retain(|value| !value.trim().is_empty() && seen.insert(value.clone()));
    values
}

/// The steps of the most played each count falls in. A step as large as the
/// items are, or larger, holds them all and narrows nothing, so none is offered.
fn top_values(plays: &[i64]) -> Vec<Vec<String>> {
    let mut played: Vec<i64> = plays.iter().copied().filter(|&count| count > 0).collect();
    played.sort_unstable_by_key(|&count| Reverse(count));
    let offered = (TOP_STEPS.iter())
        .take_while(|&&step| step < plays.len())
        .count();
    let reached = |count: i64| {
        (0..offered)
            .filter(|&step| count > 0 && count >= played[TOP_STEPS[step].min(played.len()) - 1])
            .map(|step| TOP_LABELS[step].to_owned())
            .collect()
    };
    match played.is_empty() {
        true => vec![Vec::new(); plays.len()],
        false => plays.iter().map(|&count| reached(count)).collect(),
    }
}

/// The steps of the recently added an item `age` days old falls in.
fn added_values(age: Option<i64>) -> Vec<String> {
    let Some(age) = age else {
        return Vec::new();
    };
    (ADDED_DAYS.iter().zip(ADDED_LABELS))
        .filter(|&(&days, _)| age <= days)
        .map(|(_, label)| label.to_owned())
        .collect()
}

/// Whether each item is one the picks and the heart want.
pub fn matching<T>(
    items: &[T],
    facets: &[Facet<T>],
    focus: &Focus,
    starred: Option<fn(&T) -> bool>,
    today: i64,
) -> Vec<bool> {
    let picked: Vec<(&Facet<T>, &Pick)> = (facets.iter())
        .filter_map(|facet| Some((facet, focus.picks.get(facet.title)?)))
        .filter(|(_, pick)| !pick.values.is_empty())
        .collect();
    let columns: Vec<Vec<Vec<String>>> = (picked.iter())
        .map(|(facet, _)| values_of(items, facet, today))
        .collect();
    let wanted_by_heart =
        |item: &T| !focus.favourites || starred.is_some_and(|starred| starred(item));
    (0..items.len())
        .map(|i| {
            wanted_by_heart(&items[i])
                && (picked.iter().zip(&columns)).all(|((_, pick), column)| pick.admits(&column[i]))
        })
        .collect()
}

/// For each facet that has values, those its items have and how many items
/// have each, under the picks of the other facets and the heart; a value no
/// item has under them stays listed, with none. One pass over the items:
/// each counts for every facet when it passes every pick, and for the one
/// facet it fails alone.
// ponytail: the values of every item and facet are held at once; count a facet at a time if libraries outgrow it.
pub fn count<T>(
    items: &[T],
    facets: &[Facet<T>],
    focus: &Focus,
    starred: Option<fn(&T) -> bool>,
    today: i64,
) -> Vec<Counted> {
    let columns: Vec<Vec<Vec<String>>> = (facets.iter())
        .map(|facet| values_of(items, facet, today))
        .collect();
    let picks: Vec<Option<&Pick>> = (facets.iter())
        .map(|facet| focus.picks.get(facet.title))
        .collect();
    let mut tallies: Vec<HashMap<&str, usize>> = vec![HashMap::new(); facets.len()];
    for (i, item) in items.iter().enumerate() {
        for (tally, column) in tallies.iter_mut().zip(&columns) {
            for value in &column[i] {
                tally.entry(value).or_default();
            }
        }
        if focus.favourites && !starred.is_some_and(|starred| starred(item)) {
            continue;
        }
        let failed: Vec<usize> = (0..facets.len())
            .filter(|&f| picks[f].is_some_and(|pick| !pick.admits(&columns[f][i])))
            .collect();
        let counts_for = match failed.as_slice() {
            [] => (0..facets.len()).collect(),
            &[only] => vec![only],
            _ => Vec::new(),
        };
        for f in counts_for {
            for value in &columns[f][i] {
                *tallies[f].entry(value).or_default() += 1;
            }
        }
    }
    (facets.iter().zip(tallies))
        .filter(|(_, tally)| !tally.is_empty())
        .map(|(facet, tally)| Counted {
            title: facet.title,
            all_of_offered: facet.all_of_offered,
            values: ordered(&facet.order, tally),
        })
        .collect()
}

fn ordered(order: &Order, tally: HashMap<&str, usize>) -> Vec<Count> {
    let mut values: Vec<Count> = (tally.into_iter())
        .map(|(label, count)| Count {
            label: label.to_owned(),
            count,
        })
        .collect();
    match order {
        Order::Frequency => {
            values.sort_by(|a, b| b.count.cmp(&a.count).then(a.label.cmp(&b.label)))
        }
        Order::LabelDescending => values.sort_by(|a, b| b.label.cmp(&a.label)),
        Order::Listed(names) => {
            values.sort_by_key(|value| names.iter().position(|name| *name == value.label))
        }
    }
    values
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Album {
        genres: &'static [&'static str],
        year: i64,
        plays: i64,
        loved: bool,
        age: Option<i64>,
    }

    fn album(genres: &'static [&'static str], year: i64) -> Album {
        Album {
            genres,
            year,
            plays: 0,
            loved: false,
            age: None,
        }
    }

    fn genres(album: &Album) -> Vec<String> {
        album
            .genres
            .iter()
            .map(|genre| (*genre).to_owned())
            .collect()
    }

    fn decades(album: &Album) -> Vec<String> {
        decade(album.year).into_iter().collect()
    }

    const GENRES: Facet<Album> = linkable("Genres", genres);
    const DECADES: Facet<Album> = descending("Release date", decades);

    fn counts(items: &[Album], focus: &Focus) -> Vec<Counted> {
        count(items, &[GENRES, DECADES], focus, Some(|a| a.loved), 0)
    }

    fn of<'a>(counted: &'a [Counted], title: &str) -> Vec<(&'a str, usize)> {
        let facet = counted.iter().find(|facet| facet.title == title).unwrap();
        facet
            .values
            .iter()
            .map(|v| (v.label.as_str(), v.count))
            .collect()
    }

    fn library() -> Vec<Album> {
        vec![
            album(&["Blues"], 1971),
            album(&["Blues", "Rock"], 1975),
            album(&["Rock"], 1983),
            album(&["Jazz"], 1959),
        ]
    }

    #[test]
    fn an_album_with_several_genres_counts_once_in_each() {
        let counted = counts(&library(), &Focus::default());
        assert_eq!(
            of(&counted, "Genres"),
            [("Blues", 2), ("Rock", 2), ("Jazz", 1)]
        );
    }

    #[test]
    fn years_count_in_their_decades_the_newest_first() {
        let counted = counts(&library(), &Focus::default());
        assert_eq!(
            of(&counted, "Release date"),
            [("1980s", 1), ("1970s", 2), ("1950s", 1)]
        );
    }

    #[test]
    fn a_year_without_a_decade_has_none() {
        assert_eq!(decade(0), None);
        assert_eq!(decade(1999).as_deref(), Some("1990s"));
    }

    #[test]
    fn the_other_facets_count_only_what_a_pick_leaves() {
        let mut focus = Focus::default();
        focus.toggle("Genres", "Blues");
        let counted = counts(&library(), &focus);
        assert_eq!(
            of(&counted, "Release date"),
            [("1980s", 0), ("1970s", 2), ("1950s", 0)]
        );
    }

    #[test]
    fn a_facet_counts_under_the_other_picks_not_its_own() {
        let mut focus = Focus::default();
        focus.toggle("Genres", "Blues");
        let counted = counts(&library(), &focus);
        assert_eq!(
            of(&counted, "Genres"),
            [("Blues", 2), ("Rock", 2), ("Jazz", 1)]
        );
    }

    #[test]
    fn the_picks_of_one_facet_are_alternatives() {
        let mut focus = Focus::default();
        focus.toggle("Genres", "Blues");
        focus.toggle("Genres", "Jazz");
        let wanted = matching(&library(), &[GENRES, DECADES], &focus, None, 0);
        assert_eq!(wanted, [true, true, false, true]);
    }

    #[test]
    fn a_facet_switched_to_all_of_wants_every_pick() {
        let mut focus = Focus::default();
        focus.toggle("Genres", "Blues");
        focus.toggle("Genres", "Rock");
        focus.toggle_all_of("Genres");
        let wanted = matching(&library(), &[GENRES, DECADES], &focus, None, 0);
        assert_eq!(wanted, [false, true, false, false]);
    }

    #[test]
    fn the_facets_are_all_required() {
        let mut focus = Focus::default();
        focus.toggle("Genres", "Rock");
        focus.toggle("Release date", "1980s");
        let wanted = matching(&library(), &[GENRES, DECADES], &focus, None, 0);
        assert_eq!(wanted, [false, false, true, false]);
        let counted = counts(&library(), &focus);
        assert_eq!(
            of(&counted, "Genres"),
            [("Rock", 1), ("Blues", 0), ("Jazz", 0)]
        );
    }

    #[test]
    fn the_heart_wants_the_favourites_and_narrows_the_counts() {
        let mut items = library();
        items[3].loved = true;
        let focus = Focus {
            favourites: true,
            ..Focus::default()
        };
        let wanted = matching(&items, &[GENRES], &focus, Some(|a| a.loved), 0);
        assert_eq!(wanted, [false, false, false, true]);
        let counted = counts(&items, &focus);
        assert_eq!(
            of(&counted, "Genres"),
            [("Jazz", 1), ("Blues", 0), ("Rock", 0)]
        );
    }

    #[test]
    fn a_facet_no_item_has_a_value_in_is_not_offered() {
        let items = [album(&[], 0)];
        assert!(counts(&items, &Focus::default()).is_empty());
    }

    #[test]
    fn clearing_lets_the_picks_and_the_heart_go_and_keeps_all_of() {
        let mut focus = Focus::default();
        focus.toggle("Genres", "Blues");
        focus.toggle_all_of("Genres");
        focus.favourites = true;
        focus.clear();
        assert!(!focus.is_narrowing());
        assert!(focus.is_all_of("Genres"));
    }

    #[test]
    fn a_pick_is_dropped_by_picking_it_again() {
        let mut focus = Focus::default();
        focus.toggle("Genres", "Blues");
        assert!(focus.is_picked("Genres", "Blues"));
        focus.toggle("Genres", "Blues");
        assert!(!focus.is_narrowing());
    }

    fn played(plays: &[i64]) -> Vec<Album> {
        plays
            .iter()
            .map(|&plays| Album {
                plays,
                ..album(&[], 2000)
            })
            .collect()
    }

    #[test]
    fn the_most_played_steps_take_the_items_whose_count_reaches_them() {
        let plays: Vec<i64> = (1..=60).collect();
        let items = played(&plays);
        let top = [most_played("Most played", |a: &Album| a.plays)];
        let counted = count(&items, &top, &Focus::default(), None, 0);
        assert_eq!(
            of(&counted, "Most played"),
            [("Top 25", 25), ("Top 50", 50)]
        );
    }

    #[test]
    fn a_step_that_holds_every_item_is_not_offered() {
        let items = played(&[9, 5, 5, 5, 1]);
        let top = [most_played("Most played", |a: &Album| a.plays)];
        assert!(count(&items, &top, &Focus::default(), None, 0).is_empty());
    }

    #[test]
    fn a_step_larger_than_the_played_items_holds_them_all() {
        let mut plays = vec![0; 80];
        plays[..3].copy_from_slice(&[9, 5, 1]);
        let items = played(&plays);
        let top = [most_played("Most played", |a: &Album| a.plays)];
        let counted = count(&items, &top, &Focus::default(), None, 0);
        assert_eq!(of(&counted, "Most played"), [("Top 25", 3), ("Top 50", 3)]);
    }

    #[test]
    fn a_blank_value_is_not_a_value() {
        let items = [album(&["", " ", "Blues"], 2000)];
        let counted = count(&items, &[GENRES], &Focus::default(), None, 0);
        assert_eq!(of(&counted, "Genres"), [("Blues", 1)]);
    }

    #[test]
    fn unplayed_items_are_in_no_step_and_none_are_offered_without_plays() {
        let items = played(&[0, 0]);
        let top = [most_played("Most played", |a: &Album| a.plays)];
        assert!(count(&items, &top, &Focus::default(), None, 0).is_empty());
    }

    #[test]
    fn the_recently_added_steps_nest() {
        let ages = [Some(3), Some(20), Some(200), Some(900), None];
        let items: Vec<Album> = ages
            .iter()
            .map(|&age| Album {
                age,
                ..album(&[], 2000)
            })
            .collect();
        let added = [recently_added("Recently added", |a: &Album| {
            a.age.map(|age| 1000 - age)
        })];
        let counted = count(&items, &added, &Focus::default(), None, 1000);
        assert_eq!(
            of(&counted, "Recently added"),
            [("Last week", 1), ("Last month", 2), ("Last year", 3)]
        );
    }

    #[test]
    fn a_date_counts_days_from_the_epoch() {
        assert_eq!(day_number("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(day_number("2000-03-01"), Some(11_017));
        assert_eq!(day_number("not a date"), None);
        assert_eq!(today(2 * 86_400 + 5), 2);
    }
}
