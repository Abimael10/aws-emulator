// SPDX-License-Identifier: CC0-1.0

//! The InfluxDB v2 HTTP API one DB instance's own socket serves, answered out of that
//! instance's store ([`super::time_series`]).
//!
//! `GetDbInstance`'s `endpoint` is "the endpoint used to connect to InfluxDB" and its
//! `port` "the port number on which InfluxDB accepts connections", so what a client does
//! with the two is dial them and speak InfluxDB's own HTTP API. These are the routes it
//! speaks, each answered from the instance's records and nothing else:
//!
//! * `GET /health` — whether this instance is ready for queries and writes.
//! * `GET /ping` — the liveness probe, which carries the API line in its headers.
//! * `POST /api/v2/signin` — the admin user signs in with HTTP basic authentication and is
//!   given a session, which later requests on the same connection carry as a cookie.
//! * `POST /api/v2/write?org=&bucket=&precision=` — the points the body carries as
//!   InfluxDB line protocol are parsed by the format's own reader
//!   (`influxdb_line_protocol`) and stored in the named bucket.
//! * `POST /api/v2/query?org=` — Flux. This emulator runs no Flux engine (see
//!   [`FLUX_UNSERVED`]), so the request is answered with InfluxDB's own
//!   `not implemented` error rather than with a result nothing computed.
//!
//! Every other path is answered with InfluxDB's own `not found` error: this socket serves
//! one DB instance's API and nothing else.
//!
//! Authentication is the instance's INITIAL ADMIN USER and nothing else — this emulator
//! mints no API tokens, because no modelled operation creates one. A request presents that
//! user as HTTP basic authentication, as `Authorization: Token <user>:<password>`
//! (InfluxDB's v1-compatible credential form) or as the session cookie a sign-in set; a
//! request presenting none is refused `unauthorized`, as InfluxDB refuses one.

use std::collections::BTreeMap;

use ::base64::Engine as _;

use super::time_series::{FieldValue, Point, SharedTimeSeries};

/// Why `/api/v2/query` is not served: stated once, and stated to the client verbatim, so a
/// caller is told what this emulator does rather than given a result nothing produced.
pub const FLUX_UNSERVED: &str =
    "this endpoint serves no Flux engine, so a Flux query cannot be evaluated here; \
     points written with /api/v2/write are stored and can be read back through the \
     emulator's own state";

/// The InfluxDB API line this plane speaks. Timestream for InfluxDB runs InfluxDB v2 (the
/// default DB parameter group of this service configures `InfluxDBv2Parameters`), and
/// `/health` and `/ping` report the line rather than a build of it: this emulator serves
/// the v2 HTTP API, it does not ship an InfluxDB binary whose build number it could name.
pub const SERVED_API_VERSION: &str = "2.7";

/// The cookie an InfluxDB v2 sign-in sets, and the one later requests present.
pub const SESSION_COOKIE: &str = "influxdb-oss-session";

/// The default `precision` of a write, as `/api/v2/write` documents: nanoseconds.
const DEFAULT_PRECISION: &str = "ns";

/// Answer one request this instance's socket accepted, out of `store`.
///
/// `status` is the control-plane status of the DB instance this socket was opened for, as
/// the record holds it at this instant, or `None` for a record that is no longer there: an
/// instance that is not `AVAILABLE` is not serving, so every route answers that it is not
/// (see [`unavailable`]) rather than reporting a plane that is still coming up as ready.
///
/// Synchronous: every answer is computed from the store and the status the caller handed
/// in, so nothing here waits on anything.
#[must_use]
pub fn answer(
    store: &SharedTimeSeries,
    request: &::http::Request<Vec<u8>>,
    status: Option<&str>,
) -> ::http::Response<Vec<u8>> {
    let method = request.method();
    let path = request.uri().path();
    let serving = status == Some(STATUS_AVAILABLE);
    match (method.as_str(), path) {
        ("GET", "/health") => health(status),
        _ if !serving => unavailable(status),
        ("GET", "/ping") | ("HEAD", "/ping") => ping(),
        ("POST", "/api/v2/signin") => signin(store, request),
        ("POST", "/api/v2/write") => write(store, request),
        ("POST", "/api/v2/query") => query(store, request),
        _ => error(
            NOT_FOUND,
            CODE_NOT_FOUND,
            &format!("path not found: {method} {path}"),
        ),
    }
}

/// The one `Status` value a DB instance serves its InfluxDB API in, from this service's own
/// reporting vocabulary (`crate::state::STATUS_AVAILABLE`).
const STATUS_AVAILABLE: &str = crate::state::STATUS_AVAILABLE;

// =============================================================================
// Routes
// =============================================================================

/// `GET /health`: whether this instance is ready for queries and writes, which is what its
/// control-plane `status` says — the one record of that, read for every request rather than
/// inferred from the socket being open (the socket is opened by the create, which is a
/// transition span before the instance is `AVAILABLE`).
///
/// InfluxDB answers a failing health check `503` with `status` `fail`, and a passing one
/// `200` with `pass`.
fn health(status: Option<&str>) -> ::http::Response<Vec<u8>> {
    let passing = status == Some(STATUS_AVAILABLE);
    let body = ::serde_json::json!({
        "name": "influxdb",
        "message": if passing {
            "ready for queries and writes".to_string()
        } else {
            format!(
                "the DB instance is not ready for queries and writes: it is {}",
                status.unwrap_or("gone")
            )
        },
        "status": if passing { "pass" } else { "fail" },
        "checks": [],
        "version": SERVED_API_VERSION,
        "commit": "",
    });
    json(if passing { OK } else { SERVICE_UNAVAILABLE }, &body)
}

/// What every route but `/health` answers while the DB instance is not `AVAILABLE`: it is
/// not serving, so no request on it is answered out of its data.
fn unavailable(status: Option<&str>) -> ::http::Response<Vec<u8>> {
    error(
        SERVICE_UNAVAILABLE,
        CODE_UNAVAILABLE,
        &format!(
            "the DB instance is not ready for queries and writes: it is {}",
            status.unwrap_or("gone")
        ),
    )
}

/// `GET /ping`: no body, with the API line in the headers InfluxDB puts it in.
fn ping() -> ::http::Response<Vec<u8>> {
    let mut response = empty(NO_CONTENT);
    let headers = response.headers_mut();
    if let Ok(value) = ::http::HeaderValue::from_str(SERVED_API_VERSION) {
        headers.insert("x-influxdb-version", value.clone());
        headers.insert("x-influxdb-build", ::http::HeaderValue::from_static("oss2"));
    }
    response
}

/// `POST /api/v2/signin`: the admin user presents HTTP basic authentication and is given a
/// session, which the response sets as a cookie.
fn signin(store: &SharedTimeSeries, request: &::http::Request<Vec<u8>>) -> ::http::Response<Vec<u8>> {
    let Some(presented) = basic_credential(request) else {
        return unauthorized();
    };
    let mut held = store.lock().unwrap_or_else(::std::sync::PoisonError::into_inner);
    if !held
        .credential
        .authenticates(&presented.0, &presented.1)
    {
        return unauthorized();
    }
    let session = held.open_session();
    drop(held);
    let mut response = empty(NO_CONTENT);
    if let Ok(value) =
        ::http::HeaderValue::from_str(&format!("{SESSION_COOKIE}={session}; Path=/; HttpOnly"))
    {
        response.headers_mut().insert(::http::header::SET_COOKIE, value);
    }
    response
}

/// `POST /api/v2/write`: parse the body as line protocol and store its points in the
/// bucket the query string names.
///
/// Nothing is stored unless EVERY line parses, which is how InfluxDB answers a partially
/// unparseable body: the whole request is rejected.
fn write(store: &SharedTimeSeries, request: &::http::Request<Vec<u8>>) -> ::http::Response<Vec<u8>> {
    if !authenticated(store, request) {
        return unauthorized();
    }
    let parameters = query_parameters(request);
    let Some(organization) = parameters.get("org") else {
        return error(BAD_REQUEST, CODE_INVALID, "missing the org parameter");
    };
    let Some(bucket) = parameters.get("bucket") else {
        return error(BAD_REQUEST, CODE_INVALID, "missing the bucket parameter");
    };
    let precision = parameters
        .get("precision")
        .map_or(DEFAULT_PRECISION, String::as_str);
    let Some(scale) = precision_scale(precision) else {
        return error(
            BAD_REQUEST,
            CODE_INVALID,
            &format!("unknown precision: {precision}"),
        );
    };
    let Ok(body) = ::std::str::from_utf8(request.body()) else {
        return error(
            BAD_REQUEST,
            CODE_INVALID,
            "the request body is not valid UTF-8 line protocol",
        );
    };
    let mut held = store.lock().unwrap_or_else(::std::sync::PoisonError::into_inner);
    if !held.has_organization(organization) {
        return error(
            NOT_FOUND,
            CODE_NOT_FOUND,
            &format!("organization not found: {organization}"),
        );
    }
    if !held.has_bucket(bucket) {
        return error(
            NOT_FOUND,
            CODE_NOT_FOUND,
            &format!("bucket not found: {bucket}"),
        );
    }
    let written_at = ::std::time::SystemTime::now()
        .duration_since(::std::time::SystemTime::UNIX_EPOCH)
        .map_or(0, |since| i64::try_from(since.as_nanos()).unwrap_or(i64::MAX));
    let points = match parsed_points(body, scale, written_at) {
        Ok(points) => points,
        Err(reason) => return error(BAD_REQUEST, CODE_INVALID, &reason),
    };
    // The bucket was checked above, so the write is taken; a bucket that disappeared
    // between the two is reported as the not-found the check would have given.
    if !held.write(bucket, points) {
        return error(
            NOT_FOUND,
            CODE_NOT_FOUND,
            &format!("bucket not found: {bucket}"),
        );
    }
    drop(held);
    empty(NO_CONTENT)
}

/// `POST /api/v2/query`: Flux, which this emulator runs no engine for.
///
/// Answered with InfluxDB's own `not implemented` error, carrying [`FLUX_UNSERVED`]: a
/// caller is told that this endpoint evaluates no Flux rather than handed a result set
/// nothing computed from the points it wrote.
fn query(store: &SharedTimeSeries, request: &::http::Request<Vec<u8>>) -> ::http::Response<Vec<u8>> {
    if !authenticated(store, request) {
        return unauthorized();
    }
    error(NOT_IMPLEMENTED, CODE_NOT_IMPLEMENTED, FLUX_UNSERVED)
}

// =============================================================================
// Line protocol
// =============================================================================

/// The points `body` carries, with each timestamp scaled from `precision_scale`
/// nanoseconds and a line that carried none taken at `written_at`.
///
/// The parse is the line-protocol crate's, so a body this emulator accepts is one InfluxDB
/// accepts: the error text a rejection carries is the parser's own.
fn parsed_points(body: &str, scale: i64, written_at: i64) -> Result<Vec<Point>, String> {
    let mut points = Vec::new();
    for line in ::influxdb_line_protocol::parse_lines(body) {
        let line = line.map_err(|error| format!("unable to parse points: {error}"))?;
        let tags = line
            .series
            .tag_set
            .as_ref()
            .map(|set| {
                set.iter()
                    .map(|(key, value)| (key.to_string(), value.to_string()))
                    .collect::<BTreeMap<String, String>>()
            })
            .unwrap_or_default();
        let fields = line
            .field_set
            .iter()
            .map(|(name, value)| (name.to_string(), field_value(value)))
            .collect();
        points.push(Point {
            measurement: line.series.measurement.to_string(),
            tags,
            fields,
            timestamp_ns: line
                .timestamp
                .map_or(written_at, |stamp| stamp.saturating_mul(scale)),
        });
    }
    Ok(points)
}

/// One parsed field value as this store holds it.
fn field_value(value: &::influxdb_line_protocol::FieldValue<'_>) -> FieldValue {
    match value {
        ::influxdb_line_protocol::FieldValue::I64(number) => FieldValue::Integer(*number),
        ::influxdb_line_protocol::FieldValue::U64(number) => FieldValue::Unsigned(*number),
        ::influxdb_line_protocol::FieldValue::F64(number) => FieldValue::Float(*number),
        ::influxdb_line_protocol::FieldValue::Boolean(value) => FieldValue::Boolean(*value),
        ::influxdb_line_protocol::FieldValue::String(text) => FieldValue::Text(text.to_string()),
    }
}

/// How many nanoseconds one unit of `precision` is, for the four values
/// `/api/v2/write`'s `precision` admits (`ns`, `us`, `ms`, `s`), or `None` for anything
/// else.
#[must_use]
pub fn precision_scale(precision: &str) -> Option<i64> {
    match precision {
        "ns" => Some(1),
        "us" => Some(1_000),
        "ms" => Some(1_000_000),
        "s" => Some(1_000_000_000),
        _ => None,
    }
}

// =============================================================================
// Authentication
// =============================================================================

/// Whether `request` presents this instance's admin user, in any of the three forms an
/// InfluxDB client presents it in.
fn authenticated(store: &SharedTimeSeries, request: &::http::Request<Vec<u8>>) -> bool {
    let held = store.lock().unwrap_or_else(::std::sync::PoisonError::into_inner);
    if let Some((username, password)) = basic_credential(request) {
        return held.credential.authenticates(&username, &password);
    }
    if let Some(token) = bearer_token(request) {
        // InfluxDB's v1-compatible credential: `Token <username>:<password>`. This
        // emulator mints no API tokens, so a token carrying no user name names nobody.
        if let Some((username, password)) = token.split_once(':') {
            return held.credential.authenticates(username, password);
        }
        return false;
    }
    presented_session(request).is_some_and(|session| held.has_session(&session))
}

/// The `(username, password)` of the request's `Authorization: Basic` credential, or
/// `None` when it carries none or one this emulator cannot decode.
fn basic_credential(request: &::http::Request<Vec<u8>>) -> Option<(String, String)> {
    let header = request
        .headers()
        .get(::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?;
    let encoded = header
        .strip_prefix("Basic ")
        .or_else(|| header.strip_prefix("basic "))?;
    let decoded = ::base64::engine::general_purpose::STANDARD
        .decode(encoded.trim())
        .ok()?;
    let pair = String::from_utf8(decoded).ok()?;
    let (username, password) = pair.split_once(':')?;
    Some((username.to_string(), password.to_string()))
}

/// The value of the request's `Authorization: Token` credential, or `None` when it carries
/// none.
fn bearer_token(request: &::http::Request<Vec<u8>>) -> Option<String> {
    let header = request
        .headers()
        .get(::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?;
    for prefix in ["Token ", "token ", "Bearer ", "bearer "] {
        if let Some(value) = header.strip_prefix(prefix) {
            return Some(value.trim().to_string());
        }
    }
    None
}

/// The session id the request's `Cookie` header carries, or `None` when it carries none.
fn presented_session(request: &::http::Request<Vec<u8>>) -> Option<String> {
    let header = request.headers().get(::http::header::COOKIE)?.to_str().ok()?;
    header.split(';').find_map(|pair| {
        let (name, value) = pair.trim().split_once('=')?;
        (name == SESSION_COOKIE).then(|| value.to_string())
    })
}

// =============================================================================
// Query string
// =============================================================================

/// The request's query parameters, percent-decoded.
fn query_parameters(request: &::http::Request<Vec<u8>>) -> BTreeMap<String, String> {
    let Some(query) = request.uri().query() else {
        return BTreeMap::new();
    };
    query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| match pair.split_once('=') {
            Some((name, value)) => (
                ::smithy_server_runtime::percent_decode(name),
                ::smithy_server_runtime::percent_decode(value),
            ),
            None => (
                ::smithy_server_runtime::percent_decode(pair),
                String::new(),
            ),
        })
        .collect()
}

// =============================================================================
// Answers
// =============================================================================

/// The statuses this plane answers with.
const OK: u16 = 200;
/// See [`OK`].
const NO_CONTENT: u16 = 204;
/// See [`OK`].
const BAD_REQUEST: u16 = 400;
/// See [`OK`].
const UNAUTHORIZED: u16 = 401;
/// See [`OK`].
const NOT_FOUND: u16 = 404;
/// See [`OK`].
const NOT_IMPLEMENTED: u16 = 501;
/// See [`OK`].
const SERVICE_UNAVAILABLE: u16 = 503;

/// The `code` values of the InfluxDB error body, each paired with the status above that
/// InfluxDB answers it with.
const CODE_INVALID: &str = "invalid";
/// See [`CODE_INVALID`].
const CODE_UNAUTHORIZED: &str = "unauthorized";
/// See [`CODE_INVALID`].
const CODE_NOT_FOUND: &str = "not found";
/// See [`CODE_INVALID`].
const CODE_NOT_IMPLEMENTED: &str = "not implemented";
/// See [`CODE_INVALID`].
const CODE_UNAVAILABLE: &str = "unavailable";

/// The refusal a request presenting no recognised credential gets.
fn unauthorized() -> ::http::Response<Vec<u8>> {
    error(
        UNAUTHORIZED,
        CODE_UNAUTHORIZED,
        "unauthorized access: this DB instance authenticates its initial admin user",
    )
}

/// One InfluxDB error body: the `code` a client branches on and the `message` it shows.
#[must_use]
pub fn error(status: u16, code: &str, message: &str) -> ::http::Response<Vec<u8>> {
    json(
        status,
        &::serde_json::json!({ "code": code, "message": message }),
    )
}

/// One JSON answer.
fn json(status: u16, body: &::serde_json::Value) -> ::http::Response<Vec<u8>> {
    let bytes = ::serde_json::to_vec(body).unwrap_or_default();
    let mut response = ::http::Response::new(bytes);
    *response.status_mut() =
        ::http::StatusCode::from_u16(status).unwrap_or(::http::StatusCode::INTERNAL_SERVER_ERROR);
    response.headers_mut().insert(
        ::http::header::CONTENT_TYPE,
        ::http::HeaderValue::from_static("application/json; charset=utf-8"),
    );
    response
}

/// One answer with no body.
fn empty(status: u16) -> ::http::Response<Vec<u8>> {
    let mut response = ::http::Response::new(Vec::new());
    *response.status_mut() =
        ::http::StatusCode::from_u16(status).unwrap_or(::http::StatusCode::INTERNAL_SERVER_ERROR);
    response
}

#[cfg(test)]
mod tests {
    use super::super::time_series::{AdminCredential, TimeSeries};
    use super::*;
    use ::std::sync::{Arc, Mutex};

    /// A plane whose instance was created with this admin user, organization and bucket.
    fn plane() -> SharedTimeSeries {
        Arc::new(Mutex::new(TimeSeries::new(AdminCredential::new(
            Some("admin"),
            "supersecret1",
            Some("sim2real"),
            Some("metrics"),
        ))))
    }

    /// The `Authorization: Basic` header value for `user:password`.
    fn basic(user: &str, password: &str) -> String {
        format!(
            "Basic {}",
            ::base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"))
        )
    }

    /// One request answered for an instance whose control plane reports it `AVAILABLE`,
    /// which is the state every case below but the last asserts the routes in.
    fn answered(
        plane: &SharedTimeSeries,
        request: &::http::Request<Vec<u8>>,
    ) -> ::http::Response<Vec<u8>> {
        answer(plane, request, Some(STATUS_AVAILABLE))
    }

    /// One request to this plane.
    fn request(
        method: &str,
        uri: &str,
        headers: &[(&str, String)],
        body: &str,
    ) -> ::http::Request<Vec<u8>> {
        let mut builder = ::http::Request::builder().method(method).uri(uri);
        for (name, value) in headers {
            builder = builder.header(*name, value);
        }
        builder
            .body(body.as_bytes().to_vec())
            .expect("a well-formed request")
    }

    /// RULE: `/health` reports this instance ready for queries and writes, with no
    /// credential needed.
    #[test]
    fn health_reports_a_passing_instance() {
        let plane = plane();
        let answer = answered(&plane, &request("GET", "/health", &[], ""));
        assert_eq!(
            answer.status().as_u16(),
            OK,
            "case \"GET /health\": expected 200"
        );
        let body: ::serde_json::Value =
            ::serde_json::from_slice(answer.body()).expect("a JSON body");
        assert_eq!(
            body["status"].as_str(),
            Some("pass"),
            "case \"GET /health\": expected status pass, got {body}"
        );
    }

    /// RULE: a credential this instance does not hold is refused `unauthorized`, whichever
    /// form it is presented in, and the admin user's own is accepted in all three forms.
    #[test]
    fn only_the_instances_admin_user_is_authenticated() {
        let plane = plane();
        let session = plane
            .lock()
            .expect("the plane's store")
            .open_session();
        let body = "cpu,host=a value=1i 1700000000000000000";
        let uri = "/api/v2/write?org=sim2real&bucket=metrics&precision=ns";
        for (case, headers, expected) in [
            (
                "the admin user by basic authentication",
                vec![("authorization", basic("admin", "supersecret1"))],
                NO_CONTENT,
            ),
            (
                "the admin user by the v1-compatible token form",
                vec![("authorization", "Token admin:supersecret1".to_string())],
                NO_CONTENT,
            ),
            (
                "a session the plane minted",
                vec![("cookie", format!("{SESSION_COOKIE}={session}"))],
                NO_CONTENT,
            ),
            (
                "the right user with the wrong password",
                vec![("authorization", basic("admin", "nope"))],
                UNAUTHORIZED,
            ),
            (
                "a user this instance does not hold",
                vec![("authorization", basic("someone", "supersecret1"))],
                UNAUTHORIZED,
            ),
            (
                "a token naming no user",
                vec![("authorization", "Token an-api-token".to_string())],
                UNAUTHORIZED,
            ),
            (
                "a session nobody minted",
                vec![("cookie", format!("{SESSION_COOKIE}=deadbeef"))],
                UNAUTHORIZED,
            ),
            ("no credential at all", vec![], UNAUTHORIZED),
        ] {
            let answer = answered(&plane, &request("POST", uri, &headers, body));
            assert_eq!(
                answer.status().as_u16(),
                expected,
                "case {case:?}: expected {expected}, got {} with {:?}",
                answer.status().as_u16(),
                String::from_utf8_lossy(answer.body())
            );
        }
    }

    /// RULE: a write stores every point the body carries, with each timestamp scaled from
    /// the precision the request named; a body that does not parse stores nothing.
    #[test]
    fn a_write_stores_the_points_the_body_carries() {
        for (case, precision, body, expected) in [
            (
                "one point in nanoseconds",
                "ns",
                "cpu,host=a value=1i 1700000000000000000",
                vec![1_700_000_000_000_000_000_i64],
            ),
            (
                "one point in seconds",
                "s",
                "cpu,host=a value=1i 1700000000",
                vec![1_700_000_000_000_000_000],
            ),
            (
                "one point in milliseconds",
                "ms",
                "cpu,host=a value=1i 1700000000000",
                vec![1_700_000_000_000_000_000],
            ),
            (
                "one point in microseconds",
                "us",
                "cpu,host=a value=1i 1700000000000000",
                vec![1_700_000_000_000_000_000],
            ),
            (
                "three points in one body",
                "ns",
                "cpu,host=a value=1i 1\ncpu,host=b value=2i 2\nmem free=3i 3",
                vec![1, 2, 3],
            ),
        ] {
            let plane = plane();
            let uri = format!("/api/v2/write?org=sim2real&bucket=metrics&precision={precision}");
            let answer = answered(
                &plane,
                &request(
                    "POST",
                    &uri,
                    &[("authorization", basic("admin", "supersecret1"))],
                    body,
                ),
            );
            assert_eq!(
                answer.status().as_u16(),
                NO_CONTENT,
                "case {case:?}: expected 204, got {} with {:?}",
                answer.status().as_u16(),
                String::from_utf8_lossy(answer.body())
            );
            let held = plane.lock().expect("the plane's store");
            let stamps: Vec<i64> = held
                .points("metrics")
                .iter()
                .map(|point| point.timestamp_ns)
                .collect();
            assert_eq!(
                stamps, expected,
                "case {case:?}: expected the points at {expected:?}, got {stamps:?}"
            );
        }
    }

    /// RULE: a write that names an organization or a bucket this instance does not hold is
    /// `not found`, one whose body does not parse is `invalid`, and neither stores a point.
    #[test]
    fn a_write_this_instance_cannot_serve_stores_nothing() {
        for (case, uri, body, expected) in [
            (
                "another organization",
                "/api/v2/write?org=elsewhere&bucket=metrics",
                "cpu value=1i 1",
                NOT_FOUND,
            ),
            (
                "a bucket this instance does not hold",
                "/api/v2/write?org=sim2real&bucket=other",
                "cpu value=1i 1",
                NOT_FOUND,
            ),
            (
                "no org parameter",
                "/api/v2/write?bucket=metrics",
                "cpu value=1i 1",
                BAD_REQUEST,
            ),
            (
                "no bucket parameter",
                "/api/v2/write?org=sim2real",
                "cpu value=1i 1",
                BAD_REQUEST,
            ),
            (
                "a precision the API does not admit",
                "/api/v2/write?org=sim2real&bucket=metrics&precision=fortnights",
                "cpu value=1i 1",
                BAD_REQUEST,
            ),
            (
                "a body that is not line protocol",
                "/api/v2/write?org=sim2real&bucket=metrics",
                "this is not line protocol at all",
                BAD_REQUEST,
            ),
            (
                "a body whose SECOND line does not parse",
                "/api/v2/write?org=sim2real&bucket=metrics",
                "cpu value=1i 1\n=====",
                BAD_REQUEST,
            ),
        ] {
            let plane = plane();
            let answer = answered(
                &plane,
                &request(
                    "POST",
                    uri,
                    &[("authorization", basic("admin", "supersecret1"))],
                    body,
                ),
            );
            assert_eq!(
                answer.status().as_u16(),
                expected,
                "case {case:?}: expected {expected}, got {} with {:?}",
                answer.status().as_u16(),
                String::from_utf8_lossy(answer.body())
            );
            assert!(
                plane
                    .lock()
                    .expect("the plane's store")
                    .points("metrics")
                    .is_empty(),
                "case {case:?}: a refused write must store nothing"
            );
        }
    }

    /// RULE: a Flux query is answered with InfluxDB's own `not implemented`, carrying what
    /// this emulator does not do — never with a result set.
    #[test]
    fn a_flux_query_is_refused_rather_than_answered_with_a_result() {
        let plane = plane();
        let answer = answered(
            &plane,
            &request(
                "POST",
                "/api/v2/query?org=sim2real",
                &[("authorization", basic("admin", "supersecret1"))],
                "from(bucket:\"metrics\") |> range(start:-1h)",
            ),
        );
        assert_eq!(
            answer.status().as_u16(),
            NOT_IMPLEMENTED,
            "case \"a Flux query\": expected 501"
        );
        let body: ::serde_json::Value =
            ::serde_json::from_slice(answer.body()).expect("a JSON body");
        assert_eq!(
            body["code"].as_str(),
            Some(CODE_NOT_IMPLEMENTED),
            "case \"a Flux query\": expected InfluxDB's own code, got {body}"
        );
        assert_eq!(
            body["message"].as_str(),
            Some(FLUX_UNSERVED),
            "case \"a Flux query\": the refusal must say what is not served, got {body}"
        );
    }

    /// RULE: a path this plane does not serve is `not found`.
    #[test]
    fn a_path_this_plane_does_not_serve_is_not_found() {
        let plane = plane();
        for (case, method, uri) in [
            ("the root", "GET", "/"),
            ("an API path of another version", "POST", "/api/v1/write"),
            ("a path under a served one", "GET", "/health/detail"),
            ("the write path by GET", "GET", "/api/v2/write"),
        ] {
            let answer = answered(&plane, &request(method, uri, &[], ""));
            assert_eq!(
                answer.status().as_u16(),
                NOT_FOUND,
                "case {case:?}: expected 404, got {}",
                answer.status().as_u16()
            );
        }
    }

    /// RULE: whether this plane serves at all is the DB instance's control-plane `status`,
    /// not whether its socket is open: an instance in any other status reports `fail` on
    /// `/health` with HTTP 503, and every other route answers `unavailable`.
    ///
    /// The rows span the transient statuses this service reports, the terminal one, and the
    /// record being gone.
    #[test]
    fn the_plane_serves_only_while_the_instance_is_available() {
        use crate::state::{
            STATUS_CREATING, STATUS_DELETING, STATUS_REBOOTING, STATUS_UPDATING,
            STATUS_UPDATING_DEPLOYMENT_TYPE, STATUS_UPDATING_INSTANCE_TYPE,
        };

        let plane = plane();
        for (case, status, health_status, route_status) in [
            ("available", Some(STATUS_AVAILABLE), OK, NO_CONTENT),
            ("coming up", Some(STATUS_CREATING), SERVICE_UNAVAILABLE, SERVICE_UNAVAILABLE),
            ("updating", Some(STATUS_UPDATING), SERVICE_UNAVAILABLE, SERVICE_UNAVAILABLE),
            (
                "updating its instance type",
                Some(STATUS_UPDATING_INSTANCE_TYPE),
                SERVICE_UNAVAILABLE,
                SERVICE_UNAVAILABLE,
            ),
            (
                "updating its deployment type",
                Some(STATUS_UPDATING_DEPLOYMENT_TYPE),
                SERVICE_UNAVAILABLE,
                SERVICE_UNAVAILABLE,
            ),
            ("rebooting", Some(STATUS_REBOOTING), SERVICE_UNAVAILABLE, SERVICE_UNAVAILABLE),
            ("deleting", Some(STATUS_DELETING), SERVICE_UNAVAILABLE, SERVICE_UNAVAILABLE),
            ("no record at all", None, SERVICE_UNAVAILABLE, SERVICE_UNAVAILABLE),
        ] {
            let health = answer(&plane, &request("GET", "/health", &[], ""), status);
            assert_eq!(
                health.status().as_u16(),
                health_status,
                "case {case:?}: expected /health to answer {health_status}, got {}",
                health.status().as_u16()
            );
            let body: ::serde_json::Value =
                ::serde_json::from_slice(health.body()).expect("a JSON body");
            let reported = if health_status == OK { "pass" } else { "fail" };
            assert_eq!(
                body["status"].as_str(),
                Some(reported),
                "case {case:?}: expected /health to report {reported}, got {body}"
            );
            let write = answer(
                &plane,
                &request(
                    "POST",
                    "/api/v2/write?org=sim2real&bucket=metrics",
                    &[("authorization", basic("admin", "supersecret1"))],
                    "cpu value=1i 1",
                ),
                status,
            );
            assert_eq!(
                write.status().as_u16(),
                route_status,
                "case {case:?}: expected a write to answer {route_status}, got {} with {:?}",
                write.status().as_u16(),
                String::from_utf8_lossy(write.body())
            );
        }
    }

    /// RULE: a query parameter's percent escapes are resolved through the framework's one
    /// decoder, so an organization or bucket whose name carries one is read as the client
    /// wrote it — and the DECODED value is what the write is judged against.
    #[test]
    fn query_parameters_are_percent_decoded() {
        let plane = plane();
        for (case, encoded, expected) in [
            ("no escape", "sim2real", "sim2real"),
            ("a space as %20", "two%20words", "two words"),
            ("a space as +", "two+words", "two words"),
            ("a reserved character", "a%2Fb", "a/b"),
            ("a trailing percent with no pair", "abc%", "abc%"),
            ("an escape that spells the instance's own organization", "sim2%72eal", "sim2real"),
        ] {
            let read = request(
                "POST",
                &format!("/api/v2/write?org={encoded}&bucket=metrics"),
                &[("authorization", basic("admin", "supersecret1"))],
                "cpu value=1i 1",
            );
            let got = query_parameters(&read);
            assert_eq!(
                got.get("org").map(String::as_str),
                Some(expected),
                "case {case:?}: expected the org {expected:?}, got {:?}",
                got.get("org")
            );
            let answered_status = answered(&plane, &read).status().as_u16();
            let expected_status = if expected == "sim2real" {
                NO_CONTENT
            } else {
                NOT_FOUND
            };
            assert_eq!(
                answered_status, expected_status,
                "case {case:?}: expected {expected_status}, got {answered_status}"
            );
        }
    }
}
