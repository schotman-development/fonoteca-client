//! The fonts the browser build carries for text Inter, the one font Slint
//! has there, has no glyphs for: Japanese, Chinese characters, Korean,
//! Arabic and emoji. A desktop or phone has the system's fonts for these.

use std::sync::Arc;

use slint::fontique_011::fontique::{Blob, Collection, FallbackKey, Script, ScriptExt};

/// Each font, and the scripts whose text falls back on it after Inter.
const FONTS: [(&[u8], &[Script]); 3] = [
    (
        include_bytes!("../data/fonts/NotoSansJP-Regular.otf"),
        &[
            Script::from_bytes(*b"Hira"),
            Script::from_bytes(*b"Kana"),
            Script::from_bytes(*b"Hani"),
        ],
    ),
    (
        include_bytes!("../data/fonts/NotoSansKR-Regular.otf"),
        &[Script::from_bytes(*b"Hang"), Script::from_bytes(*b"Hani")],
    ),
    (
        include_bytes!("../data/fonts/NotoSansArabic-Regular.ttf"),
        &[Script::from_bytes(*b"Arab")],
    ),
];

const EMOJI: &[u8] = include_bytes!("../data/fonts/NotoEmoji-VariableFont.ttf");

/// Adds the fonts to the ones Slint lays text out with, which exist once a
/// window does.
pub fn register() {
    let mut collection = slint::fontique_011::shared_collection();
    for (font, scripts) in FONTS {
        add_fallback(&mut collection, font, scripts.iter().copied());
    }
    // An emoji has no script of its own: it takes that of the text around
    // it, or none (Zyyy) on its own.
    let every_script = Script::all_samples().iter().map(|(script, _)| *script);
    let no_script = Script::from_bytes(*b"Zyyy");
    add_fallback(&mut collection, EMOJI, every_script.chain([no_script]));
}

fn add_fallback(
    collection: &mut Collection,
    font: &'static [u8],
    scripts: impl Iterator<Item = Script>,
) {
    let families: Vec<_> = (collection.register_fonts(Blob::new(Arc::new(font)), None))
        .into_iter()
        .map(|(family, _)| family)
        .collect();
    for script in scripts {
        collection.append_fallbacks(FallbackKey::new(script, None), families.iter().copied());
    }
}
