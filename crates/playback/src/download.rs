//! A track as it downloads: written to a file on disk by the network side and
//! read by the decoder, which waits for bytes that haven't arrived yet. A
//! long hi-res track runs to hundreds of megabytes, too much to hold in
//! memory on a phone.

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
    inner: Arc<Inner>,
}

/// The writing side, for whatever fetches the track.
pub struct Fill {
    file: File,
    inner: Arc<Inner>,
}

/// How long a read waits before it lets the decoder check whether it should
/// stop waiting, for a skip or a seek.
const WAIT: Duration = Duration::from_millis(100);

/// A new, empty download in `folder`, `total` bytes long when the size is
/// known. The file has no name left once both sides have it open, so
/// nothing stays behind on disk, even after a crash.
pub fn new(folder: &Path, total: Option<u64>) -> io::Result<(Download, Fill)> {
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
        inner: inner.clone(),
    };
    Ok((download, Fill { file: write, inner }))
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
        let result = self.file.write_all(bytes);
        let mut state = self.inner.state.lock().unwrap();
        match result {
            Ok(()) => state.written += bytes.len() as u64,
            Err(e) => state.failed = Some(format!("The track couldn't be saved to disk: {e}")),
        }
        self.inner.arrived.notify_all();
        state.failed.is_none()
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

impl Download {
    /// Whether every byte has arrived.
    pub fn complete(&self) -> bool {
        self.inner.state.lock().unwrap().done
    }

    /// Another reader of the same download, from its start, so the track
    /// can be opened again without fetching it again.
    pub fn reader(&self) -> io::Result<Download> {
        let file = self.file.try_clone()?;
        self.inner.state.lock().unwrap().readers += 1;
        Ok(Download {
            file,
            position: 0,
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
        let position = self.position;
        let (written, _) = self.wait(|state| state.written > position)?;
        let available = written.saturating_sub(position);
        let wanted = (buf.len() as u64).min(available) as usize;
        if wanted == 0 {
            return Ok(0);
        }
        self.file.seek(SeekFrom::Start(position))?;
        let read = self.file.read(&mut buf[..wanted])?;
        self.position += read as u64;
        Ok(read)
    }
}

impl Seek for Download {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let position = match to {
            SeekFrom::Start(at) => at,
            SeekFrom::Current(by) => self.position.saturating_add_signed(by),
            SeekFrom::End(by) => {
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
}
