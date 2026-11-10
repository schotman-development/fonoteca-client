//! The settings' Appearance page: the color scheme and the accent color,
//! each kept in the preferences and applied at once.

use slint::{Color, ModelRc, VecModel};

use crate::{AccentChoice, AppWindow, prefs};

type Rgb = [u8; 3];

const WHITE: Rgb = [255; 3];
const BLACK: Rgb = [0; 3];

/// An accent color the user may pick, in the shade that reads on a light
/// background and the one that reads on a dark one.
struct Accent {
    name: &'static str,
    light: Rgb,
    dark: Rgb,
}

/// After the system's own, which the settings list first.
const ACCENTS: [Accent; 8] = [
    Accent {
        name: "Blue",
        light: [0x1a, 0x5f, 0xb4],
        dark: [0x6c, 0xb2, 0xff],
    },
    Accent {
        name: "Purple",
        light: [0x7c, 0x3a, 0xed],
        dark: [0xb7, 0x94, 0xf6],
    },
    Accent {
        name: "Pink",
        light: [0xc2, 0x25, 0x5c],
        dark: [0xff, 0x7a, 0xa8],
    },
    Accent {
        name: "Red",
        light: [0xc4, 0x2b, 0x1c],
        dark: [0xff, 0x7b, 0x72],
    },
    Accent {
        name: "Orange",
        light: [0xb3, 0x47, 0x00],
        dark: [0xff, 0xa0, 0x4d],
    },
    Accent {
        name: "Green",
        light: [0x1b, 0x7a, 0x3d],
        dark: [0x56, 0xd2, 0x7c],
    },
    Accent {
        name: "Teal",
        light: [0x0a, 0x7a, 0x7a],
        dark: [0x4f, 0xd1, 0xc5],
    },
    Accent {
        name: "Graphite",
        light: [0x4f, 0x5b, 0x66],
        dark: [0xaa, 0xb4, 0xbe],
    },
];

/// Wires the page up: shows what is saved and keeps what is chosen.
pub fn wire(ui: &AppWindow) {
    let saved = prefs::load();
    ui.set_accents(accent_choices());
    ui.set_color_scheme(saved.color_scheme.clamp(0, 2));
    ui.set_accent(saved.accent.clamp(0, ACCENTS.len() as i32));
    ui.on_choose_color_scheme(|index| {
        let mut saved = prefs::load();
        saved.color_scheme = index;
        prefs::save(&saved);
    });
    ui.on_choose_accent(|index| {
        let mut saved = prefs::load();
        saved.accent = index;
        prefs::save(&saved);
    });
}

fn accent_choices() -> ModelRc<AccentChoice> {
    let choices: Vec<_> = ACCENTS
        .iter()
        .map(|accent| AccentChoice {
            name: accent.name.into(),
            light: color(accent.light),
            dark: color(accent.dark),
            on_light: color(on_accent(accent.light)),
            on_dark: color(on_accent(accent.dark)),
        })
        .collect();
    ModelRc::new(VecModel::from(choices))
}

fn color([red, green, blue]: Rgb) -> Color {
    Color::from_rgb_u8(red, green, blue)
}

/// The color of what is drawn on `accent`: white or black, whichever is
/// easier to read.
fn on_accent(accent: Rgb) -> Rgb {
    if contrast(WHITE, accent) >= contrast(BLACK, accent) {
        WHITE
    } else {
        BLACK
    }
}

/// WCAG's contrast ratio of two colors, 1 to 21.
fn contrast(a: Rgb, b: Rgb) -> f64 {
    let (a, b) = (luminance(a), luminance(b));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

fn luminance(rgb: Rgb) -> f64 {
    let [red, green, blue] = rgb.map(|channel| {
        let level = f64::from(channel) / 255.0;
        if level <= 0.04045 {
            level / 12.92
        } else {
            ((level + 0.055) / 1.055).powf(2.4)
        }
    });
    0.2126 * red + 0.7152 * green + 0.0722 * blue
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pages' backgrounds in the app's styles, which an accent drawn as
    /// text or an icon must stand out from.
    const LIGHT_PAGE: Rgb = [0xfa, 0xfa, 0xfa];
    const DARK_PAGE: Rgb = [0x1c, 0x1c, 0x1c];
    const READABLE: f64 = 4.5;

    #[test]
    fn what_is_drawn_on_an_accent_is_the_easier_to_read_of_white_and_black() {
        assert_eq!(on_accent([0x1a, 0x5f, 0xb4]), WHITE);
        assert_eq!(on_accent([0xff, 0xff, 0x00]), BLACK);
        assert_eq!(on_accent([0x6c, 0xb2, 0xff]), BLACK);
        assert!((contrast(WHITE, BLACK) - 21.0).abs() < 1e-9);
    }

    #[test]
    fn every_accent_is_readable_in_its_scheme_and_carries_readable_marks() {
        for accent in &ACCENTS {
            for (shade, page) in [(accent.light, LIGHT_PAGE), (accent.dark, DARK_PAGE)] {
                assert!(
                    contrast(shade, page) >= READABLE,
                    "{} on the page",
                    accent.name
                );
                assert!(
                    contrast(shade, on_accent(shade)) >= READABLE,
                    "{}'s marks on it",
                    accent.name
                );
            }
        }
    }
}
