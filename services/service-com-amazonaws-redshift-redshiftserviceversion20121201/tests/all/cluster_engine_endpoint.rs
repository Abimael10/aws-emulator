// SPDX-License-Identifier: CC0-1.0

//! The provisioned cluster's ENGINE ENDPOINT: the `Cluster$Endpoint` a cluster
//! publishes, and the PostgreSQL sessions a client opens on it.
//!
//! The rule under test: **the endpoint a cluster publishes once it is serving is
//! an address this emulator answers on and a port where that cluster's own
//! database engine is served — and a session on it is admitted as the cluster's
//! admin user with its current password, or with a temporary credential
//! `GetClusterCredentials` minted for that database, until that credential's
//! `Expiration`.** A client that creates a cluster, waits for `available`, mints
//! credentials and connects to the address the control plane handed it reaches
//! this emulator; everything else is refused with the protocol's own error.
//!
//! The client here is written against the wire itself (the optional
//! `SSLRequest`, the startup message, the cleartext-password exchange and the
//! simple query protocol), so what these tests drive is the protocol a stock
//! driver (psql, libpq, psycopg, a JDBC driver) speaks rather than an in-process
//! shortcut — and everything runs in ONE process: the listeners are opened by
//! the control-plane calls below, on the core's own loopback interface.
//!
//! The rows span what the rule generalises over: the cluster's shape
//! (single-node and multi-node), whether the request names the port, the
//! database and the Availability Zone or takes the defaults, a session in the
//! clear and one that negotiates TLS in band on the same port, every lifetime
//! the documented window admits and each bound past it, both `AutoCreate`
//! prefixes a credential is reported under, and the credential, database and
//! lifecycle cases a login is refused for.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use service_com_amazonaws_redshift_redshiftserviceversion20121201::state;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::cluster_lifecycle::{Answer, Emulator, POLL_INTERVAL};

/// The admin user every cluster below is created with.
const MASTER_USER: &str = "adminuser";

/// Its password. A test credential, and the only place in this suite it
/// appears.
const MASTER_PASSWORD: &str = "Pw9xZqEmulator1";

/// The address every published endpoint carries: this emulator's own loopback
/// interface, which is where the core binds its listeners and so the only
/// address a client of this suite can reach.
const LOOPBACK: &str = "127.0.0.1";

/// How long a case waits for a condition it cannot make happen itself — a
/// cluster reaching a status, a closed port refusing a connection. Generous next
/// to the spans this service owes, so a loaded runner cannot fail a case, and
/// the wait is on the CONDITION rather than on a clock.
const WAIT_DEADLINE: Duration = Duration::from_secs(120);

/// How far past (issuance + the lifetime asked for) a reported `Expiration` may
/// fall: the time the issuing call itself may take on a loaded host, and far
/// short of the shortest lifetime the documented window admits (900 s), so an
/// expiry that is wrong by minutes cannot pass.
const ISSUANCE_SLACK_SECONDS: i64 = 60;

/// How a session is opened: in the clear, or with the PostgreSQL in-band TLS
/// upgrade on the same port — which is what `sslmode=require` does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Transport {
    Plaintext,
    Tls,
}

/// One endpoint case: the cluster the case creates, how its session is opened,
/// and the database that session uses.
struct Row {
    case: &'static str,
    identifier: &'static str,
    /// The create members beyond the three required ones.
    params: Vec<(&'static str, &'static str)>,
    transport: Transport,
    /// The database the cluster was created with, which is the one a session
    /// uses.
    database: &'static str,
}

/// The endpoint cases.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a single-node cluster on the default port, a session in the clear",
            identifier: "wire-single-plain",
            params: vec![("ClusterType", "single-node")],
            transport: Transport::Plaintext,
            database: state::DEFAULT_DB_NAME,
        },
        Row {
            case: "a single-node cluster, TLS negotiated in band",
            identifier: "wire-single-tls",
            params: vec![("ClusterType", "single-node")],
            transport: Transport::Tls,
            database: state::DEFAULT_DB_NAME,
        },
        Row {
            case: "a multi-node cluster whose create names a port, a session in the clear",
            identifier: "wire-multi-ported",
            params: vec![
                ("ClusterType", "multi-node"),
                ("NumberOfNodes", "2"),
                ("Port", "5455"),
            ],
            transport: Transport::Plaintext,
            database: state::DEFAULT_DB_NAME,
        },
        Row {
            case: "a cluster created with its own database name and zone, TLS negotiated in band",
            identifier: "wire-named-db",
            params: vec![
                ("ClusterType", "single-node"),
                ("DBName", "warehouse"),
                ("AvailabilityZone", "us-west-2c"),
            ],
            transport: Transport::Tls,
            database: "warehouse",
        },
    ]
}

/// RULE: the endpoint a serving cluster publishes is an address this emulator
/// answers on where that cluster's database engine is served, and a client that
/// dials it round-trips SQL against the cluster's own storage.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_published_endpoint_serves_the_clusters_database() {
    let emulator = Emulator::new();
    for row in rows() {
        let case = row.case;
        let described = available_cluster(&emulator, row.identifier, &row.params).await;
        let (address, port) = endpoint_of(case, &described);

        let mut session = postgres_login(
            case,
            &address,
            port,
            row.transport,
            MASTER_USER,
            MASTER_PASSWORD,
            Some(row.database),
        )
        .await
        .unwrap_or_else(|refusal| {
            panic!("case {case}: the admin user must be admitted, got {refusal:?}")
        });
        assert_eq!(
            session.parameter("server_version").as_deref(),
            Some(state::CLUSTER_SERVER_VERSION),
            "case {case}: expected the backend to report the engine version the cluster serves, \
             got {:?}",
            session.parameter("server_version")
        );

        // CREATE TABLE / INSERT / SELECT, the round trip a warehouse is
        // provisioned for, plus a value the ENGINE computes rather than echoes.
        for statement in [
            "CREATE TABLE sim2real_roundtrip (id INT PRIMARY KEY, val VARCHAR(64))",
            "INSERT INTO sim2real_roundtrip VALUES (1, 'hello-redshift')",
            "INSERT INTO sim2real_roundtrip VALUES (2, 'second-row')",
        ] {
            session.query(statement).await.unwrap_or_else(|refusal| {
                panic!("case {case}: {statement} must run, got {refusal:?}")
            });
        }
        let answer = session
            .query("SELECT val FROM sim2real_roundtrip WHERE id = 1")
            .await
            .unwrap_or_else(|refusal| panic!("case {case}: the SELECT must run, got {refusal:?}"));
        assert_eq!(
            answer.only(),
            Some("hello-redshift"),
            "case {case}: expected the row the INSERT wrote, got {answer:?}"
        );
        let counted = session
            .query("SELECT COUNT(*) FROM sim2real_roundtrip")
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
            "case {case}: expected the database the create named, got {named:?}"
        );
    }
}

/// One credential case: what the request asks for, and the user name the
/// response must report it under.
struct CredentialRow {
    case: &'static str,
    db_user: &'static str,
    auto_create: Option<&'static str>,
    duration_seconds: Option<&'static str>,
    /// The lifetime the credential must carry, in seconds — the request's own
    /// value, or the documented default when it names none.
    expected_seconds: i64,
    /// The prefix the operation documents for that `AutoCreate`.
    expected_prefix: &'static str,
}

/// The credential cases: both `AutoCreate` prefixes, the documented default
/// lifetime and each published bound of the window.
///
/// Source for the prefixes and the window:
/// `smithy_query(query="doc", operation="GetClusterCredentials")` — "returns the
/// database user name prefixed with IAM: if AutoCreate is False or IAMA: if
/// AutoCreate is True" — and
/// `smithy_query(query="doc", shape="GetClusterCredentialsMessage",
/// member="DurationSeconds")` — "minimum 900, maximum 3600. Default: 900".
fn credential_rows() -> Vec<CredentialRow> {
    vec![
        CredentialRow {
            case: "a created user, the documented default lifetime",
            db_user: "defaultuser",
            auto_create: Some("true"),
            duration_seconds: None,
            expected_seconds: i64::from(state::CREDENTIAL_DURATION_DEFAULT_SECONDS),
            expected_prefix: "IAMA:",
        },
        CredentialRow {
            case: "a created user, the shortest lifetime the window admits",
            db_user: "shortestuser",
            auto_create: Some("true"),
            duration_seconds: Some("900"),
            expected_seconds: i64::from(state::CREDENTIAL_DURATION_MIN_SECONDS),
            expected_prefix: "IAMA:",
        },
        CredentialRow {
            case: "an existing user, the longest lifetime the window admits",
            db_user: MASTER_USER,
            auto_create: None,
            duration_seconds: Some("3600"),
            expected_seconds: i64::from(state::CREDENTIAL_DURATION_MAX_SECONDS),
            expected_prefix: "IAM:",
        },
    ]
}

/// RULE: the temporary credential `GetClusterCredentials` answers logs on to the
/// cluster's published endpoint, and expires at the instant the response
/// reports — issuance plus the lifetime the request asked for.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_temporary_credential_logs_on_to_the_published_endpoint() {
    let emulator = Emulator::new();
    let described = available_cluster(
        &emulator,
        "wire-credentials",
        &[("ClusterType", "single-node")],
    )
    .await;
    let (address, port) = endpoint_of("credentials", &described);

    for row in credential_rows() {
        let case = row.case;
        let mut params: Vec<(&str, &str)> = vec![
            ("ClusterIdentifier", "wire-credentials"),
            ("DbUser", row.db_user),
            ("DbName", state::DEFAULT_DB_NAME),
        ];
        if let Some(auto_create) = row.auto_create {
            params.push(("AutoCreate", auto_create));
        }
        if let Some(duration) = row.duration_seconds {
            params.push(("DurationSeconds", duration));
        }
        // The instant the credentials are issued at, read before the call, so
        // the lifetime asserted below is measured from a point the issuance
        // cannot precede.
        let issued_before = unix_seconds(SystemTime::now());
        let answer = emulator.call("GetClusterCredentials", &params).await;
        assert_eq!(
            answer.status, 200,
            "case {case}: expected 200, got {} with body {}",
            answer.status, answer.body
        );
        let reported_user = answer.text("DbUser").unwrap_or_else(|| {
            panic!("case {case}: expected a DbUser, got body {}", answer.body)
        });
        assert_eq!(
            reported_user,
            format!("{}{}", row.expected_prefix, row.db_user),
            "case {case}: expected the documented prefix on the user named, got {reported_user:?}"
        );
        let password = answer.text("DbPassword").unwrap_or_else(|| {
            panic!("case {case}: expected a DbPassword, got body {}", answer.body)
        });
        // "The date and time the password in DbPassword expires": issuance plus
        // the lifetime the request asked for. The expiry can never fall before
        // that lifetime from the instant read above; the slack above it is the
        // time this call itself may take, and it is far short of the shortest
        // lifetime the window admits, so no off-by-minutes expiry passes.
        let expiration = answer.text("Expiration").unwrap_or_else(|| {
            panic!("case {case}: expected an Expiration, got body {}", answer.body)
        });
        let expires_at = unix_timestamp(case, &expiration);
        assert!(
            expires_at >= issued_before + row.expected_seconds
                && expires_at <= issued_before + row.expected_seconds + ISSUANCE_SLACK_SECONDS,
            "case {case}: expected the expiry {} s past issuance (between {} and {}), got \
             {expires_at}",
            row.expected_seconds,
            issued_before + row.expected_seconds,
            issued_before + row.expected_seconds + ISSUANCE_SLACK_SECONDS
        );

        // The pair logs on, which is what "temporary authorization to log on to
        // an Amazon Redshift database" means — and the session runs statements
        // against the cluster's own database.
        let mut session = postgres_login(
            case,
            &address,
            port,
            Transport::Tls,
            &reported_user,
            &password,
            Some(state::DEFAULT_DB_NAME),
        )
        .await
        .unwrap_or_else(|refusal| {
            panic!("case {case}: the minted credential must be admitted, got {refusal:?}")
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
        let whoami = session
            .query("SELECT current_user()")
            .await
            .unwrap_or_else(|refusal| {
                panic!("case {case}: current_user() must run, got {refusal:?}")
            });
        assert_eq!(
            whoami.only(),
            Some(reported_user.as_str()),
            "case {case}: expected the session to run as the user the credential names, got \
             {whoami:?}"
        );
    }
}

/// RULE: a lifetime outside the documented window is refused with
/// `InvalidParameterValue` and the fixed sentence for that error class, and
/// nothing is issued.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_lifetime_outside_the_documented_window_is_refused() {
    let emulator = Emulator::new();
    available_cluster(&emulator, "wire-window", &[("ClusterType", "single-node")]).await;

    // One row per way a value leaves the window: just under the minimum, just
    // over the maximum, and far outside it either way.
    for (case, duration) in [
        ("one second under the minimum", "899"),
        ("one second over the maximum", "3601"),
        ("no lifetime at all", "0"),
        ("a day", "86400"),
    ] {
        let refused = emulator
            .call(
                "GetClusterCredentials",
                &[
                    ("ClusterIdentifier", "wire-window"),
                    ("DbUser", "windowuser"),
                    ("DurationSeconds", duration),
                ],
            )
            .await;
        assert_eq!(
            (refused.status, refused.error_code()),
            (400, Some("InvalidParameterValue".to_string())),
            "case {case}: expected 400 InvalidParameterValue, got {} / {:?}",
            refused.status,
            refused.error_code()
        );
        assert_eq!(
            refused.text("Message").as_deref(),
            Some(state::credential_duration_out_of_range_message().as_str()),
            "case {case}: expected the documented sentence for the window, got {:?}",
            refused.text("Message")
        );
        assert_eq!(
            refused.text("DbPassword"),
            None,
            "case {case}: a refused request must issue nothing, got {:?}",
            refused.text("DbPassword")
        );
    }
}

/// RULE: every login that is not the cluster's admin credentials or a valid
/// credential it issued is refused with the PostgreSQL error for it — a wrong
/// password, another user's name, a database the cluster does not hold, and a
/// credential whose lifetime has run out.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn only_the_clusters_own_credentials_are_admitted() {
    let emulator = Emulator::new();
    let described = available_cluster(
        &emulator,
        "wire-refusals",
        &[("ClusterType", "single-node")],
    )
    .await;
    let (address, port) = endpoint_of("refusals", &described);

    let issued = emulator
        .call(
            "GetClusterCredentials",
            &[
                ("ClusterIdentifier", "wire-refusals"),
                ("DbUser", "tempuser"),
                ("DbName", state::DEFAULT_DB_NAME),
                ("AutoCreate", "true"),
                ("DurationSeconds", "900"),
            ],
        )
        .await;
    let temp_user = issued
        .text("DbUser")
        .expect("case refusals: the response carries a DbUser");
    let temp_password = issued
        .text("DbPassword")
        .expect("case refusals: the response carries a DbPassword");

    // (case, the user, the password, the database asked for, the SQLSTATE the
    // protocol refuses it with)
    let cases: Vec<(&str, &str, &str, Option<&str>, &str)> = vec![
        (
            "the admin user with the wrong password",
            MASTER_USER,
            "not-the-password",
            Some(state::DEFAULT_DB_NAME),
            "28P01",
        ),
        (
            "a user the cluster's database does not authenticate",
            "someone-else",
            MASTER_PASSWORD,
            Some(state::DEFAULT_DB_NAME),
            "28P01",
        ),
        (
            "a temporary credential with the wrong password",
            temp_user.as_str(),
            "not-the-password",
            Some(state::DEFAULT_DB_NAME),
            "28P01",
        ),
        (
            "the admin user on a database the cluster does not hold",
            MASTER_USER,
            MASTER_PASSWORD,
            Some("no-such-database"),
            "3D000",
        ),
        (
            "a temporary credential on a database it was not issued for",
            temp_user.as_str(),
            temp_password.as_str(),
            Some("no-such-database"),
            "28P01",
        ),
    ];
    for (case, user, password, database, code) in cases {
        let refusal = postgres_login(
            case,
            &address,
            port,
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

    // The credential logs on while it is valid...
    postgres_login(
        "a valid credential",
        &address,
        port,
        Transport::Plaintext,
        &temp_user,
        &temp_password,
        Some(state::DEFAULT_DB_NAME),
    )
    .await
    .unwrap_or_else(|refusal| {
        panic!("case a valid credential: expected it to be admitted, got {refusal:?}")
    });
    // ... and not one second past its `Expiration`, which this emulator's own
    // clock is moved across rather than waited for.
    emulator.advance_clock(Duration::from_secs(
        u64::try_from(state::CREDENTIAL_DURATION_MIN_SECONDS).expect("the window is positive") + 1,
    ));
    let expired = postgres_login(
        "an expired credential",
        &address,
        port,
        Transport::Plaintext,
        &temp_user,
        &temp_password,
        Some(state::DEFAULT_DB_NAME),
    )
    .await
    .err()
    .unwrap_or_else(|| panic!("case an expired credential: expected the login to be refused"));
    assert_eq!(
        expired.code, "28P01",
        "case an expired credential: expected the authentication refusal, got {expired:?}"
    );
    // The admin credentials are not temporary, so they still log on.
    postgres_login(
        "the admin user past the credential's expiry",
        &address,
        port,
        Transport::Plaintext,
        MASTER_USER,
        MASTER_PASSWORD,
        Some(state::DEFAULT_DB_NAME),
    )
    .await
    .unwrap_or_else(|refusal| {
        panic!("case the admin user past the expiry: expected it to be admitted, got {refusal:?}")
    });
}

/// RULE: a cluster that is not available for queries answers no session — and
/// the refusal says which way it is not serving: one still being provisioned is
/// coming up, and one whose compute is suspended is not.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cluster_that_is_not_serving_refuses_a_session() {
    let emulator = Emulator::new();
    // The clock is stopped first, so the cluster stays `creating` for as long as
    // this case needs however fast the host is.
    emulator.stop_clock();
    let created = emulator
        .call(
            "CreateCluster",
            &[
                ("ClusterIdentifier", "wire-not-serving"),
                ("NodeType", "ra3.xlplus"),
                ("MasterUsername", MASTER_USER),
                ("MasterUserPassword", MASTER_PASSWORD),
                ("ClusterType", "single-node"),
            ],
        )
        .await;
    assert_eq!(
        created.status, 200,
        "case creating: expected 200, got {} with body {}",
        created.status, created.body
    );
    assert_eq!(
        created.text("Address"),
        None,
        "case creating: a cluster that is not serving publishes no endpoint, got {:?}",
        created.text("Address")
    );
    // The listener is open from the moment the cluster is allocated — the
    // control plane just does not announce it yet — so this case reads where it
    // answers out of the store the handlers write.
    let (address, port) = served_endpoint(&emulator, "wire-not-serving");
    let refused = postgres_login(
        "creating",
        &address,
        port,
        Transport::Plaintext,
        MASTER_USER,
        MASTER_PASSWORD,
        None,
    )
    .await
    .err()
    .unwrap_or_else(|| panic!("case creating: expected the login to be refused"));
    assert_eq!(
        (refused.code.as_str(), refused.message.as_str()),
        ("57P03", "the database system is starting up"),
        "case creating: expected the refusal of an engine that is coming up, got {refused:?}"
    );

    // Past the provisioning span the cluster serves, on the very endpoint it now
    // publishes.
    emulator.advance_clock(state::PROVISIONING_SPAN);
    let described = emulator
        .call(
            "DescribeClusters",
            &[("ClusterIdentifier", "wire-not-serving")],
        )
        .await;
    assert_eq!(
        endpoint_of("serving", &described),
        (address.clone(), port),
        "case serving: expected the cluster to publish the endpoint it is served on"
    );
    postgres_login(
        "serving",
        &address,
        port,
        Transport::Plaintext,
        MASTER_USER,
        MASTER_PASSWORD,
        None,
    )
    .await
    .unwrap_or_else(|refusal| {
        panic!("case serving: the admin user must be admitted, got {refusal:?}")
    });

    // Suspended compute: the cluster is not coming up, so the refusal is the
    // other one.
    let paused = emulator
        .call("PauseCluster", &[("ClusterIdentifier", "wire-not-serving")])
        .await;
    assert_eq!(
        paused.status, 200,
        "case paused: expected 200, got {} with body {}",
        paused.status, paused.body
    );
    emulator.advance_clock(state::PROVISIONING_SPAN);
    let read = emulator
        .call(
            "DescribeClusters",
            &[("ClusterIdentifier", "wire-not-serving")],
        )
        .await;
    assert_eq!(
        read.text("ClusterStatus").as_deref(),
        Some(state::STATUS_PAUSED),
        "case paused: expected the cluster to report paused, got {:?}",
        read.text("ClusterStatus")
    );
    let refused = postgres_login(
        "paused",
        &address,
        port,
        Transport::Plaintext,
        MASTER_USER,
        MASTER_PASSWORD,
        None,
    )
    .await
    .err()
    .unwrap_or_else(|| panic!("case paused: expected the login to be refused"));
    assert_eq!(
        (refused.code.as_str(), refused.message.as_str()),
        ("57P03", "the database system is shutting down"),
        "case paused: expected the refusal of an engine that is not serving, got {refused:?}"
    );
}

/// RULE: each cluster serves its OWN database on its own endpoint, and deleting
/// a cluster closes the endpoint it published.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn each_cluster_serves_its_own_database_and_a_delete_closes_its_endpoint() {
    let emulator = Emulator::new();
    let first = available_cluster(&emulator, "wire-own-first", &[("ClusterType", "single-node")])
        .await;
    let second = available_cluster(
        &emulator,
        "wire-own-second",
        &[("ClusterType", "single-node")],
    )
    .await;
    let (first_address, first_port) = endpoint_of("first", &first);
    let (second_address, second_port) = endpoint_of("second", &second);
    assert_ne!(
        first_port, second_port,
        "case two clusters: expected each cluster to be served on its own port, got {first_port} \
         twice"
    );

    let mut first_session = postgres_login(
        "first",
        &first_address,
        first_port,
        Transport::Plaintext,
        MASTER_USER,
        MASTER_PASSWORD,
        Some(state::DEFAULT_DB_NAME),
    )
    .await
    .unwrap_or_else(|refusal| panic!("case first: expected a session, got {refusal:?}"));
    first_session
        .query("CREATE TABLE only_here (id INT)")
        .await
        .unwrap_or_else(|refusal| panic!("case first: the CREATE must run, got {refusal:?}"));

    let mut second_session = postgres_login(
        "second",
        &second_address,
        second_port,
        Transport::Plaintext,
        MASTER_USER,
        MASTER_PASSWORD,
        Some(state::DEFAULT_DB_NAME),
    )
    .await
    .unwrap_or_else(|refusal| panic!("case second: expected a session, got {refusal:?}"));
    let missing = second_session
        .query("SELECT * FROM only_here")
        .await
        .err()
        .unwrap_or_else(|| {
            panic!("case second: expected the other cluster's table to be unknown here")
        });
    assert_eq!(
        missing.code, "42P01",
        "case second: expected the undefined-table error, got {missing:?}"
    );

    let deleted = emulator
        .call(
            "DeleteCluster",
            &[
                ("ClusterIdentifier", "wire-own-first"),
                ("SkipFinalClusterSnapshot", "true"),
            ],
        )
        .await;
    assert_eq!(
        deleted.status, 200,
        "case delete: expected 200, got {} with body {}",
        deleted.status, deleted.body
    );
    // The port closes with the cluster. Waited for as a CONDITION — the socket
    // refusing a connection — rather than for a fixed time.
    let deadline = Instant::now() + WAIT_DEADLINE;
    loop {
        if TcpStream::connect(("127.0.0.1", first_port)).await.is_err() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "case delete: expected the deleted cluster's endpoint to stop answering"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    // The cluster that was not deleted keeps serving at its own endpoint.
    postgres_login(
        "the surviving cluster",
        &second_address,
        second_port,
        Transport::Plaintext,
        MASTER_USER,
        MASTER_PASSWORD,
        Some(state::DEFAULT_DB_NAME),
    )
    .await
    .unwrap_or_else(|refusal| {
        panic!("case the surviving cluster: expected it to keep serving, got {refusal:?}")
    });
}

// ---------------------------------------------------------------------------
// The control plane, as a client drives it
// ---------------------------------------------------------------------------

/// Create the cluster `identifier` with `params`, wait for it to report
/// `available`, and answer the read a client takes of it.
async fn available_cluster(
    emulator: &Emulator,
    identifier: &str,
    params: &[(&str, &str)],
) -> Answer {
    let mut create: Vec<(&str, &str)> = vec![
        ("ClusterIdentifier", identifier),
        ("NodeType", "ra3.xlplus"),
        ("MasterUsername", MASTER_USER),
        ("MasterUserPassword", MASTER_PASSWORD),
    ];
    create.extend(params.iter().copied());
    let created = emulator.call("CreateCluster", &create).await;
    assert_eq!(
        created.status, 200,
        "case {identifier}: expected the create to be served, got {} with body {}",
        created.status, created.body
    );
    let (described, _polls) = emulator
        .poll_until(
            identifier,
            "DescribeClusters",
            &[("ClusterIdentifier", identifier)],
            |answer| answer.text("ClusterStatus").as_deref() == Some(state::STATUS_AVAILABLE),
        )
        .await;
    described
}

/// The endpoint a cluster published, as a client reads it: an address this
/// emulator answers on and the port its database engine listens on there.
fn endpoint_of(case: &str, described: &Answer) -> (String, u16) {
    let address = described.text("Address").unwrap_or_else(|| {
        panic!(
            "case {case}: expected a serving cluster to publish an endpoint address, got body {}",
            described.body
        )
    });
    assert_eq!(
        address, LOOPBACK,
        "case {case}: expected the address this emulator serves the cluster on, got {address:?}"
    );
    let port = described
        .text("Port")
        .and_then(|port| port.parse::<u16>().ok())
        .unwrap_or_else(|| {
            panic!(
                "case {case}: expected a serving cluster to publish an endpoint port, got body {}",
                described.body
            )
        });
    assert!(
        port != 0,
        "case {case}: expected the port the cluster's listener is bound to, got {port}"
    );
    (address, port)
}

/// Where the cluster `identifier` is SERVED, read out of the store the handlers
/// write — for the one case that has to reach the endpoint of a cluster the
/// control plane does not announce one for yet.
fn served_endpoint(emulator: &Emulator, identifier: &str) -> (String, u16) {
    let store =
        state::service_state(&*emulator.engine).expect("the emulator holds the Redshift store");
    let store = store.lock();
    let stored = store
        .cluster(identifier)
        .unwrap_or_else(|| panic!("case {identifier}: the cluster the create made"));
    let plane = stored
        .endpoint_plane
        .as_ref()
        .unwrap_or_else(|| panic!("case {identifier}: the create opens the cluster's endpoint"));
    let endpoint = plane.endpoint();
    let port = u16::try_from(endpoint.port).expect("a listener port fits");
    (endpoint.address.clone(), port)
}

/// The UNIX seconds of `instant`.
fn unix_seconds(instant: SystemTime) -> i64 {
    i64::try_from(
        instant
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("the test clock is past the epoch")
            .as_secs(),
    )
    .expect("the instant fits")
}

/// The UNIX seconds of a timestamp an awsQuery response carries, which the
/// protocol renders in ISO 8601.
fn unix_timestamp(case: &str, rendered: &str) -> i64 {
    let parsed = ::aws_smithy_types::DateTime::from_str(
        rendered,
        ::aws_smithy_types::date_time::Format::DateTime,
    )
    .unwrap_or_else(|error| {
        panic!("case {case}: expected an ISO 8601 timestamp, got {rendered:?} ({error})")
    });
    parsed.secs()
}

// ---------------------------------------------------------------------------
// The PostgreSQL client
// ---------------------------------------------------------------------------

/// A connection to a cluster's endpoint, in the clear or upgraded to TLS.
trait Connection: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send {}
impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send> Connection for T {}

/// What a client holds: the stream, whatever it is now.
type Client = Box<dyn Connection>;

/// A refusal the server sent: the SQLSTATE a client branches on and the
/// sentence it reports.
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

/// A PostgreSQL session a client has opened, and the parameters the backend
/// reported for it.
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
                    let mut at = 2usize;
                    answer.columns.clear();
                    for _ in 0..count {
                        let (name, next) = read_cstring(&body, at);
                        answer.columns.push(name);
                        // name, table oid, column number, type oid, size,
                        // modifier, format code.
                        at = next + 18;
                    }
                }
                // DataRow.
                b'D' => {
                    let count = usize::from(u16::from_be_bytes([body[0], body[1]]));
                    let mut at = 2usize;
                    let mut row = Vec::with_capacity(count);
                    for _ in 0..count {
                        let length =
                            i32::from_be_bytes([body[at], body[at + 1], body[at + 2], body[at + 3]]);
                        at += 4;
                        if length < 0 {
                            row.push(None);
                            continue;
                        }
                        let length = usize::try_from(length).expect("a value length fits");
                        row.push(Some(
                            String::from_utf8_lossy(&body[at..at + length]).into_owned(),
                        ));
                        at += length;
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

/// Open a PostgreSQL session on `address:port`, as libpq does: the optional
/// `SSLRequest` first, then the startup message, the password, and the
/// parameters the server reports.
async fn postgres_login(
    case: &str,
    address: &str,
    port: u16,
    transport: Transport,
    user: &str,
    password: &str,
    database: Option<&str>,
) -> Result<PostgresSession, Refusal> {
    assert_eq!(
        address, LOOPBACK,
        "case {case}: the published endpoint must be this machine's loopback address, got \
         {address:?}"
    );
    let stream = TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap_or_else(|error| {
            panic!(
                "case {case}: the published endpoint {address}:{port} must accept a connection, \
                 got {error}"
            )
        });
    let mut conn: Client = Box::new(stream);
    if transport == Transport::Tls {
        let mut request = Vec::new();
        request.extend_from_slice(&8i32.to_be_bytes());
        // The `SSLRequest` code, sent in the clear as the connection's first
        // bytes — which is what `sslmode=require` sends.
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
/// The verifier accepts the certificate the EMULATOR'S OWN core generated for
/// its listeners, which is self-signed and names loopback: this client is the
/// emulator's own test, connecting to a port the same process opened, so there
/// is no authority to check it against.
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

/// Read one PostgreSQL message, or `None` where the server closed the
/// connection.
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
    let mut at = 0usize;
    while at < body.len() && body[at] != 0 {
        let field = body[at];
        let (value, next) = read_cstring(body, at + 1);
        match field {
            b'C' => refusal.code = value,
            b'M' => refusal.message = value,
            _ => {}
        }
        at = next;
    }
    refusal
}

/// The NUL-terminated string at `at`, and the index after it.
fn read_cstring(payload: &[u8], at: usize) -> (String, usize) {
    let rest = &payload[at..];
    let end = rest.iter().position(|byte| *byte == 0).unwrap_or(rest.len());
    (
        String::from_utf8_lossy(&rest[..end]).into_owned(),
        at + end + 1,
    )
}

/// Append a NUL-terminated string.
fn put_cstring(payload: &mut Vec<u8>, text: &str) {
    payload.extend_from_slice(text.as_bytes());
    payload.push(0);
}
