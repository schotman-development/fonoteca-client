//! The Focus of the overview pages: the filters each page has, their values
//! and counts, and what the panel and the phone's sheet show of them. The
//! values live as long as the account is signed in; the set of filters, whether
//! the panel is open and the filters saved by name are kept on this device.
//! The counts are made when the panel opens, a filter changes or more of the
//! page's items have arrived, never as the panel is drawn.

use std::borrow::Cow;

use opensubsonic::ArtistID3;

use crate::focus::words::{self as say};
use crate::focus::{
    self as model, Condition, Counted, Field, Focus, Info, Kind, Tally, info_of, top_labels,
    top_threshold,
};
use crate::rules::words::operator_label;
use crate::rules::{Entry, FieldValue, Group, MOST_DEPTH, Now, Operator, Value, today};
use crate::{FocusBand, FocusMenuEntry, FocusRow, FocusState, FocusValue};

use super::fields::{
    ALBUM_FIELDS, ARTIST_FIELDS, ArtistFacts, COMPOSER_FIELDS, COMPOSITION_FIELDS, GROUPS,
    PLAYLIST_FIELDS, SONG_FIELDS, album_genres, defaults_of, infos_of, key,
};
use super::*;

/// How many values of a filter a column lists before View more.
const VALUES_IN_COLUMN: usize = 6;
/// The longest name a saved set of filters has.
const LONGEST_NAME: usize = 60;

/// What kind of control a filter's row is, as `FocusRow.kind` says it.
const GROUP_ROW: i32 = 0;
const VALUES_ROW: i32 = 1;
const TEXT_ROW: i32 = 2;
const BOXES_ROW: i32 = 3;
const FLAG_ROW: i32 = 4;

/// A flag filter's choices: either, yes, no.
const EITHER: i32 = 0;
const YES: i32 = 1;
const NO: i32 = 2;

/// What the Focus keeps: the filters of each page, which last until sign-out,
/// and what the panel last counted.
#[derive(Default)]
pub(super) struct State {
    focuses: BTreeMap<&'static str, Focus>,
    /// Each page's panel as saved on this device, by `key`.
    pages: BTreeMap<String, prefs::FocusPage>,
    /// The filters saved by name, by the page's `key`.
    saved: BTreeMap<String, Vec<prefs::SavedFilter>>,
    /// The page the panel was last shown for, so that another page starts
    /// without a sheet or a list.
    last_page: Option<Page>,
    /// The counts of the page on screen, and what they were made for.
    counts: Counted,
    counted: Option<Made>,
    /// The filter whose values are listed whole, and the one a phone's sheet
    /// has opened, by its place among the panel's rows.
    listing: Option<usize>,
    editing: Option<usize>,
    /// What each chip lets go of when dropped: the filter, and a value of it.
    chips: Vec<(Vec<usize>, Option<String>)>,
    /// Set while a box is typed in, so that the rows stay as they are.
    typing: bool,
    /// The search over the filters of the "Add filter" menu.
    menu_query: String,
}

impl State {
    fn focus(&self, page: Page) -> Option<&Focus> {
        self.focuses.get(key(page)?)
    }

    /// The page's filters, made of what is saved or else the page's own
    /// first choice the first time.
    fn focus_mut(&mut self, page: Page) -> Option<&mut Focus> {
        let page_key = key(page)?;
        let kept = self
            .pages
            .get(page_key)
            .and_then(|saved| saved.filters.clone());
        Some(self.focuses.entry(page_key).or_insert_with(|| {
            let infos = infos_of(page);
            let mut focus = kept.unwrap_or_else(|| Focus::of(defaults_of(page), &infos));
            focus.retain_known(&infos);
            focus
        }))
    }
}

/// What the counts were made for, which they hold until it changes: the page,
/// the items it and the artists' albums held, and how many were hearted.
#[derive(PartialEq)]
struct Made {
    page: Page,
    items: usize,
    albums: usize,
    hearted: usize,
}

/// The page's items, with the fields they are told apart by.
trait Surface {
    fn len(&self) -> usize;
    fn hearted(&self, now: &Now) -> usize;
    fn matching(&self, focus: &Focus, now: &Now) -> Vec<bool>;
    fn count(&self, focus: &Focus, now: &Now) -> Counted;
    fn plays(&self, now: &Now) -> Vec<i64>;
}

struct Source<'a, T: Clone + 'static> {
    items: Cow<'a, [T]>,
    fields: &'static [Field<T>],
}

impl<T: Clone + 'static> Source<'_, T> {
    fn numbers(&self, key: &str, now: &Now) -> Vec<i64> {
        let Some(field) = self.fields.iter().find(|field| field.key == key) else {
            return Vec::new();
        };
        (self.items.iter())
            .filter_map(|item| match (field.get)(item, now) {
                FieldValue::Number(at) => at,
                _ => None,
            })
            .collect()
    }
}

impl<T: Clone + 'static> Surface for Source<'_, T> {
    fn len(&self) -> usize {
        self.items.len()
    }

    fn hearted(&self, now: &Now) -> usize {
        let Some(field) = (self.fields.iter()).find(|field| field.key == model::FAVOURITE) else {
            return 0;
        };
        (self.items.iter())
            .filter(|item| (field.get)(item, now) == FieldValue::Flag(true))
            .count()
    }

    fn matching(&self, focus: &Focus, now: &Now) -> Vec<bool> {
        model::matching(&self.items, self.fields, focus, now)
    }

    fn count(&self, focus: &Focus, now: &Now) -> Counted {
        model::count(&self.items, self.fields, focus, now)
    }

    fn plays(&self, now: &Now) -> Vec<i64> {
        self.numbers("playCount", now)
    }
}

fn surface(b: &Browser, page: Page) -> Option<Box<dyn Surface + '_>> {
    Some(match page {
        Page::Albums => Box::new(Source {
            items: Cow::Borrowed(b.library.as_slice()),
            fields: ALBUM_FIELDS,
        }),
        Page::Tracks => Box::new(Source {
            items: Cow::Borrowed(b.song_list.as_slice()),
            fields: SONG_FIELDS,
        }),
        Page::Artists => Box::new(Source {
            items: Cow::Owned(artist_facts(b)),
            fields: ARTIST_FIELDS,
        }),
        Page::Composers => Box::new(Source {
            items: Cow::Borrowed(b.composers.as_slice()),
            fields: COMPOSER_FIELDS,
        }),
        Page::Compositions => Box::new(Source {
            items: Cow::Borrowed(b.compositions.as_slice()),
            fields: COMPOSITION_FIELDS,
        }),
        Page::Playlists => Box::new(Source {
            items: Cow::Borrowed(b.playlists.as_slice()),
            fields: PLAYLIST_FIELDS,
        }),
        _ => return None,
    })
}

/// What the page lists, for a value's count: "1 album", "198 albums".
fn nouns(page: Page) -> (&'static str, &'static str) {
    match page {
        Page::Tracks => ("track", "tracks"),
        Page::Artists => ("artist", "artists"),
        Page::Composers => ("composer", "composers"),
        Page::Compositions => ("composition", "compositions"),
        Page::Playlists => ("playlist", "playlists"),
        _ => ("album", "albums"),
    }
}

fn now_of<R>(b: &Browser, use_now: impl FnOnce(&Now) -> R) -> R {
    let tags = crate::smart::tag_names(&b.tags);
    use_now(&Now {
        today: today(prefs::now()),
        tags: &tags,
    })
}

/// Whether each of the page's items passes the filters, in the order of the
/// page's list, for the page to leave out those that don't. None where no
/// filter narrows it.
pub(super) fn allowed(b: &Browser, page: Page) -> Option<Vec<bool>> {
    let focus = b.focus.focus(page).filter(|focus| focus.is_narrowing())?;
    let items = surface(b, page)?;
    Some(now_of(b, |now| items.matching(focus, now)))
}

/// The `view` of the page's items without those the filters leave out.
pub(super) fn narrow(b: &Browser, page: Page, mut view: Vec<usize>) -> Vec<usize> {
    if let Some(allowed) = allowed(b, page) {
        view.retain(|&i| allowed[i]);
    }
    view
}

/// Picks up what is saved on this device, on sign-in.
pub(super) fn load_prefs(b: &mut Browser, prefs: &prefs::Prefs) {
    b.focus.pages = prefs.focus.clone();
    b.focus.saved = prefs.saved_filters.clone();
    b.focus.focuses.clear();
}

/// The Focus' own saved state is written again with the filters now there,
/// without their values.
fn keep(b: &mut Browser) {
    remember(b);
    prefs::save(&prefs::Prefs {
        focus: b.focus.pages.clone(),
        saved_filters: b.focus.saved.clone(),
        ..prefs::load()
    });
}

/// The filters of every page as they are, minus their values, for `keep`.
fn remember(b: &mut Browser) {
    for page_key in PAGES.iter().filter_map(|&page| key(page)) {
        let Some(focus) = b.focus.focuses.get(page_key) else {
            continue;
        };
        let mut set = focus.clone();
        set.clear();
        b.focus
            .pages
            .entry(page_key.to_owned())
            .or_default()
            .filters = Some(set);
    }
}

const PAGES: [Page; 6] = [
    Page::Albums,
    Page::Tracks,
    Page::Artists,
    Page::Composers,
    Page::Compositions,
    Page::Playlists,
];

/// How a callback changes the page's filters: whether the set of filters
/// changes with it, so that it is kept, and whether the rows are shown anew.
#[derive(Clone, Copy, PartialEq)]
enum Change {
    Value,
    Set,
    Typing,
}

/// Has `change` made to the filters of the page on screen, and shows the page
/// narrowed by them.
fn edit(ui: &AppWindow, browser: &Shared, how: Change, change: impl FnOnce(&mut Focus, &[Info])) {
    let page = ui.get_page();
    {
        let mut b = browser.borrow_mut();
        let infos = infos_of(page);
        if let Some(focus) = b.focus.focus_mut(page) {
            change(focus, &infos);
        }
        b.focus.counted = None;
        b.focus.typing = how == Change::Typing;
        if how == Change::Set {
            keep(&mut b);
        }
    }
    super::show(ui, browser);
    browser.borrow_mut().focus.typing = false;
}

/// The path of the row at `index` among the panel's rows.
fn path_of(focus: &Focus, index: i32) -> Option<Vec<usize>> {
    let place = focus
        .places()
        .into_iter()
        .nth(usize::try_from(index).ok()?)?;
    Some(place.path)
}

/// Has `change` made to the filter in row `index`, which is not a group.
fn edit_condition(
    ui: &AppWindow,
    browser: &Shared,
    how: Change,
    index: i32,
    change: impl FnOnce(&mut Condition, Kind),
) {
    edit(ui, browser, how, |focus, infos| {
        let Some(condition) = path_of(focus, index).and_then(|path| focus.condition_mut(&path))
        else {
            return;
        };
        if let Some(info) = info_of(infos, &condition.field) {
            change(condition, info.kind);
        }
    });
}

pub(super) fn wire(ui: &AppWindow, browser: &Shared) {
    let state = ui.global::<FocusState>();
    state.on_visibility_changed({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move |open, sheet| change_visibility(&weak.unwrap(), &browser, open, sheet)
    });
    state.on_operator_picked({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move |row, at| {
            edit_condition(
                &weak.unwrap(),
                &browser,
                Change::Value,
                row,
                |condition, kind| {
                    if let Some(&operator) = kind.operators().get(at as usize) {
                        set_operator(condition, kind, operator);
                    }
                },
            )
        }
    });
    state.on_text_edited({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move |row, second, text| {
            edit_condition(
                &weak.unwrap(),
                &browser,
                Change::Typing,
                row,
                |condition, kind| {
                    let mut boxes = say::typed(kind, condition);
                    boxes[usize::from(second)] = text.to_string();
                    condition.value = say::read(kind, condition.operator, &boxes);
                    condition.typed = Some(boxes);
                },
            )
        }
    });
    state.on_text_committed({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move || {
            let ui = weak.unwrap();
            browser.borrow_mut().focus.counted = None;
            super::show(&ui, &browser);
        }
    });
    state.on_flag_picked({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move |row, choice| {
            edit_condition(
                &weak.unwrap(),
                &browser,
                Change::Value,
                row,
                |condition, _| {
                    condition.value = (choice != EITHER).then_some(Value::Flag(choice == YES));
                },
            )
        }
    });
    state.on_value_picked({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move |row, value| {
            edit(&weak.unwrap(), &browser, Change::Value, |focus, _| {
                if let Some(path) = path_of(focus, row) {
                    focus.toggle(&path, &value);
                }
            })
        }
    });
    state.on_preset_picked({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move |row, label| {
            let ui = weak.unwrap();
            let plays = presets_plays(&browser.borrow(), ui.get_page());
            edit_condition(&ui, &browser, Change::Value, row, |condition, _| {
                apply_preset(condition, &label, &plays)
            })
        }
    });
    state.on_enabled_toggled({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move |row| {
            let ui = weak.unwrap();
            edit(&ui, &browser, Change::Set, |focus, _| {
                let Some(path) = path_of(focus, row) else {
                    return;
                };
                if let Some(condition) = focus.condition_mut(&path) {
                    condition.enabled = !condition.enabled;
                }
            })
        }
    });
    state.on_removed({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move |row| {
            let ui = weak.unwrap();
            {
                let mut b = browser.borrow_mut();
                b.focus.listing = None;
                b.focus.editing = None;
            }
            edit(&ui, &browser, Change::Set, |focus, _| {
                if let Some(path) = path_of(focus, row) {
                    focus.remove(&path);
                }
            })
        }
    });
    state.on_group_toggled({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move |row| {
            edit(&weak.unwrap(), &browser, Change::Set, |focus, _| {
                if let Some(path) = path_of(focus, row) {
                    focus.toggle_match_all(&path);
                }
            })
        }
    });
    state.on_group_added({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move |row| {
            edit(&weak.unwrap(), &browser, Change::Set, |focus, _| {
                if let Some(path) = path_of(focus, row) {
                    focus.add_group(&path, true);
                }
            })
        }
    });
    state.on_filter_added({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move |row, at| {
            let ui = weak.unwrap();
            browser.borrow_mut().focus.menu_query.clear();
            ui.global::<FocusState>().set_menu_query("".into());
            edit(&ui, &browser, Change::Set, |focus, infos| {
                let (Some(path), Some(info)) = (path_of(focus, row), infos.get(at as usize)) else {
                    return;
                };
                focus.add(&path, Condition::new(info));
            })
        }
    });
    state.on_menu_searched({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move |query| {
            let ui = weak.unwrap();
            browser.borrow_mut().focus.menu_query = query.trim().to_owned();
            let b = browser.borrow();
            show_menu(&ui.global::<FocusState>(), &b, ui.get_page());
        }
    });
    state.on_chip_dropped({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move |index| {
            let target = browser.borrow().focus.chips.get(index as usize).cloned();
            let Some((path, value)) = target else { return };
            edit(&weak.unwrap(), &browser, Change::Value, |focus, infos| {
                match value {
                    Some(value) => focus.toggle(&path, &value),
                    None => clear_condition(focus, &path, infos),
                };
            })
        }
    });
    state.on_favourites_toggled({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move || {
            edit(&weak.unwrap(), &browser, Change::Value, |focus, _| {
                focus.toggle_favourites()
            })
        }
    });
    state.on_cleared({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move || restore_defaults(&weak.unwrap(), &browser)
    });
    state.on_smart_playlist_requested({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move || request_smart_playlist(&weak.unwrap(), &browser)
    });
    state.on_saved_applied({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move |at| apply_saved(&weak.unwrap(), &browser, at as usize)
    });
    state.on_saved_removed({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move |at| {
            let ui = weak.unwrap();
            if let Some(page_key) = key(ui.get_page()) {
                let mut b = browser.borrow_mut();
                if let Some(list) = b.focus.saved.get_mut(page_key)
                    && (at as usize) < list.len()
                {
                    list.remove(at as usize);
                }
                keep(&mut b);
            }
            show(&ui, &browser);
        }
    });
    state.on_saved_saved({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move |name| save_filters(&weak.unwrap(), &browser, name.trim())
    });
    state.on_defaults_restored({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move || restore_defaults(&weak.unwrap(), &browser)
    });
    state.on_listed({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move |row| {
            let ui = weak.unwrap();
            browser.borrow_mut().focus.listing = usize::try_from(row).ok();
            ui.global::<FocusState>().set_query("".into());
            show(&ui, &browser);
        }
    });
    state.on_list_searched({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move || show(&weak.unwrap(), &browser)
    });
    state.on_list_closed({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move || {
            let ui = weak.unwrap();
            browser.borrow_mut().focus.listing = None;
            ui.global::<FocusState>().set_query("".into());
            show(&ui, &browser);
        }
    });
    state.on_step_opened({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move |row| {
            browser.borrow_mut().focus.editing = usize::try_from(row).ok();
            show(&weak.unwrap(), &browser);
        }
    });
    state.on_step_closed({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move || {
            let ui = weak.unwrap();
            {
                let mut b = browser.borrow_mut();
                b.focus.editing = None;
                b.focus.listing = None;
            }
            ui.global::<FocusState>().set_query("".into());
            show(&ui, &browser);
        }
    });
}

/// Back to the filters the page opens with, none of them holding a value.
/// Clear all does this too: the panel stays as useful as it first was.
fn restore_defaults(ui: &AppWindow, browser: &Shared) {
    let page = ui.get_page();
    edit(ui, browser, Change::Set, |focus, infos| {
        *focus = Focus::of(defaults_of(page), infos);
    });
}

/// Has the filter take `operator`, keeping what was typed where it still
/// reads, and the values picked where the new operator picks them too.
fn set_operator(condition: &mut Condition, kind: Kind, operator: Operator) {
    let keeps_picks = say::picks_values(condition.operator) && say::picks_values(operator);
    condition.operator = operator;
    if kind == Kind::Flag {
        return;
    }
    if keeps_picks {
        return;
    }
    if say::picks_values(operator) || kind == Kind::Values {
        condition.value = None;
        condition.typed = None;
        return;
    }
    let boxes = say::typed(kind, condition);
    condition.value = say::read(kind, operator, &boxes);
    condition.typed = Some(boxes);
}

fn clear_condition(focus: &mut Focus, path: &[usize], infos: &[Info]) {
    let Some(condition) = focus.condition_mut(path) else {
        return;
    };
    condition.value = None;
    condition.typed = None;
    if let Some(info) = info_of(infos, &condition.field) {
        condition.operator = info.kind.operators()[0];
    }
}

/// The plays of the page's items, for the steps of the most played.
fn presets_plays(b: &Browser, page: Page) -> Vec<i64> {
    let Some(items) = surface(b, page) else {
        return Vec::new();
    };
    now_of(b, |now| items.plays(now))
}

/// Has the filter say what the preset is: the top of the most played, or the
/// time back that things were added in.
fn apply_preset(condition: &mut Condition, label: &str, plays: &[i64]) {
    condition.typed = None;
    if let Some(threshold) = top_threshold(plays, label) {
        condition.operator = Operator::GreaterThan;
        condition.value = Some(Value::Number(threshold - 1));
    } else if let Some((_, span)) = model::recent_steps()
        .into_iter()
        .find(|(name, _)| *name == label)
    {
        condition.operator = Operator::InTheLast;
        condition.value = Some(Value::Within(span));
    }
}

/// The panel or the sheet opened or closed. A panel's state is kept; a sheet
/// starts at its list of filters.
fn change_visibility(ui: &AppWindow, browser: &Shared, open: bool, sheet: bool) {
    {
        let mut b = browser.borrow_mut();
        b.focus.listing = None;
        b.focus.editing = None;
        if !sheet && let Some(page_key) = key(ui.get_page()) {
            b.focus.pages.entry(page_key.to_owned()).or_default().open = open;
            keep(&mut b);
        }
    }
    ui.global::<FocusState>().set_query("".into());
    show(ui, browser);
}

/// Names the filters on the page on screen, which are kept with their values.
fn save_filters(ui: &AppWindow, browser: &Shared, name: &str) {
    let page = ui.get_page();
    if name.is_empty() {
        return;
    }
    {
        let mut b = browser.borrow_mut();
        let (Some(page_key), Some(focus)) = (key(page), b.focus.focus(page).cloned()) else {
            return;
        };
        let name: String = name.chars().take(LONGEST_NAME).collect();
        let list = b.focus.saved.entry(page_key.to_owned()).or_default();
        list.retain(|saved| saved.name != name);
        list.push(prefs::SavedFilter {
            name,
            filters: focus,
        });
        keep(&mut b);
    }
    show(ui, browser);
}

fn apply_saved(ui: &AppWindow, browser: &Shared, at: usize) {
    let page = ui.get_page();
    let saved = key(page).and_then(|page_key| {
        let b = browser.borrow();
        b.focus
            .saved
            .get(page_key)?
            .get(at)
            .map(|saved| saved.filters.clone())
    });
    let Some(saved) = saved else { return };
    edit(ui, browser, Change::Set, |focus, infos| {
        *focus = saved;
        focus.retain_known(infos);
    });
}

/// Opens the tracks page with its Focus set to a smart playlist's rules, to
/// look at the songs they pick and change the rules there.
pub(super) fn show_rules(ui: &AppWindow, browser: &Shared, rules: &Group<crate::smart::Rule>) {
    {
        let mut b = browser.borrow_mut();
        b.focus
            .focuses
            .insert("tracks", crate::smart::from_focus::into_focus(rules));
        b.focus.pages.entry("tracks".to_owned()).or_default().open = true;
        b.focus.counted = None;
    }
    super::open(ui, browser, "tracks".into());
}

/// Opens the smart playlist editor with the filters as its rules, or says
/// which filter has none.
fn request_smart_playlist(ui: &AppWindow, browser: &Shared) {
    let page = ui.get_page();
    let rules = {
        let b = browser.borrow();
        let Some(focus) = b.focus.focus(page) else {
            return;
        };
        rules_of(focus)
    };
    match rules {
        Ok(rules) => super::smart::open_for_focus(ui, browser, rules),
        Err(why) => super::notify(ui, browser, why),
    }
}

/// The rules the tracks page's filters are, where they can be said in rules.
fn rules_of(focus: &Focus) -> Result<Group<crate::smart::Rule>, String> {
    let infos = infos_of(Page::Tracks);
    let title_of = |field: &str| {
        info_of(&infos, field).map_or_else(|| field.to_owned(), |info| info.title.to_owned())
    };
    crate::smart::from_focus::from_focus(focus, &title_of)
}

/// Shows the filters of the page on screen as the Focus: the active ones as
/// chips, and where the panel or the sheet is open, its rows, with counts
/// made first if the page's items or filters changed since.
pub(super) fn show(ui: &AppWindow, browser: &Shared) {
    let page = ui.get_page();
    let state = ui.global::<FocusState>();
    let Some(page_key) = key(page) else {
        return state.set_offered(false);
    };
    let mut b = browser.borrow_mut();
    if b.focus.last_page != Some(page) {
        b.focus.last_page = Some(page);
        b.focus.listing = None;
        b.focus.editing = None;
        b.focus.menu_query.clear();
        state.set_sheet(false);
        state.set_query("".into());
        state.set_menu_query("".into());
    }
    let Some(focus) = b.focus.focus_mut(page).cloned() else {
        return;
    };
    let infos = infos_of(page);
    let open = b.focus.pages.get(page_key).is_some_and(|saved| saved.open);
    // Without the plays of the compositions there is nothing to count on the
    // composers' page.
    state.set_offered(page != Page::Composers || counts_composition_plays(&b));
    state.set_heart(info_of(&infos, model::FAVOURITE).is_some());
    state.set_open(open);
    state.set_favourites(focus.favourites());
    state.set_narrowing(focus.is_narrowing());
    state.set_no_filters(focus.filters.leaves().is_empty());
    state.set_noun(nouns(page).1.into());
    show_smart_offer(&state, &b, page, &focus);
    show_chips(&state, &mut b, &focus, &infos);
    show_saved(&state, &b, page);
    if b.focus.typing || !(open || state.get_sheet()) {
        return;
    }
    count_if_stale(&mut b, page, &focus);
    publish(&state, &b, page, &focus, &infos);
}

fn show_smart_offer(state: &FocusState<'_>, b: &Browser, page: Page, focus: &Focus) {
    let can_make = page == Page::Tracks && super::smart::offered(b) && focus.is_narrowing();
    let reason = match (can_make, rules_of(focus)) {
        (true, Err(why)) => why,
        _ => String::new(),
    };
    state.set_smart_offered(can_make && reason.is_empty());
    state.set_smart_reason(reason.into());
}

fn count_if_stale(b: &mut Browser, page: Page, focus: &Focus) {
    let Some(items) = surface(b, page) else {
        return;
    };
    let (counts, made) = now_of(b, |now| {
        let made = Made {
            page,
            items: items.len(),
            albums: b.library.len(),
            hearted: items.hearted(now),
        };
        (b.focus.counted.as_ref() != Some(&made)).then(|| (items.count(focus, now), made))
    })
    .unzip();
    drop(items);
    if let Some((counts, made)) = counts.zip(made) {
        b.focus.counts = counts;
        b.focus.counted = Some(made);
    }
}

/// What the rows are made from, apart from the rows themselves.
struct Setting<'a> {
    page: Page,
    infos: &'a [Info],
    counts: &'a Counted,
    top_steps: Vec<&'static str>,
}

/// Gives the panel its rows in bands, the menu and the saved filters.
fn publish(state: &FocusState<'_>, b: &Browser, page: Page, focus: &Focus, infos: &[Info]) {
    let items = surface(b, page).map_or(0, |items| items.len());
    let setting = Setting {
        page,
        infos,
        counts: &b.focus.counts,
        top_steps: top_labels(items),
    };
    let mut walked = Vec::new();
    walk(
        &setting,
        &focus.filters,
        &mut Vec::new(),
        &mut 0,
        &mut walked,
    );
    let (rows, leaf_at): (Vec<FocusRow>, Vec<Option<usize>>) = walked.into_iter().unzip();
    let listing = b.focus.listing.or_else(|| {
        let at = b.focus.editing?;
        (rows.get(at)?.kind == VALUES_ROW).then_some(at)
    });
    let listed = listing.and_then(|at| rows.get(at));
    state.set_list_title(listed.map(|row| row.title.clone()).unwrap_or_default());
    let list = listing.and_then(|at| {
        let leaf = (*leaf_at.get(at)?)?;
        let condition = focus.filters.leaves().get(leaf).copied()?;
        let Tally::Values(values) = b.focus.counts.get(leaf)? else {
            return None;
        };
        let query = state.get_query();
        Some(values_listed(values, condition, query.trim(), nouns(page)))
    });
    state.set_list(ModelRc::new(VecModel::from(list.unwrap_or_default())));
    let editing = b.focus.editing.and_then(|at| rows.get(at).cloned());
    state.set_editing_open(editing.is_some());
    state.set_editing(editing.unwrap_or_default());
    state.set_bands(ModelRc::new(VecModel::from(bands_of(rows))));
    show_menu(state, b, page);
}

/// Lists the saved filters of the page. The closed bar offers them too, so
/// this does not wait for the panel to open.
fn show_saved(state: &FocusState<'_>, b: &Browser, page: Page) {
    let names: Vec<SharedString> = key(page)
        .and_then(|page_key| b.focus.saved.get(page_key))
        .map(|list| {
            list.iter()
                .map(|saved| saved.name.as_str().into())
                .collect()
        })
        .unwrap_or_default();
    state.set_saved(ModelRc::new(VecModel::from(names)));
}

/// The rows of a group and what is in it, each with the place of its
/// filter's counts, in the order the focus lists them.
fn walk(
    setting: &Setting,
    group: &Group<Condition>,
    path: &mut Vec<usize>,
    leaf: &mut usize,
    rows: &mut Vec<(FocusRow, Option<usize>)>,
) {
    rows.push((group_row(rows.len(), path.len(), group), None));
    for (at, entry) in group.rules.iter().enumerate() {
        path.push(at);
        match entry {
            Entry::Group(inner) => walk(setting, inner, path, leaf, rows),
            Entry::Leaf(condition) => {
                let tally = setting.counts.get(*leaf);
                let row = filter_row(setting, rows.len(), path.len() - 1, condition, tally);
                rows.push((row, Some(*leaf)));
                *leaf += 1;
            }
        }
        path.pop();
    }
}

fn group_row(index: usize, depth: usize, group: &Group<Condition>) -> FocusRow {
    let which = if group.match_all { "all" } else { "any" };
    FocusRow {
        index: index as i32,
        kind: GROUP_ROW,
        depth: depth as i32,
        spoken: format!("Match {which} of these").into(),
        match_all: group.match_all,
        can_nest: depth + 2 <= MOST_DEPTH,
        removable: depth > 0,
        enabled: true,
        invalid: depth > 0 && group.rules.is_empty() && !group.match_all,
        ..FocusRow::default()
    }
}

fn row_kind(kind: Kind, operator: Operator) -> i32 {
    match kind {
        Kind::Values if say::picks_values(operator) => VALUES_ROW,
        Kind::Flag => FLAG_ROW,
        Kind::Number(_) | Kind::Date => BOXES_ROW,
        Kind::Text | Kind::Values => TEXT_ROW,
    }
}

fn filter_row(
    setting: &Setting,
    index: usize,
    depth: usize,
    condition: &Condition,
    tally: Option<&Tally>,
) -> FocusRow {
    let Some(info) = info_of(setting.infos, &condition.field) else {
        return FocusRow {
            index: index as i32,
            kind: TEXT_ROW,
            depth: depth as i32,
            title: condition.field.as_str().into(),
            enabled: condition.enabled,
            ..FocusRow::default()
        };
    };
    let (kind, operator) = (info.kind, condition.operator);
    let row_kind = row_kind(kind, operator);
    let boxes = say::typed(kind, condition);
    let reads = say::read(kind, operator, &boxes).is_some();
    let has_typed = boxes.iter().any(|text| !text.trim().is_empty());
    let off = if condition.enabled { "" } else { ", off" };
    let labels: Vec<SharedString> = (kind.operators().iter())
        .map(|&operator| operator_label(operator).into())
        .collect();
    let at_operator = kind.operators().iter().position(|&o| o == operator);
    FocusRow {
        index: index as i32,
        kind: row_kind,
        depth: depth as i32,
        title: info.title.into(),
        spoken: format!("{}{off}", say::describe(info.title, kind, condition)).into(),
        enabled: condition.enabled,
        operators: ModelRc::new(VecModel::from(labels)),
        operator: at_operator.unwrap_or(0) as i32,
        takes_value: say::takes_value(operator),
        two_boxes: say::is_range(operator),
        hint_first: say::hint(kind, operator, false).into(),
        hint_second: say::hint(kind, operator, true).into(),
        invalid: row_kind != VALUES_ROW
            && row_kind != FLAG_ROW
            && say::takes_value(operator)
            && has_typed
            && !reads,
        first: boxes[0].as_str().into(),
        second: boxes[1].as_str().into(),
        flag: match condition.value {
            Some(Value::Flag(true)) => YES,
            Some(Value::Flag(false)) => NO,
            _ => EITHER,
        },
        values: ModelRc::new(VecModel::from(column_values(setting, condition, tally))),
        hidden: hidden_values(condition, tally) as i32,
        note: range_note(kind, condition, tally).into(),
        presets: ModelRc::new(VecModel::from(presets_for(setting, info))),
        ..FocusRow::default()
    }
}

/// Whether a value is in its column: the most frequent are, and those picked.
fn is_in_column(rank: usize, count: &model::Count, condition: &Condition) -> bool {
    rank < VALUES_IN_COLUMN || condition.picked().contains(&count.label)
}

/// The values a column lists: the most frequent and those picked.
fn column_values(
    setting: &Setting,
    condition: &Condition,
    tally: Option<&Tally>,
) -> Vec<FocusValue> {
    let kind = info_of(setting.infos, &condition.field).map(|info| info.kind);
    let lists = kind.is_some_and(|kind| row_kind(kind, condition.operator) == VALUES_ROW);
    let Some(Tally::Values(values)) = tally.filter(|_| lists) else {
        return Vec::new();
    };
    let nouns = nouns(setting.page);
    (values.iter().enumerate())
        .filter(|&(rank, count)| is_in_column(rank, count, condition))
        .map(|(_, count)| value(count, condition, nouns))
        .collect()
}

fn hidden_values(condition: &Condition, tally: Option<&Tally>) -> usize {
    let Some(Tally::Values(values)) = tally else {
        return 0;
    };
    let shown = (values.iter().enumerate())
        .filter(|&(rank, count)| is_in_column(rank, count, condition))
        .count();
    values.len() - shown
}

/// The values of a list, or those of it that match a search.
fn values_listed(
    values: &[model::Count],
    condition: &Condition,
    query: &str,
    nouns: (&str, &str),
) -> Vec<FocusValue> {
    filtered(values, query, |count| count.label.clone())
        .into_iter()
        .map(|at| value(&values[at], condition, nouns))
        .collect()
}

/// A value, said as a screen reader should: "Blues, 198 albums, selected".
fn value(count: &model::Count, condition: &Condition, nouns: (&str, &str)) -> FocusValue {
    let picked = condition.picked().contains(&count.label);
    let selected = if picked { ", selected" } else { "" };
    FocusValue {
        label: count.label.as_str().into(),
        count: count.count as i32,
        picked,
        spoken: format!(
            "{}, {}{selected}",
            count.label,
            super::count(count.count, nouns.0, nouns.1)
        )
        .into(),
    }
}

/// "212 of 3,418" while the filter narrows, and the range the library has.
fn range_note(kind: Kind, condition: &Condition, tally: Option<&Tally>) -> String {
    let Some(&Tally::Range { left, of, bounds }) = tally else {
        return String::new();
    };
    let show = |at: i64| match kind {
        Kind::Number(measure) => measure.show(at),
        _ => crate::rules::iso_date(at),
    };
    let mut parts = Vec::new();
    if condition.is_active() {
        parts.push(format!("{} of {}", thousands(left), thousands(of)));
    }
    if let Some((least, most)) = bounds {
        parts.push(format!("Library: {} to {}", show(least), show(most)));
    }
    parts.join(" \u{b7} ")
}

fn thousands(number: usize) -> String {
    let digits = number.to_string();
    let mut grouped = String::new();
    for (at, digit) in digits.chars().enumerate() {
        if at > 0 && (digits.len() - at).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}

/// The steps that set a filter in one press: the most played of the plays,
/// the time back of when things were added.
fn presets_for(setting: &Setting, info: &Info) -> Vec<SharedString> {
    match (info.key, info.kind) {
        ("playCount", Kind::Number(_)) => (setting.top_steps.iter())
            .map(|&step| step.into())
            .collect(),
        ("added", Kind::Date) => (model::recent_steps().into_iter())
            .map(|(label, _)| label.into())
            .collect(),
        _ => Vec::new(),
    }
}

/// The rows in bands: values filters side by side as columns, any other
/// row on its own.
fn bands_of(rows: Vec<FocusRow>) -> Vec<FocusBand> {
    let mut bands: Vec<(bool, Vec<FocusRow>)> = Vec::new();
    for row in rows {
        let strip = row.kind == VALUES_ROW;
        match bands.last_mut() {
            Some((true, strip_rows)) if strip => strip_rows.push(row),
            _ => bands.push((strip, vec![row])),
        }
    }
    (bands.into_iter())
        .map(|(strip, rows)| FocusBand {
            strip,
            depth: rows[0].depth,
            lines: (rows.iter())
                .map(|row| row.values.row_count() + usize::from(row.hidden > 0))
                .max()
                .unwrap_or(0) as i32,
            rows: ModelRc::new(VecModel::from(rows)),
        })
        .collect()
}

/// The fields the "Add filter" menu offers that the search leaves, under
/// their headings.
fn show_menu(state: &FocusState<'_>, b: &Browser, page: Page) {
    let infos = infos_of(page);
    let found = filtered(&infos, &b.focus.menu_query, |info| {
        format!("{} {}", info.title, info.group)
    });
    let mut entries = Vec::new();
    for group in GROUPS {
        let in_group: Vec<usize> = (found.iter().copied())
            .filter(|&at| infos[at].group == group)
            .collect();
        if in_group.is_empty() {
            continue;
        }
        entries.push(FocusMenuEntry {
            title: group.into(),
            header: true,
            index: -1,
        });
        entries.extend(in_group.into_iter().map(|at| FocusMenuEntry {
            title: infos[at].title.into(),
            header: false,
            index: at as i32,
        }));
    }
    state.set_menu(ModelRc::new(VecModel::from(entries)));
}

/// What the active filters are, as chips for the closed panel to show.
fn show_chips(state: &FocusState<'_>, b: &mut Browser, focus: &Focus, infos: &[Info]) {
    let mut chips = Vec::new();
    collect_chips(&focus.filters, &mut Vec::new(), infos, &mut chips);
    let texts: Vec<SharedString> = (chips.iter())
        .map(|(text, _)| SharedString::from(text.as_str()))
        .collect();
    state.set_chips(ModelRc::new(VecModel::from(texts)));
    b.focus.chips = chips.into_iter().map(|(_, target)| target).collect();
}

type ChipTarget = (Vec<usize>, Option<String>);

fn collect_chips(
    group: &Group<Condition>,
    path: &mut Vec<usize>,
    infos: &[Info],
    chips: &mut Vec<(String, ChipTarget)>,
) {
    for (at, entry) in group.rules.iter().enumerate() {
        path.push(at);
        match entry {
            Entry::Group(inner) => collect_chips(inner, path, infos, chips),
            Entry::Leaf(condition) if is_chipped(condition) => {
                if let Some(info) = info_of(infos, &condition.field) {
                    chips_of(info, condition, path, chips);
                }
            }
            Entry::Leaf(_) => {}
        }
        path.pop();
    }
}

/// The heart shows the favourites, so they need no chip.
fn is_chipped(condition: &Condition) -> bool {
    condition.is_active() && condition.field != model::FAVOURITE
}

fn chips_of(
    info: &Info,
    condition: &Condition,
    path: &[usize],
    chips: &mut Vec<(String, ChipTarget)>,
) {
    if condition.picked().is_empty() {
        let text = say::describe(info.title, info.kind, condition);
        chips.push((text, (path.to_vec(), None)));
    }
    for picked in condition.picked() {
        let text = say::pick_chip(condition.operator, picked);
        chips.push((text, (path.to_vec(), Some(picked.clone()))));
    }
}

/// The genres of each artist's albums, and how often they were played.
fn artist_facts(b: &Browser) -> Vec<ArtistFacts> {
    let mut by_link: HashMap<String, ArtistFacts> = HashMap::new();
    for album in &b.library {
        let genres = album_genres(album);
        for name in album_names(album) {
            let facts = by_link.entry(name.link.into()).or_default();
            facts.plays += album.play_count.unwrap_or(0);
            for genre in &genres {
                if !facts.genres.contains(genre) {
                    facts.genres.push(genre.clone());
                }
            }
        }
    }
    (b.artists.iter())
        .map(|artist: &ArtistID3| ArtistFacts {
            name: artist.name.clone(),
            albums: artist.album_count,
            music_brainz_id: artist.music_brainz_id.clone(),
            starred: artist.starred.is_some(),
            ..by_link
                .remove(&format!("artist:{}", artist.id))
                .unwrap_or_default()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn album(id: &str, genre: &str, starred: bool) -> AlbumID3 {
        serde_json::from_value(serde_json::json!({
            "id": id, "name": id, "songCount": 1, "duration": 1,
            "created": "2021-01-01T00:00:00Z", "genre": genre,
            "starred": starred.then_some("2021-01-01T00:00:00Z"),
        }))
        .unwrap()
    }

    fn browser_with_albums() -> Browser {
        Browser {
            library: vec![
                album("a", "Blues", false),
                album("b", "Jazz", true),
                album("c", "Blues", true),
            ],
            ..Browser::default()
        }
    }

    /// The albums' first default filter is the genres.
    fn pick_blues(b: &mut Browser) {
        let focus = b.focus.focus_mut(Page::Albums).unwrap();
        focus.toggle(&[0], "Blues");
    }

    #[test]
    fn a_pick_leaves_the_albums_of_the_view_that_have_it() {
        let mut b = browser_with_albums();
        assert_eq!(narrow(&b, Page::Albums, vec![2, 0, 1]), [2, 0, 1]);
        pick_blues(&mut b);
        assert_eq!(narrow(&b, Page::Albums, vec![2, 0, 1]), [2, 0]);
        b.focus.focus_mut(Page::Albums).unwrap().toggle_favourites();
        assert_eq!(narrow(&b, Page::Albums, vec![2, 0, 1]), [2]);
    }

    #[test]
    fn the_picks_of_one_page_do_not_narrow_another() {
        let mut b = browser_with_albums();
        pick_blues(&mut b);
        assert_eq!(narrow(&b, Page::Genre, vec![0, 1, 2]), [0, 1, 2]);
        assert_eq!(narrow(&b, Page::Tracks, vec![0, 1, 2]), [0, 1, 2]);
    }

    #[test]
    fn the_hearted_count_changes_when_an_album_is_hearted() {
        let mut b = browser_with_albums();
        let hearted = |b: &Browser| {
            let items = surface(b, Page::Albums).unwrap();
            now_of(b, |now| items.hearted(now))
        };
        let before = hearted(&b);
        b.library[0].starred = Some("2021-01-01T00:00:00Z".into());
        assert_eq!(hearted(&b), before + 1);
    }

    #[test]
    fn a_pages_saved_filters_come_back_without_their_values() {
        let mut b = browser_with_albums();
        pick_blues(&mut b);
        remember(&mut b);
        let kept = b.focus.pages["albums"].filters.clone().unwrap();
        assert!(!kept.is_narrowing());
        assert_eq!(kept.filters.leaf_count(), defaults_of(Page::Albums).len());
    }

    #[test]
    fn a_chip_says_when_the_picks_are_all_or_none_of_them() {
        let infos = infos_of(Page::Albums);
        let info = info_of(&infos, "genre").unwrap();
        let mut genres = Condition::new(info);
        genres.value = Some(Value::List(vec!["Blues".into(), "Jazz".into()]));
        let mut texts = |operator| {
            genres.operator = operator;
            let mut chips = Vec::new();
            chips_of(info, &genres, &[0], &mut chips);
            chips.into_iter().map(|(text, _)| text).collect::<Vec<_>>()
        };
        assert_eq!(texts(Operator::AnyOf), ["Blues", "Jazz"]);
        assert_eq!(texts(Operator::NoneOf), ["none of Blues", "none of Jazz"]);
        assert_eq!(texts(Operator::AllOf), ["all of Blues", "all of Jazz"]);
    }

    #[test]
    fn what_a_page_keeps_holds_the_operators_and_groups_as_they_were_set() {
        let mut b = browser_with_albums();
        let genres = Condition::new(info_of(&infos_of(Page::Albums), "genre").unwrap());
        let focus = b.focus.focus_mut(Page::Albums).unwrap();
        let nested = focus.filters.rules.len();
        focus.condition_mut(&[0]).unwrap().operator = Operator::AllOf;
        focus.add_group(&[], false);
        focus.add(&[nested], genres);
        focus.condition_mut(&[nested, 0]).unwrap().operator = Operator::NoneOf;
        focus.toggle(&[nested, 0], "Blues");
        remember(&mut b);
        let mut kept = b.focus.pages["albums"].filters.clone().unwrap();
        assert_eq!(kept.condition_mut(&[0]).unwrap().operator, Operator::AllOf);
        assert_eq!(
            kept.condition_mut(&[nested, 0]).unwrap().operator,
            Operator::NoneOf
        );
        assert!(!kept.filters.group_at_mut(&[nested]).unwrap().match_all);
        assert!(!kept.is_narrowing(), "the values are not kept");
    }
}
