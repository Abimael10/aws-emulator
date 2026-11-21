// SPDX-License-Identifier: CC0-1.0

//! How Athena refuses the Spark surface's two kinds of bad reference, driven end to end over
//! the same router the emulator boots (awsJson1_1, `x-amz-target: AmazonAthena.<Operation>`).
//!
//! THE RULES UNDER TEST:
//!
//! 1. An interactive session exists only in a Spark enabled workgroup, so an operation that
//!    resolves a `WorkGroup` to reach its sessions refuses a workgroup whose selected engine
//!    version is not of the Spark family — about the ENGINE VERSION, in the fixed sentence
//!    this service states for that class — rather than reporting that it holds none.
//! 2. A request that addresses ONE session or ONE calculation by an id this account holds no
//!    such resource under is refused with HTTP 400 `InvalidRequestException` whose class is
//!    the reference class `UNAUTHORIZED` — not the malformed-request class — under both of
//!    the names the wire gives a refusal's code, with the fixed sentence that discloses
//!    nothing about which ids name resources.
//!
//! Each rule is asserted over a TABLE rather than one request: the engine-version rows are
//! the catalog of selectable versions this service publishes
//! (`state::SELECTABLE_ENGINE_VERSIONS`) plus the workgroup forms that carry no selection at
//! all, and the reference rows span the shape and the length of an id, which resource it
//! names and which operation resolves it. Every table carries the positive control beside
//! the refusals, so what each row asserts is the reference and not the operation refusing
//! everything.
//!
//! Nothing here waits for a fixed span: the one transition the clock owes is waited on by
//! POLLING for the state itself, under a deadline far above the span it is due after.

use std::time::Duration;

use serde_json::{json, Value};
use service_com_amazonaws_athena_amazonathena::state::{
    engine_version_category, CALCULATION_NOT_FOUND, DEFAULT_WORK_GROUP, ENGINE_CATEGORY_SPARK,
    ERROR_CODE_INVALID_INPUT, ERROR_CODE_UNAUTHORIZED, NOTEBOOK_ID_CONFIG, PYSPARK_ENGINE_VERSION,
    SELECTABLE_ENGINE_VERSIONS, SESSION_NOT_FOUND, SESSION_STATE_IDLE,
    WORK_GROUP_ENGINE_VERSION_NOT_SPARK, WORK_GROUP_NOT_FOUND,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{assert_refusal, call, router};

/// The execution role a Spark workgroup accesses user resources through, inside the modelled
/// `RoleArn` pattern.
const EXECUTION_ROLE: &str = "arn:aws:iam::000000000000:role/AthenaSparkExecution";

/// Where the workgroups below write their results.
const OUTPUT_LOCATION: &str = "s3://athena-spark-references/results/";

/// How long a poll for a transition the clock owes keeps looking, and how long it waits
/// between looks. The deadline is far above the span a session's resources are due after, so
/// the CONDITION is what is waited on rather than a span of the host's own speed.
const POLL_INTERVAL: Duration = Duration::from_millis(50);
/// See [`POLL_INTERVAL`].
const POLL_LOOKS: u32 = 1_200;

/// One session-addressed operation of this surface: its name, and the request body it takes
/// for one `SessionId`.
type SessionAddressed = (&'static str, fn(&str) -> Value);

/// Create the workgroup `name` carrying `configuration`, or carrying none at all.
async fn given_work_group(router: &MultiServiceRouter, name: &str, configuration: Option<Value>) {
    let mut body = json!({ "Name": name });
    if let (Some(body), Some(configuration)) = (body.as_object_mut(), configuration) {
        body.insert("Configuration".to_string(), configuration);
    }
    let (status, created) = call(router, "CreateWorkGroup", &body).await;
    assert_eq!(
        status, 200,
        "the workgroup {name} must be created, got {status} with {created:?}"
    );
}

/// The configuration of a workgroup that selects `engine_version`.
fn selecting(engine_version: &str) -> Value {
    json!({
        "EngineVersion": { "SelectedEngineVersion": engine_version },
        "ResultConfiguration": { "OutputLocation": OUTPUT_LOCATION }
    })
}

/// One `ListSessions` call for `work_group`, carrying `extra` members as well.
async fn list_sessions(
    router: &MultiServiceRouter,
    work_group: &str,
    extra: Value,
) -> (u16, Value) {
    let mut body = json!({ "WorkGroup": work_group });
    if let (Some(body), Some(extra)) = (body.as_object_mut(), extra.as_object()) {
        for (member, value) in extra {
            body.insert(member.clone(), value.clone());
        }
    }
    call(router, "ListSessions", &body).await
}

/// Start one session in `work_group`, bound to `notebook_id` when one is given, and hand back
/// its `SessionId`.
async fn given_session(
    router: &MultiServiceRouter,
    work_group: &str,
    notebook_id: Option<&str>,
) -> String {
    let mut engine_configuration = json!({
        "CoordinatorDpuSize": 1,
        "MaxConcurrentDpus": 4,
        "DefaultExecutorDpuSize": 1
    });
    if let (Some(configuration), Some(notebook_id)) =
        (engine_configuration.as_object_mut(), notebook_id)
    {
        configuration.insert(
            "AdditionalConfigs".to_string(),
            json!({ NOTEBOOK_ID_CONFIG: notebook_id }),
        );
    }
    let (status, started) = call(
        router,
        "StartSession",
        &json!({
            "WorkGroup": work_group,
            "EngineConfiguration": engine_configuration
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "StartSession in {work_group} must be accepted, got {status} with {started:?}"
    );
    started["SessionId"]
        .as_str()
        .unwrap_or_else(|| panic!("StartSession must answer with a SessionId, got {started:?}"))
        .to_string()
}

/// Poll `GetSessionStatus` until the session `session_id` reports [`SESSION_STATE_IDLE`], the
/// state it accepts a calculation in.
///
/// The CONDITION is what is waited on: every look asks the service which state the session is
/// in, so a loaded host cannot turn a slow transition into a failure.
async fn settled_idle(router: &MultiServiceRouter, session_id: &str) {
    for _ in 0..POLL_LOOKS {
        let (status, read) = call(
            router,
            "GetSessionStatus",
            &json!({ "SessionId": session_id }),
        )
        .await;
        assert_eq!(
            status, 200,
            "the session {session_id} must be readable, got {status} with {read:?}"
        );
        if read["Status"]["State"].as_str() == Some(SESSION_STATE_IDLE) {
            return;
        }
        ::tokio::time::sleep(POLL_INTERVAL).await;
    }
    panic!("the session {session_id} never reached {SESSION_STATE_IDLE}");
}

/// RULE 1: a session listing resolves its workgroup's engine version, and a workgroup that
/// does not run the Spark engine hosts no interactive session — so the listing is refused
/// about that engine version instead of reporting an empty set.
#[tokio::test]
async fn a_session_listing_refuses_a_workgroup_that_runs_no_spark_engine() {
    let router = router();

    // The rows this service's OWN catalog of selectable engine versions supplies
    // (`state::SELECTABLE_ENGINE_VERSIONS`, the set `ListEngineVersions` publishes): one
    // workgroup per selectable version, each expected to host sessions exactly where the
    // version it selected is of the Spark family.
    let mut rows: Vec<(String, String, Option<&str>)> = Vec::new();
    for (index, version) in SELECTABLE_ENGINE_VERSIONS.iter().enumerate() {
        let name = format!("engine-version-{index}");
        given_work_group(&router, &name, Some(selecting(version))).await;
        let refusal = (engine_version_category(version) != ENGINE_CATEGORY_SPARK)
            .then_some(WORK_GROUP_ENGINE_VERSION_NOT_SPARK);
        rows.push((format!("a workgroup selecting {version:?}"), name, refusal));
    }

    // Beside the catalog: the workgroup forms that select no engine version at all.
    given_work_group(&router, "no-configuration", None).await;
    rows.push((
        "a workgroup created with no configuration at all".to_string(),
        "no-configuration".to_string(),
        Some(WORK_GROUP_ENGINE_VERSION_NOT_SPARK),
    ));
    rows.push((
        "the account's own default workgroup, which no request created".to_string(),
        DEFAULT_WORK_GROUP.to_string(),
        Some(WORK_GROUP_ENGINE_VERSION_NOT_SPARK),
    ));
    given_work_group(
        &router,
        "spark-by-execution-role",
        Some(json!({
            "ExecutionRole": EXECUTION_ROLE,
            "ResultConfiguration": { "OutputLocation": OUTPUT_LOCATION }
        })),
    )
    .await;
    rows.push((
        "a workgroup Spark enabled by a member rather than by its engine version".to_string(),
        "spark-by-execution-role".to_string(),
        None,
    ));

    for (case, work_group, refusal) in &rows {
        let (status, answered) = list_sessions(&router, work_group, json!({})).await;
        match refusal {
            Some(expected) => assert_refusal(
                case,
                status,
                &answered,
                ERROR_CODE_INVALID_INPUT,
                expected,
            ),
            None => assert_eq!(
                (status, answered["Sessions"].is_array()),
                (200, true),
                "case {case:?}: a Spark enabled workgroup must be listed, got {status} with \
                 {answered:?}"
            ),
        }
    }

    // The refusal is decided before anything is listed, so the members that would narrow a
    // listing make no difference to it.
    let narrowing = [
        ("a state filter", json!({ "StateFilter": SESSION_STATE_IDLE })),
        ("a page size", json!({ "MaxResults": 1 })),
        (
            "a filter and a page size together",
            json!({ "StateFilter": SESSION_STATE_IDLE, "MaxResults": 100 }),
        ),
    ];
    for (case, extra) in narrowing {
        let (status, refused) = list_sessions(&router, DEFAULT_WORK_GROUP, extra).await;
        assert_refusal(
            case,
            status,
            &refused,
            ERROR_CODE_INVALID_INPUT,
            WORK_GROUP_ENGINE_VERSION_NOT_SPARK,
        );
    }

    // A workgroup this account does not hold at all is the OTHER class: the miss is resolved
    // before any engine version can be, so the two refusals do not collapse into one.
    let (status, refused) = list_sessions(&router, "never-created", json!({})).await;
    assert_refusal(
        "a workgroup this account does not hold",
        status,
        &refused,
        ERROR_CODE_INVALID_INPUT,
        WORK_GROUP_NOT_FOUND,
    );

    // And the Spark workgroup of the same account lists the session it holds, so the rows
    // above are about the engine version and not about this operation refusing everything.
    let spark = "spark-sessions";
    given_work_group(&router, spark, Some(selecting(PYSPARK_ENGINE_VERSION))).await;
    let session_id = given_session(&router, spark, None).await;
    let (status, listed) = list_sessions(&router, spark, json!({})).await;
    let empty = Vec::new();
    let found: Vec<&str> = listed["Sessions"]
        .as_array()
        .unwrap_or(&empty)
        .iter()
        .filter_map(|summary| summary["SessionId"].as_str())
        .collect();
    assert_eq!(
        (status, found.contains(&session_id.as_str())),
        (200, true),
        "case \"the Spark workgroup's own session\": expected {session_id} listed, got {status} \
         with {listed:?}"
    );
}

/// RULE 2: every request addressing a session or a calculation this account holds none of
/// carries the reference class, whatever the id looks like and whichever operation resolved
/// it.
#[tokio::test]
async fn an_unheld_session_or_calculation_reference_carries_the_unauthorized_class() {
    let router = router();
    let work_group = "spark-references";
    given_work_group(&router, work_group, Some(selecting(PYSPARK_ENGINE_VERSION))).await;
    let (status, created) = call(
        &router,
        "CreateNotebook",
        &json!({ "WorkGroup": work_group, "Name": "references" }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the notebook must be created, got {status} with {created:?}"
    );
    let notebook_id = created["NotebookId"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateNotebook must answer with an id, got {created:?}"))
        .to_string();
    // A session and a calculation this account DOES hold exist throughout, so each refusal
    // below is about the id the request carried rather than about an empty account.
    let session_id = given_session(&router, work_group, Some(&notebook_id)).await;
    settled_idle(&router, &session_id).await;
    let (status, submitted) = call(
        &router,
        "StartCalculationExecution",
        &json!({ "SessionId": session_id, "CodeBlock": "1" }),
    )
    .await;
    assert_eq!(
        status, 200,
        "a calculation must be submitted into the live session, got {status} with {submitted:?}"
    );
    let calculation_id = submitted["CalculationExecutionId"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("StartCalculationExecution must answer with an id, got {submitted:?}")
        })
        .to_string();

    // `SessionId` is 1 to 256 characters: the rows span both published bounds, the form this
    // service mints, a form of another shape, an id of another resource, and one character
    // away from the session this account does hold.
    let session_rows: Vec<(String, String)> = vec![
        (
            "a uuid nothing was started under".to_string(),
            "00000000-0000-4000-8000-000000000000".to_string(),
        ),
        (
            "an id at the shortest the model admits".to_string(),
            "x".to_string(),
        ),
        (
            "an id at the longest the model admits".to_string(),
            "s".repeat(256),
        ),
        (
            "an id of another shape".to_string(),
            "sessions/none-of-ours_42".to_string(),
        ),
        ("the notebook's own id".to_string(), notebook_id.clone()),
        (
            "one character past a held session's id".to_string(),
            format!("{session_id}z"),
        ),
    ];
    // The operations that resolve ONE session, and the body each takes for an id.
    let session_addressed: Vec<SessionAddressed> = vec![
        ("GetSession", |id| json!({ "SessionId": id })),
        ("GetSessionStatus", |id| json!({ "SessionId": id })),
        ("TerminateSession", |id| json!({ "SessionId": id })),
        ("ListExecutors", |id| json!({ "SessionId": id })),
        ("StartCalculationExecution", |id| {
            json!({ "SessionId": id, "CodeBlock": "1" })
        }),
        ("CreatePresignedNotebookUrl", |id| {
            json!({ "SessionId": id })
        }),
    ];
    for (row, unknown) in &session_rows {
        assert_ne!(
            unknown, &session_id,
            "case {row:?}: the unknown id must not be the held session's"
        );
        for (operation, body) in &session_addressed {
            let case = format!("{operation} with {row}");
            let (status, refused) = call(&router, operation, &body(unknown)).await;
            assert_refusal(
                &case,
                status,
                &refused,
                ERROR_CODE_UNAUTHORIZED,
                SESSION_NOT_FOUND,
            );
        }
    }

    // `CalculationExecutionId` is 1 to 36 characters, so its rows span ITS bounds: the form
    // this service mints, both published lengths, another resource's id, and one character
    // changed in the calculation this account does hold.
    let changed = format!(
        "{}{}",
        if calculation_id.starts_with('a') { 'b' } else { 'a' },
        &calculation_id[1..]
    );
    let calculation_rows: Vec<(String, String)> = vec![
        (
            "a uuid nothing was minted under".to_string(),
            "00000000-0000-4000-8000-000000000000".to_string(),
        ),
        (
            "an id at the shortest the model admits".to_string(),
            "c".to_string(),
        ),
        (
            "an id at the longest the model admits".to_string(),
            "c".repeat(36),
        ),
        ("the session's own id".to_string(), session_id.clone()),
        ("the notebook's own id".to_string(), notebook_id.clone()),
        (
            "one character changed in a held calculation's id".to_string(),
            changed,
        ),
    ];
    for (row, unknown) in &calculation_rows {
        assert_ne!(
            unknown, &calculation_id,
            "case {row:?}: the unknown id must not be the held calculation's"
        );
        let case = format!("GetCalculationExecutionCode with {row}");
        let (status, refused) = call(
            &router,
            "GetCalculationExecutionCode",
            &json!({ "CalculationExecutionId": unknown }),
        )
        .await;
        assert_refusal(
            &case,
            status,
            &refused,
            ERROR_CODE_UNAUTHORIZED,
            CALCULATION_NOT_FOUND,
        );
    }

    // The positive control in the same account: every one of those operations answers for the
    // ids this account DOES hold, so the refusals above are about the reference. The
    // terminate is left out of this pass, since it is the one call that would end the
    // session the others read.
    for (operation, body) in &session_addressed {
        if *operation == "TerminateSession" {
            continue;
        }
        let (status, answered) = call(&router, operation, &body(&session_id)).await;
        assert_eq!(
            status, 200,
            "case \"{operation} with the held session\": expected an answer, got {status} with \
             {answered:?}"
        );
    }
    let (status, read) = call(
        &router,
        "GetCalculationExecutionCode",
        &json!({ "CalculationExecutionId": calculation_id }),
    )
    .await;
    assert_eq!(
        (status, read["CodeBlock"].as_str()),
        (200, Some("1")),
        "case \"the held calculation's code\": expected the submitted code back, got {status} \
         with {read:?}"
    );
}
