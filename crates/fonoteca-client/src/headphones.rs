//! The headphone presets page: OPRA's catalog is unzipped and parsed once,
//! off the interface's thread, when the page is first opened; typing then
//! filters its headphones and a preset picked is handed to `load`.

use std::cell::RefCell;
use std::rc::Rc;

use slint::{ComponentHandle, ModelRc, VecModel};

use crate::opra::{self, Catalog, Preset, ProductId};
use crate::prefs::Equalizer;
use crate::{AppWindow, HeadphonePresetLine, HeadphoneRow};

/// How many matches are listed; typing more narrows them.
const MAX_ROWS: usize = 200;

#[derive(Default)]
struct Picker {
    catalog: Option<Rc<Catalog>>,
    loading: bool,
    /// The rows listed, as the products they are.
    listed: Vec<ProductId>,
    picked: Option<ProductId>,
}

/// Shows the page's headphones and presets, and hands the EQ of a preset
/// the user loads to `load`.
pub fn wire(ui: &AppWindow, load: impl Fn(Equalizer) + 'static) {
    let picker: Rc<RefCell<Picker>> = Rc::default();

    ui.on_open_headphones({
        let (weak, picker) = (ui.as_weak(), picker.clone());
        move || {
            if picker.borrow().catalog.is_some() || picker.borrow().loading {
                return;
            }
            picker.borrow_mut().loading = true;
            weak.unwrap()
                .set_headphone_status("Loading the headphone presets…".into());
            let (weak, picker) = (weak.clone(), picker.clone());
            slint::spawn_local(async move {
                let loaded = tokio::task::spawn_blocking(opra::bundled).await;
                let Some(ui) = weak.upgrade() else { return };
                picker.borrow_mut().loading = false;
                match loaded {
                    Ok(Ok(catalog)) => {
                        picker.borrow_mut().catalog = Some(Rc::new(catalog));
                        show_results(&ui, &picker, &ui.get_headphone_filter());
                    }
                    Ok(Err(problem)) => ui.set_headphone_status(unreadable(&problem).into()),
                    Err(problem) => ui.set_headphone_status(unreadable(&problem).into()),
                }
            })
            .expect("no event loop to load the headphone presets in");
        }
    });
    ui.on_search_headphones({
        let (weak, picker) = (ui.as_weak(), picker.clone());
        move |text| show_results(&weak.unwrap(), &picker, &text)
    });
    ui.on_pick_headphone({
        let (weak, picker) = (ui.as_weak(), picker.clone());
        move |row| {
            let ui = weak.unwrap();
            {
                let mut picker = picker.borrow_mut();
                picker.picked = picker.listed.get(row as usize).copied();
            }
            show_picked(&ui, &picker.borrow());
        }
    });
    ui.on_load_headphone({
        let picker = picker.clone();
        move |row| {
            let picker = picker.borrow();
            let (Some(catalog), Some(id)) = (&picker.catalog, picker.picked) else {
                return;
            };
            if let Some(preset) = catalog.product(id).presets.get(row as usize)
                && let Ok(eq) = preset.equalizer()
            {
                load(eq);
            }
        }
    });
}

fn unreadable(problem: &dyn std::fmt::Display) -> String {
    format!("The headphone presets couldn't be read: {problem}")
}

/// Lists the headphones that match `query`, and says how many there are.
/// What was listed stays until the catalog has its answer.
fn show_results(ui: &AppWindow, picker: &RefCell<Picker>, query: &str) {
    let Some(catalog) = picker.borrow().catalog.clone() else {
        return;
    };
    let found = catalog.search(query);
    let rows: Vec<HeadphoneRow> = found
        .iter()
        .take(MAX_ROWS)
        .map(|&id| HeadphoneRow {
            title: format!("{} {}", catalog.vendor(id).name, catalog.product(id).name).into(),
            details: kind(&catalog.product(id).subtype).into(),
        })
        .collect();
    ui.set_headphone_status(note(query, found.len(), &catalog).into());
    ui.set_headphone_results(ModelRc::new(VecModel::from(rows)));
    picker.borrow_mut().listed = found.into_iter().take(MAX_ROWS).collect();
}

fn show_picked(ui: &AppWindow, picker: &Picker) {
    let (Some(catalog), Some(id)) = (&picker.catalog, picker.picked) else {
        return;
    };
    let product = catalog.product(id);
    let lines: Vec<HeadphonePresetLine> = product.presets.iter().map(preset_line).collect();
    ui.set_headphone_picked(format!("{} {}", catalog.vendor(id).name, product.name).into());
    ui.set_headphone_presets(ModelRc::new(VecModel::from(lines)));
}

fn preset_line(preset: &Preset) -> HeadphonePresetLine {
    HeadphonePresetLine {
        author: preset.author.as_str().into(),
        details: preset.details.as_str().into(),
        summary: format!(
            "{} {}, preamp {} dB",
            preset.bands.len(),
            if preset.bands.len() == 1 {
                "band"
            } else {
                "bands"
            },
            preset.gain
        )
        .into(),
        link: preset
            .link
            .as_deref()
            .filter(|link| link.starts_with("https://") || link.starts_with("http://"))
            .unwrap_or_default()
            .into(),
        problem: preset.equalizer().err().unwrap_or_default().into(),
    }
}

/// What is under the search: how many it found, or what to type.
fn note(query: &str, found: usize, catalog: &Catalog) -> String {
    if query.trim().is_empty() {
        return format!(
            "Type a brand or a model: {} headphones from {} brands.",
            catalog.product_count(),
            catalog.vendor_count()
        );
    }
    match found {
        0 => "No headphones match.".to_owned(),
        found if found > MAX_ROWS => {
            format!("Showing the first {MAX_ROWS} of {found}. Type more to narrow them.")
        }
        1 => "1 headphone".to_owned(),
        found => format!("{found} headphones"),
    }
}

fn kind(subtype: &str) -> &str {
    match subtype {
        "in_ear" => "In-ear",
        "over_the_ear" => "Over-ear",
        "on_ear" => "On-ear",
        "earbuds" => "Earbuds",
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_note_says_how_many_matched_or_what_to_type() {
        let catalog = opra::bundled().unwrap();
        assert!(note("", 0, &catalog).starts_with("Type a brand or a model: 6229 headphones"));
        assert_eq!(note("zzzz", 0, &catalog), "No headphones match.");
        assert_eq!(note("a", 1, &catalog), "1 headphone");
        assert_eq!(note("a", 12, &catalog), "12 headphones");
        assert!(note("a", 3_000, &catalog).starts_with("Showing the first 200 of 3000."));
    }

    #[test]
    fn a_link_that_is_not_a_web_address_is_left_out() {
        let preset = |link: &str| Preset {
            author: "A".into(),
            details: "d".into(),
            link: Some(link.into()),
            gain: -2.0,
            bands: vec![("peak_dip".into(), 100.0, Some(1.0), Some(1.0))],
        };
        assert_eq!(
            preset_line(&preset("https://x.org/a.pdf")).link,
            "https://x.org/a.pdf"
        );
        assert_eq!(preset_line(&preset("file:///etc/passwd")).link, "");
        assert_eq!(preset_line(&preset("x")).summary, "1 band, preamp -2 dB");
    }
}
