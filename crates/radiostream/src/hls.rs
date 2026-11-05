//! The part of an HLS playlist (RFC 8216) a live radio station uses: a master
//! playlist naming variants, or a media playlist listing the latest segments,
//! and which of its segments are new since the last look.

use reqwest::Url;

use crate::Error;

/// A live playlist is started this many segments from its end, as the
/// standard advises, so a segment late in coming doesn't run the player dry.
const SEGMENTS_BEHIND_LIVE: usize = 3;

#[derive(Debug, PartialEq)]
pub enum Playlist {
    Master(Vec<Variant>),
    Media(Media),
}

/// One way of hearing the station, as a master playlist offers it.
#[derive(Debug, PartialEq)]
pub struct Variant {
    pub url: Url,
    pub bandwidth: u64,
    /// It has pictures, which a radio station's audio variants don't.
    pub has_video: bool,
}

#[derive(Debug, PartialEq)]
pub struct Media {
    pub target_seconds: f64,
    pub segments: Vec<Segment>,
    /// The station will add no more segments.
    pub ended: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Segment {
    pub sequence: u64,
    pub url: Url,
    /// The bytes every segment from here on needs first, as fragmented MP4
    /// has.
    pub init: Option<Url>,
    /// The stream breaks here: timestamps and the init segment may change.
    pub discontinuity: bool,
}

impl Playlist {
    /// Reads `text`, whose URLs may be relative to `base`.
    pub fn parse(text: &str, base: &Url) -> Result<Playlist, Error> {
        let mut lines = text.lines().map(str::trim).filter(|line| !line.is_empty());
        if lines.next() != Some("#EXTM3U") {
            return Err(Error::Playlist("it doesn't start with #EXTM3U".into()));
        }
        let mut reading = Reading::default();
        for line in lines {
            match line.strip_prefix('#') {
                Some(tag) => reading.tag(tag, base)?,
                None => reading.uri(line, base)?,
            }
        }
        Ok(reading.finish())
    }

    /// The variant to play: the one with the most bandwidth that has no
    /// pictures, else the first.
    pub fn best_variant(variants: &[Variant]) -> Option<&Variant> {
        variants
            .iter()
            .filter(|variant| !variant.has_video)
            .max_by_key(|variant| variant.bandwidth)
            .or_else(|| variants.first())
    }
}

/// What the lines read so far say about the line to come.
#[derive(Default)]
struct Reading {
    variants: Vec<Variant>,
    segments: Vec<Segment>,
    target_seconds: f64,
    first_sequence: u64,
    ended: bool,
    /// Set by the tag before the URI line it describes.
    variant_attributes: Option<Vec<(String, String)>>,
    awaiting_segment: bool,
    discontinuity: bool,
    init: Option<Url>,
}

impl Reading {
    fn tag(&mut self, tag: &str, base: &Url) -> Result<(), Error> {
        let (name, value) = tag.split_once(':').unwrap_or((tag, ""));
        match name {
            "EXT-X-TARGETDURATION" => self.target_seconds = number(value, name)?,
            "EXT-X-MEDIA-SEQUENCE" => self.first_sequence = number(value, name)?,
            "EXT-X-ENDLIST" => self.ended = true,
            "EXT-X-DISCONTINUITY" => self.discontinuity = true,
            "EXT-X-STREAM-INF" => self.variant_attributes = Some(attributes(value)),
            "EXTINF" => self.awaiting_segment = true,
            "EXT-X-MAP" => {
                let map = attributes(value);
                let uri = attribute(&map, "URI")
                    .ok_or_else(|| Error::Playlist("EXT-X-MAP without a URI".into()))?;
                self.init = Some(join(base, uri)?);
            }
            "EXT-X-KEY" => {
                let key = attributes(value);
                if attribute(&key, "METHOD").is_some_and(|method| method != "NONE") {
                    return Err(Error::Unsupported("the stream is encrypted".into()));
                }
            }
            "EXT-X-BYTERANGE" => {
                return Err(Error::Unsupported(
                    "the stream is cut by byte ranges".into(),
                ));
            }
            _ => {}
        }
        Ok(())
    }

    fn uri(&mut self, uri: &str, base: &Url) -> Result<(), Error> {
        let url = join(base, uri)?;
        if let Some(attributes) = self.variant_attributes.take() {
            self.variants.push(Variant {
                url,
                bandwidth: attribute(&attributes, "BANDWIDTH")
                    .and_then(|bandwidth| bandwidth.parse().ok())
                    .unwrap_or(0),
                has_video: attribute(&attributes, "RESOLUTION").is_some(),
            });
        } else if std::mem::take(&mut self.awaiting_segment) {
            self.segments.push(Segment {
                sequence: self.first_sequence + self.segments.len() as u64,
                url,
                init: self.init.clone(),
                discontinuity: std::mem::take(&mut self.discontinuity),
            });
        }
        Ok(())
    }

    fn finish(self) -> Playlist {
        if !self.variants.is_empty() {
            return Playlist::Master(self.variants);
        }
        Playlist::Media(Media {
            target_seconds: self.target_seconds,
            segments: self.segments,
            ended: self.ended,
        })
    }
}

fn number<T: std::str::FromStr>(value: &str, tag: &str) -> Result<T, Error> {
    value
        .trim()
        .parse()
        .map_err(|_| Error::Playlist(format!("{tag} is not a number")))
}

fn join(base: &Url, uri: &str) -> Result<Url, Error> {
    base.join(uri)
        .map_err(|e| Error::Playlist(format!("{uri} is not an address: {e}")))
}

fn attribute<'a>(attributes: &'a [(String, String)], name: &str) -> Option<&'a str> {
    attributes
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.as_str())
}

/// `KEY=value,KEY="quoted, with commas"` as pairs, quotes removed.
fn attributes(list: &str) -> Vec<(String, String)> {
    let mut pairs = Vec::new();
    let (mut rest, mut quoted) = (list, false);
    while !rest.is_empty() {
        let end = rest
            .char_indices()
            .find(|&(_, c)| {
                quoted ^= c == '"';
                c == ',' && !quoted
            })
            .map_or(rest.len(), |(at, _)| at);
        if let Some((key, value)) = rest[..end].split_once('=') {
            pairs.push((
                key.trim().to_owned(),
                value.trim().trim_matches('"').to_owned(),
            ));
        }
        rest = rest.get(end + 1..).unwrap_or_default();
    }
    pairs
}

/// Which segments of a playlist read again and again are new.
#[derive(Debug, Default)]
pub struct Sequencer {
    next: Option<u64>,
}

impl Sequencer {
    /// The segments of `media` not handed out before, in order. The first
    /// look starts a few segments from the end of a live playlist, and the
    /// start of one that has ended. Segments that left the playlist before
    /// they were fetched are lost, and a playlist that starts over is
    /// followed from its new end.
    pub fn new_segments<'a>(&mut self, media: &'a Media) -> &'a [Segment] {
        let (Some(first), Some(last)) = (media.segments.first(), media.segments.last()) else {
            return &[];
        };
        let from = match self.next {
            Some(next) if (first.sequence..=last.sequence + 1).contains(&next) => {
                (next - first.sequence) as usize
            }
            Some(next) if next < first.sequence => 0,
            _ if media.ended => 0,
            _ => media.segments.len().saturating_sub(SEGMENTS_BEHIND_LIVE),
        };
        self.next = Some(last.sequence + 1);
        &media.segments[from..]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Url {
        Url::parse("https://radio.example/live/index.m3u8").unwrap()
    }

    fn media(text: &str) -> Media {
        match Playlist::parse(text, &base()).unwrap() {
            Playlist::Media(media) => media,
            other => panic!("not a media playlist: {other:?}"),
        }
    }

    fn live(first: u64, count: u64) -> Media {
        let mut text = format!("#EXTM3U\n#EXT-X-TARGETDURATION:5\n#EXT-X-MEDIA-SEQUENCE:{first}\n");
        for sequence in first..first + count {
            text += &format!("#EXTINF:5.0,\nseg{sequence}.ts\n");
        }
        media(&text)
    }

    fn sequences(segments: &[Segment]) -> Vec<u64> {
        segments.iter().map(|segment| segment.sequence).collect()
    }

    #[test]
    fn a_master_playlist_names_variants_and_the_best_without_pictures_is_chosen() {
        let text = "#EXTM3U\n\
            #EXT-X-STREAM-INF:BANDWIDTH=64000,CODECS=\"mp4a.40.5\"\nlow/index.m3u8\n\
            #EXT-X-STREAM-INF:BANDWIDTH=192000,CODECS=\"mp4a.40.2, avc1.4d401e\"\nhigh/index.m3u8\n\
            #EXT-X-STREAM-INF:BANDWIDTH=9000000,RESOLUTION=1280x720\nvideo/index.m3u8\n";
        let Playlist::Master(variants) = Playlist::parse(text, &base()).unwrap() else {
            panic!("not a master playlist");
        };
        assert_eq!(variants.len(), 3);
        assert_eq!(
            variants[0].url.as_str(),
            "https://radio.example/live/low/index.m3u8"
        );
        let best = Playlist::best_variant(&variants).unwrap();
        assert_eq!(best.bandwidth, 192_000);
        let only_video = &variants[2..];
        assert_eq!(
            Playlist::best_variant(only_video).unwrap().bandwidth,
            9_000_000
        );
        assert!(Playlist::best_variant(&[]).is_none());
    }

    #[test]
    fn a_media_playlist_numbers_its_segments_and_resolves_their_addresses() {
        let media = media(
            "#EXTM3U\n## a comment\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:6\n\
             #EXT-X-MEDIA-SEQUENCE:6298\n#EXTINF:5.013,\nrelative.ts\n\
             #EXTINF:4.992,\nhttps://cdn.example/absolute.ts\n#EXTINF:5,\n/rooted.ts\n",
        );
        assert_eq!(media.target_seconds, 6.0);
        assert!(!media.ended);
        assert_eq!(sequences(&media.segments), [6298, 6299, 6300]);
        let urls: Vec<&str> = media.segments.iter().map(|s| s.url.as_str()).collect();
        assert_eq!(
            urls,
            [
                "https://radio.example/live/relative.ts",
                "https://cdn.example/absolute.ts",
                "https://radio.example/rooted.ts",
            ]
        );
    }

    #[test]
    fn a_discontinuity_marks_only_the_segment_after_it_and_a_map_the_ones_it_covers() {
        let media = media(
            "#EXTM3U\n#EXT-X-TARGETDURATION:4\n#EXT-X-MAP:URI=\"init-a.mp4\",BYTERANGE_NOT=\"1,2\"\n\
             #EXTINF:4,\none.m4s\n#EXT-X-DISCONTINUITY\n#EXT-X-MAP:URI=\"init-b.mp4\"\n\
             #EXTINF:4,\ntwo.m4s\n#EXTINF:4,\nthree.m4s\n#EXT-X-ENDLIST\n",
        );
        assert!(media.ended);
        let flags: Vec<bool> = media.segments.iter().map(|s| s.discontinuity).collect();
        assert_eq!(flags, [false, true, false]);
        let inits: Vec<&str> = (media.segments.iter())
            .map(|s| s.init.as_ref().unwrap().path())
            .collect();
        assert_eq!(
            inits,
            ["/live/init-a.mp4", "/live/init-b.mp4", "/live/init-b.mp4"]
        );
    }

    #[test]
    fn what_cannot_be_played_is_said_so() {
        let parse = |text: &str| Playlist::parse(text, &base()).unwrap_err().to_string();
        assert!(parse("<html>").contains("#EXTM3U"));
        assert!(parse("#EXTM3U\n#EXT-X-KEY:METHOD=AES-128,URI=\"k\"\n").contains("encrypted"));
        assert!(parse("#EXTM3U\n#EXT-X-BYTERANGE:100@0\n").contains("byte ranges"));
        assert!(parse("#EXTM3U\n#EXT-X-TARGETDURATION:soon\n").contains("TARGETDURATION"));
        // A key that says there is none is fine.
        Playlist::parse("#EXTM3U\n#EXT-X-KEY:METHOD=NONE\n", &base()).unwrap();
    }

    #[test]
    fn attributes_keep_commas_inside_quotes() {
        let pairs = attributes("BANDWIDTH=1,CODECS=\"a,b\",NAME=x");
        assert_eq!(
            pairs,
            [("BANDWIDTH", "1"), ("CODECS", "a,b"), ("NAME", "x")]
                .map(|(key, value)| (key.to_owned(), value.to_owned()))
        );
    }

    #[test]
    fn a_live_playlist_starts_three_segments_from_its_end_and_then_hands_out_only_new_ones() {
        let mut sequencer = Sequencer::default();
        assert_eq!(
            sequences(sequencer.new_segments(&live(10, 6))),
            [13, 14, 15]
        );
        assert!(sequencer.new_segments(&live(10, 6)).is_empty());
        assert_eq!(sequences(sequencer.new_segments(&live(12, 6))), [16, 17]);
        // A playlist with fewer than three segments is played from its first.
        let mut sequencer = Sequencer::default();
        assert_eq!(sequences(sequencer.new_segments(&live(0, 2))), [0, 1]);
    }

    #[test]
    fn segments_that_left_the_playlist_are_skipped_and_a_restart_is_followed() {
        let mut sequencer = Sequencer::default();
        sequencer.new_segments(&live(10, 3));
        assert_eq!(
            sequences(sequencer.new_segments(&live(20, 3))),
            [20, 21, 22]
        );
        assert_eq!(sequences(sequencer.new_segments(&live(0, 5))), [2, 3, 4]);
    }

    #[test]
    fn a_playlist_that_has_ended_is_played_from_its_start() {
        let mut text = String::from("#EXTM3U\n#EXT-X-TARGETDURATION:5\n");
        for n in 0..5 {
            text += &format!("#EXTINF:5,\ns{n}.ts\n");
        }
        text += "#EXT-X-ENDLIST\n";
        let ended = media(&text);
        assert_eq!(
            sequences(Sequencer::default().new_segments(&ended)),
            [0, 1, 2, 3, 4]
        );
        assert!(
            Sequencer::default()
                .new_segments(&media("#EXTM3U\n"))
                .is_empty()
        );
    }
}
