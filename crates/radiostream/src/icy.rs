//! Shoutcast's older servers answer a request with `ICY 200 OK` where HTTP
//! has `HTTP/1.1 200 OK`, which the HTTP client refuses. The reply is read
//! here on a plain socket instead, and its body is a stream like any other.

use std::io;

use reqwest::Url;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;

use super::{CONNECT_TIMEOUT, Error, READ_TIMEOUT, Result};

const HEAD_LIMIT: usize = 8 << 10;
const READ_SIZE: usize = 16 << 10;
const ICY_PREFIX: &str = "ICY ";
const DEFAULT_PORT: u16 = 80;

/// An open ICY reply, headers read.
pub(crate) struct Connection {
    socket: TcpStream,
    /// Body bytes that came in with the headers.
    unread: Vec<u8>,
}

/// Asks for `url` on a plain connection. `None` if the server doesn't answer
/// with an ICY status line.
pub(crate) async fn get(url: &Url, user_agent: &str) -> Result<Option<Connection>> {
    let host = url
        .host_str()
        .ok_or_else(|| Error::Address("it has no host".into()))?
        .trim_matches(['[', ']']);
    let port = url.port().unwrap_or(DEFAULT_PORT);
    let mut socket = timeout(CONNECT_TIMEOUT, TcpStream::connect((host, port)))
        .await
        .map_err(timed_out)??;
    // HTTP/1.0, which the servers that answer in ICY are written for.
    let query = url.query().map(|q| format!("?{q}")).unwrap_or_default();
    let request = format!(
        "GET {}{query} HTTP/1.0\r\nHost: {}\r\nUser-Agent: {user_agent}\r\n\r\n",
        url.path(),
        url.host_str().unwrap_or_default(),
    );
    socket.write_all(request.as_bytes()).await?;

    let mut head = Vec::new();
    let body_at = loop {
        if let Some(end) = head_end(&head) {
            break end;
        }
        if head.len() > HEAD_LIMIT || read_into(&mut socket, &mut head).await? == 0 {
            return Ok(None);
        }
    };
    let text = String::from_utf8_lossy(&head[..body_at]);
    let Some(status) = text
        .lines()
        .next()
        .and_then(|line| line.strip_prefix(ICY_PREFIX))
    else {
        return Ok(None);
    };
    match status.split(' ').next().map(str::parse::<u16>) {
        Some(Ok(200)) => Ok(Some(Connection {
            socket,
            unread: head.split_off(body_at),
        })),
        Some(Ok(code)) => Err(Error::Status(code)),
        _ => Ok(None),
    }
}

impl Connection {
    /// The next bytes of the body; none once the server has closed.
    pub(crate) async fn chunk(&mut self) -> Result<Option<Vec<u8>>> {
        if !self.unread.is_empty() {
            return Ok(Some(std::mem::take(&mut self.unread)));
        }
        let mut bytes = Vec::new();
        Ok((read_into(&mut self.socket, &mut bytes).await? > 0).then_some(bytes))
    }
}

/// Where the body starts: after the blank line that ends the headers.
fn head_end(head: &[u8]) -> Option<usize> {
    [b"\r\n\r\n".as_slice(), b"\n\n"]
        .iter()
        .filter_map(|end| {
            head.windows(end.len())
                .position(|w| w == *end)
                .map(|at| at + end.len())
        })
        .min()
}

/// Appends what the socket gives next to `bytes`, and how much that was.
async fn read_into(socket: &mut TcpStream, bytes: &mut Vec<u8>) -> Result<usize> {
    let mut buf = vec![0; READ_SIZE];
    let read = timeout(READ_TIMEOUT, socket.read(&mut buf))
        .await
        .map_err(timed_out)??;
    bytes.extend_from_slice(&buf[..read]);
    Ok(read)
}

fn timed_out(_: tokio::time::error::Elapsed) -> io::Error {
    io::ErrorKind::TimedOut.into()
}
