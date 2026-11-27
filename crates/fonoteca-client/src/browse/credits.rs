//! The album page's Credits shelf: everyone credited on the album once,
//! grouped by role, with the tracks they are on where not all.

use std::collections::{BTreeSet, HashSet};

use super::*;
use crate::{CreditPerson, CreditSection};

/// How many people a role shows before "Show all".
const PEOPLE_PER_ROLE: usize = 8;

/// Roles the shelf knows, in the order it lists them, with their headings.
/// Any other role comes after them under its own name.
const KNOWN_ROLES: [(&str, &str); 11] = [
    ("artist", "Artists"),
    ("albumartist", "Album artists"),
    ("composer", "Composers"),
    ("lyricist", "Lyricists"),
    ("performer", "Performers"),
    ("conductor", "Conductors"),
    ("ensemble", "Ensembles"),
    ("producer", "Producers"),
    ("engineer", "Engineers"),
    ("mixer", "Mixing"),
    ("mastering", "Mastering"),
];

/// The people of one role, or of one instrument or other sub-role of it.
#[derive(Debug, PartialEq)]
pub(super) struct CreditGroup {
    pub role: String,
    pub sub_role: Option<String>,
    pub people: Vec<Credit>,
}

#[derive(Debug, PartialEq)]
pub(super) struct Credit {
    pub name: String,
    /// Empty where the server gives the person no id.
    pub artist_id: String,
    /// "tracks 1, 3-5"; empty for a person on every track.
    pub tracks: String,
}

impl CreditGroup {
    fn heading(&self) -> String {
        let known = KNOWN_ROLES.iter().find(|(role, _)| *role == self.role);
        let role = known.map_or_else(
            || capitalised(&self.role),
            |(_, heading)| heading.to_string(),
        );
        match (&self.sub_role, self.role.as_str()) {
            (Some(sub), "performer") => capitalised(sub),
            (Some(sub), _) => format!("{role} · {sub}"),
            (None, _) => role,
        }
    }

    fn rank(&self) -> usize {
        (KNOWN_ROLES.iter())
            .position(|(role, _)| *role == self.role)
            .unwrap_or(KNOWN_ROLES.len())
    }

    /// The group's person `credit` names, added when not there yet; their
    /// place in `people`.
    fn person_of(&mut self, credit: &Credited) -> usize {
        let is_them = |person: &Credit| match credit.artist_id.is_empty() {
            true => person.artist_id.is_empty() && person.name == credit.name,
            false => person.artist_id == credit.artist_id,
        };
        if let Some(at) = self.people.iter().position(is_them) {
            return at;
        }
        self.people.push(Credit {
            name: credit.name.clone(),
            artist_id: credit.artist_id.clone(),
            tracks: String::new(),
        });
        self.people.len() - 1
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

/// Everyone credited on the songs of an album, by role: each person once
/// per role, with the tracks they are on where that isn't every one.
pub(super) fn credits(songs: &[Child]) -> Vec<CreditGroup> {
    let places = Places::of(songs);
    // Each group with, for each of its people, the songs they are on.
    let mut drafts: Vec<(CreditGroup, Vec<BTreeSet<usize>>)> = Vec::new();
    for (index, song) in songs.iter().enumerate() {
        for credit in credited(song) {
            let at = drafts
                .iter()
                .position(|(group, _)| {
                    group.role == credit.role && group.sub_role == credit.sub_role
                })
                .unwrap_or_else(|| {
                    let group = CreditGroup {
                        role: credit.role.clone(),
                        sub_role: credit.sub_role.clone(),
                        people: Vec::new(),
                    };
                    drafts.push((group, Vec::new()));
                    drafts.len() - 1
                });
            let (group, on) = &mut drafts[at];
            let person = group.person_of(&credit);
            on.resize_with(group.people.len(), BTreeSet::new);
            on[person].insert(index);
        }
    }
    let mut groups: Vec<CreditGroup> = (drafts.into_iter())
        .map(|(mut group, on)| {
            for (person, songs_on) in group.people.iter_mut().zip(on) {
                person.tracks = places.describe(&songs_on, songs.len());
            }
            group
        })
        .collect();
    // Stable, so the sub-roles of one role and the unknown roles keep the order they came in.
    groups.sort_by_key(CreditGroup::rank);
    groups
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

/// The groups as the page draws them, with each cut to its first people
/// unless `all`.
fn sections(groups: &[CreditGroup], all: bool) -> ModelRc<CreditSection> {
    let limit = if all { usize::MAX } else { PEOPLE_PER_ROLE };
    let sections: Vec<CreditSection> = (groups.iter())
        .map(|group| {
            let people: Vec<CreditPerson> = (group.people.iter().take(limit))
                .map(|person| CreditPerson {
                    label: person.name.as_str().into(),
                    link: match person.artist_id.is_empty() {
                        true => "".into(),
                        false => format!("artist:{}", person.artist_id).into(),
                    },
                    tracks: person.tracks.as_str().into(),
                })
                .collect();
            CreditSection {
                heading: group.heading().into(),
                people: ModelRc::new(VecModel::from(people)),
            }
        })
        .collect();
    ModelRc::new(VecModel::from(sections))
}

/// Draws the shelf for the songs of the album on screen; not at all where
/// nothing is credited.
pub(super) fn show(ui: &AppWindow, songs: &[Child]) {
    let groups = credits(songs);
    let is_cut = groups
        .iter()
        .any(|group| group.people.len() > PEOPLE_PER_ROLE);
    ui.set_credits(sections(&groups, false));
    ui.set_credits_more(is_cut);
}

pub(super) fn clear(ui: &AppWindow) {
    ui.set_credits(ModelRc::default());
    ui.set_credits_more(false);
}

pub(super) fn wire(ui: &AppWindow, browser: &Shared) {
    ui.on_show_all_credits({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move || {
            let ui = weak.unwrap();
            let groups = credits(&browser.borrow().songs);
            ui.set_credits(sections(&groups, true));
            ui.set_credits_more(false);
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

    fn tracks_of(group: &CreditGroup) -> Vec<(&str, &str)> {
        (group.people.iter())
            .map(|person| (person.name.as_str(), person.tracks.as_str()))
            .collect()
    }

    fn headings(songs: &[Child]) -> Vec<String> {
        credits(songs).iter().map(CreditGroup::heading).collect()
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
        let groups = credits(&songs);
        assert_eq!(groups.len(), 1);
        assert_eq!(
            tracks_of(&groups[0]),
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
        let groups = credits(&songs);
        assert_eq!(
            tracks_of(&groups[0]),
            [("Ada", ""), ("Ben", "track 2"), ("Cy", "tracks 3, 4")]
        );
    }

    #[test]
    fn one_person_in_two_roles_is_in_both_groups() {
        let songs = vec![song(
            1,
            1,
            json!([composer("Ada", "a"), who("producer", None, "Ada", "a")]),
        )];
        assert_eq!(headings(&songs), ["Composers", "Producers"]);
    }

    #[test]
    fn performers_are_grouped_by_instrument_and_come_after_the_composers() {
        let songs = vec![song(
            1,
            1,
            json!([
                who("performer", Some("Guitar"), "Tom", "t"),
                who("performer", Some("drums"), "Steve", "s"),
                composer("Ada", "a"),
            ]),
        )];
        assert_eq!(headings(&songs), ["Composers", "Guitar", "Drums"]);
    }

    #[test]
    fn a_person_without_an_id_is_told_by_name_and_has_no_link() {
        let songs = vec![
            song(1, 1, json!([composer("Ada", "")])),
            song(2, 1, json!([composer("Ada", ""), composer("Bo", "")])),
        ];
        let groups = credits(&songs);
        assert_eq!(tracks_of(&groups[0]), [("Ada", ""), ("Bo", "track 2")]);
        assert_eq!(groups[0].people[0].artist_id, "");
    }

    #[test]
    fn tracks_of_an_album_of_several_discs_carry_their_disc() {
        let songs = vec![
            song(1, 1, json!([composer("Ada", "a")])),
            song(2, 1, json!([composer("Ada", "a"), composer("Ben", "b")])),
            song(1, 2, json!([composer("Ada", "a"), composer("Ben", "b")])),
            song(2, 2, json!([composer("Ada", "a")])),
        ];
        let groups = credits(&songs);
        assert_eq!(
            tracks_of(&groups[0]),
            [("Ada", ""), ("Ben", "tracks 1.2, 2.1")]
        );
    }

    #[test]
    fn a_role_nobody_knows_is_shown_under_its_name_after_the_known_ones() {
        let songs = vec![song(
            1,
            1,
            json!([
                who("arranger", None, "Zed", "z"),
                who("producer", None, "Pat", "p")
            ]),
        )];
        assert_eq!(headings(&songs), ["Producers", "Arranger"]);
    }

    #[test]
    fn songs_without_contributors_give_their_composer_and_artist() {
        let song: Child = serde_json::from_value(json!({
            "id": "s", "isDir": false, "title": "T",
            "displayComposer": "Ada", "displayArtist": "The Band",
        }))
        .unwrap();
        let groups = credits(&[song]);
        let found: Vec<(&str, &str)> = (groups.iter())
            .map(|group| (group.role.as_str(), group.people[0].name.as_str()))
            .collect();
        assert_eq!(found, [("artist", "The Band"), ("composer", "Ada")]);
    }

    #[test]
    fn no_credits_at_all_give_no_groups() {
        let song: Child =
            serde_json::from_value(json!({ "id": "s", "isDir": false, "title": "T" })).unwrap();
        assert!(credits(&[song]).is_empty());
    }
}
