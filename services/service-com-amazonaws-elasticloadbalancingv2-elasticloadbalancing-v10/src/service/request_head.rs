// SPDX-License-Identifier: CC0-1.0

//! The FRAMING of every request on one connection, as the CLIENT wrote it.
//!
//! The HTTP server a load balancer's own address speaks through NORMALISES a request's framing
//! while it parses it: a request carrying both a transfer coding and a length reaches the
//! handler carrying the coding alone — the reading RFC 7230 §3.3.3 gives it, "the
//! Transfer-Encoding overrides the Content-Length", which is also the reading that decides
//! where its body ends — so by the time anything behind the parser sees the request, the one
//! fact `routing.http.desync_mitigation_mode` is about has already been decided for it
//! ([`super::request_framing`]).
//!
//! So the bytes are read ONCE, as they arrive, by a wrapper around the connection
//! ([`sniffed`]): every request head is classified and the classification queued for the
//! request it belongs to, in the order the client sent them. HTTP/1.1 is sequential — one
//! request's head, then its body, then the next — so the queue the address takes from hands
//! each request its own framing, however many a client sends on one connection.
//!
//! Nothing is answered here and no byte is changed: this reads exactly the stream the server
//! reads, and the server's own reading of the body is what a forwarded request carries. A head
//! is parsed by `httparse`, the HTTP/1.x head parser that server is itself built on, rather
//! than by anything written here; what is written here is the FRAMING the head states — which
//! header says where the body ends — and the walk over the body to the next head.
//!
//! Where the framing of a request leaves the end of its body undeterminable — which is what an
//! ambiguous framing does — that request is still classified, and the connection is then no
//! longer tracked rather than guessed at: a later request on it carries no classification, and
//! the address that reads these answers such a request as it answers one it has no
//! classification for at all.

use std::collections::VecDeque;
use std::io;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use ::tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use super::request_framing::{framing, Framing};

/// The most bytes one request head may take before this reader stops tracking the connection:
/// a head longer than this is one the HTTP server itself refuses, so there is nothing behind
/// it to classify, and the bound is what stops a client's head growing what this reader holds.
const MAX_HEAD_BYTES: usize = 64 * 1024;

/// The most header fields of one head this reader reads. A head carrying more is one the HTTP
/// server itself refuses, and the bound is what stops a client's field count growing the
/// parse — far above the fields any client sends.
const MAX_HEAD_FIELDS: usize = 128;

/// How many requests' framings one connection may hold at once. A client that sends requests
/// faster than they are answered (pipelining) grows the queue by one per unanswered request, so
/// past this bound the connection stops being tracked rather than holding a classification per
/// request a client chose to send — far above the pipeline depth any client uses.
const MAX_QUEUED_FRAMINGS: usize = 64;

/// What ends a request head.
const HEAD_END: &[u8] = b"\r\n\r\n";

/// The two headers whose fields say where a body ends, read off the parsed head.
const TRANSFER_ENCODING_HEADER: &str = "transfer-encoding";
const CONTENT_LENGTH_HEADER: &str = "content-length";

/// The bytes of a chunk's own framing: the two that end its data, and the longest size or
/// trailer line this reader accumulates — a longer line is one no chunked body carries, and the
/// connection stops being tracked rather than growing what this holds.
const CHUNK_DATA_END: usize = 2;
const MAX_CHUNK_LINE_BYTES: usize = 1024;

/// The framings of the requests one connection has carried, in the order they arrived.
///
/// Written by the reader as each head arrives and read by whatever answers the request, so the
/// two never hold the same request twice: a classification is taken once.
#[derive(Debug, Default)]
pub(super) struct Framings {
    queued: Mutex<VecDeque<Framing>>,
}

impl Framings {
    /// The framing of the next request this connection carried, or `None` where this reader
    /// has none for it — a connection it no longer tracks.
    pub(super) fn next_request(&self) -> Option<Framing> {
        self.queued
            .lock()
            .ok()
            .and_then(|mut queued| queued.pop_front())
    }

    /// Record the framing of one request, for whatever answers it. `false` where the queue is
    /// already holding as many as it may ([`MAX_QUEUED_FRAMINGS`]), on which the connection
    /// stops being tracked.
    fn record(&self, framing: Framing) -> bool {
        let Ok(mut queued) = self.queued.lock() else {
            return false;
        };
        if queued.len() >= MAX_QUEUED_FRAMINGS {
            return false;
        }
        queued.push_back(framing);
        true
    }
}

/// Wrap `inner` so every request head read from it is classified, and hand back the queue those
/// classifications land in.
pub(super) fn sniffed<S>(inner: S) -> (Sniffed<S>, Arc<Framings>) {
    let framings = Arc::new(Framings::default());
    (
        Sniffed {
            inner,
            reading: Reading::Head(Vec::new()),
            framings: Arc::clone(&framings),
        },
        framings,
    )
}

/// One connection, with every request head read off it classified on the way past.
pub(super) struct Sniffed<S> {
    inner: S,
    reading: Reading,
    framings: Arc<Framings>,
}

/// Where in the stream of one connection this reader is.
#[derive(Debug)]
enum Reading {
    /// Accumulating a request head, up to [`MAX_HEAD_BYTES`].
    Head(Vec<u8>),
    /// Passing over a body whose length the head stated, with this many bytes of it left.
    Body(u64),
    /// Passing over a body the chunked transfer coding frames.
    Chunked(Chunked),
    /// This connection is no longer tracked: nothing further on it is classified.
    Stopped,
}

/// Where in a chunked body this reader is: the size line, the data of one chunk, the end of
/// that data, or the trailer section the last chunk opens.
#[derive(Debug)]
enum Chunked {
    Size(Vec<u8>),
    Data(u64),
    AfterData(usize),
    Trailer(Vec<u8>),
}

impl<S> Sniffed<S> {
    /// Read `bytes` as the continuation of this connection, classifying every request head
    /// they complete.
    fn feed(&mut self, mut bytes: &[u8]) {
        while !bytes.is_empty() {
            match &mut self.reading {
                Reading::Stopped => return,
                Reading::Head(head) => {
                    // A head ends at the first blank line, so the bytes up to and including it
                    // are the head and everything after it belongs to the body.
                    let (upto, complete) = up_to_head_end(head, bytes);
                    head.extend_from_slice(&bytes[..upto]);
                    bytes = &bytes[upto..];
                    if !complete {
                        if head.len() > MAX_HEAD_BYTES {
                            self.reading = Reading::Stopped;
                        }
                        return;
                    }
                    let Some((classified, next)) = judged(head) else {
                        self.reading = Reading::Stopped;
                        continue;
                    };
                    if !self.framings.record(classified) {
                        self.reading = Reading::Stopped;
                        continue;
                    }
                    self.reading = next;
                }
                Reading::Body(left) => {
                    let upto = (*left).min(bytes.len() as u64);
                    *left -= upto;
                    bytes = &bytes[usize::try_from(upto).unwrap_or(bytes.len())..];
                    if *left == 0 {
                        self.reading = Reading::Head(Vec::new());
                    }
                }
                Reading::Chunked(chunked) => {
                    let (upto, next) = chunked.feed(bytes);
                    bytes = &bytes[upto..];
                    if let Some(next) = next {
                        self.reading = next;
                    }
                }
            }
        }
    }
}

/// How many of `bytes` belong to the head being accumulated, and whether they complete it:
/// everything up to and including the blank line that ends it.
fn up_to_head_end(head: &[u8], bytes: &[u8]) -> (usize, bool) {
    // The terminator may straddle the two, so the search starts just far enough back in what is
    // already held for a split one to be found.
    let overlap = head.len().saturating_sub(HEAD_END.len() - 1);
    let mut joined = head[overlap..].to_vec();
    joined.extend_from_slice(bytes);
    match joined
        .windows(HEAD_END.len())
        .position(|window| window == HEAD_END)
    {
        Some(at) => {
            let end = at + HEAD_END.len();
            let already = head.len() - overlap;
            (end.saturating_sub(already), true)
        }
        None => (bytes.len(), false),
    }
}

impl Chunked {
    /// Read `bytes` as the continuation of a chunked body: how many of them belong to it, and
    /// the state the connection moves to where this body ended or stopped being readable.
    fn feed(&mut self, bytes: &[u8]) -> (usize, Option<Reading>) {
        match self {
            // The size of the next chunk, in hexadecimal, up to its own line end; the chunk of
            // size zero is the last one, and opens the trailer section.
            Chunked::Size(line) => {
                let (upto, complete) = up_to_line_end(bytes);
                line.extend_from_slice(&bytes[..upto]);
                if !complete {
                    if line.len() > MAX_CHUNK_LINE_BYTES {
                        return (upto, Some(Reading::Stopped));
                    }
                    return (upto, None);
                }
                trim_line_end(line);
                let Some(size) = chunk_size(line) else {
                    return (upto, Some(Reading::Stopped));
                };
                *self = match size {
                    0 => Chunked::Trailer(Vec::new()),
                    size => Chunked::Data(size),
                };
                (upto, None)
            }
            Chunked::Data(left) => {
                let upto = (*left).min(bytes.len() as u64);
                *left -= upto;
                if *left == 0 {
                    *self = Chunked::AfterData(0);
                }
                (usize::try_from(upto).unwrap_or(bytes.len()), None)
            }
            // The two bytes that end a chunk's data.
            Chunked::AfterData(seen) => {
                let upto = (CHUNK_DATA_END - *seen).min(bytes.len());
                *seen += upto;
                if *seen == CHUNK_DATA_END {
                    *self = Chunked::Size(Vec::new());
                }
                (upto, None)
            }
            // The trailer section: header lines until a blank one, which ends the body.
            Chunked::Trailer(line) => {
                let (upto, complete) = up_to_line_end(bytes);
                line.extend_from_slice(&bytes[..upto]);
                if !complete {
                    if line.len() > MAX_CHUNK_LINE_BYTES {
                        return (upto, Some(Reading::Stopped));
                    }
                    return (upto, None);
                }
                trim_line_end(line);
                let ended = line.is_empty();
                line.clear();
                (upto, ended.then(|| Reading::Head(Vec::new())))
            }
        }
    }
}

/// How many of `bytes` belong to the line being accumulated, and whether they complete it. The
/// bytes that END the line are part of the count and are trimmed off the line by
/// [`trim_line_end`], so a line end split across two reads is still one end.
fn up_to_line_end(bytes: &[u8]) -> (usize, bool) {
    match bytes.iter().position(|byte| *byte == b'\n') {
        Some(at) => (at + 1, true),
        None => (bytes.len(), false),
    }
}

/// Take the line end off a line that has just been completed.
fn trim_line_end(line: &mut Vec<u8>) {
    while line
        .last()
        .is_some_and(|byte| *byte == b'\n' || *byte == b'\r')
    {
        line.pop();
    }
}

/// The size one chunk line states, or `None` where it states no size at all: the hexadecimal
/// number at its start, before any chunk extension.
fn chunk_size(line: &[u8]) -> Option<u64> {
    let line = std::str::from_utf8(line).ok()?;
    let digits = line.split(';').next().unwrap_or_default().trim();
    if digits.is_empty() {
        return None;
    }
    u64::from_str_radix(digits, 16).ok()
}

/// How one request head reads, and where its body ends — or `None` where these bytes are no
/// request head, after which the connection is no longer tracked.
///
/// The head itself is parsed by `httparse`; what is read off the parse is the FRAMING its
/// fields state ([`framing`]) and, from the same fields, where the body ends — by the transfer
/// coding where one is present, which is the reading RFC 7230 §3.3.3 gives it and the one the
/// server behind this reader takes too.
fn judged(head: &[u8]) -> Option<(Framing, Reading)> {
    let mut fields = [::httparse::EMPTY_HEADER; MAX_HEAD_FIELDS];
    let mut request = ::httparse::Request::new(&mut fields);
    match request.parse(head) {
        Ok(::httparse::Status::Complete(_)) => {}
        _ => return None,
    }
    let stated: Vec<(&str, &str)> = request
        .headers
        .iter()
        .filter_map(|field| {
            std::str::from_utf8(field.value)
                .ok()
                .map(|value| (field.name, value.trim()))
        })
        .collect();
    let classified = framing(stated.iter().copied());
    let length = stated
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(CONTENT_LENGTH_HEADER))
        .and_then(|(_, value)| value.split(',').next())
        .and_then(|value| value.trim().parse::<u64>().ok());
    let transfer_encoded = stated
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case(TRANSFER_ENCODING_HEADER));
    let next = match classified {
        // The end of the body is exactly what an ambiguous framing leaves undeterminable, so
        // the request is classified and nothing further on the connection is.
        Framing::Ambiguous => Reading::Stopped,
        _ if transfer_encoded => Reading::Chunked(Chunked::Size(Vec::new())),
        _ => match length {
            Some(0) | None => Reading::Head(Vec::new()),
            Some(length) => Reading::Body(length),
        },
    };
    Some((classified, next))
}

impl<S: AsyncRead + Unpin> AsyncRead for Sniffed<S> {
    /// Every byte the server reads passes through here on its way to it, unchanged.
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let before = buf.filled().len();
        let sniffed = self.get_mut();
        let polled = Pin::new(&mut sniffed.inner).poll_read(cx, buf);
        if matches!(polled, Poll::Ready(Ok(()))) {
            let read = buf.filled()[before..].to_vec();
            sniffed.feed(&read);
        }
        polled
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for Sniffed<S> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().inner).poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    //! The reader over bytes alone. What a CLIENT is answered is pinned end to end in
    //! `tests/all/alb_desync_mitigation_and_forwarded_for.rs`.

    use super::*;

    /// Every framing a connection carrying `stream` is read as, fed in chunks of `chunk` bytes
    /// so a head, a line end or a body split across two reads is read as one.
    fn framings_of(stream: &str, chunk: usize) -> Vec<Framing> {
        let (mut sniffed, framings) = sniffed(::std::io::Cursor::new(Vec::<u8>::new()));
        for bytes in stream.as_bytes().chunks(chunk.max(1)) {
            sniffed.feed(bytes);
        }
        let mut read = Vec::new();
        while let Some(framing) = framings.next_request() {
            read.push(framing);
        }
        read
    }

    /// One row: the case, the bytes a client sends, and the framing of each of its requests.
    type Row<'a> = (&'a str, &'a str, Vec<Framing>);

    #[test]
    fn every_request_of_a_connection_is_classified_as_the_client_wrote_it() {
        // The rows span what one connection can carry: one request with no body, one with a
        // length-framed body, one with a chunked body (with and without a trailer), several in
        // a row, a head longer than the fields this reader reads, and the ambiguous framing
        // that ends the tracking of the connection it is on.
        let cases: &[Row<'_>] = &[
            (
                "one request with no body",
                "GET / HTTP/1.1\r\nHost: alb\r\n\r\n",
                vec![Framing::Compliant],
            ),
            (
                "two requests with no body",
                "GET /a HTTP/1.1\r\nHost: alb\r\n\r\nGET /b HTTP/1.1\r\nHost: alb\r\n\r\n",
                vec![Framing::Compliant, Framing::Compliant],
            ),
            (
                "a length-framed body, then another request",
                "POST /a HTTP/1.1\r\nContent-Length: 5\r\n\r\nhello\
                 POST /b HTTP/1.1\r\nContent-Length: 0\r\n\r\n",
                vec![Framing::Compliant, Framing::Compliant],
            ),
            (
                "a chunked body, then another request",
                "POST /a HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n0\r\n\r\n\
                 GET /b HTTP/1.1\r\nHost: alb\r\n\r\n",
                vec![Framing::Compliant, Framing::Compliant],
            ),
            (
                "a chunked body with a trailer, then another request",
                "POST /a HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n\
                 5\r\nhello\r\n0\r\nX-Checksum: 1\r\n\r\nGET /b HTTP/1.1\r\nHost: alb\r\n\r\n",
                vec![Framing::Compliant, Framing::Compliant],
            ),
            (
                "a chunked body whose size line carries an extension",
                "POST /a HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n\
                 5;name=value\r\nhello\r\n0\r\n\r\nGET /b HTTP/1.1\r\nHost: alb\r\n\r\n",
                vec![Framing::Compliant, Framing::Compliant],
            ),
            (
                "the same length written twice",
                "POST /a HTTP/1.1\r\nContent-Length: 2\r\nContent-Length: 2\r\n\r\nhi",
                vec![Framing::NotCompliant],
            ),
            (
                "a length beside a transfer coding, which ends the tracking",
                "POST /a HTTP/1.1\r\nContent-Length: 8\r\nTransfer-Encoding: chunked\r\n\r\n\
                 0\r\n\r\nGET /b HTTP/1.1\r\nHost: alb\r\n\r\n",
                vec![Framing::Ambiguous],
            ),
            (
                "a compliant request, then an ambiguous one on the same connection",
                "GET /a HTTP/1.1\r\nHost: alb\r\n\r\n\
                 POST /b HTTP/1.1\r\nContent-Length: 8\r\nTransfer-Encoding: chunked\r\n\r\n\
                 0\r\n\r\n",
                vec![Framing::Compliant, Framing::Ambiguous],
            ),
            (
                "bytes that are no request at all",
                "not a request line\r\n\r\n",
                Vec::new(),
            ),
        ];
        // Fed whole, and one byte at a time: the reading cannot depend on how the bytes
        // arrived.
        for chunk in [usize::MAX, 1, 7] {
            for (case, stream, expected) in cases {
                let read = framings_of(stream, chunk);
                assert_eq!(
                    read, *expected,
                    "case {case:?} in chunks of {chunk}: expected {expected:?}, got {read:?}"
                );
            }
        }
    }

    #[test]
    fn a_head_past_the_fields_this_reader_reads_ends_the_tracking() {
        // A head no parse this reader runs accepts leaves the connection untracked rather than
        // classified on a guess: the bound is this reader's own ([`MAX_HEAD_FIELDS`]).
        let mut stream = String::from("GET / HTTP/1.1\r\nHost: alb\r\n");
        for field in 0..=MAX_HEAD_FIELDS {
            stream.push_str(&format!("X-Field-{field}: value\r\n"));
        }
        stream.push_str("\r\n");
        let read = framings_of(&stream, usize::MAX);
        assert!(
            read.is_empty(),
            "case a head past the field bound: expected no classification, got {read:?}"
        );
    }
}
