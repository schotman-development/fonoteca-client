//! The EQ's graph: frequency along the bottom on a log axis, gain up the
//! side, a point for each band and the curve they make together. A place on
//! the graph is a fraction of its width and height, from the left and from
//! the top, so this holds how those fractions and frequencies and gains map
//! to each other and the screen only has to scale them.

use playback::Band;

const LOWEST_FREQUENCY: f64 = 20.0;
const HIGHEST_FREQUENCY: f64 = 20_000.0;
/// How much a point on the graph can boost or cut, in dB; the typed gains
/// may go beyond, and their points stay at the edge.
const GAIN_RANGE: f64 = 15.0;
/// The curve is calculated at the rate most music has; the filters differ
/// from it only near half of the rate, above what the graph shows.
const CURVE_RATE: f64 = 48_000.0;
const CURVE_STEPS: usize = 160;
/// The size of the box the curve's path is in, which the screen scales to
/// the graph.
const CURVE_BOX: f64 = 1_000.0;
/// (Hz, words) of the grid's vertical lines; a line with no words is only a
/// line.
const FREQUENCY_LINES: [(f64, &str); 8] = [
    (50.0, ""),
    (100.0, "100"),
    (200.0, ""),
    (500.0, ""),
    (1_000.0, "1k"),
    (2_000.0, ""),
    (5_000.0, ""),
    (10_000.0, "10k"),
];
/// (dB, words) of the grid's horizontal lines.
const GAIN_LINES: [(f64, &str); 5] = [
    (10.0, "+10"),
    (5.0, ""),
    (0.0, "0"),
    (-5.0, ""),
    (-10.0, "−10"),
];

/// A line of the grid, at `position` along its axis.
#[derive(Debug, PartialEq)]
pub struct GridLine {
    pub vertical: bool,
    pub position: f64,
    pub label: &'static str,
}

pub fn grid() -> Vec<GridLine> {
    let frequencies = FREQUENCY_LINES.iter().map(|&(frequency, label)| GridLine {
        vertical: true,
        position: fraction_of_frequency(frequency),
        label,
    });
    let gains = GAIN_LINES.iter().map(|&(gain, label)| GridLine {
        vertical: false,
        position: fraction_of_gain(gain),
        label,
    });
    frequencies.chain(gains).collect()
}

/// How far along the graph, from its left, `frequency` is.
pub fn fraction_of_frequency(frequency: f64) -> f64 {
    let fraction =
        (frequency / LOWEST_FREQUENCY).ln() / (HIGHEST_FREQUENCY / LOWEST_FREQUENCY).ln();
    fraction.clamp(0.0, 1.0)
}

pub fn frequency_at(fraction: f64) -> f64 {
    LOWEST_FREQUENCY * (HIGHEST_FREQUENCY / LOWEST_FREQUENCY).powf(fraction.clamp(0.0, 1.0))
}

/// How far down the graph, from its top, `gain` is: the top is the most
/// boost.
pub fn fraction_of_gain(gain: f64) -> f64 {
    ((GAIN_RANGE - gain) / (2.0 * GAIN_RANGE)).clamp(0.0, 1.0)
}

pub fn gain_at(fraction: f64) -> f64 {
    GAIN_RANGE * (1.0 - 2.0 * fraction.clamp(0.0, 1.0))
}

/// Where the band's point is: at its gain, or for a pass, which has none, on
/// the line of no gain.
pub fn point(band: &Band) -> (f64, f64) {
    let gain = if band.filter.has_gain() {
        band.gain
    } else {
        0.0
    };
    (
        fraction_of_frequency(band.frequency),
        fraction_of_gain(gain),
    )
}

/// The band with its point moved to `x` and `y` of the graph: its frequency
/// to whole Hz and its gain to tenths of a dB, which is what is worth
/// typing. A pass moves along the frequencies only.
pub fn moved(band: &Band, x: f64, y: f64) -> Band {
    let gain = if band.filter.has_gain() {
        // Adding 0 turns the -0 of a gain just under the line into 0.
        (gain_at(y) * 10.0).round() / 10.0 + 0.0
    } else {
        band.gain
    };
    Band {
        frequency: frequency_at(x).round(),
        gain,
        ..*band
    }
}

/// How many dB all the bands together change `frequency` by.
fn total_response(bands: &[Band], frequency: f64) -> f64 {
    bands
        .iter()
        .map(|band| band.response(CURVE_RATE, frequency))
        .sum()
}

/// The path, in a box of `CURVE_BOX` square, of the bands' total response.
pub fn curve(bands: &[Band]) -> String {
    let mut commands = String::new();
    for step in 0..=CURVE_STEPS {
        let x = step as f64 / CURVE_STEPS as f64;
        let y = fraction_of_gain(total_response(bands, frequency_at(x)));
        let command = if step == 0 { 'M' } else { 'L' };
        commands += &format!("{command}{:.1} {:.1} ", x * CURVE_BOX, y * CURVE_BOX);
    }
    commands
}

#[cfg(test)]
mod tests {
    use playback::Filter;

    use super::*;

    fn band(filter: Filter, frequency: f64, gain: f64) -> Band {
        Band {
            filter,
            frequency,
            gain,
            q: 1.0,
        }
    }

    #[test]
    fn frequencies_are_spread_by_octave_over_the_audible_range() {
        assert_eq!(fraction_of_frequency(20.0), 0.0);
        assert_eq!(fraction_of_frequency(20_000.0), 1.0);
        assert!((fraction_of_frequency(632.455_532) - 0.5).abs() < 1e-6);
        assert!((frequency_at(0.5) - 632.455_532).abs() < 1e-3);
        let octave = fraction_of_frequency(2_000.0) - fraction_of_frequency(1_000.0);
        let next = fraction_of_frequency(4_000.0) - fraction_of_frequency(2_000.0);
        assert!((octave - next).abs() < 1e-12);
        assert_eq!(fraction_of_frequency(5.0), 0.0, "below the graph");
        assert_eq!(fraction_of_frequency(48_000.0), 1.0, "above the graph");
        for frequency in [20.0, 87.5, 1_000.0, 16_000.0] {
            let back = frequency_at(fraction_of_frequency(frequency));
            assert!((back - frequency).abs() < 1e-9, "{frequency} Hz");
        }
    }

    #[test]
    fn gains_run_from_the_most_boost_at_the_top_to_the_most_cut() {
        assert_eq!(fraction_of_gain(15.0), 0.0);
        assert_eq!(fraction_of_gain(0.0), 0.5);
        assert_eq!(fraction_of_gain(-15.0), 1.0);
        assert_eq!(fraction_of_gain(40.0), 0.0, "beyond the graph");
        assert_eq!(gain_at(0.25), 7.5);
        assert_eq!(gain_at(2.0), -15.0);
    }

    #[test]
    fn a_point_dragged_to_a_place_gives_the_band_its_frequency_and_gain() {
        let moved = moved(&band(Filter::Peak, 1_000.0, 0.0), 0.5, 0.25);
        assert_eq!(moved.frequency, 632.0);
        assert_eq!(moved.gain, 7.5);
        assert_eq!(moved.q, 1.0);
        let (x, y) = point(&moved);
        assert!((x - 0.5).abs() < 1e-3 && y == 0.25);
        let rounded = super::moved(&moved, 0.5, 0.3123);
        assert_eq!(rounded.gain, 5.6);
    }

    #[test]
    fn a_gain_just_under_the_line_is_zero_not_minus_zero() {
        let moved = moved(&band(Filter::Peak, 1_000.0, 3.0), 0.5, 0.500_000_1);
        assert!(
            moved.gain == 0.0 && moved.gain.is_sign_positive(),
            "{}",
            moved.gain
        );
    }

    #[test]
    fn a_pass_has_its_point_on_the_line_of_no_gain_and_keeps_its_gain() {
        let pass = band(Filter::HighPass, 80.0, 3.0);
        assert_eq!(point(&pass).1, 0.5);
        let moved = moved(&pass, 0.4, 0.0);
        assert_eq!(moved.gain, 3.0);
        assert_eq!(moved.frequency, frequency_at(0.4).round());
    }

    #[test]
    fn the_curve_adds_the_bands_responses() {
        let boost = band(Filter::Peak, 1_000.0, 6.0);
        let cut = band(Filter::Peak, 1_000.0, -2.0);
        let total = total_response(&[boost, boost, cut], 1_000.0);
        assert!((total - 10.0).abs() < 1e-9, "{total}");
        assert_eq!(total_response(&[], 1_000.0), 0.0);
        let path = curve(&[boost]);
        assert_eq!(path.matches('L').count(), CURVE_STEPS);
        assert!(path.starts_with("M0.0 "), "{path}");
        assert!(path.contains("L1000.0 "), "{path}");
    }

    #[test]
    fn a_boost_rises_above_the_line_of_no_gain_and_a_cut_falls_below_it() {
        // Where the curve is at 1 kHz, in the box it is drawn in.
        let height_at_1k = |gain: f64| {
            let path = curve(&[band(Filter::Peak, 1_000.0, gain)]);
            let at = fraction_of_frequency(1_000.0) * CURVE_BOX;
            path.split(['M', 'L'])
                .filter_map(|point| {
                    let mut numbers = point.split_whitespace().map(|n| n.parse::<f64>().unwrap());
                    Some((numbers.next()?, numbers.next()?))
                })
                .min_by(|a, b| (a.0 - at).abs().total_cmp(&(b.0 - at).abs()))
                .unwrap()
                .1
        };
        let (boost, cut) = (height_at_1k(6.0), height_at_1k(-6.0));
        assert!((boost - 300.0).abs() < 3.0, "{boost}");
        assert!((cut - 700.0).abs() < 3.0, "{cut}");
    }

    #[test]
    fn the_grid_has_its_lines_in_place() {
        let grid = grid();
        let labelled: Vec<&str> = grid
            .iter()
            .map(|line| line.label)
            .filter(|label| !label.is_empty())
            .collect();
        assert_eq!(labelled, ["100", "1k", "10k", "+10", "0", "−10"]);
        let zero = grid.iter().find(|line| line.label == "0").unwrap();
        assert_eq!((zero.vertical, zero.position), (false, 0.5));
    }
}
