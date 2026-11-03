//! The format and bitrate to stream songs in, picked in the settings for wifi
//! and for mobile data, and which of the two the connection in use calls for.

use crate::prefs::{Prefs, StreamQuality};

/// The settings' formats, in their order: what the picker shows and the
/// `format` the server is asked for. The first is the song as it is kept.
const FORMATS: [&str; 4] = ["raw", "mp3", "opus", "aac"];
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

#[cfg(not(target_os = "android"))]
fn on_mobile_data() -> bool {
    false
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
    let StreamQuality { format, bitrate } = within_lists(match connection {
        Connection::Wifi => prefs.wifi_quality,
        Connection::MobileData => prefs.mobile_quality,
    });
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
    fn a_saved_entry_out_of_range_is_held_to_the_list() {
        let prefs = prefs((9, 40), (-1, -1));
        assert_eq!(request(&prefs, Connection::Wifi), ("aac", 64));
        assert_eq!(request(&prefs, Connection::MobileData), ("raw", 0));
    }
}
