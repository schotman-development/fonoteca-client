//! The seek bar as the song's waveform, on servers that have Fonoteca's
//! `fonotecaWaveform` extension: thin bars of its level over time, drawn by
//! the bar as one filled path, as many as its width has room for.

use std::fmt::Write;
use std::sync::Arc;

use opensubsonic::{Client, Error};
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};

use crate::{AppWindow, Waveform};

const EXTENSION: &str = "fonotecaWaveform";
/// The error a song without a waveform answers with.
const NO_SUCH_WAVEFORM: i64 = 70;
/// The room a bar takes in logical pixels, its gap included: about what
/// each of the hundred bars of a phone's seek bar had, so a wider bar draws
/// more of them rather than thicker ones.
const BAR_PITCH: f32 = 3.0;
/// A bar's width within its room, so the bars keep a gap.
const BAR_WIDTH: f32 = 0.6;
/// The shortest a bar gets, as a share of the bar's height, so silence still
/// shows a mark.
const SHORTEST_BAR: f32 = 0.06;

/// Has the seek bars draw their waveforms, which Slint has no loop for.
pub fn wire(ui: &AppWindow) {
    ui.global::<Waveform>().on_path(path);
}

/// The commands of the path a seek bar `width` logical pixels wide draws the
/// song's `levels` as, in a view box as wide and 1 high.
pub fn path(levels: ModelRc<f32>, width: f32) -> SharedString {
    let levels: Vec<f32> = levels.iter().collect();
    bars_path(&levels, width).unwrap_or_default().into()
}

/// Shows the waveform of the song `id` once it comes, and the plain bar
/// until then. `still_playing` says whether the song is still the one shown,
/// for a reply that arrives after it moved on.
pub fn show(
    ui: &AppWindow,
    client: Option<Arc<Client>>,
    id: String,
    still_playing: impl Fn() -> bool + 'static,
) {
    ui.set_playing_waveform(ModelRc::default());
    let Some(client) = client.filter(|client| client.supports(EXTENSION)) else {
        return;
    };
    let weak = ui.as_weak();
    slint::spawn_local(async move {
        let levels = match client.get_waveform(&id).await {
            Ok(waveform) => waveform.level,
            Err(Error::Api(problem)) if problem.code == NO_SUCH_WAVEFORM => return,
            Err(problem) => return eprintln!("The waveform of {id}: {problem}"),
        };
        let (Some(ui), Some(_), true) = (weak.upgrade(), loudest_level(&levels), still_playing())
        else {
            return;
        };
        ui.set_playing_waveform(ModelRc::new(VecModel::from(levels)));
    })
    .expect("the event loop is running");
}

/// The loudest of the song's levels; None for a song with no sound in it,
/// which has no waveform to draw.
fn loudest_level(levels: &[f32]) -> Option<f32> {
    let peak = levels.iter().copied().fold(0.0, f32::max);
    (peak > 0.0).then_some(peak)
}

/// How many bars a seek bar `width` logical pixels wide has room for.
fn bar_count(width: f32) -> usize {
    (width / BAR_PITCH) as usize
}

/// The heights of `bars` bars, 0 to 1, from the server's levels: each bar
/// takes the loudest level in its stretch of the song, and all are scaled so
/// the loudest bar of the song fills the bar.
fn bar_heights(levels: &[f32], bars: usize) -> Option<Vec<f32>> {
    let peak = loudest_level(levels)?;
    let heights = (0..bars)
        .map(|bar| {
            let start = bar * levels.len() / bars;
            let end = ((bar + 1) * levels.len() / bars).clamp(start + 1, levels.len());
            let loudest = levels[start..end].iter().copied().fold(0.0, f32::max);
            (loudest / peak).max(SHORTEST_BAR)
        })
        .collect();
    Some(heights)
}

/// The bars across `width` logical pixels as the commands of a path whose
/// view box is as wide and 1 high, each bar a rectangle centred on the
/// middle.
fn bars_path(levels: &[f32], width: f32) -> Option<String> {
    let bars = bar_count(width);
    let heights = bar_heights(levels, bars)?;
    let pitch = width / bars as f32;
    let (bar_width, margin) = (BAR_WIDTH * pitch, (1.0 - BAR_WIDTH) * pitch / 2.0);
    let mut commands = String::new();
    for (bar, height) in heights.iter().enumerate() {
        let (x, top) = (bar as f32 * pitch + margin, (1.0 - height) / 2.0);
        write!(
            commands,
            "M{x:.2} {top:.3}h{bar_width:.2}v{height:.3}h-{bar_width:.2}z"
        )
        .expect("writing to a string");
    }
    Some(commands)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wider_bar_has_more_bars_as_thin() {
        assert_eq!(bar_count(300.0), 100);
        assert_eq!(bar_count(1200.0), 400);
        for width in [90.0, 268.0, 412.0, 1000.0, 2500.0, 3840.0] {
            let pitch = width / bar_count(width) as f32;
            assert!(
                (BAR_PITCH..BAR_PITCH * 1.04).contains(&pitch),
                "{pitch} at {width}"
            );
        }
    }

    #[test]
    fn a_bar_too_narrow_for_one_draws_none() {
        assert_eq!(bar_count(BAR_PITCH - 0.1), 0);
        assert_eq!(bars_path(&[0.5], BAR_PITCH - 0.1).as_deref(), Some(""));
    }

    #[test]
    fn bars_scale_to_the_songs_own_loudest_level() {
        let mut levels = vec![0.1; 500];
        levels[250] = 0.4;
        let heights = bar_heights(&levels, 100).unwrap();
        assert_eq!(heights.len(), 100);
        assert_eq!(heights[50], 1.0);
        assert!((heights[0] - 0.25).abs() < 1e-6, "{}", heights[0]);
    }

    #[test]
    fn a_bar_takes_the_loudest_level_of_its_stretch() {
        let mut levels = vec![0.0; 500];
        levels[3] = 0.5;
        let heights = bar_heights(&levels, 100).unwrap();
        assert_eq!(heights[0], 1.0);
        assert_eq!(heights[1], SHORTEST_BAR);
    }

    #[test]
    fn a_short_song_stretches_its_levels_over_the_bars() {
        let heights = bar_heights(&[0.2, 0.4], 100).unwrap();
        assert_eq!((heights[0], heights[99]), (0.5, 1.0));
    }

    #[test]
    fn a_silent_or_empty_song_has_no_waveform() {
        assert_eq!(loudest_level(&[0.0, 0.0]), None);
        assert_eq!(loudest_level(&[]), None);
        assert_eq!(bar_heights(&[0.0, 0.0], 100), None);
    }

    #[test]
    fn the_path_has_a_rectangle_per_bar_as_wide_at_any_width() {
        let path = bars_path(&[0.5, 1.0], 300.0).unwrap();
        assert_eq!(path.matches('M').count(), 100);
        assert!(path.starts_with("M0.60 0.250h1.80v0.500h-1.80z"), "{path}");
        let wide = bars_path(&[0.5, 1.0], 1500.0).unwrap();
        assert_eq!(wide.matches('M').count(), 500);
        assert!(wide.starts_with("M0.60 0.250h1.80v0.500h-1.80z"), "{wide}");
    }
}
