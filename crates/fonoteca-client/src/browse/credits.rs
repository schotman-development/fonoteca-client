//! The album page's Credits shelf: a round tile for everyone credited on the
//! album, once, with their roles and the tracks they are on where not all.

use std::collections::BTreeSet;

use super::*;

/// What the role is of: the album's performing, its writing, its direction
/// or its production, which put its people in that order, other roles last.
fn group_of(role: &str) -> usize {
    match role {
        "artist" | "albumartist" | "performer" | "ensemble" => 0,
        "composer" | "lyricist" => 1,
        "conductor" | "arranger" => 2,
        "producer" | "engineer" | "mixer" | "mastering" => 3,
        _ => 4,
    }
}

/// One role of a person, with the instrument or other sub-role of it.
#[derive(Debug, PartialEq)]
pub(super) struct RoleCredit {
    pub role: String,
    pub sub_role: Option<String>,
}

impl RoleCredit {
    /// "producer", "album artist", "guitar" for a performer of one, and
    /// "engineer (recording)" for any other role with a sub-role.
    fn label(&self) -> String {
        let role = match self.role.as_str() {
            "albumartist" => "album artist",
            other => other,
        };
        match (&self.sub_role, role) {
            (Some(sub), "performer") => sub.clone(),
            (Some(sub), _) => format!("{role} ({sub})"),
            (None, _) => role.to_owned(),
        }
    }
}

#[derive(Debug, PartialEq)]
pub(super) struct CreditedPerson {
    pub name: String,
    /// Empty where the server gives the person no id.
    pub artist_id: String,
    pub roles: Vec<RoleCredit>,
    /// "tracks 1, 3-5"; empty for a person on every track.
    pub tracks: String,
    pub track_count: usize,
}

impl CreditedPerson {
    /// The roles as one line: "Producer, mixer".
    fn roles_line(&self) -> String {
        let mut labels: Vec<String> = Vec::new();
        for label in self.roles.iter().map(RoleCredit::label) {
            if !labels.contains(&label) {
                labels.push(label);
            }
        }
        capitalised(&labels.join(", "))
    }

    /// The first group of any of their roles.
    fn group(&self) -> usize {
        (self.roles.iter())
            .map(|credit| group_of(&credit.role))
            .min()
            .unwrap_or(usize::MAX)
    }

    fn is(&self, credit: &Credited) -> bool {
        match credit.artist_id.is_empty() {
            true => self.artist_id.is_empty() && self.name == credit.name,
            false => self.artist_id == credit.artist_id,
        }
    }
}

fn capitalised(text: &str) -> String {
    let mut letters = text.chars();
    (letters.next().into_iter())
        .flat_map(char::to_uppercase)
        .chain(letters)
        .collect()
}

/// One credit of one song.
struct Credited {
    role: String,
    sub_role: Option<String>,
    name: String,
    artist_id: String,
}

/// The credits of a song: its contributors, or where it has none the
/// composer and artist it names, so a server without the credits shows those.
fn credited(song: &Child) -> Vec<Credited> {
    let plain = |role: &str, name: &str| Credited {
        role: role.into(),
        sub_role: None,
        name: name.into(),
        artist_id: String::new(),
    };
    if song.contributors.is_empty() {
        let composer = (song.display_composer.as_deref()).map(|name| plain("composer", name));
        let artist = (song.display_artist.as_deref().or(song.artist.as_deref()))
            .map(|name| plain("artist", name));
        return composer.into_iter().chain(artist).collect();
    }
    (song.contributors.iter())
        .map(|contributor| Credited {
            role: contributor.role.trim().to_lowercase(),
            sub_role: (contributor.sub_role.as_deref())
                .map(|sub| sub.trim().to_lowercase())
                .filter(|sub| !sub.is_empty()),
            name: contributor.artist.name.clone(),
            artist_id: contributor.artist.id.clone(),
        })
        .collect()
}

/// Everyone credited on the songs of an album, each person once with all
/// their roles and the tracks they are on where that isn't every one: the
/// album's makers first, in each group those on most tracks.
pub(super) fn credits_by_person(songs: &[Child]) -> Vec<CreditedPerson> {
    let places = Places::of(songs);
    // Each person with the songs they are on.
    let mut drafts: Vec<(CreditedPerson, BTreeSet<usize>)> = Vec::new();
    for (index, song) in songs.iter().enumerate() {
        for credit in credited(song) {
            let at = (drafts.iter())
                .position(|(person, _)| person.is(&credit))
                .unwrap_or_else(|| {
                    let person = CreditedPerson {
                        name: credit.name.clone(),
                        artist_id: credit.artist_id.clone(),
                        roles: Vec::new(),
                        tracks: String::new(),
                        track_count: 0,
                    };
                    drafts.push((person, BTreeSet::new()));
                    drafts.len() - 1
                });
            let (person, on) = &mut drafts[at];
            let role = RoleCredit {
                role: credit.role,
                sub_role: credit.sub_role,
            };
            if !person.roles.contains(&role) {
                person.roles.push(role);
            }
            on.insert(index);
        }
    }
    let mut people: Vec<CreditedPerson> = (drafts.into_iter())
        .map(|(mut person, on)| {
            // Stable, so the roles of one group keep the order they came in.
            person.roles.sort_by_key(|credit| group_of(&credit.role));
            person.tracks = places.describe(&on, songs.len());
            person.track_count = on.len();
            person
        })
        .collect();
    people.sort_by(|a, b| {
        (a.group().cmp(&b.group()))
            .then_with(|| b.track_count.cmp(&a.track_count))
            .then_with(|| a.name.cmp(&b.name))
    });
    people
}

/// Where each song of an album is: its disc and track number.
struct Places {
    /// Whether the album has several discs, whose numbers then carry the disc.
    has_discs: bool,
    of_song: Vec<(i64, i64)>,
}

impl Places {
    fn of(songs: &[Child]) -> Self {
        let discs: HashSet<i64> = songs.iter().filter_map(|song| song.disc_number).collect();
        let of_song = (songs.iter().enumerate())
            .map(|(index, song)| {
                let track = song.track.unwrap_or(index as i64 + 1);
                (song.disc_number.unwrap_or(1), track)
            })
            .collect();
        Places {
            has_discs: discs.len() > 1,
            of_song,
        }
    }

    /// "tracks 1, 3-5" for the songs `on`, nothing for all `total` of them.
    fn describe(&self, on: &BTreeSet<usize>, total: usize) -> String {
        if on.len() == total {
            return String::new();
        }
        let mut places: Vec<(i64, i64)> = on.iter().map(|&song| self.of_song[song]).collect();
        places.sort_unstable();
        let mut runs: Vec<Vec<(i64, i64)>> = Vec::new();
        for place in places {
            let continues = |run: &Vec<(i64, i64)>| {
                run.last()
                    .is_some_and(|&(disc, track)| disc == place.0 && track + 1 == place.1)
            };
            match runs.last_mut() {
                Some(run) if continues(run) => run.push(place),
                _ => runs.push(vec![place]),
            }
        }
        let parts: Vec<String> = runs.iter().map(|run| self.run_text(run)).collect();
        let noun = if on.len() == 1 { "track" } else { "tracks" };
        format!("{noun} {}", parts.join(", "))
    }

    /// A run of neighbouring tracks: "3", "3, 4" or "3-5".
    fn run_text(&self, run: &[(i64, i64)]) -> String {
        match run {
            [only] => self.number(*only),
            [first, last] => format!("{}, {}", self.number(*first), self.number(*last)),
            [first, .., last] => format!("{}-{}", self.number(*first), last.1),
            [] => String::new(),
        }
    }

    fn number(&self, (disc, track): (i64, i64)) -> String {
        if self.has_discs {
            format!("{disc}.{track}")
        } else {
            track.to_string()
        }
    }
}

/// What the server says of a credited person's id.
#[derive(Clone, Debug, PartialEq)]
enum Lookup {
    /// An artist, with the id of their picture where they have one.
    Artist {
        cover: Option<String>,
    },
    NotAnArtist,
}

/// The server's error code for what isn't there.
const NOT_FOUND: i64 = 70;

/// The shelf's tiles and the people they are for, and what is known of the
/// ids of people credited on any album this session.
#[derive(Default)]
pub(super) struct Credits {
    tiles: Rc<VecModel<AlbumTile>>,
    people: Vec<CreditedPerson>,
    lookups: HashMap<String, Lookup>,
    /// The ids being asked of the server, so none is asked twice at once.
    asking: HashSet<String>,
}

/// A person's tile: their name, roles and tracks, opening their artist page
/// where they are an artist, and the id of their picture.
fn tile(person: &CreditedPerson, lookup: Option<&Lookup>) -> (AlbumTile, Option<String>) {
    let lines = [person.roles_line(), capitalised(&person.tracks)];
    let (link, cover) = match (person.artist_id.as_str(), lookup) {
        ("", _) | (_, Some(Lookup::NotAnArtist)) => (String::new(), None),
        (id, Some(Lookup::Artist { cover })) => (format!("artist:{id}"), cover.clone()),
        (id, None) => (format!("artist:{id}"), None),
    };
    let lines: Vec<Name> = (lines.into_iter())
        .filter(|line| !line.is_empty())
        .map(unlinked)
        .collect();
    let tile = AlbumTile {
        title: person.name.as_str().into(),
        names: names(lines),
        link: link.into(),
        ..AlbumTile::default()
    };
    (tile, cover)
}

/// The people in `rows` with an id nothing is known of yet, by row.
fn to_look_up(
    people: &[CreditedPerson],
    rows: Range<usize>,
    lookups: &HashMap<String, Lookup>,
) -> Vec<(usize, String)> {
    rows.filter_map(|row| Some((row, people.get(row)?.artist_id.clone())))
        .filter(|(_, id)| !id.is_empty() && !lookups.contains_key(id))
        .collect()
}

/// What the library's artists say of `id`, where it is one of them.
fn from_library(b: &Browser, id: &str) -> Option<Lookup> {
    let artist = b.artists.iter().find(|artist| artist.id == id)?;
    let cover = artist.cover_art.clone().filter(|cover| !cover.is_empty());
    Some(Lookup::Artist { cover })
}

/// What the server's answer to asking for an artist settles, nothing where
/// it didn't (the network failing, say), so it is asked again.
fn lookup_of(answer: opensubsonic::Result<opensubsonic::ArtistWithAlbumsID3>) -> Option<Lookup> {
    match answer {
        Ok(found) => Some(Lookup::Artist {
            cover: found.artist.cover_art.filter(|cover| !cover.is_empty()),
        }),
        Err(opensubsonic::Error::Api(error)) if error.code == NOT_FOUND => {
            Some(Lookup::NotAnArtist)
        }
        Err(_) => None,
    }
}

/// Draws the shelf for the songs of the album on screen, with the pictures
/// the library's artists give; not at all where nothing is credited. The
/// others are asked for as their tiles come into view.
pub(super) fn show(ui: &AppWindow, browser: &Shared, songs: &[Child]) {
    let size = tile_pixels(ui);
    let wanted = {
        let mut b = browser.borrow_mut();
        let people = credits_by_person(songs);
        for person in &people {
            if b.credits.lookups.contains_key(&person.artist_id) {
                continue;
            }
            if let Some(found) = from_library(&b, &person.artist_id) {
                b.credits.lookups.insert(person.artist_id.clone(), found);
            }
        }
        let tiles = (people.iter())
            .map(|person| tile(person, b.credits.lookups.get(&person.artist_id)))
            .collect();
        let (model, wanted) = seen_covers(&b, tiles, size, false);
        b.credits.tiles = model;
        b.credits.people = people;
        ui.set_credits(ModelRc::from(b.credits.tiles.clone()));
        wanted
    };
    side_covers(browser, |b| &b.credits.tiles, wanted, size, false);
}

pub(super) fn clear(ui: &AppWindow, browser: &Shared) {
    let mut b = browser.borrow_mut();
    b.credits.tiles = Rc::default();
    b.credits.people.clear();
    b.credits.asking.clear();
    ui.set_credits(ModelRc::default());
}

/// Asks the server about the people in `rows` nothing is known of, one
/// after the other, and gives their tiles what it answers.
fn look_up(ui: &AppWindow, browser: &Shared, rows: Range<usize>) {
    let (wanted, client, tiles, visit) = {
        let mut b = browser.borrow_mut();
        let Some(client) = b.client.clone() else {
            return;
        };
        if b.offline {
            return;
        }
        let mut wanted = to_look_up(&b.credits.people, rows, &b.credits.lookups);
        wanted.retain(|(_, id)| b.credits.asking.insert(id.clone()));
        (wanted, client, b.credits.tiles.clone(), b.visit)
    };
    let (weak, browser) = (ui.as_weak(), browser.clone());
    slint::spawn_local(async move {
        for (row, id) in wanted {
            let answer = lookup_of(client.get_artist(&id).await);
            let Some(ui) = weak.upgrade() else { return };
            let mut b = browser.borrow_mut();
            if b.visit != visit || !Rc::ptr_eq(&b.credits.tiles, &tiles) {
                return;
            }
            b.credits.asking.remove(&id);
            let Some(found) = answer else { continue };
            b.credits.lookups.insert(id, found.clone());
            let Some(person) = b.credits.people.get(row) else {
                continue;
            };
            let (shown, cover) = tile(person, Some(&found));
            drop(b);
            if let Some(mut current) = tiles.row_data(row) {
                current.link = shown.link;
                tiles.set_row_data(row, current);
            }
            if let Some(cover) = cover {
                let size = tile_pixels(&ui);
                side_covers(
                    &browser,
                    |b| &b.credits.tiles,
                    vec![(row, cover)],
                    size,
                    false,
                );
            }
        }
    })
    .expect("the event loop is running");
}

pub(super) fn wire(ui: &AppWindow, browser: &Shared) {
    ui.on_credits_in_view({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move |first, last| {
            let rows = first.max(0) as usize..(last + 1).max(0) as usize;
            look_up(&weak.unwrap(), &browser, rows);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn song(track: i64, disc: i64, contributors: serde_json::Value) -> Child {
        serde_json::from_value(json!({
            "id": format!("s{disc}.{track}"), "isDir": false, "title": "T",
            "track": track, "discNumber": disc, "contributors": contributors,
        }))
        .unwrap()
    }

    fn who(role: &str, sub: Option<&str>, name: &str, id: &str) -> serde_json::Value {
        json!({ "role": role, "subRole": sub, "artist": { "id": id, "name": name } })
    }

    fn composer(name: &str, id: &str) -> serde_json::Value {
        who("composer", None, name, id)
    }

    fn performer(name: &str, id: &str, sub: &str) -> serde_json::Value {
        who("performer", Some(sub), name, id)
    }

    fn tracks_of(people: &[CreditedPerson]) -> Vec<(&str, &str)> {
        (people.iter())
            .map(|person| (person.name.as_str(), person.tracks.as_str()))
            .collect()
    }

    fn names_of(people: &[CreditedPerson]) -> Vec<&str> {
        people.iter().map(|person| person.name.as_str()).collect()
    }

    #[test]
    fn a_person_on_every_track_has_no_list_and_one_on_some_has_one() {
        let songs: Vec<Child> = (1..=6)
            .map(|n| {
                let mut people = vec![composer("Ada", "a")];
                if [1, 3, 4, 5].contains(&n) {
                    people.push(composer("Ben", "b"));
                }
                song(n, 1, json!(people))
            })
            .collect();
        assert_eq!(
            tracks_of(&credits_by_person(&songs)),
            [("Ada", ""), ("Ben", "tracks 1, 3-5")]
        );
    }

    #[test]
    fn a_pair_of_tracks_is_listed_and_a_single_one_is_named_in_the_singular() {
        let songs = vec![
            song(1, 1, json!([composer("Ada", "a")])),
            song(2, 1, json!([composer("Ada", "a"), composer("Ben", "b")])),
            song(3, 1, json!([composer("Ada", "a"), composer("Cy", "c")])),
            song(4, 1, json!([composer("Ada", "a"), composer("Cy", "c")])),
        ];
        assert_eq!(
            tracks_of(&credits_by_person(&songs)),
            [("Ada", ""), ("Cy", "tracks 3, 4"), ("Ben", "track 2")]
        );
    }

    #[test]
    fn one_person_in_two_roles_is_one_person_with_both_roles() {
        let songs = vec![
            song(1, 1, json!([who("arranger", None, "Vince", "v")])),
            song(2, 1, json!([who("conductor", None, "Vince", "v")])),
        ];
        let people = credits_by_person(&songs);
        assert_eq!(people.len(), 1);
        assert_eq!(people[0].roles_line(), "Arranger, conductor");
        assert_eq!(people[0].tracks, "");
    }

    #[test]
    fn a_performer_is_told_by_instrument_and_other_roles_keep_theirs() {
        let songs = vec![song(
            1,
            1,
            json!([
                performer("Tom", "t", "Guitar"),
                who("producer", None, "Tom", "t"),
                who("mixer", None, "Tom", "t"),
                who("engineer", Some("Recording"), "Tom", "t"),
                who("albumartist", None, "Tom", "t"),
            ]),
        )];
        assert_eq!(
            credits_by_person(&songs)[0].roles_line(),
            "Guitar, album artist, producer, mixer, engineer (recording)"
        );
    }

    #[test]
    fn the_makers_come_first_then_by_most_tracks_then_by_name() {
        let songs = vec![
            song(
                1,
                1,
                json!([
                    who("mixer", None, "Mia", "m"),
                    who("lyricist", None, "Lou", "l"),
                    who("arranger", None, "Ari", "r"),
                    performer("Zed", "z", "drums"),
                    performer("Abe", "e", "bass"),
                    who("other", None, "Odd", "o"),
                ]),
            ),
            song(2, 1, json!([performer("Zed", "z", "drums")])),
        ];
        assert_eq!(
            names_of(&credits_by_person(&songs)),
            ["Zed", "Abe", "Lou", "Ari", "Mia", "Odd"]
        );
    }

    #[test]
    fn a_person_without_an_id_is_told_by_name_and_has_no_id() {
        let songs = vec![
            song(1, 1, json!([composer("Ada", "")])),
            song(2, 1, json!([composer("Ada", ""), composer("Bo", "")])),
        ];
        let people = credits_by_person(&songs);
        assert_eq!(tracks_of(&people), [("Ada", ""), ("Bo", "track 2")]);
        assert_eq!(people[0].artist_id, "");
    }

    #[test]
    fn tracks_of_an_album_of_several_discs_carry_their_disc() {
        let songs = vec![
            song(1, 1, json!([composer("Ada", "a")])),
            song(2, 1, json!([composer("Ada", "a"), composer("Ben", "b")])),
            song(1, 2, json!([composer("Ada", "a"), composer("Ben", "b")])),
            song(2, 2, json!([composer("Ada", "a")])),
        ];
        assert_eq!(
            tracks_of(&credits_by_person(&songs)),
            [("Ada", ""), ("Ben", "tracks 1.2, 2.1")]
        );
    }

    #[test]
    fn a_role_nobody_knows_is_shown_under_its_name_after_the_known_ones() {
        let songs = vec![song(
            1,
            1,
            json!([
                who("remixer", None, "Zed", "z"),
                who("producer", None, "Pat", "p")
            ]),
        )];
        let people = credits_by_person(&songs);
        assert_eq!(names_of(&people), ["Pat", "Zed"]);
        assert_eq!(people[1].roles_line(), "Remixer");
    }

    #[test]
    fn songs_without_contributors_give_their_composer_and_artist() {
        let song: Child = serde_json::from_value(json!({
            "id": "s", "isDir": false, "title": "T",
            "displayComposer": "Ada", "displayArtist": "The Band",
        }))
        .unwrap();
        let found: Vec<(String, String)> = (credits_by_person(&[song]).iter())
            .map(|person| (person.name.clone(), person.roles_line()))
            .collect();
        assert_eq!(
            found,
            [
                ("The Band".to_owned(), "Artist".to_owned()),
                ("Ada".to_owned(), "Composer".to_owned())
            ]
        );
    }

    #[test]
    fn no_credits_at_all_give_no_people() {
        let song: Child =
            serde_json::from_value(json!({ "id": "s", "isDir": false, "title": "T" })).unwrap();
        assert!(credits_by_person(&[song]).is_empty());
    }

    #[test]
    fn a_tile_has_the_name_the_roles_and_the_tracks_and_opens_an_artist() {
        let songs = vec![
            song(1, 1, json!([composer("Ada", "a")])),
            song(
                2,
                1,
                json!([composer("Ada", "a"), performer("Ben", "b", "Guitar")]),
            ),
        ];
        let people = credits_by_person(&songs);
        let found = Lookup::Artist {
            cover: Some("c".into()),
        };
        let (ben, cover) = tile(&people[0], Some(&found));
        assert_eq!(ben.title, "Ben");
        let lines: Vec<String> = (ben.names.iter())
            .map(|name| name.label.to_string())
            .collect();
        assert_eq!(lines, ["Guitar", "Track 2"]);
        assert_eq!(
            (ben.link.as_str(), cover.as_deref()),
            ("artist:b", Some("c"))
        );
    }

    #[test]
    fn a_person_who_is_not_an_artist_or_has_no_id_has_a_tile_that_opens_nothing() {
        let songs = vec![song(
            1,
            1,
            json!([composer("Ada", "a"), composer("Bo", "")]),
        )];
        let people = credits_by_person(&songs);
        assert_eq!(tile(&people[0], Some(&Lookup::NotAnArtist)).0.link, "");
        assert_eq!(tile(&people[1], None).0.link, "");
        assert_eq!(tile(&people[0], None).0.link, "artist:a");
    }

    #[test]
    fn only_people_with_an_unknown_id_in_view_are_looked_up() {
        let songs = vec![song(
            1,
            1,
            json!([
                composer("Ada", "a"),
                composer("Ben", "b"),
                composer("Cy", "c"),
                composer("Di", "")
            ]),
        )];
        let people = credits_by_person(&songs);
        let lookups = HashMap::from([("b".to_owned(), Lookup::NotAnArtist)]);
        assert_eq!(
            to_look_up(&people, 0..4, &lookups),
            [(0, "a".to_owned()), (2, "c".to_owned())]
        );
        assert!(to_look_up(&people, 3..9, &lookups).is_empty());
    }

    #[test]
    fn only_an_answer_that_is_not_found_marks_a_person_as_no_artist() {
        let not_found = opensubsonic::Error::Api(opensubsonic::ApiError {
            code: NOT_FOUND,
            message: Some("No such artist".into()),
            help_url: None,
        });
        assert_eq!(lookup_of(Err(not_found)), Some(Lookup::NotAnArtist));
        assert_eq!(lookup_of(Err(opensubsonic::Error::Status(500))), None);
    }
}
