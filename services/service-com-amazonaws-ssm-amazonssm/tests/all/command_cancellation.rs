// SPDX-License-Identifier: CC0-1.0

//! `CancelCommand` end to end: the cancel of a command a caller sent, and what every later
//! read of that command and of its invocations reports because of it.
//!
//! RULE: a cancel stops the work of one command that is still RUNNING — every invocation of
//! it when the request names no node, and the invocations of the nodes it does name — so a
//! cancelled invocation reports `Cancelling` until the cancel settles and `Cancelled` after
//! it with no step result at all, a command cancelled on every node it was requested on
//! reports the same, and anything that had already reached a terminal status keeps the
//! status it reached. A call that resolves no command of the endpoint it was signed for is
//! refused `InvalidCommandId` NAMING the id that resolved none, the way every operation of
//! this slice that resolves a command id words that refusal.
//!
//! Every case goes through the front door a client uses — `POST /` with
//! `X-Amz-Target: AmazonSSM.<Operation>` and an `awsJson1_1` body, dispatched by the
//! multi-service router (`OPERATIONS.md`, "Wire protocol"): the commands cancelled here
//! were persisted by a `SendCommand` through that same door and are read back through
//! `ListCommands`, `ListCommandInvocations` and `GetCommandInvocation`.
//!
//! Two parts of the fixture are seeded through the service's own state rather than through
//! an API. The node inventory is, because a node enters it only by REGISTERING and no
//! public API of this service registers one (the sibling `run_command` and `command_history`
//! suites seed it the same way). The TERMINAL invocation statuses are, because an SSM
//! Agent's poll from the node is what moves an invocation to one and this fixture runs no
//! guest for its nodes — so the status a finished invocation holds is written onto the
//! record the send persisted, and the cancel still acts on what the store holds.
//!
//! The suites are tables. Each row is one input along the axis the rule generalises over —
//! how the send selected its nodes, how many nodes it resolved, how many of them the cancel
//! names, which terminal status an invocation had already reached, and which identity a
//! cancel cannot act on — rather than the single cancel the recorded units happened to make.
//!
//! Nothing here asserts on the wall clock: a case that waits for the cancel to settle LOOKS
//! AGAIN until the status it waits for is there or a deadline far longer than the span
//! passes.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_ssm_amazonssm::state::{
    commands, ComAmazonawsSsmAmazonssmState, ManagedNodeRecord, RESOURCE_TYPE_EC2_INSTANCE,
    RESOURCE_TYPE_MANAGED_INSTANCE,
};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request signs for unless a case says otherwise. Deliberately not the
/// emulator's default, so a cancel served here proves the Region came from the request's
/// SigV4 credential scope.
const REGION: &str = "us-west-2";

/// A second Region, to pin that a command is cancellable only through the Region it was
/// sent to.
const OTHER_REGION: &str = "us-east-1";

/// The `X-Amz-Target` prefix from the crate's `OPERATIONS.md` wire-protocol section.
const TARGET_PREFIX: &str = "AmazonSSM";

/// An online EC2 node running Linux (`InstanceId` admits an 8-character suffix).
const LINUX: &str = "i-0abc1234";

/// An online EC2 node running Windows (`InstanceId` admits a 17-character suffix).
const WINDOWS: &str = "i-0123456789abcdef0";

/// An online hybrid node running macOS (`mi-` and 17 characters).
const HYBRID: &str = "mi-01234567890123456";

/// A registered, online node nothing below is sent to.
const IDLE: &str = "i-0fff5555";

/// A registered node whose agent has stopped reporting.
const OFFLINE: &str = "i-0dead111";

/// A well-formed node id no node is registered under.
const UNREGISTERED: &str = "i-0dead999";

/// A node registered only in [`OTHER_REGION`].
const OTHER_ONLY: &str = "i-0eeee222";

/// A command document Systems Manager publishes whose steps are restricted to Linux and
/// macOS, and which declares one parameter with no default (`commands`).
const SHELL: &str = "AWS-RunShellScript";

/// A command document Systems Manager publishes that declares no step precondition, so it
/// runs on every platform, and whose every parameter has a default.
const AGENT: &str = "AWS-UpdateSSMAgent";

/// A `CommandId` of the modelled shape (exactly 36 characters, the hyphenated UUID form)
/// that no send below minted.
const UNKNOWN_COMMAND: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";

/// How long a case looks for a cancel to settle, far longer than the span the clock owes
/// that move after — so the case does not depend on how fast the host runs.
const SETTLE_DEADLINE: Duration = Duration::from_secs(60);

/// How often it looks again.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// A router with just this service mounted, and the engine it was mounted on — the handle a
/// case seeds the node inventory and a finished invocation's status through.
fn service() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Signatures are not what these tests are about; the router still reads the SigV4
    // credential scope below, which is where the Region comes from.
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_ssm_amazonssm::EmulatedService::new(
        Arc::clone(&engine),
    ));
    (router, engine)
}

/// Call `operation` signed for `region`, returning `(status, body)`.
async fn call_in_region(
    router: &MultiServiceRouter,
    region: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{TARGET_PREFIX}.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTEST/20240101/{region}/ssm/aws4_request, \
                 SignedHeaders=host;x-amz-target, Signature=0000"
            ),
        )
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a valid test request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let error_type = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let raw = response.into_body();
    let mut parsed: Value = if raw.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&raw)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&raw).to_string()))
    };
    // The error shape travels in `__type`; fold in the header the framework also sets so
    // one accessor reads either.
    if let (Some(error_type), Value::Object(map)) = (error_type, &mut parsed) {
        map.entry("__type")
            .or_insert_with(|| Value::String(error_type));
    }
    (status, parsed)
}

/// Call `operation` signed for [`REGION`].
async fn call(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    call_in_region(router, REGION, operation, body).await
}

/// The wire error code of a failure body (`__type`, without its namespace).
fn error_code(body: &Value) -> String {
    let rendered = body["__type"].as_str().unwrap_or("<no __type>");
    rendered
        .split('#')
        .next_back()
        .unwrap_or(rendered)
        .to_string()
}

/// The refusal message of a failure body.
fn error_message(body: &Value) -> String {
    body["message"]
        .as_str()
        .or_else(|| body["Message"].as_str())
        .unwrap_or("<no message>")
        .to_string()
}

/// Register `record` in `region` the way a node's own registration would.
fn register(engine: &Arc<engine::InMemoryEngine>, region: &str, record: ManagedNodeRecord) {
    assert!(
        engine
            .state::<ComAmazonawsSsmAmazonssmState>()
            .lock()
            .insert_managed_node(region, record),
        "a seeded node must carry the node id it is stored under"
    );
}

/// One registered node: its id, kind, platform, ping status and tags.
fn node(
    id: &str,
    resource_type: &str,
    platform: &str,
    ping_status: &str,
    tags: &[(&str, &str)],
) -> ManagedNodeRecord {
    let at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let mut record = ManagedNodeRecord::registering(id, resource_type, at);
    record.properties.platform_type = Some(platform.to_string());
    record.properties.ping_status = Some(ping_status.to_string());
    record.properties.computer_name = Some(format!("{id}.compute.internal"));
    record.tags = tags
        .iter()
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect::<BTreeMap<String, String>>();
    record
}

/// The inventory every suite below sends against: nodes spanning node kind, platform and
/// tag, one node nothing is sent to, one whose agent has stopped reporting, and one of
/// another Region.
fn seed_inventory(engine: &Arc<engine::InMemoryEngine>) {
    register(
        engine,
        REGION,
        node(
            LINUX,
            RESOURCE_TYPE_EC2_INSTANCE,
            "Linux",
            "Online",
            &[("env", "dev")],
        ),
    );
    register(
        engine,
        REGION,
        node(
            WINDOWS,
            RESOURCE_TYPE_EC2_INSTANCE,
            "Windows",
            "Online",
            &[("env", "prod")],
        ),
    );
    register(
        engine,
        REGION,
        node(
            HYBRID,
            RESOURCE_TYPE_MANAGED_INSTANCE,
            "MacOS",
            "Online",
            &[("env", "qa")],
        ),
    );
    register(
        engine,
        REGION,
        node(
            IDLE,
            RESOURCE_TYPE_EC2_INSTANCE,
            "Linux",
            "Online",
            &[("env", "idle")],
        ),
    );
    register(
        engine,
        REGION,
        node(
            OFFLINE,
            RESOURCE_TYPE_EC2_INSTANCE,
            "Linux",
            "ConnectionLost",
            &[("env", "offline")],
        ),
    );
    register(
        engine,
        OTHER_REGION,
        node(
            OTHER_ONLY,
            RESOURCE_TYPE_EC2_INSTANCE,
            "Linux",
            "Online",
            &[("env", "dev")],
        ),
    );
}

/// Send one command signed for `region` and answer the id it minted.
async fn send_in_region(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    body: &Value,
) -> String {
    let (status, parsed) = call_in_region(router, region, "SendCommand", body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the send to be served with HTTP 200, got {status} with \
         {parsed:?}"
    );
    parsed["Command"]["CommandId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a CommandId, got {parsed:?}"))
        .to_string()
}

/// Send one command signed for [`REGION`].
async fn send(router: &MultiServiceRouter, case: &str, body: &Value) -> String {
    send_in_region(router, case, REGION, body).await
}

/// Call `CancelCommand` signed for [`REGION`].
async fn cancel(router: &MultiServiceRouter, body: &Value) -> (u16, Value) {
    call(router, "CancelCommand", body).await
}

/// Cancel a command that must be served, and pin that the result carries no member of its
/// own ("whether or not the command was successfully canceled" is the 200 itself).
async fn cancelled(router: &MultiServiceRouter, case: &str, body: &Value) {
    let (status, parsed) = cancel(router, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the cancel to be served with HTTP 200, got {status} with \
         {parsed:?}"
    );
    assert!(
        parsed.is_null() || parsed.as_object().is_some_and(|members| members.is_empty()),
        "case {case:?}: expected a result with no member, got {parsed:?}"
    );
}

/// The `Command` one command id names, read back through `ListCommands`.
async fn listed(router: &MultiServiceRouter, case: &str, command_id: &str) -> Value {
    let (status, parsed) = call(router, "ListCommands", &json!({"CommandId": command_id})).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the history to be served with HTTP 200, got {status} with \
         {parsed:?}"
    );
    parsed["Commands"]
        .as_array()
        .and_then(|commands| commands.first())
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: expected command {command_id:?}, got {parsed:?}"))
}

/// The `Status` that command reports.
async fn command_status(router: &MultiServiceRouter, case: &str, command_id: &str) -> String {
    listed(router, case, command_id).await["Status"]
        .as_str()
        .unwrap_or("<unset>")
        .to_string()
}

/// One node's invocation of a command, read back through `GetCommandInvocation`.
async fn invocation(
    router: &MultiServiceRouter,
    case: &str,
    command_id: &str,
    instance_id: &str,
) -> Value {
    let (status, parsed) = call(
        router,
        "GetCommandInvocation",
        &json!({"CommandId": command_id, "InstanceId": instance_id}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the invocation of {instance_id} to be served with HTTP \
         200, got {status} with {parsed:?}"
    );
    parsed
}

/// The `Status` one node's invocation reports.
async fn invocation_status(
    router: &MultiServiceRouter,
    case: &str,
    command_id: &str,
    instance_id: &str,
) -> String {
    invocation(router, case, command_id, instance_id).await["Status"]
        .as_str()
        .unwrap_or("<unset>")
        .to_string()
}

/// Look at the command until it reports `target`, or the deadline passes.
async fn command_reaches(router: &MultiServiceRouter, case: &str, command_id: &str, target: &str) {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let held = command_status(router, case, command_id).await;
        if held == target {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected command {command_id} to reach {target:?} with nothing \
             but the clock running, got {held:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Look at one node's invocation until it reports `target`, or the deadline passes.
async fn invocation_reaches(
    router: &MultiServiceRouter,
    case: &str,
    command_id: &str,
    instance_id: &str,
    target: &str,
) {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let held = invocation_status(router, case, command_id, instance_id).await;
        if held == target {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the invocation of {instance_id} to reach {target:?} \
             with nothing but the clock running, got {held:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Reach the stored command `id` of [`REGION`] and let `edit` write on it.
fn with_record(
    engine: &Arc<engine::InMemoryEngine>,
    id: &str,
    edit: impl FnOnce(&mut commands::CommandRecord),
) {
    let state = engine.state::<ComAmazonawsSsmAmazonssmState>();
    let mut store = state.lock();
    let record = store
        .command_mut(REGION, id)
        .unwrap_or_else(|| panic!("the send persisted command {id:?} in {REGION:?}"));
    edit(record);
}

/// Write onto the stored records the status an agent's poll would have moved the command and
/// one of its invocations to.
fn set_finished(
    engine: &Arc<engine::InMemoryEngine>,
    id: &str,
    instance_id: &str,
    status: &str,
    command_status: &str,
) {
    with_record(engine, id, |record| {
        record.status = command_status.to_string();
        record.status_details = command_status.to_string();
        let invocation = record
            .invocations
            .get_mut(instance_id)
            .unwrap_or_else(|| panic!("the send created an invocation for {instance_id:?}"));
        invocation.status = status.to_string();
        invocation.status_details = status.to_string();
    });
}

/// One row of the whole-command table: what a failing assertion calls it, the send to make,
/// and the nodes that send must resolve to.
struct CancelRow {
    case: &'static str,
    body: Value,
    nodes: Vec<&'static str>,
}

/// RULE: a cancel that names no node cancels the command on every node it was requested on,
/// whichever way the send selected them and however many it resolved.
#[tokio::test]
async fn a_cancel_stops_the_command_on_every_node_it_was_requested_on() {
    let (router, engine) = service();
    seed_inventory(&engine);

    // One row per shape of send: a named node, several named nodes, nodes described by a
    // tag, and criteria that describe no node at all, which leaves the command with no
    // invocation to cancel.
    let rows = vec![
        CancelRow {
            case: "one named node",
            body: json!({
                "DocumentName": SHELL,
                "InstanceIds": [LINUX],
                "Parameters": {"commands": ["echo one"]},
            }),
            nodes: vec![LINUX],
        },
        CancelRow {
            case: "two named nodes",
            body: json!({
                "DocumentName": SHELL,
                "InstanceIds": [LINUX, HYBRID],
                "Parameters": {"commands": ["echo two"]},
            }),
            nodes: vec![HYBRID, LINUX],
        },
        CancelRow {
            case: "a node described by a tag",
            body: json!({
                "DocumentName": AGENT,
                "Targets": [{"Key": "tag:env", "Values": ["prod"]}],
            }),
            nodes: vec![WINDOWS],
        },
        CancelRow {
            case: "criteria that describe no node",
            body: json!({
                "DocumentName": AGENT,
                "Targets": [{"Key": "tag:env", "Values": ["nothing-is-tagged-this"]}],
            }),
            nodes: vec![],
        },
    ];

    // Every row is sent and cancelled before any one is polled, so the spans the clock owes
    // run together rather than one after another.
    let mut sent: Vec<(&CancelRow, String)> = Vec::new();
    for row in &rows {
        let id = send(&router, row.case, &row.body).await;
        cancelled(&router, row.case, &json!({"CommandId": id})).await;
        // The cancel is RECORDED by the call itself: the command is being cancelled, and
        // the only other status it can hold now is the settled one.
        let held = command_status(&router, row.case, &id).await;
        assert!(
            held == "Cancelling" || held == "Cancelled",
            "case {:?}: expected the cancel to be recorded as Cancelling or Cancelled, got \
             {held:?}",
            row.case
        );
        sent.push((row, id));
    }

    for (row, id) in &sent {
        let case = row.case;
        command_reaches(&router, case, id, "Cancelled").await;
        for instance_id in &row.nodes {
            invocation_reaches(&router, case, id, instance_id, "Cancelled").await;
            // "Cancelled: The command was terminated before it was completed", so there is
            // no finished step to report an exit code or any output for.
            let reported = invocation(&router, case, id, instance_id).await;
            assert_eq!(
                reported["StatusDetails"].as_str().unwrap_or("<unset>"),
                "Cancelled",
                "case {case:?}: expected the detailed status of {instance_id} to be \
                 Cancelled, got {reported:?}"
            );
            assert_eq!(
                reported["ResponseCode"].as_i64().unwrap_or_default(),
                i64::from(commands::NO_RESPONSE_CODE),
                "case {case:?}: expected no exit code for the cancelled invocation of \
                 {instance_id}, got {reported:?}"
            );
            assert_eq!(
                reported["StandardOutputContent"].as_str().unwrap_or("<unset>"),
                "",
                "case {case:?}: expected no output for the cancelled invocation of \
                 {instance_id}, got {reported:?}"
            );
        }
        // Every invocation of the command has reached a terminal state and none of them
        // failed: "the number of targets for which the command invocation reached a
        // terminal state" (`Command$CompletedCount`).
        let command = listed(&router, case, id).await;
        let counts = (
            command["TargetCount"].as_i64().unwrap_or(-1),
            command["CompletedCount"].as_i64().unwrap_or(-1),
            command["ErrorCount"].as_i64().unwrap_or(-1),
        );
        let nodes = i64::try_from(row.nodes.len()).expect("a small node count");
        assert_eq!(
            counts,
            (nodes, nodes, 0),
            "case {case:?}: expected (TargetCount, CompletedCount, ErrorCount) of \
             ({nodes}, {nodes}, 0), got {counts:?} in {command:?}"
        );
    }
}

/// RULE: a cancel acts on the nodes it NAMES, so an invocation of another node of the same
/// command keeps running, and the command itself records the cancel only once the cancel
/// covers every node it was requested on.
#[tokio::test]
async fn a_cancel_that_names_nodes_stops_those_nodes_only() {
    // One row per number of nodes named out of the three the command was sent to: one, two,
    // and all three.
    let rows: Vec<(&str, Vec<&str>, Vec<&str>, &str)> = vec![
        (
            "one of three nodes",
            vec![WINDOWS],
            vec![HYBRID, LINUX],
            "InProgress",
        ),
        (
            "two of three nodes",
            vec![WINDOWS, LINUX],
            vec![HYBRID],
            "InProgress",
        ),
        (
            "every node of the command",
            vec![WINDOWS, LINUX, HYBRID],
            vec![],
            "Cancelled",
        ),
    ];

    for (case, named, untouched, command_target) in rows {
        let (router, engine) = service();
        seed_inventory(&engine);
        let id = send(
            &router,
            case,
            &json!({
                "DocumentName": AGENT,
                "InstanceIds": [LINUX, WINDOWS, HYBRID],
            }),
        )
        .await;

        cancelled(
            &router,
            case,
            &json!({"CommandId": id, "InstanceIds": named}),
        )
        .await;

        for instance_id in &named {
            invocation_reaches(&router, case, &id, instance_id, "Cancelled").await;
        }
        command_reaches(&router, case, &id, command_target).await;
        // A node the cancel did not name is untouched: nothing has been delivered to it in
        // this fixture, so it is still waiting — "Pending: The command hasn't been sent to
        // the managed node".
        for instance_id in &untouched {
            let held = invocation_status(&router, case, &id, instance_id).await;
            assert_eq!(
                held, "Pending",
                "case {case:?}: expected the invocation of {instance_id} to be left Pending \
                 by a cancel that did not name it, got {held:?}"
            );
        }
    }
}

/// RULE: a cancel stops what is still running, so an invocation or a command that has
/// already reached a terminal status keeps the status it reached.
#[tokio::test]
async fn a_cancel_leaves_what_already_finished_alone() {
    // One row per terminal status an invocation reaches, as `Command$StatusDetails` lists
    // them and `commands::TERMINAL_INVOCATION_STATUSES` holds them.
    for status in commands::TERMINAL_INVOCATION_STATUSES {
        let case = format!("a command that already reached {status}");
        let case = case.as_str();
        let (router, engine) = service();
        seed_inventory(&engine);
        let id = send(
            &router,
            case,
            &json!({
                "DocumentName": SHELL,
                "InstanceIds": [LINUX],
                "Parameters": {"commands": ["echo finished"]},
            }),
        )
        .await;
        set_finished(&engine, &id, LINUX, status, status);

        cancelled(&router, case, &json!({"CommandId": id})).await;

        let held = command_status(&router, case, &id).await;
        assert_eq!(
            held, *status,
            "case {case:?}: expected a command that had finished to keep {status:?}, got \
             {held:?}"
        );
        let invocation = invocation_status(&router, case, &id, LINUX).await;
        assert_eq!(
            invocation, *status,
            "case {case:?}: expected an invocation that had finished to keep {status:?}, got \
             {invocation:?}"
        );
    }
}

/// RULE: the same reading per invocation — a command still running whose one invocation has
/// finished records the cancel on itself and leaves that invocation as it was.
#[tokio::test]
async fn a_cancel_records_itself_on_the_command_whose_invocation_had_finished() {
    let case = "an invocation that succeeded under a command still pending";
    let (router, engine) = service();
    seed_inventory(&engine);
    let id = send(
        &router,
        case,
        &json!({
            "DocumentName": SHELL,
            "InstanceIds": [LINUX],
            "Parameters": {"commands": ["echo done"]},
        }),
    )
    .await;
    set_finished(
        &engine,
        &id,
        LINUX,
        "Success",
        commands::COMMAND_STATUS_PENDING,
    );

    cancelled(&router, case, &json!({"CommandId": id})).await;

    // Nothing was left to stop, so the cancel settles at once rather than waiting on work in
    // flight.
    let held = command_status(&router, case, &id).await;
    assert_eq!(
        held, "Cancelled",
        "case {case:?}: expected the command to record the cancel, got {held:?}"
    );
    let invocation = invocation_status(&router, case, &id, LINUX).await;
    assert_eq!(
        invocation, "Success",
        "case {case:?}: expected the finished invocation to keep Success, got {invocation:?}"
    );
}

/// RULE: a cancel is idempotent — cancelling a command the caller has already cancelled is
/// served and leaves the cancel it recorded where it was.
#[tokio::test]
async fn cancelling_twice_is_served_and_changes_nothing() {
    let case = "a second cancel of the same command";
    let (router, engine) = service();
    seed_inventory(&engine);
    let id = send(
        &router,
        case,
        &json!({
            "DocumentName": SHELL,
            "InstanceIds": [LINUX, HYBRID],
            "Parameters": {"commands": ["echo twice"]},
        }),
    )
    .await;

    cancelled(&router, case, &json!({"CommandId": id})).await;
    command_reaches(&router, case, &id, "Cancelled").await;
    cancelled(&router, case, &json!({"CommandId": id})).await;

    let held = command_status(&router, case, &id).await;
    assert_eq!(
        held, "Cancelled",
        "case {case:?}: expected the command to stay Cancelled, got {held:?}"
    );
    for instance_id in [LINUX, HYBRID] {
        let invocation = invocation_status(&router, case, &id, instance_id).await;
        assert_eq!(
            invocation, "Cancelled",
            "case {case:?}: expected the invocation of {instance_id} to stay Cancelled, got \
             {invocation:?}"
        );
    }
}

/// RULE: a node a cancel names has to be one this cancel can act on — an invocation the
/// command created, or a node registered in that Region whose agent is reporting.
#[tokio::test]
async fn a_cancel_names_a_node_it_can_act_on() {
    let case = "a node whose agent stopped reporting after the send";
    let (router, engine) = service();
    seed_inventory(&engine);
    let id = send(
        &router,
        case,
        &json!({
            "DocumentName": SHELL,
            "InstanceIds": [LINUX],
            "Parameters": {"commands": ["echo outlives"]},
        }),
    )
    .await;
    // An invocation is a history record and outlives the node that holds it, so the cancel
    // still reaches it once that node's agent has stopped reporting.
    {
        let state = engine.state::<ComAmazonawsSsmAmazonssmState>();
        let mut store = state.lock();
        let held = store
            .managed_node_mut(REGION, LINUX)
            .expect("the seeded node is registered");
        held.properties.ping_status = Some("ConnectionLost".to_string());
    }
    cancelled(
        &router,
        case,
        &json!({"CommandId": id, "InstanceIds": [LINUX]}),
    )
    .await;
    invocation_reaches(&router, case, &id, LINUX, "Cancelled").await;

    // A node the Region holds a reporting registration of holds no invocation of this
    // command, so the cancel is served and cancels nothing.
    let case = "a registered node the command was never sent to";
    let (router, engine) = service();
    seed_inventory(&engine);
    let id = send(
        &router,
        case,
        &json!({
            "DocumentName": SHELL,
            "InstanceIds": [LINUX],
            "Parameters": {"commands": ["echo untouched"]},
        }),
    )
    .await;
    cancelled(
        &router,
        case,
        &json!({"CommandId": id, "InstanceIds": [IDLE]}),
    )
    .await;
    let held = command_status(&router, case, &id).await;
    assert_eq!(
        held, commands::COMMAND_STATUS_PENDING,
        "case {case:?}: expected a cancel that reached no invocation to leave the command \
         Pending, got {held:?}"
    );
    let invocation = invocation_status(&router, case, &id, LINUX).await;
    assert_eq!(
        invocation, commands::COMMAND_STATUS_PENDING,
        "case {case:?}: expected the invocation of {LINUX} to be left Pending, got \
         {invocation:?}"
    );
}

/// How one operation of this slice asks about a command id: the request body to send it,
/// built from the id under test.
type CommandIdRequest = fn(&str) -> Value;

/// RULE: every operation of this slice that RESOLVES a command id words its refusal of one
/// the endpoint holds no command for by naming that id — the refusal is about the id, not
/// about what was being asked of the command.
#[tokio::test]
async fn an_unknown_command_id_is_refused_by_name_whatever_resolves_it() {
    let (router, engine) = service();
    seed_inventory(&engine);
    let case = "the operation table";
    // A command of another Region, to pin that the same refusal covers an id the endpoint
    // the call was signed for holds no command of, not only an id nothing ever minted.
    let elsewhere = send_in_region(
        &router,
        case,
        OTHER_REGION,
        &json!({
            "DocumentName": SHELL,
            "InstanceIds": [OTHER_ONLY],
            "Parameters": {"commands": ["echo elsewhere"]},
        }),
    )
    .await;

    // One row per operation that resolves a command id, each asked about an id no command of
    // this Region carries.
    let operations: Vec<(&str, CommandIdRequest)> = vec![
        ("CancelCommand", |id| json!({"CommandId": id})),
        ("GetCommandInvocation", |id| {
            json!({"CommandId": id, "InstanceId": LINUX})
        }),
        ("ListCommands", |id| json!({"CommandId": id})),
        ("ListCommandInvocations", |id| json!({"CommandId": id})),
    ];

    for (operation, body) in operations {
        for unknown in [UNKNOWN_COMMAND, elsewhere.as_str()] {
            let case = format!("{operation} asked about {unknown}");
            let case = case.as_str();
            let (status, parsed) = call(&router, operation, &body(unknown)).await;
            assert_eq!(
                status, 400,
                "case {case:?}: expected HTTP 400, got {status} with {parsed:?}"
            );
            let held = error_code(&parsed);
            assert_eq!(
                held, "InvalidCommandId",
                "case {case:?}: expected InvalidCommandId, got {held:?} in {parsed:?}"
            );
            let expected = commands::command_not_found(unknown);
            let held = error_message(&parsed);
            assert_eq!(
                held, expected,
                "case {case:?}: expected the message {expected:?}, got {held:?}"
            );
        }
    }
}

/// RULE: a cancel that cannot resolve what it names is refused and cancels nothing — a
/// command id no command of the Region carries, a node id named twice, and a node id naming
/// neither an invocation of the command nor a node the Region can act on.
#[tokio::test]
async fn a_cancel_that_resolves_nothing_is_refused_and_cancels_nothing() {
    let (router, engine) = service();
    seed_inventory(&engine);
    let case = "the refusal table";
    let id = send(
        &router,
        case,
        &json!({
            "DocumentName": SHELL,
            "InstanceIds": [LINUX],
            "Parameters": {"commands": ["echo refusals"]},
        }),
    )
    .await;
    // A command of another Region, to pin that a command is cancellable only through the
    // endpoint it was sent to.
    let elsewhere = send_in_region(
        &router,
        case,
        OTHER_REGION,
        &json!({
            "DocumentName": SHELL,
            "InstanceIds": [OTHER_ONLY],
            "Parameters": {"commands": ["echo elsewhere"]},
        }),
    )
    .await;

    // One row per identity a cancel cannot resolve: the command, and each way a named node
    // names nothing this endpoint can act on.
    let rows: Vec<(&str, Value, &str, String)> = vec![
        (
            "a command id no command carries",
            json!({"CommandId": UNKNOWN_COMMAND}),
            "InvalidCommandId",
            commands::command_not_found(UNKNOWN_COMMAND),
        ),
        (
            "a command of another Region",
            json!({"CommandId": elsewhere}),
            "InvalidCommandId",
            commands::command_not_found(&elsewhere),
        ),
        (
            "the same node named twice",
            json!({"CommandId": id, "InstanceIds": [LINUX, LINUX]}),
            "DuplicateInstanceId",
            "<no message>".to_string(),
        ),
        (
            "a node id nothing is registered under",
            json!({"CommandId": id, "InstanceIds": [UNREGISTERED]}),
            "InvalidInstanceId",
            commands::NODES_NOT_IN_A_VALID_STATE.to_string(),
        ),
        (
            "a registered node whose agent is not reporting",
            json!({"CommandId": id, "InstanceIds": [OFFLINE]}),
            "InvalidInstanceId",
            commands::NODES_NOT_IN_A_VALID_STATE.to_string(),
        ),
        (
            "a node registered in another Region only",
            json!({"CommandId": id, "InstanceIds": [OTHER_ONLY]}),
            "InvalidInstanceId",
            commands::NODES_NOT_IN_A_VALID_STATE.to_string(),
        ),
    ];

    for (case, body, code, message) in rows {
        let (status, parsed) = cancel(&router, &body).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {parsed:?}"
        );
        let held = error_code(&parsed);
        assert_eq!(
            held, code,
            "case {case:?}: expected the error code {code:?}, got {held:?} in {parsed:?}"
        );
        let held = error_message(&parsed);
        assert_eq!(
            held, message,
            "case {case:?}: expected the message {message:?}, got {held:?}"
        );
        // A refused cancel cancels nothing: the command the fixture sent is still waiting.
        let held = command_status(&router, case, &id).await;
        assert_eq!(
            held, commands::COMMAND_STATUS_PENDING,
            "case {case:?}: expected the refused cancel to leave the command Pending, got \
             {held:?}"
        );
    }
}
