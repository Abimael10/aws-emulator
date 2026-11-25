// SPDX-License-Identifier: CC0-1.0

//! The gate consulted inside a caller's own `StartDeployment` tells its target that the
//! CALLER triggered the release.
//!
//! The rule under test, which the table below is a slice of: the event an extension action
//! bound at the action point AppConfig consults before it starts a deployment is handed
//! carries `TriggeredBy`, "the entity that triggered the deployment event"
//! (`smithy_query(query="doc",shape="DeploymentEvent",member="TriggeredBy")`), as one of the
//! tokens the model's `TriggeredBy` enum admits (`smithy_query(query="shape",
//! shape="TriggeredBy")`) — and for a point reached inside a caller's own request that token
//! names the caller, the same attribution the deployment's own start event carries. The
//! token is an enumerated attribution of the STAGE, so no part of it is read off the
//! request, the target the action names, the configuration being rolled out or the level the
//! extension is associated at.
//!
//! The suite mounts AWS Lambda and Amazon SQS beside AppConfig on one engine, because the
//! event is asserted where a client observes it: the rows whose target is a FUNCTION run a
//! handler that forwards the whole event it was handed into a queue beside it — the path a
//! gate written against AWS reads its event on — and the row whose target is the queue
//! itself reads the delivered document directly.
//!
//! What the neighbours pin and this one does not repeat:
//! `crate::extension_action_points` pins which associations are consulted at this point and
//! the parameters the event carries, and `crate::extension_action_point_trigger` pins the
//! attribution of each point of the deployment's own LIFECYCLE against a queue target. What
//! is only here is the attribution of the pre-start gate itself holding across the target
//! kind that can gate a release, the configuration it is handed and the binding level.
//!
//! The rows differ along the kind of target the action names, the level the association is
//! bound at, the media type and bytes of the candidate configuration, and which version of
//! it the release names.

use std::collections::BTreeMap;

use base64::Engine as _;
use serde_json::{json, Value};
use service_com_amazonaws_appconfig_amazonappconfig::ops::types::TRIGGERED_BY_VALUES;
use smithy_server_runtime::MultiServiceRouter;

use crate::appconfig_workflow::{create_application, create_environment, create_profile, Answer};
use crate::deployment_tick_directive::{emulator, given_function};
use crate::extension_action_points::{
    associate, create_version, drain, get_deployment, given_queue, given_strategy,
    start_deployment,
};
use crate::extensions::create_extension;
use crate::resource_tags::arn;

/// The attribution a release the customer asked for carries, as this service's own catalog
/// of the modelled tokens spells it.
const CALLER: &str = "USER";

/// The `Type` the event of the pre-start gate names, in the PascalCase the event uses rather
/// than the SCREAMING_SNAKE key the action point is bound under.
const PRE_START_EVENT_TYPE: &str = "PreStartDeployment";

/// The deployment event the start of a release records, whose attribution the gate consulted
/// inside the same call must agree with.
const STARTED_EVENT: &str = "DEPLOYMENT_STARTED";

/// Which kind of target an action's `Uri` names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Target {
    /// A Lambda function, the kind that can gate a release by refusing it — and the kind a
    /// customer's approval hook is written as.
    Function,
    /// An Amazon SQS queue, a target that only records what it was handed.
    Queue,
}

/// Which of the three resources a deployment runs on the extension is associated to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Level {
    Application,
    Environment,
    ConfigurationProfile,
}

/// One case: a release a caller starts with the gate bound this way, over this candidate
/// configuration.
#[derive(Debug, Clone, Copy)]
struct Row {
    case: &'static str,
    target: Target,
    level: Level,
    /// The media type the candidate configuration is written with.
    content_type: &'static str,
    /// The candidate configuration's bytes, which the event carries base64-encoded.
    content: &'static [u8],
    /// Which hosted version the release names: the row seeds filler versions before its own
    /// so the number it deploys is this one.
    version_number: i32,
}

/// The handler a FUNCTION target runs: it forwards the WHOLE event it was handed into the
/// queue beside it — the only place a client observes the event — and answers a document
/// that asks for nothing, so the release it gates goes ahead.
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
         \x20   QUEUES.send_message(QueueUrl={queue_url:?}, MessageBody=json.dumps(event))\n\
         \x20   return {{}}\n"
    )
}

/// An extension binding one action at the pre-start action point on `uri`, with the
/// invocation role a target that is invoked under one is given. Answers its id.
async fn given_gate_extension(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    uri: &str,
    role_arn: Option<&str>,
) -> String {
    let mut action = json!({ "Name": "approval-gate", "Uri": uri });
    if let Some(role_arn) = role_arn {
        action["RoleArn"] = json!(role_arn);
    }
    let actions: BTreeMap<String, Value> =
        BTreeMap::from([("PRE_START_DEPLOYMENT".to_string(), json!([action]))]);
    let created = create_extension(router, &json!({ "Name": name, "Actions": actions }), None).await;
    assert_eq!(
        created.status,
        201,
        "case {case:?}: CreateExtension answered {} / {}",
        created.status,
        String::from_utf8_lossy(&created.body)
    );
    created
        .string("Id")
        .unwrap_or_else(|| panic!("case {case:?}: CreateExtension answered no Id"))
}

/// The one event of the pre-start gate waiting on `queue_url`, failing the case when the
/// queue holds no such event or more than one.
async fn delivered_gate_event(router: &MultiServiceRouter, case: &str, queue_url: &str) -> Value {
    let events = drain(router, queue_url).await;
    let gated: Vec<&Value> = events
        .iter()
        .filter(|event| event["Type"] == json!(PRE_START_EVENT_TYPE))
        .collect();
    assert_eq!(
        gated.len(),
        1,
        "case {case:?}: expected exactly one {PRE_START_EVENT_TYPE} event, got {events:?}"
    );
    gated[0].clone()
}

/// The attribution the deployment's own `DEPLOYMENT_STARTED` event reports, which the gate of
/// the same call must agree with.
async fn started_attribution(
    router: &MultiServiceRouter,
    case: &str,
    application: &str,
    environment: &str,
    number: i32,
) -> String {
    let read: Answer = get_deployment(router, application, environment, number).await;
    assert_eq!(
        read.status,
        200,
        "case {case:?}: GetDeployment answered {} / {}",
        read.status,
        String::from_utf8_lossy(&read.body)
    );
    let document = read.json();
    let entry = document["EventLog"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .find(|entry| entry["EventType"] == json!(STARTED_EVENT))
        .unwrap_or_else(|| panic!("case {case:?}: no {STARTED_EVENT} entry in {document}"));
    entry["TriggeredBy"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: {STARTED_EVENT} reports no TriggeredBy: {entry}"))
        .to_string()
}

/// A release a customer issues is attributed to the customer at the gate AppConfig consults
/// before it starts the deployment, whatever target the action names, whatever the candidate
/// configuration is and at whichever level the extension is associated.
#[tokio::test]
async fn the_pre_start_gate_attributes_a_customer_issued_release_to_the_caller() {
    // The rows span the target kind, the binding level, the configuration's media type and
    // bytes, and the version number the release names — none of which the attribution of the
    // stage is read off.
    let rows: [Row; 3] = [
        Row {
            case: "a function gate bound to the environment, over a JSON document",
            target: Target::Function,
            level: Level::Environment,
            content_type: "application/json",
            content: br#"{"replicas":3,"flag":"on"}"#,
            version_number: 1,
        },
        Row {
            case: "a queue gate bound to the application, over a text document",
            target: Target::Queue,
            level: Level::Application,
            content_type: "text/plain",
            content: "flag=on\nregion=\u{1f680}\n".as_bytes(),
            version_number: 1,
        },
        Row {
            case: "a function gate bound to the profile, over raw bytes of a later version",
            target: Target::Function,
            level: Level::ConfigurationProfile,
            content_type: "application/octet-stream",
            content: &[0x00, 0x01, 0x02, 0xfe, 0xff],
            version_number: 2,
        },
    ];

    for (index, row) in rows.into_iter().enumerate() {
        let case = row.case;
        // One emulator per row, so each row's queue holds only what its own release
        // dispatched.
        let (router, engine) = emulator();
        let (queue_url, queue_arn) =
            given_queue(&router, &format!("gate-observations-{index}")).await;
        let application = create_application(&router, &format!("gate-app-{index}")).await;
        let environment =
            create_environment(&router, &application, &format!("gate-env-{index}")).await;
        let profile = create_profile(&router, &application, &format!("gate-profile-{index}")).await;

        // Filler versions before the row's own, so the version the release names is the
        // row's `version_number` and the bytes the event must carry are its `content`.
        for filler in 1..row.version_number {
            let seeded = create_version(
                &router,
                &application,
                &profile,
                "application/json",
                format!(r#"{{"filler":{filler}}}"#).as_bytes(),
            )
            .await;
            assert_eq!(
                seeded.status,
                201,
                "case {case:?}: seeding filler version {filler} answered {} / {}",
                seeded.status,
                String::from_utf8_lossy(&seeded.body)
            );
        }
        let seeded = create_version(
            &router,
            &application,
            &profile,
            row.content_type,
            row.content,
        )
        .await;
        assert_eq!(
            seeded.status,
            201,
            "case {case:?}: seeding the candidate version answered {} / {}",
            seeded.status,
            String::from_utf8_lossy(&seeded.body)
        );
        // The version the write reports, which this operation binds to the `Version-Number`
        // header rather than to a body member.
        let seeded_number = seeded
            .header("Version-Number")
            .unwrap_or_else(|| panic!("case {case:?}: the write answered no Version-Number"));
        assert_eq!(
            seeded_number,
            row.version_number.to_string(),
            "case {case:?}: the candidate must be version {}, got {seeded_number:?}",
            row.version_number
        );

        let (uri, role_arn) = match row.target {
            Target::Queue => (queue_arn.clone(), None),
            Target::Function => {
                let function = given_function(
                    &router,
                    &engine,
                    case,
                    &format!("gate-forwarder-{index}"),
                    &forwarding_source(&queue_url),
                )
                .await;
                let role = format!(
                    "arn:aws:iam::{}:role/appconfig-extension",
                    engine::emulator_account_id()
                );
                (function, Some(role))
            }
        };
        let extension = given_gate_extension(
            &router,
            case,
            &format!("gate-extension-{index}"),
            &uri,
            role_arn.as_deref(),
        )
        .await;
        let resource = match row.level {
            Level::Application => arn(&format!("application/{application}")),
            Level::Environment => {
                arn(&format!("application/{application}/environment/{environment}"))
            }
            Level::ConfigurationProfile => arn(&format!(
                "application/{application}/configurationprofile/{profile}"
            )),
        };
        associate(&router, case, &extension, &resource, &Value::Null).await;

        // A strategy that paces no stage: the gate is consulted inside the call that starts
        // the release, so the event is on the queue by the time the call answers.
        let strategy = given_strategy(&router, &format!("gate-instant-{index}"), 0, 0).await;
        let started = start_deployment(
            &router,
            &application,
            &environment,
            &json!({
                "DeploymentStrategyId": strategy,
                "ConfigurationProfileId": profile,
                "ConfigurationVersion": row.version_number.to_string(),
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

        let event = delivered_gate_event(&router, case, &queue_url).await;

        // The member this case is about: the attribution of the stage the gate belongs to.
        let triggered_by = event["TriggeredBy"].clone();
        assert_eq!(
            triggered_by,
            json!(CALLER),
            "case {case:?}: a release the customer issued reports TriggeredBy {CALLER:?}, \
             got {triggered_by:?} in {event}"
        );
        let named = triggered_by.as_str().unwrap_or_default();
        assert!(
            TRIGGERED_BY_VALUES.contains(&named),
            "case {case:?}: expected one of the modelled TriggeredBy values \
             {TRIGGERED_BY_VALUES:?}, got {named:?}"
        );
        let logged = started_attribution(&router, case, &application, &environment, 1).await;
        assert_eq!(
            logged, CALLER,
            "case {case:?}: the {STARTED_EVENT} entry of the release the gate let through \
             reports {logged:?}, so the gate must hand its target the same attribution"
        );

        // The members the attribution sits beside, so a row that carried the token in an
        // event that was otherwise wrong does not pass.
        assert_eq!(
            event["Application"]["Id"],
            json!(application),
            "case {case:?}: the event names the application the release runs on, got {event}"
        );
        assert_eq!(
            event["Environment"]["Id"],
            json!(environment),
            "case {case:?}: the event names the environment the release runs in, got {event}"
        );
        assert_eq!(
            event["ConfigurationProfile"]["Id"],
            json!(profile),
            "case {case:?}: the event names the configuration profile being released, got \
             {event}"
        );
        assert_eq!(
            event["ContentVersion"],
            json!(row.version_number.to_string()),
            "case {case:?}: the event names the version as a string, got {event}"
        );
        let content = event["Content"].as_str().unwrap_or_default();
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(content)
            .unwrap_or_else(|error| {
                panic!("case {case:?}: Content must be base64 ({error}): {content:?}")
            });
        assert_eq!(
            decoded,
            row.content.to_vec(),
            "case {case:?}: Content must decode to the candidate's exact bytes, got \
             {decoded:?}"
        );
    }
}
