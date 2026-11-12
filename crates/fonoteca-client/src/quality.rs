//! The format and bitrate to stream songs in, picked in the settings for wifi
//! and for mobile data, and which of the two the connection in use calls for.

use crate::prefs::{Prefs, StreamQuality};

/// The settings' formats, in their order: what the picker shows and the
/// `format` the server is asked for. The first is the song as it is kept.
const FORMATS: [&str; 4] = [ORIGINAL_FORMAT, "mp3", "opus", "aac"];
/// The `format` that asks for the song as it is kept.
pub const ORIGINAL_FORMAT: &str = "raw";
/// Stands for whatever format a server's decision converted to, where a
/// download is weighed against the quality wanted.
pub const CONVERTED_FORMAT: &str = crate::offline::CONVERTED;
/// The settings' bitrates in kbit/s, in their order.
const BITRATES: [u32; 7] = [320, 256, 192, 160, 128, 96, 64];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Connection {
    Wifi,
    MobileData,
}

/// The connection songs are being fetched over now. Only Android says: a
/// computer or the browser has no mobile data to tell apart.
pub fn connection() -> Connection {
    if on_mobile_data() {
        Connection::MobileData
    } else {
        Connection::Wifi
    }
}

#[cfg(target_os = "android")]
use crate::network::on_mobile_data;

#[cfg(all(not(target_os = "android"), not(test)))]
fn on_mobile_data() -> bool {
    false
}

/// What the tests say the connection is, since a computer has no mobile data.
#[cfg(test)]
pub static TEST_ON_MOBILE_DATA: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

#[cfg(all(not(target_os = "android"), test))]
fn on_mobile_data() -> bool {
    TEST_ON_MOBILE_DATA.load(std::sync::atomic::Ordering::Relaxed)
}

/// Whether songs may be downloaded over `connection`: wifi always, mobile
/// data only when the user allowed it.
pub fn may_download(prefs: &Prefs, connection: Connection) -> bool {
    connection == Connection::Wifi || prefs.download_on_mobile_data
}

/// The OpenSubsonic extension with which the server decides, from what the
/// client plays, whether a song is sent as it is or converted.
pub const TRANSCODING: &str = "transcoding";

/// How much quality to stream at from a server with `TRANSCODING`: the
/// settings' entries, in their order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Level {
    Original,
    High,
    Medium,
    Low,
}

impl Level {
    const IN_ORDER: [Level; 4] = [Level::Original, Level::High, Level::Medium, Level::Low];

    /// The level the settings' entry `index` is, held to the entries there
    /// are, for a saved file that has one out of range.
    pub fn at(index: i32) -> Level {
        Self::IN_ORDER[index.clamp(0, Self::IN_ORDER.len() as i32 - 1) as usize]
    }

    /// The level whose most is `kbit` kbit/s, the original for none.
    pub fn of_kbit_per_second(kbit: u32) -> Level {
        Self::IN_ORDER
            .into_iter()
            .find(|level| level.kbit_per_second() == kbit)
            .unwrap_or(Level::Original)
    }

    /// The settings' entry for this level.
    pub fn index(self) -> i32 {
        Self::IN_ORDER
            .iter()
            .position(|&level| level == self)
            .unwrap_or(0) as i32
    }

    /// The most kbit/s a conversion at this level may use; zero for the
    /// original.
    pub fn kbit_per_second(self) -> u32 {
        match self {
            Level::Original => 0,
            Level::High => 256,
            Level::Medium => 160,
            Level::Low => 96,
        }
    }
}

/// The level to stream at over `connection`: what the settings picked, else
/// the original on wifi and one step lower on mobile data.
pub fn level(prefs: &Prefs, connection: Connection) -> Level {
    match connection {
        Connection::Wifi => prefs.wifi_level.map_or(Level::Original, Level::at),
        Connection::MobileData => prefs.mobile_level.map_or(Level::High, Level::at),
    }
}

/// Whether the settings offer a choice for mobile data apart from wifi's.
pub const HAS_MOBILE_DATA: bool = cfg!(target_os = "android");

/// `choice` held to the entries the settings have, for a saved file that
/// has one out of range.
pub fn within_lists(choice: StreamQuality) -> StreamQuality {
    StreamQuality {
        format: choice.format.clamp(0, FORMATS.len() as i32 - 1),
        bitrate: choice.bitrate.clamp(0, BITRATES.len() as i32 - 1),
    }
}

/// What to ask the server for over `connection`: a format and the most
/// kbit/s it may use, zero for no limit.
pub fn request(prefs: &Prefs, connection: Connection) -> (&'static str, u32) {
    ask_for(match connection {
        Connection::Wifi => prefs.wifi_quality,
        Connection::MobileData => prefs.mobile_quality,
    })
}

/// What to ask the server for when downloading, whatever the connection: the
/// level picked for downloads from a server that `decides` what to convert,
/// as `CONVERTED_FORMAT` at its kbit/s, else the format and bitrate picked.
pub fn download_request(prefs: &Prefs, decides: bool) -> (&'static str, u32) {
    if !decides {
        return ask_for(prefs.download_quality);
    }
    match Level::at(prefs.download_level) {
        Level::Original => (ORIGINAL_FORMAT, 0),
        level => (CONVERTED_FORMAT, level.kbit_per_second()),
    }
}

fn ask_for(choice: StreamQuality) -> (&'static str, u32) {
    let StreamQuality { format, bitrate } = within_lists(choice);
    let format = FORMATS[format as usize];
    if format == FORMATS[0] {
        return (format, 0);
    }
    (format, BITRATES[bitrate as usize])
}

/// Where in the song a stream in `format` asked for from `seconds` begins:
/// there, for a conversion by a server that converts from anywhere; none
/// for the whole file, which the player reads on to that point.
pub fn stream_start(format: &str, converts_from_anywhere: bool, seconds: f64) -> Option<f64> {
    (format != FORMATS[0] && converts_from_anywhere).then_some(seconds)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prefs(wifi: (i32, i32), mobile: (i32, i32)) -> Prefs {
        let quality = |(format, bitrate)| StreamQuality { format, bitrate };
        Prefs {
            wifi_quality: quality(wifi),
            mobile_quality: quality(mobile),
            ..Prefs::default()
        }
    }

    #[test]
    fn mobile_data_is_downloaded_over_only_when_allowed() {
        let allowed = Prefs {
            download_on_mobile_data: true,
            ..Prefs::default()
        };
        assert!(may_download(&Prefs::default(), Connection::Wifi));
        assert!(!may_download(&Prefs::default(), Connection::MobileData));
        assert!(may_download(&allowed, Connection::MobileData));
    }

    #[test]
    fn levels_default_to_the_original_on_wifi_and_one_step_lower_on_mobile_data() {
        let none = Prefs::default();
        assert_eq!(level(&none, Connection::Wifi), Level::Original);
        assert_eq!(level(&none, Connection::MobileData), Level::High);
        let chosen = Prefs {
            wifi_level: Some(3),
            mobile_level: Some(0),
            ..Prefs::default()
        };
        assert_eq!(level(&chosen, Connection::Wifi), Level::Low);
        assert_eq!(level(&chosen, Connection::MobileData), Level::Original);
    }

    #[test]
    fn a_saved_level_out_of_range_is_held_to_the_list() {
        assert_eq!(Level::at(-4), Level::Original);
        assert_eq!(Level::at(40), Level::Low);
        assert_eq!(Level::Medium.index(), 2);
        assert_eq!(Level::Medium.kbit_per_second(), 160);
    }

    #[test]
    fn nothing_chosen_streams_the_original() {
        assert_eq!(request(&Prefs::default(), Connection::Wifi), ("raw", 0));
        assert_eq!(
            request(&Prefs::default(), Connection::MobileData),
            ("raw", 0)
        );
    }

    #[test]
    fn each_connection_takes_its_own_choice() {
        let prefs = prefs((0, 3), (2, 5));
        assert_eq!(request(&prefs, Connection::Wifi), ("raw", 0));
        assert_eq!(request(&prefs, Connection::MobileData), ("opus", 96));
    }

    #[test]
    fn only_a_conversion_on_a_server_that_offsets_it_begins_part_way() {
        assert_eq!(stream_start("opus", true, 61.5), Some(61.5));
        assert_eq!(stream_start("raw", true, 61.5), None);
        assert_eq!(stream_start("mp3", false, 61.5), None);
    }

    #[test]
    fn downloads_take_their_own_choice_whatever_the_connection() {
        let prefs = Prefs {
            download_quality: StreamQuality {
                format: 2,
                bitrate: 4,
            },
            ..prefs((0, 0), (1, 1))
        };
        assert_eq!(download_request(&prefs, false), ("opus", 128));
        assert_eq!(download_request(&Prefs::default(), false), ("raw", 0));
    }

    #[test]
    fn a_server_that_decides_is_downloaded_from_by_level() {
        let prefs = Prefs {
            download_level: 2,
            ..Prefs::default()
        };
        assert_eq!(download_request(&prefs, true), (CONVERTED_FORMAT, 160));
        assert_eq!(download_request(&Prefs::default(), true), ("raw", 0));
        assert_eq!(Level::of_kbit_per_second(160), Level::Medium);
        assert_eq!(Level::of_kbit_per_second(0), Level::Original);
    }

    #[test]
    fn a_saved_entry_out_of_range_is_held_to_the_list() {
        let prefs = prefs((9, 40), (-1, -1));
        assert_eq!(request(&prefs, Connection::Wifi), ("aac", 64));
        assert_eq!(request(&prefs, Connection::MobileData), ("raw", 0));
    }
}
