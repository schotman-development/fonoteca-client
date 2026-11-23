use super::*;

const TODAY: &str = "2026-03-31";

fn today() -> i64 {
    day_number(TODAY).unwrap()
}

fn texts(values: &[&str]) -> FieldValue {
    FieldValue::Texts(values.iter().map(|value| value.to_string()).collect())
}

fn text(value: &str) -> Option<Value> {
    Some(Value::Text(value.to_owned()))
}

fn list(values: &[&str]) -> Option<Value> {
    Some(Value::List(
        values.iter().map(|value| value.to_string()).collect(),
    ))
}

fn within(amount: u32, unit: Unit) -> Option<Value> {
    Some(Value::Within(Span { amount, unit }))
}

fn passes_as(
    matching: Matching,
    have: &FieldValue,
    operator: Operator,
    value: Option<Value>,
) -> bool {
    let tags = TagNames::new();
    let now = Now {
        today: today(),
        matching,
        tags: &tags,
    };
    passes(have, operator, &value, &now)
}

fn passes_today(have: &FieldValue, operator: Operator, value: Option<Value>) -> bool {
    passes_as(Matching::Device, have, operator, value)
}

fn on_server_at(time: &str) -> Matching {
    Matching::Server {
        seconds: moment_number(time).unwrap(),
    }
}

#[test]
fn a_server_matches_words_by_case_only_and_a_device_by_accents_too() {
    let have = texts(&["Beyoncé"]);
    let server = on_server_at("2026-03-31T12:00:00Z");
    for (word, on_server) in [("beyonce", false), ("BEYONCÉ", true), ("beyoncé", true)] {
        let wanted = || text(word);
        assert!(passes_as(Matching::Device, &have, Operator::Is, wanted()));
        assert_eq!(passes_as(server, &have, Operator::Is, wanted()), on_server);
        assert_eq!(
            passes_as(server, &have, Operator::StartsWith, wanted()),
            on_server
        );
    }
}

#[test]
fn a_server_counts_in_the_last_from_the_moment_and_a_device_by_days() {
    let noon = on_server_at("2026-03-31T12:00:00Z");
    let in_the_last_day = |matching, time| {
        passes_as(
            matching,
            &FieldValue::Moment(moment_number(time)),
            Operator::InTheLast,
            within(1, Unit::Days),
        )
    };
    assert!(in_the_last_day(noon, "2026-03-30T12:00:00Z"));
    assert!(!in_the_last_day(noon, "2026-03-30T11:59:59Z"));
    // A device's day starts at midnight, so the same song is still in.
    assert!(in_the_last_day(Matching::Device, "2026-03-30T11:59:59Z"));
    assert!(!in_the_last_day(Matching::Device, "2026-03-29T23:59:59Z"));
}

#[test]
fn a_server_counts_months_back_to_the_same_time_of_day() {
    let noon = on_server_at("2026-03-31T12:00:00Z");
    let in_a_month = |time| {
        passes_as(
            noon,
            &FieldValue::Moment(moment_number(time)),
            Operator::InTheLast,
            within(1, Unit::Months),
        )
    };
    // A month before the 31st of March is the 28th of February.
    assert!(in_a_month("2026-02-28T12:00:00Z"));
    assert!(!in_a_month("2026-02-28T11:00:00Z"));
}

#[test]
fn a_moment_that_is_missing_is_not_in_the_last_and_never() {
    let missing = FieldValue::Moment(None);
    let noon = on_server_at("2026-03-31T12:00:00Z");
    let span = || within(30, Unit::Days);
    assert!(!passes_as(noon, &missing, Operator::InTheLast, span()));
    assert!(passes_as(noon, &missing, Operator::NotInTheLast, span()));
    assert!(passes_as(noon, &missing, Operator::Never, None));
}

#[test]
fn a_moment_is_judged_by_its_day_for_before_and_after() {
    let noon = on_server_at("2026-03-31T12:00:00Z");
    let have = FieldValue::Moment(moment_number("2026-03-30T23:59:59Z"));
    assert!(passes_as(noon, &have, Operator::Before, text("2026-03-31")));
    assert!(!passes_as(noon, &have, Operator::After, text("2026-03-30")));
}

#[test]
fn moments_count_seconds_from_the_epoch() {
    assert_eq!(moment_number("1970-01-01T00:00:00Z"), Some(0));
    assert_eq!(
        moment_number("1970-01-02T01:02:03.456Z"),
        Some(86_400 + 3723)
    );
    assert_eq!(moment_number("1970-01-02"), Some(86_400));
    assert_eq!(moment_number("nonsense"), None);
}

#[test]
fn dates_count_days_from_the_epoch_and_back() {
    assert_eq!(day_number("1970-01-01"), Some(0));
    assert_eq!(day_number("1970-01-02T10:00:00Z"), Some(1));
    assert_eq!(day_number("not a date"), None);
    for date in [
        "1970-01-01",
        "2000-02-29",
        "2026-03-31",
        "2024-12-31",
        "1999-03-01",
    ] {
        let (year, month, day) = civil_date(day_number(date).unwrap());
        assert_eq!(format!("{year:04}-{month:02}-{day:02}"), date);
        assert_eq!(iso_date(day_number(date).unwrap()), date);
    }
}

#[test]
fn text_operators_fold_case_and_accents_and_look_at_every_value() {
    let have = texts(&["Café Blues", "Soul"]);
    assert!(passes_today(&have, Operator::Contains, text("cafe")));
    assert!(passes_today(&have, Operator::Is, text("SOUL")));
    assert!(!passes_today(&have, Operator::Is, text("Café")));
    assert!(passes_today(&have, Operator::IsNot, text("Rock")));
    assert!(!passes_today(&have, Operator::IsNot, text("soul")));
    assert!(passes_today(&have, Operator::StartsWith, text("caf")));
    assert!(!passes_today(&have, Operator::StartsWith, text("blues")));
    assert!(passes_today(&have, Operator::NotContains, text("rock")));
    assert!(!passes_today(&have, Operator::NotContains, text("oul")));
}

#[test]
fn missing_and_empty_text_values() {
    let none = texts(&[]);
    assert!(passes_today(&none, Operator::IsEmpty, None));
    assert!(!passes_today(&none, Operator::IsNotEmpty, None));
    assert!(!passes_today(&none, Operator::Contains, text("a")));
    assert!(passes_today(&none, Operator::IsNot, text("a")));
    assert!(passes_today(&texts(&["a"]), Operator::IsNotEmpty, None));
    // No text to compare with is a condition that can't say yes.
    assert!(!passes_today(&texts(&["a"]), Operator::Contains, None));
}

#[test]
fn any_all_and_none_of_compare_the_values_exactly() {
    let have = texts(&["Blues", " Soul "]);
    assert!(passes_today(
        &have,
        Operator::AnyOf,
        list(&["Rock", "Soul"])
    ));
    assert!(!passes_today(
        &have,
        Operator::AnyOf,
        list(&["Rock", "Jazz"])
    ));
    assert!(passes_today(
        &have,
        Operator::AllOf,
        list(&["Blues", "Soul"])
    ));
    assert!(!passes_today(
        &have,
        Operator::AllOf,
        list(&["Blues", "Rock"])
    ));
    assert!(passes_today(
        &have,
        Operator::NoneOf,
        list(&["Rock", "Jazz"])
    ));
    assert!(!passes_today(
        &have,
        Operator::NoneOf,
        list(&["Rock", "Blues"])
    ));
    assert!(
        !passes_today(&have, Operator::AnyOf, list(&["blues"])),
        "case counts"
    );
    assert!(passes_today(&texts(&[]), Operator::NoneOf, list(&["Rock"])));
    assert!(!passes_today(&texts(&[]), Operator::AnyOf, list(&["Rock"])));
}

#[test]
fn number_operators_and_the_ends_of_between() {
    let have = FieldValue::Number(Some(1975));
    let number = |at| Some(Value::Number(at));
    assert!(passes_today(&have, Operator::Is, number(1975)));
    assert!(passes_today(&have, Operator::IsNot, number(1976)));
    assert!(passes_today(&have, Operator::GreaterThan, number(1974)));
    assert!(!passes_today(&have, Operator::GreaterThan, number(1975)));
    assert!(passes_today(&have, Operator::LessThan, number(1976)));
    assert!(!passes_today(&have, Operator::LessThan, number(1975)));
    for ends in [[1975, 1980], [1970, 1975], [1975, 1975]] {
        assert!(passes_today(
            &have,
            Operator::Between,
            Some(Value::Range(ends))
        ));
    }
    assert!(!passes_today(
        &have,
        Operator::Between,
        Some(Value::Range([1976, 1980]))
    ));
    assert!(!passes_today(
        &have,
        Operator::Between,
        Some(Value::Range([1970, 1974]))
    ));
}

#[test]
fn an_item_without_a_number_passes_only_is_not() {
    let missing = FieldValue::Number(None);
    let number = |at| Some(Value::Number(at));
    assert!(passes_today(&missing, Operator::IsNot, number(3)));
    assert!(!passes_today(&missing, Operator::Is, number(3)));
    assert!(!passes_today(&missing, Operator::GreaterThan, number(-1)));
    assert!(!passes_today(&missing, Operator::LessThan, number(1000)));
    assert!(!passes_today(
        &missing,
        Operator::Between,
        Some(Value::Range([0, 1000]))
    ));
    assert!(!passes_today(
        &FieldValue::Number(Some(1)),
        Operator::Is,
        None
    ));
}

#[test]
fn date_operators_with_spans_text_and_ranges() {
    let day = |date| FieldValue::Day(day_number(date));
    let on = day("2026-03-01");
    assert!(passes_today(
        &on,
        Operator::InTheLast,
        within(31, Unit::Days)
    ));
    assert!(!passes_today(
        &on,
        Operator::InTheLast,
        within(7, Unit::Days)
    ));
    assert!(passes_today(
        &on,
        Operator::NotInTheLast,
        within(7, Unit::Days)
    ));
    assert!(passes_today(&on, Operator::Before, text("2026-03-02")));
    assert!(!passes_today(&on, Operator::Before, text("2026-03-01")));
    assert!(passes_today(&on, Operator::After, text("2026-02-28")));
    assert!(!passes_today(&on, Operator::After, text("2026-03-01")));
    let range = |from, to| {
        Some(Value::Range([
            day_number(from).unwrap(),
            day_number(to).unwrap(),
        ]))
    };
    assert!(passes_today(
        &on,
        Operator::Between,
        range("2026-03-01", "2026-03-01")
    ));
    assert!(passes_today(
        &on,
        Operator::Between,
        range("2026-02-01", "2026-03-01")
    ));
    assert!(!passes_today(
        &on,
        Operator::Between,
        range("2026-03-02", "2026-04-01")
    ));
    assert!(!passes_today(&on, Operator::Never, None));
}

#[test]
fn an_item_with_no_date_is_never_in_a_span_and_never_is_for_it() {
    let missing = FieldValue::Day(None);
    assert!(passes_today(&missing, Operator::Never, None));
    assert!(passes_today(
        &missing,
        Operator::NotInTheLast,
        within(1, Unit::Years)
    ));
    assert!(!passes_today(
        &missing,
        Operator::InTheLast,
        within(1, Unit::Years)
    ));
    assert!(!passes_today(
        &missing,
        Operator::Before,
        text("2999-01-01")
    ));
    assert!(!passes_today(&missing, Operator::After, text("1900-01-01")));
    assert!(!passes_today(
        &missing,
        Operator::Between,
        Some(Value::Range([0, 99999]))
    ));
}

#[test]
fn a_span_counts_back_by_the_calendar() {
    let first = |amount, unit| first_day_of(Span { amount, unit }, today());
    assert_eq!(first(0, Unit::Days), today());
    assert_eq!(first(10, Unit::Days), day_number("2026-03-21").unwrap());
    assert_eq!(first(1, Unit::Months), day_number("2026-02-28").unwrap());
    assert_eq!(first(1, Unit::Years), day_number("2025-03-31").unwrap());
}

#[test]
fn flags_are_yes_or_no() {
    let flag = |wanted| Some(Value::Flag(wanted));
    assert!(passes_today(
        &FieldValue::Flag(true),
        Operator::Is,
        flag(true)
    ));
    assert!(!passes_today(
        &FieldValue::Flag(true),
        Operator::Is,
        flag(false)
    ));
    assert!(passes_today(
        &FieldValue::Flag(false),
        Operator::Is,
        flag(false)
    ));
    assert!(!passes_today(&FieldValue::Flag(false), Operator::Is, None));
}

fn leaf(verdict: bool) -> Entry<bool> {
    Entry::Leaf(verdict)
}

fn group(match_all: bool, rules: Vec<Entry<bool>>) -> Group<bool> {
    Group { match_all, rules }
}

fn verdict(group: &Group<bool>) -> bool {
    group.passes(&|_, &verdict| Some(verdict))
}

#[test]
fn groups_of_all_and_any_nest() {
    assert!(verdict(&group(true, vec![leaf(true), leaf(true)])));
    assert!(!verdict(&group(true, vec![leaf(true), leaf(false)])));
    assert!(verdict(&group(false, vec![leaf(false), leaf(true)])));
    assert!(!verdict(&group(false, vec![leaf(false), leaf(false)])));
    let inner = |all, one, two| Entry::Group(group(all, vec![leaf(one), leaf(two)]));
    // a and (b or c)
    assert!(verdict(&group(
        true,
        vec![leaf(true), inner(false, false, true)]
    )));
    assert!(!verdict(&group(
        true,
        vec![leaf(false), inner(false, false, true)]
    )));
    assert!(!verdict(&group(
        true,
        vec![leaf(true), inner(false, false, false)]
    )));
    // a or (b and c)
    assert!(verdict(&group(
        false,
        vec![leaf(false), inner(true, true, true)]
    )));
    assert!(!verdict(&group(
        false,
        vec![leaf(false), inner(true, true, false)]
    )));
}

#[test]
fn conditions_that_do_not_count_are_left_out_of_the_verdict() {
    let skipped = group(true, vec![leaf(true), leaf(false)]);
    assert!(skipped.passes(&|at, &verdict| (at == 0).then_some(verdict)));
    let none_count = group(true, vec![leaf(false)]);
    assert!(
        none_count.passes(&|_, _| None),
        "a group of nothing wants everything"
    );
    let nested = group(
        true,
        vec![leaf(true), Entry::Group(group(false, vec![leaf(false)]))],
    );
    assert!(nested.passes(&|at, &verdict| (at == 0).then_some(verdict)));
}

#[test]
fn the_places_of_conditions_count_through_the_groups() {
    let tree = group(
        true,
        vec![
            leaf(true),
            Entry::Group(group(false, vec![leaf(false), leaf(true)])),
            leaf(false),
        ],
    );
    assert_eq!(tree.leaf_count(), 4);
    assert_eq!(tree.depth(), 2);
    assert!(!tree.is_flat());
    let seen = std::cell::RefCell::new(Vec::new());
    tree.passes(&|at, &verdict| {
        seen.borrow_mut().push((at, verdict));
        Some(verdict)
    });
    assert_eq!(
        *seen.borrow(),
        [(0, true), (1, false), (2, true), (3, false)]
    );
    assert_eq!(tree.leaves(), [&true, &false, &true, &false]);
}

#[test]
fn entries_are_found_and_taken_out_by_their_places() {
    let mut tree = group(
        true,
        vec![
            leaf(true),
            Entry::Group(group(false, vec![leaf(false), leaf(true)])),
        ],
    );
    assert_eq!(
        tree.group_at_mut(&[1]).map(|inner| inner.rules.len()),
        Some(2)
    );
    assert!(tree.group_at_mut(&[0]).is_none());
    assert!(tree.group_at_mut(&[]).is_some());
    assert!(matches!(
        tree.entry_at_mut(&[1, 0]),
        Some(Entry::Leaf(false))
    ));
    assert!(tree.remove_at(&[]).is_none());
    assert!(matches!(tree.remove_at(&[1, 0]), Some(Entry::Leaf(false))));
    assert_eq!(tree.leaf_count(), 2);
    assert!(tree.remove_at(&[5]).is_none());
}

#[test]
fn groups_round_trip_through_json_and_flat_ones_look_like_the_first_version() {
    let tree: Group<String> = Group {
        match_all: false,
        rules: vec![
            Entry::Leaf("a".to_owned()),
            Entry::Group(Group::of(true, vec!["b".to_owned()])),
        ],
    };
    let json = serde_json::to_value(&tree).unwrap();
    assert_eq!(
        json,
        serde_json::json!({"match": "any", "rules": ["a", {"match": "all", "rules": ["b"]}]})
    );
    assert_eq!(serde_json::from_value::<Group<String>>(json).unwrap(), tree);
    assert!(serde_json::from_str::<Group<String>>(r#"{"match":"some","rules":[]}"#).is_err());
}

#[test]
fn a_blank_text_is_no_value() {
    let blank = texts(&["", "  "]);
    assert!(passes_today(&blank, Operator::IsEmpty, None));
    assert!(!passes_today(&blank, Operator::IsNotEmpty, None));
    assert!(!passes_today(&blank, Operator::Contains, text("")));
}

#[test]
fn an_empty_group_of_any_matches_nothing_and_one_of_all_is_left_out() {
    let nothing = Entry::Group(group(false, vec![]));
    assert!(!verdict(&group(true, vec![leaf(true), nothing])));
    let vacuous = Entry::Group(group(true, vec![]));
    assert!(verdict(&group(true, vec![leaf(true), vacuous])));
    assert!(
        verdict(&group(false, vec![])),
        "the filters' own group is no filter"
    );
}
