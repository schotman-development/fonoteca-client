//! Sound presets: every setting of the sound processing saved by name, and
//! which of them the settings were last set from, to tell what changed since.
//! A preset is linked to sound devices, and a device to one preset at most:
//! a device takes its preset up as it becomes the output, one linked to none
//! plays with no processing at all, and what wasn't saved goes either way.

use crate::prefs::{DevicePreset, Prefs, SoundPreset};

/// The preset of this name.
pub fn find<'a>(saved: &'a Prefs, name: &str) -> Option<&'a SoundPreset> {
    saved
        .sound_presets
        .iter()
        .find(|preset| preset.name == name)
}

/// The settings become the preset `name`'s, or no processing at all for
/// none.
pub fn choose(saved: &mut Prefs, name: Option<&str>) {
    let chosen = name.and_then(|name| find(saved, name)).cloned();
    saved.sound = chosen
        .as_ref()
        .map(|preset| preset.sound.clone())
        .unwrap_or_default();
    saved.sound_preset = chosen.map(|preset| preset.name);
}

/// `device`, by its key, became the output: the settings become the preset
/// linked to it, or no processing at all.
pub fn use_device(saved: &mut Prefs, device: &str) {
    let linked = saved
        .device_presets
        .get(device)
        .map(|link| link.preset.clone());
    choose(saved, linked.as_deref());
}

/// Links the preset the settings were set from to `device`, by its key and
/// name, in place of the one it had.
pub fn link(saved: &mut Prefs, device: &str, device_name: &str) {
    let Some(preset) = saved.sound_preset.clone() else {
        return;
    };
    let link = DevicePreset {
        device_name: device_name.to_owned(),
        preset,
    };
    saved.device_presets.insert(device.to_owned(), link);
}

/// Leaves `device`, by its key, linked to no preset.
pub fn unlink(saved: &mut Prefs, device: &str) {
    saved.device_presets.remove(device);
}

/// The devices linked to the preset `name`, by their keys.
pub fn linked<'a>(
    saved: &'a Prefs,
    name: &'a str,
) -> impl Iterator<Item = (&'a String, &'a DevicePreset)> {
    saved
        .device_presets
        .iter()
        .filter(move |(_, link)| link.preset == name)
}

/// Saves the settings as the preset `name`, in place of one of that name.
pub fn save_as(saved: &mut Prefs, name: &str) {
    let preset = SoundPreset {
        name: name.trim().to_owned(),
        sound: saved.sound.clone(),
    };
    if preset.name.is_empty() {
        return;
    }
    saved.sound_preset = Some(preset.name.clone());
    match saved
        .sound_presets
        .iter_mut()
        .find(|same| same.name == preset.name)
    {
        Some(same) => *same = preset,
        None => saved.sound_presets.push(preset),
    }
}

/// Renames the preset the settings were set from, unless another has the
/// name already.
pub fn rename(saved: &mut Prefs, name: &str) -> Result<(), String> {
    let name = name.trim();
    let Some(old) = saved.sound_preset.clone() else {
        return Ok(());
    };
    if name.is_empty() || name == old {
        return Ok(());
    }
    if find(saved, name).is_some() {
        return Err(format!("There is a preset named “{name}” already."));
    }
    for preset in &mut saved.sound_presets {
        if preset.name == old {
            preset.name = name.to_owned();
        }
    }
    for link in saved.device_presets.values_mut() {
        if link.preset == old {
            link.preset = name.to_owned();
        }
    }
    saved.sound_preset = Some(name.to_owned());
    Ok(())
}

/// Deletes the preset the settings were set from, and its links; the
/// settings stay as they are.
pub fn delete(saved: &mut Prefs) {
    let Some(name) = saved.sound_preset.take() else {
        return;
    };
    saved.sound_presets.retain(|preset| preset.name != name);
    saved.device_presets.retain(|_, link| link.preset != name);
}

/// Whether the settings differ from the preset they were set from, or from
/// no processing when they weren't set from one.
pub fn unsaved(saved: &Prefs) -> bool {
    let kept = saved
        .sound_preset
        .as_deref()
        .and_then(|name| find(saved, name))
        .map(|preset| &preset.sound);
    saved.sound != kept.cloned().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prefs::Sound;

    fn louder(headroom: i32) -> Sound {
        Sound {
            headroom,
            ..Sound::default()
        }
    }

    #[test]
    fn a_preset_holds_every_setting_and_changes_since_are_unsaved() {
        let mut saved = Prefs::default();
        assert!(!unsaved(&saved), "no processing is no change");
        saved.sound = Sound {
            headroom: 3,
            crossfeed: 2,
            upsampling: 1,
            eq_on: true,
            eq: crate::prefs::Equalizer {
                preamp: -4.0,
                bands: Vec::new(),
            },
        };
        let all = saved.sound.clone();
        assert!(unsaved(&saved));
        save_as(&mut saved, " Desk ");
        assert_eq!(saved.sound_preset.as_deref(), Some("Desk"));
        assert_eq!(find(&saved, "Desk").unwrap().sound, all);
        assert!(!unsaved(&saved));
        saved.sound.crossfeed = 0;
        assert!(unsaved(&saved));
        choose(&mut saved, Some("Desk"));
        assert_eq!((saved.sound.clone(), unsaved(&saved)), (all, false));
        choose(&mut saved, None);
        assert_eq!(
            (saved.sound.clone(), saved.sound_preset.clone()),
            (Sound::default(), None)
        );
    }

    #[test]
    fn saving_under_a_name_taken_replaces_that_preset() {
        let mut saved = Prefs {
            sound: louder(1),
            ..Prefs::default()
        };
        save_as(&mut saved, "Desk");
        saved.sound = louder(2);
        save_as(&mut saved, "Desk");
        assert_eq!(saved.sound_presets.len(), 1);
        assert_eq!(find(&saved, "Desk").unwrap().sound, louder(2));
        save_as(&mut saved, "  ");
        assert_eq!(saved.sound_presets.len(), 1, "a blank name saves nothing");
    }

    #[test]
    fn a_rename_keeps_the_settings_and_refuses_a_name_taken() {
        let mut saved = Prefs {
            sound: louder(1),
            ..Prefs::default()
        };
        save_as(&mut saved, "Desk");
        saved.sound = louder(2);
        save_as(&mut saved, "Couch");
        assert!(rename(&mut saved, "Desk").is_err());
        assert_eq!(rename(&mut saved, "Sofa"), Ok(()));
        assert_eq!(saved.sound_preset.as_deref(), Some("Sofa"));
        assert_eq!(find(&saved, "Sofa").unwrap().sound, louder(2));
        assert!(find(&saved, "Couch").is_none());
        delete(&mut saved);
        assert_eq!(saved.sound_preset, None);
        assert_eq!(saved.sound, louder(2), "the settings stay");
        assert!(unsaved(&saved));
        assert_eq!(saved.sound_presets.len(), 1);
    }

    /// Prefs with the presets Desk, louder by 1, linked to the DAC and the
    /// headphones, and Couch, louder by 2, linked to the speaker.
    fn linked_up() -> Prefs {
        let mut saved = Prefs {
            sound: louder(1),
            ..Prefs::default()
        };
        save_as(&mut saved, "Desk");
        link(&mut saved, "dac", "USB DAC");
        link(&mut saved, "headphones", "Headphones");
        saved.sound = louder(2);
        save_as(&mut saved, "Couch");
        link(&mut saved, "speaker", "Speaker");
        saved
    }

    #[test]
    fn a_device_without_a_preset_plays_with_no_processing() {
        let mut saved = linked_up();
        use_device(&mut saved, "dac");
        use_device(&mut saved, "hdmi");
        assert_eq!(saved.sound, Sound::default());
        assert_eq!(saved.sound_preset, None);
        assert!(!unsaved(&saved));
        unlink(&mut saved, "dac");
        use_device(&mut saved, "dac");
        assert_eq!(saved.sound, Sound::default(), "unlinked, it has none");
    }

    #[test]
    fn a_device_takes_up_its_linked_preset() {
        let mut saved = linked_up();
        use_device(&mut saved, "headphones");
        assert_eq!(saved.sound_preset.as_deref(), Some("Desk"));
        assert_eq!(saved.sound, louder(1));
        use_device(&mut saved, "speaker");
        assert_eq!(
            (saved.sound_preset.as_deref(), saved.sound.clone()),
            (Some("Couch"), louder(2))
        );
        // A device has one preset: linked to another, it leaves the first.
        link(&mut saved, "dac", "USB DAC");
        use_device(&mut saved, "dac");
        assert_eq!(saved.sound, louder(2));
        let desk: Vec<&String> = linked(&saved, "Desk").map(|(key, _)| key).collect();
        assert_eq!(desk, ["headphones"]);
    }

    #[test]
    fn unsaved_changes_go_when_the_device_changes() {
        let mut saved = linked_up();
        use_device(&mut saved, "dac");
        saved.sound.crossfeed = 3;
        assert!(unsaved(&saved));
        use_device(&mut saved, "headphones");
        assert_eq!(saved.sound, louder(1), "the same preset, as it was saved");
        saved.sound.crossfeed = 3;
        use_device(&mut saved, "hdmi");
        use_device(&mut saved, "headphones");
        assert_eq!(saved.sound, louder(1));
        assert!(!unsaved(&saved));
    }

    #[test]
    fn links_follow_a_rename_and_go_with_a_delete() {
        let mut saved = linked_up();
        use_device(&mut saved, "dac");
        rename(&mut saved, "Study").unwrap();
        use_device(&mut saved, "speaker");
        use_device(&mut saved, "headphones");
        assert_eq!(saved.sound_preset.as_deref(), Some("Study"));
        delete(&mut saved);
        use_device(&mut saved, "dac");
        assert_eq!(saved.sound, Sound::default());
        assert_eq!(saved.device_presets.keys().collect::<Vec<_>>(), ["speaker"]);
    }
}
