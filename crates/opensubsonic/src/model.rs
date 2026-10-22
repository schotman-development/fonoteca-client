use serde::Deserialize;

/// Playlist.
///
/// <https://opensubsonic.netlify.app/docs/responses/playlist/>
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Playlist {
    /// Id of the playlist
    pub id: String,
    /// Name of the playlist
    pub name: String,
    /// A comment
    pub comment: Option<String>,
    /// Owner of the playlist
    pub owner: Option<String>,
    /// Is the playlist public
    pub public: Option<bool>,
    /// number of songs
    pub song_count: i64,
    /// Playlist duration in seconds
    pub duration: i64,
    /// Creation date [ISO 8601]
    pub created: String,
    /// Last changed date [ISO 8601]
    pub changed: String,
    /// A cover Art Id
    pub cover_art: Option<String>,
    /// A list of allowed usernames
    #[serde(default)]
    pub allowed_user: Vec<String>,
    /// [OS] If true the playlist cannot be edited by the current user
    pub readonly: Option<bool>,
    /// [OS] Date the playlist contents are considered valid until [ISO 8601]. Empty or absent means no caching guarantee.
    pub valid_until: Option<String>,
    /// [OS] `playlistFolders`: ID of the containing folder in the selected user's playlist organization. Empty for a root-level playlist.
    pub playlist_folder_id: Option<String>,
    /// [OS] `playlistFolders`: Position among siblings in the selected user's playlist organization.
    pub playlist_folder_sort_order: Option<i64>,
}

/// A folder of the user's playlists, from the proposed `playlistFolders`
/// extension.
///
/// <https://github.com/opensubsonic/open-subsonic-api/pull/255>
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaylistFolder {
    pub id: String,
    /// The human-readable name of the playlist folder.
    pub name: String,
    /// ID of the parent playlist folder. Omitted for a root-level folder.
    pub parent_id: Option<String>,
    /// Position among siblings, as a client-assigned integer.
    pub sort_order: Option<i64>,
}

/// All folders of a user's playlists, as a flat list.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaylistFolders {
    #[serde(default)]
    pub playlist_folder: Vec<PlaylistFolder>,
}

/// The generic type of media [music/podcast/audiobook/video]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum GenericMediaType {
    #[serde(rename = "music")]
    Music,
    #[serde(rename = "video")]
    Video,
    #[serde(rename = "podcast")]
    Podcast,
    #[serde(rename = "audiobook")]
    Audiobook,
    /// A value this version of the spec doesn't list.
    #[serde(other)]
    Unknown,
}

/// Note: If you support `musicBrainzId` you must support this field to ensure clients knows what the ID refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum MediaType {
    #[serde(rename = "song")]
    Song,
    #[serde(rename = "album")]
    Album,
    #[serde(rename = "artist")]
    Artist,
    /// A value this version of the spec doesn't list.
    #[serde(other)]
    Unknown,
}

/// A genre returned in list of genres for an item.
///
/// <https://opensubsonic.netlify.app/docs/responses/itemgenre/>
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemGenre {
    /// Genre name
    pub name: String,
}

/// An artist from ID3 tags.
///
/// <https://opensubsonic.netlify.app/docs/responses/artistid3/>
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtistID3 {
    /// The id of the artist
    pub id: String,
    /// The artist name.
    pub name: String,
    /// A covertArt id.
    pub cover_art: Option<String>,
    /// An url to an external image source.
    pub artist_image_url: Option<String>,
    /// Artist album count.
    pub album_count: Option<i64>,
    /// Date the artist was starred. [ISO 8601]
    pub starred: Option<String>,
    /// The artist MusicBrainzID.
    pub music_brainz_id: Option<String>,
    /// The artist sort name.
    pub sort_name: Option<String>,
    /// A short human-readable comment, meant to be shown to users to distinguish between artists with the same name (e.g. 'French electronic duo').
    pub disambiguation: Option<String>,
    /// The list of all roles this artist has in the library.
    #[serde(default)]
    pub roles: Vec<String>,
}

/// A contributor artist for a song or an album
///
/// <https://opensubsonic.netlify.app/docs/responses/contributor/>
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Contributor {
    /// The contributor role.
    pub role: String,
    /// The subRole for roles that may require it. Ex: The instrument for the performer role (TMCL/performer tags). Note: For consistency between different tag formats, the TIPL sub roles should be directly exposed in the role field.
    pub sub_role: Option<String>,
    pub artist: ArtistID3,
}

/// The replay gain data of a song. Note: If the data is not present the field must be ommited in the answer. (But the replayGain field on Child must always be present)
///
/// <https://opensubsonic.netlify.app/docs/responses/replaygain/>
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplayGain {
    /// The track replay gain value. (In Db)
    pub track_gain: Option<f64>,
    /// The album replay gain value. (In Db)
    pub album_gain: Option<f64>,
    /// The track peak value. (Must be positive)
    pub track_peak: Option<f64>,
    /// The album peak value. (Must be positive)
    pub album_peak: Option<f64>,
    /// The base gain value. (In Db) (Ogg Opus Output Gain for example)
    pub base_gain: Option<f64>,
    /// An optional fallback gain that clients should apply when the corresponding gain value is missing. (Can be computed from the tracks or exposed as an user setting.)
    pub fallback_gain: Option<f64>,
}

/// Returns “explicit”, “clean” or “”. (For songs extracted from tags “ITUNESADVISORY”: 1 = explicit, 2 = clean, MP4 “rtng”: 1 or 4 = explicit, 2 = clean. See `albumID3` for albums)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum ExplicitStatus {
    #[serde(rename = "clean")]
    Clean,
    #[serde(rename = "explicit")]
    Explicit,
    #[serde(rename = "")]
    NotSet,
    /// A value this version of the spec doesn't list.
    #[serde(other)]
    Unknown,
}

/// A work associated with a song.
///
/// <https://opensubsonic.netlify.app/docs/responses/work/>
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Work {
    /// The work name.
    pub name: String,
    /// The MusicBrainz Work ID.
    pub music_brainz_id: Option<String>,
    /// Not in the spec: when the work was written, from Fonoteca's
    /// `fonotecaPerformance` extension.
    pub composed: Option<Span>,
}

/// A movement associated with a song.
///
/// <https://opensubsonic.netlify.app/docs/responses/movement/>
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Movement {
    /// The movement name.
    pub name: String,
    /// The movement number.
    pub number: Option<i64>,
    /// The total number of movements.
    pub count: Option<i64>,
}

/// A media.
///
/// <https://opensubsonic.netlify.app/docs/responses/child/>
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Child {
    /// The id of the media.
    pub id: String,
    /// The id of the parent (folder/album)
    pub parent: Option<String>,
    /// The media is a directory
    pub is_dir: bool,
    /// The media name.
    pub title: String,
    /// The album name.
    pub album: Option<String>,
    /// The artist name.
    pub artist: Option<String>,
    /// The track number.
    pub track: Option<i64>,
    /// The media year.
    pub year: Option<i64>,
    /// The media genre
    pub genre: Option<String>,
    /// The coverArt id.
    pub cover_art: Option<String>,
    /// A file size of the media.
    pub size: Option<i64>,
    /// The mimeType of the media.
    pub content_type: Option<String>,
    /// The file suffix of the media.
    pub suffix: Option<String>,
    /// The transcoded mediaType if transcoding should happen.
    pub transcoded_content_type: Option<String>,
    /// The file suffix of the transcoded media.
    pub transcoded_suffix: Option<String>,
    /// The duration of the media in seconds.
    pub duration: Option<i64>,
    /// The bitrate of the media.
    pub bit_rate: Option<i64>,
    /// The bit depth of the media.
    pub bit_depth: Option<i64>,
    /// The sampling rate of the media.
    pub sampling_rate: Option<i64>,
    /// The number of channels of the media.
    pub channel_count: Option<i64>,
    /// The full path of the media.
    pub path: Option<String>,
    /// Media is a video
    pub is_video: Option<bool>,
    /// The user rating of the media [1-5]
    pub user_rating: Option<i64>,
    /// The average rating of the media [1.0-5.0]
    pub average_rating: Option<f64>,
    /// The play count.
    pub play_count: Option<i64>,
    /// The disc number.
    pub disc_number: Option<i64>,
    /// Date the media was created. [ISO 8601]
    pub created: Option<String>,
    /// Date the media was starred. [ISO 8601]
    pub starred: Option<String>,
    /// The corresponding album id
    pub album_id: Option<String>,
    /// The corresponding artist id
    pub artist_id: Option<String>,
    pub r#type: Option<GenericMediaType>,
    pub media_type: Option<MediaType>,
    /// The bookmark position in seconds
    pub bookmark_position: Option<i64>,
    /// The video original Width
    pub original_width: Option<i64>,
    /// The video original Height
    pub original_height: Option<i64>,
    /// Date the album was last played. [ISO 8601]
    pub played: Option<String>,
    /// The BPM of the song.
    pub bpm: Option<i64>,
    /// The comment tag of the song.
    pub comment: Option<String>,
    /// The song sort name.
    pub sort_name: Option<String>,
    /// The track MusicBrainzID.
    pub music_brainz_id: Option<String>,
    /// The track ISRC(s).
    #[serde(default)]
    pub isrc: Vec<String>,
    /// The list of all genres of the song.
    #[serde(default)]
    pub genres: Vec<ItemGenre>,
    /// The list of all song artists of the song. (Note: Only the required `ArtistID3` fields should be returned by default)
    #[serde(default)]
    pub artists: Vec<ArtistID3>,
    /// The single value display artist.
    pub display_artist: Option<String>,
    /// The list of all album artists of the song. (Note: Only the required `ArtistID3` fields should be returned by default)
    #[serde(default)]
    pub album_artists: Vec<ArtistID3>,
    /// The single value display album artist.
    pub display_album_artist: Option<String>,
    /// The list of all contributor artists of the song.
    #[serde(default)]
    pub contributors: Vec<Contributor>,
    /// The single value display composer.
    pub display_composer: Option<String>,
    /// The list of all moods of the song.
    #[serde(default)]
    pub moods: Vec<String>,
    /// The replay gain data of the song.
    pub replay_gain: Option<ReplayGain>,
    pub explicit_status: Option<ExplicitStatus>,
    /// The list of works associated with the song.
    #[serde(default)]
    pub works: Vec<Work>,
    /// The list of movements associated with the song.
    #[serde(default)]
    pub movements: Vec<Movement>,
    /// The list of groupings associated with the song.
    #[serde(default)]
    pub groupings: Vec<String>,
    /// Not in the spec: when and where the song was recorded, from Fonoteca's
    /// `fonotecaPerformance` extension.
    pub recorded: Option<Recorded>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaylistWithSongs {
    #[serde(flatten)]
    pub playlist: Playlist,
    /// The list of songs
    #[serde(default)]
    pub entry: Vec<Child>,
}

/// A sonic match entry, containing a Child entry object and a normalized similarity score.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SonicMatch {
    pub entry: Child,
    /// The normalized similarity score (1.0 = same exact song, 0.0 = most different). For getSonicSimilarTracks, relative to the query song. For findSonicPath, relative to the starting song. Returns -1 when similarity is not supported by the server.
    pub similarity: f64,
}

/// A record label for an album.
///
/// <https://opensubsonic.netlify.app/docs/responses/recordlabel/>
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordLabel {
    pub name: String,
}

/// A date for a media item that may be just a year, or year-month, or full date.
///
/// <https://opensubsonic.netlify.app/docs/responses/itemdate/>
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemDate {
    /// The year
    pub year: Option<i64>,
    /// The month (1-12)
    pub month: Option<i64>,
    /// The day (1-31)
    pub day: Option<i64>,
}

/// Not in the spec: a stretch of time from Fonoteca's `fonotecaPerformance`
/// extension, each end as precise as it is known.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Span {
    pub from: Option<ItemDate>,
    pub to: Option<ItemDate>,
}

/// Not in the spec: when and where a song was recorded, from Fonoteca's
/// `fonotecaPerformance` extension.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Recorded {
    pub from: Option<ItemDate>,
    pub to: Option<ItemDate>,
    /// The studios, halls or areas it was recorded at.
    #[serde(default)]
    pub places: Vec<String>,
}

/// Not in the spec: the composers of classical pieces, indexed by letter, from
/// Fonoteca's `fonotecaCompositions` extension.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Composers {
    #[serde(default)]
    pub index: Vec<ComposerIndex>,
}

/// Not in the spec: the composers under one letter, from Fonoteca's
/// `fonotecaCompositions` extension.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComposerIndex {
    pub name: String,
    #[serde(default)]
    pub composer: Vec<Composer>,
}

/// Not in the spec: a composer, from Fonoteca's `fonotecaCompositions`
/// extension. A composition names its composers by id and name only.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Composer {
    /// The composer's artist id.
    pub id: String,
    pub name: String,
    pub sort_name: Option<String>,
    pub music_brainz_id: Option<String>,
    pub cover_art: Option<String>,
    pub composition_count: Option<i64>,
}

/// Not in the spec: classical pieces, from Fonoteca's `fonotecaCompositions`
/// extension.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Compositions {
    #[serde(default)]
    pub composition: Vec<Composition>,
}

/// Not in the spec: a classical piece, the work rather than a performance of
/// it, from Fonoteca's `fonotecaCompositions` extension. Its movements and
/// recordings come only when it is asked for by id.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Composition {
    pub id: String,
    pub name: String,
    pub music_brainz_id: Option<String>,
    #[serde(default)]
    pub composers: Vec<Composer>,
    /// When it was written.
    pub composed: Option<Span>,
    /// The albums holding a recording of it or its movements.
    pub album_count: Option<i64>,
    /// The cover of one of those albums.
    pub cover_art: Option<String>,
    /// When it was loved, as songs are, with `star`/`unstar` by its id.
    pub starred: Option<String>,
    /// Its MusicBrainz type ("Concerto", "Ballet").
    pub form: Option<String>,
    /// Its MusicBrainz key ("B-flat minor").
    pub key: Option<String>,
    /// Medieval, Renaissance, Baroque, Classical, Romantic, Modern or
    /// Contemporary, by the year it was begun, else its composer's life.
    pub period: Option<String>,
    /// Its parts on MusicBrainz, else the movements held.
    pub movement_count: Option<i64>,
    /// The longest held performance, in seconds.
    pub duration: Option<i64>,
    /// Who wrote its words.
    #[serde(default)]
    pub lyricists: Vec<Composer>,
    /// The languages of its words, as ISO 639-3 codes.
    #[serde(default)]
    pub languages: Vec<String>,
    #[serde(default)]
    pub movement: Vec<Movement>,
    /// Every recording of it and its movements, across all albums.
    #[serde(default)]
    pub song: Vec<Child>,
}

/// A disc title for an album with an optional cover art
///
/// <https://opensubsonic.netlify.app/docs/responses/disctitle/>
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscTitle {
    /// The disc number.
    pub disc: i64,
    /// The name of the disc.
    pub title: String,
    /// The cover art ID of the disc.
    pub cover_art: Option<String>,
}

/// Album with songs.
///
/// <https://opensubsonic.netlify.app/docs/responses/albumid3/>
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AlbumID3 {
    /// The id of the album
    pub id: String,
    /// The album name.
    pub name: String,
    /// The album version name (Remastered, Anniversary Box Set, …).
    pub version: Option<String>,
    /// Artist name.
    pub artist: Option<String>,
    /// The id of the artist
    pub artist_id: Option<String>,
    /// A covertArt id.
    pub cover_art: Option<String>,
    /// Number of songs
    pub song_count: i64,
    /// Total duration of the album in seconds
    pub duration: i64,
    /// Number of play of the album
    pub play_count: Option<i64>,
    /// Date the album was added. [ISO 8601]
    pub created: String,
    /// Date the album was starred. [ISO 8601]
    pub starred: Option<String>,
    /// The album year
    pub year: Option<i64>,
    /// The album genre
    pub genre: Option<String>,
    /// Date the album was last played. [ISO 8601]
    pub played: Option<String>,
    /// The user rating of the album. [1-5]
    pub user_rating: Option<i64>,
    /// The labels producing the album.
    #[serde(default)]
    pub record_labels: Vec<RecordLabel>,
    /// The album MusicBrainzID.
    pub music_brainz_id: Option<String>,
    /// The list of all genres of the album.
    #[serde(default)]
    pub genres: Vec<ItemGenre>,
    /// The list of all album artists of the album.
    #[serde(default)]
    pub artists: Vec<ArtistID3>,
    /// The single value display artist.
    pub display_artist: Option<String>,
    /// The types of this album release. (Album, Compilation, EP, Remix, …).
    #[serde(default)]
    pub release_types: Vec<String>,
    /// The list of all moods of the album.
    #[serde(default)]
    pub moods: Vec<String>,
    /// The album sort name.
    pub sort_name: Option<String>,
    /// Date the album was originally released.
    pub original_release_date: Option<ItemDate>,
    /// Date the specific edition of the album was released. Note: for files using ID3 tags, releaseDate should generally be read from the TDRL tag. Servers that use a different source for this field should document the behavior.
    pub release_date: Option<ItemDate>,
    /// True if the album is a compilation.
    pub is_compilation: Option<bool>,
    pub explicit_status: Option<ExplicitStatus>,
    /// The list of all disc titles of the album.
    #[serde(default)]
    pub disc_titles: Vec<DiscTitle>,
}

/// An album with its songs.
///
/// <https://opensubsonic.netlify.app/docs/responses/albumid3withsongs/>
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AlbumID3WithSongs {
    #[serde(flatten)]
    pub album: AlbumID3,
    /// The list of songs
    #[serde(default)]
    pub song: Vec<Child>,
}

/// Album info.
///
/// <https://opensubsonic.netlify.app/docs/responses/albuminfo/>
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AlbumInfo {
    /// Album notes
    pub notes: Option<String>,
    /// Album musicBrainzId
    pub music_brainz_id: Option<String>,
    /// Album lastFmUrl
    pub last_fm_url: Option<String>,
    /// Album smallImageUrl
    pub small_image_url: Option<String>,
    /// Album mediumImageUrl
    pub medium_image_url: Option<String>,
    /// Album largeImageUrl
    pub large_image_url: Option<String>,
}

/// Album list.
///
/// <https://opensubsonic.netlify.app/docs/responses/albumlist/>
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AlbumList {
    /// Artist albums
    #[serde(default)]
    pub album: Vec<Child>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AlbumList2 {
    /// Artist albums
    #[serde(default)]
    pub album: Vec<AlbumID3>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtistWithAlbumsID3 {
    #[serde(flatten)]
    pub artist: ArtistID3,
    /// Artist albums
    #[serde(default)]
    pub album: Vec<AlbumID3>,
}

/// Artist details.
///
/// <https://opensubsonic.netlify.app/docs/responses/artist/>
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Artist {
    /// Artist id
    pub id: String,
    /// Artist name
    pub name: String,
    /// Artist image url
    pub artist_image_url: Option<String>,
    /// Artist starred date [ISO 8601]
    pub starred: Option<String>,
    /// Artist rating [1-5]
    pub user_rating: Option<i64>,
    /// Artist average rating [1.0-5.0]
    pub average_rating: Option<f64>,
}

/// Artist info.
///
/// <https://opensubsonic.netlify.app/docs/responses/artistinfo/>
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtistInfo {
    /// Artist biography
    pub biography: Option<String>,
    /// Artist musicBrainzId
    pub music_brainz_id: Option<String>,
    /// Artist lastFmUrl
    pub last_fm_url: Option<String>,
    /// Artist smallImageUrl
    pub small_image_url: Option<String>,
    /// Artist mediumImageUrl
    pub medium_image_url: Option<String>,
    /// Artist largeImageUrl
    pub large_image_url: Option<String>,
    /// Similar artists
    #[serde(default)]
    pub similar_artist: Vec<Artist>,
}

/// Artist info.
///
/// <https://opensubsonic.netlify.app/docs/responses/artistinfo2/>
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtistInfo2 {
    /// Artist biography
    pub biography: Option<String>,
    /// Artist musicBrainzId
    pub music_brainz_id: Option<String>,
    /// Artist lastFmUrl
    pub last_fm_url: Option<String>,
    /// Artist smallImageUrl
    pub small_image_url: Option<String>,
    /// Artist mediumImageUrl
    pub medium_image_url: Option<String>,
    /// Artist largeImageUrl
    pub large_image_url: Option<String>,
    /// Similar artists
    #[serde(default)]
    pub similar_artist: Vec<ArtistID3>,
}

/// An indexed artist list by ID3 tags.
///
/// <https://opensubsonic.netlify.app/docs/responses/indexid3/>
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexID3 {
    /// Index name
    pub name: String,
    /// Artist list
    #[serde(default)]
    pub artist: Vec<ArtistID3>,
}

/// A list of indexed Artists.
///
/// <https://opensubsonic.netlify.app/docs/responses/artistsid3/>
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtistsID3 {
    /// List of ignored articles space separated
    pub ignored_articles: String,
    /// Index list
    #[serde(default)]
    pub index: Vec<IndexID3>,
}

/// A genre.
///
/// <https://opensubsonic.netlify.app/docs/responses/genre/>
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Genre {
    /// Genre name
    pub value: String,
    /// Genre song count
    pub song_count: i64,
    /// Genre album count
    pub album_count: i64,
}

/// Genres list.
///
/// <https://opensubsonic.netlify.app/docs/responses/genres/>
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Genres {
    /// List of genre
    #[serde(default)]
    pub genre: Vec<Genre>,
}

/// An indexed artist list.
///
/// <https://opensubsonic.netlify.app/docs/responses/index_/>
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Index {
    /// Index name
    pub name: String,
    /// Artist list
    #[serde(default)]
    pub artist: Vec<Artist>,
}

/// Artist list.
///
/// <https://opensubsonic.netlify.app/docs/responses/indexes/>
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Indexes {
    /// The ignored articles
    pub ignored_articles: String,
    /// Last time the index was modified in milliseconds after January 1, 1970 UTC
    pub last_modified: i64,
    /// Shortcut
    #[serde(default)]
    pub shortcut: Vec<Artist>,
    /// Array of children
    #[serde(default)]
    pub child: Vec<Child>,
    /// Indexed artists
    #[serde(default)]
    pub index: Vec<Index>,
}

/// getLicense result.
///
/// <https://opensubsonic.netlify.app/docs/responses/license/>
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct License {
    /// The status of the license
    pub valid: bool,
    /// User email
    pub email: Option<String>,
    /// End of license date. [ISO 8601]
    pub license_expires: Option<String>,
    /// End of trial date. [ISO 8601]
    pub trial_expires: Option<String>,
}

/// Lyrics.
///
/// <https://opensubsonic.netlify.app/docs/responses/lyrics/>
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Lyrics {
    /// The lyrics
    pub value: String,
    /// The artist name
    pub artist: Option<String>,
    /// The song title
    pub title: Option<String>,
}

/// One line of a song lyric
///
/// <https://opensubsonic.netlify.app/docs/responses/line/>
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Line {
    /// The actual text of this line
    pub value: String,
    /// The start time of the lyrics, relative to the start time of the track, in milliseconds. If this is not part of synced lyrics, start __must__ be omitted
    pub start: Option<f64>,
}

/// The primary lyric-layer classification for this structuredLyrics entry. One of: main (primary vocals for this entry, default if omitted), translation (a translation of another lyric layer into another language), pronunciation (a phonetic/romanized rendering, e.g. romaji for Japanese, pinyin for Chinese). Tracks are independent across kind values; clients should not assume 1:1 line or cue alignment between entries. Only returned when enhanced=true
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum LyricsKind {
    #[serde(rename = "main")]
    Main,
    #[serde(rename = "translation")]
    Translation,
    #[serde(rename = "pronunciation")]
    Pronunciation,
    /// A value this version of the spec doesn't list.
    #[serde(other)]
    Unknown,
}

/// Semantic vocal-layer classification for cueLines that reference this agent. One of: main (lead/default vocal layer), voice (additional explicit individual voice part), bg (background vocals), group (group/chorus vocals). When a structuredLyrics entry uses agents for cue-attributed lyrics, it must define exactly one main agent
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum AgentRole {
    #[serde(rename = "main")]
    Main,
    #[serde(rename = "voice")]
    Voice,
    #[serde(rename = "bg")]
    Bg,
    #[serde(rename = "group")]
    Group,
    /// A value this version of the spec doesn't list.
    #[serde(other)]
    Unknown,
}

/// Reusable metadata for a vocal agent within a structuredLyrics entry
///
/// <https://opensubsonic.netlify.app/docs/responses/agent/>
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Agent {
    /// Opaque identifier for this agent. The value is only meaningful within the parent structuredLyrics entry and must be unique within that entry
    pub id: String,
    /// Semantic vocal-layer classification for cueLines that reference this agent. One of: main (lead/default vocal layer), voice (additional explicit individual voice part), bg (background vocals), group (group/chorus vocals). When a structuredLyrics entry uses agents for cue-attributed lyrics, it must define exactly one main agent
    pub role: AgentRole,
    /// Optional human-readable label for this agent, such as a singer or character name from the source metadata
    pub name: Option<String>,
}

/// A word or syllable cue within a cueLine
///
/// <https://opensubsonic.netlify.app/docs/responses/cue/>
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Cue {
    /// Start time in milliseconds
    pub start: i64,
    /// End time in milliseconds. Within a cueLine, end must be either present on all cues or none. When the source provides partial end times, servers must fill missing values (e.g., using the next cue's start, or the cueLine's end for the final cue). When no cues have end times (e.g., Enhanced LRC with start-only timing), end is omitted from all cues. This is a documented contract rule; this schema does not enforce the all-or-none shape structurally
    pub end: Option<i64>,
    /// Required zero-based inclusive UTF-8 byte offset into the parent cueLine.value where this cue begins. The parent cueLine must include value, and the offsets must be calculated against the final cueLine.value with no normalization step
    pub byte_start: i64,
    /// Required zero-based inclusive UTF-8 byte offset into the parent cueLine.value where this cue ends. byteStart must be less than or equal to byteEnd, the parent cueLine must include value, and the offsets must be calculated against the final cueLine.value with no normalization step. This schema does not enforce the cross-field ordering constraint structurally
    pub byte_end: i64,
    /// The text of this word or syllable
    pub value: String,
}

/// Word/syllable-level timing data for a lyrics line or agent layer
///
/// <https://opensubsonic.netlify.app/docs/responses/cueline/>
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CueLine {
    /// Zero-based index into the parent line array this cueLine corresponds to
    pub index: i64,
    /// Start time in milliseconds (may differ from the parent line if cues are more precise)
    pub start: Option<i64>,
    /// End time in milliseconds
    pub end: Option<i64>,
    /// Required full text for this cueLine. When agent attribution splits one parent line into multiple cueLines, this is the text for that cueLine's agent/layer, not necessarily the parent line's combined text. Nested cue byteStart and byteEnd offsets are always calculated against this exact final UTF-8 string
    pub value: String,
    /// Opaque identifier referencing an agent in the same structuredLyrics entry. If the parent structuredLyrics entry includes agents, every cueLine in that entry must include agentId, and the value must match exactly one agents[].id in that entry. If the parent entry does not include agents, cueLines must not include agentId. When multiple cueLines share the same index, the cueLine whose referenced agent has role main must come first
    pub agent_id: Option<String>,
    /// Ordered list of word/syllable cues. Every cue includes required byteStart and byteEnd offsets into value
    #[serde(default)]
    pub cue: Vec<Cue>,
}

/// Structured lyrics
///
/// <https://opensubsonic.netlify.app/docs/responses/structuredlyrics/>
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StructuredLyrics {
    /// The lyrics language (ideally ISO 639). If the language is unknown (e.g. lrc file), the server must return `und` (ISO standard) or `xxx` (common value for taggers). Ideally, the server will return lang as an ISO 639 (2/3) code. However, tagged files and external lyrics can come with any value as a potential language code, so clients should take care when displaying lang.
    ///
    /// Furthermore, there is special behavior for the value xxx. While not an ISO code, it is commonly used by taggers and other parsing software. Clients should treat xxx as not having a specified language (equivalent to the und code).
    pub lang: String,
    /// True if the lyrics are synced, false otherwise
    pub synced: bool,
    /// The actual lyrics. Ordered by start time (synced) or appearance order (unsynced)
    #[serde(default)]
    pub line: Vec<Line>,
    /// The artist name to display. This could be the localized name, or any other value
    pub display_artist: Option<String>,
    /// The title to display. This could be the song title (localized), or any other value
    pub display_title: Option<String>,
    /// The offset to apply to all lyrics, in milliseconds. Positive means lyrics appear sooner, negative means later. If not included, the offset must be assumed to be 0
    pub offset: Option<f64>,
    /// The primary lyric-layer classification for this structuredLyrics entry. One of: main (primary vocals for this entry, default if omitted), translation (a translation of another lyric layer into another language), pronunciation (a phonetic/romanized rendering, e.g. romaji for Japanese, pinyin for Chinese). Tracks are independent across kind values; clients should not assume 1:1 line or cue alignment between entries. Only returned when enhanced=true
    pub kind: Option<LyricsKind>,
    /// Reusable per-track attribution metadata for cueLine entries. When present, agents must contain at least one entry, and each agents[].id must be unique within this structuredLyrics entry. Agents are optional for simple unattributed single-layer lyrics. When a structuredLyrics entry represents multiple vocal agents or layers, it must include agents; a single-agent attributed or default entry may also include agents, and if it does, exactly one agent must use role main. Agents should not be emitted without cueLine data
    #[serde(default)]
    pub agents: Vec<Agent>,
    /// Word/syllable-level timing data. Each cueLine corresponds to a line in this structuredLyrics entry by its index field. Every cueLine includes value, and every cue includes byteStart and byteEnd offsets into that exact string. When agents split one parent line into multiple cueLines, each cueLine.value is the renderable text for that agent/layer, not necessarily the parent line's combined text. If agents is present, every cueLine in the entry must include agentId, and each agentId must match exactly one agents[].id in that entry. If agents is absent, cueLines must not include agentId. Only returned when enhanced=true and synced is true
    #[serde(default)]
    pub cue_line: Vec<CueLine>,
}

/// List of structured lyrics
///
/// <https://opensubsonic.netlify.app/docs/responses/lyricslist/>
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LyricsList {
    /// Structured lyrics. There can be multiple lyrics of the same type with the same language
    #[serde(default)]
    pub structured_lyrics: Vec<StructuredLyrics>,
}

/// Directory.
///
/// <https://opensubsonic.netlify.app/docs/responses/directory/>
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Directory {
    /// The id
    pub id: String,
    /// Parent item
    pub parent: Option<String>,
    /// The directory name
    pub name: String,
    /// Starred date [ISO 8601]
    pub starred: Option<String>,
    /// The user rating [1-5]
    pub user_rating: Option<i64>,
    /// The average rating [1-5]
    pub average_rating: Option<f64>,
    /// The play count
    pub play_count: Option<i64>,
    /// The directory content
    #[serde(default)]
    pub child: Vec<Child>,
}

/// MusicFolder.
///
/// <https://opensubsonic.netlify.app/docs/responses/musicfolder/>
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MusicFolder {
    /// The id
    pub id: i64,
    /// The folder name
    pub name: Option<String>,
}

/// MusicFolders.
///
/// <https://opensubsonic.netlify.app/docs/responses/musicfolders/>
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MusicFolders {
    /// The folders
    #[serde(default)]
    pub music_folder: Vec<MusicFolder>,
}

/// Playback state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum PlaybackState {
    #[serde(rename = "starting")]
    Starting,
    #[serde(rename = "playing")]
    Playing,
    #[serde(rename = "paused")]
    Paused,
    #[serde(rename = "stopped")]
    Stopped,
    /// A value this version of the spec doesn't list.
    #[serde(other)]
    Unknown,
}

/// NowPlayingEntry.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NowPlayingEntry {
    #[serde(flatten)]
    pub media: Child,
    /// The username
    pub username: String,
    /// Last update
    pub minutes_ago: i64,
    /// Player Id
    pub player_id: i64,
    /// Player name
    pub player_name: Option<String>,
    /// Playback state.
    pub state: Option<PlaybackState>,
    /// Current playback position in milliseconds (Calculated by the server from the last playbackReport call received)
    pub position_ms: Option<i64>,
    /// Playback speed multiplier
    pub playback_rate: Option<f64>,
}

/// nowPlaying.
///
/// <https://opensubsonic.netlify.app/docs/responses/nowplaying/>
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NowPlaying {
    /// The now playing entries
    #[serde(default)]
    pub entry: Vec<NowPlayingEntry>,
}

/// A supported OpenSubsonic API extension.
///
/// <https://opensubsonic.netlify.app/docs/responses/opensubsonicextension/>
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenSubsonicExtension {
    /// The name of the extension.
    pub name: String,
    /// The list of supported versions of the this extension.
    #[serde(default)]
    pub versions: Vec<i64>,
}

/// Playlists.
///
/// <https://opensubsonic.netlify.app/docs/responses/playlists/>
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Playlists {
    /// The playlists
    #[serde(default)]
    pub playlist: Vec<Playlist>,
}

/// Songs list.
///
/// <https://opensubsonic.netlify.app/docs/responses/songs/>
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Songs {
    /// List of songs
    #[serde(default)]
    pub song: Vec<Child>,
}

/// SimilarSongs list.
///
/// <https://opensubsonic.netlify.app/docs/responses/similarsongs/>
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SimilarSongs {
    /// List of songs
    #[serde(default)]
    pub song: Vec<Child>,
}

/// SimilarSongs2 list.
///
/// <https://opensubsonic.netlify.app/docs/responses/similarsongs2/>
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SimilarSongs2 {
    /// List of songs
    #[serde(default)]
    pub song: Vec<Child>,
}

/// starred.
///
/// <https://opensubsonic.netlify.app/docs/responses/starred/>
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Starred {
    /// Starred artists
    #[serde(default)]
    pub artist: Vec<Artist>,
    /// Starred albums
    #[serde(default)]
    pub album: Vec<Child>,
    /// Starred songs
    #[serde(default)]
    pub song: Vec<Child>,
}

/// Starred2.
///
/// <https://opensubsonic.netlify.app/docs/responses/starred2/>
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Starred2 {
    /// Starred artists
    #[serde(default)]
    pub artist: Vec<ArtistID3>,
    /// Starred albums
    #[serde(default)]
    pub album: Vec<AlbumID3>,
    /// Starred songs
    #[serde(default)]
    pub song: Vec<Child>,
}

/// TopSongs list.
///
/// <https://opensubsonic.netlify.app/docs/responses/topsongs/>
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TopSongs {
    /// List of songs
    #[serde(default)]
    pub song: Vec<Child>,
}

/// searchResult2
///
/// <https://opensubsonic.netlify.app/docs/responses/searchresult2/>
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResult2 {
    /// Starred artists
    #[serde(default)]
    pub artist: Vec<Artist>,
    /// Starred albums
    #[serde(default)]
    pub album: Vec<Child>,
    /// Starred songs
    #[serde(default)]
    pub song: Vec<Child>,
}

/// searchResult3
///
/// <https://opensubsonic.netlify.app/docs/responses/searchresult3/>
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResult3 {
    /// Matching artists
    #[serde(default)]
    pub artist: Vec<ArtistID3>,
    /// Matching albums
    #[serde(default)]
    pub album: Vec<AlbumID3>,
    /// Matching songs
    #[serde(default)]
    pub song: Vec<Child>,
}

/// Scan status information.
///
/// <https://opensubsonic.netlify.app/docs/responses/scanstatus/>
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanStatus {
    /// The status of the scan
    pub scanning: bool,
    /// Scanned item count
    pub count: Option<i64>,
}

/// Information about an API key
///
/// <https://opensubsonic.netlify.app/docs/responses/tokeninfo/>
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenInfo {
    /// Username associated with token
    pub username: String,
}

/// An internet radio station.
///
/// <https://opensubsonic.netlify.app/docs/responses/internetradiostation/>
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InternetRadioStation {
    /// The Id
    pub id: String,
    /// The name
    pub name: String,
    /// The radio url
    pub stream_url: String,
    /// The home page URL for the station.
    pub home_page_url: Option<String>,
    /// [OS] The cover art id.
    pub cover_art: Option<String>,
}

/// Internet radio stations.
///
/// <https://opensubsonic.netlify.app/docs/responses/internetradiostations/>
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InternetRadioStations {
    /// A list of internetRadioStation
    #[serde(default)]
    pub internet_radio_station: Vec<InternetRadioStation>,
}

/// A user, with what it may do.
///
/// <https://opensubsonic.netlify.app/docs/responses/user/>
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct User {
    /// Username
    pub username: String,
    /// In the spec's examples, not its table.
    pub email: Option<String>,
    /// Whether scrobling is enabled for the user
    pub scrobbling_enabled: bool,
    pub max_bit_rate: Option<i64>,
    /// Whether the user is an admin
    pub admin_role: bool,
    /// Whether the user is can edit settings
    pub settings_role: bool,
    /// Whether the user can download
    pub download_role: bool,
    /// Whether the user can upload
    pub upload_role: bool,
    /// Whether the user can create playlists
    pub playlist_role: bool,
    /// Whether the user can get cover art
    pub cover_art_role: bool,
    /// Whether the user can create comments
    pub comment_role: bool,
    /// Whether the user can create/refresh podcasts
    pub podcast_role: bool,
    /// Whether the user can stream
    pub stream_role: bool,
    /// Whether the user can control the jukebox
    pub jukebox_role: bool,
    /// Whether the user can share
    pub share_role: bool,
    /// Whether the user can convert videos
    pub video_conversion_role: bool,
    /// Last time the avatar was changed [ISO 8601]
    pub avatar_last_changed: Option<String>,
    /// Folder ID(s)
    #[serde(default)]
    pub folder: Vec<i64>,
}

/// Users.
///
/// <https://opensubsonic.netlify.app/docs/responses/users/>
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Users {
    /// Array of users
    #[serde(default)]
    pub user: Vec<User>,
}

/// The error element of a failed response.
///
/// <https://opensubsonic.netlify.app/docs/responses/error/>
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiError {
    /// The error code: 0 generic, 10 missing parameter, 20 client too old,
    /// 30 server too old, 40 wrong username or password, 41 token
    /// authentication not supported, 42 authentication method not supported,
    /// 43 conflicting authentication methods, 44 invalid API key, 50 not
    /// authorized, 60 trial over, 70 not found.
    pub code: i64,
    /// A human readable error message.
    pub message: Option<String>,
    /// [OS] A URL that explains the error, e.g. where to get an API key.
    pub help_url: Option<String>,
}
