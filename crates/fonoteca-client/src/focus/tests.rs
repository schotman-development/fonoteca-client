use super::words::{describe, hint, read, typed};
use super::*;
use crate::rules::{TagNames, day_number};

struct Item {
    genres: &'static [&'static str],
    year: Option<i64>,
    plays: i64,
    added: Option<&'static str>,
    loved: bool,
}

const fn item(
    genres: &'static [&'static str],
    year: Option<i64>,
    plays: i64,
    added: Option<&'static str>,
    loved: bool,
) -> Item {
    Item {
        genres,
        year,
        plays,
        added,
        loved,
    }
}

const FIELDS: &[Field<Item>] = &[
    values("genre", "Genres", "Music", |item, _| {
        FieldValue::Texts(item.genres.iter().map(|genre| genre.to_string()).collect())
    }),
    number("year", "Year", "Music", Measure::Count, |item, _| {
        FieldValue::Number(item.year)
    }),
    number("plays", "Plays", "Plays", Measure::Plays, |item, _| {
        FieldValue::Number(Some(item.plays))
    }),
    date("added", "Added", "Plays", |item, _| {
        FieldValue::Day(item.added.and_then(day_number))
    }),
    flag(FAVOURITE, "Favourite", "Plays", |item, _| {
        FieldValue::Flag(item.loved)
    }),
    text("genreText", "Genre text", "Music", |item, _| {
        FieldValue::Texts(item.genres.iter().map(|genre| genre.to_string()).collect())
    }),
];

const ITEMS: &[Item] = &[
    item(&["Blues"], Some(1965), 10, Some("2026-03-30"), false),
    item(&["Blues", "Soul"], Some(1972), 5, Some("2025-01-01"), true),
    item(&["Jazz"], Some(1972), 0, None, true),
    item(&["Soul"], None, 3, Some("2026-03-01"), false),
];

fn now() -> Now<'static> {
    static TAGS: std::sync::OnceLock<TagNames> = std::sync::OnceLock::new();
    Now {
        today: day_number("2026-03-31").unwrap(),
        tags: TAGS.get_or_init(TagNames::new),
    }
}

fn condition(field: &str, operator: Operator, value: Value) -> Condition {
    Condition {
        field: field.to_owned(),
        operator,
        value: Some(value),
        enabled: true,
        typed: None,
    }
}

fn picks(field: &str, operator: Operator, values: &[&str]) -> Condition {
    let listed = values.iter().map(|value| value.to_string()).collect();
    condition(field, operator, Value::List(listed))
}

fn focus_of(match_all: bool, conditions: Vec<Condition>) -> Focus {
    Focus {
        filters: Group::of(match_all, conditions),
    }
}

fn matched(focus: &Focus) -> Vec<usize> {
    let passed = matching(ITEMS, FIELDS, focus, &now());
    (0..ITEMS.len()).filter(|&i| passed[i]).collect()
}

fn infos_of_items() -> Vec<Info> {
    infos(FIELDS)
}

#[test]
fn a_new_filter_wants_everything_until_it_has_a_value() {
    let focus = Focus::of(
        &["genre", "year", "plays", "added", FAVOURITE],
        &infos_of_items(),
    );
    assert_eq!(focus.filters.leaf_count(), 5);
    assert!(!focus.is_narrowing());
    assert_eq!(matched(&focus), [0, 1, 2, 3]);
}

#[test]
fn each_kind_narrows_by_its_value() {
    let by = |condition| matched(&focus_of(true, vec![condition]));
    assert_eq!(by(picks("genre", Operator::AnyOf, &["Soul"])), [1, 3]);
    assert_eq!(by(picks("genre", Operator::AllOf, &["Blues", "Soul"])), [1]);
    assert_eq!(by(picks("genre", Operator::NoneOf, &["Blues"])), [2, 3]);
    assert_eq!(
        by(condition(
            "year",
            Operator::Between,
            Value::Range([1970, 1979])
        )),
        [1, 2]
    );
    assert_eq!(
        by(condition("plays", Operator::GreaterThan, Value::Number(4))),
        [0, 1]
    );
    assert_eq!(
        by(condition(
            "added",
            Operator::InTheLast,
            Value::Within(Span {
                amount: 31,
                unit: Unit::Days
            })
        )),
        [0, 3]
    );
    assert_eq!(
        by(condition(FAVOURITE, Operator::Is, Value::Flag(true))),
        [1, 2]
    );
    assert_eq!(
        by(condition(
            "genreText",
            Operator::Contains,
            Value::Text("bl".into())
        )),
        [0, 1]
    );
}

#[test]
fn a_missing_value_is_not_in_a_range_and_a_date_never_is_for_it() {
    let by = |condition| matched(&focus_of(true, vec![condition]));
    assert_eq!(
        by(condition(
            "year",
            Operator::Between,
            Value::Range([0, 3000])
        )),
        [0, 1, 2]
    );
    assert_eq!(
        by(condition("added", Operator::Never, Value::Flag(true))),
        [2]
    );
    assert_eq!(
        by(picks("genreText", Operator::IsEmpty, &[])),
        Vec::<usize>::new()
    );
}

#[test]
fn an_empty_text_a_list_of_nothing_and_a_switched_off_filter_say_nothing() {
    let blank = condition("genreText", Operator::Contains, Value::Text("  ".into()));
    let nothing = picks("genre", Operator::AnyOf, &[]);
    let mut off = condition("plays", Operator::GreaterThan, Value::Number(100));
    off.enabled = false;
    let focus = focus_of(true, vec![blank, nothing, off]);
    assert!(!focus.is_narrowing());
    assert_eq!(matched(&focus), [0, 1, 2, 3]);
}

#[test]
fn groups_of_any_and_all_combine_filters() {
    let blues = picks("genre", Operator::AnyOf, &["Blues"]);
    let loved = condition(FAVOURITE, Operator::Is, Value::Flag(true));
    let jazz = picks("genre", Operator::AnyOf, &["Jazz"]);
    // Blues and (loved or jazz)
    let mut focus = focus_of(true, vec![blues]);
    focus
        .filters
        .rules
        .push(Entry::Group(Group::of(false, vec![loved, jazz])));
    assert_eq!(matched(&focus), [1]);
    focus.toggle_match_all(&[]);
    assert_eq!(matched(&focus), [0, 1, 2]);
}

#[test]
fn a_value_counts_the_items_that_have_it_under_the_other_filters() {
    let focus = focus_of(
        true,
        vec![
            picks("genre", Operator::AnyOf, &["Blues"]),
            condition("year", Operator::Between, Value::Range([1970, 1979])),
        ],
    );
    let counted = count(ITEMS, FIELDS, &focus, &now());
    let Tally::Values(genres) = &counted[0] else {
        panic!("genres are counted as values");
    };
    // The genre filter itself is left out of its own counts, so Soul and Jazz
    // show how many items the year leaves with them.
    let of = |label: &str| {
        genres
            .iter()
            .find(|count| count.label == label)
            .map(|count| count.count)
    };
    assert_eq!(of("Blues"), Some(1));
    assert_eq!(of("Soul"), Some(1));
    assert_eq!(of("Jazz"), Some(1));
}

#[test]
fn a_value_no_item_has_under_the_others_stays_listed_with_none() {
    let focus = focus_of(
        true,
        vec![
            picks("genre", Operator::AnyOf, &[]),
            condition("plays", Operator::GreaterThan, Value::Number(100)),
        ],
    );
    let counted = count(ITEMS, FIELDS, &focus, &now());
    let Tally::Values(genres) = &counted[0] else {
        panic!("genres are counted as values");
    };
    assert_eq!(genres.len(), 3);
    assert!(genres.iter().all(|count| count.count == 0));
}

#[test]
fn values_come_most_frequent_first_or_by_name_backwards() {
    let focus = Focus::of(&["genre"], &infos_of_items());
    let Tally::Values(genres) = &count(ITEMS, FIELDS, &focus, &now())[0] else {
        panic!("values are counted");
    };
    let labels: Vec<_> = genres.iter().map(|count| count.label.as_str()).collect();
    assert_eq!(labels, ["Blues", "Soul", "Jazz"]);
    assert_eq!(
        ordered(
            Order::LabelDescending,
            HashMap::from([("1960s".into(), 1), ("1970s".into(), 2)])
        )
        .first()
        .unwrap()
        .label,
        "1970s"
    );
}

#[test]
fn a_number_says_how_many_it_leaves_of_what_the_others_leave_and_the_range() {
    let focus = focus_of(
        true,
        vec![
            picks("genre", Operator::AnyOf, &["Blues", "Soul"]),
            condition("year", Operator::Between, Value::Range([1970, 1979])),
        ],
    );
    let counted = count(ITEMS, FIELDS, &focus, &now());
    assert_eq!(
        counted[1],
        Tally::Range {
            left: 1,
            of: 3,
            bounds: Some((1965, 1972))
        }
    );
}

#[test]
fn a_date_counts_like_a_number_and_ignores_items_without_one_for_its_bounds() {
    let focus = focus_of(
        true,
        vec![condition(
            "added",
            Operator::InTheLast,
            Value::Within(Span {
                amount: 31,
                unit: Unit::Days,
            }),
        )],
    );
    let bounds = day_number("2025-01-01").zip(day_number("2026-03-30"));
    assert_eq!(
        count(ITEMS, FIELDS, &focus, &now())[0],
        Tally::Range {
            left: 2,
            of: 4,
            bounds
        }
    );
}

#[test]
fn a_filter_nothing_is_counted_for_has_no_tally() {
    let focus = focus_of(
        true,
        vec![condition(FAVOURITE, Operator::Is, Value::Flag(true))],
    );
    assert_eq!(count(ITEMS, FIELDS, &focus, &now()), [Tally::Nothing]);
    let text_focus = Focus::of(&["genreText"], &infos_of_items());
    assert_eq!(count(ITEMS, FIELDS, &text_focus, &now()), [Tally::Nothing]);
}

#[test]
fn a_filter_on_a_field_the_page_lacks_counts_nothing_and_narrows_nothing() {
    let focus = focus_of(true, vec![picks("gone", Operator::AnyOf, &["x"])]);
    assert_eq!(matched(&focus), [0, 1, 2, 3]);
    assert_eq!(count(ITEMS, FIELDS, &focus, &now()), [Tally::Nothing]);
}

#[test]
fn picking_toggles_a_value_in_and_out() {
    let mut focus = Focus::of(&["genre"], &infos_of_items());
    focus.toggle(&[0], "Soul");
    focus.toggle(&[0], "Blues");
    assert_eq!(
        focus.condition_mut(&[0]).unwrap().picked(),
        ["Blues", "Soul"]
    );
    focus.toggle(&[0], "Soul");
    assert_eq!(focus.condition_mut(&[0]).unwrap().picked(), ["Blues"]);
    assert!(focus.is_narrowing());
    focus.toggle(&[0], "Blues");
    assert!(!focus.is_narrowing());
}

#[test]
fn the_heart_is_a_favourite_filter_that_comes_and_goes() {
    let mut focus = Focus::default();
    assert!(!focus.favourites());
    focus.toggle_favourites();
    assert!(focus.favourites() && focus.is_narrowing());
    assert_eq!(matched(&focus), [1, 2]);
    focus.toggle_favourites();
    assert!(!focus.favourites());
    assert_eq!(focus.filters.leaf_count(), 0);
}

#[test]
fn clearing_lets_every_value_go_and_keeps_the_filters() {
    let mut focus = Focus::of(&["genre", "year"], &infos_of_items());
    focus.toggle(&[0], "Blues");
    focus.condition_mut(&[1]).unwrap().operator = Operator::GreaterThan;
    focus.condition_mut(&[1]).unwrap().value = Some(Value::Number(1970));
    focus.condition_mut(&[1]).unwrap().enabled = false;
    focus.clear();
    assert!(!focus.is_narrowing());
    assert_eq!(focus.filters.leaf_count(), 2);
    let year = focus.condition_mut(&[1]).unwrap();
    assert_eq!(
        (year.operator, year.value.clone()),
        (Operator::GreaterThan, None)
    );
    assert!(!year.enabled, "whether a filter is on is not a value");
}

#[test]
fn filters_and_groups_are_added_and_removed_by_their_places() {
    let infos = infos_of_items();
    let mut focus = Focus::of(&["genre"], &infos);
    focus.add_group(&[], false);
    focus.add(&[1], Condition::new(info_of(&infos, "year").unwrap()));
    assert_eq!(focus.filters.leaf_count(), 2);
    assert_eq!(
        focus
            .places()
            .iter()
            .map(|place| (place.path.clone(), place.is_group))
            .collect::<Vec<_>>(),
        [
            (vec![], true),
            (vec![0], false),
            (vec![1], true),
            (vec![1, 0], false)
        ]
    );
    assert!(
        focus
            .filters
            .group_at_mut(&[1])
            .is_some_and(|group| !group.match_all)
    );
    focus.toggle_match_all(&[1]);
    assert!(
        focus
            .filters
            .group_at_mut(&[1])
            .is_some_and(|group| group.match_all)
    );
    focus.remove(&[1]);
    assert_eq!(focus.filters.leaf_count(), 1);
    focus.remove(&[]);
    assert_eq!(focus.filters.leaf_count(), 1, "the focus itself stays");
}

#[test]
fn groups_nest_no_deeper_than_the_rules_wire_allows() {
    let mut focus = Focus::default();
    let mut path = Vec::new();
    for _ in 0..crate::rules::MOST_DEPTH + 2 {
        focus.add_group(&path, true);
        path.push(0);
    }
    assert_eq!(focus.filters.depth(), crate::rules::MOST_DEPTH);
}

#[test]
fn filters_on_unknown_fields_are_let_go() {
    let mut focus = focus_of(true, vec![picks("gone", Operator::AnyOf, &["x"])]);
    focus.filters.rules.push(Entry::Group(Group::of(
        true,
        vec![
            picks("genre", Operator::AnyOf, &["Blues"]),
            picks("gone", Operator::AnyOf, &["y"]),
        ],
    )));
    focus.retain_known(&infos_of_items());
    assert_eq!(focus.filters.leaf_count(), 1);
}

#[test]
fn the_steps_of_the_most_played_say_what_count_takes_an_item_in() {
    assert_eq!(top_labels(10), Vec::<&str>::new());
    assert_eq!(top_labels(60), ["Top 25", "Top 50"]);
    let plays: Vec<i64> = (1..=100).collect();
    assert_eq!(top_threshold(&plays, "Top 25"), Some(76));
    assert_eq!(top_threshold(&plays, "Top 1000"), Some(1));
    assert_eq!(top_threshold(&[0, 0], "Top 25"), None);
    assert_eq!(top_threshold(&plays, "Top 7"), None);
    assert_eq!(
        recent_steps()
            .iter()
            .map(|(label, _)| *label)
            .collect::<Vec<_>>(),
        ["Last week", "Last month", "Last year"]
    );
}

#[test]
fn a_decade_is_named_by_its_first_year() {
    assert_eq!(decade(1972).as_deref(), Some("1970s"));
    assert_eq!(decade(0), None);
}

#[test]
fn a_focus_round_trips_through_json_with_its_groups_and_off_switches() {
    let mut off = condition("plays", Operator::GreaterThan, Value::Number(3));
    off.enabled = false;
    off.typed = Some(["3".into(), String::new()]);
    let mut focus = focus_of(
        false,
        vec![picks("genre", Operator::AllOf, &["Blues", "Soul"]), off],
    );
    focus.filters.rules.push(Entry::Group(Group::of(
        true,
        vec![condition(
            "year",
            Operator::Between,
            Value::Range([1970, 1979]),
        )],
    )));
    let json = serde_json::to_string(&focus).unwrap();
    assert!(!json.contains("typed"));
    let read: Focus = serde_json::from_str(&json).unwrap();
    focus.condition_mut(&[1]).unwrap().typed = None;
    assert_eq!(read, focus);
    assert!(
        !serde_json::to_string(&condition("year", Operator::Is, Value::Number(1)))
            .unwrap()
            .contains("enabled")
    );
}

#[test]
fn what_is_typed_reads_as_a_value_where_it_can() {
    let number = Kind::Number(Measure::Count);
    let boxes = |first: &str, second: &str| [first.to_owned(), second.to_owned()];
    assert_eq!(
        read(number, Operator::Between, &boxes("1970", "1979")),
        Some(Value::Range([1970, 1979]))
    );
    assert_eq!(
        read(number, Operator::Between, &boxes("1979", "1970")),
        None
    );
    assert_eq!(
        read(number, Operator::Is, &boxes(" 5 ", "")),
        Some(Value::Number(5))
    );
    assert_eq!(read(number, Operator::Is, &boxes("x", "")), None);
    assert_eq!(
        read(
            Kind::Number(Measure::Duration),
            Operator::LessThan,
            &boxes("3:30", "")
        ),
        Some(Value::Number(210))
    );
    assert_eq!(
        read(Kind::Date, Operator::InTheLast, &boxes("90 days", "")),
        Some(Value::Within(Span {
            amount: 90,
            unit: Unit::Days
        }))
    );
    assert!(read(Kind::Date, Operator::Before, &boxes("2026-01-31", "")).is_some());
    assert_eq!(
        read(Kind::Date, Operator::Before, &boxes("2026-1-31", "")),
        None
    );
    assert_eq!(read(Kind::Text, Operator::Contains, &boxes("  ", "")), None);
    assert_eq!(read(Kind::Date, Operator::Never, &boxes("", "")), None);
}

#[test]
fn the_boxes_show_what_was_typed_else_the_value() {
    let mut year = condition("year", Operator::Between, Value::Range([1970, 1979]));
    assert_eq!(typed(Kind::Number(Measure::Count), &year), ["1970", "1979"]);
    year.typed = Some(["19".into(), String::new()]);
    assert_eq!(typed(Kind::Number(Measure::Count), &year), ["19", ""]);
    let span = condition(
        "added",
        Operator::InTheLast,
        Value::Within(Span {
            amount: 1,
            unit: Unit::Months,
        }),
    );
    assert_eq!(typed(Kind::Date, &span), ["1 month", ""]);
    assert_eq!(
        hint(Kind::Number(Measure::Duration), Operator::Between, false),
        "3:00"
    );
}

#[test]
fn a_filter_is_said_for_a_screen_reader() {
    let year = condition("year", Operator::Between, Value::Range([1970, 1979]));
    assert_eq!(
        describe("Year", Kind::Number(Measure::Count), &year),
        "Year, between 1970 and 1979"
    );
    let bitrate = condition("bitRate", Operator::GreaterThan, Value::Number(320));
    assert_eq!(
        describe(
            "Bit rate",
            Kind::Number(Measure::KilobitsPerSecond),
            &bitrate
        ),
        "Bit rate, more than 320 kbit/s"
    );
    let genres = picks("genre", Operator::NoneOf, &["Blues", "Soul"]);
    assert_eq!(
        describe("Genres", Kind::Values, &genres),
        "Genres, none of Blues, Soul"
    );
    let loved = condition(FAVOURITE, Operator::Is, Value::Flag(true));
    assert_eq!(describe("Favourite", Kind::Flag, &loved), "Favourite, yes");
    let added = condition("added", Operator::Never, Value::Flag(true));
    assert_eq!(describe("Added", Kind::Date, &added), "Added, never");
    let none = Condition {
        value: None,
        ..genres
    };
    assert_eq!(describe("Genres", Kind::Values, &none), "Genres, no picks");
}

#[test]
fn an_empty_group_of_any_narrows_the_page_by_itself() {
    let mut focus = Focus::of(&["genre"], &infos(FIELDS));
    assert!(!focus.is_narrowing());
    focus.add_group(&[], false);
    assert!(focus.is_narrowing());
}
