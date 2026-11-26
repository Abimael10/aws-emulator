// SPDX-License-Identifier: CC0-1.0

//! The ONE reading of a data-plane connection whose ENCRYPTION is negotiated IN BAND, for
//! every service that opens a core-owned plaintext listener and has to serve both the
//! `https://`/`wss://` a client of AWS dials and the `http://`/`ws://` a client that does
//! not verify a self-signed certificate dials.
//!
//! The core lends a service a socket for a resource with a native protocol
//! ([`::emulator_server::Listeners::tcp_plaintext`]), which hands the handler the
//! connection's FIRST byte. That byte says what the client opened with: a TLS record is
//! handed to the core's own TLS server ([`::emulator_server::TlsUpgrader`], so the service
//! never touches a key or a certificate) and everything after it is read decrypted, while
//! an HTTP method is read as it stands. ONE announced address therefore serves both
//! schemes.
//!
//! Doing that needs the first byte REPLAYED to whoever reads the stream next — the TLS
//! server has to see byte 0, and so does an HTTP or WebSocket parser — which is what
//! [`Prefixed`] is for. The rule is here so the sniff and the replay are written once:
//! a fix to either is a fix for every plane.
//!
//! WHICH handshakes an address completes is part of that one reading
//! ([`negotiated_scheme_for_covered_names`]). A TLS server presents a certificate for the
//! server name the client asked for, so an address that holds no certificate for a name
//! refuses that name in the HANDSHAKE — a fatal `handshake_failure` alert and no session —
//! instead of completing it and answering in the protocol above. The name is read off the
//! client's hello before the core runs the handshake, and which names an address covers is
//! the address's own business: an address served under a certificate for every name it is
//! dialled by uses [`negotiated_scheme`], which is this same reading admitting all of them.

use ::std::pin::Pin;
use ::std::task::{Context, Poll};

use ::emulator_server::{ListenerConn, TlsUpgrader};
use ::tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt as _, ReadBuf};

/// The first byte of a TLS record, which is what a client that dialled `https://` or
/// `wss://` opens with: the `handshake` content type (22) of TLS 1.0 and later. A request
/// made in the clear begins with an HTTP method instead, so one byte tells the two apart.
pub const TLS_RECORD_FIRST_BYTE: u8 = 0x16;

/// The most bytes of a client's hello this reading collects while deciding whether to run
/// the handshake at all.
///
/// One TLS record carries at most 2^14 bytes and a hello may be fragmented across records,
/// so this allows several of them. A client that has sent this much without a readable hello
/// is one no server name can be read from, and its connection is dropped rather than served.
const MAX_HELLO_BYTES: usize = 4 * 16_384;

/// How many bytes are taken off the connection at a time while the hello is collected.
const HELLO_READ_CHUNK: usize = 4_096;

/// The `alert` content type of a TLS record (21, RFC 8446 §5.1), which is what a refused
/// handshake is answered with: the connection carries an alert and then nothing.
const ALERT_RECORD: u8 = 21;

/// The `fatal` alert level (2, RFC 8446 §6): the connection ends with the alert, and no
/// handshake and no application data follow it.
const FATAL: u8 = 2;

/// The `handshake_failure` alert description (40, RFC 8446 §6.2) — "indicates that the
/// sender was unable to negotiate an acceptable set of security parameters given the options
/// available" — which is what a server with no certificate for the requested name answers.
const HANDSHAKE_FAILURE: u8 = 40;

/// The record-layer version of the alert sent to a client whose hello was too short to carry
/// one: TLS 1.0's (3, 1), the version every later implementation still accepts at the record
/// layer.
const LEGACY_RECORD_VERSION: (u8, u8) = (3, 1);

/// The connection with encryption negotiated: handed to the core's TLS server when the
/// client opened with a TLS record, and read as it stands otherwise.
///
/// `upgrader` is the core's TLS server, as the listener's owner obtained it
/// (`engine.listeners().tls_upgrader()`); `None` means this emulator has none prepared, and
/// a client that dialled TLS is then dropped rather than answered in the clear.
///
/// `None` for a client that sent nothing, for a read that failed, and for a TLS handshake
/// this emulator could not complete — in each case there is nothing to serve on the
/// connection.
pub async fn negotiated(
    conn: ListenerConn,
    upgrader: Option<TlsUpgrader>,
) -> Option<ListenerConn> {
    negotiated_scheme(conn, upgrader)
        .await
        .map(|(conn, _encrypted)| conn)
}

/// [`negotiated`], reporting WHICH half of the address the client dialled: `true` when the
/// connection was opened with a TLS record and is now read decrypted, `false` when the
/// client sent its request in the clear.
///
/// One announced address serves both schemes, so a service that tells a client (or an origin
/// behind it) which scheme the client used reads it here — there is nothing else on the
/// connection that says so.
///
/// Every server name is admitted: this is [`negotiated_scheme_for_covered_names`] for an
/// address served under a certificate for whatever name a client dials it by. An address that
/// holds certificates for particular names only states that there instead.
pub async fn negotiated_scheme(
    conn: ListenerConn,
    upgrader: Option<TlsUpgrader>,
) -> Option<(ListenerConn, bool)> {
    negotiated_scheme_for_covered_names(conn, upgrader, |_asked_for| true).await
}

/// [`negotiated_scheme`] for an address that completes a handshake only for the server names
/// it presents a certificate for.
///
/// The same reading with ONE step in front of the handshake: when the client opened with a
/// TLS record, the server name its hello asks for is read and handed to `covers`. A name that
/// answers `false` — including a client that asked for no name at all — gets the fatal
/// `handshake_failure` alert and no session, so nothing of the protocol above is ever read on
/// that connection. A request sent in the CLEAR is no handshake, so no name is asked for and
/// none is checked.
///
/// `None` for a client that sent nothing, for a read that failed, for a hello this reading
/// could not read, for a handshake it refused, and for one the core could not complete: in
/// each case there is nothing to serve on the connection.
pub async fn negotiated_scheme_for_covered_names<Covers>(
    mut conn: ListenerConn,
    upgrader: Option<TlsUpgrader>,
    covers: Covers,
) -> Option<(ListenerConn, bool)>
where
    Covers: FnOnce(Option<&str>) -> bool,
{
    let mut first = [0u8; 1];
    match conn.read(&mut first).await {
        Ok(1) => {}
        _ => return None,
    }
    let mut buffered = first.to_vec();
    if first[0] != TLS_RECORD_FIRST_BYTE {
        return Some((Box::new(Prefixed::new(buffered, conn)), false));
    }
    // ONE parser for the whole hello, fed each chunk as it arrives: a hello fragmented across
    // records is read once rather than re-read from the start per chunk.
    let mut acceptor = ::rustls::server::Acceptor::default();
    let mut fed = 0usize;
    let server_name = loop {
        let mut arrived: &[u8] = &buffered[fed..];
        let asked = asked_for(&mut acceptor, &mut arrived);
        // Only what the parser TOOK counts as fed: bytes it declined are offered again on the
        // next round rather than dropped.
        fed = buffered.len() - arrived.len();
        match asked {
            AskedFor::Name(name) => break name,
            AskedFor::Unreadable => return None,
            AskedFor::More => {}
        }
        if buffered.len() >= MAX_HELLO_BYTES {
            return None;
        }
        let mut chunk = [0u8; HELLO_READ_CHUNK];
        match conn.read(&mut chunk).await {
            Ok(0) | Err(_) => return None,
            Ok(read) => buffered.extend_from_slice(&chunk[..read]),
        }
    };
    if !covers(server_name.as_deref()) {
        refuse_the_handshake(&mut conn, &buffered).await;
        return None;
    }
    let upgrader = upgrader?;
    // Every byte read while deciding is replayed, so the core's TLS server reads the hello as
    // the client sent it. ALPN `None`: a client of such a plane offers `http/1.1` or nothing,
    // and what follows is HTTP/1.1 either way.
    let prefixed: ListenerConn = Box::new(Prefixed::new(buffered, conn));
    match upgrader.accept(prefixed, None).await {
        Ok((upgraded, _negotiated)) => Some((upgraded, true)),
        Err(error) => {
            ::tracing::debug!(%error, "a TLS handshake on a data-plane listener was not completed");
            None
        }
    }
}

/// What reading the bytes collected so far as a client's hello answered.
enum AskedFor {
    /// The hello is complete: the server name it asked for, `None` when it asked for none.
    Name(Option<String>),
    /// The hello is not all here yet, so more of the connection has to be read.
    More,
    /// These bytes are no hello at all: there is nothing to serve on the connection.
    Unreadable,
}

/// The server name the hello `acceptor` is reading asks for, after as much of `unread` as the
/// parser takes has been handed to it — read by the TLS library every handshake in this tree is
/// run with rather than by walking the record's bytes here, so the spellings and the
/// fragmentations covered are that library's own.
///
/// `acceptor` holds what has been read of the hello so far, so a caller feeds it each chunk as
/// it arrives and asks again. `unread` is advanced past exactly the bytes the parser took, so a
/// caller offers what is left again rather than losing it.
///
/// Nothing of the handshake is run: the hello is parsed, the name is read out of it, and the
/// parser is dropped. The handshake, with the key and the certificate it needs, stays the
/// core's.
fn asked_for(acceptor: &mut ::rustls::server::Acceptor, unread: &mut &[u8]) -> AskedFor {
    while !unread.is_empty() {
        match acceptor.read_tls(unread) {
            Ok(0) => break,
            Ok(_) => {}
            Err(_) => return AskedFor::Unreadable,
        }
    }
    match acceptor.accept() {
        Ok(Some(accepted)) => AskedFor::Name(
            accepted
                .client_hello()
                .server_name()
                .map(::std::string::ToString::to_string),
        ),
        Ok(None) => AskedFor::More,
        Err(_) => AskedFor::Unreadable,
    }
}

/// Refuse the handshake `hello` opened: the fatal `handshake_failure` alert a TLS server
/// sends when it holds no certificate for the name that hello asked for, and nothing else.
///
/// The one alert record is written here rather than taken from the library, because WHICH
/// alert a client sees is the point: a server with no certificate for the requested name
/// answers `handshake_failure`, while a library that is handed a configuration resolving to
/// no certificate picks its own description for the handshake it was running. Nothing else
/// moves to this side: no key, no certificate and no handshake message — those stay the
/// core's.
///
/// The alert is framed at the record-layer version the client opened with (the second and
/// third byte of its first record), so the refusal is framed as that client frames its own
/// records. The connection is then shut down, which is what makes a client see a failed
/// handshake rather than a served session.
async fn refuse_the_handshake(conn: &mut ListenerConn, hello: &[u8]) {
    let (major, minor) = match hello {
        [_content_type, major, minor, ..] => (*major, *minor),
        _ => LEGACY_RECORD_VERSION,
    };
    // ONE alert record: the content type, the record version, the two-byte length of the body
    // that follows, and that body — an alert's level and its description (RFC 8446 §5.1, §6).
    let alert = [
        ALERT_RECORD,
        major,
        minor,
        0,
        2,
        FATAL,
        HANDSHAKE_FAILURE,
    ];
    let _ = conn.write_all(&alert).await;
    let _ = conn.flush().await;
    let _ = conn.shutdown().await;
}

/// A connection whose first bytes have already been read: they are served from the prefix
/// before the socket itself is read again, so a reader that has to see byte 0 — the core's
/// TLS server, an HTTP parser, a WebSocket handshake — reads the stream as the client sent
/// it.
pub struct Prefixed<S> {
    /// The bytes already taken off the socket, to be served first.
    prefix: Vec<u8>,
    /// How many of them have been served.
    read: usize,
    /// The socket itself.
    inner: S,
}

impl<S> Prefixed<S> {
    /// A stream that serves `prefix` before reading `inner`.
    #[must_use]
    pub fn new(prefix: Vec<u8>, inner: S) -> Self {
        Self {
            prefix,
            read: 0,
            inner,
        }
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for Prefixed<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        if self.read < self.prefix.len() {
            let remaining = &self.prefix[self.read..];
            let taken = remaining.len().min(buffer.remaining());
            buffer.put_slice(&remaining[..taken]);
            self.read += taken;
            return Poll::Ready(Ok(()));
        }
        Pin::new(&mut self.inner).poll_read(context, buffer)
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for Prefixed<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(context, buffer)
    }

    fn poll_flush(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(context)
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(context)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_prefixed_stream_serves_the_bytes_already_read_before_the_socket() {
        // The rows span what the replay generalises over: no prefix at all, one byte (the
        // sniffed one), and a prefix longer than the reader's first read.
        let cases: &[(&str, &[u8], &[u8])] = &[
            ("no prefix", b"", b"GET / HTTP/1.1"),
            ("the one sniffed byte", b"G", b"ET / HTTP/1.1"),
            ("several bytes", b"GET ", b"/ HTTP/1.1"),
        ];
        for (case, prefix, rest) in cases {
            let mut stream = Prefixed::new(prefix.to_vec(), &rest[..]);
            let mut read = Vec::new();
            stream
                .read_to_end(&mut read)
                .await
                .unwrap_or_else(|error| panic!("case {case:?}: the stream must be read: {error}"));
            let mut expected = prefix.to_vec();
            expected.extend_from_slice(rest);
            assert_eq!(
                read, expected,
                "case {case:?}: expected the prefix then the socket, got {read:?}"
            );
        }
    }

    /// A hello for `server_name`, as a stock TLS client sends one: produced by the library
    /// that writes the client half of a handshake, so what is parsed below is a real hello
    /// rather than bytes written here.
    ///
    /// The client is never answered — one hello is written and the connection is dropped — so
    /// it is built over an empty trust anchor set: nothing it would verify ever arrives.
    fn client_hello(server_name: &str) -> Vec<u8> {
        let config = ::rustls::ClientConfig::builder_with_provider(
            crate::acm_certificate_termination::provider(),
        )
        .with_safe_default_protocol_versions()
        .expect("the library's own protocol versions")
        .with_root_certificates(::rustls::RootCertStore::empty())
        .with_no_client_auth();
        let name = ::rustls::pki_types::ServerName::try_from(server_name.to_string())
            .expect("a server name");
        let mut connection = ::rustls::ClientConnection::new(::std::sync::Arc::new(config), name)
            .expect("a client connection");
        let mut written: Vec<u8> = Vec::new();
        connection
            .write_tls(&mut written)
            .expect("the hello is written");
        written
    }

    /// What one whole hello asks for, read the way the connection above reads one: a fresh
    /// parser fed those bytes.
    fn asked_for_whole(hello: &[u8]) -> AskedFor {
        asked_for(
            &mut ::rustls::server::Acceptor::default(),
            &mut &hello[..],
        )
    }

    #[test]
    fn the_name_a_hello_asks_for_is_read_off_it() {
        // The rows differ along the FORM of the name a client dials with: a short label, a
        // deep one, one carrying digits and hyphens, and an upper-case spelling (DNS names
        // are case-insensitive, so a client's own spelling is what arrives).
        let names = [
            "example.test",
            "a.very.deep.tenant.domain.example.test",
            "tenant-01.example.test",
            "Tenant.Example.Test",
        ];
        for name in names {
            let hello = client_hello(name);
            match asked_for_whole(&hello) {
                AskedFor::Name(Some(asked)) => assert!(
                    asked.eq_ignore_ascii_case(name),
                    "case {name:?}: expected the name the client asked for, got {asked:?}"
                ),
                AskedFor::Name(None) => {
                    panic!("case {name:?}: expected a server name, got a hello asking for none")
                }
                AskedFor::More => panic!("case {name:?}: expected the whole hello to be readable"),
                AskedFor::Unreadable => panic!("case {name:?}: expected a readable hello"),
            }
        }
    }

    #[test]
    fn a_client_dialling_an_address_asks_for_no_name() {
        // A client that dials an IP literal sends no server name at all — which is what a
        // viewer handed an address rather than a name does — and a hello that is not all here
        // yet is neither a name nor a refusal.
        let hello = client_hello("127.0.0.1");
        match asked_for_whole(&hello) {
            AskedFor::Name(asked) => assert!(
                asked.is_none(),
                "case an address rather than a name: expected no server name, got {asked:?}"
            ),
            AskedFor::More => panic!("case an address rather than a name: expected a whole hello"),
            AskedFor::Unreadable => {
                panic!("case an address rather than a name: expected a readable hello")
            }
        }
        for prefix in [1usize, 5, 20] {
            let truncated = &hello[..prefix.min(hello.len())];
            assert!(
                matches!(asked_for_whole(truncated), AskedFor::More),
                "case {prefix} bytes of a hello: expected more of the connection to be read"
            );
        }
        // The same hello fed CHUNK BY CHUNK to one parser, as a fragmented hello arrives:
        // every chunk but the last answers "read more", and the name is read once it is whole.
        let mut acceptor = ::rustls::server::Acceptor::default();
        let mut asked = AskedFor::More;
        for chunk in hello.chunks(7) {
            asked = asked_for(&mut acceptor, &mut &chunk[..]);
            if matches!(asked, AskedFor::Name(_)) {
                break;
            }
            assert!(
                matches!(asked, AskedFor::More),
                "case a hello arriving in chunks: expected more of the connection to be read"
            );
        }
        assert!(
            matches!(asked, AskedFor::Name(None)),
            "case a hello arriving in chunks: expected the whole hello to be read, asking for \
             no name"
        );
    }

    #[test]
    fn bytes_that_are_no_hello_are_not_read_as_one() {
        // A record that claims to be a handshake and carries something else is no hello: the
        // connection is dropped rather than served.
        let not_a_hello = [TLS_RECORD_FIRST_BYTE, 3, 1, 0, 4, 0xff, 0xff, 0xff, 0xff];
        assert!(
            matches!(asked_for_whole(&not_a_hello), AskedFor::Unreadable),
            "case a handshake record carrying no hello: expected it to be unreadable"
        );
    }
}
