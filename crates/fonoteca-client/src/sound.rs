//! The sound processing in the settings: headroom, a parametric EQ with
//! presets and AutoEQ's settings pasted in, crossfeed and upsampling. They
//! are kept in the preferences and handed to the player as they change.

use std::rc::Rc;
use std::time::Duration;

use playback::{Band, Crossfeed, Dsp, Equalizer, Filter};
use slint::{ComponentHandle, ModelRc, SharedString, Timer, TimerMode, VecModel};

use crate::prefs::{self, EqBand, EqPreset, Prefs};
use crate::{AppWindow, EqBandLine};

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

/// The processing the settings ask for.
pub fn dsp() -> Dsp {
    let saved = prefs::load();
    Dsp {
        headroom: HEADROOM[saved.headroom.clamp(0, 7) as usize],
        eq: saved.eq_on.then(|| Equalizer {
            preamp: saved.eq.preamp,
            bands: saved.eq.bands.iter().map(band).collect(),
        }),
        crossfeed: CROSSFEED[saved.crossfeed.clamp(0, 3) as usize],
        upsampling: saved.upsampling == 1,
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
        let (settle, apply) = (settle.clone(), apply.clone());
        move |change: &dyn Fn(&mut Prefs)| {
            let mut saved = prefs::load();
            change(&mut saved);
            prefs::save(&saved);
            let apply = apply.clone();
            settle.start(TimerMode::SingleShot, SETTLE, move || apply(dsp()));
        }
    };
    let changed = Rc::new(changed);

    ui.on_choose_headroom({
        let changed = changed.clone();
        move |index| changed(&|saved| saved.headroom = index)
    });
    ui.on_choose_crossfeed({
        let changed = changed.clone();
        move |index| changed(&|saved| saved.crossfeed = index)
    });
    ui.on_choose_upsampling({
        let changed = changed.clone();
        move |index| changed(&|saved| saved.upsampling = index)
    });
    ui.on_toggle_eq({
        let changed = changed.clone();
        move |on| changed(&|saved| saved.eq_on = on)
    });
    ui.on_edit_preamp({
        let (weak, changed) = (ui.as_weak(), changed.clone());
        move |text| {
            let Some(preamp) = number(&text) else { return };
            changed(&|saved| saved.eq.preamp = preamp);
            show_preset(&weak.unwrap(), &prefs::load());
        }
    });
    ui.on_edit_band({
        let (weak, changed) = (ui.as_weak(), changed.clone());
        move |row, line| {
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
                if let Some(band) = saved.eq.bands.get_mut(row as usize) {
                    *band = edited.clone();
                }
            });
            show_preset(&weak.unwrap(), &prefs::load());
        }
    });
    ui.on_add_band({
        let (weak, changed) = (ui.as_weak(), changed.clone());
        move || {
            changed(&|saved| saved.eq.bands.push(NEW_BAND));
            show(&weak.unwrap(), &prefs::load());
        }
    });
    ui.on_remove_band({
        let (weak, changed) = (ui.as_weak(), changed.clone());
        move |row| {
            changed(&|saved| {
                if (row as usize) < saved.eq.bands.len() {
                    saved.eq.bands.remove(row as usize);
                }
            });
            show(&weak.unwrap(), &prefs::load());
        }
    });
    ui.on_choose_eq_preset({
        let (weak, changed) = (ui.as_weak(), changed.clone());
        move |index| {
            // The first entry is the EQ as it is.
            changed(&|saved| {
                if let Some(preset) = (index as usize)
                    .checked_sub(1)
                    .and_then(|index| saved.eq_presets.get(index))
                {
                    saved.eq = preset.eq.clone();
                }
            });
            show(&weak.unwrap(), &prefs::load());
        }
    });
    ui.on_save_eq_preset({
        let weak = ui.as_weak();
        move |name| {
            let mut saved = prefs::load();
            let preset = EqPreset {
                name: name.trim().to_owned(),
                eq: saved.eq.clone(),
            };
            if preset.name.is_empty() {
                return;
            }
            match saved.eq_presets.iter_mut().find(|p| p.name == preset.name) {
                Some(same) => *same = preset,
                None => saved.eq_presets.push(preset),
            }
            prefs::save(&saved);
            show(&weak.unwrap(), &saved);
        }
    });
    ui.on_delete_eq_preset({
        let weak = ui.as_weak();
        move || {
            let mut saved = prefs::load();
            saved.eq_presets.retain(|preset| preset.eq != saved.eq);
            prefs::save(&saved);
            show(&weak.unwrap(), &saved);
        }
    });
    ui.on_import_eq({
        let (weak, changed) = (ui.as_weak(), changed.clone());
        move |text| match parametric(&text) {
            Ok(eq) => {
                changed(&|saved| {
                    saved.eq = eq.clone();
                    saved.eq_on = true;
                });
                show(&weak.unwrap(), &prefs::load());
                SharedString::new()
            }
            Err(problem) => problem.into(),
        }
    });
}

/// Puts the saved processing in the settings.
fn show(ui: &AppWindow, saved: &Prefs) {
    ui.set_headroom(saved.headroom.clamp(0, 7));
    ui.set_crossfeed(saved.crossfeed.clamp(0, 3));
    ui.set_upsampling(saved.upsampling.clamp(0, 1));
    ui.set_eq_on(saved.eq_on);
    ui.set_eq_preamp(decimal(saved.eq.preamp).into());
    let lines: Vec<EqBandLine> = saved
        .eq
        .bands
        .iter()
        .map(|band| EqBandLine {
            filter: band.filter.clamp(0, 4),
            frequency: decimal(band.frequency).into(),
            gain: decimal(band.gain).into(),
            q: decimal(band.q).into(),
        })
        .collect();
    ui.set_eq_bands(ModelRc::new(VecModel::from(lines)));
    let mut names = vec![SharedString::from("Custom")];
    names.extend(
        saved
            .eq_presets
            .iter()
            .map(|preset| preset.name.as_str().into()),
    );
    ui.set_eq_presets(ModelRc::new(VecModel::from(names)));
    show_preset(ui, saved);
}

/// Selects the preset the EQ is, or the first entry when it is none.
fn show_preset(ui: &AppWindow, saved: &Prefs) {
    let preset = saved
        .eq_presets
        .iter()
        .position(|preset| preset.eq == saved.eq);
    ui.set_eq_preset(preset.map_or(0, |index| index as i32 + 1));
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
