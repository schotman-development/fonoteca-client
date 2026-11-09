//! The sound processing in the settings: headroom, a parametric EQ with
//! AutoEQ's settings pasted in, crossfeed and upsampling, and presets of
//! them all. They are kept in the preferences and handed to the player as
//! they change.

use std::cell::RefCell;
use std::cmp::Ordering;
use std::rc::Rc;
use std::time::Duration;

use playback::{Band, Crossfeed, Dsp, Equalizer, Filter};
use slint::{ComponentHandle, Model, ModelRc, SharedString, Timer, TimerMode, VecModel};

use crate::prefs::{self, EqBand, Prefs, Sound};
use crate::{AppWindow, EqBandLine, EqGridLine, EqPoint, eq_graph, headphones, sound_presets};

/// The settings' headroom, crossfeed and filter entries, in their order.
const HEADROOM: [f64; 8] = [0.0, -1.0, -2.0, -3.0, -4.5, -6.0, -9.0, -12.0];
const CROSSFEED: [Crossfeed; 4] = [
    Crossfeed::Off,
    Crossfeed::Light,
    Crossfeed::Medium,
    Crossfeed::Strong,
];
const FILTERS: [Filter; 5] = [
    Filter::Peak,
    Filter::LowShelf,
    Filter::HighShelf,
    Filter::LowPass,
    Filter::HighPass,
];
/// A new band: a flat peak in the middle, to shape from there.
const NEW_BAND: EqBand = EqBand {
    filter: 0,
    frequency: 1_000.0,
    gain: 0.0,
    q: 1.0,
};
/// How long typing pauses before what was typed is heard: each change
/// opens the track again where it is.
const SETTLE: Duration = Duration::from_millis(400);
/// The presets picker's first entry, which processes nothing.
const NO_PRESET: &str = "No preset";
/// How often the player gets the EQ while a point is being dragged. Each
/// new EQ opens the track again where it is, so this is how often the sound
/// catches up, not how often the mouse moves.
const DRAG_FOLLOW: Duration = Duration::from_millis(500);

/// The processing the settings ask for.
pub fn dsp() -> Dsp {
    dsp_of(&prefs::load().sound)
}

fn dsp_of(sound: &Sound) -> Dsp {
    Dsp {
        headroom: HEADROOM[sound.headroom.clamp(0, 7) as usize],
        eq: sound.eq_on.then(|| Equalizer {
            preamp: sound.eq.preamp,
            bands: sound.eq.bands.iter().map(band).collect(),
        }),
        crossfeed: CROSSFEED[sound.crossfeed.clamp(0, 3) as usize],
        upsampling: sound.upsampling == 1,
    }
}

fn band(band: &EqBand) -> Band {
    Band {
        filter: FILTERS[band.filter.clamp(0, 4) as usize],
        frequency: band.frequency,
        gain: band.gain,
        q: band.q,
    }
}

/// Shows the settings and has each change saved and handed to `apply`.
pub fn wire(ui: &AppWindow, apply: impl Fn(Dsp) + 'static) {
    show(ui, &prefs::load());
    let settle = Rc::new(Timer::default());
    let apply = Rc::new(apply);
    // Saves `change` to the preferences, and has the player follow once it
    // has settled.
    let changed = {
        let (weak, settle, apply) = (ui.as_weak(), settle.clone(), apply.clone());
        move |change: &dyn Fn(&mut Prefs)| {
            let mut saved = prefs::load();
            change(&mut saved);
            prefs::save(&saved);
            show_presets(&weak.unwrap(), &saved);
            let apply = apply.clone();
            settle.start(TimerMode::SingleShot, SETTLE, move || apply(dsp()));
        }
    };
    let changed = Rc::new(changed);

    ui.on_choose_headroom({
        let changed = changed.clone();
        move |index| changed(&|saved| saved.sound.headroom = index)
    });
    ui.on_choose_crossfeed({
        let changed = changed.clone();
        move |index| changed(&|saved| saved.sound.crossfeed = index)
    });
    ui.on_choose_upsampling({
        let changed = changed.clone();
        move |index| changed(&|saved| saved.sound.upsampling = index)
    });
    ui.on_toggle_eq({
        let changed = changed.clone();
        move |on| changed(&|saved| saved.sound.eq_on = on)
    });
    ui.on_edit_preamp({
        let changed = changed.clone();
        move |text| {
            let Some(preamp) = number(&text) else { return };
            changed(&|saved| saved.sound.eq.preamp = preamp);
        }
    });
    ui.on_edit_band({
        let (weak, changed) = (ui.as_weak(), changed.clone());
        move |row, line| {
            // The row's model holds what is typed, even half typed, so a move
            // on the graph always differs from it and reaches the fields.
            weak.unwrap()
                .get_eq_bands()
                .set_row_data(row as usize, line.clone());
            let (Some(frequency), Some(gain), Some(q)) =
                (number(&line.frequency), number(&line.gain), number(&line.q))
            else {
                return;
            };
            let edited = EqBand {
                filter: line.filter,
                frequency,
                gain,
                q,
            };
            changed(&|saved| {
                if let Some(band) = saved.sound.eq.bands.get_mut(row as usize) {
                    *band = edited.clone();
                }
            });
            show_graph(&weak.unwrap(), &prefs::load().sound.eq);
        }
    });
    ui.on_add_band({
        let (weak, changed) = (ui.as_weak(), changed.clone());
        move || {
            changed(&|saved| saved.sound.eq.bands.push(NEW_BAND));
            show(&weak.unwrap(), &prefs::load());
        }
    });
    ui.on_remove_band({
        let (weak, changed) = (ui.as_weak(), changed.clone());
        move |row| {
            changed(&|saved| {
                if (row as usize) < saved.sound.eq.bands.len() {
                    saved.sound.eq.bands.remove(row as usize);
                }
            });
            let ui = weak.unwrap();
            // The band picked stays picked, a place up when one before it
            // went; the one that went leaves none picked.
            let picked = ui.get_eq_selected();
            ui.set_eq_selected(match picked.cmp(&row) {
                Ordering::Less => picked,
                Ordering::Equal => -1,
                Ordering::Greater => picked - 1,
            });
            show(&ui, &prefs::load());
        }
    });
    wire_presets(ui, apply.clone());
    // Makes `eq` the EQ, and turns the EQ on to hear it.
    let load_eq = {
        let (weak, changed) = (ui.as_weak(), changed.clone());
        Rc::new(move |eq: prefs::Equalizer| {
            changed(&|saved| {
                saved.sound.eq = eq.clone();
                saved.sound.eq_on = true;
            });
            show(&weak.unwrap(), &prefs::load());
        })
    };
    ui.on_import_eq({
        let load_eq = load_eq.clone();
        move |text| match parametric(&text) {
            Ok(eq) => {
                load_eq(eq);
                SharedString::new()
            }
            Err(problem) => problem.into(),
        }
    });
    headphones::wire(ui, {
        let weak = ui.as_weak();
        move |eq| {
            load_eq(eq);
            // Back to the EQ, where the preset shows on the graph.
            weak.unwrap().invoke_back();
        }
    });
    wire_graph(ui, changed, apply);
}

/// The presets' picker and menu: a preset chosen becomes the settings at
/// once, and the settings are saved over the one they were set from, as a
/// new one, renamed or deleted.
fn wire_presets(ui: &AppWindow, apply: Rc<impl Fn(Dsp) + 'static>) {
    ui.on_choose_sound_preset({
        let weak = ui.as_weak();
        move |index| {
            let mut saved = prefs::load();
            // The first entry is no preset.
            let name = (index as usize)
                .checked_sub(1)
                .and_then(|index| saved.sound_presets.get(index))
                .map(|preset| preset.name.clone());
            sound_presets::choose(&mut saved, name.as_deref());
            prefs::save(&saved);
            show(&weak.unwrap(), &saved);
            apply(dsp_of(&saved.sound));
        }
    });
    // Saves what `change` does to the presets, and shows it.
    let presets_changed = {
        let weak = ui.as_weak();
        Rc::new(move |change: &dyn Fn(&mut Prefs)| {
            let mut saved = prefs::load();
            change(&mut saved);
            prefs::save(&saved);
            show_presets(&weak.unwrap(), &saved);
        })
    };
    ui.on_save_sound_preset({
        let presets_changed = presets_changed.clone();
        move || {
            presets_changed(&|saved| {
                if let Some(name) = saved.sound_preset.clone() {
                    sound_presets::save_as(saved, &name);
                }
            })
        }
    });
    ui.on_save_sound_preset_as({
        let presets_changed = presets_changed.clone();
        move |name| presets_changed(&|saved| sound_presets::save_as(saved, &name))
    });
    ui.on_rename_sound_preset({
        let weak = ui.as_weak();
        move |name| {
            let mut saved = prefs::load();
            if let Err(problem) = sound_presets::rename(&mut saved, &name) {
                return problem.into();
            }
            prefs::save(&saved);
            show_presets(&weak.unwrap(), &saved);
            SharedString::new()
        }
    });
    ui.on_delete_sound_preset(move || presets_changed(&sound_presets::delete));
}

/// The EQ's graph: a click adds a band, and a point dragged moves its band.
/// The drag is kept apart from the saved preferences until it ends, so the
/// disk is written once, and the player follows every `DRAG_FOLLOW` instead
/// of at each move.
fn wire_graph(
    ui: &AppWindow,
    changed: Rc<impl Fn(&dyn Fn(&mut Prefs)) + 'static>,
    apply: Rc<impl Fn(Dsp) + 'static>,
) {
    ui.set_eq_grid(ModelRc::new(VecModel::from(
        eq_graph::grid()
            .into_iter()
            .map(|line| EqGridLine {
                vertical: line.vertical,
                position: line.position as f32,
                label: line.label.into(),
            })
            .collect::<Vec<_>>(),
    )));
    let drag: Rc<RefCell<Option<Sound>>> = Rc::default();
    let follow = Rc::new(Timer::default());

    ui.on_add_band_at({
        let (weak, changed) = (ui.as_weak(), changed.clone());
        move |x, y| {
            let moved = eq_graph::moved(&band(&NEW_BAND), x.into(), y.into());
            let new = EqBand {
                frequency: moved.frequency,
                gain: moved.gain,
                ..NEW_BAND
            };
            changed(&|saved| saved.sound.eq.bands.push(new.clone()));
            let (ui, saved) = (weak.unwrap(), prefs::load());
            show(&ui, &saved);
            ui.set_eq_selected(saved.sound.eq.bands.len() as i32 - 1);
        }
    });
    ui.on_move_band({
        let (weak, drag, follow) = (ui.as_weak(), drag.clone(), follow.clone());
        move |row, x, y| {
            let mut dragged = drag.borrow_mut();
            let editing = dragged.get_or_insert_with(|| prefs::load().sound);
            let Some(saved) = editing.eq.bands.get_mut(row as usize) else {
                return;
            };
            let moved = eq_graph::moved(&band(saved), x.into(), y.into());
            saved.frequency = moved.frequency;
            saved.gain = moved.gain;
            let ui = weak.unwrap();
            show_band(&ui, row as usize, saved);
            show_graph(&ui, &editing.eq);
            if !follow.running() {
                let (drag, apply) = (drag.clone(), apply.clone());
                follow.start(TimerMode::SingleShot, DRAG_FOLLOW, move || {
                    if let Some(editing) = drag.borrow().as_ref() {
                        apply(dsp_of(editing));
                    }
                });
            }
        }
    });
    ui.on_release_band(move || {
        follow.stop();
        let Some(editing) = drag.borrow_mut().take() else {
            return;
        };
        changed(&|saved| saved.sound.eq = editing.eq.clone());
    });
}

/// Puts the saved processing in the settings.
fn show(ui: &AppWindow, saved: &Prefs) {
    let sound = &saved.sound;
    ui.set_headroom(sound.headroom.clamp(0, 7));
    ui.set_crossfeed(sound.crossfeed.clamp(0, 3));
    ui.set_upsampling(sound.upsampling.clamp(0, 1));
    ui.set_eq_on(sound.eq_on);
    ui.set_eq_preamp(decimal(sound.eq.preamp).into());
    let lines: Vec<EqBandLine> = sound.eq.bands.iter().map(band_line).collect();
    ui.set_eq_bands(ModelRc::new(VecModel::from(lines)));
    if ui.get_eq_selected() >= sound.eq.bands.len() as i32 {
        ui.set_eq_selected(-1);
    }
    show_graph(ui, &sound.eq);
    show_presets(ui, saved);
}

/// Lists the presets after no preset, with the one the settings were set
/// from picked, and says whether they changed since.
fn show_presets(ui: &AppWindow, saved: &Prefs) {
    let mut names = vec![SharedString::from(NO_PRESET)];
    names.extend(
        saved
            .sound_presets
            .iter()
            .map(|preset| preset.name.as_str().into()),
    );
    ui.set_sound_presets(ModelRc::new(VecModel::from(names)));
    let picked = saved.sound_preset.as_deref().and_then(|name| {
        saved
            .sound_presets
            .iter()
            .position(|preset| preset.name == name)
    });
    ui.set_sound_preset(picked.map_or(0, |index| index as i32 + 1));
    ui.set_sound_unsaved(sound_presets::unsaved(saved));
}

fn band_line(band: &EqBand) -> EqBandLine {
    EqBandLine {
        filter: band.filter.clamp(0, 4),
        frequency: decimal(band.frequency).into(),
        gain: decimal(band.gain).into(),
        q: decimal(band.q).into(),
    }
}

/// Puts the `row`th band's numbers in its row, leaving the others as typed.
fn show_band(ui: &AppWindow, row: usize, band: &EqBand) {
    ui.get_eq_bands().set_row_data(row, band_line(band));
}

/// Puts the bands' points and their curve on the graph. A point already
/// there is changed where it is, as a drag holds it.
fn show_graph(ui: &AppWindow, eq: &prefs::Equalizer) {
    let bands: Vec<Band> = eq.bands.iter().map(band).collect();
    let points: Vec<EqPoint> = bands
        .iter()
        .map(|band| {
            let (x, y) = eq_graph::point(band);
            EqPoint {
                x: x as f32,
                y: y as f32,
            }
        })
        .collect();
    let shown = ui.get_eq_points();
    if shown.row_count() == points.len() {
        for (row, point) in points.into_iter().enumerate() {
            shown.set_row_data(row, point);
        }
    } else {
        ui.set_eq_points(ModelRc::new(VecModel::from(points)));
    }
    ui.set_eq_curve(eq_graph::curve(&bands).into());
}

/// A number as typed, with a point or a comma before its fraction.
fn number(text: &str) -> Option<f64> {
    text.trim()
        .replace(',', ".")
        .parse()
        .ok()
        .filter(|number: &f64| number.is_finite())
}

fn decimal(number: f64) -> String {
    let text = format!("{number:.2}");
    text.trim_end_matches('0').trim_end_matches('.').to_owned()
}

/// The EQ in AutoEQ's parametric settings, which Equalizer APO reads too:
/// a `Preamp: -6.2 dB` line and `Filter 1: ON PK Fc 105 Hz Gain 2.3 dB Q
/// 0.70` lines. Filters that are off are left out.
fn parametric(text: &str) -> Result<prefs::Equalizer, String> {
    let mut eq = prefs::Equalizer::default();
    for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
        let words: Vec<&str> = line.split_whitespace().collect();
        match words.as_slice() {
            ["Preamp:", value, "dB"] => {
                eq.preamp = number(value).ok_or_else(|| unreadable(line))?;
            }
            ["Filter", _, "OFF", ..] | ["Filter:", "OFF", ..] => {}
            ["Filter", _, "ON", kind, rest @ ..] | ["Filter:", "ON", kind, rest @ ..] => {
                eq.bands
                    .push(filter(kind, rest).ok_or_else(|| unreadable(line))?);
            }
            _ => return Err(unreadable(line)),
        }
    }
    if eq.bands.is_empty() {
        return Err("There are no filters in it.".into());
    }
    Ok(eq)
}

/// A filter of `kind` from its `Fc 105 Hz Gain 2.3 dB Q 0.70`; a pass or
/// shelf without a Q gets the usual 0.71.
fn filter(kind: &str, settings: &[&str]) -> Option<EqBand> {
    let filter = match kind {
        "PK" => 0,
        "LS" | "LSC" => 1,
        "HS" | "HSC" => 2,
        "LP" | "LPQ" => 3,
        "HP" | "HPQ" => 4,
        _ => return None,
    };
    // The number after `key`, or `missing` without the key.
    let setting =
        |key: &str, missing: Option<f64>| match settings.iter().position(|word| *word == key) {
            Some(at) => number(settings.get(at + 1)?),
            None => missing,
        };
    Some(EqBand {
        filter,
        frequency: setting("Fc", None)?,
        gain: setting("Gain", Some(0.0))?,
        q: setting("Q", Some(0.71))?,
    })
}

fn unreadable(line: &str) -> String {
    format!("This line isn't AutoEQ's: {line}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn autoeqs_settings_become_the_eq() {
        let text = "Preamp: -6.2 dB\n\
                    Filter 1: ON LSC Fc 105 Hz Gain 5.5 dB Q 0.70\n\
                    Filter 2: ON PK Fc 2500 Hz Gain -3.1 dB Q 1.41\n\
                    Filter 3: OFF PK Fc 4000 Hz Gain 2 dB Q 2\n\
                    Filter 4: ON HSC Fc 10000 Hz Gain -4 dB Q 0.7\n";
        let eq = parametric(text).unwrap();
        assert_eq!(eq.preamp, -6.2);
        let bands: Vec<(i32, f64, f64, f64)> = eq
            .bands
            .iter()
            .map(|band| (band.filter, band.frequency, band.gain, band.q))
            .collect();
        assert_eq!(
            bands,
            [
                (1, 105.0, 5.5, 0.7),
                (0, 2_500.0, -3.1, 1.41),
                (2, 10_000.0, -4.0, 0.7)
            ]
        );
        assert!(parametric("Filter 1: ON BP Fc 100 Hz Q 1").is_err());
        assert!(parametric("Preamp: -3 dB").is_err());
        let shelf = parametric("Filter 1: ON LS Fc 100 Hz Gain 3 dB").unwrap();
        assert_eq!(shelf.bands[0].q, 0.71, "a shelf without a Q");
        assert_eq!(number(" 1,5 "), Some(1.5));
    }
}
