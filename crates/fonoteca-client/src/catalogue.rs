//! The server's curated radio stations, shelved the way the radio page
//! shelves the directory: the device's country, the languages picked, then
//! genres by how much of them the library holds. A server that gives no
//! country, language or genre (version 1) gets one plain shelf.

use opensubsonic::CatalogueStation;

/// A shelf of the catalogue: its title, and its stations as places in the
/// catalogue.
#[derive(Debug, PartialEq)]
pub struct Shelf {
    pub title: String,
    pub stations: Vec<usize>,
}

const PLAIN_TITLE: &str = "Curated stations";
const LEFTOVER_TITLE: &str = "Curated · More stations";

/// The genres of spoken stations. Those stations sit on their country's and
/// language's shelves like any, and their genres' shelves come after the
/// music's.
const SPOKEN_GENRES: [&str; 2] = ["news", "talk"];

/// The languages the catalogue's stations speak, by ISO 639-1 code, named
/// as the radio directory names them in lower case.
const LANGUAGES: [(&str, &str); 10] = [
    ("nl", "dutch"),
    ("en", "english"),
    ("de", "german"),
    ("fr", "french"),
    ("es", "spanish"),
    ("it", "italian"),
    ("ca", "catalan"),
    ("eu", "basque"),
    ("gl", "galician"),
    ("pt", "portuguese"),
];
/// The countries the catalogue's stations are in, by ISO 3166-1 code, as a
/// title reads them after "in".
const COUNTRIES: [(&str, &str); 7] = [
    ("NL", "the Netherlands"),
    ("US", "the United States"),
    ("CA", "Canada"),
    ("DE", "Germany"),
    ("FR", "France"),
    ("ES", "Spain"),
    ("IT", "Italy"),
];

/// A language's name for a code, as the directory writes it.
fn language_name(code: &str) -> Option<&'static str> {
    let code = code.to_ascii_lowercase();
    LANGUAGES
        .iter()
        .find(|(known, _)| *known == code)
        .map(|(_, name)| *name)
}

fn country_name(code: &str) -> &str {
    let code = code.to_ascii_uppercase();
    COUNTRIES
        .iter()
        .find(|(known, _)| *known == code)
        .map_or("somewhere else", |(_, name)| name)
}

fn speaks(station: &CatalogueStation, language: &str) -> bool {
    station
        .language_codes
        .iter()
        .any(|code| language_name(code) == Some(language))
}

fn has_details(station: &CatalogueStation) -> bool {
    station.country_code.is_some()
        || !station.language_codes.is_empty()
        || !station.genres.is_empty()
}

/// Puts a shelf last, unless it has no stations.
fn add(shelves: &mut Vec<Shelf>, title: String, stations: Vec<usize>) {
    if !stations.is_empty() {
        shelves.push(Shelf { title, stations });
    }
}

/// A word with its first letter capital, for a title.
fn capital(word: &str) -> String {
    let mut letters = word.chars();
    letters.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(letters).collect()
    })
}

/// The catalogue's shelves. `languages` are the ones picked, in lower case
/// in the order picked; `albums_in` says how many albums the library has of
/// a genre; `local` is the device's country code.
pub fn shelves(
    stations: &[CatalogueStation],
    languages: &[String],
    local: Option<&str>,
    albums_in: impl Fn(&str) -> i64,
    genre_title: impl Fn(&str) -> String,
) -> Vec<Shelf> {
    if stations.is_empty() {
        return Vec::new();
    }
    if !stations.iter().any(has_details) {
        return vec![Shelf {
            title: PLAIN_TITLE.into(),
            stations: (0..stations.len()).collect(),
        }];
    }
    let picked_first = |mut places: Vec<usize>| {
        places.sort_by_key(|&place| {
            !languages
                .iter()
                .any(|language| speaks(&stations[place], language))
        });
        places
    };
    let in_catalogue = |wanted: &dyn Fn(&CatalogueStation) -> bool| -> Vec<usize> {
        (0..stations.len())
            .filter(|&place| wanted(&stations[place]))
            .collect()
    };
    let mut shelves = Vec::new();
    if let Some(local) = local {
        let there = in_catalogue(&|station| {
            station
                .country_code
                .as_deref()
                .is_some_and(|code| code.eq_ignore_ascii_case(local))
        });
        add(
            &mut shelves,
            format!("Curated · in {}", country_name(local)),
            there,
        );
    }
    for language in languages {
        let speakers = in_catalogue(&|station| speaks(station, language));
        add(
            &mut shelves,
            format!("Curated · {}", capital(language)),
            speakers,
        );
    }
    for genre in genres_by_albums(stations, &albums_in) {
        let tagged = in_catalogue(&|station| {
            station
                .genres
                .iter()
                .any(|name| name.eq_ignore_ascii_case(&genre))
        });
        add(
            &mut shelves,
            format!("Curated · {}", genre_title(&genre)),
            picked_first(tagged),
        );
    }
    let shelved: Vec<usize> = shelves
        .iter()
        .flat_map(|shelf| shelf.stations.clone())
        .collect();
    let rest = in_catalogue(&|_| true)
        .into_iter()
        .filter(|place| !shelved.contains(place))
        .collect();
    add(&mut shelves, LEFTOVER_TITLE.into(), rest);
    shelves
}

/// The genres the stations have, each once as first written: those the
/// library has the most albums of first, then those with the most stations,
/// then by name.
fn genres_by_albums(
    stations: &[CatalogueStation],
    albums_in: &impl Fn(&str) -> i64,
) -> Vec<String> {
    let mut genres: Vec<(String, usize)> = Vec::new();
    for name in stations.iter().flat_map(|station| &station.genres) {
        match genres
            .iter_mut()
            .find(|(known, _)| known.eq_ignore_ascii_case(name))
        {
            Some((_, count)) => *count += 1,
            None => genres.push((name.clone(), 1)),
        }
    }
    let spoken = |genre: &str| {
        SPOKEN_GENRES
            .iter()
            .any(|name| name.eq_ignore_ascii_case(genre))
    };
    genres.sort_by(|(a, a_count), (b, b_count)| {
        spoken(a)
            .cmp(&spoken(b))
            .then_with(|| albums_in(b).cmp(&albums_in(a)))
            .then(b_count.cmp(a_count))
            .then_with(|| a.to_lowercase().cmp(&b.to_lowercase()))
    });
    genres.into_iter().map(|(name, _)| name).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn station(
        name: &str,
        country: Option<&str>,
        languages: &[&str],
        genres: &[&str],
    ) -> CatalogueStation {
        CatalogueStation {
            id: format!("rc-{name}.xspf"),
            name: name.into(),
            stream_url: format!("https://example.org/{name}"),
            home_page_url: None,
            cover_art: None,
            country_code: country.map(Into::into),
            language_codes: languages.iter().map(|&code| code.into()).collect(),
            genres: genres.iter().map(|&genre| genre.into()).collect(),
        }
    }

    fn shelved(
        stations: &[CatalogueStation],
        languages: &[String],
        local: Option<&str>,
        albums_in: impl Fn(&str) -> i64,
    ) -> Vec<Shelf> {
        let titled = |genre: &str| genre[..1].to_uppercase() + &genre[1..];
        shelves(stations, languages, local, albums_in, titled)
    }

    fn titles(shelves: &[Shelf]) -> Vec<&str> {
        shelves.iter().map(|shelf| shelf.title.as_str()).collect()
    }

    fn catalogue() -> Vec<CatalogueStation> {
        vec![
            station("a", Some("NL"), &["nl"], &["pop", "news"]),
            station("b", Some("DE"), &["de"], &["jazz"]),
            station("c", Some("DE"), &["de", "nl"], &["jazz", "Pop"]),
            station("d", Some("ES"), &["es"], &[]),
            station("e", None, &[], &[]),
        ]
    }

    #[test]
    fn a_catalogue_without_details_is_one_plain_shelf() {
        let plain = vec![station("a", None, &[], &[]), station("b", None, &[], &[])];
        let shelves = shelved(&plain, &["dutch".into()], Some("NL"), |_| 5);
        assert_eq!(
            shelves,
            [Shelf {
                title: "Curated stations".into(),
                stations: vec![0, 1]
            }]
        );
        assert!(shelved(&[], &[], None, |_| 0).is_empty());
    }

    #[test]
    fn the_local_country_comes_first_then_the_languages_then_the_genres() {
        let albums = |genre: &str| if genre == "jazz" { 9 } else { 0 };
        let shelves = shelved(&catalogue(), &["dutch".into()], Some("DE"), albums);
        assert_eq!(
            titles(&shelves),
            [
                "Curated · in Germany",
                "Curated · Dutch",
                "Curated · Jazz",
                "Curated · Pop",
                "Curated · News",
                "Curated · More stations",
            ]
        );
        assert_eq!(shelves[0].stations, [1, 2]);
        assert_eq!(shelves[1].stations, [0, 2]);
    }

    #[test]
    fn a_country_without_stations_has_no_shelf() {
        let shelves = shelved(&catalogue(), &[], Some("US"), |_| 0);
        assert!(
            !titles(&shelves)
                .iter()
                .any(|title| title.contains("United States"))
        );
        let shelves = shelved(&catalogue(), &[], None, |_| 0);
        assert!(!titles(&shelves).iter().any(|title| title.contains(" in ")));
    }

    #[test]
    fn genres_go_by_the_librarys_albums_then_stations_then_name() {
        let albums = |genre: &str| if genre == "pop" { 3 } else { 0 };
        let shelves = shelved(&catalogue(), &[], None, albums);
        assert_eq!(
            titles(&shelves),
            [
                "Curated · Pop",
                "Curated · Jazz",
                "Curated · News",
                "Curated · More stations"
            ]
        );
        // Without albums Jazz and Pop tie on two stations, so by name.
        let shelves = shelved(&catalogue(), &[], None, |_| 0);
        assert_eq!(titles(&shelves)[..2], ["Curated · Jazz", "Curated · Pop"]);
        let fewer = vec![
            station("a", None, &[], &["x"]),
            station("b", None, &[], &["y"]),
            station("c", None, &[], &["y"]),
        ];
        assert_eq!(
            titles(&shelved(&fewer, &[], None, |_| 0))[..2],
            ["Curated · Y", "Curated · X"]
        );
    }

    #[test]
    fn spoken_genres_come_after_music_whatever_the_albums() {
        let spoken = vec![
            station("a", None, &[], &["news"]),
            station("b", None, &[], &["news"]),
            station("c", None, &[], &["talk"]),
            station("d", None, &[], &["jazz"]),
        ];
        let shelves = shelved(
            &spoken,
            &[],
            None,
            |genre| if genre == "news" { 50 } else { 0 },
        );
        assert_eq!(
            titles(&shelves),
            ["Curated · Jazz", "Curated · News", "Curated · Talk"]
        );
    }

    #[test]
    fn a_spoken_station_is_on_its_country_and_language_shelves() {
        let spoken = vec![station("a", Some("NL"), &["nl"], &["news"])];
        let shelves = shelved(&spoken, &["dutch".into()], Some("NL"), |_| 0);
        assert_eq!(
            titles(&shelves)[..2],
            ["Curated · in the Netherlands", "Curated · Dutch"]
        );
        assert_eq!(shelves[0].stations, [0]);
    }

    #[test]
    fn a_genre_written_two_ways_is_one_shelf_with_both_stations() {
        let shelves = shelved(&catalogue(), &[], None, |_| 0);
        let pop = shelves
            .iter()
            .find(|shelf| shelf.title == "Curated · Pop")
            .unwrap();
        assert_eq!(pop.stations, [0, 2]);
    }

    #[test]
    fn a_genre_shelf_puts_the_picked_languages_first() {
        let shelves = shelved(&catalogue(), &["dutch".into()], None, |_| 0);
        let jazz = shelves
            .iter()
            .find(|shelf| shelf.title == "Curated · Jazz")
            .unwrap();
        assert_eq!(jazz.stations, [2, 1]);
    }

    #[test]
    fn stations_in_no_shelf_are_left_for_the_last_one() {
        let shelves = shelved(&catalogue(), &[], None, |_| 0);
        assert_eq!(shelves.last().unwrap().stations, [3, 4]);
    }

    #[test]
    fn a_language_nobody_speaks_has_no_shelf() {
        let shelves = shelved(
            &catalogue(),
            &["klingon".into(), "german".into()],
            None,
            |_| 0,
        );
        assert_eq!(titles(&shelves)[0], "Curated · German");
        assert!(
            !titles(&shelves)
                .iter()
                .any(|title| title.contains("Klingon"))
        );
    }
}
