//! The smart playlist editor: rules typed in rows, with the songs they pick
//! worked out on this device from the library's songs as they are typed, and
//! the calls that keep them on the server.

use std::rc::Rc;

use opensubsonic::{Child, Playlist};
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};

use super::{
    Browser, SMART_PLAYLISTS, Shared, aside, count, duration_spoken, load_playlists, load_songs,
    notify, offers, offers_version, open, playlist_on_screen,
};
use crate::rules::{Group, today};
use crate::smart::draft::{Draft, Row};
use crate::smart::{Now, Operator, Rule, SmartRules, select, tag_names, words};
use crate::{AppWindow, SmartRuleRow, prefs};

/// How many of the songs the rules pick the editor lists.
const PREVIEW_LINES: usize = 6;
const SECONDS_PER_HOUR: u64 = 3600;
/// The version of the smart playlists extension that has groups of rules.
const GROUPS_VERSION: i64 = 2;
const NO_GROUPS_ON_SERVER: &str = "Your server keeps flat rules only, and these have groups. You can still keep the songs they pick as a normal playlist.";

/// What the editor holds: the draft, and the rows it shows.
#[derive(Default)]
pub(super) struct Editor {
    draft: Draft,
    rows: Rc<VecModel<SmartRuleRow>>,
}

/// Whether the playlist on screen is a smart one.
pub(super) fn is_on_screen(b: &Browser) -> bool {
    b.playlist
        .as_ref()
        .is_some_and(|playlist| playlist.smart == Some(true))
}

/// A smart playlist's rules in a line, for its page; empty for any other.
pub(super) fn rules_summary(playlist: &Playlist) -> String {
    (playlist.smart_rules.clone())
        .and_then(|json| SmartRules::from_value(json).ok())
        .map(|rules| words::summary(&rules))
        .unwrap_or_default()
}

/// Opens the editor for a new playlist (`new`), for the Focus on screen
/// (`focus`, whose rules are in the draft already), or for the playlist
/// with this id.
pub(super) fn open_editor(ui: &AppWindow, browser: &Shared, id: &str) {
    {
        let mut b = browser.borrow_mut();
        match id {
            "focus" => {}
            "new" => b.smart.draft = Draft::default(),
            id => {
                b.smart.draft = Draft {
                    playlist_id: Some(id.to_owned()),
                    rows: Vec::new(),
                    ..Draft::default()
                };
            }
        }
    }
    show_draft(ui, browser);
    let wanted = browser.borrow().smart.draft.playlist_id.clone();
    if let Some(id) = wanted {
        load_rules(ui, browser, id);
    }
    let b = browser.borrow();
    let must_load = !b.song_list_loaded && !b.song_list_loading;
    drop(b);
    if must_load {
        load_songs(ui, browser);
    }
}

/// Puts the draft in the editor's fields and rows. Its name, limit and
/// choices are set only here: once typed in, they are the editor's own.
fn show_draft(ui: &AppWindow, browser: &Shared) {
    {
        let mut b = browser.borrow_mut();
        let rows = b.smart.draft.rows.iter().map(row_of).collect::<Vec<_>>();
        b.smart.rows = Rc::new(VecModel::from(rows));
        let draft = &b.smart.draft;
        ui.set_smart_editing(draft.playlist_id.is_some());
        ui.set_smart_name(draft.name.as_str().into());
        ui.set_smart_match_all(draft.match_all);
        ui.set_smart_sort(draft.sort as i32);
        ui.set_smart_limit(draft.limit.as_str().into());
        ui.set_smart_rows(ModelRc::from(b.smart.rows.clone()));
        let fields =
            crate::smart::Field::ALL.map(|field| SharedString::from(words::field_label(field)));
        ui.set_smart_fields(ModelRc::new(VecModel::from(fields.to_vec())));
        let sorts = words::SORTS.map(|(name, _)| SharedString::from(name));
        ui.set_smart_sorts(ModelRc::new(VecModel::from(sorts.to_vec())));
    }
    render(ui, browser);
}

/// Asks for the playlist being edited, for its name and rules.
fn load_rules(ui: &AppWindow, browser: &Shared, id: String) {
    aside(
        ui,
        browser,
        {
            let id = id.clone();
            move |client| {
                let id = id.clone();
                async move { client.get_playlist(&id).await }
            }
        },
        move |ui, browser, answer| {
            let mut b = browser.borrow_mut();
            let untouched = b.smart.draft.playlist_id.as_deref() == Some(id.as_str())
                && b.smart.draft.rows.is_empty();
            if !untouched {
                return;
            }
            let Some(json) = answer.playlist.smart_rules else {
                drop(b);
                return ui.set_smart_problem("The server keeps this playlist's rules in a form this app can't show, so they can only be edited where it was made.".into());
            };
            match SmartRules::from_value(json) {
                Ok(rules) => {
                    b.smart.draft = Draft::editing(&id, &answer.playlist.name, &rules);
                    drop(b);
                    show_draft(ui, browser);
                }
                Err(e) => {
                    drop(b);
                    ui.set_smart_problem(format!("These rules can't be read here: {e}").into());
                }
            }
        },
    );
}

fn row_of(row: &Row) -> SmartRuleRow {
    let operators = row
        .field
        .operators()
        .iter()
        .map(|&operator| SharedString::from(words::operator_label(operator)));
    SmartRuleRow {
        field: row.field_index() as i32,
        operator: row.operator_index() as i32,
        operators: ModelRc::new(VecModel::from(operators.collect::<Vec<_>>())),
        value: row.text.as_str().into(),
        hint: words::value_hint(row.field, row.operator).into(),
        takes_value: row.operator != Operator::Never,
        invalid: row.is_broken(),
    }
}

/// Shows what the draft gives: its count and first songs, why it can't be
/// saved yet, and whether it can.
fn render(ui: &AppWindow, browser: &Shared) {
    show_preview(ui, browser);
    let b = browser.borrow();
    let draft = &b.smart.draft;
    let rules = draft.rules();
    let named = !draft.name.trim().is_empty();
    let can_keep = rules.is_ok() && named;
    let needs_groups = rules.as_ref().is_ok_and(|rules| rules.version() > 1);
    let blocked = needs_groups && !offers_version(&b, SMART_PLAYLISTS, GROUPS_VERSION);
    ui.set_smart_can_keep(can_keep);
    ui.set_smart_can_save(can_keep && !blocked);
    ui.set_smart_groups(groups_line(draft).into());
    let problem = match blocked {
        true => NO_GROUPS_ON_SERVER.to_owned(),
        false => draft.problem(),
    };
    ui.set_smart_problem(problem.into());
}

/// What the groups of the rules are, for the rows that can't show them.
fn groups_line(draft: &Draft) -> String {
    match draft.groups.len() {
        0 => String::new(),
        groups => format!(
            "{} of rules kept as they are.",
            count(groups, "group", "groups")
        ),
    }
}

/// Works out the songs the rules pick from the library's songs, as far as
/// they have arrived, and shows how many there are and the first few.
pub(super) fn show_preview(ui: &AppWindow, browser: &Shared) {
    let b = browser.borrow();
    let (summary, lines) = match picked(&b) {
        None => ("Add a rule to see its songs.".to_owned(), Vec::new()),
        Some(songs) => (
            count_line(&b, &songs),
            songs
                .iter()
                .take(PREVIEW_LINES)
                .map(|song| song_line(song))
                .collect(),
        ),
    };
    ui.set_smart_count(summary.into());
    ui.set_smart_preview(ModelRc::new(VecModel::from(lines)));
}

fn count_line(b: &Browser, found: &[&Child]) -> String {
    let seconds: i64 = found.iter().filter_map(|song| song.duration).sum();
    let songs = format!(
        "{}, {}",
        count(found.len(), "song", "songs"),
        duration_spoken(seconds)
    );
    match b.song_list_loaded {
        true => songs,
        false => format!("{songs} so far, the library is still loading"),
    }
}

fn song_line(song: &Child) -> SharedString {
    match song.artist.as_deref() {
        Some(artist) => format!("{} - {artist}", song.title).into(),
        None => song.title.as_str().into(),
    }
}

/// The songs the rows that read pick, none while no row reads.
fn picked(b: &Browser) -> Option<Vec<&Child>> {
    let rules = b.smart.draft.readable_rules()?;
    let tags = tag_names(&b.tags);
    let now = Now {
        today: today(prefs::now()),
        tags: &tags,
    };
    let seed = prefs::now() / SECONDS_PER_HOUR;
    Some(select(&b.song_list, &rules, &now, seed))
}

pub(super) fn wire(ui: &AppWindow, browser: &Shared) {
    ui.on_smart_edit({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move || {
            let Some(id) = playlist_on_screen(&browser.borrow()) else {
                return;
            };
            open(&weak.unwrap(), &browser, format!("smart:{id}"));
        }
    });
    ui.on_smart_name_edited({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move |name| {
            change(&weak.unwrap(), &browser, |draft| {
                draft.name = name.to_string()
            })
        }
    });
    ui.on_smart_match_chosen({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move |all| change(&weak.unwrap(), &browser, |draft| draft.match_all = all)
    });
    ui.on_smart_sort_chosen({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move |sort| change(&weak.unwrap(), &browser, |draft| draft.sort = sort as usize)
    });
    ui.on_smart_limit_edited({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move |limit| {
            change(&weak.unwrap(), &browser, |draft| {
                draft.limit = limit.to_string()
            })
        }
    });
    ui.on_smart_value_edited({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move |row, text| {
            let row = row as usize;
            change_row(&weak.unwrap(), &browser, row, Shown::Updated, |draft| {
                draft.set_text(row, &text)
            })
        }
    });
    ui.on_smart_operator_picked({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move |row, operator| {
            let row = row as usize;
            change_row(&weak.unwrap(), &browser, row, Shown::Updated, |draft| {
                draft.set_operator(row, operator as usize)
            })
        }
    });
    ui.on_smart_field_picked({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move |row, field| {
            let row = row as usize;
            change_row(&weak.unwrap(), &browser, row, Shown::Replaced, |draft| {
                draft.set_field(row, field as usize)
            })
        }
    });
    ui.on_smart_rule_added({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move || {
            let ui = weak.unwrap();
            {
                let mut b = browser.borrow_mut();
                b.smart.draft.add_row();
                let added = b.smart.draft.rows.last().map(row_of);
                b.smart.rows.push(added.expect("a row was just added"));
            }
            render(&ui, &browser);
        }
    });
    ui.on_smart_rule_removed({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move |row| {
            let ui = weak.unwrap();
            {
                let mut b = browser.borrow_mut();
                b.smart.draft.remove_row(row as usize);
                if (row as usize) < b.smart.rows.row_count() {
                    b.smart.rows.remove(row as usize);
                }
            }
            render(&ui, &browser);
        }
    });
    ui.on_smart_save({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move || save(&weak.unwrap(), &browser)
    });
    ui.on_smart_open_in_focus({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move || {
            let rules = browser.borrow().smart.draft.rules();
            if let Ok(rules) = rules {
                super::focus::show_rules(&weak.unwrap(), &browser, &rules.root);
            }
        }
    });
    ui.on_smart_save_as_playlist({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move || save_as_playlist(&weak.unwrap(), &browser)
    });
}

/// Changes the draft and shows what it gives now.
fn change(ui: &AppWindow, browser: &Shared, change: impl FnOnce(&mut Draft)) {
    change(&mut browser.borrow_mut().smart.draft);
    render(ui, browser);
}

/// How a changed rule shows in its row: the row's data replaced, or the row
/// made anew, for a change that sets the choices in it back.
enum Shown {
    Updated,
    Replaced,
}

fn change_row(
    ui: &AppWindow,
    browser: &Shared,
    at: usize,
    shown: Shown,
    change: impl FnOnce(&mut Draft),
) {
    {
        let mut b = browser.borrow_mut();
        change(&mut b.smart.draft);
        let Some(row) = b.smart.draft.rows.get(at).map(row_of) else {
            return;
        };
        match shown {
            Shown::Updated => b.smart.rows.set_row_data(at, row),
            Shown::Replaced => {
                b.smart.rows.remove(at);
                b.smart.rows.insert(at, row);
            }
        }
    }
    render(ui, browser);
}

/// Makes the smart playlist, or gives the one edited its new rules, and opens
/// it.
fn save(ui: &AppWindow, browser: &Shared) {
    let (draft, client) = {
        let b = browser.borrow();
        (b.smart.draft.clone(), b.client.clone())
    };
    let (Some(client), Ok(rules)) = (client, draft.rules()) else {
        return;
    };
    if rules.version() > 1 && !offers_version(&browser.borrow(), SMART_PLAYLISTS, GROUPS_VERSION) {
        return ui.set_smart_problem(NO_GROUPS_ON_SERVER.into());
    }
    let (name, json) = (draft.name.trim().to_owned(), rules.to_json());
    let (weak, browser) = (ui.as_weak(), browser.clone());
    slint::spawn_local(async move {
        let saved = match &draft.playlist_id {
            None => (client.create_smart_playlist(&name, &json).await).map(|made| made.playlist.id),
            Some(id) => (client
                .update_smart_playlist(id, Some(&name), Some(&json))
                .await)
                .map(|()| id.clone()),
        };
        let Some(ui) = weak.upgrade() else { return };
        match saved {
            Ok(id) => {
                load_playlists(&ui, &browser);
                open(&ui, &browser, format!("playlist:{id}"));
            }
            Err(e) => ui.set_smart_problem(format!("Your server didn't take that: {e}").into()),
        }
    })
    .expect("the event loop is running");
}

/// Makes a plain playlist of the songs the rules pick now, which no longer
/// follow the rules, and opens it.
fn save_as_playlist(ui: &AppWindow, browser: &Shared) {
    let (ids, name, client, loaded) = {
        let b = browser.borrow();
        let ids: Vec<String> = (picked(&b).unwrap_or_default().into_iter())
            .map(|song| song.id.clone())
            .collect();
        (
            ids,
            b.smart.draft.name.trim().to_owned(),
            b.client.clone(),
            b.song_list_loaded,
        )
    };
    let Some(client) = client else { return };
    if !loaded {
        return notify(ui, browser, "The library is still loading.".into());
    }
    if ids.is_empty() {
        return notify(ui, browser, "These rules pick no songs.".into());
    }
    let (weak, browser) = (ui.as_weak(), browser.clone());
    slint::spawn_local(async move {
        let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
        let made = client.create_playlist(&name, &ids).await;
        let Some(ui) = weak.upgrade() else { return };
        load_playlists(&ui, &browser);
        match made {
            Ok(Some(made)) => open(&ui, &browser, format!("playlist:{}", made.playlist.id)),
            Ok(None) => {}
            Err(e) => ui.set_smart_problem(format!("Your server didn't take that: {e}").into()),
        }
    })
    .expect("the event loop is running");
}

/// Opens the editor with the rules a Focus is.
pub(super) fn open_for_focus(ui: &AppWindow, browser: &Shared, rules: Group<Rule>) {
    browser.borrow_mut().smart.draft = Draft::of_rules(&rules);
    open(ui, browser, "smart:focus".into());
}

/// Whether the server has smart playlists.
pub(super) fn offered(b: &Browser) -> bool {
    offers(b, SMART_PLAYLISTS)
}
