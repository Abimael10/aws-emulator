// SPDX-License-Identifier: CC0-1.0

//! The workgroup's CONNECTION ENDPOINT: the `Workgroup$endpoint` a workgroup publishes,
//! and the Redshift sessions a client opens on it.
//!
//! The rule under test: **the endpoint a workgroup publishes names the host this emulator
//! itself serves that workgroup's SQL wire on and the port the workgroup listens on, that
//! pair is an address which answers, and a session opened on it is admitted as the
//! administrator of the namespace the workgroup is the compute of, on the database that
//! namespace was created with, and runs statements on a real engine.** A client that creates
//! a namespace and a workgroup, reads `endpoint.address` / `endpoint.port` and connects with
//! a PostgreSQL driver reaches this emulator; everything else is refused with the protocol's
//! own error.
//!
//! The client here is written against the wire itself (the optional `SSLRequest`, the
//! startup message, the cleartext-password exchange and the simple query protocol), so what
//! these cases drive is the protocol a stock driver (psql, libpq, a JDBC driver) speaks
//! rather than an in-process shortcut — and everything runs in ONE process: the listener is
//! opened by the `CreateWorkgroup` call below, on the core's own loopback interface.
//!
//! A workgroup answers at TWO addresses, and the cases drive both: the pair the control
//! plane PUBLISHES, which a client inside this emulator resolves through the register of
//! dialable planes ([`::service_common::dialable_data_planes`]), and the `host:port` the
//! CORE assigned the listener, which is a real socket on this machine — the core owns the
//! bind interface and the port range, so a service cannot bind the port its resource is
//! configured for.
//!
//! The rows span what the rule generalises over: whether the create names a port (and which
//! documented range it lies in), whether the namespace takes the default administrator and
//! database or names its own, and whether the session is opened in the clear or negotiates
//! TLS in band on the same connection. Neither address is a value a case names: each case
//! reads what the workgroup published and dials exactly that.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_redshiftserverless_redshiftserverless::state;
use smithy_server_runtime::MultiServiceRouter;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use super::namespace_workgroup_lifecycle::at;

/// The admin password every namespace below is created with. A test credential, and the
/// only place in this suite it appears.
const ADMIN_PASSWORD: &str = "Pw9xZqEmulator1";

/// The address the core binds its listeners on, which is therefore the only address a
/// client of this suite dials and the host every workgroup of it publishes: this machine's
/// loopback interface (`vendor/server/src/listeners.rs`, whose bind address is loopback
/// unless an operator chose otherwise).
const LOOPBACK: &str = "127.0.0.1";

/// How long a case waits for a condition it cannot make happen itself — a workgroup
/// reaching `AVAILABLE`, a closed port refusing a connection. Generous next to the spans
/// this service owes, so a loaded runner cannot fail a case, and the wait is on the
/// CONDITION rather than on a clock.
const WAIT_DEADLINE: Duration = Duration::from_secs(120);

/// How often such a wait looks again.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// One emulator with an empty store, and the shared engine it is mounted on — which is
/// what lets a case read the listener the handlers opened out of the store.
struct Emulator {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(
            service_com_amazonaws_redshiftserverless_redshiftserverless::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        Self { router, engine }
    }

    /// Call `operation` with `body` and answer its HTTP status and parsed JSON body.
    async fn call(&self, operation: &str, body: Value) -> (u16, Value) {
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("content-type", "application/x-amz-json-1.1")
            .header("x-amz-target", format!("RedshiftServerless.{operation}"))
            .header(
                "authorization",
                "AWS4-HMAC-SHA256 Credential=AKIDTEST/20260101/us-east-1/redshift-serverless/\
                 aws4_request, SignedHeaders=host, Signature=deadbeef",
            )
            .body(bytes::Bytes::from(
                serde_json::to_vec(&body).expect("the request body serializes"),
            ))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let parsed = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
        (status, parsed)
    }

    /// [`Self::call`] for an operation that must succeed.
    async fn ok(&self, case: &str, operation: &str, body: Value) -> Value {
        let (status, parsed) = self.call(operation, body).await;
        assert_eq!(
            status, 200,
            "case {case}: {operation} expected 200, got {status} with {parsed}"
        );
        parsed
    }

    /// Where the workgroup `name` is SERVED, read out of the store the handlers write: the
    /// address and port of the listener `CreateWorkgroup` opened for it.
    fn served_endpoint(&self, case: &str, name: &str) -> (String, u16) {
        let store = state::service_state(&*self.engine)
            .unwrap_or_else(|| panic!("case {case}: the emulator holds this service's store"));
        let store = store.lock();
        let record = store
            .workgroups
            .get(name)
            .unwrap_or_else(|| panic!("case {case}: the workgroup {name} the create made"));
        let plane = record
            .endpoint_plane
            .as_ref()
            .unwrap_or_else(|| panic!("case {case}: the create opens the workgroup's endpoint"));
        let endpoint = plane.endpoint();
        (
            endpoint.address.clone(),
            u16::try_from(endpoint.port).expect("a listener port fits"),
        )
    }
}

/// Create the namespace `namespace` (with `members` set on it) and the workgroup
/// `workgroup` on it, wait for the workgroup to report `AVAILABLE`, and answer the
/// workgroup as `GetWorkgroup` reports it then.
async fn available_workgroup(
    emulator: &Emulator,
    case: &str,
    namespace: &str,
    workgroup: &str,
    members: Value,
) -> Value {
    let mut request = json!({
        "namespaceName": namespace,
        "adminUserPassword": ADMIN_PASSWORD,
    });
    merge(&mut request, &members, &["adminUsername", "dbName"]);
    emulator.ok(case, "CreateNamespace", request).await;

    let mut request = json!({ "workgroupName": workgroup, "namespaceName": namespace });
    merge(&mut request, &members, &["port"]);
    emulator.ok(case, "CreateWorkgroup", request).await;

    wait_until_available(emulator, case, workgroup).await
}

/// Wait until `workgroup` reports `AVAILABLE` and answer it as `GetWorkgroup` reports it
/// then.
///
/// A create and an update both leave a workgroup in a transient status that settles on its
/// own a moment later, with nobody calling; the wait is on that status rather than on a
/// clock, and only a workgroup that is available admits a session.
async fn wait_until_available(emulator: &Emulator, case: &str, workgroup: &str) -> Value {
    let deadline = Instant::now() + WAIT_DEADLINE;
    loop {
        let read = emulator
            .ok(case, "GetWorkgroup", json!({ "workgroupName": workgroup }))
            .await;
        let workgroup_read = read["workgroup"].clone();
        if workgroup_read["status"] == json!("AVAILABLE") {
            return workgroup_read;
        }
        assert!(
            Instant::now() < deadline,
            "case {case}: the workgroup {workgroup} never reported AVAILABLE, last status {:?}",
            workgroup_read["status"]
        );
        ::tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Copy the members of `members` that `keys` names onto `request`.
fn merge(request: &mut Value, members: &Value, keys: &[&str]) {
    for key in keys {
        if let Some(value) = members.get(*key) {
            if let Some(object) = request.as_object_mut() {
                object.insert((*key).to_string(), value.clone());
            }
        }
    }
}

/// One endpoint case: the workgroup it creates, the namespace members it creates it on,
/// how its session is opened, and the administrator, database and published port that
/// session uses.
struct Row {
    case: &'static str,
    namespace: &'static str,
    workgroup: &'static str,
    /// The create members beyond the required ones: `adminUsername` / `dbName` go to the
    /// namespace, `port` to the workgroup.
    members: Value,
    transport: Transport,
    admin_username: &'static str,
    database: &'static str,
    /// The port the workgroup must PUBLISH: the one the create named, or the documented
    /// default when it named none.
    published_port: i64,
}

/// How a session is opened: in the clear, or with the PostgreSQL in-band TLS upgrade on
/// the same port — which is what `sslmode=require` does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Transport {
    Plaintext,
    Tls,
}

/// The endpoint cases.
///
/// The published ports are the ones `Workgroup$port` documents: "Valid port ranges are
/// 5431-5455 and 8191-8215. The default is 5439."
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the default administrator and database, a session in the clear",
            namespace: "wire-default-ns",
            workgroup: "wire-default-wg",
            members: json!({}),
            transport: Transport::Plaintext,
            // The namespace defaults: "the username of the administrator for the first
            // database created in the namespace" and "the name of the first database
            // created in the namespace".
            admin_username: state::DEFAULT_ADMIN_USERNAME,
            database: state::DEFAULT_DB_NAME,
            published_port: i64::from(state::DEFAULT_PORT),
        },
        Row {
            case: "the default administrator and database, TLS negotiated in band",
            namespace: "wire-tls-ns",
            workgroup: "wire-tls-wg",
            members: json!({}),
            transport: Transport::Tls,
            admin_username: state::DEFAULT_ADMIN_USERNAME,
            database: state::DEFAULT_DB_NAME,
            published_port: i64::from(state::DEFAULT_PORT),
        },
        Row {
            case: "a namespace with its own administrator and database, a port in the low range",
            namespace: "wire-named-ns",
            workgroup: "wire-named-wg",
            members: json!({ "adminUsername": "warehouse", "dbName": "analytics", "port": 5431 }),
            transport: Transport::Plaintext,
            admin_username: "warehouse",
            database: "analytics",
            published_port: 5431,
        },
        Row {
            case: "a namespace with its own database, a port in the high range, TLS in band",
            namespace: "wire-high-ns",
            workgroup: "wire-high-wg",
            members: json!({ "dbName": "reporting", "port": 8215 }),
            transport: Transport::Tls,
            admin_username: state::DEFAULT_ADMIN_USERNAME,
            database: "reporting",
            published_port: 8215,
        },
    ]
}

/// RULE: the endpoint a workgroup publishes names the host this emulator serves that
/// workgroup's SQL wire on and the port the workgroup listens on — every reader of the
/// workgroup reports the same pair — and a session opened on that pair as the namespace's
/// administrator runs statements on the workgroup's own engine.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_workgroup_publishes_the_endpoint_this_emulator_serves_it_on() {
    for row in rows() {
        let case = row.case;
        let emulator = Emulator::new();
        let workgroup = available_workgroup(
            &emulator,
            case,
            row.namespace,
            row.workgroup,
            row.members.clone(),
        )
        .await;

        // 1. What the endpoint PUBLISHES: the host of the listener the create opened —
        //    never a public AWS DNS name this emulator does not answer at — and the port
        //    the workgroup listens on.
        let address = at(&workgroup, "endpoint.address")
            .as_str()
            .unwrap_or_default()
            .to_string();
        let port = at(&workgroup, "endpoint.port")
            .as_i64()
            .and_then(|port| u16::try_from(port).ok())
            .unwrap_or_default();
        let (served_address, served_port) = emulator.served_endpoint(case, row.workgroup);
        assert_eq!(
            address, served_address,
            "case {case}: expected the host this emulator serves the workgroup on \
             ({served_address}), got {address}"
        );
        assert!(
            !address.ends_with(".amazonaws.com"),
            "case {case}: the endpoint must name a host this emulator answers on, got {address}"
        );
        assert_eq!(
            at(&workgroup, "endpoint.port"),
            &json!(row.published_port),
            "case {case}: expected the port the workgroup listens on ({}), got {:?}",
            row.published_port,
            at(&workgroup, "endpoint.port")
        );

        // 2. Every reader of the workgroup reports that same endpoint: it is STORED.
        let listed = emulator.ok(case, "ListWorkgroups", json!({})).await;
        let entry = listed["workgroups"]
            .as_array()
            .and_then(|workgroups| {
                workgroups
                    .iter()
                    .find(|entry| entry["workgroupName"] == json!(row.workgroup))
                    .cloned()
            })
            .unwrap_or_else(|| panic!("case {case}: ListWorkgroups must report {}", row.workgroup));
        assert_eq!(
            at(&entry, "endpoint"),
            at(&workgroup, "endpoint"),
            "case {case}: expected ListWorkgroups to report the stored endpoint {:?}, got {:?}",
            at(&workgroup, "endpoint"),
            at(&entry, "endpoint")
        );

        // 3. A session opened on the PUBLISHED pair is admitted as the namespace's
        //    administrator, on the database the namespace was created with.
        let mut session = postgres_login(
            case,
            dial_published(case, &emulator, &address, port).await,
            row.transport,
            row.admin_username,
            ADMIN_PASSWORD,
            Some(row.database),
        )
        .await
        .unwrap_or_else(|refusal| {
            panic!("case {case}: the namespace's administrator must be admitted, got {refusal:?}")
        });
        assert_eq!(
            session.parameter("server_version").as_deref(),
            Some(state::workgroup_data_plane::WORKGROUP_SERVER_VERSION),
            "case {case}: expected the backend to report the version the workgroup serves, got \
             {:?}",
            session.parameter("server_version")
        );

        // 4. CREATE TABLE / INSERT / SELECT, the round trip a warehouse is provisioned
        //    for, plus values the ENGINE computes rather than echoes.
        for statement in [
            "CREATE TABLE endpoint_roundtrip (id INT PRIMARY KEY, val VARCHAR(64))",
            "INSERT INTO endpoint_roundtrip VALUES (1, 'hello-serverless')",
            "INSERT INTO endpoint_roundtrip VALUES (2, 'second-row')",
        ] {
            session.query(statement).await.unwrap_or_else(|refusal| {
                panic!("case {case}: {statement} must run, got {refusal:?}")
            });
        }
        let answer = session
            .query("SELECT val FROM endpoint_roundtrip WHERE id = 1")
            .await
            .unwrap_or_else(|refusal| panic!("case {case}: the SELECT must run, got {refusal:?}"));
        assert_eq!(
            answer.only(),
            Some("hello-serverless"),
            "case {case}: expected the row the INSERT wrote, got {answer:?}"
        );
        let counted = session
            .query("SELECT COUNT(*) FROM endpoint_roundtrip")
            .await
            .unwrap_or_else(|refusal| {
                panic!("case {case}: the aggregate must run, got {refusal:?}")
            });
        assert_eq!(
            counted.only(),
            Some("2"),
            "case {case}: expected the engine's own count of the rows, got {counted:?}"
        );
        let named = session
            .query("SELECT current_database()")
            .await
            .unwrap_or_else(|refusal| {
                panic!("case {case}: current_database() must run, got {refusal:?}")
            });
        assert_eq!(
            named.only(),
            Some(row.database),
            "case {case}: expected the database the namespace was created with, got {named:?}"
        );

        // 5. The address the CORE assigned the listener answers the same plane over a real
        //    socket: a client that dials it sees the rows written through the published
        //    pair above.
        let mut on_the_socket = postgres_login(
            case,
            connect_tcp(case, &served_address, served_port).await,
            row.transport,
            row.admin_username,
            ADMIN_PASSWORD,
            Some(row.database),
        )
        .await
        .unwrap_or_else(|refusal| {
            panic!("case {case}: the listener's own address must admit the session, got {refusal:?}")
        });
        let counted = on_the_socket
            .query("SELECT COUNT(*) FROM endpoint_roundtrip")
            .await
            .unwrap_or_else(|refusal| {
                panic!("case {case}: the aggregate must run on the socket, got {refusal:?}")
            });
        assert_eq!(
            counted.only(),
            Some("2"),
            "case {case}: both addresses serve ONE workgroup's engine, got {counted:?}"
        );
    }
}

/// RULE: only the namespace's own administrator credentials are admitted on a workgroup's
/// endpoint, and only on the database the namespace holds; every other login is refused
/// with the protocol's own error for it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn only_the_namespaces_own_credentials_are_admitted() {
    let emulator = Emulator::new();
    let workgroup = available_workgroup(
        &emulator,
        "refusals",
        "wire-refusals-ns",
        "wire-refusals-wg",
        json!({}),
    )
    .await;
    let address = at(&workgroup, "endpoint.address")
        .as_str()
        .unwrap_or_default()
        .to_string();
    let port = at(&workgroup, "endpoint.port")
        .as_i64()
        .and_then(|port| u16::try_from(port).ok())
        .unwrap_or_default();

    // (case, the user, the password, the database asked for, the SQLSTATE the protocol
    // refuses it with)
    let cases: Vec<(&str, &str, &str, Option<&str>, &str)> = vec![
        (
            "the administrator with the wrong password",
            state::DEFAULT_ADMIN_USERNAME,
            "not-the-password",
            Some(state::DEFAULT_DB_NAME),
            "28P01",
        ),
        (
            "a user the namespace does not authenticate",
            "someone-else",
            ADMIN_PASSWORD,
            Some(state::DEFAULT_DB_NAME),
            "28P01",
        ),
        (
            "the administrator on a database the namespace does not hold",
            state::DEFAULT_ADMIN_USERNAME,
            ADMIN_PASSWORD,
            Some("no-such-database"),
            "3D000",
        ),
    ];
    for (case, user, password, database, code) in cases {
        let refusal = postgres_login(
            case,
            dial_published(case, &emulator, &address, port).await,
            Transport::Plaintext,
            user,
            password,
            database,
        )
        .await
        .err()
        .unwrap_or_else(|| panic!("case {case}: expected the login to be refused"));
        assert_eq!(
            refusal.code, code,
            "case {case}: expected the PostgreSQL SQLSTATE {code}, got {refusal:?}"
        );
    }

    // A login that names NO database is served the namespace's own, which is what a client
    // that leaves the database to its driver gets.
    let mut session = postgres_login(
        "no database named",
        dial_published("no database named", &emulator, &address, port).await,
        Transport::Plaintext,
        state::DEFAULT_ADMIN_USERNAME,
        ADMIN_PASSWORD,
        None,
    )
    .await
    .unwrap_or_else(|refusal| {
        panic!("case no database named: expected the administrator to be admitted, got {refusal:?}")
    });
    let named = session
        .query("SELECT current_database()")
        .await
        .unwrap_or_else(|refusal| {
            panic!("case no database named: current_database() must run, got {refusal:?}")
        });
    assert_eq!(
        named.only(),
        Some(state::DEFAULT_DB_NAME),
        "case no database named: expected the namespace's first database, got {named:?}"
    );
}

/// RULE: the endpoint belongs to the workgroup — two workgroups answer on two addresses,
/// each with its own engine, and deleting a workgroup closes the port it answered on.
/// RULE: the endpoint belongs to the workgroup — two workgroups answer on two addresses,
/// each with its own engine, and deleting a workgroup closes the port it answered on and
/// withdraws the address it published.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn each_workgroup_serves_its_own_endpoint_and_a_delete_closes_it() {
    let emulator = Emulator::new();
    // A namespace carries ONE workgroup, so each workgroup of this case has its own; the
    // two are configured on different ports, which is what two servers on one host are.
    let first = available_workgroup(
        &emulator,
        "two workgroups",
        "wire-first-ns",
        "wire-first-wg",
        json!({ "port": 5431 }),
    )
    .await;
    let second = available_workgroup(
        &emulator,
        "two workgroups",
        "wire-second-ns",
        "wire-second-wg",
        json!({ "dbName": "second", "port": 8191 }),
    )
    .await;
    let endpoint_of = |workgroup: &Value| {
        (
            at(workgroup, "endpoint.address")
                .as_str()
                .unwrap_or_default()
                .to_string(),
            at(workgroup, "endpoint.port")
                .as_i64()
                .and_then(|port| u16::try_from(port).ok())
                .unwrap_or_default(),
        )
    };
    let (first_address, first_port) = endpoint_of(&first);
    let (second_address, second_port) = endpoint_of(&second);
    assert_ne!(
        (first_address.as_str(), first_port),
        (second_address.as_str(), second_port),
        "case two workgroups: each workgroup answers on its own endpoint, got \
         {first_address}:{first_port} twice"
    );
    let (first_served_address, first_served_port) =
        emulator.served_endpoint("two workgroups", "wire-first-wg");
    let (second_served_address, second_served_port) =
        emulator.served_endpoint("two workgroups", "wire-second-wg");
    // The core binds both listeners on this machine's loopback interface, which is the
    // address dialled below.
    assert_eq!(
        first_served_address, LOOPBACK,
        "case two workgroups: the core serves the workgroup on this machine's loopback \
         address, got {first_served_address:?}"
    );

    // Each endpoint is its own engine: a table written through one is not on the other.
    let mut session = postgres_login(
        "two workgroups",
        dial_published("two workgroups", &emulator, &first_address, first_port).await,
        Transport::Plaintext,
        state::DEFAULT_ADMIN_USERNAME,
        ADMIN_PASSWORD,
        Some(state::DEFAULT_DB_NAME),
    )
    .await
    .unwrap_or_else(|refusal| {
        panic!("case two workgroups: the first workgroup must admit its administrator, got {refusal:?}")
    });
    session
        .query("CREATE TABLE only_on_the_first (id INT)")
        .await
        .unwrap_or_else(|refusal| {
            panic!("case two workgroups: the CREATE must run, got {refusal:?}")
        });
    let mut other = postgres_login(
        "two workgroups",
        dial_published("two workgroups", &emulator, &second_address, second_port).await,
        Transport::Plaintext,
        state::DEFAULT_ADMIN_USERNAME,
        ADMIN_PASSWORD,
        Some("second"),
    )
    .await
    .unwrap_or_else(|refusal| {
        panic!("case two workgroups: the second workgroup must admit its administrator, got {refusal:?}")
    });
    let refusal = other
        .query("SELECT * FROM only_on_the_first")
        .await
        .err()
        .unwrap_or_else(|| {
            panic!("case two workgroups: the other workgroup must not hold the first's table")
        });
    assert!(
        !refusal.code.is_empty(),
        "case two workgroups: expected the engine's own refusal, got {refusal:?}"
    );

    // Deleting the workgroup closes the port it answered on and withdraws the address it
    // published: the listener IS the resource.
    let _ = emulator
        .ok(
            "delete",
            "DeleteWorkgroup",
            json!({ "workgroupName": "wire-first-wg" }),
        )
        .await;
    let deadline = Instant::now() + WAIT_DEADLINE;
    loop {
        let reached = TcpStream::connect(("127.0.0.1", first_served_port)).await;
        if reached.is_err() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "case delete: the deleted workgroup's port {first_served_port} must stop answering"
        );
        ::tokio::time::sleep(POLL_INTERVAL).await;
    }
    assert!(
        published_plane(&emulator, &first_address, first_port).is_none(),
        "case delete: the deleted workgroup's published address \
         {first_address}:{first_port} must answer nothing"
    );
    // The other workgroup is untouched: its socket still answers, and so does the address
    // it published.
    assert_eq!(
        second_served_address, LOOPBACK,
        "case delete: the core serves the workgroup on this machine's loopback address, got \
         {second_served_address:?}"
    );
    TcpStream::connect(("127.0.0.1", second_served_port))
        .await
        .unwrap_or_else(|error| {
            panic!(
                "case delete: the other workgroup's listener \
                 {second_served_address}:{second_served_port} must still answer, got {error}"
            )
        });
    assert!(
        published_plane(&emulator, &second_address, second_port).is_some(),
        "case delete: the other workgroup's published address must still answer"
    );
}

/// RULE: a change to the port a workgroup listens on moves what it publishes AND the
/// address that answers for it — the workgroup is reached at the pair it reports now, and
/// at nothing it used to report.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_port_change_moves_the_published_endpoint_and_what_answers_on_it() {
    // The rows span the documented ranges a port may move between: `Workgroup$port` —
    // "Valid port ranges are 5431-5455 and 8191-8215. The default is 5439."
    for (case, created_port, updated_port) in [
        ("within the low range", 5431_i64, 5455_i64),
        ("from the low range to the high one", 5439, 8191),
        ("from the high range to the low one", 8215, 5440),
    ] {
        let emulator = Emulator::new();
        let workgroup = available_workgroup(
            &emulator,
            case,
            "wire-moved-ns",
            "wire-moved-wg",
            json!({ "port": created_port }),
        )
        .await;
        let address = at(&workgroup, "endpoint.address")
            .as_str()
            .unwrap_or_default()
            .to_string();
        let before = u16::try_from(created_port).expect("a documented port fits");
        assert!(
            published_plane(&emulator, &address, before).is_some(),
            "case {case}: the created workgroup must answer at {address}:{before}"
        );

        let updated = emulator
            .ok(
                case,
                "UpdateWorkgroup",
                json!({ "workgroupName": "wire-moved-wg", "port": updated_port }),
            )
            .await;
        assert_eq!(
            at(&updated["workgroup"], "endpoint.port"),
            &json!(updated_port),
            "case {case}: the endpoint must report the port the workgroup listens on now, got \
             {:?}",
            at(&updated["workgroup"], "endpoint.port")
        );
        assert_eq!(
            at(&updated["workgroup"], "endpoint.address"),
            &json!(address),
            "case {case}: the host serving the workgroup is unchanged, got {:?}",
            at(&updated["workgroup"], "endpoint.address")
        );

        // The pair it publishes NOW answers, and the one it published before does not. An
        // update leaves the workgroup MODIFYING for a moment, and only an available
        // workgroup admits a session.
        let after = u16::try_from(updated_port).expect("a documented port fits");
        let settled = wait_until_available(&emulator, case, "wire-moved-wg").await;
        assert_eq!(
            at(&settled, "endpoint.port"),
            &json!(updated_port),
            "case {case}: GetWorkgroup must report the moved endpoint, got {:?}",
            at(&settled, "endpoint.port")
        );
        let mut session = postgres_login(
            case,
            dial_published(case, &emulator, &address, after).await,
            Transport::Plaintext,
            state::DEFAULT_ADMIN_USERNAME,
            ADMIN_PASSWORD,
            Some(state::DEFAULT_DB_NAME),
        )
        .await
        .unwrap_or_else(|refusal| {
            panic!("case {case}: the moved endpoint must admit the administrator, got {refusal:?}")
        });
        let answered = session
            .query("SELECT 1")
            .await
            .unwrap_or_else(|refusal| panic!("case {case}: SELECT 1 must run, got {refusal:?}"));
        assert_eq!(
            answered.only(),
            Some("1"),
            "case {case}: expected the engine to answer the statement, got {answered:?}"
        );
        assert!(
            published_plane(&emulator, &address, before).is_none(),
            "case {case}: the port the workgroup no longer listens on must answer nothing, \
             {address}:{before} still does"
        );
    }
}

// ---------------------------------------------------------------------------
// The PostgreSQL client
// ---------------------------------------------------------------------------

/// A connection to a workgroup's endpoint, in the clear or upgraded to TLS.
trait Connection: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send {}
impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send> Connection for T {}

/// What a client holds: the stream, whatever it is now.
type Client = Box<dyn Connection>;

/// What answers at the address `host`:`port` for a client of this emulator, or `None` when
/// nothing does.
///
/// The register of the shared engine ([`::service_common::dialable_data_planes`]): what a
/// client running INSIDE this emulator resolves the address a control plane handed it
/// against. The published endpoint of a workgroup has to be in it, which is what makes the
/// pair the control plane reports an address that answers.
fn published_plane(
    emulator: &Emulator,
    host: &str,
    port: u16,
) -> Option<::service_common::dialable_data_planes::Dialed> {
    ::service_common::dialable_data_planes::DialablePlanes::of(&*emulator.engine)
        .and_then(|planes| planes.dialable(host, port))
}

/// A connection to the address a workgroup PUBLISHED, opened the way a client inside this
/// emulator opens one: through the register above, which answers with the very handler the
/// workgroup's listener serves.
async fn dial_published(case: &str, emulator: &Emulator, host: &str, port: u16) -> Client {
    let dialed = published_plane(emulator, host, port).unwrap_or_else(|| {
        panic!("case {case}: the published endpoint {host}:{port} must be an address that answers")
    });
    let conn = dialed.plane.dial(None).await.unwrap_or_else(|error| {
        panic!("case {case}: the published endpoint {host}:{port} must accept a connection, got {error}")
    });
    Box::new(conn)
}

/// A connection to the address the CORE assigned the workgroup's listener, opened over a
/// real socket — the way a client on this machine reaches it.
///
/// The core binds its listeners on this machine's loopback interface ([`LOOPBACK`]), which
/// is asserted here and then dialled as that literal: this suite reaches no other host.
async fn connect_tcp(case: &str, host: &str, port: u16) -> Client {
    assert_eq!(
        host, LOOPBACK,
        "case {case}: the listener must be bound on this machine's loopback address, got \
         {host:?}"
    );
    let stream = TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap_or_else(|error| {
            panic!("case {case}: the listener {host}:{port} must accept a connection, got {error}")
        });
    Box::new(stream)
}

/// A refusal the server sent: the SQLSTATE a client branches on and the sentence it
/// reports.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Refusal {
    code: String,
    message: String,
}

/// One result set a client read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Rows {
    columns: Vec<String>,
    rows: Vec<Vec<Option<String>>>,
}

impl Rows {
    /// The one value a single-row, single-column answer carries.
    fn only(&self) -> Option<&str> {
        self.rows
            .first()
            .and_then(|row| row.first())
            .and_then(Option::as_deref)
    }
}

/// A PostgreSQL session a client has opened, and the parameters the backend reported for
/// it.
struct PostgresSession {
    conn: Client,
    parameters: Vec<(String, String)>,
}

impl PostgresSession {
    /// The value the backend reported for the parameter `name`.
    fn parameter(&self, name: &str) -> Option<String> {
        self.parameters
            .iter()
            .find(|(held, _)| held == name)
            .map(|(_, value)| value.clone())
    }

    /// Run one simple query and read everything the server answers for it.
    async fn query(&mut self, sql: &str) -> Result<Rows, Refusal> {
        let case = "a statement";
        let mut message = Vec::new();
        put_cstring(&mut message, sql);
        postgres_send(case, &mut self.conn, b'Q', &message).await;
        let mut answer = Rows::default();
        let mut refusal = None;
        loop {
            let Some((kind, body)) = postgres_try_read(&mut self.conn).await else {
                return Err(refusal.unwrap_or(Refusal {
                    code: "08006".to_string(),
                    message: "the server closed the connection".to_string(),
                }));
            };
            match kind {
                // RowDescription.
                b'T' => {
                    let count = usize::from(u16::from_be_bytes([body[0], body[1]]));
                    let mut cursor = 2usize;
                    answer.columns.clear();
                    for _ in 0..count {
                        let (name, next) = read_cstring(&body, cursor);
                        answer.columns.push(name);
                        // name, table oid, column number, type oid, size, modifier,
                        // format code.
                        cursor = next + 18;
                    }
                }
                // DataRow.
                b'D' => {
                    let count = usize::from(u16::from_be_bytes([body[0], body[1]]));
                    let mut cursor = 2usize;
                    let mut row = Vec::with_capacity(count);
                    for _ in 0..count {
                        let length = i32::from_be_bytes([
                            body[cursor],
                            body[cursor + 1],
                            body[cursor + 2],
                            body[cursor + 3],
                        ]);
                        cursor += 4;
                        if length < 0 {
                            row.push(None);
                            continue;
                        }
                        let length = usize::try_from(length).expect("a value length fits");
                        row.push(Some(
                            String::from_utf8_lossy(&body[cursor..cursor + length]).into_owned(),
                        ));
                        cursor += length;
                    }
                    answer.rows.push(row);
                }
                b'E' => refusal = Some(postgres_refusal(&body)),
                // ReadyForQuery ends the exchange.
                b'Z' => break,
                // CommandComplete, EmptyQueryResponse, NoticeResponse.
                _ => {}
            }
        }
        match refusal {
            Some(refusal) => Err(refusal),
            None => Ok(answer),
        }
    }
}

/// Open a PostgreSQL session on `conn`, as libpq does: the optional `SSLRequest` first,
/// then the startup message, the password, and the parameters the server reports.
async fn postgres_login(
    case: &str,
    conn: Client,
    transport: Transport,
    user: &str,
    password: &str,
    database: Option<&str>,
) -> Result<PostgresSession, Refusal> {
    let mut conn = conn;
    if transport == Transport::Tls {
        let mut request = Vec::new();
        request.extend_from_slice(&8i32.to_be_bytes());
        // The `SSLRequest` code, sent in the clear as the connection's first bytes —
        // which is what `sslmode=require` sends.
        request.extend_from_slice(&80_877_103i32.to_be_bytes());
        conn.write_all(&request)
            .await
            .unwrap_or_else(|error| panic!("case {case}: the request is sent, got {error}"));
        let mut answer = [0u8; 1];
        conn.read_exact(&mut answer)
            .await
            .unwrap_or_else(|error| panic!("case {case}: the server answers, got {error}"));
        assert_eq!(
            answer[0], b'S',
            "case {case}: the server must answer an SSLRequest with 'S' before any TLS"
        );
        conn = upgrade_to_tls(case, conn).await;
    }
    let mut body = Vec::new();
    // Protocol version 3.0.
    body.extend_from_slice(&196_608i32.to_be_bytes());
    put_cstring(&mut body, "user");
    put_cstring(&mut body, user);
    if let Some(database) = database {
        put_cstring(&mut body, "database");
        put_cstring(&mut body, database);
    }
    body.push(0);
    let mut startup = Vec::new();
    startup.extend_from_slice(
        &i32::try_from(body.len() + 4)
            .expect("the startup message fits")
            .to_be_bytes(),
    );
    startup.extend_from_slice(&body);
    conn.write_all(&startup)
        .await
        .unwrap_or_else(|error| panic!("case {case}: the startup is sent, got {error}"));

    let mut parameters = Vec::new();
    loop {
        let (kind, body) = postgres_try_read(&mut conn)
            .await
            .unwrap_or_else(|| panic!("case {case}: the server answers the startup"));
        match kind {
            // AuthenticationRequest.
            b'R' => {
                let code = i32::from_be_bytes([body[0], body[1], body[2], body[3]]);
                if code == 0 {
                    continue;
                }
                assert_eq!(
                    code, 3,
                    "case {case}: this endpoint asks for a cleartext password, got the \
                     authentication request {code}"
                );
                let mut message = Vec::new();
                put_cstring(&mut message, password);
                postgres_send(case, &mut conn, b'p', &message).await;
            }
            b'E' => return Err(postgres_refusal(&body)),
            // ParameterStatus.
            b'S' => {
                let (name, next) = read_cstring(&body, 0);
                let (value, _) = read_cstring(&body, next);
                parameters.push((name, value));
            }
            // ReadyForQuery: the session is open.
            b'Z' => return Ok(PostgresSession { conn, parameters }),
            // BackendKeyData, NoticeResponse.
            _ => {}
        }
    }
}

/// Run the client side of a TLS handshake on `conn`.
///
/// The verifier accepts the certificate the EMULATOR'S OWN core generated for its
/// listeners, which is self-signed and names loopback: this client is the emulator's own
/// test, connecting to a port the same process opened, so there is no authority to check it
/// against.
async fn upgrade_to_tls(case: &str, conn: Client) -> Client {
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(Arc::clone(&provider))
        .with_safe_default_protocol_versions()
        .expect("the provider supports the default protocol versions")
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptTheEmulatorsCertificate(provider)))
        .with_no_client_auth();
    let connector = tokio_rustls::TlsConnector::from(Arc::new(config));
    let server_name =
        rustls::pki_types::ServerName::try_from("localhost").expect("localhost is a server name");
    let stream = connector
        .connect(server_name, conn)
        .await
        .unwrap_or_else(|error| {
            panic!("case {case}: the server must complete the in-band TLS handshake, got {error}")
        });
    Box::new(stream)
}

/// See [`upgrade_to_tls`].
#[derive(Debug)]
struct AcceptTheEmulatorsCertificate(Arc<rustls::crypto::CryptoProvider>);

impl rustls::client::danger::ServerCertVerifier for AcceptTheEmulatorsCertificate {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

/// Write one PostgreSQL message.
async fn postgres_send(case: &str, conn: &mut Client, kind: u8, body: &[u8]) {
    let mut message = vec![kind];
    message.extend_from_slice(
        &i32::try_from(body.len() + 4)
            .expect("the message fits")
            .to_be_bytes(),
    );
    message.extend_from_slice(body);
    conn.write_all(&message)
        .await
        .unwrap_or_else(|error| panic!("case {case}: the message is sent, got {error}"));
}

/// Read one PostgreSQL message, or `None` where the server closed the connection.
async fn postgres_try_read(conn: &mut Client) -> Option<(u8, Vec<u8>)> {
    let mut header = [0u8; 5];
    conn.read_exact(&mut header).await.ok()?;
    let length = i32::from_be_bytes([header[1], header[2], header[3], header[4]]);
    let body_length = usize::try_from(length - 4).expect("a message length fits");
    let mut body = vec![0u8; body_length];
    conn.read_exact(&mut body).await.ok()?;
    Some((header[0], body))
}

/// The SQLSTATE and the message an `ErrorResponse` carries.
fn postgres_refusal(body: &[u8]) -> Refusal {
    let mut refusal = Refusal {
        code: String::new(),
        message: String::new(),
    };
    let mut cursor = 0usize;
    while cursor < body.len() && body[cursor] != 0 {
        let field = body[cursor];
        let (value, next) = read_cstring(body, cursor + 1);
        match field {
            b'C' => refusal.code = value,
            b'M' => refusal.message = value,
            _ => {}
        }
        cursor = next;
    }
    refusal
}

/// The NUL-terminated string at `cursor`, and the index after it.
fn read_cstring(payload: &[u8], cursor: usize) -> (String, usize) {
    let rest = &payload[cursor..];
    let end = rest.iter().position(|byte| *byte == 0).unwrap_or(rest.len());
    (
        String::from_utf8_lossy(&rest[..end]).into_owned(),
        cursor + end + 1,
    )
}

/// Append a NUL-terminated string.
fn put_cstring(payload: &mut Vec<u8>, text: &str) {
    payload.extend_from_slice(text.as_bytes());
    payload.push(0);
}
