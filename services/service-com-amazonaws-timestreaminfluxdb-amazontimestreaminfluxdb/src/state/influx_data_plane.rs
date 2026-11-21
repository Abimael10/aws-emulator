// SPDX-License-Identifier: CC0-1.0

//! The SOCKET one DB instance's InfluxDB v2 API is served on: the address a client dials to
//! read and write that instance's data.
//!
//! `GetDbInstance$endpoint` is "the endpoint used to connect to InfluxDB" and
//! `GetDbInstance$port` is "the port number on which InfluxDB accepts connections", so what
//! a client does with the pair is DIAL it and speak InfluxDB's own HTTP API there. A value
//! no socket answers on is therefore unusable to every such client, however well it is
//! formed: the host Timestream for InfluxDB's own zone would carry resolves nowhere on the
//! machine this emulator runs on.
//!
//! So each DB instance gets a socket of the EMULATOR's own, opened through the core
//! (`engine.listeners().tcp_plaintext(..)`, which owns the socket, the bind interface, the
//! port range, the connection caps and the idle timeout), and the instance reports that
//! socket's host as its `endpoint` and its port as its `port`. ONE socket per instance, so
//! the ADDRESS identifies the instance and the whole path of a request stays the client's —
//! which is what an InfluxDB client needs, because it joins absolute paths
//! (`/api/v2/write`) onto the address it was configured with.
//!
//! The socket is opened PLAINTEXT because InfluxDB answers HTTP on its port and nothing in
//! the published address says otherwise; the encryption is then negotiated in band
//! ([`::service_common::in_band_tls`]), so the same address serves a client that dialled
//! `https://` as well.
//!
//! The handle lives in
//! [`super::ComAmazonawsTimestreaminfluxdbAmazontimestreaminfluxdbState::db_instance_planes`]
//! under the instance's id, beside the records that say which instances exist: dropping it
//! closes the port, so a deleted instance stops answering. How many ports one store can
//! hold open is bounded by the resources a caller may create: one per DB instance, and a
//! store holds at most [`super::DB_INSTANCE_QUOTA`] of those.
//!
//! An emulator that can open no socket at all (a host driving it on a runtime with no IO
//! driver) opens none, and the instance keeps the host Timestream for InfluxDB's own zone
//! would carry ([`super::db_instance_endpoint`]) — so an instance is never left reporting
//! nothing at all.
//!
//! WHAT is answered there is [`v2_api`], out of the instance's own store
//! ([`time_series`]); this module owns only the TRANSPORT: it speaks HTTP/1.1 on the
//! connection the core handed it and hands each request to that surface.

use ::std::future::Future;
use ::std::pin::Pin;
use ::std::sync::{Arc, Mutex};

use ::bytes::Bytes;
use ::emulator_server::{listener_caps, ListenerConn, NativeHandler, TlsUpgrader};
use ::http_body_util::{Full, Limited};
use ::http_body_util::BodyExt as _;

/// The InfluxDB v2 HTTP API this socket serves.
pub mod v2_api;

/// One DB instance's own data: the admin user its plane authenticates and the points its
/// buckets hold.
pub mod time_series;

pub use time_series::{AdminCredential, SharedTimeSeries, TimeSeries, TimeSeriesSnapshot};

/// How long one request on this socket may take before the connection is given up on.
///
/// An EMULATOR bound: every answer is computed in this process out of the instance's own
/// store, so this only bounds a request whose serving does not return.
const REQUEST_DEADLINE: ::std::time::Duration = ::std::time::Duration::from_secs(30);

/// What the socket answers a request whose body could not be read inside the core's
/// per-connection byte ceiling.
const PAYLOAD_TOO_LARGE: u16 = 413;

/// And what it answers a request that did not finish inside [`REQUEST_DEADLINE`].
const GATEWAY_TIMEOUT: u16 = 504;

/// One DB instance's data-plane socket: the address it answers on, the store it answers
/// from, and the handle that keeps the port open.
pub struct InfluxDataPlane {
    /// The host a client dials this instance at — what the instance reports as its
    /// `endpoint`.
    host: String,
    /// The port it dials — what the instance reports as its `port`.
    port: i32,
    /// The instance's own data, shared with every connection the socket serves, so a point
    /// one connection wrote is a point the next one reads.
    series: SharedTimeSeries,
    /// Held only to keep the port open; the core owns the socket. Dropping it closes the
    /// port and severs what is in flight.
    _listener: ::emulator_server::ResourceListener,
}

impl InfluxDataPlane {
    /// The host a client dials this instance's InfluxDB API at.
    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }

    /// The port it dials.
    #[must_use]
    pub fn port(&self) -> i32 {
        self.port
    }

    /// The instance's own data, for a caller that reads it in process (a test asserting
    /// what a write stored).
    #[must_use]
    pub fn series(&self) -> &SharedTimeSeries {
        &self.series
    }
}

/// Written by hand rather than derived: a plane holds a core `ResourceListener`, which is a
/// live socket and carries no `Debug`. What is diagnosable is the address it answers on.
impl ::std::fmt::Debug for InfluxDataPlane {
    fn fmt(&self, formatter: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        formatter
            .debug_struct("InfluxDataPlane")
            .field("host", &self.host)
            .field("port", &self.port)
            .finish_non_exhaustive()
    }
}

/// Open the socket the DB instance `id` is served on, record it on the store, and report
/// the address it answers at onto the instance's record — so every reader of that instance
/// reports the address this emulator answers it at.
///
/// Called by the create once the record is stored and the store's lock is released, and by
/// every other operation that brings an instance into the store. IDEMPOTENT, and checked
/// BEFORE a socket is bound: an instance already served at an address keeps that address,
/// because a client may be dialling it. Nothing is opened for an instance that is already
/// gone, or when no socket can be opened at all — in which case the record keeps the host
/// Timestream for InfluxDB's own zone would carry.
pub async fn serve_db_instance_plane<Eng: Send + Sync + 'static>(
    engine: &Eng,
    state: &::engine::ServiceState<super::ComAmazonawsTimestreaminfluxdbAmazontimestreaminfluxdbState>,
    id: &str,
    credential: AdminCredential,
) {
    serve_over(engine, state, id, TimeSeries::new(credential)).await;
}

/// Bring the DB instance `id` onto a socket serving the data a backup captured — what a
/// restore does, because the restored resource holds the data and the admin user of the
/// resource the backup was taken from and the restore request carries no password.
///
/// An instance that is ALREADY served keeps its address, a client may be dialling it, and
/// the restored data is written into the store that address answers from; one that is not
/// (a resource this restore just minted) is published on a socket of its own.
pub async fn restore_db_instance_plane<Eng: Send + Sync + 'static>(
    engine: &Eng,
    state: &::engine::ServiceState<super::ComAmazonawsTimestreaminfluxdbAmazontimestreaminfluxdbState>,
    id: &str,
    data: TimeSeriesSnapshot,
) {
    let held = state
        .lock()
        .db_instance_planes
        .get(id)
        .map(|plane| Arc::clone(plane.series()));
    match held {
        Some(series) => {
            if let Ok(mut store) = series.lock() {
                *store = TimeSeries::restored(data);
            }
        }
        None => serve_over(engine, state, id, TimeSeries::restored(data)).await,
    }
}

/// Open the socket the DB instance `id` is served on over `store` — the data it answers
/// from — and publish its address onto the instance's record. The one spelling of that
/// walk, shared by the create and by a restore that mints a resource.
async fn serve_over<Eng: Send + Sync + 'static>(
    engine: &Eng,
    state: &::engine::ServiceState<super::ComAmazonawsTimestreaminfluxdbAmazontimestreaminfluxdbState>,
    id: &str,
    store: TimeSeries,
) {
    if state.lock().db_instance_planes.contains_key(id) {
        return;
    }
    let series: SharedTimeSeries = Arc::new(Mutex::new(store));
    let Some(plane) = open(engine, state, id, series).await else {
        return;
    };
    let (host, port) = (plane.host.clone(), plane.port);
    let mut store = state.lock();
    // Checked AGAIN under this guard, because the check above released it for the bind: a
    // second call that got there first published an address a client may already be
    // dialling, and REPLACING its record would close that port. The address stands, and the
    // socket this call opened closes as it returns.
    if store.db_instance_planes.contains_key(id) {
        return;
    }
    // The instance may have been deleted while the port was being bound; its address is
    // then nothing to publish, and the socket closes as this returns.
    let Some(record) = store.db_instances.get_mut(id) else {
        return;
    };
    record.served_endpoint = Some(host);
    record.served_port = Some(port);
    store.db_instance_planes.insert(id.to_string(), plane);
}

/// Move the DB instance `id` onto a socket of its own again, keeping the data it holds —
/// what an update that names a new `port` does, because the port a client dials it on is
/// the one thing such a request changes.
///
/// The NEW socket is bound while the old one is still open, so the two can never be the
/// same port; the old handle is dropped as the new one takes its place, which closes the
/// port the instance was published on. The instance's data moves with it: the same
/// [`TimeSeries`] handle serves the new socket, so a point written before the change is a
/// point read after it. Does nothing for an instance that publishes no socket, or when no
/// socket can be opened — in which case the instance keeps the address it has.
pub async fn rebind_db_instance_plane<Eng: Send + Sync + 'static>(
    engine: &Eng,
    state: &::engine::ServiceState<super::ComAmazonawsTimestreaminfluxdbAmazontimestreaminfluxdbState>,
    id: &str,
) {
    let held = state
        .lock()
        .db_instance_planes
        .get(id)
        .map(|plane| Arc::clone(plane.series()));
    let Some(series) = held else {
        return;
    };
    let Some(plane) = open(engine, state, id, series).await else {
        return;
    };
    let (host, port) = (plane.host.clone(), plane.port);
    let mut store = state.lock();
    // Nothing to move an address onto if the instance has gone while the new port was being
    // bound; the socket this call opened closes as it returns.
    let Some(record) = store.db_instances.get_mut(id) else {
        return;
    };
    record.served_endpoint = Some(host);
    record.served_port = Some(port);
    // Replacing the entry drops the handle that held the OLD port open, so the instance
    // answers only at the address it now publishes.
    store.db_instance_planes.insert(id.to_string(), plane);
}

/// Open one instance's socket over `series` — the data it serves — or `None` when this
/// engine vends no listeners and when the socket cannot be bound at all. Either leaves the
/// instance publishing the host Timestream for InfluxDB's own zone would carry rather than
/// taking the request down with it.
async fn open<Eng: Send + Sync + 'static>(
    engine: &Eng,
    state: &::engine::ServiceState<super::ComAmazonawsTimestreaminfluxdbAmazontimestreaminfluxdbState>,
    id: &str,
    series: SharedTimeSeries,
) -> Option<InfluxDataPlane> {
    let shared = (engine as &dyn ::std::any::Any).downcast_ref::<::engine::InMemoryEngine>()?;
    let listeners = shared.listeners();
    let handler = InfluxSocket {
        series: Arc::clone(&series),
        state: state.clone(),
        instance_id: id.to_string(),
        // The core's own TLS server, lent to this plaintext socket so the `https://` half
        // of the address is terminated by the core rather than here. A socket this emulator
        // could prepare none for serves the `http://` half alone.
        upgrader: listeners.tls_upgrader().ok(),
    };
    // Opened on a task of its OWN, so a runtime that cannot bind a socket at all — one
    // built with no IO driver, which is what a host driving this emulator in process may
    // hand a request — leaves the create without a socket instead of taking the request
    // down with it.
    let opened = ::tokio::spawn(async move { listeners.tcp_plaintext(handler).await }).await;
    let listener = match opened {
        Ok(Ok(listener)) => listener,
        // Reported rather than discarded, so a port range or a descriptor table that is
        // exhausted is told apart from a runtime that opens no sockets at all.
        Ok(Err(error)) => {
            ::tracing::warn!(%error, "a DB instance's data-plane socket could not be opened");
            return None;
        }
        Err(_) => return None,
    };
    // The address the CORE says the socket is dialable at, read through the one shared
    // reading of it: a wildcard bind has no single address to dial and is published as
    // loopback, and an IPv6 bind keeps its brackets.
    let authority = ::service_common::data_plane_addressing::listener_authority(&listener);
    let host = ::service_common::data_plane_addressing::host_only(&authority).to_string();
    let port = ::service_common::data_plane_addressing::addressed_port(Some(&authority))
        .unwrap_or_else(|| i32::from(listener.port()));
    Some(InfluxDataPlane {
        host,
        port,
        series,
        _listener: listener,
    })
}

/// Serves ONE DB instance's socket: it speaks HTTP/1.1 on every connection the core hands
/// it and answers each request from that instance's own store.
struct InfluxSocket {
    series: SharedTimeSeries,
    upgrader: Option<TlsUpgrader>,
    /// The store the DB instance's own record lives in, and that instance's id: whether the
    /// plane is serving is the record's `status`, which is read for every request rather
    /// than inferred from this socket being open.
    state: ::engine::ServiceState<super::ComAmazonawsTimestreaminfluxdbAmazontimestreaminfluxdbState>,
    instance_id: String,
}

impl NativeHandler for InfluxSocket {
    /// One call per accepted connection. The handles are cloned into the connection's own
    /// future, which is what the core's `'static` handler contract asks for.
    fn serve(&self, conn: ListenerConn) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        Box::pin(serve_connection(
            Arc::clone(&self.series),
            self.state.clone(),
            self.instance_id.clone(),
            self.upgrader.clone(),
            conn,
        ))
    }
}

/// Speak HTTP/1.1 on one accepted connection until the client closes it.
///
/// The encryption is negotiated in band first, so one address serves the `http://` an
/// InfluxDB client dials and the `https://` a client of AWS dials. HTTP/1.1 only, as the
/// emulator's own front door offers: every request is fully buffered, so nothing relies on
/// HTTP/2 streaming.
async fn serve_connection(
    series: SharedTimeSeries,
    state: ::engine::ServiceState<super::ComAmazonawsTimestreaminfluxdbAmazontimestreaminfluxdbState>,
    instance_id: String,
    upgrader: Option<TlsUpgrader>,
    conn: ListenerConn,
) {
    let Some(conn) = ::service_common::in_band_tls::negotiated(conn, upgrader).await else {
        return;
    };
    let io = ::hyper_util::rt::TokioIo::new(conn);
    let service = ::hyper::service::service_fn(move |request: ::http::Request<_>| {
        let series = Arc::clone(&series);
        let state = state.clone();
        let instance_id = instance_id.clone();
        async move {
            // A panic anywhere behind this address is CONTAINED here and answered as the
            // protocol-shaped internal failure, exactly as the endpoint a control call
            // arrives on answers it: one client cannot take the connection, the socket or
            // the process down with it.
            let facts = ::emulator_server::RequestFacts::from_request(&request);
            Ok::<_, ::std::convert::Infallible>(
                ::emulator_server::contain_panics(
                    facts,
                    answer(series, state, instance_id, request),
                )
                .await,
            )
        }
    });
    if let Err(error) = ::hyper::server::conn::http1::Builder::new()
        .timer(::hyper_util::rt::TokioTimer::new())
        .header_read_timeout(REQUEST_DEADLINE)
        .serve_connection(io, service)
        .await
    {
        // A connection the client dropped, or a request this emulator could not frame, ends
        // here and takes nothing else with it: the next connection is served normally.
        ::tracing::debug!(
            %error,
            "a connection to a DB instance's InfluxDB address ended before its response"
        );
    }
}

/// Answer one request this socket accepted, from the instance's own store, with the
/// instance's control-plane status deciding whether it is serving at all.
async fn answer(
    series: SharedTimeSeries,
    state: ::engine::ServiceState<super::ComAmazonawsTimestreaminfluxdbAmazontimestreaminfluxdbState>,
    instance_id: String,
    request: ::http::Request<::hyper::body::Incoming>,
) -> ::http::Response<Full<Bytes>> {
    let (parts, body) = request.into_parts();
    let ceiling = usize::try_from(listener_caps::MAX_BYTES_PER_CONN).unwrap_or(usize::MAX);
    let limited = Limited::new(body, ceiling);
    let served = ::tokio::time::timeout(REQUEST_DEADLINE, async move {
        let Ok(collected) = limited.collect().await else {
            return v2_api::error(
                PAYLOAD_TOO_LARGE,
                "request too large",
                "the request body is larger than this endpoint reads on one connection",
            );
        };
        let read = ::http::Request::from_parts(parts, collected.to_bytes().to_vec());
        let status = instance_status(&state, &instance_id);
        v2_api::answer(&series, &read, status.as_deref())
    })
    .await;
    let answered = served.unwrap_or_else(|_| {
        v2_api::error(
            GATEWAY_TIMEOUT,
            "unavailable",
            "this endpoint did not finish the request in time",
        )
    });
    let (parts, body) = answered.into_parts();
    ::http::Response::from_parts(parts, Full::new(Bytes::from(body)))
}

/// The control-plane status of the DB instance `id` at this instant, or `None` for a record
/// that is no longer in the store.
///
/// The clock-owed settle to `AVAILABLE` is materialised first, exactly as every reporting
/// operation of this service does before answering: the transition was owed at the record's
/// `settles_at` whether or not anybody called, so applying it here is not a read-time
/// mutation of the instance's lifecycle.
fn instance_status(
    state: &::engine::ServiceState<super::ComAmazonawsTimestreaminfluxdbAmazontimestreaminfluxdbState>,
    id: &str,
) -> Option<String> {
    let mut store = state.lock();
    let record = store.db_instances.get_mut(id)?;
    record.settle(::std::time::SystemTime::now());
    Some(record.status.clone())
}
