// SPDX-License-Identifier: CC0-1.0

//! `StartSession`, driven end to end over the same router the emulator boots (awsJson1_1,
//! `x-amz-target: AmazonAthena.StartSession`), and read back through `GetSession`.
//!
//! THE RULE UNDER TEST: a start mints a fresh `SessionId` for a session in the Spark enabled
//! workgroup the request names, persists every parameter that request carried, and answers
//! with the state the clock puts the session in; the session's own read reports what the
//! start persisted, and the session reaches IDLE from the instant its resources are due
//! whether or not anybody polls it. A workgroup this account does not hold, a disabled one,
//! one that runs Athena SQL, a notebook id naming no notebook and a notebook version with no
//! notebook are each refused with `InvalidRequestException`, a repeat under one
//! `ClientRequestToken` starts nothing further, and a `SessionId` nothing was minted under
//! resolves to no session.
//!
//! Each rule is asserted over a TABLE rather than one request: the rows differ along which
//! parameters the start carries (description, DPU sizing, Spark properties, log delivery, an
//! execution role of its own, tags), the bounds of the idle timeout the model admits, whether
//! the session is bound to a notebook, which precondition of the target workgroup fails,
//! which parameter a reused idempotency token changed, and the shape of a session id that
//! names no session.
//!
//! Nothing here asserts on the wall clock: a state read straight after a start is asserted
//! to be one of the ACTIVE states rather than a particular one, and the idle transition is
//! waited for by polling the CONDITION under a deadline far above the span it is due after.

use std::collections::BTreeSet;
use std::time::Duration;

use serde_json::{json, Value};
use service_com_amazonaws_athena_amazonathena::state::{
    self, ACTIVE_SESSION_STATES, DEFAULT_WORK_GROUP, ERROR_CODE_INVALID_INPUT,
    ERROR_CODE_NOTEBOOK_NOT_FOUND, NOTEBOOK_ID_CONFIG, NOTEBOOK_TYPE_IPYNB,
    PYSPARK_ENGINE_VERSION, SESSION_STATE_IDLE,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{call, error_code, message, request_token, router};

/// The Spark enabled workgroup the sessions below run in.
const WORK_GROUP: &str = "spark-sessions";

/// An Athena SQL workgroup: it runs no Spark calculation, so it runs no session.
const SQL_WORK_GROUP: &str = "sql-sessions";

/// A Spark enabled workgroup that has been disabled.
const DISABLED_WORK_GROUP: &str = "spark-sessions-disabled";

/// The execution role the WORKGROUP carries, which a start that names none runs under.
const WORK_GROUP_ROLE: &str = "arn:aws:iam::000000000000:role/AthenaSparkExecution";

/// The execution role a START may carry instead, which then overrides the workgroup's.
const SESSION_ROLE: &str = "arn:aws:iam::000000000000:role/AthenaSessionExecution";

/// The workgroup's result configuration, which is where a session's notebook information is
/// stored (`SessionConfiguration$WorkingDirectory`).
const OUTPUT_LOCATION: &str = "s3://athena-results/sessions/";

/// One of the `EncryptionOption` values the model admits, carried by the workgroup's result
/// configuration and reported for the session that runs in it.
const ENCRYPTION_OPTION: &str = "SSE_S3";

/// The longest `DescriptionString` the model admits (len 1..1024).
const LONGEST_DESCRIPTION_CHARS: usize = 1024;

/// The bounds of the modelled `SessionIdleTimeoutInMinutes` range (1..480).
const SHORTEST_IDLE_TIMEOUT_MINUTES: i64 = 1;
const LONGEST_IDLE_TIMEOUT_MINUTES: i64 = 480;

/// The bounds of the modelled `MaxConcurrentDpus` range (2..5000).
const FEWEST_CONCURRENT_DPUS: i64 = 2;
const MOST_CONCURRENT_DPUS: i64 = 5000;

/// The only size the modelled `CoordinatorDpuSize` and `DefaultExecutorDpuSize` ranges
/// admit (1..1).
const DPU_SIZE: i64 = 1;

/// How long a wait for a session to reach IDLE is given: far above the span this emulator
/// owes that transition, so a loaded host cannot turn a slow run into a failure.
const IDLE_DEADLINE: Duration = Duration::from_secs(60);

/// The Spark property keys the oversized-configuration row carries. The model bounds each
/// `ParametersMap` value (len 1..51200) but not how MANY entries a map holds, so the bytes a
/// stored session configuration may hold are passed with several entries rather than one.
const OVERSIZED_PROPERTY_KEYS: [&str; 2] = ["spark.oversized.one", "spark.oversized.two"];

/// The value each of those keys carries: under the modelled per-value length, and large
/// enough that the two together pass the ceiling.
const OVERSIZED_PROPERTY_CHARS: usize = 40_000;

/// How often the wait looks again. The CONDITION is what is waited on, never a span.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Create a Spark enabled workgroup called `name`, carrying an execution role, a result
/// configuration and a tag of its own.
async fn given_spark_work_group(router: &MultiServiceRouter, name: &str) {
    let (status, created) = call(
        router,
        "CreateWorkGroup",
        &json!({
            "Name": name,
            "Configuration": {
                "EngineVersion": { "SelectedEngineVersion": PYSPARK_ENGINE_VERSION },
                "ExecutionRole": WORK_GROUP_ROLE,
                "ResultConfiguration": {
                    "OutputLocation": OUTPUT_LOCATION,
                    "EncryptionConfiguration": { "EncryptionOption": ENCRYPTION_OPTION },
                },
            },
            "Tags": [{ "Key": "team", "Value": "analytics" }],
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the Spark workgroup {name} must be created, got {status} with {created:?}"
    );
}

/// Import an ipynb notebook into `WORK_GROUP` and hand back the `NotebookId` it minted.
async fn given_notebook(router: &MultiServiceRouter, name: &str) -> String {
    let (status, imported) = call(
        router,
        "ImportNotebook",
        &json!({
            "WorkGroup": WORK_GROUP,
            "Name": name,
            "Type": NOTEBOOK_TYPE_IPYNB,
            "Payload": "{\"cells\": []}",
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the notebook {name} must be imported, got {status} with {imported:?}"
    );
    imported["NotebookId"]
        .as_str()
        .unwrap_or_else(|| panic!("an import must answer with a NotebookId, got {imported:?}"))
        .to_string()
}

/// The smallest engine configuration a start can carry: the model requires the member but
/// every one of its own is optional.
fn bare_engine_configuration() -> Value {
    json!({})
}

/// One `StartSession` call, as a client sends it.
async fn start(router: &MultiServiceRouter, request: &Value) -> (u16, Value) {
    call(router, "StartSession", request).await
}

/// `GetSession`'s view of one session.
async fn session(router: &MultiServiceRouter, session_id: &str) -> (u16, Value) {
    call(router, "GetSession", &json!({ "SessionId": session_id })).await
}

/// The session `session_id` names, which must read back.
async fn read_session(router: &MultiServiceRouter, case: &str, session_id: &str) -> Value {
    let (status, found) = session(router, session_id).await;
    assert_eq!(
        status, 200,
        "case {case:?}: a started session must read back, got {status} with {found:?}"
    );
    found
}

/// Poll `session_id` until it reports IDLE, and hand back that read.
///
/// The deadline is generous and the wait is on the CONDITION: the session becomes idle when
/// its resources are due, which is a transition the clock owes rather than one a reader makes.
async fn settled_idle(router: &MultiServiceRouter, case: &str, session_id: &str) -> Value {
    let looks = IDLE_DEADLINE.as_millis() / POLL_INTERVAL.as_millis();
    for _ in 0..looks {
        let found = read_session(router, case, session_id).await;
        if found["Status"]["State"].as_str() == Some(SESSION_STATE_IDLE) {
            return found;
        }
        ::tokio::time::sleep(POLL_INTERVAL).await;
    }
    panic!("case {case:?}: the session {session_id} never reached {SESSION_STATE_IDLE}");
}

/// The `SessionId` a start answered with, which must be one no earlier start minted.
fn minted_id(case: &str, started: &Value, already: &mut BTreeSet<String>) -> String {
    let session_id = started["SessionId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: a start answers with a SessionId, got {started:?}"))
        .to_string();
    assert!(
        !session_id.is_empty() && session_id.len() <= 256,
        "case {case:?}: expected a SessionId of the modelled length (1..256), got {session_id:?}"
    );
    assert!(
        already.insert(session_id.clone()),
        "case {case:?}: expected an id no earlier start minted, got {session_id:?} among \
         {already:?}"
    );
    session_id
}

/// A start answers with a non-terminal state the clock owes, which is one of the states a
/// session is ACTIVE in.
fn assert_active_state(case: &str, where_from: &str, state: Option<&str>) {
    let reported = state.unwrap_or("<absent>");
    assert!(
        ACTIVE_SESSION_STATES.contains(&reported),
        "case {case:?}: expected {where_from} to report one of {ACTIVE_SESSION_STATES:?}, got \
         {reported:?}"
    );
}

/// A started session reports back, member for member, what the request that started it
/// carried — beside the engine version and the configuration of the workgroup it runs in.
#[tokio::test]
async fn a_started_session_reads_back_what_the_start_persisted() {
    let router = router();
    given_spark_work_group(&router, WORK_GROUP).await;
    let notebook = given_notebook(&router, "analysis").await;

    // The rows differ along WHICH parameters the start carries: none beyond the two the
    // model requires, a sized session, the bounds of the idle timeout and the concurrency
    // the model admits, Spark properties and log delivery, a notebook binding, an execution
    // role of its own, and tags copied from the workgroup.
    let longest_description = "d".repeat(LONGEST_DESCRIPTION_CHARS);
    let rows: Vec<(&str, Value)> = vec![
        (
            "the smallest start the model admits",
            json!({ "WorkGroup": WORK_GROUP, "EngineConfiguration": bare_engine_configuration() }),
        ),
        (
            "a described session with the shortest idle timeout",
            json!({
                "WorkGroup": WORK_GROUP,
                "Description": "the quarterly rollup",
                "SessionIdleTimeoutInMinutes": SHORTEST_IDLE_TIMEOUT_MINUTES,
                "EngineConfiguration": {
                    "CoordinatorDpuSize": DPU_SIZE,
                    "DefaultExecutorDpuSize": DPU_SIZE,
                    "MaxConcurrentDpus": FEWEST_CONCURRENT_DPUS,
                },
            }),
        ),
        (
            "the longest description, idle timeout and concurrency the model admits",
            json!({
                "WorkGroup": WORK_GROUP,
                "Description": longest_description,
                "SessionIdleTimeoutInMinutes": LONGEST_IDLE_TIMEOUT_MINUTES,
                "EngineConfiguration": { "MaxConcurrentDpus": MOST_CONCURRENT_DPUS },
            }),
        ),
        (
            "Spark properties, classifications and log delivery",
            json!({
                "WorkGroup": WORK_GROUP,
                "EngineConfiguration": {
                    "MaxConcurrentDpus": FEWEST_CONCURRENT_DPUS,
                    "SparkProperties": { "spark.sql.shuffle.partitions": "8" },
                    "Classifications": [{
                        "Name": "athena-query-engine-properties",
                        "Properties": { "max-dpu-count": "4" },
                    }],
                },
                "MonitoringConfiguration": {
                    "S3LoggingConfiguration": {
                        "Enabled": true,
                        "LogLocation": "s3://athena-results/session-logs/",
                    },
                },
            }),
        ),
        (
            "a notebook session",
            json!({
                "WorkGroup": WORK_GROUP,
                "NotebookVersion": "Athena notebook version 1",
                "EngineConfiguration": {
                    "MaxConcurrentDpus": FEWEST_CONCURRENT_DPUS,
                    "AdditionalConfigs": { NOTEBOOK_ID_CONFIG: notebook.clone() },
                },
            }),
        ),
        (
            "an execution role of its own, with the workgroup's tags copied",
            json!({
                "WorkGroup": WORK_GROUP,
                "ExecutionRole": SESSION_ROLE,
                "CopyWorkGroupTags": true,
                "Tags": [{ "Key": "owner", "Value": "data-platform" }],
                "EngineConfiguration": bare_engine_configuration(),
            }),
        ),
    ];

    let mut minted: BTreeSet<String> = BTreeSet::new();
    for (case, request) in &rows {
        let (status, started) = start(&router, request).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected StartSession to answer 200, got {status} with {started:?}"
        );
        assert_active_state(case, "the start", started["State"].as_str());
        let session_id = minted_id(case, &started, &mut minted);

        let found = read_session(&router, case, &session_id).await;
        assert_eq!(
            found["SessionId"].as_str(),
            Some(session_id.as_str()),
            "case {case:?}: the read names the session that was started, got {found:?}"
        );
        assert_eq!(
            found["WorkGroup"].as_str(),
            Some(WORK_GROUP),
            "case {case:?}: the read names the workgroup the start named, got {found:?}"
        );
        assert_eq!(
            found["Description"], request["Description"],
            "case {case:?}: the read reports the description the start carried, got {found:?}"
        );
        assert_eq!(
            found["NotebookVersion"], request["NotebookVersion"],
            "case {case:?}: the read reports the notebook version the start carried, got \
             {found:?}"
        );
        assert_eq!(
            found["MonitoringConfiguration"], request["MonitoringConfiguration"],
            "case {case:?}: the read reports the log delivery the start carried, got {found:?}"
        );
        // The engine version is the one the WORKGROUP selected: the engine the session's
        // calculations run on, not anything the start chose.
        assert_eq!(
            found["EngineVersion"].as_str(),
            Some(PYSPARK_ENGINE_VERSION),
            "case {case:?}: expected the workgroup's selected engine version, got {found:?}"
        );
        // Every engine-configuration member the start set is reported with the value it set.
        let sent = &request["EngineConfiguration"];
        let reported = &found["EngineConfiguration"];
        for member in [
            "CoordinatorDpuSize",
            "DefaultExecutorDpuSize",
            "MaxConcurrentDpus",
            "AdditionalConfigs",
            "SparkProperties",
            "Classifications",
        ] {
            if !sent[member].is_null() {
                assert_eq!(
                    reported[member], sent[member],
                    "case {case:?}: expected EngineConfiguration.{member} to read back as \
                     {:?}, got {reported:?}",
                    sent[member]
                );
            }
        }

        let configuration = &found["SessionConfiguration"];
        let expected_role = request["ExecutionRole"]
            .as_str()
            .unwrap_or(WORK_GROUP_ROLE);
        assert_eq!(
            configuration["ExecutionRole"].as_str(),
            Some(expected_role),
            "case {case:?}: expected the execution role {expected_role:?}, got {configuration:?}"
        );
        assert_eq!(
            configuration["WorkingDirectory"].as_str(),
            Some(OUTPUT_LOCATION),
            "case {case:?}: expected the workgroup's output location as the working \
             directory, got {configuration:?}"
        );
        assert_eq!(
            configuration["EncryptionConfiguration"]["EncryptionOption"].as_str(),
            Some(ENCRYPTION_OPTION),
            "case {case:?}: expected the workgroup's encryption settings, got {configuration:?}"
        );
        assert_eq!(
            configuration["SessionIdleTimeoutInMinutes"], request["SessionIdleTimeoutInMinutes"],
            "case {case:?}: expected the idle timeout the start asked for, got {configuration:?}"
        );
        let expected_seconds = request["SessionIdleTimeoutInMinutes"]
            .as_i64()
            .map(|minutes| minutes * 60);
        assert_eq!(
            configuration["IdleTimeoutSeconds"].as_i64(),
            expected_seconds,
            "case {case:?}: expected that idle timeout in seconds, got {configuration:?}"
        );

        let status_member = &found["Status"];
        assert_active_state(case, "the read", status_member["State"].as_str());
        assert!(
            status_member["StartDateTime"].as_f64().is_some(),
            "case {case:?}: the status carries the instant the session started, got \
             {status_member:?}"
        );
        assert!(
            status_member["EndDateTime"].is_null(),
            "case {case:?}: a session nothing shut down has not ended, got {status_member:?}"
        );
        assert!(
            found["Statistics"].is_null(),
            "case {case:?}: a session no calculation ran on reports no DPU execution time, \
             got {found:?}"
        );
    }
}

/// A started session is real state: a session bound to a notebook is the one that notebook's
/// listing reports, and a session started for programmatic access belongs to no notebook.
#[tokio::test]
async fn a_started_notebook_session_is_the_one_the_notebook_lists() {
    let router = router();
    given_spark_work_group(&router, WORK_GROUP).await;
    let notebook = given_notebook(&router, "listed").await;
    let elsewhere = given_notebook(&router, "untouched").await;

    let (status, bound) = start(
        &router,
        &json!({
            "WorkGroup": WORK_GROUP,
            "EngineConfiguration": { "AdditionalConfigs": { NOTEBOOK_ID_CONFIG: notebook } },
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "a notebook session must start, got {status} with {bound:?}"
    );
    let bound_id = bound["SessionId"].as_str().unwrap_or_default().to_string();

    let (status, unbound) = start(
        &router,
        &json!({ "WorkGroup": WORK_GROUP, "EngineConfiguration": bare_engine_configuration() }),
    )
    .await;
    assert_eq!(
        status, 200,
        "a programmatic session must start, got {status} with {unbound:?}"
    );
    let unbound_id = unbound["SessionId"].as_str().unwrap_or_default().to_string();

    for (case, notebook_id, expected) in [
        ("the notebook the session was started in", &notebook, vec![bound_id.clone()]),
        ("a notebook nothing started a session in", &elsewhere, Vec::new()),
    ] {
        let (status, listed) = call(
            &router,
            "ListNotebookSessions",
            &json!({ "NotebookId": notebook_id }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the listing must answer 200, got {status} with {listed:?}"
        );
        let reported: Vec<String> = listed["NotebookSessionsList"]
            .as_array()
            .unwrap_or(&Vec::new())
            .iter()
            .filter_map(|entry| entry["SessionId"].as_str().map(str::to_string))
            .collect();
        assert_eq!(
            reported, expected,
            "case {case:?}: expected the sessions {expected:?}, got {listed:?}"
        );
        assert!(
            !reported.contains(&unbound_id),
            "case {case:?}: a session bound to no notebook is listed by none, got {listed:?}"
        );
    }
}

/// The session reaches IDLE because the clock made its resources due, not because anybody
/// read it: a session nobody looked at is idle by the time one that was polled is, and a read
/// changes nothing a later read sees.
#[tokio::test]
async fn a_session_becomes_idle_whether_or_not_it_is_polled() {
    let router = router();
    given_spark_work_group(&router, WORK_GROUP).await;

    let request =
        json!({ "WorkGroup": WORK_GROUP, "EngineConfiguration": bare_engine_configuration() });
    let (status, polled) = start(&router, &request).await;
    assert_eq!(
        status, 200,
        "the polled session must start, got {status} with {polled:?}"
    );
    let (status, unread) = start(&router, &request).await;
    assert_eq!(
        status, 200,
        "the session nothing reads must start, got {status} with {unread:?}"
    );
    let polled_id = polled["SessionId"].as_str().unwrap_or_default().to_string();
    let unread_id = unread["SessionId"].as_str().unwrap_or_default().to_string();

    let first = read_session(&router, "the first read", &polled_id).await;
    let idle = settled_idle(&router, "the polled session", &polled_id).await;
    assert_eq!(
        idle["Status"]["StartDateTime"], first["Status"]["StartDateTime"],
        "the instant a session started does not move as it is read, got {idle:?} after {first:?}"
    );
    assert_eq!(
        idle["Status"]["IdleSinceDateTime"], idle["Status"]["LastModifiedDateTime"],
        "an idle session was last modified when it became idle, got {idle:?}"
    );
    assert!(
        idle["Status"]["IdleSinceDateTime"].as_f64().is_some(),
        "an idle session reports the instant it became idle, got {idle:?}"
    );

    // The session NOTHING read is idle too, on its first read ever: the transition is owed by
    // the clock, so no reader has to make it.
    let never_read = read_session(&router, "the session nothing read", &unread_id).await;
    assert_eq!(
        never_read["Status"]["State"].as_str(),
        Some(SESSION_STATE_IDLE),
        "a session nobody polled is idle when one that was polled is, got {never_read:?}"
    );
}

/// A start needs a Spark enabled, enabled workgroup this account holds, and a notebook
/// binding that names a notebook: every other case is refused as a bad request.
#[tokio::test]
async fn a_start_a_workgroup_cannot_run_is_refused() {
    let router = router();
    given_spark_work_group(&router, WORK_GROUP).await;
    given_spark_work_group(&router, DISABLED_WORK_GROUP).await;
    crate::harness::create_work_group(&router, SQL_WORK_GROUP).await;
    let (status, disabled) = call(
        &router,
        "UpdateWorkGroup",
        &json!({ "WorkGroup": DISABLED_WORK_GROUP, "State": state::WORK_GROUP_STATE_DISABLED }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the workgroup must be disabled, got {status} with {disabled:?}"
    );

    // The rows differ along WHICH precondition fails: the workgroup does not exist, it is
    // disabled, it runs Athena SQL (named, or the account's default), the notebook the start
    // binds the session to does not exist, a notebook version names no notebook at all, or
    // the open collections the start carries are larger than a stored session may hold.
    let oversized_value = "x".repeat(OVERSIZED_PROPERTY_CHARS);
    let oversized_properties: serde_json::Map<String, Value> = OVERSIZED_PROPERTY_KEYS
        .iter()
        .map(|key| ((*key).to_string(), Value::String(oversized_value.clone())))
        .collect();
    let oversized_bytes: usize = OVERSIZED_PROPERTY_KEYS
        .iter()
        .map(|key| key.len() + OVERSIZED_PROPERTY_CHARS)
        .sum();
    let rows: Vec<(&str, Value, &str, String)> = vec![
        (
            "a workgroup this account does not hold",
            json!({ "WorkGroup": "never-created", "EngineConfiguration": {} }),
            ERROR_CODE_INVALID_INPUT,
            state::WORK_GROUP_NOT_FOUND.to_string(),
        ),
        (
            "a disabled workgroup",
            json!({ "WorkGroup": DISABLED_WORK_GROUP, "EngineConfiguration": {} }),
            ERROR_CODE_INVALID_INPUT,
            state::work_group_disabled(DISABLED_WORK_GROUP),
        ),
        (
            "an Athena SQL workgroup",
            json!({ "WorkGroup": SQL_WORK_GROUP, "EngineConfiguration": {} }),
            ERROR_CODE_INVALID_INPUT,
            state::WORK_GROUP_ENGINE_VERSION_NOT_SPARK.to_string(),
        ),
        (
            "the account's default workgroup",
            json!({ "WorkGroup": DEFAULT_WORK_GROUP, "EngineConfiguration": {} }),
            ERROR_CODE_INVALID_INPUT,
            state::WORK_GROUP_ENGINE_VERSION_NOT_SPARK.to_string(),
        ),
        (
            "a notebook no import or create minted",
            json!({
                "WorkGroup": WORK_GROUP,
                "EngineConfiguration": {
                    "AdditionalConfigs": { NOTEBOOK_ID_CONFIG: "11111111-1111-1111-1111-111111111111" },
                },
            }),
            ERROR_CODE_NOTEBOOK_NOT_FOUND,
            state::notebook_not_found("11111111-1111-1111-1111-111111111111"),
        ),
        (
            "a notebook version naming no notebook",
            json!({
                "WorkGroup": WORK_GROUP,
                "NotebookVersion": "Athena notebook version 1",
                "EngineConfiguration": {},
            }),
            ERROR_CODE_INVALID_INPUT,
            state::NOTEBOOK_ID_REQUIRED.to_string(),
        ),
        (
            "Spark properties past the bytes a stored session configuration may hold",
            json!({
                "WorkGroup": WORK_GROUP,
                "EngineConfiguration": { "SparkProperties": oversized_properties },
            }),
            ERROR_CODE_INVALID_INPUT,
            format!(
                "EngineConfiguration and MonitoringConfiguration carry {oversized_bytes} bytes \
                 of Spark properties, classifications and log types, more than the {} bytes a \
                 session configuration can hold.",
                state::MAX_WORK_GROUP_CONFIGURATION_BYTES
            ),
        ),
    ];

    for (case, request, code, expected) in rows {
        let (status, refused) = start(&router, &request).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {refused:?}"
        );
        assert_eq!(
            error_code(&refused),
            "InvalidRequestException",
            "case {case:?}: expected InvalidRequestException, got {refused:?}"
        );
        assert_eq!(
            message(&refused),
            expected,
            "case {case:?}: expected the sentence {expected:?}, got {refused:?}"
        );
        assert_eq!(
            refused["AthenaErrorCode"].as_str(),
            Some(code),
            "case {case:?}: expected AthenaErrorCode {code:?}, got {refused:?}"
        );
        assert_eq!(
            refused["ErrorCode"].as_str(),
            Some(code),
            "case {case:?}: expected the same code under ErrorCode, got {refused:?}"
        );
    }
}

/// `ClientRequestToken` makes a start idempotent: a repeat under one token answers with the
/// session that token already started, and the same token for other parameters describes
/// another session, which is refused.
#[tokio::test]
async fn one_client_request_token_starts_one_session() {
    let router = router();
    given_spark_work_group(&router, WORK_GROUP).await;

    let token = request_token("session-idempotency");
    let request = json!({
        "WorkGroup": WORK_GROUP,
        "Description": "one session",
        "SessionIdleTimeoutInMinutes": SHORTEST_IDLE_TIMEOUT_MINUTES,
        "ExecutionRole": SESSION_ROLE,
        "Tags": [{ "Key": "owner", "Value": "data-platform" }],
        "EngineConfiguration": { "MaxConcurrentDpus": FEWEST_CONCURRENT_DPUS },
        "ClientRequestToken": token,
    });

    let (status, first) = start(&router, &request).await;
    assert_eq!(
        status, 200,
        "the first start must be accepted, got {status} with {first:?}"
    );
    let (status, again) = start(&router, &request).await;
    assert_eq!(
        status, 200,
        "a start repeated under one token executes once and answers the same way, got {status} \
         with {again:?}"
    );
    assert_eq!(
        again["SessionId"], first["SessionId"],
        "the repeat answers with the session the token already started, got {again:?} after \
         {first:?}"
    );

    // The rows differ along WHICH parameter the reused token changed: each one describes
    // another session, so the token is not free for it.
    let changed: Vec<(&str, Value)> = vec![
        ("the description", json!("another session")),
        ("the idle timeout", json!(null)),
        ("the execution role", json!(null)),
    ];
    for (case, description) in &changed {
        let mut other = request.clone();
        match *case {
            "the description" => other["Description"] = description.clone(),
            "the idle timeout" => {
                other["SessionIdleTimeoutInMinutes"] = json!(LONGEST_IDLE_TIMEOUT_MINUTES)
            }
            _ => other["ExecutionRole"] = json!(WORK_GROUP_ROLE),
        }
        let (status, refused) = start(&router, &other).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected a reused token under changed parameters to be refused, \
             got {status} with {refused:?}"
        );
        assert_eq!(
            message(&refused),
            state::IDEMPOTENT_PARAMETERS_DO_NOT_MATCH,
            "case {case:?}: expected the idempotency refusal, got {refused:?}"
        );
    }

    // A DIFFERENT token starts another session, and the first one is still the one its id
    // resolves to.
    let mut other = request.clone();
    other["ClientRequestToken"] = json!(request_token("another-session"));
    let (status, second) = start(&router, &other).await;
    assert_eq!(
        status, 200,
        "another token must start another session, got {status} with {second:?}"
    );
    assert_ne!(
        second["SessionId"], first["SessionId"],
        "two tokens start two sessions, got {second:?} and {first:?}"
    );
    let first_id = first["SessionId"].as_str().unwrap_or_default();
    let found = read_session(&router, "the session the token started", first_id).await;
    assert_eq!(
        found["Description"].as_str(),
        Some("one session"),
        "the session the token started still carries what that start persisted, got {found:?}"
    );

    // The parameters the comparison is about are the REQUEST's: a workgroup updated between
    // two identical starts changes what a session would run under and changes nothing the
    // request asked for, so the repeat is still answered with the session the token started.
    let (status, updated) = call(
        &router,
        "UpdateWorkGroup",
        &json!({
            "WorkGroup": WORK_GROUP,
            "ConfigurationUpdates": {
                "ResultConfigurationUpdates": { "OutputLocation": "s3://athena-results/moved/" },
                "ExecutionRole": "arn:aws:iam::000000000000:role/AthenaSparkExecutionMoved",
            },
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the workgroup must be updated, got {status} with {updated:?}"
    );
    let (status, after_update) = start(&router, &request).await;
    assert_eq!(
        status, 200,
        "the repeat after a workgroup update must still be answered, got {status} with \
         {after_update:?}"
    );
    assert_eq!(
        after_update["SessionId"], first["SessionId"],
        "the repeat answers with the session the token started, got {after_update:?} after \
         {first:?}"
    );
}

/// A `SessionId` nothing was started under names no session to read.
#[tokio::test]
async fn reading_a_session_that_was_never_started_is_refused() {
    let router = router();
    given_spark_work_group(&router, WORK_GROUP).await;

    // The rows differ along the SHAPE of the id: one of the form this service mints, the
    // shortest the model admits, the longest it admits, and one carrying no id-like text at
    // all. The refusal quotes the id back for none of them, which only an id long enough not
    // to occur inside the sentence itself can show.
    let ids = [
        (
            "an id of the minted form",
            "3f2504e0-4f89-41d3-9a0c-0305e82c3301".to_string(),
            true,
        ),
        ("the shortest id the model admits", "a".to_string(), false),
        ("the longest id the model admits", "s".repeat(256), true),
        ("text that is no session id", "not a session".to_string(), true),
    ];

    for (case, session_id, quotable) in &ids {
        let (status, refused) = session(&router, session_id).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {refused:?}"
        );
        assert_eq!(
            error_code(&refused),
            "InvalidRequestException",
            "case {case:?}: expected InvalidRequestException, got {refused:?}"
        );
        assert_eq!(
            message(&refused),
            state::SESSION_NOT_FOUND,
            "case {case:?}: expected Athena's refusal for a session id that resolves to \
             nothing, got {refused:?}"
        );
        assert!(
            !quotable || !message(&refused).contains(session_id.as_str()),
            "case {case:?}: the refusal quotes no session id back, got {refused:?}"
        );
    }
}
