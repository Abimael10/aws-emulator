// SPDX-License-Identifier: CC0-1.0

//! The LOAD Performance Insights reports for an Amazon RDS DB instance, across the two
//! services that have to agree for a client to read it.
//!
//! RULE: the load Performance Insights reports for a data source is the session activity the
//! engine behind that resource's published endpoint actually had. Amazon RDS runs the
//! sessions — a client connects to `Endpoint{Address,Port}` with its ordinary driver and runs
//! statements — and records what each statement cost; Performance Insights reports that back,
//! grouped by the dimensions of the sessions it came from (the wait event each session was on,
//! the user that opened it, the host it came from, the statement it ran) and ordered by the
//! load each group accounted for, descending, with the same figure in `GetResourceMetrics`'s
//! `db.load.avg` series. A data source whose engine ran nothing reports no key and no load.
//!
//! Only a cross-service suite can pin this: the sessions run on the engine Amazon RDS opened
//! and the answer is read back through Performance Insights' own operations, over ONE engine,
//! exactly as `core/router/src/lib.rs` mounts them. The client is the PostgreSQL protocol
//! itself — the same wire client the engine-log suite drives — so what produces the load here
//! is what a stock driver sends.
//!
//! Two data sources are driven: one whose sessions contend for a row, so its window holds both
//! the wait event a blocked session is on and the CPU time of the statements that ran, and one
//! nothing ever connected to, which reports the absence of load rather than a share of the
//! other's.

use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use aws_emulator_router::{build_router, InMemoryEngine};
use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, form_encode};
use crate::rds_engine_log_export_to_cloudwatch_logs::{
    Session, BOOT_STACK_SIZE, DATABASE, MASTER_PASSWORD, MASTER_USER, POLL_DEADLINE,
    POLL_INTERVAL, REGION,
};

/// The engine version the instances run.
///
/// Amazon RDS publishes `rds.force_ssl`'s default as 1 for the PostgreSQL 15 and later release
/// lines and 0 before them, so an instance of this line admits the PLAINTEXT session this
/// suite's client opens; the in-band TLS upgrade is the subject of the RDS crate's own
/// endpoint suites.
const ENGINE_VERSION: &str = "14.13";

/// The `X-Amz-Target` prefix the Performance Insights model answers.
const PI_TARGET_PREFIX: &str = "PerformanceInsightsv20180227";

/// The wait event the MySQL and PostgreSQL engines report for a session that was RUNNING
/// rather than waiting, which both publish under this one name.
const CPU_WAIT_EVENT: &str = "CPU";

/// The wait event the PostgreSQL engines report for a session waiting on a row-level lock,
/// which is what a second transaction updating a held row is on.
const ROW_LOCK_WAIT_EVENT: &str = "Lock:transactionid";

/// The row every contending transaction below updates.
const CONTENDED_ROW: i64 = 1;

#[test]
fn performance_insights_reports_the_load_the_rds_engine_actually_had() {
    std::thread::Builder::new()
        .name("pi-rds-session-load".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
            let mut router = build_router(engine);
            router.set_request_authentication(false);
            // `enable_all`, which is what the emulator's own boot path builds: opening a DB
            // instance's engine listener needs the runtime's I/O driver, and the sessions
            // below are real sockets on it.
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("a current-thread runtime");
            runtime.block_on(drive(&router));
        })
        .expect("spawn the session-load thread")
        .join()
        .expect("Performance Insights must report the load the engine had");
}

/// Epoch seconds, as the service reads its own clock.
fn unix_now() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("the clock is at or after the Unix epoch")
            .as_secs(),
    )
    .expect("the current time fits in an i64")
}

/// One call of `method` to the service `signing_name`, with `body`.
async fn dispatch(
    router: &MultiServiceRouter,
    signing_name: &str,
    headers: &[(&str, &str)],
    body: String,
) -> (u16, String) {
    let host = format!("{signing_name}.{REGION}.amazonaws.com");
    let mut builder = ::http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", host)
        .header(
            // The signature is never checked: this router runs with authentication off.
            // What the scope carries is the Region and the service a handler reads.
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 \
                 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
                 SignedHeaders=host, Signature=not-verified"
            ),
        );
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let request = builder
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let (parts, body) = response.into_parts();
    (
        parts.status.as_u16(),
        String::from_utf8_lossy(&body).to_string(),
    )
}

/// One awsQuery call to Amazon RDS that must be served, as an SDK client sends one.
async fn rds(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    params: &[(&str, String)],
) -> String {
    let mut body = format!("Action={action}&Version=2014-10-31");
    for (name, value) in params {
        body.push('&');
        body.push_str(name);
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let (status, body) = dispatch(
        router,
        "rds",
        &[("content-type", "application/x-www-form-urlencoded")],
        body,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the {action} call must be served, got HTTP {status} with {body}"
    );
    body
}

/// One Performance Insights call that must be served, as its JSON client sends one.
async fn pi(router: &MultiServiceRouter, case: &str, operation: &str, input: Value) -> Value {
    let (status, body) = dispatch(
        router,
        "pi",
        &[
            ("content-type", "application/x-amz-json-1.1"),
            (
                "x-amz-target",
                Box::leak(format!("{PI_TARGET_PREFIX}.{operation}").into_boxed_str()),
            ),
        ],
        input.to_string(),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the {operation} call must be served, got HTTP {status} with {body}"
    );
    serde_json::from_str(&body).unwrap_or_else(|error| {
        panic!("case {case:?}: Performance Insights answers JSON, got {error} in {body}")
    })
}

/// Create a PI-enabled PostgreSQL DB instance, wait for it to report `available`, and answer
/// with the endpoint it published and the `DbiResourceId` Performance Insights addresses it
/// by.
async fn available_instance(
    router: &MultiServiceRouter,
    case: &str,
    identifier: &str,
) -> (String, u16, String) {
    let created = rds(
        router,
        case,
        "CreateDBInstance",
        &[
            ("DBInstanceIdentifier", identifier.to_string()),
            ("Engine", "postgres".to_string()),
            ("EngineVersion", ENGINE_VERSION.to_string()),
            ("DBInstanceClass", "db.t3.medium".to_string()),
            ("AllocatedStorage", "20".to_string()),
            ("MasterUsername", MASTER_USER.to_string()),
            ("MasterUserPassword", MASTER_PASSWORD.to_string()),
            ("DBName", DATABASE.to_string()),
            ("EnablePerformanceInsights", "true".to_string()),
        ],
    )
    .await;
    let resource_id = element(&created, "DbiResourceId");
    assert!(
        resource_id.starts_with("db-"),
        "case {case:?}: expected a minted DbiResourceId, got {created}"
    );

    // The instance comes up on its own, so what this waits for is the CONDITION a client has
    // to see before it connects — read again until it holds or a deadline far longer than the
    // span this service owes that transition in has passed.
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let described = rds(
            router,
            case,
            "DescribeDBInstances",
            &[("DBInstanceIdentifier", identifier.to_string())],
        )
        .await;
        if element(&described, "DBInstanceStatus") == "available" {
            let address = element(&described, "Address");
            let port: u16 = element(&described, "Port")
                .parse()
                .unwrap_or_else(|error| panic!("case {case:?}: a published port: {error}"));
            return (address, port, resource_id);
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the instance must settle to available before a client connects, \
             last read {described}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The dimension keys Performance Insights reports for `resource_id` over the last quarter of
/// an hour, grouped by `group`.
async fn dimension_keys(
    router: &MultiServiceRouter,
    case: &str,
    resource_id: &str,
    group: &str,
) -> Vec<Value> {
    let now = unix_now();
    let answer = pi(
        router,
        case,
        "DescribeDimensionKeys",
        json!({
            "ServiceType": "RDS",
            "Identifier": resource_id,
            "StartTime": now - 900,
            // The window's end is ahead of the clock, so the activity of the seconds just
            // past is inside it: `EndTime` is exclusive and the keys follow the window.
            "EndTime": now + 300,
            "Metric": "db.load.avg",
            "GroupBy": {"Group": group, "Limit": 10},
            "PeriodInSeconds": 60,
        }),
    )
    .await;
    answer
        .get("Keys")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// The `Dimensions` value `dimension` has on `key`.
fn dimension(key: &Value, dimension: &str) -> String {
    key.get("Dimensions")
        .and_then(Value::as_object)
        .and_then(|map| map.get(dimension))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The `Total` of `key`.
fn total(key: &Value) -> f64 {
    key.get("Total").and_then(Value::as_f64).unwrap_or(f64::NAN)
}

/// Run the whole flow: provision, drive real sessions, and read the load back.
async fn drive(router: &MultiServiceRouter) {
    let case = "a PI-enabled postgres instance under row-lock contention";
    let (address, port, resource_id) = available_instance(router, case, "pi-load-postgres").await;

    // A second data source, provisioned the same way and never connected to: whatever the
    // first one's sessions do, this one's window holds no activity.
    let (_, _, idle_resource_id) = available_instance(router, case, "pi-load-idle").await;

    // Before any session runs, the engine has had no activity, so the window holds no key.
    assert!(
        dimension_keys(router, case, &resource_id, "db.wait_event")
            .await
            .is_empty(),
        "case {case:?}: expected no keys before any session ran"
    );

    let mut holder = Session::open(case, &address, port).await;
    let mut waiter = Session::open(case, &address, port).await;
    for statement in [
        "CREATE TABLE ledger (id int primary key, amount int)",
        "INSERT INTO ledger VALUES (1, 0)",
    ] {
        holder
            .query(case, statement)
            .await
            .unwrap_or_else(|refusal| panic!("case {case:?}: {statement:?} must run: {refusal:?}"));
    }

    // Two transactions over ONE row: the first holds it, so the second's update WAITS for it
    // — which is the wait event a loaded engine reports. Retried until the engine reports that
    // wait rather than waited out, because the two sessions are served concurrently and the
    // first attempt may be serialised by the host; the deadline is generous and nothing here
    // is timed.
    let deadline = Instant::now() + POLL_DEADLINE;
    let wait_event_keys: Vec<Value> = loop {
        contend_for_the_row(case, &mut holder, &mut waiter).await;
        let keys = dimension_keys(router, case, &resource_id, "db.wait_event").await;
        let waits: Vec<String> = keys
            .iter()
            .map(|key| dimension(key, "db.wait_event.name"))
            .collect();
        if waits.iter().any(|name| name == ROW_LOCK_WAIT_EVENT)
            && waits.iter().any(|name| name == CPU_WAIT_EVENT)
        {
            break keys;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the engine to report a row-lock wait beside its CPU time, \
             got {waits:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    };

    // Every key carries its dimensions and a float total, and the keys are ordered by the load
    // each accounted for, descending: "the top N dimension keys for a metric".
    assert!(
        wait_event_keys.len() >= 2,
        "case {case:?}: expected at least two differently weighted wait events, got \
         {wait_event_keys:?}"
    );
    let totals: Vec<f64> = wait_event_keys.iter().map(total).collect();
    assert!(
        totals.iter().all(|value| *value > 0.0),
        "case {case:?}: expected every key to carry the load it accounted for, got {totals:?}"
    );
    assert!(
        totals.windows(2).all(|pair| pair[0] >= pair[1]),
        "case {case:?}: expected the totals ordered descending, got {totals:?}"
    );
    for key in &wait_event_keys {
        let name = dimension(key, "db.wait_event.name");
        assert!(
            [CPU_WAIT_EVENT, ROW_LOCK_WAIT_EVENT].contains(&name.as_str()),
            "case {case:?}: expected a wait event this engine reports, got {name:?}"
        );
        assert!(
            !dimension(key, "db.wait_event.type").is_empty(),
            "case {case:?}: expected the event's type beside its name, got {key:?}"
        );
    }

    // The other dimension groups report the SAME activity, projected onto their own
    // dimensions: the user that opened the sessions, the host they came from, and the
    // statements they ran.
    let users = dimension_keys(router, case, &resource_id, "db.user").await;
    assert_eq!(
        users
            .iter()
            .map(|key| dimension(key, "db.user.name"))
            .collect::<Vec<_>>(),
        vec![MASTER_USER.to_string()],
        "case {case:?}: expected the master user's sessions, got {users:?}"
    );
    let hosts = dimension_keys(router, case, &resource_id, "db.host").await;
    assert!(
        hosts
            .iter()
            .all(|key| !dimension(key, "db.host.name").is_empty()
                && !dimension(key, "db.host.id").is_empty()),
        "case {case:?}: expected each host key to carry its name and id, got {hosts:?}"
    );
    let statements = dimension_keys(router, case, &resource_id, "db.sql").await;
    assert!(
        statements
            .iter()
            .any(|key| dimension(key, "db.sql.statement").contains("UPDATE ledger")),
        "case {case:?}: expected the statements the sessions ran, got {statements:?}"
    );
    let digests = dimension_keys(router, case, &resource_id, "db.sql_tokenized").await;
    assert!(
        digests
            .iter()
            .any(|key| dimension(key, "db.sql_tokenized.statement").contains('?')),
        "case {case:?}: expected the digest of the statements, with their values replaced, \
         got {digests:?}"
    );

    // The same load reaches the time series: `db.load.avg` reports the average active
    // sessions of each period, so a period that held the activity above carries it. Polled
    // because the series stops at the last period boundary the service holds, which the clock
    // moves past on its own.
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let series = load_series(router, case, &resource_id).await;
        if series.iter().any(|value| *value > 0.0) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected db.load.avg to report the recorded activity, got {series:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }

    // And the data source nothing connected to has none of it: the activity is attributed to
    // the resource whose engine ran it.
    for group in ["db.wait_event", "db.user", "db.sql"] {
        assert!(
            dimension_keys(router, case, &idle_resource_id, group)
                .await
                .is_empty(),
            "case an idle data source / {group}: expected no keys, got some"
        );
    }
    assert!(
        load_series(router, case, &idle_resource_id)
            .await
            .iter()
            .all(|value| *value == 0.0),
        "case an idle data source: expected no load at all"
    );
}

/// Hold the contended row in one transaction while the other updates it, so the second
/// session WAITS for the first.
///
/// The waiting session's statement is written to the socket before the holder's `COMMIT` is,
/// which is what puts the engine's row lock in the way of it; the two are then driven
/// together, because the wait only ends when the holder releases the row.
async fn contend_for_the_row(case: &str, holder: &mut Session, waiter: &mut Session) {
    let update = format!("UPDATE ledger SET amount = amount + 1 WHERE id = {CONTENDED_ROW}");
    for statement in ["BEGIN", update.as_str()] {
        holder
            .query(case, statement)
            .await
            .unwrap_or_else(|refusal| panic!("case {case:?}: {statement:?} must run: {refusal:?}"));
    }
    let (blocked, committed) = tokio::join!(
        waiter.query(case, &update),
        holder.query(case, "COMMIT"),
    );
    blocked.unwrap_or_else(|refusal| {
        panic!("case {case:?}: the waiting update must run once the row is free: {refusal:?}")
    });
    committed
        .unwrap_or_else(|refusal| panic!("case {case:?}: the holder must commit: {refusal:?}"));
}

/// The `db.load.avg` series Performance Insights reports for `resource_id` over the last
/// quarter of an hour, at the finest granularity, so the periods it covers reach as close to
/// the clock as the service holds.
async fn load_series(router: &MultiServiceRouter, case: &str, resource_id: &str) -> Vec<f64> {
    let now = unix_now();
    let answer = pi(
        router,
        case,
        "GetResourceMetrics",
        json!({
            "ServiceType": "RDS",
            "Identifier": resource_id,
            "MetricQueries": [{"Metric": "db.load.avg"}],
            "StartTime": now - 300,
            "EndTime": now,
            "PeriodInSeconds": 1,
        }),
    )
    .await;
    answer
        .get("MetricList")
        .and_then(Value::as_array)
        .and_then(|list| list.first())
        .and_then(|entry| entry.get("DataPoints"))
        .and_then(Value::as_array)
        .map(|points| {
            points
                .iter()
                .map(|point| point.get("Value").and_then(Value::as_f64).unwrap_or(f64::NAN))
                .collect()
        })
        .unwrap_or_default()
}
