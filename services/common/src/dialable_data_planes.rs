// SPDX-License-Identifier: CC0-1.0

//! The data planes an IN-PROCESS client of this emulator may open a connection to, and the
//! `host:port` each of them answers at.
//!
//! A resource whose data plane speaks its own wire protocol — a database, a cache, a broker —
//! is served by a protocol HANDLER, which the core runs for every connection accepted on the
//! listener it opened for that resource (`engine.listeners()`); what the owning service
//! publishes for a client to dial is that listener's `host:port`
//! ([`crate::data_plane_addressing::listener_authority`]). A client that runs INSIDE this
//! process — the code of a Lambda function the emulator itself runs — then reads that address
//! out of the control plane and opens a connection to it. This register is how such a client
//! reaches the plane WITHOUT a socket: the service that opened the listener records, under the
//! address it published, the plane itself, and a connection opened here is a pipe whose other
//! end that plane's own handler is serving — in this process, exactly as a request a running
//! handler sends over HTTP is carried by the in-process callback rather than by a socket.
//!
//! So the whole of what an in-process client can reach is what a service put here, and nothing
//! it opens leaves this process. An address absent from these records is one no plane of this
//! emulator answers at, and a client opening a connection to it is answered the way a host
//! answers one where nothing is listening.
//!
//! It is the NATIVE-wire counterpart of [`crate::published_data_planes`], which records which
//! service answers HTTP at an address so an in-process request can be signed and routed to it.
//! The two are kept apart because they answer different questions with different values: a
//! Postgres listener answers no HTTP request and has no credential scope, and what is needed
//! for it is the handler that speaks Postgres.
//!
//! Engine-scoped, like every other store two services on either side of a hand-off share: the
//! records live in the SAME shared engine as the services' own, so a plane one service opened
//! is the one another resolves, and two engines in one process share nothing. An engine this
//! emulator did not build vends no store, and then an address resolves to nothing — the
//! position every address was in before a service recorded one.
//!
//! LOCK ORDER: these records are taken LAST. A publisher holds its own service's store while
//! it records a plane ([`DialablePlanes::opening`] from a create, and the drop of the record it
//! answers with from the drop of the handle that kept the port open), and nothing here takes
//! any other lock, so this store is never held while another is taken. A connection is opened
//! with no lock held at all: [`DialablePlanes::dialable`] clones the plane out of the store
//! first.

use std::collections::BTreeMap;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;

use ::emulator_server::{ListenerConn, NativeHandler, TlsUpgrader};

/// How much one in-process connection buffers in each direction before the writer waits for
/// the reader.
///
/// A pipe has no kernel socket buffer behind it, so this is that buffer: large enough that a
/// protocol's handshake and a result set travel without the two ends having to interleave
/// perfectly, small enough that one connection holds a bounded amount of this process's
/// memory. The CLIENT's own bounds on how much it may carry are the client's
/// (`service_com_amazonaws_lambda_awsgirapiservice::state::execution::guest_socket`).
const CONNECTION_BUFFER_BYTES: usize = 64 * 1024;

/// A data plane an in-process client may open a connection to.
///
/// Implemented by [`served_in_process`] for a plane served by a core listener's own protocol
/// handler, which is every plane this emulator opens.
pub trait DialablePlane: Send + Sync + 'static {
    /// Open ONE connection to this plane: the stream answered is the CLIENT end of a pipe
    /// whose other end this plane's own handler is serving, so the bytes written to it are
    /// the bytes that handler reads, and the bytes it writes are the ones read back.
    ///
    /// `from` is the address the CLIENT reaches this plane from, which the plane's own handler
    /// reads back with [`connected_from`] — the address a protocol that states who its client
    /// is (an engine's own log of a command, a connection list) names. `None` for a client
    /// with no such address, and then the handler is told nothing about where it is.
    fn dial(
        &self,
        from: Option<SocketAddr>,
    ) -> Pin<Box<dyn Future<Output = std::io::Result<ListenerConn>> + Send>>;
}

::tokio::task_local! {
    /// The address the client of the connection being served reaches this plane FROM, set
    /// around the handler of an in-process connection ([`DialablePlane::dial`]).
    ///
    /// Per CONNECTION, which is what a task serves one of: the handler of a connection reads
    /// its own client's address and no other's.
    static CONNECTED_FROM: Option<SocketAddr>;
}

/// The address the client of the connection being served reaches this plane from, as the
/// client stated it when it opened the connection ([`DialablePlane::dial`]).
///
/// `None` where nothing stated one: a connection the CORE accepted on the resource's own
/// socket, whose peer the core does not hand its handlers, and a client that carries no
/// address of its own.
#[must_use]
pub fn connected_from() -> Option<SocketAddr> {
    CONNECTED_FROM.try_with(|from| *from).ok().flatten()
}

/// A plane served by the protocol handler of a core listener: the ONE kind this emulator has.
struct ServedInProcess<H> {
    /// The handler the core runs for every connection accepted on the listener, shared with
    /// the listener itself — so a connection opened here is served by the very handler that
    /// serves a client dialling the published address, over the same resource state.
    handler: Arc<H>,
    /// The core's TLS server, for a listener the core terminates TLS on
    /// (`engine.listeners().tcp`): the plane's handler is handed the DECRYPTED stream there,
    /// so a connection opened here has the same handshake run on its own end. `None` for a
    /// plaintext listener (`tcp_plaintext`), whose handler reads byte 0 itself — including a
    /// protocol that negotiates encryption in band, which it upgrades with the same core
    /// upgrader it would on a socket.
    tls: Option<TlsUpgrader>,
}

impl<H: NativeHandler> DialablePlane for ServedInProcess<H> {
    fn dial(
        &self,
        from: Option<SocketAddr>,
    ) -> Pin<Box<dyn Future<Output = std::io::Result<ListenerConn>> + Send>> {
        let (client, served) = ::tokio::io::duplex(CONNECTION_BUFFER_BYTES);
        let handler = Arc::clone(&self.handler);
        let tls = self.tls.clone();
        Box::pin(async move {
            // The plane's end is served on a task of its own, exactly as the core serves one
            // accepted connection: the client's reads and writes are answered while it holds
            // the stream, and dropping the stream ends the task's own. The task carries the
            // address its client opened the connection from, so the handler can state who it
            // is serving.
            ::tokio::spawn(CONNECTED_FROM.scope(from, async move {
                let served: ListenerConn = Box::new(served);
                let served = match tls {
                    Some(upgrader) => match upgrader.accept(served, None).await {
                        Ok((decrypted, _alpn)) => decrypted,
                        // A handshake the client did not complete is a connection the plane
                        // never saw, which is what the core does with one too.
                        Err(_unfinished) => return,
                    },
                    None => served,
                };
                handler.serve(served).await;
            }));
            Ok(Box::new(client) as ListenerConn)
        })
    }
}

/// The plane a core listener's `handler` serves, for the register below.
///
/// `tls` is the core's TLS server where the listener was opened as a TLS one
/// (`engine.listeners().tcp`), and `None` where it was opened as a plaintext one
/// (`tcp_plaintext`) — so a connection opened in process meets the same byte 0 a client
/// dialling the published address meets.
#[must_use]
pub fn served_in_process<H: NativeHandler>(
    handler: Arc<H>,
    tls: Option<TlsUpgrader>,
) -> Arc<dyn DialablePlane> {
    Arc::new(ServedInProcess { handler, tls })
}

/// ONE protocol handler served at BOTH ends of a resource's data plane: the listener the core
/// accepts connections on, and the connections an in-process client of this emulator opens
/// ([`served_in_process`]).
///
/// The core's listener takes its handler by value, and the register holds one too, so a
/// service that serves both hands the listener this — the same handler behind a shared
/// reference — rather than building a second one over the same resource state.
pub struct SharedHandler<H>(pub Arc<H>);

impl<H: NativeHandler> NativeHandler for SharedHandler<H> {
    fn serve(&self, conn: ListenerConn) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        self.0.serve(conn)
    }
}

/// The planes this emulator has opened a resource's own listener for, by the address each
/// answers at.
///
/// Held once per engine. The key is the `host:port` authority the owning service published,
/// lower-cased so a client that wrote an IPv6 literal or a name in another case resolves the
/// same record.
#[derive(Default)]
pub struct DialableDataPlanes {
    open: BTreeMap<String, Open>,
}

/// Written by hand rather than derived: a plane is a live handler and carries no `Debug`.
impl std::fmt::Debug for DialableDataPlanes {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DialableDataPlanes")
            .field("open", &self.open.len())
            .finish()
    }
}

/// One open plane: what answers there, and the plane itself.
struct Open {
    /// The SigV4 signing name of the service that opened the listener, which is what a
    /// diagnosis of a connection has to say.
    served_by: String,
    /// What a connection to this address is served by.
    plane: Arc<dyn DialablePlane>,
}

/// A handle on one engine's [`DialableDataPlanes`].
///
/// Clone-cheap (an `Arc` inside) and `'static`, so a service can keep one beside the listener
/// handle it opened and a seam that outlives the request that built it — a running function's
/// own — can hold one too.
#[derive(Clone)]
pub struct DialablePlanes(::engine::ServiceState<DialableDataPlanes>);

/// Written by hand rather than derived: the state handle inside carries no `Debug`, and what
/// is diagnosable about this handle is how many addresses it holds — never a caller's address
/// list in a log line.
impl std::fmt::Debug for DialablePlanes {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DialablePlanes")
            .field("open", &self.0.lock().open.len())
            .finish()
    }
}

/// What one address resolved to: the authority as the register holds it, what answers there,
/// and the plane a connection is opened on.
pub struct Dialed {
    /// The authority the record is held under, which is the address the owning service
    /// published.
    pub authority: String,
    /// The signing name of the service that opened the listener.
    pub served_by: String,
    /// The plane itself.
    pub plane: Arc<dyn DialablePlane>,
}

/// Written by hand rather than derived: the plane carries no `Debug`.
impl std::fmt::Debug for Dialed {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Dialed")
            .field("authority", &self.authority)
            .field("served_by", &self.served_by)
            .finish()
    }
}

impl DialablePlanes {
    /// The records of the SAME shared engine the services' own stores live in, or `None` for
    /// an engine that vends none.
    #[must_use]
    pub fn of<Eng: Send + Sync + 'static>(engine: &Eng) -> Option<Self> {
        (engine as &dyn ::std::any::Any)
            .downcast_ref::<::engine::InMemoryEngine>()
            .map(|engine| Self(engine.state::<DialableDataPlanes>()))
    }

    /// Record `plane` as the plane answering at each address of `authorities` — the
    /// `host:port` the core reports for the listeners just opened for it
    /// ([`crate::data_plane_addressing::listener_authority`]), which is what the control plane
    /// publishes and therefore what a client opens a connection to. Answer with the record
    /// that holds them: dropping it withdraws them, exactly as dropping the listener closes
    /// the port.
    ///
    /// Recording an authority again replaces the record, which is what a port the operating
    /// system has handed out a second time needs.
    #[must_use]
    pub fn opening<I, S>(
        &self,
        authorities: I,
        served_by: &str,
        plane: &Arc<dyn DialablePlane>,
    ) -> OpenedPlanes
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let authorities: Vec<String> = authorities.into_iter().map(Into::into).collect();
        let mut held = self.0.lock();
        for authority in &authorities {
            held.open.insert(
                key(authority),
                Open {
                    served_by: served_by.to_string(),
                    plane: Arc::clone(plane),
                },
            );
        }
        drop(held);
        OpenedPlanes {
            planes: self.clone(),
            authorities,
        }
    }

    /// Forget `authority`: the plane behind it is gone, so nothing answers there any more and
    /// a client opening a connection to it must be answered as one opened where nothing is
    /// listening.
    pub fn closed(&self, authority: &str) {
        self.0.lock().open.remove(&key(authority));
    }

    /// What answers at `authority`, or `None` for an address this emulator has opened no
    /// plane at.
    #[must_use]
    pub fn served_by(&self, authority: &str) -> Option<String> {
        self.0
            .lock()
            .open
            .get(&key(authority))
            .map(|open| open.served_by.clone())
    }

    /// The plane to open a connection on for `host`:`port` as a client addressed it, or
    /// `None` when this emulator has opened no plane there.
    ///
    /// The address as written is resolved first. A client that reached one of this machine's
    /// own LOOPBACK spellings (`localhost`, `127.0.0.1`, `::1`) is resolved against the other
    /// spellings of the same interface too, because the listener was opened on one of them and
    /// all of them name it: the port is what picks the plane out, and an address on another
    /// host resolves only as itself.
    #[must_use]
    pub fn dialable(&self, host: &str, port: u16) -> Option<Dialed> {
        let asked = crate::data_plane_addressing::host_port_authority(host, port);
        let held = self.0.lock();
        let found = held.open.get_key_value(&key(&asked)).or_else(|| {
            if !::emulator_server::is_loopback_host(trimmed(host)) {
                return None;
            }
            held.open.iter().find(|(authority, _open)| {
                let (held_host, held_port) = split_authority(authority);
                held_port == Some(port) && ::emulator_server::is_loopback_host(trimmed(held_host))
            })
        });
        found.map(|(authority, open)| Dialed {
            authority: authority.clone(),
            served_by: open.served_by.clone(),
            plane: Arc::clone(&open.plane),
        })
    }

    /// How many addresses are open, for a caller that reports on the register itself.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.lock().open.len()
    }

    /// Whether no address is open at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// The record of a resource's OPEN planes, held for as long as its listeners are.
///
/// A service keeps one beside the listener handles themselves
/// ([`DialablePlanes::opening`]): dropping it withdraws the addresses, exactly as dropping the
/// listener closes the port, so a resource that is deleted leaves no address recorded as
/// answering. Nothing resolves to a plane that is gone.
pub struct OpenedPlanes {
    /// Where the records live.
    planes: DialablePlanes,
    /// The addresses this record holds open.
    authorities: Vec<String>,
}

/// Written by hand rather than derived: what is diagnosable here is how many addresses are
/// held, which is also all this type carries.
impl std::fmt::Debug for OpenedPlanes {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OpenedPlanes")
            .field("authorities", &self.authorities.len())
            .finish()
    }
}

impl Drop for OpenedPlanes {
    fn drop(&mut self) {
        for authority in &self.authorities {
            self.planes.closed(authority);
        }
    }
}

/// One authority as it is keyed: as the client addressed it, lower-cased — a `host:port`,
/// where the port is part of the key because one address per listener is the point.
fn key(authority: &str) -> String {
    authority.trim().to_ascii_lowercase()
}

/// The host and port one authority spells, with an IPv6 host left bracketed as it was
/// written: the port is the text after the LAST colon, which is what separates `[::1]:4000`'s
/// port from its address.
fn split_authority(authority: &str) -> (&str, Option<u16>) {
    match authority.rsplit_once(':') {
        Some((host, port)) => (host, port.parse().ok()),
        None => (authority, None),
    }
}

/// A host without the brackets an IPv6 literal is written in, which is the form an address
/// reading takes it in.
fn trimmed(host: &str) -> &str {
    host.trim_start_matches('[').trim_end_matches(']')
}

#[cfg(test)]
mod tests {
    use ::tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;

    /// A plane for the rows below: it answers every line it is sent with that line under a
    /// prefix, which is how an answer is told from an echo.
    struct Answering;

    const ANSWER_PREFIX: &[u8] = b"ANSWER ";

    impl NativeHandler for Answering {
        fn serve(
            &self,
            conn: ListenerConn,
        ) -> Pin<Box<dyn Future<Output = ()> + Send>> {
            Box::pin(async move {
                let mut conn = conn;
                let mut arriving = [0_u8; 256];
                loop {
                    let read = match conn.read(&mut arriving).await {
                        Ok(0) | Err(_) => return,
                        Ok(read) => read,
                    };
                    let mut answer = ANSWER_PREFIX.to_vec();
                    answer.extend_from_slice(&arriving[..read]);
                    if conn.write_all(&answer).await.is_err() {
                        return;
                    }
                }
            })
        }
    }

    /// RULE: a connection opened to an address a service recorded is served by that plane's
    /// own handler, in this process; an address no plane was recorded at resolves to nothing,
    /// and so does one whose record has been withdrawn.
    ///
    /// The rows span what the reading generalises over: the address shapes a core listener is
    /// published at (an IPv4 literal, a bracketed IPv6 one), the case a client writes one in,
    /// the other spellings of the SAME loopback interface, two planes of two services on one
    /// engine, and the addresses that resolve to nothing (another port of the same host, a
    /// routable host nothing was opened at).
    #[tokio::test]
    async fn a_connection_to_a_recorded_address_is_served_by_that_planes_handler() {
        let engine = ::engine::InMemoryEngine::new();
        let planes = DialablePlanes::of(&engine).expect("the engine vends the records");
        let plane = served_in_process(Arc::new(Answering), None);
        let held = planes.opening(["127.0.0.1:37333"], "rds", &plane);
        let elsewhere = planes.opening(["[::1]:40127", "LOCALHOST:8443"], "elasticache", &plane);
        let rows: &[(&str, &str, u16, Option<&str>)] = &[
            ("the endpoint as published", "127.0.0.1", 37333, Some("127.0.0.1:37333")),
            ("an IPv6 listener", "::1", 40127, Some("[::1]:40127")),
            ("an address recorded in another case", "localhost", 8443, Some("localhost:8443")),
            ("the loopback name for an IPv4 listener", "localhost", 37333, Some("127.0.0.1:37333")),
            ("the IPv4 literal for an IPv6 listener", "127.0.0.1", 40127, Some("[::1]:40127")),
            ("another port of the same host", "127.0.0.1", 37334, None),
            ("a routable host nothing was opened at", "10.0.0.1", 37333, None),
        ];
        for (case, host, port, expected) in rows {
            let dialed = planes.dialable(host, *port);
            let authority = dialed.as_ref().map(|dialed| dialed.authority.clone());
            assert_eq!(
                authority.as_deref(),
                *expected,
                "case {case:?}: expected {expected:?}, got {authority:?}"
            );
            let Some(dialed) = dialed else {
                continue;
            };
            // And the plane really is reached: the bytes written to the connection are the
            // bytes its handler read, and the answer is the one that handler wrote.
            let mut conn = dialed
                .plane
                .dial(None)
                .await
                .expect("the plane opens a connection");
            conn.write_all(b"ping").await.expect("the plane reads what is written");
            let mut answered = [0_u8; 64];
            let read = conn.read(&mut answered).await.expect("the plane answers");
            assert_eq!(
                &answered[..read],
                b"ANSWER ping",
                "case {case:?}: expected the plane's own answer, got {:?}",
                String::from_utf8_lossy(&answered[..read])
            );
        }
        let served = planes.served_by("127.0.0.1:37333");
        assert_eq!(
            served.as_deref(),
            Some("rds"),
            "case \"what answers at an open address\": expected the recording service, got \
             {served:?}"
        );

        // A handle built again over the same engine reads the same records, which is what lets
        // the service that opened a listener and the client that reaches it be in two crates.
        let read_again = DialablePlanes::of(&engine).expect("the engine vends the records");
        assert!(
            read_again.dialable("127.0.0.1", 37333).is_some(),
            "case \"a second handle on the same engine\": expected the recorded plane"
        );

        // Withdrawn with the listener: nothing answers there again.
        drop(held);
        let dialed = planes.dialable("127.0.0.1", 37333);
        assert!(
            dialed.is_none(),
            "case \"an address whose listener is closed\": expected nothing, got {dialed:?}"
        );
        assert_eq!(
            planes.len(),
            2,
            "expected the other service's addresses to stay open, got {planes:?}"
        );
        drop(elsewhere);
        assert!(
            planes.is_empty(),
            "expected no address to be left open, got {planes:?}"
        );
    }

    /// An engine this emulator did not build vends no records, so an address resolves to
    /// nothing rather than taking the caller down.
    #[test]
    fn an_engine_that_vends_no_records_opens_nothing() {
        struct Elsewhere;
        assert!(
            DialablePlanes::of(&Elsewhere).is_none(),
            "expected no records for an engine of another kind"
        );
    }

    /// RULE: the handler serving an in-process connection reads back the address its client
    /// opened that connection FROM, and reads none where the client stated none — which is
    /// what lets a plane state who it is serving.
    ///
    /// The rows span what the reading generalises over: an IPv4 client address, an IPv6 one,
    /// and a client that stated none.
    #[tokio::test]
    async fn a_plane_reads_back_the_address_its_client_dialled_from() {
        /// A plane that answers with the address its client was recorded as reaching it from.
        struct Stating;

        impl NativeHandler for Stating {
            fn serve(&self, conn: ListenerConn) -> Pin<Box<dyn Future<Output = ()> + Send>> {
                let stated = connected_from()
                    .map(|from| from.to_string())
                    .unwrap_or_else(|| "none".to_string());
                Box::pin(async move {
                    let mut conn = conn;
                    let _ = conn.write_all(stated.as_bytes()).await;
                })
            }
        }

        let plane = served_in_process(Arc::new(Stating), None);
        let rows: &[(&str, Option<&str>)] = &[
            ("an IPv4 client", Some("10.42.1.7:41234")),
            ("an IPv6 client", Some("[fd00::7]:41235")),
            ("a client that stated no address", None),
        ];
        for (case, from) in rows {
            let dialled = from.map(|from| {
                from.parse::<SocketAddr>()
                    .unwrap_or_else(|error| panic!("case {case:?}: {from} is an address: {error}"))
            });
            let mut conn = plane
                .dial(dialled)
                .await
                .unwrap_or_else(|error| panic!("case {case:?}: the plane opens one: {error}"));
            let mut stated = String::new();
            conn.read_to_string(&mut stated)
                .await
                .unwrap_or_else(|error| panic!("case {case:?}: the plane answers: {error}"));
            assert_eq!(
                stated,
                from.unwrap_or("none"),
                "case {case:?}: expected the plane to be told {from:?}, got {stated:?}"
            );
        }
    }
}
