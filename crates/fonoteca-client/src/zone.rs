//! The country the device is in, as its time zone tells: Europe/Amsterdam is
//! the Netherlands. Nothing leaves the device; zones.txt, from the time zone
//! database, maps one to the other.

/// The time zone Android reports, which the app learns at start.
#[cfg(target_os = "android")]
pub static ANDROID: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// The time zone the tests pretend to be in.
#[cfg(test)]
pub static TESTS: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());

/// The device's country as an ISO 3166-1 code, when its time zone names one.
pub fn country() -> Option<&'static str> {
    country_of(&zone()?)
}

fn country_of(zone: &str) -> Option<&'static str> {
    let table = include_str!("zones.txt").lines();
    table
        .filter(|line| !line.starts_with('#'))
        .find_map(|line| {
            let (name, code) = line.split_once(' ')?;
            (name == zone).then_some(code)
        })
}

#[cfg(test)]
fn zone() -> Option<String> {
    Some(TESTS.lock().unwrap().clone())
}

#[cfg(all(not(test), target_os = "android"))]
fn zone() -> Option<String> {
    ANDROID.get().cloned()
}

#[cfg(all(not(test), target_family = "wasm"))]
fn zone() -> Option<String> {
    let format = js_sys::Intl::DateTimeFormat::new(&js_sys::Array::new(), &js_sys::Object::new());
    let options = format.resolved_options();
    js_sys::Reflect::get(&options, &"timeZone".into())
        .ok()?
        .as_string()
}

/// TZ when it names a zone, else the zone /etc/localtime links to, as on
/// Linux, the BSDs and macOS, else Debian's /etc/timezone.
#[cfg(all(not(test), not(target_os = "android"), not(target_family = "wasm")))]
fn zone() -> Option<String> {
    let named = std::env::var("TZ")
        .ok()
        .map(|tz| tz.trim_start_matches(':').to_owned())
        .filter(|tz| country_of(tz).is_some());
    let linked = || {
        let target = std::fs::read_link("/etc/localtime").ok()?;
        let (_, zone) = target.to_str()?.split_once("zoneinfo/")?;
        let zone = zone
            .trim_start_matches("posix/")
            .trim_start_matches("right/");
        Some(zone.to_owned())
    };
    let written = || {
        Some(
            std::fs::read_to_string("/etc/timezone")
                .ok()?
                .trim()
                .to_owned(),
        )
    };
    named.or_else(linked).or_else(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn countries_of_zones_and_their_old_names() {
        assert_eq!(country_of("Europe/Amsterdam"), Some("NL"));
        assert_eq!(country_of("Europe/Brussels"), Some("BE"));
        assert_eq!(country_of("Asia/Calcutta"), Some("IN"));
        assert_eq!(country_of("Etc/UTC"), None);
        assert_eq!(country_of("#"), None);
    }
}
