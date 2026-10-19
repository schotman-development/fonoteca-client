//! One method per OpenSubsonic call, grouped as on
//! <https://opensubsonic.netlify.app/docs/opensubsonic-api/>.

use std::sync::atomic::Ordering::Relaxed;

use serde_json::Value;

use crate::model::*;
use crate::{Client, Error, Params, Result, ServerInfo};

/// Which albums `getAlbumList` and `getAlbumList2` return.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AlbumListType {
    Random,
    Newest,
    Highest,
    Frequent,
    Recent,
    AlphabeticalByName,
    AlphabeticalByArtist,
    Starred,
    /// Albums from `from` to `to`, inclusive. With `from` later than `to` the
    /// list runs newest first.
    ByYear {
        from: i32,
        to: i32,
    },
    ByGenre(String),
}

impl AlbumListType {
    fn params(&self, params: Params) -> Params {
        let name = match self {
            AlbumListType::Random => "random",
            AlbumListType::Newest => "newest",
            AlbumListType::Highest => "highest",
            AlbumListType::Frequent => "frequent",
            AlbumListType::Recent => "recent",
            AlbumListType::AlphabeticalByName => "alphabeticalByName",
            AlbumListType::AlphabeticalByArtist => "alphabeticalByArtist",
            AlbumListType::Starred => "starred",
            AlbumListType::ByYear { from, to } => {
                return params
                    .req("type", "byYear")
                    .req("fromYear", from)
                    .req("toYear", to);
            }
            AlbumListType::ByGenre(genre) => {
                return params.req("type", "byGenre").req("genre", genre);
            }
        };
        params.req("type", name)
    }
}

/// Paging and folder filter for album lists and `getSongsByGenre`. Left
/// `None`, the server's defaults apply.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ListOptions<'a> {
    /// How many to return: 10 by default, at most 500.
    pub size: Option<u32>,
    pub offset: Option<u32>,
    /// Only items in this music folder, see [`Client::get_music_folders`].
    pub music_folder_id: Option<&'a str>,
}

/// Filters for [`Client::get_random_songs`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RandomSongsOptions<'a> {
    /// How many to return: 10 by default, at most 500.
    pub size: Option<u32>,
    pub genre: Option<&'a str>,
    /// Songs from this year or later.
    pub from_year: Option<i32>,
    /// Songs from this year or earlier.
    pub to_year: Option<i32>,
    pub music_folder_id: Option<&'a str>,
}

/// Paging for `search2` and `search3`. Each count defaults to 20.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchOptions<'a> {
    pub artist_count: Option<u32>,
    pub artist_offset: Option<u32>,
    pub album_count: Option<u32>,
    pub album_offset: Option<u32>,
    pub song_count: Option<u32>,
    pub song_offset: Option<u32>,
    pub music_folder_id: Option<&'a str>,
}

impl SearchOptions<'_> {
    fn params(&self, params: Params) -> Params {
        params
            .opt("artistCount", self.artist_count)
            .opt("artistOffset", self.artist_offset)
            .opt("albumCount", self.album_count)
            .opt("albumOffset", self.album_offset)
            .opt("songCount", self.song_count)
            .opt("songOffset", self.song_offset)
            .opt("musicFolderId", self.music_folder_id)
    }
}

/// Changes for [`Client::update_playlist`]. Whatever is left `None` or empty
/// stays as it is.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlaylistUpdate<'a> {
    pub name: Option<&'a str>,
    pub comment: Option<&'a str>,
    /// Whether all users can see the playlist.
    pub public: Option<bool>,
    pub song_ids_to_add: &'a [&'a str],
    /// Positions in the playlist, starting at 0.
    pub song_indexes_to_remove: &'a [u32],
}

impl Client {
    // System

    /// Checks that the server answers and the credentials work, and returns
    /// what the server says about itself.
    ///
    /// <https://opensubsonic.netlify.app/docs/endpoints/ping/>
    pub async fn ping(&self) -> Result<ServerInfo> {
        let response = self.json("ping", &Params::new()).await?;
        Ok(serde_json::from_value(Value::Object(response))?)
    }

    /// Details about the server's software license.
    ///
    /// <https://opensubsonic.netlify.app/docs/endpoints/getlicense/>
    pub async fn get_license(&self) -> Result<License> {
        self.call("getLicense", Params::new(), "license").await
    }

    /// The OpenSubsonic extensions the server supports. Sent without
    /// credentials, which servers must accept. Call it first: when `formPost`
    /// is among them, every later call from this client is sent as a form
    /// POST, which keeps credentials out of URLs.
    ///
    /// <https://opensubsonic.netlify.app/docs/endpoints/getopensubsonicextensions/>
    pub async fn get_open_subsonic_extensions(&self) -> Result<Vec<OpenSubsonicExtension>> {
        let extensions: Vec<OpenSubsonicExtension> = self
            .call(
                "getOpenSubsonicExtensions",
                Params::new(),
                "openSubsonicExtensions",
            )
            .await?;
        self.form_post
            .store(extensions.iter().any(|e| e.name == "formPost"), Relaxed);
        if let Ok(mut names) = self.extensions.lock() {
            *names = extensions.iter().map(|e| e.name.clone()).collect();
        }
        Ok(extensions)
    }

    /// The user an API key belongs to. Needs `apiKeyAuthentication`.
    ///
    /// <https://opensubsonic.netlify.app/docs/endpoints/tokeninfo/>
    pub async fn token_info(&self) -> Result<TokenInfo> {
        self.call("tokenInfo", Params::new(), "tokenInfo").await
    }

    // Browsing

    /// The top-level music folders.
    ///
    /// <https://opensubsonic.netlify.app/docs/endpoints/getmusicfolders/>
    pub async fn get_music_folders(&self) -> Result<MusicFolders> {
        self.call("getMusicFolders", Params::new(), "musicFolders")
            .await
    }

    /// All artists by file structure, indexed by letter. With
    /// `if_modified_since` (milliseconds since 1970), `None` means nothing
    /// changed since then.
    ///
    /// <https://opensubsonic.netlify.app/docs/endpoints/getindexes/>
    pub async fn get_indexes(
        &self,
        music_folder_id: Option<&str>,
        if_modified_since: Option<i64>,
    ) -> Result<Option<Indexes>> {
        let params = Params::new()
            .opt("musicFolderId", music_folder_id)
            .opt("ifModifiedSince", if_modified_since);
        self.call("getIndexes", params, "indexes").await
    }

    /// The files and folders in a music directory, by file structure.
    ///
    /// <https://opensubsonic.netlify.app/docs/endpoints/getmusicdirectory/>
    pub async fn get_music_directory(&self, id: &str) -> Result<Directory> {
        self.call(
            "getMusicDirectory",
            Params::new().req("id", id),
            "directory",
        )
        .await
    }

    /// All genres, with song and album counts.
    ///
    /// <https://opensubsonic.netlify.app/docs/endpoints/getgenres/>
    pub async fn get_genres(&self) -> Result<Genres> {
        self.call("getGenres", Params::new(), "genres").await
    }

    /// All artists by ID3 tags, indexed by letter.
    ///
    /// <https://opensubsonic.netlify.app/docs/endpoints/getartists/>
    pub async fn get_artists(&self, music_folder_id: Option<&str>) -> Result<ArtistsID3> {
        self.call(
            "getArtists",
            Params::new().opt("musicFolderId", music_folder_id),
            "artists",
        )
        .await
    }

    /// An artist with their albums, by ID3 tags.
    ///
    /// <https://opensubsonic.netlify.app/docs/endpoints/getartist/>
    pub async fn get_artist(&self, id: &str) -> Result<ArtistWithAlbumsID3> {
        self.call("getArtist", Params::new().req("id", id), "artist")
            .await
    }

    /// An album with its songs, by ID3 tags.
    ///
    /// <https://opensubsonic.netlify.app/docs/endpoints/getalbum/>
    pub async fn get_album(&self, id: &str) -> Result<AlbumID3WithSongs> {
        self.call("getAlbum", Params::new().req("id", id), "album")
            .await
    }

    /// One song.
    ///
    /// <https://opensubsonic.netlify.app/docs/endpoints/getsong/>
    pub async fn get_song(&self, id: &str) -> Result<Child> {
        self.call("getSong", Params::new().req("id", id), "song")
            .await
    }

    /// Biography, images and similar artists, by file structure. `id` is an
    /// artist, album or song; `count` caps similar artists (default 20);
    /// `include_not_present` adds similar artists missing from the library.
    ///
    /// <https://opensubsonic.netlify.app/docs/endpoints/getartistinfo/>
    pub async fn get_artist_info(
        &self,
        id: &str,
        count: Option<u32>,
        include_not_present: Option<bool>,
    ) -> Result<ArtistInfo> {
        let params = Params::new()
            .req("id", id)
            .opt("count", count)
            .opt("includeNotPresent", include_not_present);
        self.call("getArtistInfo", params, "artistInfo").await
    }

    /// Like [`Client::get_artist_info`], by ID3 tags.
    ///
    /// <https://opensubsonic.netlify.app/docs/endpoints/getartistinfo2/>
    pub async fn get_artist_info2(
        &self,
        id: &str,
        count: Option<u32>,
        include_not_present: Option<bool>,
    ) -> Result<ArtistInfo2> {
        let params = Params::new()
            .req("id", id)
            .opt("count", count)
            .opt("includeNotPresent", include_not_present);
        self.call("getArtistInfo2", params, "artistInfo2").await
    }

    /// Album notes and images. `id` is an album or song.
    ///
    /// <https://opensubsonic.netlify.app/docs/endpoints/getalbuminfo/>
    pub async fn get_album_info(&self, id: &str) -> Result<AlbumInfo> {
        self.call("getAlbumInfo", Params::new().req("id", id), "albumInfo")
            .await
    }

    /// Like [`Client::get_album_info`], by ID3 tags.
    ///
    /// <https://opensubsonic.netlify.app/docs/endpoints/getalbuminfo2/>
    pub async fn get_album_info2(&self, id: &str) -> Result<AlbumInfo> {
        self.call("getAlbumInfo2", Params::new().req("id", id), "albumInfo")
            .await
    }

    /// Random songs by the artist and similar artists. `id` is an artist,
    /// album or song; `count` defaults to 50.
    ///
    /// <https://opensubsonic.netlify.app/docs/endpoints/getsimilarsongs/>
    pub async fn get_similar_songs(&self, id: &str, count: Option<u32>) -> Result<SimilarSongs> {
        self.call(
            "getSimilarSongs",
            Params::new().req("id", id).opt("count", count),
            "similarSongs",
        )
        .await
    }

    /// Like [`Client::get_similar_songs`], by ID3 tags. `id` is an artist.
    ///
    /// <https://opensubsonic.netlify.app/docs/endpoints/getsimilarsongs2/>
    pub async fn get_similar_songs2(&self, id: &str, count: Option<u32>) -> Result<SimilarSongs2> {
        self.call(
            "getSimilarSongs2",
            Params::new().req("id", id).opt("count", count),
            "similarSongs2",
        )
        .await
    }

    /// Top songs of the artist with this name. `count` defaults to 50.
    ///
    /// <https://opensubsonic.netlify.app/docs/endpoints/gettopsongs/>
    pub async fn get_top_songs(&self, artist: &str, count: Option<u32>) -> Result<TopSongs> {
        self.call(
            "getTopSongs",
            Params::new().req("artist", artist).opt("count", count),
            "topSongs",
        )
        .await
    }

    /// Top songs of the artist with this ID. Needs `topSongsByArtistId`.
    ///
    /// <https://opensubsonic.netlify.app/docs/endpoints/gettopsongs/>
    pub async fn get_top_songs_by_artist_id(
        &self,
        id: &str,
        count: Option<u32>,
    ) -> Result<TopSongs> {
        self.call(
            "getTopSongs",
            Params::new().req("id", id).opt("count", count),
            "topSongs",
        )
        .await
    }

    // Album and song lists

    /// A list of albums, by file structure.
    ///
    /// <https://opensubsonic.netlify.app/docs/endpoints/getalbumlist/>
    pub async fn get_album_list(
        &self,
        list: &AlbumListType,
        options: &ListOptions<'_>,
    ) -> Result<AlbumList> {
        self.call(
            "getAlbumList",
            album_list_params(list, options),
            "albumList",
        )
        .await
    }

    /// A list of albums, by ID3 tags.
    ///
    /// <https://opensubsonic.netlify.app/docs/endpoints/getalbumlist2/>
    pub async fn get_album_list2(
        &self,
        list: &AlbumListType,
        options: &ListOptions<'_>,
    ) -> Result<AlbumList2> {
        self.call(
            "getAlbumList2",
            album_list_params(list, options),
            "albumList2",
        )
        .await
    }

    /// Random songs.
    ///
    /// <https://opensubsonic.netlify.app/docs/endpoints/getrandomsongs/>
    pub async fn get_random_songs(&self, options: &RandomSongsOptions<'_>) -> Result<Songs> {
        let params = Params::new()
            .opt("size", options.size)
            .opt("genre", options.genre)
            .opt("fromYear", options.from_year)
            .opt("toYear", options.to_year)
            .opt("musicFolderId", options.music_folder_id);
        self.call("getRandomSongs", params, "randomSongs").await
    }

    /// Songs in a genre, as named by [`Client::get_genres`].
    ///
    /// <https://opensubsonic.netlify.app/docs/endpoints/getsongsbygenre/>
    pub async fn get_songs_by_genre(
        &self,
        genre: &str,
        options: &ListOptions<'_>,
    ) -> Result<Songs> {
        let params = Params::new()
            .req("genre", genre)
            .opt("count", options.size)
            .opt("offset", options.offset)
            .opt("musicFolderId", options.music_folder_id);
        self.call("getSongsByGenre", params, "songsByGenre").await
    }

    /// What all users are playing now.
    ///
    /// <https://opensubsonic.netlify.app/docs/endpoints/getnowplaying/>
    pub async fn get_now_playing(&self) -> Result<NowPlaying> {
        self.call("getNowPlaying", Params::new(), "nowPlaying")
            .await
    }

    /// Starred songs, albums and artists, by file structure.
    ///
    /// <https://opensubsonic.netlify.app/docs/endpoints/getstarred/>
    pub async fn get_starred(&self, music_folder_id: Option<&str>) -> Result<Starred> {
        self.call(
            "getStarred",
            Params::new().opt("musicFolderId", music_folder_id),
            "starred",
        )
        .await
    }

    /// Starred songs, albums and artists, by ID3 tags.
    ///
    /// <https://opensubsonic.netlify.app/docs/endpoints/getstarred2/>
    pub async fn get_starred2(&self, music_folder_id: Option<&str>) -> Result<Starred2> {
        self.call(
            "getStarred2",
            Params::new().opt("musicFolderId", music_folder_id),
            "starred2",
        )
        .await
    }

    // Searching

    /// Artists, albums and songs matching `query`, by file structure.
    ///
    /// <https://opensubsonic.netlify.app/docs/endpoints/search2/>
    pub async fn search2(&self, query: &str, options: &SearchOptions<'_>) -> Result<SearchResult2> {
        self.call(
            "search2",
            options.params(Params::new().req("query", query)),
            "searchResult2",
        )
        .await
    }

    /// Artists, albums and songs matching `query`, by ID3 tags. An empty
    /// query returns everything, page by page.
    ///
    /// <https://opensubsonic.netlify.app/docs/endpoints/search3/>
    pub async fn search3(&self, query: &str, options: &SearchOptions<'_>) -> Result<SearchResult3> {
        self.call(
            "search3",
            options.params(Params::new().req("query", query)),
            "searchResult3",
        )
        .await
    }

    // Playlists

    /// The playlists the user may play. `username` (admins only) lists
    /// another user's playlists.
    ///
    /// <https://opensubsonic.netlify.app/docs/endpoints/getplaylists/>
    pub async fn get_playlists(&self, username: Option<&str>) -> Result<Playlists> {
        self.call(
            "getPlaylists",
            Params::new().opt("username", username),
            "playlists",
        )
        .await
    }

    /// A playlist with its songs.
    ///
    /// <https://opensubsonic.netlify.app/docs/endpoints/getplaylist/>
    pub async fn get_playlist(&self, id: &str) -> Result<PlaylistWithSongs> {
        self.call("getPlaylist", Params::new().req("id", id), "playlist")
            .await
    }

    /// Creates a playlist with these songs. Servers older than API 1.14.0
    /// return `None`.
    ///
    /// <https://opensubsonic.netlify.app/docs/endpoints/createplaylist/>
    pub async fn create_playlist(
        &self,
        name: &str,
        song_ids: &[&str],
    ) -> Result<Option<PlaylistWithSongs>> {
        self.call(
            "createPlaylist",
            Params::new().req("name", name).list("songId", song_ids),
            "playlist",
        )
        .await
    }

    /// Replaces all songs of a playlist (`createPlaylist` with a
    /// `playlistId`). Servers older than API 1.14.0 return `None`.
    ///
    /// <https://opensubsonic.netlify.app/docs/endpoints/createplaylist/>
    pub async fn replace_playlist(
        &self,
        playlist_id: &str,
        song_ids: &[&str],
    ) -> Result<Option<PlaylistWithSongs>> {
        let params = Params::new()
            .req("playlistId", playlist_id)
            .list("songId", song_ids);
        self.call("createPlaylist", params, "playlist").await
    }

    /// Renames, describes, shares or edits the songs of a playlist. Only its
    /// owner may.
    ///
    /// <https://opensubsonic.netlify.app/docs/endpoints/updateplaylist/>
    pub async fn update_playlist(
        &self,
        playlist_id: &str,
        update: &PlaylistUpdate<'_>,
    ) -> Result<()> {
        let params = Params::new()
            .req("playlistId", playlist_id)
            .opt("name", update.name)
            .opt("comment", update.comment)
            .opt("public", update.public)
            .list("songIdToAdd", update.song_ids_to_add)
            .list("songIndexToRemove", update.song_indexes_to_remove);
        self.call_unit("updatePlaylist", params).await
    }

    /// Deletes a playlist.
    ///
    /// <https://opensubsonic.netlify.app/docs/endpoints/deleteplaylist/>
    pub async fn delete_playlist(&self, id: &str) -> Result<()> {
        self.call_unit("deletePlaylist", Params::new().req("id", id))
            .await
    }

    // Media retrieval

    /// A cover art image, scaled to `size` pixels when given. `id` is the
    /// `coverArt` field of a song, album, artist or playlist.
    ///
    /// <https://opensubsonic.netlify.app/docs/endpoints/getcoverart/>
    pub async fn get_cover_art(&self, id: &str, size: Option<u32>) -> Result<Vec<u8>> {
        self.bytes("getCoverArt", Params::new().req("id", id).opt("size", size))
            .await
    }

    /// A user's avatar image.
    ///
    /// <https://opensubsonic.netlify.app/docs/endpoints/getavatar/>
    pub async fn get_avatar(&self, username: &str) -> Result<Vec<u8>> {
        self.bytes("getAvatar", Params::new().req("username", username))
            .await
    }

    /// An album's motion cover, the looping video a shop shows in place of
    /// its sleeve, as an animated WebP scaled to `size` pixels when given.
    /// Not in the spec: Fonoteca's own call, offered where the client
    /// [`supports`](Client::supports) `fonotecaAlbumMotion`. `id` is the
    /// album's id.
    pub async fn get_album_motion(&self, id: &str, size: Option<u32>) -> Result<Vec<u8>> {
        self.bytes(
            "getAlbumMotion",
            Params::new().req("id", id).opt("size", size),
        )
        .await
    }

    /// Everyone credited as composer of a classical piece the server holds,
    /// indexed by letter. Not in the spec: Fonoteca's own call, offered where
    /// the client [`supports`](Client::supports) `fonotecaCompositions`.
    pub async fn get_composers(&self) -> Result<Composers> {
        self.call("getComposers", Params::new(), "composers").await
    }

    /// The classical pieces the server holds, or only those `composer_id`
    /// wrote, sorted by name. Not in the spec: Fonoteca's own call, offered
    /// where the client [`supports`](Client::supports) `fonotecaCompositions`.
    pub async fn get_compositions(&self, composer_id: Option<&str>) -> Result<Compositions> {
        self.call(
            "getCompositions",
            Params::new().opt("composerId", composer_id),
            "compositions",
        )
        .await
    }

    /// One classical piece with its movements and every recording of it.
    /// Not in the spec: Fonoteca's `getCompositions` with an `id`, offered
    /// where the client [`supports`](Client::supports) `fonotecaCompositions`.
    pub async fn get_composition(&self, id: &str) -> Result<Composition> {
        let list: Compositions = self
            .call(
                "getCompositions",
                Params::new().req("id", id),
                "compositions",
            )
            .await?;
        list.composition
            .into_iter()
            .next()
            .ok_or_else(|| Error::Unexpected(format!("no composition {id}")))
    }

    /// Lyrics found by artist name and song title.
    ///
    /// <https://opensubsonic.netlify.app/docs/endpoints/getlyrics/>
    pub async fn get_lyrics(&self, artist: Option<&str>, title: Option<&str>) -> Result<Lyrics> {
        self.call(
            "getLyrics",
            Params::new().opt("artist", artist).opt("title", title),
            "lyrics",
        )
        .await
    }

    /// All lyrics of a song, synced where available. Needs `songLyrics`;
    /// `enhanced` (version 2) adds word timing, translations and
    /// pronunciations.
    ///
    /// <https://opensubsonic.netlify.app/docs/endpoints/getlyricsbysongid/>
    pub async fn get_lyrics_by_song_id(&self, id: &str, enhanced: bool) -> Result<LyricsList> {
        let params = Params::new()
            .req("id", id)
            .opt("enhanced", enhanced.then_some(true));
        self.call("getLyricsBySongId", params, "lyricsList").await
    }

    // Media annotation

    /// Stars songs or folders (`ids`), albums and artists. By ID3 tags, use
    /// `album_ids` and `artist_ids` for albums and artists.
    ///
    /// <https://opensubsonic.netlify.app/docs/endpoints/star/>
    pub async fn star(&self, ids: &[&str], album_ids: &[&str], artist_ids: &[&str]) -> Result<()> {
        self.call_unit("star", star_params(ids, album_ids, artist_ids))
            .await
    }

    /// Removes stars, the same way [`Client::star`] sets them.
    ///
    /// <https://opensubsonic.netlify.app/docs/endpoints/unstar/>
    pub async fn unstar(
        &self,
        ids: &[&str],
        album_ids: &[&str],
        artist_ids: &[&str],
    ) -> Result<()> {
        self.call_unit("unstar", star_params(ids, album_ids, artist_ids))
            .await
    }

    /// Rates a song, album or artist from 1 to 5; 0 removes the rating.
    ///
    /// <https://opensubsonic.netlify.app/docs/endpoints/setrating/>
    pub async fn set_rating(&self, id: &str, rating: u8) -> Result<()> {
        self.call_unit(
            "setRating",
            Params::new().req("id", id).req("rating", rating),
        )
        .await
    }

    // Sonic similarity

    /// Songs that sound like this one, by audio analysis. Needs
    /// `sonicSimilarity`; `count` defaults to 10.
    ///
    /// <https://opensubsonic.netlify.app/docs/endpoints/getsonicsimilartracks/>
    pub async fn get_sonic_similar_tracks(
        &self,
        id: &str,
        count: Option<u32>,
    ) -> Result<Vec<SonicMatch>> {
        self.call(
            "getSonicSimilarTracks",
            Params::new().req("id", id).opt("count", count),
            "sonicMatch",
        )
        .await
    }

    /// A path of songs from one song to another through similar sound,
    /// starting and ending with those two. Needs `sonicSimilarity`; `count`
    /// defaults to 25.
    ///
    /// <https://opensubsonic.netlify.app/docs/endpoints/findsonicpath/>
    pub async fn find_sonic_path(
        &self,
        start_song_id: &str,
        end_song_id: &str,
        count: Option<u32>,
    ) -> Result<Vec<SonicMatch>> {
        let params = Params::new()
            .req("startSongId", start_song_id)
            .req("endSongId", end_song_id)
            .opt("count", count);
        self.call("findSonicPath", params, "sonicMatch").await
    }
}

fn album_list_params(list: &AlbumListType, options: &ListOptions<'_>) -> Params {
    list.params(Params::new())
        .opt("size", options.size)
        .opt("offset", options.offset)
        .opt("musicFolderId", options.music_folder_id)
}

fn star_params(ids: &[&str], album_ids: &[&str], artist_ids: &[&str]) -> Params {
    Params::new()
        .list("id", ids)
        .list("albumId", album_ids)
        .list("artistId", artist_ids)
}
