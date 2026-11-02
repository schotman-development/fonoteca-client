//! The seek bar as the song's waveform, on servers that have Fonoteca's
//! `fonotecaWaveform` extension: thin bars of its level over time, drawn by
//! the bar as one filled path.

use std::fmt::Write;
use std::sync::Arc;

use opensubsonic::{Client, Error};
use slint::ComponentHandle;

use crate::AppWindow;

const EXTENSION: &str = "fonotecaWaveform";
/// The error a song without a waveform answers with.
const NO_SUCH_WAVEFORM: i64 = 70;
/// How many bars the path has, however many levels the server sent: few
/// enough to stay apart on a phone's narrow bar, and one path to redraw.
const BARS: usize = 100;
/// A bar's width within its slot of 1, so the bars keep a gap.
const BAR_WIDTH: f32 = 0.6;
/// The shortest a bar gets, as a share of the bar's height, so silence still
/// shows a mark.
const SHORTEST_BAR: f32 = 0.06;

/// Shows the waveform of the song `id` once it comes, and the plain bar
/// until then. `still_playing` says whether the song is still the one shown,
/// for a reply that arrives after it moved on.
pub fn show(
    ui: &AppWindow,
    client: Option<Arc<Client>>,
    id: String,
    still_playing: impl Fn() -> bool + 'static,
) {
    ui.set_playing_waveform("".into());
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
        let (Some(ui), Some(path), true) = (weak.upgrade(), bars_path(&levels), still_playing())
        else {
            return;
        };
        ui.set_playing_waveform(path.into());
    })
    .expect("the event loop is running");
}

/// The heights of `BARS` bars, 0 to 1, from the server's levels: each bar
/// takes the loudest level in its stretch of the song, and all are scaled so
/// the loudest bar of the song fills the bar. None for a song with no sound
/// in it, which has no waveform to draw.
fn bar_heights(levels: &[f32]) -> Option<Vec<f32>> {
    let peak = levels.iter().copied().fold(0.0, f32::max);
    if peak <= 0.0 {
        return None;
    }
    let heights = (0..BARS)
        .map(|bar| {
            let start = bar * levels.len() / BARS;
            let end = ((bar + 1) * levels.len() / BARS).clamp(start + 1, levels.len());
            let loudest = levels[start..end].iter().copied().fold(0.0, f32::max);
            (loudest / peak).max(SHORTEST_BAR)
        })
        .collect();
    Some(heights)
}

/// The bars as the commands of a path whose view box is `BARS` wide and 1
/// high, each bar a rectangle centred on the middle.
fn bars_path(levels: &[f32]) -> Option<String> {
    let heights = bar_heights(levels)?;
    let margin = (1.0 - BAR_WIDTH) / 2.0;
    let mut commands = String::new();
    for (bar, height) in heights.iter().enumerate() {
        let (x, top) = (bar as f32 + margin, (1.0 - height) / 2.0);
        write!(
            commands,
            "M{x:.2} {top:.3}h{BAR_WIDTH}v{height:.3}h-{BAR_WIDTH}z"
        )
        .expect("writing to a string");
    }
    Some(commands)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bars_scale_to_the_songs_own_loudest_level() {
        let mut levels = vec![0.1; 500];
        levels[250] = 0.4;
        let heights = bar_heights(&levels).unwrap();
        assert_eq!(heights.len(), BARS);
        assert_eq!(heights[50], 1.0);
        assert!((heights[0] - 0.25).abs() < 1e-6, "{}", heights[0]);
    }

    #[test]
    fn a_bar_takes_the_loudest_level_of_its_stretch() {
        let mut levels = vec![0.0; 500];
        levels[3] = 0.5;
        let heights = bar_heights(&levels).unwrap();
        assert_eq!(heights[0], 1.0);
        assert_eq!(heights[1], SHORTEST_BAR);
    }

    #[test]
    fn a_short_song_stretches_its_levels_over_the_bars() {
        let heights = bar_heights(&[0.2, 0.4]).unwrap();
        assert_eq!((heights[0], heights[BARS - 1]), (0.5, 1.0));
    }

    #[test]
    fn a_silent_or_empty_song_has_no_waveform() {
        assert_eq!(bar_heights(&[0.0, 0.0]), None);
        assert_eq!(bar_heights(&[]), None);
    }

    #[test]
    fn the_path_has_a_rectangle_per_bar() {
        let path = bars_path(&[0.5, 1.0]).unwrap();
        assert_eq!(path.matches('M').count(), BARS);
        assert!(path.starts_with("M0.20 0.250h0.6v0.500h-0.6z"), "{path}");
    }
}
