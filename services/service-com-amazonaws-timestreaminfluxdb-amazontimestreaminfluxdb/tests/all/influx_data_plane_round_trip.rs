// SPDX-License-Identifier: CC0-1.0

//! The address a DB instance publishes, and the InfluxDB v2 HTTP API served there.
//!
//! RULE: `endpoint` and `port` are the address a client DIALS to speak InfluxDB's own HTTP
//! API to that instance (`endpoint` is "the endpoint used to connect to InfluxDB", `port`
//! "the port number on which InfluxDB accepts connections"), so for every standalone DB
//! instance this emulator holds, a socket answers the InfluxDB v2 API at exactly the
//! `endpoint:port` the control plane reports — the same pair from the create, from
//! `GetDbInstance` and from `ListDbInstances` — and it answers out of that instance's own
//! data: `GET /health` reports it ready, `POST /api/v2/signin` accepts the instance's
//! initial admin user and nobody else, and `POST /api/v2/write` stores the points its
//! line-protocol body carries in the instance's own bucket. An update that asks InfluxDB to
//! accept connections on another port moves the instance onto another socket, carrying its
//! data with it and closing the port it left. Deleting the instance closes the port.
//!
//! The rows span what the rule generalises over: the number of instances one store holds
//! (so two instances answer on two addresses, each out of its own data), which of the
//! optional members the create named (the admin user, organization and bucket, or none of
//! them, in which case InfluxDB's own defaults stand), the port the request asked for (so
//! the address reported is the one served however the request was written), and the
//! credential form a client presents.
//!
//! Every path and status named here is InfluxDB's own v2 HTTP API; the two control-plane
//! members come from
//! `smithy_query(query="operation", operation="GetDbInstance")`.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use service_com_amazonaws_timestreaminfluxdb_amazontimestreaminfluxdb as influxdb;

use crate::db_instance_lifecycle::{call, ok, router_and_engine};

/// How long a test waits for something it polls for. Generous: it bounds a condition that
/// should already hold, and a loaded host must not fail on it.
const DEADLINE: Duration = Duration::from_secs(30);

/// How long a poll waits between looks.
const PROBE_INTERVAL: Duration = Duration::from_millis(20);

/// One HTTP exchange on a DB instance's own socket, over a plain TCP connection: the
/// status, the response headers lower-cased, and the body.
pub(crate) struct Answer {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

impl Answer {
    /// The status the answer carries.
    pub(crate) fn status(&self) -> u16 {
        self.status
    }

    /// The body it carries.
    pub(crate) fn body(&self) -> &str {
        &self.body
    }

    /// The value of the response header `name`, if it carries one.
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(header, _)| header == name)
            .map(|(_, value)| value.as_str())
    }
}

/// The loopback address the core binds a resource listener on by default, and so the only
/// address a case here dials. The host the control plane announced is asserted to BE this
/// address before anything is dialled, so a case can only ever reach the emulator's own
/// listener on this machine; each dial then names `127.0.0.1` itself.
const LOOPBACK: &str = "127.0.0.1";

/// The port of an announced `host:port` pair, having first held the host to [`LOOPBACK`].
fn loopback_port(case: &str, host: &str, port: i64) -> u16 {
    assert_eq!(
        host, LOOPBACK,
        "case {case:?}: a case dials only this emulator's own loopback listener, got the \
         announced host {host:?}"
    );
    u16::try_from(port).unwrap_or_else(|_| {
        panic!("case {case:?}: the announced port {port} is not a port number")
    })
}

/// Speak one HTTP/1.1 request on the announced `host:port` and read the whole answer.
///
/// Written over a raw socket rather than with an HTTP client crate because what is under
/// test is that the ADDRESS the control plane reported answers the InfluxDB API at all:
/// the request is exactly the bytes an InfluxDB client sends.
///
/// `pub(crate)` because the restore suite asserts the same thing about the address a
/// RESTORED instance reports, and a second spelling of this dial would let the two drift.
pub(crate) async fn exchange(
    case: &str,
    host: &str,
    port: i64,
    method: &str,
    target: &str,
    headers: &[(&str, String)],
    body: &str,
) -> Answer {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    let dialled = loopback_port(case, host, port);
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", dialled))
        .await
        .unwrap_or_else(|error| {
            panic!("case {case:?}: the published address {host}:{port} must accept a connection, got {error}")
        });
    let mut request = format!("{method} {target} HTTP/1.1\r\nHost: {host}:{port}\r\n");
    for (name, value) in headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    request.push_str(&format!("content-length: {}\r\n", body.len()));
    request.push_str("connection: close\r\n\r\n");
    request.push_str(body);
    stream
        .write_all(request.as_bytes())
        .await
        .unwrap_or_else(|error| panic!("case {case:?}: the request must be written, got {error}"));
    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .await
        .unwrap_or_else(|error| panic!("case {case:?}: the answer must be read, got {error}"));
    parsed(case, &raw)
}

/// One raw HTTP/1.1 answer, split into its status, headers and body.
fn parsed(case: &str, raw: &[u8]) -> Answer {
    let text = String::from_utf8_lossy(raw).into_owned();
    let (head, body) = text
        .split_once("\r\n\r\n")
        .unwrap_or_else(|| panic!("case {case:?}: expected an HTTP answer, got {text:?}"));
    let mut lines = head.split("\r\n");
    let status_line = lines
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: expected a status line, got {text:?}"));
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok())
        .unwrap_or_else(|| panic!("case {case:?}: expected a status code, got {status_line:?}"));
    let headers = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_string()))
        .collect();
    Answer {
        status,
        headers,
        body: body.to_string(),
    }
}

/// The `Authorization: Basic` value for `user:password`, encoded the way an HTTP client
/// encodes it.
pub(crate) fn basic(user: &str, password: &str) -> String {
    use base64::Engine as _;
    format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"))
    )
}

/// The `(endpoint, port)` a response reports.
pub(crate) fn address(case: &str, body: &Value) -> (String, i64) {
    let endpoint = body["endpoint"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected an `endpoint`, got {body}"))
        .to_string();
    let port = body["port"]
        .as_i64()
        .unwrap_or_else(|| panic!("case {case:?}: expected a `port`, got {body}"));
    (endpoint, port)
}

/// Poll `GET /health` on the published address until it answers 200, the way a client
/// waiting for an instance to come up does: look, and if it has not answered yet look
/// again one probe interval later, until the generous deadline above passes.
///
/// `pub(crate)` because the lifecycle suite asserts the SAME rule about the pair every
/// create reports — that it is an address the InfluxDB API answers on — and a second
/// spelling of that probe would let the two drift.
pub(crate) async fn poll_health(case: &str, host: &str, port: i64) -> Answer {
    let started = std::time::Instant::now();
    loop {
        let answer = exchange(case, host, port, "GET", "/health", &[], "").await;
        if answer.status == 200 {
            return answer;
        }
        assert!(
            started.elapsed() < DEADLINE,
            "case {case:?}: /health never answered 200 on {host}:{port}, last was {} with {:?}",
            answer.status,
            answer.body
        );
        tokio::time::sleep(PROBE_INTERVAL).await;
    }
}

/// One row: a `CreateDbInstance` body, and the admin user, organization and bucket the
/// instance's own plane must then authenticate and write into.
struct Row {
    case: &'static str,
    body: Value,
    username: &'static str,
    password: &'static str,
    organization: &'static str,
    bucket: &'static str,
}

/// Rows spanning which optional members the create named and the port it asked for.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the admin user, organization and bucket all named, port omitted",
            body: json!({
                "name": "influx-named",
                "password": "supersecret1",
                "username": "myadmin",
                "organization": "sim2real",
                "bucket": "metrics",
                "dbInstanceType": "db.influx.medium",
                "allocatedStorage": 20,
                "vpcSubnetIds": ["subnet-aaa", "subnet-bbb"],
                "vpcSecurityGroupIds": ["sg-aaa"],
                "publiclyAccessible": false,
                "networkType": "IPV4",
            }),
            username: "myadmin",
            password: "supersecret1",
            organization: "sim2real",
            bucket: "metrics",
        },
        Row {
            case: "none of the three named, so InfluxDB's own defaults stand",
            body: json!({
                "name": "influx-defaults",
                "password": "anotherpassword9",
                "dbInstanceType": "db.influx.large",
                "allocatedStorage": 20,
                "vpcSubnetIds": ["subnet-ccc"],
                "vpcSecurityGroupIds": ["sg-ccc"],
            }),
            username: "admin",
            password: "anotherpassword9",
            organization: "default",
            bucket: "default",
        },
        Row {
            case: "the published lower bound of the modelled port asked for",
            body: json!({
                "name": "influx-low-port",
                "password": "thirdpassword12",
                "username": "lowuser",
                "organization": "loworg",
                "bucket": "lowbucket",
                "dbInstanceType": "db.influx.medium",
                "allocatedStorage": 20,
                "port": 1024,
                "vpcSubnetIds": ["subnet-ddd"],
                "vpcSecurityGroupIds": ["sg-ddd"],
            }),
            username: "lowuser",
            password: "thirdpassword12",
            organization: "loworg",
            bucket: "lowbucket",
        },
        Row {
            case: "the published upper bound of the modelled port asked for",
            body: json!({
                "name": "influx-high-port",
                "password": "fourthpassword7",
                "username": "highuser",
                "organization": "highorg",
                "bucket": "highbucket",
                "dbInstanceType": "db.influx.medium",
                "allocatedStorage": 20,
                "port": 65535,
                "vpcSubnetIds": ["subnet-eee"],
                "vpcSecurityGroupIds": ["sg-eee"],
            }),
            username: "highuser",
            password: "fourthpassword7",
            organization: "highorg",
            bucket: "highbucket",
        },
    ]
}

#[tokio::test]
async fn every_instance_serves_the_influxdb_api_at_the_address_it_reports() {
    let (router, engine) = router_and_engine();
    for row in rows() {
        let case = row.case;
        let created = ok(case, call(&router, "CreateDbInstance", row.body.clone()).await);
        let id = created["id"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: expected an `id`, got {created}"))
            .to_string();
        let (host, port) = address(case, &created);

        // The pair every read path reports is the SAME pair: an address a client was given
        // by one call is the address the next call confirms.
        let fetched = ok(
            case,
            call(&router, "GetDbInstance", json!({ "identifier": &id })).await,
        );
        assert_eq!(
            address(case, &fetched),
            (host.clone(), port),
            "case {case:?}: GetDbInstance must report the address the create reported"
        );
        let listed = ok(case, call(&router, "ListDbInstances", json!({})).await);
        let summary = listed["items"]
            .as_array()
            .unwrap_or_else(|| panic!("case {case:?}: expected an `items` list, got {listed}"))
            .iter()
            .find(|item| item["id"].as_str() == Some(id.as_str()))
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: the created instance must be listed"));
        assert_eq!(
            address(case, &summary),
            (host.clone(), port),
            "case {case:?}: the summary must report the same address"
        );

        // 1. /health answers at that address.
        let health = poll_health(case, &host, port).await;
        let reported: Value = serde_json::from_str(&health.body)
            .unwrap_or_else(|_| panic!("case {case:?}: expected a JSON /health body, got {:?}", health.body));
        assert_eq!(
            reported["status"].as_str(),
            Some("pass"),
            "case {case:?}: expected /health to report status pass, got {reported}"
        );

        // 2. The instance's own admin user signs in, and is given a session.
        let signin = exchange(
            case,
            &host,
            port,
            "POST",
            "/api/v2/signin",
            &[("authorization", basic(row.username, row.password))],
            "",
        )
        .await;
        assert_eq!(
            signin.status, 204,
            "case {case:?}: expected the admin user's sign-in to answer 204, got {} with {:?}",
            signin.status, signin.body
        );
        let cookie = signin
            .header("set-cookie")
            .unwrap_or_else(|| panic!("case {case:?}: a sign-in must set a session cookie"))
            .split(';')
            .next()
            .unwrap_or_default()
            .to_string();

        // 3. A write of line protocol, authenticated by that session, is accepted.
        let target = format!(
            "/api/v2/write?org={}&bucket={}&precision=ns",
            row.organization, row.bucket
        );
        let write = exchange(
            case,
            &host,
            port,
            "POST",
            &target,
            &[("cookie", cookie.clone())],
            "cpu,host=one value=42i 1700000000000000000",
        )
        .await;
        assert_eq!(
            write.status, 204,
            "case {case:?}: expected a write to answer 204, got {} with {:?}",
            write.status, write.body
        );

        // 4. The point is in the instance's own store, which is what a write MEANS.
        assert_eq!(
            stored_points(&engine, &id),
            vec![("cpu".to_string(), "42".to_string(), 1_700_000_000_000_000_000_i64)],
            "case {case:?}: the written point must be in the instance's own bucket"
        );
    }
}

/// The points the instance `id` holds in its own bucket, as `(measurement, first field
/// value, timestamp)` — read from the store the control plane and the socket share, so what
/// is asserted is state and not an echo.
///
/// `pub(crate)` for the same reason as [`exchange`]: the restore suite reads the data a
/// restored instance serves out of the same store.
pub(crate) fn stored_points(
    engine: &Arc<engine::InMemoryEngine>,
    id: &str,
) -> Vec<(String, String, i64)> {
    let state = influxdb::state::service_state(&**engine).expect("this service's store");
    let store = state.lock();
    let plane = store
        .db_instance_planes
        .get(id)
        .expect("the instance publishes a socket of its own");
    let series = plane.series().lock().expect("the instance's own data");
    series
        .points(&series.credential.bucket)
        .iter()
        .map(|point| {
            (
                point.measurement.clone(),
                point
                    .fields
                    .first()
                    .map(|(_, value)| value.rendered())
                    .unwrap_or_default(),
                point.timestamp_ns,
            )
        })
        .collect()
}

/// One credential row: how a client presents it, and what sign-in and a write answer.
struct CredentialRow {
    case: &'static str,
    headers: Vec<(&'static str, String)>,
    signin_status: u16,
    write_status: u16,
}

#[tokio::test]
async fn a_credential_the_instance_does_not_hold_is_refused_and_writes_nothing() {
    let router = router_and_engine().0;
    let created = ok(
        "setup",
        call(
            &router,
            "CreateDbInstance",
            json!({
                "name": "influx-auth",
                "password": "supersecret1",
                "username": "myadmin",
                "organization": "sim2real",
                "bucket": "metrics",
                "dbInstanceType": "db.influx.medium",
                "allocatedStorage": 20,
                "vpcSubnetIds": ["subnet-aaa"],
                "vpcSecurityGroupIds": ["sg-aaa"],
            }),
        )
        .await,
    );
    let (host, port) = address("setup", &created);
    poll_health("setup", &host, port).await;

    // Rows: the credential forms an InfluxDB client presents, and the ones this instance
    // does not hold. `/api/v2/signin` authenticates with HTTP BASIC authentication alone,
    // as InfluxDB's own sign-in does, so a token credential is refused there and accepted
    // on the write it is a credential for.
    let rows: Vec<CredentialRow> = vec![
        CredentialRow {
            case: "the instance's own admin user by basic authentication",
            headers: vec![("authorization", basic("myadmin", "supersecret1"))],
            signin_status: 204,
            write_status: 204,
        },
        CredentialRow {
            case: "the same user by the v1-compatible token form, which signs in with no \
                   basic credential",
            headers: vec![("authorization", "Token myadmin:supersecret1".to_string())],
            signin_status: 401,
            write_status: 204,
        },
        CredentialRow {
            case: "the right user with another password",
            headers: vec![("authorization", basic("myadmin", "supersecret2"))],
            signin_status: 401,
            write_status: 401,
        },
        CredentialRow {
            case: "a user this instance does not hold",
            headers: vec![("authorization", basic("someone", "supersecret1"))],
            signin_status: 401,
            write_status: 401,
        },
        CredentialRow {
            case: "an API token naming no user, which this emulator mints none of",
            headers: vec![("authorization", "Token an-api-token".to_string())],
            signin_status: 401,
            write_status: 401,
        },
        CredentialRow {
            case: "no credential at all",
            headers: vec![],
            signin_status: 401,
            write_status: 401,
        },
    ];

    for row in rows {
        let case = row.case;
        let headers = row.headers;
        let signin = exchange(
            case,
            &host,
            port,
            "POST",
            "/api/v2/signin",
            &headers,
            "",
        )
        .await;
        assert_eq!(
            signin.status, row.signin_status,
            "case {case:?}: expected sign-in to answer {}, got {} with {:?}",
            row.signin_status, signin.status, signin.body
        );
        let write = exchange(
            case,
            &host,
            port,
            "POST",
            "/api/v2/write?org=sim2real&bucket=metrics",
            &headers,
            "cpu value=1i 1",
        )
        .await;
        assert_eq!(
            write.status, row.write_status,
            "case {case:?}: expected a write to answer {}, got {} with {:?}",
            row.write_status, write.status, write.body
        );
    }
}

#[tokio::test]
async fn two_instances_answer_on_two_addresses_each_out_of_its_own_data() {
    let (router, engine) = router_and_engine();
    let mut published: Vec<(String, String, i64)> = Vec::new();
    for (name, user, org, bucket) in [
        ("influx-one", "oneuser", "oneorg", "onebucket"),
        ("influx-two", "twouser", "twoorg", "twobucket"),
    ] {
        let created = ok(
            name,
            call(
                &router,
                "CreateDbInstance",
                json!({
                    "name": name,
                    "password": "supersecret1",
                    "username": user,
                    "organization": org,
                    "bucket": bucket,
                    "dbInstanceType": "db.influx.medium",
                    "allocatedStorage": 20,
                    "vpcSubnetIds": ["subnet-aaa"],
                    "vpcSecurityGroupIds": ["sg-aaa"],
                }),
            )
            .await,
        );
        let (host, port) = address(name, &created);
        published.push((
            created["id"].as_str().unwrap_or_default().to_string(),
            host,
            port,
        ));
    }
    assert_ne!(
        published[0].2, published[1].2,
        "case \"two instances\": each must publish a port of its own, got {published:?}"
    );

    // A write authenticated as ONE instance's user is refused by the other's socket, and
    // only the instance written to holds the point.
    let (first_id, first_host, first_port) = published[0].clone();
    let (second_id, second_host, second_port) = published[1].clone();
    poll_health("the first instance", &first_host, first_port).await;
    poll_health("the second instance", &second_host, second_port).await;

    let accepted = exchange(
        "a write to the first instance",
        &first_host,
        first_port,
        "POST",
        "/api/v2/write?org=oneorg&bucket=onebucket",
        &[("authorization", basic("oneuser", "supersecret1"))],
        "cpu value=7i 1700000000000000000",
    )
    .await;
    assert_eq!(
        accepted.status, 204,
        "case \"a write to the first instance\": expected 204, got {} with {:?}",
        accepted.status, accepted.body
    );

    let crossed = exchange(
        "the first instance's user on the second instance",
        &second_host,
        second_port,
        "POST",
        "/api/v2/write?org=oneorg&bucket=onebucket",
        &[("authorization", basic("oneuser", "supersecret1"))],
        "cpu value=7i 1700000000000000000",
    )
    .await;
    assert_eq!(
        crossed.status, 401,
        "case \"the first instance's user on the second instance\": expected 401, got {} \
         with {:?}",
        crossed.status, crossed.body
    );

    assert_eq!(
        stored_points(&engine, &first_id).len(),
        1,
        "case \"two instances\": the instance written to must hold the point"
    );
    assert!(
        stored_points(&engine, &second_id).is_empty(),
        "case \"two instances\": the other instance must hold nothing"
    );
}

#[tokio::test]
async fn a_port_change_moves_the_address_and_carries_the_data_with_it() {
    let (router, engine) = router_and_engine();
    let created = ok(
        "setup",
        call(
            &router,
            "CreateDbInstance",
            json!({
                "name": "influx-moving",
                "password": "supersecret1",
                "username": "myadmin",
                "organization": "sim2real",
                "bucket": "metrics",
                "dbInstanceType": "db.influx.medium",
                "allocatedStorage": 20,
                "vpcSubnetIds": ["subnet-aaa"],
                "vpcSecurityGroupIds": ["sg-aaa"],
            }),
        )
        .await,
    );
    let id = created["id"].as_str().unwrap_or_default().to_string();
    let (host, port) = address("setup", &created);
    poll_health("setup", &host, port).await;
    let write = exchange(
        "the point written before the move",
        &host,
        port,
        "POST",
        "/api/v2/write?org=sim2real&bucket=metrics&precision=ns",
        &[("authorization", basic("myadmin", "supersecret1"))],
        "cpu,host=one value=11i 1700000000000000000",
    )
    .await;
    assert_eq!(
        write.status, 204,
        "case \"the point written before the move\": expected 204, got {} with {:?}",
        write.status, write.body
    );

    // Rows: each published bound of the modelled `Port` range and one inside it. Each ask
    // moves the instance onto another socket, so each row asserts against the address the
    // PREVIOUS row left behind.
    let mut previous = port;
    for (case, asked) in [
        ("the published lower bound 1024", 1024),
        ("a port inside the range", 9000),
        ("the published upper bound 65535", 65535),
    ] {
        let updated = ok(
            case,
            call(
                &router,
                "UpdateDbInstance",
                json!({ "identifier": &id, "port": asked }),
            )
            .await,
        );
        let (moved_host, moved_port) = address(case, &updated);
        assert_ne!(
            moved_port, previous,
            "case {case:?}: a port change must move the address the instance publishes"
        );
        assert_eq!(
            moved_host, host,
            "case {case:?}: the host the instance is served on does not move"
        );
        // The new address answers, and the point written before the move is still there:
        // the instance's data moved with it.
        poll_health(case, &moved_host, moved_port).await;
        let before_write = stored_points(&engine, &id);
        assert_eq!(
            before_write.first(),
            Some(&("cpu".to_string(), "11".to_string(), 1_700_000_000_000_000_000_i64)),
            "case {case:?}: the point written before the move must survive it, got \
             {before_write:?}"
        );
        let after = exchange(
            case,
            &moved_host,
            moved_port,
            "POST",
            "/api/v2/write?org=sim2real&bucket=metrics&precision=ns",
            &[("authorization", basic("myadmin", "supersecret1"))],
            "cpu,host=one value=11i 1700000000000000000",
        )
        .await;
        assert_eq!(
            after.status, 204,
            "case {case:?}: the moved address must take a write, got {} with {:?}",
            after.status, after.body
        );
        // The write landed in the SAME bucket the moved instance already held.
        assert_eq!(
            stored_points(&engine, &id).len(),
            before_write.len() + 1,
            "case {case:?}: a write on the moved address must add to the instance's own \
             bucket"
        );
        // And the port it left stops answering.
        let dialled = loopback_port(case, &host, previous);
        let started = std::time::Instant::now();
        loop {
            let reachable = tokio::net::TcpStream::connect(("127.0.0.1", dialled))
                .await
                .is_ok();
            if !reachable {
                break;
            }
            assert!(
                started.elapsed() < DEADLINE,
                "case {case:?}: the port the instance left ({previous}) still accepts \
                 connections"
            );
            tokio::time::sleep(PROBE_INTERVAL).await;
        }
        previous = moved_port;
    }
}

#[tokio::test]
async fn deleting_an_instance_closes_the_port_it_published() {
    let router = router_and_engine().0;
    let created = ok(
        "setup",
        call(
            &router,
            "CreateDbInstance",
            json!({
                "name": "influx-closing",
                "password": "supersecret1",
                "dbInstanceType": "db.influx.medium",
                "allocatedStorage": 20,
                "vpcSubnetIds": ["subnet-aaa"],
                "vpcSecurityGroupIds": ["sg-aaa"],
            }),
        )
        .await,
    );
    let id = created["id"].as_str().unwrap_or_default().to_string();
    let (host, port) = address("setup", &created);
    poll_health("setup", &host, port).await;

    ok(
        "the delete",
        call(&router, "DeleteDbInstance", json!({ "identifier": &id })).await,
    );

    // The port stops answering. Polled rather than probed once, because an in-flight
    // accept may still complete as the listener is torn down.
    let dialled = loopback_port("the deleted instance", &host, port);
    let started = std::time::Instant::now();
    loop {
        let reachable = tokio::net::TcpStream::connect(("127.0.0.1", dialled))
            .await
            .is_ok();
        if !reachable {
            break;
        }
        assert!(
            started.elapsed() < DEADLINE,
            "case \"the deleted instance\": {host}:{port} still accepts connections"
        );
        tokio::time::sleep(PROBE_INTERVAL).await;
    }
}
