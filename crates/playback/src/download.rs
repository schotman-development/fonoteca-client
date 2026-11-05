//! A track as it downloads: written to a file on disk by the network side and
//! read by the decoder, which waits for bytes that haven't arrived yet. A
//! long hi-res track runs to hundreds of megabytes, too much to hold in
//! memory on a phone. A live stream has no end, so it is kept in a file of
//! a fixed size that is written round and round: what the player hasn't
//! taken by the time it is written over is dropped.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

#[derive(Default)]
struct State {
    written: u64,
    /// The size the server announced, if it did.
    total: Option<u64>,
    done: bool,
    failed: Option<String>,
    /// How many readers there are; once none, the rest isn't wanted.
    readers: usize,
    /// A [`Probing`] is held.
    probing: bool,
    /// How far the fill has begun writing, ahead of `written` while it
    /// writes, so a reader can tell whether what it read was written over.
    writing: u64,
}

#[derive(Default)]
struct Inner {
    state: Mutex<State>,
    arrived: Condvar,
}

/// The reading side, handed to the player.
pub struct Download {
    file: File,
    position: u64,
    /// Where in the track the download begins, in seconds, when its source
    /// can begin a track anywhere; none for a whole file.
    starts_at: Option<f64>,
    /// The file's size when it is written round: a live stream.
    ring: Option<u64>,
    inner: Arc<Inner>,
}

/// Held while FFmpeg opens a download and reads enough of it to know the
/// track's streams. A transcoded track has no announced length, and FFmpeg
/// asks for the end of a file while opening it (its size, an ID3 tag), which
/// would otherwise hold the track back until every byte has arrived. While
/// this is held, such an answer is refused for a download of unknown length
/// that is still arriving, and FFmpeg goes on without.
pub struct Probing(Arc<Inner>);

impl Drop for Probing {
    fn drop(&mut self) {
        self.0.state.lock().unwrap().probing = false;
    }
}

/// The writing side, for whatever fetches the track.
pub struct Fill {
    file: File,
    ring: Option<u64>,
    inner: Arc<Inner>,
}

/// Why a download stopped arriving, read after the download itself has gone
/// to FFmpeg, which reports every failed read as the same I/O error.
pub struct Failure(Arc<Inner>);

impl Failure {
    pub fn reason(&self) -> Option<String> {
        self.0.state.lock().unwrap().failed.clone()
    }
}

/// How long a read waits before it lets the decoder check whether it should
/// stop waiting, for a skip or a seek.
const WAIT: Duration = Duration::from_millis(100);

/// A new, empty download in `folder`, `total` bytes long when the size is
/// known. The file has no name left once both sides have it open, so
/// nothing stays behind on disk, even after a crash.
pub fn new(folder: &Path, total: Option<u64>) -> io::Result<(Download, Fill)> {
    create(folder, total, None)
}

/// A new, empty download of a live stream: it never ends, and keeps the last
/// `ring` bytes of it, which is all the disk it takes.
pub fn live(folder: &Path, ring: u64) -> io::Result<(Download, Fill)> {
    create(folder, None, Some(ring))
}

fn create(folder: &Path, total: Option<u64>, ring: Option<u64>) -> io::Result<(Download, Fill)> {
    std::fs::create_dir_all(folder)?;
    let path = loop {
        let path = folder.join(format!("track-{:016x}", random()));
        match File::create_new(&path) {
            Ok(_) => break path,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    };
    let opened = File::options()
        .write(true)
        .open(&path)
        .and_then(|write| Ok((write, File::open(&path)?)));
    let _ = std::fs::remove_file(&path);
    let (write, read) = opened?;
    let inner = Arc::new(Inner::default());
    {
        let mut state = inner.state.lock().unwrap();
        state.total = total;
        state.readers = 1;
    }
    let download = Download {
        file: read,
        position: 0,
        starts_at: None,
        ring,
        inner: inner.clone(),
    };
    Ok((
        download,
        Fill {
            file: write,
            ring,
            inner,
        },
    ))
}

/// A track already on disk at `path`, whole and ready: nothing is waited for
/// and nothing is written.
pub fn open(path: &Path) -> io::Result<Download> {
    let file = File::open(path)?;
    let length = file.metadata()?.len();
    let inner = Arc::new(Inner::default());
    *inner.state.lock().unwrap() = State {
        written: length,
        total: Some(length),
        done: true,
        readers: 1,
        ..State::default()
    };
    Ok(Download {
        file,
        position: 0,
        starts_at: None,
        ring: None,
        inner,
    })
}

fn random() -> u64 {
    use std::hash::{BuildHasher, RandomState};
    RandomState::new().hash_one(std::time::SystemTime::now())
}

impl Fill {
    /// Adds the next bytes. False once the player no longer wants them, or
    /// the disk refused them, which the reading side then reports.
    pub fn append(&mut self, bytes: &[u8]) -> bool {
        if self.inner.state.lock().unwrap().readers == 0 {
            return false;
        }
        {
            let mut state = self.inner.state.lock().unwrap();
            state.writing = state.written + bytes.len() as u64;
        }
        let result = self.write(bytes);
        let mut state = self.inner.state.lock().unwrap();
        match result {
            Ok(()) => state.written += bytes.len() as u64,
            Err(e) => state.failed = Some(format!("The track couldn't be saved to disk: {e}")),
        }
        self.inner.arrived.notify_all();
        state.failed.is_none()
    }

    /// Writes `bytes` at the end, round to the file's start where it is a
    /// ring.
    fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        let Some(ring) = self.ring else {
            return self.file.write_all(bytes);
        };
        let mut at = self.inner.state.lock().unwrap().written;
        let mut rest = bytes;
        while !rest.is_empty() {
            let offset = at % ring;
            let (now, later) = rest.split_at(rest.len().min((ring - offset) as usize));
            self.file.seek(SeekFrom::Start(offset))?;
            self.file.write_all(now)?;
            at += now.len() as u64;
            rest = later;
        }
        Ok(())
    }

    /// Whether the player still reads it.
    pub fn wanted(&self) -> bool {
        self.inner.state.lock().unwrap().readers > 0
    }

    /// All of it has arrived.
    pub fn finish(self) {
        self.inner.state.lock().unwrap().done = true;
        self.inner.arrived.notify_all();
    }

    /// The rest won't come, for `reason`.
    pub fn fail(self, reason: String) {
        self.inner.state.lock().unwrap().failed = Some(reason);
        self.inner.arrived.notify_all();
    }
}

/// A fetch that ends without saying so, such as a panic, would leave the
/// reader waiting for ever.
impl Drop for Fill {
    fn drop(&mut self) {
        let mut state = self.inner.state.lock().unwrap();
        if !state.done && state.failed.is_none() {
            state.failed = Some("The download ended before all of it arrived.".into());
            self.inner.arrived.notify_all();
        }
    }
}

impl Download {
    /// Marks a download whose source begins a track wherever it is asked
    /// to, as this one does `seconds` into it. Until all of it has arrived,
    /// a seek then fetches the track again from there instead of reading on
    /// to it, which in a download of unknown length waits until then.
    pub fn starting_at(mut self, seconds: f64) -> Self {
        self.starts_at = Some(seconds);
        self
    }

    /// Where in the track it begins, when its source begins a track anywhere.
    pub fn starts_at(&self) -> Option<f64> {
        self.starts_at
    }

    /// Whether it is a stream with no end.
    pub fn is_live(&self) -> bool {
        self.ring.is_some()
    }

    /// Whether every byte has arrived.
    pub fn complete(&self) -> bool {
        self.inner.state.lock().unwrap().done
    }

    /// Has the end of this download not been announced, and not come, while
    /// FFmpeg is only opening it?
    fn length_unknown_while_probing(&self) -> bool {
        let state = self.inner.state.lock().unwrap();
        state.probing && state.total.is_none() && !state.done
    }

    /// Makes the end of a download of unknown length unanswerable, until the
    /// guard is dropped.
    pub fn probing(&self) -> Probing {
        self.inner.state.lock().unwrap().probing = true;
        Probing(self.inner.clone())
    }

    /// Where to ask why it stopped arriving, once it has been handed on.
    pub fn failure(&self) -> Failure {
        Failure(self.inner.clone())
    }

    /// Another reader of the same download, from its start, so the track
    /// can be opened again without fetching it again.
    pub fn reader(&self) -> io::Result<Download> {
        let file = self.file.try_clone()?;
        self.inner.state.lock().unwrap().readers += 1;
        Ok(Download {
            file,
            position: 0,
            starts_at: self.starts_at,
            ring: self.ring,
            inner: self.inner.clone(),
        })
    }

    /// Waits until `until` holds or everything has arrived, and returns how
    /// many bytes have and the size announced. After at most `WAIT` it gives
    /// up with `Interrupted`, so the decoder can check whether it still wants
    /// them and try again.
    fn wait(&self, until: impl Fn(&State) -> bool) -> io::Result<(u64, Option<u64>)> {
        let state = self.inner.state.lock().unwrap();
        let (state, _) = self
            .inner
            .arrived
            .wait_timeout_while(state, WAIT, |state| {
                !until(state) && !state.done && state.failed.is_none()
            })
            .unwrap();
        if let Some(reason) = &state.failed {
            return Err(io::Error::other(reason.clone()));
        }
        if !until(&state) && !state.done {
            return Err(io::ErrorKind::Interrupted.into());
        }
        Ok((state.written, state.total))
    }
}

impl Read for Download {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            let (written, _) = self.wait(|state| state.written > self.position)?;
            // What was written over is gone: go on from the oldest left.
            let oldest = self.ring.map_or(0, |ring| written.saturating_sub(ring));
            let position = self.position.max(oldest);
            let (offset, room) = match self.ring {
                Some(ring) => (position % ring, ring - position % ring),
                None => (position, u64::MAX),
            };
            let available = written.saturating_sub(position).min(room);
            let wanted = (buf.len() as u64).min(available) as usize;
            if wanted == 0 {
                return Ok(0);
            }
            self.file.seek(SeekFrom::Start(offset))?;
            let read = self.file.read(&mut buf[..wanted])?;
            // The fill may have come round to what was just read.
            let overwritten = self.ring.is_some_and(|ring| {
                self.inner
                    .state
                    .lock()
                    .unwrap()
                    .writing
                    .saturating_sub(ring)
                    > position
            });
            if overwritten {
                self.position = position;
                continue;
            }
            self.position = position + read as u64;
            return Ok(read);
        }
    }
}

impl Seek for Download {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let position = match to {
            SeekFrom::Start(at) => at,
            SeekFrom::Current(by) => self.position.saturating_add_signed(by),
            SeekFrom::End(by) => {
                if self.is_live() || self.length_unknown_while_probing() {
                    return Err(io::ErrorKind::Unsupported.into());
                }
                let (written, total) = self.wait(|state| state.total.is_some())?;
                total.unwrap_or(written).saturating_add_signed(by)
            }
        };
        self.position = position;
        Ok(position)
    }
}

impl Drop for Download {
    fn drop(&mut self) {
        self.inner.state.lock().unwrap().readers -= 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_wait_for_bytes_and_the_file_leaves_no_name() {
        let folder = std::env::temp_dir().join(format!("playback-test-{:x}", random()));
        let (mut download, mut fill) = new(&folder, Some(6)).unwrap();
        assert_eq!(std::fs::read_dir(&folder).unwrap().count(), 0);

        let mut buf = [0; 4];
        let early = download.read(&mut buf).unwrap_err();
        assert_eq!(early.kind(), io::ErrorKind::Interrupted);

        let writer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            assert!(fill.append(b"abc"));
            assert!(fill.append(b"def"));
            fill.finish();
        });
        let mut all = Vec::new();
        loop {
            match download.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => all.extend_from_slice(&buf[..n]),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => panic!("{e}"),
            }
        }
        writer.join().unwrap();
        assert_eq!(all, b"abcdef");
        assert_eq!(download.seek(SeekFrom::End(-2)).unwrap(), 4);
        assert_eq!(download.read(&mut buf).unwrap(), 2);
        assert_eq!(&buf[..2], b"ef");
        std::fs::remove_dir(&folder).unwrap();
    }

    #[test]
    fn a_file_on_disk_is_read_whole_without_waiting() {
        let folder = std::env::temp_dir().join(format!("playback-test-{:x}", random()));
        std::fs::create_dir_all(&folder).unwrap();
        let path = folder.join("song");
        std::fs::write(&path, b"abcdef").unwrap();

        let mut download = open(&path).unwrap();
        assert!(download.complete());
        assert_eq!(download.seek(SeekFrom::End(-2)).unwrap(), 4);
        let mut tail = Vec::new();
        download.read_to_end(&mut tail).unwrap();
        assert_eq!(tail, b"ef");
        let mut again = download.reader().unwrap();
        let mut all = Vec::new();
        again.read_to_end(&mut all).unwrap();
        assert_eq!(all, b"abcdef");
        assert!(open(&folder.join("missing")).is_err());
        drop((download, again));
        std::fs::remove_dir_all(&folder).unwrap();
    }

    #[test]
    fn a_failure_reaches_the_reader_and_a_dropped_reader_stops_the_writer() {
        let folder = std::env::temp_dir().join(format!("playback-test-{:x}", random()));
        let (mut download, fill) = new(&folder, None).unwrap();
        fill.fail("gone".into());
        let error = download.read(&mut [0; 4]).unwrap_err();
        assert_eq!(error.to_string(), "gone");

        let (download, mut fill) = new(&folder, None).unwrap();
        let mut second = download.reader().unwrap();
        drop(download);
        assert!(fill.append(b"x"));
        fill.finish();
        let mut all = Vec::new();
        second.read_to_end(&mut all).unwrap();
        assert_eq!(all, b"x");

        let (download, mut fill) = new(&folder, None).unwrap();
        drop(download);
        assert!(!fill.append(b"x"));
        std::fs::remove_dir(&folder).unwrap();
    }

    #[test]
    fn a_live_download_keeps_the_last_bytes_only_and_reads_them_in_order() {
        let folder = std::env::temp_dir().join(format!("playback-test-{:x}", random()));
        let (mut download, mut fill) = live(&folder, 8).unwrap();
        assert!(download.is_live());
        assert!(download.reader().unwrap().is_live());
        assert!(fill.wanted());

        // Fits, and wraps round the end of the file.
        assert!(fill.append(b"abcdef"));
        let mut buf = [0; 4];
        assert_eq!(download.read(&mut buf).unwrap(), 4);
        assert_eq!(&buf, b"abcd");
        assert!(fill.append(b"ghijkl"));
        let mut all = Vec::new();
        while all.len() < 8 {
            let read = download.read(&mut buf).unwrap();
            all.extend_from_slice(&buf[..read]);
        }
        assert_eq!(all, b"efghijkl");

        // A reader that fell behind by more than the file holds goes on from
        // the oldest bytes left, not from what is gone.
        assert!(fill.append(b"0123456789AB"));
        let mut all = Vec::new();
        while all.len() < 8 {
            let read = download.read(&mut buf).unwrap();
            all.extend_from_slice(&buf[..read]);
        }
        assert_eq!(all, b"456789AB");
        assert_eq!(
            std::fs::read_dir(&folder).unwrap().count(),
            0,
            "the file has no name"
        );
        // No end to seek from.
        assert_eq!(
            download.seek(SeekFrom::End(0)).unwrap_err().kind(),
            io::ErrorKind::Unsupported
        );
        drop(download);
        assert!(!fill.wanted());
        assert!(!fill.append(b"x"));
        std::fs::remove_dir(&folder).unwrap();
    }

    #[test]
    fn a_fill_that_ends_without_saying_so_fails_the_reader() {
        let folder = std::env::temp_dir().join(format!("playback-test-{:x}", random()));
        let (mut download, fill) = live(&folder, 8).unwrap();
        drop(fill);
        let error = download.read(&mut [0; 4]).unwrap_err();
        assert_ne!(error.kind(), io::ErrorKind::Interrupted);
        assert!(download.failure().reason().is_some());

        let (mut download, fill) = new(&folder, None).unwrap();
        fill.finish();
        assert_eq!(
            download.read(&mut [0; 4]).unwrap(),
            0,
            "finished is not failed"
        );
        assert_eq!(download.failure().reason(), None);
        std::fs::remove_dir(&folder).unwrap();
    }

    #[test]
    fn a_read_that_the_fill_is_writing_over_is_read_again_once_it_has_landed() {
        let folder = std::env::temp_dir().join(format!("playback-test-{:x}", random()));
        let (mut download, mut fill) = live(&folder, 8).unwrap();
        assert!(fill.append(b"abcdefgh"));
        assert_eq!(
            fill.inner.state.lock().unwrap().writing,
            8,
            "the fill says where it is writing before it writes"
        );
        let mut buf = [0; 4];
        assert_eq!(download.read(&mut buf).unwrap(), 4);
        // The fill is part way through six more bytes, which take the place
        // of "abcdef", though what it has written so far is not counted yet.
        fill.inner.state.lock().unwrap().writing = 14;
        let landing = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            assert!(fill.append(b"ijklmn"));
            fill
        });
        // "efgh" is still in the file, but not for long: the read waits for
        // the fill and goes on from the oldest left.
        let read = download.read(&mut buf).unwrap();
        assert_eq!(&buf[..read], b"gh");
        landing.join().unwrap();
        std::fs::remove_dir(&folder).unwrap();
    }

    #[test]
    fn a_live_download_reads_while_the_fill_writes_round_it() {
        let folder = std::env::temp_dir().join(format!("playback-test-{:x}", random()));
        let (mut download, mut fill) = live(&folder, 64).unwrap();
        let writer = std::thread::spawn(move || {
            for n in 0..2000u32 {
                assert!(fill.append(&n.to_be_bytes()));
            }
            fill.finish();
        });
        let mut all = Vec::new();
        let mut buf = [0; 16];
        loop {
            match download.read(&mut buf) {
                Ok(0) => break,
                Ok(read) => all.extend_from_slice(&buf[..read]),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => panic!("{e}"),
            }
        }
        writer.join().unwrap();
        // Whatever was read arrived in order, and the end of it is the end of
        // the stream.
        let numbers: Vec<u32> = all
            .as_chunks::<4>()
            .0
            .iter()
            .map(|n| u32::from_be_bytes(*n))
            .collect();
        assert!(numbers.windows(2).all(|pair| pair[0] < pair[1]), "in order");
        assert_eq!(numbers.last(), Some(&1999));
        std::fs::remove_dir(&folder).unwrap();
    }
}
