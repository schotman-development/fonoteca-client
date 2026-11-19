use super::draft::Draft;
use super::from_focus::{from_focus, into_focus};
use super::words::{parse_rule, sort_index, summary};
use super::*;
use crate::focus::{Condition, Focus};
use crate::rules::{Span, Unit, civil_date};

fn song(json: &str) -> Child {
    let json = format!(r#"{{"isDir":false,{json}}}"#);
    serde_json::from_str(&json).unwrap()
}

fn text(field: Field, operator: Operator, value: &str) -> Rule {
    Rule {
        field,
        operator,
        value: Some(Value::Text(value.to_owned())),
    }
}

fn number(field: Field, operator: Operator, value: i64) -> Rule {
    Rule {
        field,
        operator,
        value: Some(Value::Number(value)),
    }
}

fn within(field: Field, operator: Operator, amount: u32, unit: Unit) -> Rule {
    Rule {
        field,
        operator,
        value: Some(Value::Within(Span { amount, unit })),
    }
}

fn flat(match_all: bool, rules: Vec<Rule>, sort: Sort, limit: Option<u32>) -> SmartRules {
    SmartRules {
        root: Group::of(match_all, rules),
        sort,
        limit,
    }
}

fn all_of(rules: Vec<Rule>) -> SmartRules {
    flat(true, rules, Sort::default(), None)
}

const TODAY: &str = "2026-03-31";

fn today() -> i64 {
    day_number(TODAY).unwrap()
}

/// Whether the song passes the one rule, on the 31st of March 2026, with
/// the tags "Jazz" and "Cool" under it.
fn passes(song: &Child, rule: Rule) -> bool {
    let tags = tag_names(&[tag("t1", "Jazz", None), tag("t2", "Cool", Some("t1"))]);
    matches(
        song,
        &all_of(vec![rule]),
        &Now {
            today: today(),
            tags: &tags,
        },
    )
}

fn tag(id: &str, name: &str, parent: Option<&str>) -> Tag {
    Tag {
        id: id.to_owned(),
        name: name.to_owned(),
        parent_id: parent.map(str::to_owned),
        song_count: 0,
    }
}

#[test]
fn text_operators_ignore_case_and_accents() {
    let song = song(r#""id":"1","title":"Café del Mar""#);
    let title = |operator, value| passes(&song, text(Field::Title, operator, value));
    assert!(title(Operator::Is, "CAFE DEL MAR"));
    assert!(!title(Operator::Is, "Café"));
    assert!(title(Operator::IsNot, "Café"));
    assert!(!title(Operator::IsNot, "cafe del mar"));
    assert!(title(Operator::Contains, "del"));
    assert!(!title(Operator::Contains, "dell"));
    assert!(title(Operator::NotContains, "dell"));
    assert!(!title(Operator::NotContains, "MAR"));
    assert!(title(Operator::StartsWith, "caf"));
    assert!(!title(Operator::StartsWith, "del"));
}

#[test]
fn text_fields_read_any_of_a_songs_values() {
    let song = song(
        r#""id":"1","title":"T","album":"Night","suffix":"flac","moods":["Calm","Warm"],
        "genres":[{"name":"Blues"},{"name":"Soul"}],
        "artists":[{"id":"a","name":"Mara Lind"},{"id":"b","name":"Joe"}],
        "albumArtists":[{"id":"c","name":"Various"}],
        "contributors":[{"role":"Composer","artist":{"id":"d","name":"Ludwig"}},
                        {"role":"producer","artist":{"id":"e","name":"Pat"}}]"#,
    );
    let is = |field, value| passes(&song, text(field, Operator::Is, value));
    assert!(is(Field::Genre, "soul") && is(Field::Genre, "Blues"));
    assert!(is(Field::Artist, "joe") && is(Field::Artist, "Mara Lind"));
    assert!(is(Field::AlbumArtist, "various") && !is(Field::AlbumArtist, "joe"));
    assert!(is(Field::Composer, "Ludwig") && !is(Field::Composer, "Pat"));
    assert!(is(Field::Mood, "warm") && is(Field::Album, "night") && is(Field::Format, "FLAC"));
    // A song with a genre among others is not "genre is not" it.
    assert!(!passes(&song, text(Field::Genre, Operator::IsNot, "Soul")));
}

#[test]
fn text_fields_fall_back_to_what_the_server_displays() {
    let song = song(
        r#""id":"1","title":"T","genre":"Folk","displayArtist":"Mara & Joe",
        "displayComposer":"Bach","displayAlbumArtist":"Mara""#,
    );
    let is = |field, value| passes(&song, text(field, Operator::Is, value));
    assert!(is(Field::Genre, "Folk") && is(Field::Artist, "Mara & Joe"));
    assert!(is(Field::Composer, "Bach") && is(Field::AlbumArtist, "Mara"));
}

#[test]
fn a_song_without_a_value_fails_all_but_the_negative_rules() {
    let bare = song(r#""id":"1","title":"T""#);
    for field in [
        Field::Genre,
        Field::Artist,
        Field::Album,
        Field::Composer,
        Field::Mood,
        Field::Tag,
        Field::Format,
    ] {
        for operator in [Operator::Is, Operator::Contains, Operator::StartsWith] {
            assert!(
                !passes(&bare, text(field, operator, "x")),
                "{field:?} {operator:?}"
            );
        }
        for operator in [Operator::IsNot, Operator::NotContains] {
            assert!(
                passes(&bare, text(field, operator, "x")),
                "{field:?} {operator:?}"
            );
        }
    }
    for field in [
        Field::Year,
        Field::Duration,
        Field::Bpm,
        Field::BitRate,
        Field::BitDepth,
        Field::SamplingRate,
    ] {
        for operator in [Operator::Is, Operator::GreaterThan, Operator::LessThan] {
            assert!(
                !passes(&bare, number(field, operator, 0)),
                "{field:?} {operator:?}"
            );
        }
        assert!(!passes(
            &bare,
            Rule {
                field,
                operator: Operator::Between,
                value: Some(Value::Range([0, 9999]))
            }
        ));
        assert!(
            passes(&bare, number(field, Operator::IsNot, 2000)),
            "{field:?}"
        );
    }
    assert!(!passes(
        &bare,
        within(Field::Added, Operator::InTheLast, 1, Unit::Years)
    ));
    assert!(passes(
        &bare,
        within(Field::Added, Operator::NotInTheLast, 1, Unit::Years)
    ));
    assert!(!passes(
        &bare,
        text(Field::Added, Operator::Before, "2030-01-01")
    ));
    assert!(!passes(
        &bare,
        text(Field::LastPlayed, Operator::After, "2000-01-01")
    ));
    assert!(passes(
        &bare,
        Rule {
            field: Field::LastPlayed,
            operator: Operator::Never,
            value: None
        }
    ));
}

#[test]
fn a_song_without_a_year_is_not_more_than_2000() {
    let without = song(r#""id":"1","title":"T""#);
    let zero = song(r#""id":"2","title":"T","year":0"#);
    let with = song(r#""id":"3","title":"T","year":2001"#);
    let over = || number(Field::Year, Operator::GreaterThan, 2000);
    assert!(!passes(&without, over()) && !passes(&zero, over()) && passes(&with, over()));
    assert!(!passes(
        &zero,
        number(Field::Year, Operator::LessThan, 1990)
    ));
}

#[test]
fn number_operators() {
    let song = song(
        r#""id":"1","title":"T","year":1970,"playCount":3,"userRating":4,"duration":210,
        "bpm":120,"bitRate":320,"bitDepth":24,"samplingRate":96000"#,
    );
    let range = |field, from, to| Rule {
        field,
        operator: Operator::Between,
        value: Some(Value::Range([from, to])),
    };
    assert!(passes(&song, number(Field::Year, Operator::Is, 1970)));
    assert!(!passes(&song, number(Field::Year, Operator::Is, 1971)));
    assert!(passes(&song, number(Field::Year, Operator::IsNot, 1971)));
    assert!(!passes(&song, number(Field::Year, Operator::IsNot, 1970)));
    assert!(passes(
        &song,
        number(Field::PlayCount, Operator::GreaterThan, 2)
    ));
    assert!(!passes(
        &song,
        number(Field::PlayCount, Operator::GreaterThan, 3)
    ));
    assert!(passes(&song, number(Field::Rating, Operator::LessThan, 5)));
    assert!(!passes(&song, number(Field::Rating, Operator::LessThan, 4)));
    // Both ends are in.
    assert!(passes(&song, range(Field::Year, 1970, 1979)));
    assert!(passes(&song, range(Field::Year, 1960, 1970)));
    assert!(!passes(&song, range(Field::Year, 1971, 1979)));
    assert!(passes(&song, number(Field::Duration, Operator::Is, 210)));
    assert!(passes(&song, number(Field::Bpm, Operator::Is, 120)));
    assert!(passes(
        &song,
        number(Field::BitRate, Operator::GreaterThan, 256)
    ));
    assert!(passes(&song, number(Field::BitDepth, Operator::Is, 24)));
    assert!(passes(
        &song,
        number(Field::SamplingRate, Operator::GreaterThan, 48_000)
    ));
}

#[test]
fn a_song_never_played_or_rated_has_zero_of_each() {
    let song = song(r#""id":"1","title":"T""#);
    assert!(passes(&song, number(Field::PlayCount, Operator::Is, 0)));
    assert!(passes(&song, number(Field::Rating, Operator::LessThan, 1)));
    assert!(!passes(
        &song,
        number(Field::PlayCount, Operator::GreaterThan, 0)
    ));
}

#[test]
fn date_operators() {
    let added = |date: &str| {
        song(&format!(
            r#""id":"1","title":"T","created":"{date}T10:00:00Z""#
        ))
    };
    let in_last = |date: &str, amount, unit| {
        passes(
            &added(date),
            within(Field::Added, Operator::InTheLast, amount, unit),
        )
    };
    // Days and weeks count back from today, the first day of the stretch in.
    assert!(in_last("2026-03-01", 30, Unit::Days));
    assert!(!in_last("2026-02-28", 30, Unit::Days));
    assert!(in_last("2026-03-24", 1, Unit::Weeks) && !in_last("2026-03-23", 1, Unit::Weeks));
    // A month back from the 31st of March ends on February's last day.
    assert!(in_last("2026-02-28", 1, Unit::Months) && !in_last("2026-02-27", 1, Unit::Months));
    assert!(in_last("2025-03-31", 1, Unit::Years) && !in_last("2025-03-30", 1, Unit::Years));
    assert!(in_last("2025-12-31", 3, Unit::Months) && !in_last("2025-12-30", 3, Unit::Months));
    let not_in_last = |date: &str| {
        passes(
            &added(date),
            within(Field::Added, Operator::NotInTheLast, 30, Unit::Days),
        )
    };
    assert!(not_in_last("2026-02-28") && !not_in_last("2026-03-01"));
    let before = |date: &str| {
        passes(
            &added(date),
            text(Field::Added, Operator::Before, "2026-01-15"),
        )
    };
    let after = |date: &str| {
        passes(
            &added(date),
            text(Field::Added, Operator::After, "2026-01-15"),
        )
    };
    assert!(before("2026-01-14") && !before("2026-01-15") && !before("2026-01-16"));
    assert!(after("2026-01-16") && !after("2026-01-15") && !after("2026-01-14"));
}

#[test]
fn last_played_has_a_never() {
    let played = song(r#""id":"1","title":"T","played":"2026-03-30T10:00:00Z""#);
    let unplayed = song(r#""id":"2","title":"T""#);
    let never = Rule {
        field: Field::LastPlayed,
        operator: Operator::Never,
        value: None,
    };
    assert!(!passes(&played, never.clone()) && passes(&unplayed, never));
    let lately = || within(Field::LastPlayed, Operator::InTheLast, 7, Unit::Days);
    assert!(passes(&played, lately()) && !passes(&unplayed, lately()));
}

#[test]
fn favourite_is_a_yes_or_no() {
    let loved = song(r#""id":"1","title":"T","starred":"2026-01-01T00:00:00Z""#);
    let plain = song(r#""id":"2","title":"T""#);
    let favourite = |yes| Rule {
        field: Field::Favourite,
        operator: Operator::Is,
        value: Some(Value::Flag(yes)),
    };
    assert!(passes(&loved, favourite(true)) && !passes(&plain, favourite(true)));
    assert!(passes(&plain, favourite(false)) && !passes(&loved, favourite(false)));
}

#[test]
fn a_tag_counts_for_the_tags_above_it() {
    let cool = song(r#""id":"1","title":"T","tagIds":["t2"]"#);
    let none = song(r#""id":"2","title":"T","tagIds":["gone"]"#);
    assert!(passes(&cool, text(Field::Tag, Operator::Is, "cool")));
    assert!(passes(&cool, text(Field::Tag, Operator::Is, "Jazz")));
    assert!(!passes(&cool, text(Field::Tag, Operator::Is, "Rock")));
    assert!(passes(&none, text(Field::Tag, Operator::IsNot, "Jazz")));
}

#[test]
fn match_all_wants_every_rule_and_any_wants_one() {
    let song = song(r#""id":"1","title":"T","genre":"Blues","year":1971"#);
    let rules = |match_all| {
        let rules = vec![
            text(Field::Genre, Operator::Is, "Blues"),
            number(Field::Year, Operator::GreaterThan, 1990),
        ];
        flat(match_all, rules, Sort::default(), None)
    };
    let tags = TagNames::new();
    let now = Now {
        today: today(),
        tags: &tags,
    };
    assert!(!matches(&song, &rules(true), &now));
    assert!(matches(&song, &rules(false), &now));
}

fn numbered(count: usize) -> Vec<Child> {
    (0..count)
        .map(|i| song(&format!(r#""id":"s{i:04}","title":"Song {i}","track":{i}"#)))
        .collect()
}

fn pick(songs: &[Child], rules: &SmartRules, seed: u64) -> Vec<String> {
    let tags = TagNames::new();
    let now = Now {
        today: today(),
        tags: &tags,
    };
    select(songs, rules, &now, seed)
        .into_iter()
        .map(|song| song.id.clone())
        .collect()
}

fn everything(sort: Sort, limit: Option<u32>) -> SmartRules {
    let rules = vec![number(Field::PlayCount, Operator::GreaterThan, -1)];
    flat(true, rules, sort, limit)
}

#[test]
fn the_limit_is_the_servers_when_none_is_sent() {
    let songs = numbered(DEFAULT_LIMIT as usize + 20);
    let sort = Sort::default();
    assert_eq!(
        pick(&songs, &everything(sort, None), 0).len(),
        DEFAULT_LIMIT as usize
    );
    assert_eq!(pick(&songs, &everything(sort, Some(3)), 0).len(), 3);
    assert_eq!(
        pick(&songs, &everything(sort, Some(5000)), 0).len(),
        songs.len()
    );
}

#[test]
fn ties_break_by_album_disc_track_and_id_whichever_way_it_sorts() {
    let songs = [
        song(r#""id":"d","title":"Same","playCount":5,"album":"B","discNumber":1,"track":1"#),
        song(r#""id":"c","title":"Same","playCount":5,"album":"A","discNumber":2,"track":1"#),
        song(r#""id":"b","title":"Same","playCount":5,"album":"A","discNumber":1,"track":2"#),
        song(r#""id":"a2","title":"Same","playCount":5,"album":"A","discNumber":1,"track":1"#),
        song(r#""id":"a1","title":"Same","playCount":5,"album":"A","discNumber":1,"track":1"#),
        song(r#""id":"low","title":"Low","playCount":1"#),
    ];
    let most = Sort {
        by: SortKey::PlayCount,
        descending: true,
    };
    let least = Sort {
        by: SortKey::PlayCount,
        descending: false,
    };
    let tied = ["a1", "a2", "b", "c", "d"];
    assert_eq!(
        pick(&songs, &everything(most, None), 0),
        [tied.as_slice(), &["low"]].concat()
    );
    assert_eq!(
        pick(&songs, &everything(least, None), 0),
        [["low"].as_slice(), &tied].concat()
    );
}

#[test]
fn sorts_by_each_key_with_those_missing_it_last() {
    let songs = [
        song(
            r#""id":"1","title":"b","artist":"Zed","album":"Y","year":1990,"duration":100,"userRating":2,"created":"2020-01-01T00:00:00Z","played":"2021-01-01T00:00:00Z""#,
        ),
        song(
            r#""id":"2","title":"A","artist":"Amy","album":"X","year":1980,"duration":300,"userRating":5,"created":"2022-01-01T00:00:00Z","played":"2023-01-01T00:00:00Z""#,
        ),
        song(r#""id":"3","title":"c""#),
    ];
    let order = |by, descending| pick(&songs, &everything(Sort { by, descending }, None), 0);
    assert_eq!(order(SortKey::Title, false), ["2", "1", "3"]);
    assert_eq!(order(SortKey::Title, true), ["3", "1", "2"]);
    assert_eq!(order(SortKey::Artist, false), ["2", "1", "3"]);
    assert_eq!(order(SortKey::Album, true), ["1", "2", "3"]);
    assert_eq!(order(SortKey::Year, false), ["2", "1", "3"]);
    assert_eq!(order(SortKey::Year, true), ["1", "2", "3"]);
    assert_eq!(order(SortKey::Duration, true), ["2", "1", "3"]);
    assert_eq!(order(SortKey::Rating, true), ["2", "1", "3"]);
    assert_eq!(order(SortKey::Added, true), ["2", "1", "3"]);
    assert_eq!(order(SortKey::LastPlayed, false), ["1", "2", "3"]);
}

#[test]
fn a_random_order_holds_for_a_seed_and_changes_with_it() {
    let songs = numbered(40);
    let random = everything(
        Sort {
            by: SortKey::Random,
            descending: true,
        },
        None,
    );
    let first = pick(&songs, &random, 1);
    assert_eq!(first, pick(&songs, &random, 1));
    assert_ne!(first, pick(&songs, &random, 2));
    let mut sorted = first.clone();
    sorted.sort();
    assert_eq!(sorted.len(), 40);
}

const WIRE: &str = r#"{
  "match": "all",
  "rules": [
    { "field": "genre",     "operator": "is",        "value": "Blues" },
    { "field": "added",     "operator": "inTheLast", "value": { "amount": 90, "unit": "days" } },
    { "field": "playCount", "operator": "is",        "value": 0 },
    { "field": "year",      "operator": "between",   "value": [1960, 1979] },
    { "field": "favourite", "operator": "is",        "value": true },
    { "field": "lastPlayed", "operator": "never" },
    { "field": "added",     "operator": "before",    "value": "2026-01-31" }
  ],
  "sort": { "by": "added", "descending": true },
  "limit": 500
}"#;

#[test]
fn the_json_is_the_wire_shape_both_ways() {
    let rules = SmartRules::from_json(WIRE).unwrap();
    assert!(rules.root.match_all && rules.root.rules.len() == 7);
    assert_eq!(rules.version(), 1);
    assert_eq!(
        rules.root.leaves()[3].value,
        Some(Value::Range([1960, 1979]))
    );
    assert_eq!(rules.limit, Some(500));
    let sent: serde_json::Value = serde_json::from_str(&rules.to_json()).unwrap();
    assert_eq!(
        sent,
        serde_json::from_str::<serde_json::Value>(WIRE).unwrap()
    );
    assert_eq!(SmartRules::from_json(&rules.to_json()).unwrap(), rules);
}

#[test]
fn any_and_a_missing_limit_and_sort_round_trip() {
    let json = r#"{"match":"any","rules":[{"field":"mood","operator":"contains","value":"calm"}]}"#;
    let rules = SmartRules::from_json(json).unwrap();
    assert!(!rules.root.match_all && rules.limit.is_none());
    let sent: serde_json::Value = serde_json::from_str(&rules.to_json()).unwrap();
    assert_eq!(sent["match"], "any");
    assert!(sent.get("limit").is_none() && sent["sort"]["descending"] == false);
}

#[test]
fn what_the_server_would_refuse_is_refused() {
    let refused = |rule: &str| {
        let json = format!(r#"{{"match":"all","rules":[{rule}]}}"#);
        SmartRules::from_json(&json).is_err()
    };
    assert!(refused(r#"{"field":"colour","operator":"is","value":"x"}"#));
    assert!(refused(r#"{"field":"year","operator":"equals","value":1}"#));
    assert!(refused(
        r#"{"field":"year","operator":"is","value":"1970"}"#
    ));
    assert!(refused(
        r#"{"field":"genre","operator":"greaterThan","value":1}"#
    ));
    assert!(refused(
        r#"{"field":"year","operator":"between","value":[1980,1970]}"#
    ));
    assert!(refused(
        r#"{"field":"year","operator":"between","value":1970}"#
    ));
    assert!(refused(r#"{"field":"added","operator":"never"}"#));
    assert!(refused(
        r#"{"field":"added","operator":"before","value":"31 January"}"#
    ));
    assert!(refused(
        r#"{"field":"added","operator":"inTheLast","value":{"amount":0,"unit":"days"}}"#
    ));
    assert!(refused(
        r#"{"field":"favourite","operator":"isNot","value":true}"#
    ));
    assert!(refused(r#"{"field":"genre","operator":"is"}"#));
    assert!(!refused(r#"{"field":"lastPlayed","operator":"never"}"#));
    assert!(SmartRules::from_json(r#"{"match":"all","rules":[]}"#).is_err());
    assert!(SmartRules::from_json(r#"{"match":"some","rules":[]}"#).is_err());
    let limited = |limit| {
        let json = format!(
            r#"{{"match":"all","rules":[{{"field":"genre","operator":"is","value":"x"}}],"limit":{limit}}}"#
        );
        SmartRules::from_json(&json).is_ok()
    };
    assert!(!limited(0) && limited(1) && limited(5000) && !limited(5001));
}

#[test]
fn civil_dates_come_back_from_day_numbers() {
    for date in [
        "1970-01-01",
        "2000-02-29",
        "2026-03-31",
        "2024-12-31",
        "1999-03-01",
    ] {
        let (year, month, day) = civil_date(day_number(date).unwrap());
        assert_eq!(format!("{year:04}-{month:02}-{day:02}"), date);
    }
}

#[test]
fn the_rules_in_a_line() {
    let rules = SmartRules::from_json(WIRE).unwrap();
    assert_eq!(
        summary(&rules),
        "Blues · added in the last 90 days · never played · Year between 1960 - 1979 · favourite · never played · Added before 2026-01-31"
    );
    let any = SmartRules {
        root: Group::of(
            false,
            vec![
                text(Field::Genre, Operator::Is, "Blues"),
                number(Field::Duration, Operator::LessThan, 150),
            ],
        ),
        ..rules
    };
    assert_eq!(summary(&any), "Any of: Blues · Duration less than 2:30");
}

#[test]
fn a_typed_value_reads_by_its_field_and_operator() {
    let rule = |field, operator, typed: &str| {
        parse_rule(field, operator, typed).and_then(|rule| rule.value)
    };
    assert_eq!(
        rule(Field::Year, Operator::Between, "1960 - 1979"),
        Some(Value::Range([1960, 1979]))
    );
    assert_eq!(
        rule(Field::Year, Operator::Between, "1960-1979"),
        Some(Value::Range([1960, 1979]))
    );
    assert_eq!(
        rule(Field::Duration, Operator::Between, "3:00 - 5:00"),
        Some(Value::Range([180, 300]))
    );
    assert_eq!(
        rule(Field::Duration, Operator::GreaterThan, "3:30"),
        Some(Value::Number(210))
    );
    assert_eq!(
        rule(Field::Duration, Operator::GreaterThan, "90"),
        Some(Value::Number(90))
    );
    assert_eq!(
        rule(Field::Added, Operator::InTheLast, "3 months"),
        Some(Value::Within(Span {
            amount: 3,
            unit: Unit::Months
        }))
    );
    assert_eq!(
        rule(Field::Added, Operator::InTheLast, "1 year"),
        Some(Value::Within(Span {
            amount: 1,
            unit: Unit::Years
        }))
    );
    assert_eq!(
        rule(Field::Added, Operator::InTheLast, "90"),
        Some(Value::Within(Span {
            amount: 90,
            unit: Unit::Days
        }))
    );
    assert_eq!(
        rule(Field::Favourite, Operator::Is, "No"),
        Some(Value::Flag(false))
    );
    assert_eq!(
        rule(Field::Genre, Operator::Is, " Blues "),
        Some(Value::Text("Blues".into()))
    );
    assert!(parse_rule(Field::LastPlayed, Operator::Never, "").is_some());
    for (field, operator, typed) in [
        (Field::Year, Operator::Is, "soon"),
        (Field::Year, Operator::Between, "1970"),
        (Field::Year, Operator::Between, "1980 - 1970"),
        (Field::Added, Operator::InTheLast, "90 fortnights"),
        (Field::Added, Operator::Before, "January"),
        (Field::Genre, Operator::Is, "  "),
        (Field::Favourite, Operator::Is, "maybe"),
    ] {
        assert!(parse_rule(field, operator, typed).is_none(), "{typed}");
    }
}

#[test]
fn a_draft_is_read_when_every_row_is() {
    let mut draft = Draft::default();
    assert!(draft.rules().is_err() && draft.readable_rules().is_none());
    draft.set_text(0, "Blues");
    let rules = draft.rules().unwrap();
    assert_eq!(
        (rules.limit, rules.sort),
        (
            Some(500),
            Sort {
                by: SortKey::Added,
                descending: true
            }
        )
    );
    draft.add_row();
    draft.set_field(
        1,
        Field::ALL.iter().position(|&f| f == Field::Year).unwrap(),
    );
    assert!(draft.rules().is_err(), "the new row has no value yet");
    assert_eq!(draft.readable_rules().unwrap().root.rules.len(), 1);
    draft.set_operator(1, 4);
    draft.set_text(1, "1960 - 1979");
    draft.limit = String::new();
    assert_eq!(draft.rules().unwrap().limit, None);
    draft.limit = "many".into();
    assert!(draft.rules().is_err());
    draft.limit = "9000".into();
    assert!(draft.rules().is_err());
    draft.remove_row(1);
    draft.remove_row(0);
    assert!(draft.rules().unwrap_err().contains("at least one"));
}

#[test]
fn changing_a_rows_field_keeps_what_was_typed_for_the_same_kind() {
    let mut draft = Draft::default();
    draft.set_text(0, "Blues");
    let index = |field| Field::ALL.iter().position(|&f| f == field).unwrap();
    draft.set_field(0, index(Field::Artist));
    assert_eq!(draft.rows[0].text, "Blues");
    draft.set_field(0, index(Field::Favourite));
    assert_eq!(
        (draft.rows[0].operator, draft.rows[0].text.as_str()),
        (Operator::Is, "yes")
    );
    draft.set_field(0, index(Field::LastPlayed));
    assert_eq!(
        (draft.rows[0].operator, draft.rows[0].text.as_str()),
        (Operator::InTheLast, "")
    );
}

#[test]
fn editing_gives_back_what_the_rules_say() {
    let rules = SmartRules::from_json(WIRE).unwrap();
    let draft = Draft::editing("p1", "Blues", &rules);
    assert_eq!(draft.rules().unwrap(), rules);
    assert_eq!(draft.playlist_id.as_deref(), Some("p1"));
    assert_eq!(draft.sort, sort_index(rules.sort));
}

fn title_of(field: &str) -> String {
    field.to_owned()
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

fn focus_of(match_all: bool, conditions: Vec<Condition>) -> Focus {
    Focus {
        filters: Group::of(match_all, conditions),
    }
}

fn listed(values: &[&str]) -> Value {
    Value::List(values.iter().map(|value| value.to_string()).collect())
}

#[test]
fn an_empty_group_of_any_cannot_become_a_smart_playlist() {
    let mut focus = focus_of(
        true,
        vec![condition("genre", Operator::AnyOf, listed(&["Blues"]))],
    );
    focus.add_group(&[], false);
    assert!(from_focus(&focus, &title_of).is_err());
}

#[test]
fn a_flat_focus_is_flat_rules_the_first_version_takes() {
    let focus = focus_of(
        true,
        vec![
            condition("genre", Operator::AnyOf, listed(&["Blues"])),
            condition("year", Operator::Between, Value::Range([1970, 1979])),
            condition("favourite", Operator::Is, Value::Flag(true)),
        ],
    );
    let rules = from_focus(&focus, &title_of).unwrap();
    assert!(rules.is_flat() && rules.match_all);
    assert_eq!(
        rules.leaves()[0],
        &text(Field::Genre, Operator::Is, "Blues")
    );
    assert_eq!(rules.leaves()[1].value, Some(Value::Range([1970, 1979])));
    let version = SmartRules {
        root: rules,
        sort: Sort::default(),
        limit: None,
    };
    assert_eq!(version.version(), 1);
}

#[test]
fn alternatives_and_exclusions_become_groups_the_second_version_takes() {
    let focus = focus_of(
        true,
        vec![
            condition("genre", Operator::AnyOf, listed(&["Blues", "Soul"])),
            condition("mood", Operator::NoneOf, listed(&["Sad", "Dark"])),
        ],
    );
    let rules = from_focus(&focus, &title_of).unwrap();
    assert!(!rules.is_flat());
    let [Entry::Group(any), Entry::Group(none)] = &rules.rules[..] else {
        panic!("two groups were expected");
    };
    assert!(!any.match_all && any.leaves().len() == 2);
    assert!(none.match_all);
    assert!(
        none.leaves()
            .iter()
            .all(|rule| rule.operator == Operator::IsNot)
    );
    let sent = SmartRules {
        root: rules,
        sort: Sort::default(),
        limit: None,
    };
    assert_eq!(sent.version(), 2);
    assert!(sent.check().is_ok());
}

#[test]
fn picked_decades_are_the_years_they_span() {
    let one = focus_of(
        true,
        vec![condition("decade", Operator::AnyOf, listed(&["1970s"]))],
    );
    let rules = from_focus(&one, &title_of).unwrap();
    assert_eq!(rules.leaves()[0], &number_between(Field::Year, 1970, 1979));
    let two = focus_of(
        true,
        vec![condition(
            "decade",
            Operator::AnyOf,
            listed(&["1970s", "1990s"]),
        )],
    );
    let rules = from_focus(&two, &title_of).unwrap();
    let [Entry::Group(any)] = &rules.rules[..] else {
        panic!("a group of alternatives was expected");
    };
    assert!(!any.match_all && any.leaves().len() == 2);
    let none = focus_of(
        true,
        vec![condition("decade", Operator::NoneOf, listed(&["1970s"]))],
    );
    assert!(from_focus(&none, &title_of).is_err());
}

fn number_between(field: Field, from: i64, to: i64) -> Rule {
    Rule {
        field,
        operator: Operator::Between,
        value: Some(Value::Range([from, to])),
    }
}

#[test]
fn a_dates_between_is_two_rules_around_its_ends() {
    let from = day_number("2026-01-01").unwrap();
    let to = day_number("2026-01-31").unwrap();
    let focus = focus_of(
        true,
        vec![condition(
            "added",
            Operator::Between,
            Value::Range([from, to]),
        )],
    );
    let rules = from_focus(&focus, &title_of).unwrap();
    let [Entry::Group(between)] = &rules.rules[..] else {
        panic!("a group was expected");
    };
    let leaves: Vec<_> = between
        .leaves()
        .into_iter()
        .map(|rule| (rule.operator, rule.value.clone()))
        .collect();
    assert_eq!(
        leaves,
        [
            (Operator::After, Some(Value::Text("2025-12-31".into()))),
            (Operator::Before, Some(Value::Text("2026-02-01".into()))),
        ]
    );
    let song = song(r#""id":"1","title":"T","created":"2026-01-15T10:00:00Z""#);
    let early = self::song(r#""id":"2","title":"T","created":"2025-12-31T10:00:00Z""#);
    let wanted = SmartRules {
        root: rules,
        sort: Sort::default(),
        limit: None,
    };
    let tags = TagNames::new();
    let now = Now {
        today: today(),
        tags: &tags,
    };
    assert!(matches(&song, &wanted, &now));
    assert!(!matches(&early, &wanted, &now));
}

#[test]
fn what_the_rules_cannot_say_is_refused_by_name() {
    let unknown = focus_of(
        true,
        vec![condition("production", Operator::AnyOf, listed(&["Pat"]))],
    );
    assert_eq!(
        from_focus(&unknown, &title_of).unwrap_err(),
        "production has no smart playlist rule."
    );
    let impossible = focus_of(
        true,
        vec![condition("genre", Operator::GreaterThan, Value::Number(3))],
    );
    assert!(from_focus(&impossible, &title_of).is_err());
}

#[test]
fn filters_that_say_nothing_are_left_out() {
    let mut off = condition("genre", Operator::AnyOf, listed(&["Blues"]));
    off.enabled = false;
    let empty = Condition {
        value: None,
        ..condition("mood", Operator::AnyOf, listed(&[]))
    };
    let focus = focus_of(true, vec![off, empty]);
    assert!(from_focus(&focus, &title_of).unwrap().rules.is_empty());
}

#[test]
fn rules_open_as_the_focus_they_are_and_back() {
    let rules = Group {
        match_all: false,
        rules: vec![
            Entry::Leaf(text(Field::Genre, Operator::Is, "Blues")),
            Entry::Group(Group::of(
                true,
                vec![number(Field::Year, Operator::LessThan, 1980)],
            )),
        ],
    };
    let focus = into_focus(&rules);
    assert!(!focus.filters.match_all);
    let leaves = focus.filters.leaves();
    assert_eq!(leaves.len(), 2);
    assert_eq!(
        (leaves[0].field.as_str(), leaves[0].operator),
        ("genre", Operator::Is)
    );
    assert!(leaves.iter().all(|condition| condition.enabled));
    assert_eq!(from_focus(&focus, &title_of).unwrap(), rules);
}

#[test]
fn a_new_draft_complains_only_of_what_was_typed() {
    let mut draft = Draft::default();
    assert_eq!(draft.problem(), "");
    assert!(draft.rules().is_err());
    draft.set_field(
        0,
        Field::ALL.iter().position(|&f| f == Field::Year).unwrap(),
    );
    draft.set_text(0, "nineteen");
    assert_eq!(draft.problem(), "Fix the rules marked, or take them out.");
    draft.set_text(0, "1970");
    assert_eq!(draft.problem(), "");
    assert!(draft.rules().is_ok());
    draft.limit = "many".to_owned();
    assert!(draft.problem().contains("limit"));
}

#[test]
fn a_song_missing_the_value_sorts_last_both_ways_wherever_it_starts() {
    let with = |id: &str, year: u32| song(&format!(r#""id":"{id}","title":"t","year":{year}"#));
    let without = |id: &str| song(&format!(r#""id":"{id}","title":"t""#));
    for songs in [
        [without("a"), with("b", 1990), with("c", 1980)],
        [with("b", 1990), with("c", 1980), without("a")],
        [with("b", 1990), without("a"), with("c", 1980)],
    ] {
        let order = |descending| {
            let by = SortKey::Year;
            pick(&songs, &everything(Sort { by, descending }, None), 0)
        };
        assert_eq!(order(false), ["c", "b", "a"]);
        assert_eq!(order(true), ["b", "c", "a"]);
    }
}

#[test]
fn every_order_the_wire_takes_shows_in_the_editor_as_itself() {
    let keys = [
        SortKey::PlayCount,
        SortKey::Added,
        SortKey::LastPlayed,
        SortKey::Title,
        SortKey::Artist,
        SortKey::Album,
        SortKey::Year,
        SortKey::Rating,
        SortKey::Duration,
    ];
    for by in keys {
        for descending in [false, true] {
            let wanted = Sort { by, descending };
            assert_eq!(super::words::SORTS[sort_index(wanted)].1, wanted);
        }
    }
}

#[test]
fn text_rules_keep_spaces_and_punctuation() {
    let spaced = song(r#""id":"1","title":"ab""#);
    assert!(!passes(&spaced, text(Field::Title, Operator::Is, "a b")));
    assert!(!passes(
        &spaced,
        text(Field::Title, Operator::Contains, "a b")
    ));
    let halo = song(r#""id":"2","title":"Beyoncé's Halo""#);
    assert!(passes(
        &halo,
        text(Field::Title, Operator::Is, "BEYONCE'S halo")
    ));
    assert!(!passes(
        &halo,
        text(Field::Title, Operator::Is, "beyonces halo")
    ));
}
