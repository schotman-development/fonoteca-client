//! The fields each overview page's items can be filtered by, and the
//! filters its panel opens with: one list for a page, from which the "Add
//! filter" menu, the counts and the filters themselves are made.

use opensubsonic::{
    AlbumID3, Child, Composer, Composition, ExplicitStatus, ItemDate, ItemGenre, Playlist,
};

use crate::focus::{
    FAVOURITE, Field, Info, Measure, date, decade, descending, flag, infos, number, text, values,
};
use crate::rules::{FieldValue, day_number};
use crate::smart::Field as SongField;

use super::album_names;
use crate::{Name, Page};

pub(super) const BASICS: &str = "Basics";
pub(super) const MUSIC: &str = "Music";
pub(super) const AUDIO: &str = "Audio";
pub(super) const PLAYS: &str = "Plays and dates";
pub(super) const TAGS: &str = "Tags";
pub(super) const LIBRARY: &str = "Library";

/// The headings of the "Add filter" menu, in the order it lists them.
pub(super) const GROUPS: [&str; 6] = [BASICS, MUSIC, AUDIO, PLAYS, TAGS, LIBRARY];

const LOSSLESS_SUFFIXES: [&str; 9] = [
    "flac", "alac", "wav", "aif", "aiff", "ape", "wv", "dsf", "dff",
];
/// What Roon calls hi-res: more than CD's depth or rate.
const HI_RES_DEPTH: i64 = 16;
const HI_RES_RATE: i64 = 48_000;
const PRODUCTION_ROLES: [&str; 3] = ["producer", "engineer", "mixer"];

/// What an artist is told apart by on the artists page: the artist's own
/// facts, the genres of their albums and how often those were played.
#[derive(Clone, Default)]
pub(super) struct ArtistFacts {
    pub name: String,
    pub albums: Option<i64>,
    pub music_brainz_id: Option<String>,
    pub genres: Vec<String>,
    pub plays: i64,
    pub starred: bool,
}

pub(super) const ALBUM_FIELDS: &[Field<AlbumID3>] = &[
    text("title", "Title", BASICS, |album, _| {
        FieldValue::Texts(vec![album.name.clone()])
    }),
    text("version", "Version", BASICS, |album, _| {
        optional(&album.version)
    }),
    values("artist", "Artists", BASICS, |album, _| {
        FieldValue::Texts(labels(album_names(album)))
    }),
    number("songCount", "Songs", BASICS, Measure::Count, |album, _| {
        FieldValue::Number(Some(album.song_count))
    }),
    number(
        "duration",
        "Duration",
        BASICS,
        Measure::Duration,
        |album, _| FieldValue::Number(Some(album.duration)),
    ),
    flag("compilation", "Compilation", BASICS, |album, _| {
        FieldValue::Flag(album.is_compilation == Some(true))
    }),
    flag("explicit", "Explicit", BASICS, |album, _| {
        FieldValue::Flag(album.explicit_status == Some(ExplicitStatus::Explicit))
    }),
    values("genre", "Genres", MUSIC, |album, _| {
        FieldValue::Texts(album_genres(album))
    }),
    descending("decade", "Release decade", MUSIC, |album, _| {
        decade_of(album_year(album))
    }),
    number("year", "Release year", MUSIC, Measure::Count, |album, _| {
        FieldValue::Number(album_year(album))
    }),
    date("releaseDate", "Release date", MUSIC, |album, _| {
        item_date(&album.release_date)
    }),
    date(
        "originalReleaseDate",
        "Original release date",
        MUSIC,
        |album, _| item_date(&album.original_release_date),
    ),
    values("recordLabel", "Record labels", MUSIC, |album, _| {
        FieldValue::Texts(
            album
                .record_labels
                .iter()
                .map(|label| label.name.clone())
                .collect(),
        )
    }),
    values("mood", "Moods", MUSIC, |album, _| {
        FieldValue::Texts(album.moods.clone())
    }),
    values("releaseType", "Release type", MUSIC, |album, _| {
        FieldValue::Texts(album.release_types.clone())
    }),
    number("rating", "Rating", PLAYS, Measure::Count, |album, _| {
        FieldValue::Number(album.user_rating.filter(|&stars| stars > 0))
    }),
    date("added", "Added", PLAYS, |album, _| {
        FieldValue::Day(day_number(&album.created))
    }),
    date("lastPlayed", "Last played", PLAYS, |album, _| {
        day_of(&album.played)
    }),
    number("playCount", "Plays", PLAYS, Measure::Plays, |album, _| {
        FieldValue::Number(Some(album.play_count.unwrap_or(0)))
    }),
    flag(FAVOURITE, "Favourite", PLAYS, |album, _| {
        FieldValue::Flag(album.starred.is_some())
    }),
    text("musicBrainzId", "MusicBrainz ID", LIBRARY, |album, _| {
        optional(&album.music_brainz_id)
    }),
];

pub(super) const SONG_FIELDS: &[Field<Child>] = &[
    text("title", "Title", BASICS, |song, now| {
        SongField::Title.value_of(song, now)
    }),
    text("album", "Album", BASICS, |song, now| {
        SongField::Album.value_of(song, now)
    }),
    number("track", "Track", BASICS, Measure::Count, |song, _| {
        FieldValue::Number(song.track)
    }),
    number("disc", "Disc", BASICS, Measure::Count, |song, _| {
        FieldValue::Number(song.disc_number)
    }),
    number(
        "duration",
        "Duration",
        BASICS,
        Measure::Duration,
        |song, now| SongField::Duration.value_of(song, now),
    ),
    flag("explicit", "Explicit", BASICS, |song, _| {
        FieldValue::Flag(song.explicit_status == Some(ExplicitStatus::Explicit))
    }),
    values("genre", "Genres", MUSIC, |song, now| {
        SongField::Genre.value_of(song, now)
    }),
    descending("decade", "Release decade", MUSIC, |song, _| {
        decade_of(song.year)
    }),
    number("year", "Year", MUSIC, Measure::Count, |song, now| {
        SongField::Year.value_of(song, now)
    }),
    values("artist", "Performers", MUSIC, |song, now| {
        SongField::Artist.value_of(song, now)
    }),
    values("albumArtist", "Album artists", MUSIC, |song, now| {
        SongField::AlbumArtist.value_of(song, now)
    }),
    values("composer", "Composers", MUSIC, |song, now| {
        SongField::Composer.value_of(song, now)
    }),
    values("production", "Production", MUSIC, |song, _| {
        FieldValue::Texts(
            (song.contributors.iter())
                .filter(|contributor| PRODUCTION_ROLES.contains(&contributor.role.as_str()))
                .map(|contributor| contributor.artist.name.clone())
                .collect(),
        )
    }),
    values("mood", "Moods", MUSIC, |song, now| {
        SongField::Mood.value_of(song, now)
    }),
    values("format", "Format", AUDIO, |song, now| {
        SongField::Format.value_of(song, now)
    }),
    values("quality", "Quality", AUDIO, |song, _| {
        FieldValue::Texts(song_quality(song))
    }),
    number(
        "bitRate",
        "Bit rate",
        AUDIO,
        Measure::KilobitsPerSecond,
        |song, now| SongField::BitRate.value_of(song, now),
    ),
    number(
        "bitDepth",
        "Bit depth",
        AUDIO,
        Measure::Count,
        |song, now| SongField::BitDepth.value_of(song, now),
    ),
    number(
        "samplingRate",
        "Sample rate",
        AUDIO,
        Measure::Kilohertz,
        |song, now| SongField::SamplingRate.value_of(song, now),
    ),
    number("bpm", "BPM", AUDIO, Measure::Count, |song, now| {
        SongField::Bpm.value_of(song, now)
    }),
    number("channels", "Channels", AUDIO, Measure::Count, |song, _| {
        FieldValue::Number(song.channel_count)
    }),
    number("size", "File size", AUDIO, Measure::Megabytes, |song, _| {
        FieldValue::Number(song.size)
    }),
    number("playCount", "Plays", PLAYS, Measure::Plays, |song, now| {
        SongField::PlayCount.value_of(song, now)
    }),
    number("rating", "Rating", PLAYS, Measure::Count, |song, now| {
        SongField::Rating.value_of(song, now)
    }),
    date("added", "Added", PLAYS, |song, now| {
        SongField::Added.value_of(song, now)
    }),
    date("lastPlayed", "Last played", PLAYS, |song, now| {
        SongField::LastPlayed.value_of(song, now)
    }),
    flag(FAVOURITE, "Favourite", PLAYS, |song, now| {
        SongField::Favourite.value_of(song, now)
    }),
    text("tag", "Tag", TAGS, |song, now| {
        SongField::Tag.value_of(song, now)
    }),
    text("path", "Path", LIBRARY, |song, _| optional(&song.path)),
    text("comment", "Comment", LIBRARY, |song, _| {
        optional(&song.comment)
    }),
    text("isrc", "ISRC", LIBRARY, |song, _| {
        FieldValue::Texts(song.isrc.clone())
    }),
    text("musicBrainzId", "MusicBrainz ID", LIBRARY, |song, _| {
        optional(&song.music_brainz_id)
    }),
];

pub(super) const ARTIST_FIELDS: &[Field<ArtistFacts>] = &[
    text("title", "Name", BASICS, |artist, _| {
        FieldValue::Texts(vec![artist.name.clone()])
    }),
    values("genre", "Genres", MUSIC, |artist, _| {
        FieldValue::Texts(artist.genres.clone())
    }),
    number(
        "albumCount",
        "Albums",
        BASICS,
        Measure::Count,
        |artist, _| FieldValue::Number(artist.albums),
    ),
    number("playCount", "Plays", PLAYS, Measure::Plays, |artist, _| {
        FieldValue::Number(Some(artist.plays))
    }),
    flag(FAVOURITE, "Favourite", PLAYS, |artist, _| {
        FieldValue::Flag(artist.starred)
    }),
    text("musicBrainzId", "MusicBrainz ID", LIBRARY, |artist, _| {
        optional(&artist.music_brainz_id)
    }),
];

pub(super) const COMPOSER_FIELDS: &[Field<Composer>] = &[
    text("title", "Name", BASICS, |composer, _| {
        FieldValue::Texts(vec![composer.name.clone()])
    }),
    number(
        "compositionCount",
        "Compositions",
        BASICS,
        Measure::Count,
        |composer, _| FieldValue::Number(composer.composition_count),
    ),
    number(
        "playCount",
        "Plays",
        PLAYS,
        Measure::Plays,
        |composer, _| FieldValue::Number(composer.play_count),
    ),
    text("musicBrainzId", "MusicBrainz ID", LIBRARY, |composer, _| {
        optional(&composer.music_brainz_id)
    }),
];

pub(super) const COMPOSITION_FIELDS: &[Field<Composition>] = &[
    text("title", "Title", BASICS, |piece, _| {
        FieldValue::Texts(vec![piece.name.clone()])
    }),
    values("composer", "Composers", BASICS, |piece, _| {
        FieldValue::Texts(
            piece
                .composers
                .iter()
                .map(|composer| composer.name.clone())
                .collect(),
        )
    }),
    values("lyricist", "Lyricists", BASICS, |piece, _| {
        FieldValue::Texts(
            piece
                .lyricists
                .iter()
                .map(|writer| writer.name.clone())
                .collect(),
        )
    }),
    values("language", "Languages", BASICS, |piece, _| {
        FieldValue::Texts(piece.languages.clone())
    }),
    descending("era", "Era", MUSIC, |piece, _| {
        decade_of(composed_year(piece))
    }),
    number(
        "year",
        "Year composed",
        MUSIC,
        Measure::Count,
        |piece, _| FieldValue::Number(composed_year(piece)),
    ),
    values("period", "Period", MUSIC, |piece, _| {
        optional(&piece.period)
    }),
    values("form", "Form", MUSIC, |piece, _| optional(&piece.form)),
    values("key", "Key", MUSIC, |piece, _| optional(&piece.key)),
    number(
        "movementCount",
        "Movements",
        MUSIC,
        Measure::Count,
        |piece, _| FieldValue::Number(piece.movement_count),
    ),
    number("albumCount", "Albums", MUSIC, Measure::Count, |piece, _| {
        FieldValue::Number(piece.album_count)
    }),
    number(
        "duration",
        "Duration",
        MUSIC,
        Measure::Duration,
        |piece, _| FieldValue::Number(piece.duration),
    ),
    number("playCount", "Plays", PLAYS, Measure::Plays, |piece, _| {
        FieldValue::Number(piece.play_count)
    }),
    flag(FAVOURITE, "Favourite", PLAYS, |piece, _| {
        FieldValue::Flag(piece.starred.is_some())
    }),
    text("musicBrainzId", "MusicBrainz ID", LIBRARY, |piece, _| {
        optional(&piece.music_brainz_id)
    }),
];

pub(super) const PLAYLIST_FIELDS: &[Field<Playlist>] = &[
    text("title", "Name", BASICS, |playlist, _| {
        FieldValue::Texts(vec![playlist.name.clone()])
    }),
    text("comment", "Comment", BASICS, |playlist, _| {
        optional(&playlist.comment)
    }),
    number(
        "songCount",
        "Tracks",
        BASICS,
        Measure::Count,
        |playlist, _| FieldValue::Number(Some(playlist.song_count)),
    ),
    number(
        "duration",
        "Duration",
        BASICS,
        Measure::Duration,
        |playlist, _| FieldValue::Number(Some(playlist.duration)),
    ),
    values("owner", "Owner", BASICS, |playlist, _| {
        optional(&playlist.owner)
    }),
    flag("public", "Public", BASICS, |playlist, _| {
        FieldValue::Flag(playlist.public == Some(true))
    }),
    flag("smart", "Smart playlist", BASICS, |playlist, _| {
        FieldValue::Flag(playlist.smart == Some(true))
    }),
    date("added", "Created", PLAYS, |playlist, _| {
        FieldValue::Day(day_number(&playlist.created))
    }),
    date("changed", "Changed", PLAYS, |playlist, _| {
        FieldValue::Day(day_number(&playlist.changed))
    }),
];

const ALBUM_DEFAULTS: &[&str] = &[
    "genre",
    "decade",
    "artist",
    "releaseType",
    "added",
    "playCount",
];
const SONG_DEFAULTS: &[&str] = &[
    "genre",
    "decade",
    "artist",
    "composer",
    "production",
    "playCount",
];
const ARTIST_DEFAULTS: &[&str] = &["genre", "playCount"];
const COMPOSER_DEFAULTS: &[&str] = &["playCount"];
const COMPOSITION_DEFAULTS: &[&str] = &["composer", "era", "period", "form", "key", "playCount"];
const PLAYLIST_DEFAULTS: &[&str] = &["owner", "public", "songCount", "duration"];

/// The page's name in the saved preferences, for the pages that have a Focus.
pub(super) fn key(page: Page) -> Option<&'static str> {
    match page {
        Page::Albums => Some("albums"),
        Page::Tracks => Some("tracks"),
        Page::Artists => Some("artists"),
        Page::Composers => Some("composers"),
        Page::Compositions => Some("compositions"),
        Page::Playlists => Some("playlists"),
        _ => None,
    }
}

/// What the fields of the page are called and hold.
pub(super) fn infos_of(page: Page) -> Vec<Info> {
    match page {
        Page::Albums => infos(ALBUM_FIELDS),
        Page::Tracks => infos(SONG_FIELDS),
        Page::Artists => infos(ARTIST_FIELDS),
        Page::Composers => infos(COMPOSER_FIELDS),
        Page::Compositions => infos(COMPOSITION_FIELDS),
        Page::Playlists => infos(PLAYLIST_FIELDS),
        _ => Vec::new(),
    }
}

/// The keys of the fields the page's panel opens with a filter of.
pub(super) fn defaults_of(page: Page) -> &'static [&'static str] {
    match page {
        Page::Albums => ALBUM_DEFAULTS,
        Page::Tracks => SONG_DEFAULTS,
        Page::Artists => ARTIST_DEFAULTS,
        Page::Composers => COMPOSER_DEFAULTS,
        Page::Compositions => COMPOSITION_DEFAULTS,
        Page::Playlists => PLAYLIST_DEFAULTS,
        _ => &[],
    }
}

fn labels(names: Vec<Name>) -> Vec<String> {
    names.into_iter().map(|name| name.label.into()).collect()
}

fn optional(value: &Option<String>) -> FieldValue {
    FieldValue::Texts(value.iter().cloned().collect())
}

fn day_of(date: &Option<String>) -> FieldValue {
    FieldValue::Day(date.as_deref().and_then(day_number))
}

fn decade_of(year: Option<i64>) -> FieldValue {
    FieldValue::Texts(year.and_then(decade).into_iter().collect())
}

pub(super) fn album_genres(album: &AlbumID3) -> Vec<String> {
    genres_or_one(&album.genres, &album.genre)
}

fn genres_or_one(genres: &[ItemGenre], genre: &Option<String>) -> Vec<String> {
    if genres.is_empty() {
        return genre.iter().cloned().collect();
    }
    genres.iter().map(|genre| genre.name.clone()).collect()
}

/// The year an item of the date was released, as precisely as it is known.
fn item_date(date: &Option<ItemDate>) -> FieldValue {
    let day = date.as_ref().and_then(|date| {
        let (month, day) = (date.month.unwrap_or(1), date.day.unwrap_or(1));
        day_number(&format!("{:04}-{month:02}-{day:02}", date.year?))
    });
    FieldValue::Day(day)
}

fn album_year(album: &AlbumID3) -> Option<i64> {
    let year_of = |date: &Option<ItemDate>| date.as_ref().and_then(|date| date.year);
    (album.year)
        .or_else(|| year_of(&album.original_release_date))
        .or_else(|| year_of(&album.release_date))
}

/// The year a work was begun in, or else finished in.
fn composed_year(piece: &Composition) -> Option<i64> {
    let year_of = |date: &Option<ItemDate>| date.as_ref().and_then(|date| date.year);
    let span = piece.composed.as_ref()?;
    year_of(&span.from).or_else(|| year_of(&span.to))
}

/// Lossless for a file that loses nothing, and hi-res as well for one with
/// more than CD's depth or rate.
fn song_quality(song: &Child) -> Vec<String> {
    let suffix = song.suffix.as_deref().unwrap_or_default().to_lowercase();
    if !LOSSLESS_SUFFIXES.contains(&suffix.as_str()) {
        return Vec::new();
    }
    let mut formats = vec!["Lossless".to_owned()];
    let deep = song.bit_depth.is_some_and(|depth| depth > HI_RES_DEPTH);
    let fast = song.sampling_rate.is_some_and(|rate| rate > HI_RES_RATE);
    if deep || fast {
        formats.push("Hi-res".to_owned());
    }
    formats
}
