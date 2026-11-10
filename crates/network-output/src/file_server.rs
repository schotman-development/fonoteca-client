//! The file a Cast device plays, served to it over HTTP: the device fetches
//! the file itself, so it needs an address it can reach, and the app never
//! hands it the server's or any credential. One file is served at a time,
//! under a random path that only the device is told.

use std::fs::File;
use std::hash::{BuildHasher, Hasher, RandomState};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// A device that sends nothing for this long has gone.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// The longest request line or header taken in.
const MAX_LINE: u64 = 8 * 1024;
/// The most headers taken in before the request is turned down.
const MAX_HEADERS: usize = 64;
const CHUNK: usize = 64 * 1024;

#[derive(Clone)]
struct Served {
    /// What follows the slash in the URL.
    token: String,
    path: PathBuf,
    content_type: &'static str,
}

pub struct FileServer {
    address: SocketAddr,
    served: Arc<Mutex<Option<Served>>>,
    closed: Arc<AtomicBool>,
}

impl FileServer {
    /// Listens on `bind`, which is the address of the interface the device
    /// is reached over, at a port the system picks.
    pub fn start(bind: IpAddr) -> io::Result<Self> {
        let listener = TcpListener::bind((bind, 0))?;
        let address = listener.local_addr()?;
        let served = Arc::new(Mutex::new(None));
        let closed = Arc::new(AtomicBool::new(false));
        let (shared, stop) = (served.clone(), closed.clone());
        std::thread::Builder::new()
            .name("cast-files".into())
            .spawn(move || accept(listener, shared, stop))?;
        Ok(FileServer {
            address,
            served,
            closed,
        })
    }

    /// Serves the file at `path` in place of the one before.
    pub fn serve(&self, path: &Path) -> io::Result<Hosted> {
        let mut head = [0; 16];
        let read = File::open(path)?.read(&mut head)?;
        let served = Served {
            token: random_token(),
            path: path.to_owned(),
            content_type: content_type(&head[..read]),
        };
        let hosted = Hosted {
            url: format!("http://{}/{}", self.address, served.token),
            content_type: served.content_type,
            path: served.path.clone(),
        };
        *self.served.lock().unwrap() = Some(served);
        Ok(hosted)
    }
}

/// Where a device finds a file, and what it is told the file is; and the file
/// itself, for a device that is sent its audio.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hosted {
    pub url: String,
    pub content_type: &'static str,
    pub path: PathBuf,
}

impl Drop for FileServer {
    fn drop(&mut self) {
        self.closed.store(true, Ordering::SeqCst);
        // Wakes the accept that waits for a connection, to see it is closed.
        if let Err(problem) = TcpStream::connect(self.address) {
            eprintln!("The file server for a Cast device didn't stop: {problem}");
        }
    }
}

fn accept(listener: TcpListener, served: Arc<Mutex<Option<Served>>>, closed: Arc<AtomicBool>) {
    for connection in listener.incoming() {
        if closed.load(Ordering::SeqCst) {
            return;
        }
        let served = served.clone();
        match connection {
            Ok(stream) => {
                std::thread::spawn(move || {
                    if let Err(problem) = answer(stream, &served) {
                        eprintln!("A Cast device's request couldn't be answered: {problem}");
                    }
                });
            }
            Err(problem) => eprintln!("A Cast device's connection failed: {problem}"),
        }
    }
}

struct Request {
    is_head: bool,
    path: String,
    range: Option<String>,
}

/// None for a request that isn't a well-formed GET or HEAD, which is
/// answered as bad.
fn read_request(stream: &TcpStream) -> io::Result<Option<Request>> {
    let mut reader = BufReader::new(stream.take(MAX_LINE * (MAX_HEADERS as u64 + 1)));
    let mut line = String::new();
    reader.by_ref().take(MAX_LINE).read_line(&mut line)?;
    let mut parts = line.split_whitespace();
    let (Some(method), Some(path), Some(_version)) = (parts.next(), parts.next(), parts.next())
    else {
        return Ok(None);
    };
    let mut request = Request {
        is_head: method == "HEAD",
        path: path.to_owned(),
        range: None,
    };
    if method != "GET" && method != "HEAD" {
        return Ok(None);
    }
    for _ in 0..MAX_HEADERS {
        line.clear();
        reader.by_ref().take(MAX_LINE).read_line(&mut line)?;
        let header = line.trim_end();
        if header.is_empty() {
            return Ok(Some(request));
        }
        if let Some((name, value)) = header.split_once(':')
            && name.eq_ignore_ascii_case("range")
        {
            request.range = Some(value.trim().to_owned());
        }
    }
    Ok(None)
}

fn answer(mut stream: TcpStream, served: &Mutex<Option<Served>>) -> io::Result<()> {
    stream.set_read_timeout(Some(REQUEST_TIMEOUT))?;
    stream.set_write_timeout(Some(REQUEST_TIMEOUT))?;
    let Some(request) = read_request(&stream)? else {
        return respond(&mut stream, "400 Bad Request", &[]);
    };
    let served = served.lock().unwrap().clone();
    let Some(served) = served.filter(|s| request.path == format!("/{}", s.token)) else {
        return respond(&mut stream, "404 Not Found", &[]);
    };
    let mut file = File::open(&served.path)?;
    let length = file.metadata()?.len();
    let headers = [
        ("Content-Type", served.content_type.to_owned()),
        ("Accept-Ranges", "bytes".to_owned()),
        // The device's player asks for files as a web page would.
        ("Access-Control-Allow-Origin", "*".to_owned()),
    ];
    let range = match request
        .range
        .as_deref()
        .map(|range| byte_range(range, length))
    {
        None => 0..length,
        Some(Some(range)) => range,
        Some(None) => {
            let unsatisfied = [("Content-Range", format!("bytes */{length}"))];
            return respond(&mut stream, "416 Range Not Satisfiable", &unsatisfied);
        }
    };
    let (status, mut headers) = if range == (0..length) {
        ("200 OK", headers.to_vec())
    } else {
        let mut headers = headers.to_vec();
        let last = range.end - 1;
        headers.push((
            "Content-Range",
            format!("bytes {}-{last}/{length}", range.start),
        ));
        ("206 Partial Content", headers)
    };
    headers.push(("Content-Length", (range.end - range.start).to_string()));
    respond(&mut stream, status, &headers)?;
    if request.is_head {
        return Ok(());
    }
    file.seek(SeekFrom::Start(range.start))?;
    copy_exactly(&mut file, &mut stream, range.end - range.start)
}

fn respond(stream: &mut TcpStream, status: &str, headers: &[(&str, String)]) -> io::Result<()> {
    let mut head = format!("HTTP/1.1 {status}\r\nConnection: close\r\n");
    if headers.iter().all(|(name, _)| *name != "Content-Length") {
        head.push_str("Content-Length: 0\r\n");
    }
    for (name, value) in headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())
}

fn copy_exactly(from: &mut File, to: &mut TcpStream, mut left: u64) -> io::Result<()> {
    let mut chunk = vec![0; CHUNK];
    while left > 0 {
        let wanted = chunk.len().min(left as usize);
        let read = from.read(&mut chunk[..wanted])?;
        if read == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        to.write_all(&chunk[..read])?;
        left -= read as u64;
    }
    Ok(())
}

/// The bytes a `Range` header asks for out of `length`, as a range of
/// offsets; none when it asks for nothing the file has. Only one range is
/// served: of several, the first.
fn byte_range(header: &str, length: u64) -> Option<std::ops::Range<u64>> {
    let first = header.strip_prefix("bytes=")?.split(',').next()?.trim();
    let (start, end) = first.split_once('-')?;
    let (start, end) = match (start.trim(), end.trim()) {
        // The last `n` bytes.
        ("", n) => (length.saturating_sub(n.parse().ok()?), length),
        (start, "") => (start.parse().ok()?, length),
        (start, end) => (
            start.parse().ok()?,
            end.parse::<u64>().ok()?.checked_add(1)?.min(length),
        ),
    };
    (start < end && start < length).then_some(start..end)
}

/// What the first bytes of an audio file say it is.
fn content_type(head: &[u8]) -> &'static str {
    match head {
        [b'f', b'L', b'a', b'C', ..] => "audio/flac",
        [b'O', b'g', b'g', b'S', ..] => "audio/ogg",
        [
            b'R',
            b'I',
            b'F',
            b'F',
            _,
            _,
            _,
            _,
            b'W',
            b'A',
            b'V',
            b'E',
            ..,
        ] => "audio/wav",
        [_, _, _, _, b'f', b't', b'y', b'p', ..] => "audio/mp4",
        [b'I', b'D', b'3', ..] => "audio/mpeg",
        // A frame's sync bits; an ADTS frame's layer bits are zero, an MP3
        // frame's are not.
        [0xFF, second, ..] if second & 0xF6 == 0xF0 => "audio/aac",
        [0xFF, second, ..] if second & 0xE0 == 0xE0 => "audio/mpeg",
        _ => "application/octet-stream",
    }
}

/// 128 random bits from the standard library's randomly keyed hasher.
// ponytail: not a cryptographic generator; the token guards one audio file on the local network, so a getrandom-backed one can wait for something more valuable behind it.
fn random_token() -> String {
    let state = RandomState::new();
    (0..2)
        .map(|part| {
            let mut hasher = state.build_hasher();
            hasher.write_u8(part);
            format!("{:016x}", hasher.finish())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flac_file(bytes: usize) -> (tempdir::Dir, PathBuf) {
        let dir = tempdir::Dir::new();
        let path = dir.0.join("track");
        let mut content = b"fLaC".to_vec();
        content.extend((4..bytes).map(|i| (i % 251) as u8));
        std::fs::write(&path, content).unwrap();
        (dir, path)
    }

    /// A folder removed once the test is over.
    mod tempdir {
        use std::path::PathBuf;

        pub struct Dir(pub PathBuf);

        impl Dir {
            pub fn new() -> Self {
                let path =
                    std::env::temp_dir().join(format!("cast-files-{}", super::random_token()));
                std::fs::create_dir_all(&path).unwrap();
                Dir(path)
            }
        }

        impl Drop for Dir {
            fn drop(&mut self) {
                std::fs::remove_dir_all(&self.0).unwrap();
            }
        }
    }

    /// Sends `request` to `url`'s server and returns the head and the body
    /// of what comes back.
    fn fetch(hosted: &Hosted, method: &str, path: Option<&str>, extra: &str) -> (String, Vec<u8>) {
        let address = hosted
            .url
            .trim_start_matches("http://")
            .split('/')
            .next()
            .unwrap();
        let path = path.map_or_else(
            || hosted.url.split_once(address).unwrap().1.to_owned(),
            str::to_owned,
        );
        let mut stream = TcpStream::connect(address).unwrap();
        write!(
            stream,
            "{method} {path} HTTP/1.1\r\nHost: {address}\r\n{extra}\r\n"
        )
        .unwrap();
        let mut answer = Vec::new();
        stream.read_to_end(&mut answer).unwrap();
        let split = answer.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
        (
            String::from_utf8(answer[..split].to_vec()).unwrap(),
            answer[split + 4..].to_vec(),
        )
    }

    fn server() -> FileServer {
        FileServer::start(IpAddr::from([127, 0, 0, 1])).unwrap()
    }

    #[test]
    fn a_range_is_the_bytes_asked_for_cut_to_the_file() {
        assert_eq!(byte_range("bytes=0-99", 1000), Some(0..100));
        assert_eq!(byte_range("bytes=900-", 1000), Some(900..1000));
        assert_eq!(byte_range("bytes=-100", 1000), Some(900..1000));
        assert_eq!(byte_range("bytes=-5000", 1000), Some(0..1000));
        assert_eq!(byte_range("bytes=990-5000", 1000), Some(990..1000));
        assert_eq!(byte_range("bytes=0-9, 20-29", 1000), Some(0..10));
    }

    #[test]
    fn a_range_that_asks_for_nothing_the_file_has_is_none() {
        assert_eq!(byte_range("bytes=1000-", 1000), None);
        assert_eq!(byte_range("bytes=50-10", 1000), None);
        assert_eq!(byte_range("bytes=-0", 1000), None);
        assert_eq!(byte_range("items=0-9", 1000), None);
        assert_eq!(byte_range("bytes=a-b", 1000), None);
    }

    #[test]
    fn a_file_is_told_what_it_is_from_its_first_bytes() {
        assert_eq!(content_type(b"fLaC\0\0\0"), "audio/flac");
        assert_eq!(content_type(b"OggS"), "audio/ogg");
        assert_eq!(content_type(b"RIFF\0\0\0\0WAVEfmt "), "audio/wav");
        assert_eq!(content_type(b"\0\0\0\x20ftypM4A "), "audio/mp4");
        assert_eq!(content_type(b"ID3\x04"), "audio/mpeg");
        assert_eq!(content_type(&[0xFF, 0xFB, 0x90]), "audio/mpeg");
        assert_eq!(content_type(&[0xFF, 0xF1, 0x50]), "audio/aac");
        assert_eq!(content_type(b"hello"), "application/octet-stream");
    }

    #[test]
    fn the_file_is_served_whole_with_its_type_and_the_right_to_ask_for_parts() {
        let (_dir, path) = flac_file(1000);
        let server = server();
        let hosted = server.serve(&path).unwrap();
        assert_eq!(hosted.content_type, "audio/flac");
        let (head, body) = fetch(&hosted, "GET", None, "");
        assert!(head.starts_with("HTTP/1.1 200 OK"), "{head}");
        assert!(head.contains("Content-Type: audio/flac"), "{head}");
        assert!(head.contains("Accept-Ranges: bytes"), "{head}");
        assert_eq!(body, std::fs::read(&path).unwrap());
    }

    #[test]
    fn a_range_is_answered_with_that_part_and_where_it_lies() {
        let (_dir, path) = flac_file(1000);
        let server = server();
        let hosted = server.serve(&path).unwrap();
        let (head, body) = fetch(&hosted, "GET", None, "Range: bytes=100-199\r\n");
        assert!(head.starts_with("HTTP/1.1 206 Partial Content"), "{head}");
        assert!(head.contains("Content-Range: bytes 100-199/1000"), "{head}");
        assert!(head.contains("Content-Length: 100"), "{head}");
        assert_eq!(body, std::fs::read(&path).unwrap()[100..200]);
        let (head, body) = fetch(&hosted, "GET", None, "range: bytes=990-\r\n");
        assert!(head.contains("Content-Range: bytes 990-999/1000"), "{head}");
        assert_eq!(body.len(), 10);
    }

    #[test]
    fn a_range_past_the_end_is_turned_down_and_a_head_has_no_body() {
        let (_dir, path) = flac_file(1000);
        let server = server();
        let hosted = server.serve(&path).unwrap();
        let (head, _) = fetch(&hosted, "GET", None, "Range: bytes=2000-\r\n");
        assert!(head.starts_with("HTTP/1.1 416"), "{head}");
        assert!(head.contains("Content-Range: bytes */1000"), "{head}");
        let (head, body) = fetch(&hosted, "HEAD", None, "");
        assert!(head.contains("Content-Length: 1000"), "{head}");
        assert!(body.is_empty());
    }

    #[test]
    fn nothing_but_the_path_that_was_handed_out_is_served() {
        let (_dir, path) = flac_file(100);
        let server = server();
        let hosted = server.serve(&path).unwrap();
        let (head, _) = fetch(&hosted, "GET", Some("/"), "");
        assert!(head.starts_with("HTTP/1.1 404"), "{head}");
        let (head, _) = fetch(&hosted, "POST", None, "");
        assert!(head.starts_with("HTTP/1.1 400"), "{head}");
        let later = server.serve(&path).unwrap();
        assert_ne!(later.url, hosted.url);
        let (head, _) = fetch(&hosted, "GET", None, "");
        assert!(
            head.starts_with("HTTP/1.1 404"),
            "the file before is not served: {head}"
        );
    }
}
