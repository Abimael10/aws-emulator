// SPDX-License-Identifier: CC0-1.0

//! An extension action point of a DEPLOYMENT's own lifecycle whose `Uri` names a Lambda
//! function RUNS that function: the handler's code executes, so the effects it performs are
//! there to be read, and the deployment records the invocation it made.
//!
//! The rule under test, which the tables below are slices of: when a deployment reaches a
//! stage of its own lifecycle, every action an associated extension binds at that stage's
//! action point (`smithy_query(query="shape",shape="ActionPoint")`) is invoked at the target
//! its `Uri` names — for a function, a real synchronous run of that function, handed the
//! event of that point — and the deployment event that owed the point records it as an
//! `ActionInvocation` naming "the name, the ID, or the Amazon Resource Name (ARN) of the
//! extension", the action, its `Uri`, its `RoleArn` and "a system-generated ID for this
//! invocation" (`smithy_query(query="shape",shape="ActionInvocation")`). An action bound at
//! a point the deployment's workflow does not reach it at — an association bound to another
//! resource — is invoked nowhere, and a call a configuration profile's validators refuse
//! reaches no stage at all, so it runs none of them.
//!
//! The suite mounts AWS Lambda and Amazon SQS beside AppConfig on ONE engine, because a run
//! is asserted where a client observes it: the handler forwards the event it was handed, and
//! the `aws_request_id` of its OWN run, into a queue beside it — no AppConfig API reports
//! either — so an action point that merely recorded an invocation it never made fails these
//! rows.
//!
//! What the neighbours pin and this one does not repeat:
//! `crate::extension_action_points` pins the event each binding level is handed against a
//! QUEUE target (a target kind that cannot run code), `crate::deployment_tick_directive` has
//! a tick target's ANSWER steer the rollout, `crate::lambda_validator_relayed_verdict` reads
//! the relayed verdict of a `LAMBDA` validator out of the refusal, and
//! `crate::configuration_profile_validators` pins which calls the validators gate. What is
//! only reachable here is the FUNCTION of a lifecycle action point actually running — the
//! request id of its own run is the evidence — and the fact that a refused call reaches no
//! stage and so runs no lifecycle action at all.
//!
//! The rows of the first table differ along which action point of the lifecycle the
//! extension binds, which stage of the deployment's own clock owes it, and whether the
//! association binds the resource the deployment runs on at all. The second table binds
//! every lifecycle point at once, at the APPLICATION level rather than the environment's,
//! and its rows differ along the configuration document the profile's `LAMBDA` validator
//! judges.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::appconfig_workflow::{
    create_application, create_environment, create_profile, is_modelled_id,
    TERMINAL_DEPLOYMENT_STATES,
};
use crate::configuration_profile_validators::{
    assert_bad_request, deployment_numbers, emulator as appconfig_and_lambda, given_profile,
    given_version, validator,
};
use crate::deployment_tick_directive::given_function;
use crate::extension_action_points::{
    associate, drain, get_deployment, given_queue, given_strategy, start_deployment,
};
use crate::extensions::create_extension;
use crate::resource_tags::arn;

/// How long any row waits for the stage of a deployment it is watching.
///
/// A generous ceiling on the WAIT and nothing else: this suite runs beside every other test
/// of the workspace on one machine, so no outcome may depend on how fast the host is. A
/// deployment that has not reached its stage by here is one nothing is moving, which is the
/// defect these rows pin.
const REACHED_BY: Duration = Duration::from_secs(120);

/// How often the deployment under observation is read while waiting for that.
///
/// Reading it is also what gives the action points the clock has made due somewhere to run,
/// the way a client watching its own rollout does.
const LOOK: Duration = Duration::from_millis(25);

/// The rollout window every row's strategy declares, in minutes: enough that the deployment
/// is paced by stages AFTER the call that started it, rather than being complete the moment
/// it is asked for.
const ROLLOUT_MINUTES: i32 = 1;

/// The configuration document every deployment below rolls out, and the media type it is
/// written with.
const CONTENT: &[u8] = br#"{"replicas":3,"flag":"on"}"#;

/// The `State` a deployment holding its final bake window reports.
const BAKING: &str = "BAKING";

/// The fixed text of a refusal that relays a `LAMBDA` validator's own answer: the two
/// members of Lambda's `Invoke` answer that come before the payload, as an answer to an
/// invocation that asked for no log tail carries them.
const RELAYED_PREFIX: &str = "Function Error: Unhandled, Log Result: null, Payload: ";

/// The sentence the gate function below raises with, which a relayed refusal carries as the
/// `errorMessage` the handler's own runtime wrote.
const GATE_MESSAGE: &str = "the configuration declares no compliance";

/// The configuration document the gate function finds compliant.
const COMPLIANT: &[u8] = br#"{"compliant":true}"#;

/// The `Type` of the two action-point events a deployment the gate let through must deliver:
/// the stage it starts at and the stage it finishes at, which every rollout reaches.
const LET_THROUGH_POINTS: [&str; 2] = ["OnDeploymentStart", "OnDeploymentComplete"];

/// AppConfig, AWS Lambda and Amazon SQS on ONE engine: the action's target is a function the
/// Lambda emulator holds, and the run of it is asserted on the queue its handler writes to.
///
/// Built on the two-service fixture every suite whose target is a FUNCTION shares
/// ([`appconfig_and_lambda`]) plus the one service this suite adds.
fn emulator() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let (mut router, engine) = appconfig_and_lambda();
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        Arc::clone(&engine),
    ));
    (router, engine)
}

/// The `RoleArn` every action below declares: "an Amazon Resource Name (ARN) for an Identity
/// and Access Management assume role" (`smithy_query(query="shape",shape="Action")`), of this
/// emulator's own account.
fn action_role() -> String {
    format!(
        "arn:aws:iam::{}:role/appconfig-extension",
        engine::emulator_account_id()
    )
}

/// The handler every action target below runs: it forwards the WHOLE event it was handed and
/// the request id of its OWN run into the queue beside it — the only place a client observes
/// either — and answers an empty document, which asks for nothing.
fn forwarding_source(queue_url: &str) -> String {
    format!(
        "import json\n\
         \n\
         import boto3\n\
         \n\
         QUEUES = boto3.client(\"sqs\")\n\
         \n\
         \n\
         def lambda_handler(event, context):\n\
         \x20   QUEUES.send_message(\n\
         \x20       QueueUrl={queue_url:?},\n\
         \x20       MessageBody=json.dumps(\n\
         \x20           {{\"Event\": event, \"RequestId\": context.aws_request_id}}\n\
         \x20       ),\n\
         \x20   )\n\
         \x20   return {{}}\n"
    )
}

/// The handler of the second table's `LAMBDA` validator: it reads the candidate
/// configuration it was handed and reports a document it does not find compliant by RAISING,
/// which is the verdict that refuses the call the validator gates.
///
/// It raises with [`GATE_MESSAGE`], interpolated rather than written out twice, so the
/// sentence a row expects to read back out of the refusal cannot drift from the one the
/// function raises with.
fn gate_source() -> String {
    format!(
        "import base64\n\
         import json\n\
         \n\
         \n\
         def lambda_handler(event, context):\n\
         \x20   try:\n\
         \x20       document = json.loads(base64.b64decode(event[\"content\"]))\n\
         \x20   except ValueError:\n\
         \x20       document = None\n\
         \x20   if isinstance(document, dict):\n\
         \x20       if document.get(\"compliant\") is True:\n\
         \x20           return None\n\
         \x20   raise ValueError({GATE_MESSAGE:?})\n"
    )
}

/// An extension binding one action at each of `action_points`, all of them naming the
/// function `function_arn` under `role_arn`. Answers `(id, name)`.
async fn given_lambda_extension(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    action_points: &[&str],
    function_arn: &str,
    role_arn: &str,
) -> (String, String) {
    let actions: BTreeMap<String, Value> = action_points
        .iter()
        .map(|point| {
            (
                (*point).to_string(),
                json!([{
                    "Name": action_name(name),
                    "Uri": function_arn,
                    "RoleArn": role_arn,
                }]),
            )
        })
        .collect();
    let created = create_extension(router, &json!({ "Name": name, "Actions": actions }), None).await;
    assert_eq!(
        created.status,
        201,
        "case {case:?}: CreateExtension answered {} / {}",
        created.status,
        String::from_utf8_lossy(&created.body)
    );
    let id = created
        .string("Id")
        .unwrap_or_else(|| panic!("case {case:?}: CreateExtension answered no Id"));
    (id, name.to_string())
}

/// The `Name` the action of the extension `name` carries.
fn action_name(name: &str) -> String {
    format!("{name}-action")
}

/// Every message waiting on the queue, parsed, in the order they were written.
///
/// [`drain`] takes one `ReceiveMessage` batch, which is capped at ten; a deployment's ticks
/// can produce more than that, so this keeps taking batches until one comes back empty.
async fn observations(router: &MultiServiceRouter, queue_url: &str) -> Vec<Value> {
    let mut written: Vec<Value> = Vec::new();
    loop {
        let batch = drain(router, queue_url).await;
        if batch.is_empty() {
            return written;
        }
        written.extend(batch);
    }
}

/// One wait: the deployment being watched, where the runs of its action targets are seen, and
/// what ends the wait.
struct Watch<'wait> {
    /// The row the wait belongs to, which every assertion of it quotes.
    case: &'wait str,
    application: &'wait str,
    environment: &'wait str,
    number: i32,
    /// The queue the action targets' handlers write to.
    queue_url: &'wait str,
    /// The `State` values the wait ends at.
    until: &'wait [&'wait str],
    /// The event `Type`s that must also have been delivered before it ends.
    required: &'wait [&'wait str],
}

/// Read the deployment until it reports one of `until` AND every event type `required` names
/// has reached the queue, collecting everything the runs of its action targets wrote on the
/// way. Answers the document it was last reported with and those observations.
///
/// The wait is on the CONDITION: the loop looks again until both halves hold or [`REACHED_BY`]
/// has passed, so nothing here depends on how fast the host runs. Reading the deployment is
/// also what gives the action points the clock has made due somewhere to run, which is why
/// the queue is drained right after each read — and why the state alone is not the condition:
/// the read that reports a deployment terminal is the read that owes the last stage's
/// invocations, so a caller watching for them looks again rather than concluding from one
/// answer.
async fn watched(router: &MultiServiceRouter, watch: &Watch<'_>) -> (Value, Vec<Value>) {
    let Watch {
        case,
        application,
        environment,
        number,
        queue_url,
        until,
        required,
    } = *watch;
    let deadline = Instant::now() + REACHED_BY;
    let mut seen: Vec<Value> = Vec::new();
    loop {
        let read = get_deployment(router, application, environment, number).await;
        assert_eq!(
            read.status,
            200,
            "case {case:?}: GetDeployment answered {} / {}",
            read.status,
            String::from_utf8_lossy(&read.body)
        );
        let document = read.json();
        let state = document["State"].as_str().unwrap_or_default().to_string();
        seen.extend(observations(router, queue_url).await);
        let delivered: Vec<&str> = seen
            .iter()
            .filter_map(|observation| observation["Event"]["Type"].as_str())
            .collect();
        let awaited: Vec<&&str> = required
            .iter()
            .filter(|wanted| !delivered.contains(*wanted))
            .collect();
        if until.contains(&state.as_str()) && awaited.is_empty() {
            return (document, seen);
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: after {}s the deployment reports {state:?} (waiting for any of \
             {until:?}) and has not delivered {awaited:?} (delivered {delivered:?})",
            REACHED_BY.as_secs()
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// The event log entry of `event_type`, or `None` when the deployment logged none.
fn logged(document: &Value, event_type: &str) -> Option<Value> {
    document["EventLog"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .find(|event| event["EventType"] == json!(event_type))
}

/// Every `ActionInvocation` the deployment's whole event log records.
fn recorded_invocations(document: &Value) -> Vec<Value> {
    document["EventLog"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .flat_map(|event| {
            event["ActionInvocations"]
                .as_array()
                .cloned()
                .unwrap_or_default()
        })
        .collect()
}

/// The bytes the event's `Content` member carries, decoded.
fn decoded(case: &str, event: &Value) -> Vec<u8> {
    let text = event["Content"].as_str().unwrap_or_else(|| {
        panic!("case {case:?}: the event carries the candidate configuration, got {event}")
    });
    base64::Engine::decode(&base64::engine::general_purpose::STANDARD, text)
        .unwrap_or_else(|error| panic!("case {case:?}: {text:?} is not base64 ({error})"))
}

/// One row of the first table: which lifecycle action point the extension binds.
struct Point {
    /// The row's name, which every assertion of it quotes.
    case: &'static str,
    /// The `ActionPoint` the extension binds its action at.
    action_point: &'static str,
    /// The `Type` the event of that point carries: the action point in PascalCase.
    event_type: &'static str,
    /// The `DeploymentEventType` of the stage that owes the point, whose log entry records
    /// the invocation — and `None` for the tick, which is owed by the interval that paces the
    /// rollout rather than by a stage, so it logs nothing of its own.
    owed_by: Option<&'static str>,
    /// The final bake time the row's strategy declares, in minutes: a bake stage happens only
    /// for a strategy that declares one.
    bake_minutes: i32,
    /// The states the row waits for the deployment to report.
    until: &'static [&'static str],
    /// Whether the association binds the resource the deployment runs on.
    bound: bool,
}

/// RULE: every action point of a deployment's own lifecycle invokes the function its `Uri`
/// names when the deployment reaches that stage, and the stage's own event records that
/// invocation; an action whose association does not bind the resource the deployment runs on
/// is invoked nowhere.
#[tokio::test]
async fn each_lifecycle_action_point_runs_the_function_its_uri_names() {
    let (router, engine) = emulator();
    let application = create_application(&router, "lifecycle-runs").await;
    let profile = create_profile(&router, &application, "lifecycle-runs-profile").await;
    let version = given_version(&router, &application, &profile, "the fixture", CONTENT).await;
    let role = action_role();
    // An environment nothing is deployed into, which the unbound row's association binds
    // instead of the one its deployment runs in.
    let elsewhere = create_environment(&router, &application, "lifecycle-elsewhere").await;

    // The action points of a deployment's own lifecycle, from the model's `ActionPoint` enum
    // (`smithy_query(query="shape",shape="ActionPoint")`), each paired with the stage that
    // owes it; `ON_DEPLOYMENT_ROLLED_BACK` belongs to a deployment that was aborted and is
    // pinned where a rollback is driven.
    let rows: [Point; 6] = [
        Point {
            case: "the stage the deployment starts at",
            action_point: "ON_DEPLOYMENT_START",
            event_type: "OnDeploymentStart",
            owed_by: Some("DEPLOYMENT_STARTED"),
            bake_minutes: 0,
            until: TERMINAL_DEPLOYMENT_STATES,
            bound: true,
        },
        Point {
            case: "a growth interval of the rollout",
            action_point: "ON_DEPLOYMENT_STEP",
            event_type: "OnDeploymentStep",
            owed_by: Some("PERCENTAGE_UPDATED"),
            bake_minutes: 0,
            until: TERMINAL_DEPLOYMENT_STATES,
            bound: true,
        },
        Point {
            case: "the final bake window opening",
            action_point: "ON_DEPLOYMENT_BAKING",
            event_type: "OnDeploymentBaking",
            owed_by: Some("BAKE_TIME_STARTED"),
            bake_minutes: 1,
            until: &[BAKING],
            bound: true,
        },
        Point {
            case: "the deployment completing",
            action_point: "ON_DEPLOYMENT_COMPLETE",
            event_type: "OnDeploymentComplete",
            owed_by: Some("DEPLOYMENT_COMPLETED"),
            bake_minutes: 0,
            until: TERMINAL_DEPLOYMENT_STATES,
            bound: true,
        },
        Point {
            case: "the interval that paces the rollout",
            action_point: "AT_DEPLOYMENT_TICK",
            event_type: "AtDeploymentTick",
            owed_by: None,
            bake_minutes: 0,
            until: TERMINAL_DEPLOYMENT_STATES,
            bound: true,
        },
        Point {
            case: "an association bound to another environment",
            action_point: "ON_DEPLOYMENT_START",
            event_type: "OnDeploymentStart",
            owed_by: Some("DEPLOYMENT_STARTED"),
            bake_minutes: 0,
            until: TERMINAL_DEPLOYMENT_STATES,
            bound: false,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let (queue_url, _) = given_queue(&router, &format!("lifecycle-runs-{index}")).await;
        let function = given_function(
            &router,
            &engine,
            case,
            &format!("lifecycle-target-{index}"),
            &forwarding_source(&queue_url),
        )
        .await;
        let (extension, extension_name) = given_lambda_extension(
            &router,
            case,
            &format!("lifecycle-watcher-{index}"),
            &[row.action_point],
            &function,
            &role,
        )
        .await;
        let environment =
            create_environment(&router, &application, &format!("lifecycle-env-{index}")).await;
        let bound_to = if row.bound { &environment } else { &elsewhere };
        associate(
            &router,
            case,
            &extension,
            &arn(&format!("application/{application}/environment/{bound_to}")),
            &Value::Null,
        )
        .await;
        let strategy = given_strategy(
            &router,
            &format!("lifecycle-strategy-{index}"),
            ROLLOUT_MINUTES,
            row.bake_minutes,
        )
        .await;

        let started = start_deployment(
            &router,
            &application,
            &environment,
            &json!({
                "DeploymentStrategyId": strategy,
                "ConfigurationProfileId": profile,
                "ConfigurationVersion": version,
            }),
        )
        .await;
        assert_eq!(
            started.status,
            201,
            "case {case:?}: StartDeployment answered {} / {}",
            started.status,
            String::from_utf8_lossy(&started.body)
        );
        let number = started.json()["DeploymentNumber"]
            .as_i64()
            .unwrap_or_else(|| panic!("case {case:?}: the deployment answered no number"))
            as i32;
        // A bound row waits for its own point's event as well as for the state: the read that
        // reports the last stage is the read that owes that stage's invocations. An unbound
        // row waits for no event, because the rule says none is owed.
        let required: &[&str] = if row.bound { &[row.event_type] } else { &[] };
        let (document, seen) = watched(
            &router,
            &Watch {
                case,
                application: &application,
                environment: &environment,
                number,
                queue_url: &queue_url,
                until: row.until,
                required,
            },
        )
        .await;

        if !row.bound {
            assert!(
                seen.is_empty(),
                "case {case:?}: an action bound to another resource is invoked nowhere, got \
                 {seen:?}"
            );
            let recorded = recorded_invocations(&document);
            assert!(
                recorded.is_empty(),
                "case {case:?}: a deployment whose stages fired no action records none, got \
                 {recorded:?}"
            );
            continue;
        }

        // The function RAN: the handler wrote the event it was handed, for this point and no
        // other, and reported the request id of its own run with it.
        assert!(
            !seen.is_empty(),
            "case {case:?}: the {} action point must invoke the function its Uri names, and \
             nothing reached the queue its handler writes to",
            row.action_point
        );
        let mut request_ids: Vec<String> = Vec::new();
        for observation in &seen {
            let event = &observation["Event"];
            let event_type = event["Type"].as_str().unwrap_or_default();
            assert_eq!(
                event_type, row.event_type,
                "case {case:?}: expected only {} events, got {event_type:?} in {observation}",
                row.event_type
            );
            assert_eq!(
                event["Application"]["Id"],
                json!(application),
                "case {case:?}: the event names the application the deployment runs on, got \
                 {event}"
            );
            assert_eq!(
                event["Environment"]["Id"],
                json!(environment),
                "case {case:?}: the event names the environment the deployment runs in, got \
                 {event}"
            );
            assert_eq!(
                event["ConfigurationProfile"]["Id"],
                json!(profile),
                "case {case:?}: the event names the configuration profile being deployed, got \
                 {event}"
            );
            assert_eq!(
                event["DeploymentNumber"],
                json!(number),
                "case {case:?}: the event names the deployment it belongs to, got {event}"
            );
            assert_eq!(
                decoded(case, event),
                CONTENT.to_vec(),
                "case {case:?}: the event carries the configuration being rolled out, got \
                 {event}"
            );
            // "A system-generated ID for this invocation"
            // (`smithy_query(query="shape",shape="ActionInvocation")`), whose member targets
            // the model's `Id` shape.
            let invocation_id = event["InvocationId"].as_str().unwrap_or_default();
            assert!(
                is_modelled_id(invocation_id),
                "case {case:?}: expected a system-generated invocation id of the modelled \
                 shape, got {invocation_id:?}"
            );
            let request_id = observation["RequestId"].as_str().unwrap_or_default();
            assert!(
                !request_id.is_empty(),
                "case {case:?}: the handler reports the request id of its own run, got \
                 {observation}"
            );
            request_ids.push(request_id.to_string());
        }
        let mut distinct = request_ids.clone();
        distinct.sort();
        distinct.dedup();
        assert_eq!(
            distinct.len(),
            request_ids.len(),
            "case {case:?}: each invocation is a run of its own, so each reports its own \
             request id, got {request_ids:?}"
        );

        // And the stage that owed the point RECORDED the invocation it made.
        let Some(owed_by) = row.owed_by else {
            continue;
        };
        let entry = logged(&document, owed_by).unwrap_or_else(|| {
            panic!("case {case:?}: no {owed_by} entry in {document}")
        });
        let invocations = entry["ActionInvocations"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        // The extension binds exactly ONE action at this point, and the stage happened once,
        // so the stage records exactly one invocation — asserted on the whole list rather
        // than on its head, so a stage that recorded an invocation twice fails the row.
        assert_eq!(
            invocations.len(),
            1,
            "case {case:?}: the {owed_by} event records the one action bound at this point, \
             got {invocations:?}"
        );
        let record = invocations.first().unwrap_or_else(|| {
            panic!(
                "case {case:?}: the {owed_by} event records the extensions it invoked, got \
                 {entry}"
            )
        });
        assert_eq!(
            record["ExtensionIdentifier"],
            json!(extension_name),
            "case {case:?}: the record names the extension whose action was invoked, got \
             {record}"
        );
        assert_eq!(
            record["ActionName"],
            json!(action_name(&extension_name)),
            "case {case:?}: the record names the action that was invoked, got {record}"
        );
        assert_eq!(
            record["Uri"],
            json!(function),
            "case {case:?}: the record names the target the action points at, got {record}"
        );
        assert_eq!(
            record["RoleArn"],
            json!(role),
            "case {case:?}: the record names the role the action declared, got {record}"
        );
        assert_eq!(
            record["ErrorCode"],
            Value::Null,
            "case {case:?}: a run that reported no error is recorded with none, got {record}"
        );
        assert_eq!(
            record["ErrorMessage"],
            Value::Null,
            "case {case:?}: a run that reported no error is recorded with none, got {record}"
        );
        let first = seen
            .first()
            .unwrap_or_else(|| panic!("case {case:?}: no observation to correlate"));
        assert_eq!(
            record["InvocationId"], first["Event"]["InvocationId"],
            "case {case:?}: the invocation the stage recorded is the one the function was \
             handed, got {record} against {first}"
        );
    }
}

/// RULE: a call a configuration profile's `LAMBDA` validator refuses reaches no stage of a
/// deployment, so it records no deployment and runs none of the lifecycle actions bound to
/// it; the same call for a configuration the SAME function finds compliant runs them all.
#[tokio::test]
async fn a_validator_refusal_runs_no_lifecycle_action_point() {
    let (router, engine) = emulator();
    let application = create_application(&router, "gated-lifecycle").await;
    let role = action_role();
    let (queue_url, _) = given_queue(&router, "gated-lifecycle-observations").await;
    let observer = given_function(
        &router,
        &engine,
        "the fixture",
        "gated-lifecycle-observer",
        &forwarding_source(&queue_url),
    )
    .await;
    let gate = given_function(
        &router,
        &engine,
        "the fixture",
        "gated-lifecycle-validator",
        &gate_source(),
    )
    .await;
    // Bound at the APPLICATION level, which every environment and profile under it runs
    // inside — the level a deployment of any of them reaches the point at.
    let (extension, _) = given_lambda_extension(
        &router,
        "the fixture",
        "gated-lifecycle-watcher",
        &[
            "ON_DEPLOYMENT_START",
            "ON_DEPLOYMENT_STEP",
            "ON_DEPLOYMENT_COMPLETE",
            "AT_DEPLOYMENT_TICK",
        ],
        &observer,
        &role,
    )
    .await;
    associate(
        &router,
        "the fixture",
        &extension,
        &arn(&format!("application/{application}")),
        &Value::Null,
    )
    .await;
    let strategy = given_strategy(&router, "gated-lifecycle-paced", ROLLOUT_MINUTES, 0).await;

    // (case, the document the validator does not find compliant).
    let rows: [(&str, &[u8]); 3] = [
        ("an object the validator rejects", br#"{"compliant":false}"#),
        ("a document that is not an object at all", b"[1, 2, 3]"),
        ("bytes that are no JSON document", b"replicas: three"),
    ];

    for (index, (case, refused_content)) in rows.into_iter().enumerate() {
        let profile = given_profile(
            &router,
            &application,
            &format!("gated-profile-{index}"),
            &[validator("LAMBDA", &gate)],
        )
        .await;
        let refused_version =
            given_version(&router, &application, &profile, case, refused_content).await;
        let allowed_version = given_version(&router, &application, &profile, case, COMPLIANT).await;
        let environment =
            create_environment(&router, &application, &format!("gated-env-{index}")).await;
        // Nothing the fixtures did may be read as this row's observation.
        let residue = observations(&router, &queue_url).await;
        assert!(
            residue.is_empty(),
            "case {case:?}: the row starts with an empty side channel, got {residue:?}"
        );

        let refused = start_deployment(
            &router,
            &application,
            &environment,
            &json!({
                "DeploymentStrategyId": strategy,
                "ConfigurationProfileId": profile,
                "ConfigurationVersion": refused_version,
            }),
        )
        .await;
        assert_bad_request(case, &refused);
        let message = refused.string("Message").unwrap_or_default();
        assert!(
            message.starts_with(RELAYED_PREFIX),
            "case {case:?}: expected a refusal opening {RELAYED_PREFIX:?}, got {message:?}"
        );
        assert!(
            message.contains(GATE_MESSAGE),
            "case {case:?}: the refusal relays the message the validator raised with, got \
             {message:?}"
        );
        let numbers = deployment_numbers(&router, &application, &environment).await;
        assert!(
            numbers.is_empty(),
            "case {case:?}: a refused call creates no deployment, got {numbers:?}"
        );
        let after_refusal = observations(&router, &queue_url).await;
        assert!(
            after_refusal.is_empty(),
            "case {case:?}: a call that reached no stage runs no lifecycle action, got \
             {after_refusal:?}"
        );

        // The same gate, the same actions, a configuration the function finds compliant.
        let accepted = start_deployment(
            &router,
            &application,
            &environment,
            &json!({
                "DeploymentStrategyId": strategy,
                "ConfigurationProfileId": profile,
                "ConfigurationVersion": allowed_version,
            }),
        )
        .await;
        assert_eq!(
            accepted.status,
            201,
            "case {case:?}: the validator found this content compliant, so the deployment \
             must proceed, got {} / {}",
            accepted.status,
            String::from_utf8_lossy(&accepted.body)
        );
        let accepted_number = accepted.json()["DeploymentNumber"]
            .as_i64()
            .unwrap_or_else(|| panic!("case {case:?}: the deployment answered no number"))
            as i32;
        let (document, seen) = watched(
            &router,
            &Watch {
                case,
                application: &application,
                environment: &environment,
                number: accepted_number,
                queue_url: &queue_url,
                until: TERMINAL_DEPLOYMENT_STATES,
                required: &LET_THROUGH_POINTS,
            },
        )
        .await;
        let mut types: Vec<String> = seen
            .iter()
            .map(|observation| {
                observation["Event"]["Type"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string()
            })
            .collect();
        types.sort();
        types.dedup();
        for expected in LET_THROUGH_POINTS {
            assert!(
                types.contains(&expected.to_string()),
                "case {case:?}: the deployment the validator let through reaches its stages, \
                 so each bound action runs there; expected {expected:?} among {types:?}"
            );
        }
        let recorded = recorded_invocations(&document);
        assert!(
            !recorded.is_empty(),
            "case {case:?}: the stages that fired actions record them, got {document}"
        );
    }
}
