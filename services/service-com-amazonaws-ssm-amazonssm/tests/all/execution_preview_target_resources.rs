// SPDX-License-Identifier: CC0-1.0

//! WHICH RESOURCES an execution preview reports, read through the wire a client reads it on:
//! `AmazonSSM.StartExecutionPreview` then `AmazonSSM.GetExecutionPreview` over `awsJson1_1`,
//! dispatched by the multi-service router through the shared harness in
//! `all/parameter_store.rs`.
//!
//! RULE under test: an Automation execution preview's `TargetPreviews` report, per resource
//! type, how many DISTINCT resources the previewed execution would act on — the resources a
//! target criterion matches among the nodes the endpoint holds, the resources an identity
//! target names outright, and the resources the runbook's own steps would act on once the
//! execution's values are resolved into their inputs. A value's resource type is the node's
//! own reported type where the endpoint holds a registration for it, and otherwise the type
//! the identifier's own form names (`i-` an Amazon EC2 instance, `mi-` a hybrid-activation
//! managed node); a value that identifies no resource is reported as none. An Automation
//! action reaches a resource through that resource's own service rather than through a
//! node's agent, which is why an instance a preview names is reported whether or not a node
//! is registered for it here.
//!
//! The rows differ along what the rule generalises over: HOW a resource is named (a
//! parameter the steps reference, an identity target, a tag criterion, the wildcard, a
//! target map), which id FORM it carries, whether the endpoint holds a registration for it,
//! and how many resources of how many types there are. The recorded case — a published
//! runbook previewed with one instance id in its parameters — is one row.
//!
//! Nothing here sleeps: the one transition the service owes a started preview is a stored
//! due instant, which these cases move through the store ([`work_out`]).

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde_json::{json, Value};
use service_com_amazonaws_ssm_amazonssm::state::{
    ComAmazonawsSsmAmazonssmState, ManagedNodeRecord, RESOURCE_TYPE_EC2_INSTANCE,
    RESOURCE_TYPE_MANAGED_INSTANCE,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::parameter_store::{call_in_region, router_with_engine, REGION};

/// An Automation runbook Systems Manager publishes that acts on the instances its
/// `InstanceId` parameter names (`AWS-StartEC2Instance`), so a preview of it needs no
/// document of the account's own.
const PUBLISHED_RUNBOOK: &str = "AWS-StartEC2Instance";

/// The parameter that runbook names its instances through.
const INSTANCE_PARAMETER: &str = "InstanceId";

/// A registered Amazon EC2 node, with the 8-character id suffix the modelled `InstanceId`
/// pattern admits.
const REGISTERED_EC2: &str = "i-0abc1234";

/// A second registered Amazon EC2 node, in the 17-character suffix form.
const REGISTERED_EC2_TWO: &str = "i-0123456789abcdef0";

/// A registered hybrid-activation node (`mi-` and 17 characters, the same pattern).
const REGISTERED_HYBRID: &str = "mi-01234567890123456";

/// A well-formed Amazon EC2 instance id this endpoint holds no registration for.
const UNREGISTERED_EC2: &str = "i-0dead999";

/// A well-formed hybrid-activation id this endpoint holds no registration for.
const UNREGISTERED_HYBRID: &str = "mi-0deadbeef0deadbee";

/// A value that is no resource identifier at all.
const NOT_A_RESOURCE: &str = "a-plain-value";

/// The `TargetPreview$TargetType` of an Amazon EC2 instance, in this service's own
/// `SourceType` spelling.
const EC2_TYPE: &str = "AWS::EC2::Instance";

/// The `TargetPreview$TargetType` of a hybrid-activation managed node (same enum).
const MANAGED_INSTANCE_TYPE: &str = "AWS::SSM::ManagedInstance";

/// Register a node of `resource_type` under `id`, tagged `env`, the way its own registration
/// would.
fn register(
    engine: &Arc<engine::InMemoryEngine>,
    id: &str,
    resource_type: &str,
    env: &str,
) {
    let mut record = ManagedNodeRecord::registering(id, resource_type, SystemTime::now());
    record.tags.insert("env".to_string(), env.to_string());
    assert!(
        engine
            .state::<ComAmazonawsSsmAmazonssmState>()
            .lock()
            .insert_managed_node(REGION, record),
        "the seeded node {id} must carry the node id it is stored under"
    );
}

/// Seed the nodes the criteria resolve against: two Amazon EC2 instances and one hybrid
/// managed node, tagged so a tag criterion can select across both kinds.
fn seed_nodes(engine: &Arc<engine::InMemoryEngine>) {
    register(engine, REGISTERED_EC2, RESOURCE_TYPE_EC2_INSTANCE, "prod");
    register(engine, REGISTERED_EC2_TWO, RESOURCE_TYPE_EC2_INSTANCE, "qa");
    register(
        engine,
        REGISTERED_HYBRID,
        RESOURCE_TYPE_MANAGED_INSTANCE,
        "prod",
    );
}

/// Let the clock reach the instant every preview this endpoint holds was due to have been
/// worked out at.
///
/// The service owes a started preview ONE transition, a span after the start; a case that
/// asserts what the preview SAYS moves the stored due instant rather than waiting for it.
fn work_out(engine: &Arc<engine::InMemoryEngine>) {
    let state = engine.state::<ComAmazonawsSsmAmazonssmState>();
    let mut store = state.lock();
    let long_ago = SystemTime::now() - Duration::from_secs(3_600);
    for ((held, _), preview) in store.execution_previews.iter_mut() {
        if held == REGION {
            preview.ready_at = long_ago;
        }
    }
}

/// Start a preview of the published runbook with `inputs`, let the clock reach the instant it
/// is due, and answer the `Automation` preview the read reports.
async fn previewed(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    case: &str,
    inputs: &Value,
) -> Value {
    let (status, started) = call_in_region(
        router,
        REGION,
        "StartExecutionPreview",
        &json!({"DocumentName": PUBLISHED_RUNBOOK, "ExecutionInputs": inputs}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: StartExecutionPreview must succeed, got {status} with {started}"
    );
    let preview_id = started["ExecutionPreviewId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: a start reports an id, got {started}"))
        .to_string();

    work_out(engine);

    let (status, reported) = call_in_region(
        router,
        REGION,
        "GetExecutionPreview",
        &json!({"ExecutionPreviewId": preview_id}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetExecutionPreview must succeed, got {status} with {reported}"
    );
    assert_eq!(
        reported["Status"].as_str(),
        Some("Success"),
        "case {case:?}: expected a worked-out preview, got {reported}"
    );
    reported["ExecutionPreview"]["Automation"].clone()
}

/// One row: the execution inputs a start carries, and the resource counts the preview of
/// them reports, keyed by resource type in the ascending type order a report carries.
struct Row {
    case: &'static str,
    inputs: Value,
    counts: Value,
}

/// A preview reports the resources the previewed execution would act on, however they are
/// named.
#[tokio::test]
async fn a_preview_reports_the_resources_the_execution_would_act_on() {
    let (router, engine) = router_with_engine();
    seed_nodes(&engine);

    let rows = [
        Row {
            // The recorded shape: a published runbook previewed with one instance id in its
            // parameters, which the runbook's own step acts on.
            case: "a parameter the steps reference, naming one unregistered instance",
            inputs: json!({"Automation": {"Parameters": {INSTANCE_PARAMETER: [UNREGISTERED_EC2]}}}),
            counts: json!([{"TargetType": EC2_TYPE, "Count": 1}]),
        },
        Row {
            case: "a parameter naming a registered node",
            inputs: json!({"Automation": {"Parameters": {INSTANCE_PARAMETER: [REGISTERED_EC2]}}}),
            counts: json!([{"TargetType": EC2_TYPE, "Count": 1}]),
        },
        Row {
            case: "a parameter naming two instances",
            inputs: json!({"Automation": {"Parameters": {
                INSTANCE_PARAMETER: [REGISTERED_EC2, UNREGISTERED_EC2],
            }}}),
            counts: json!([{"TargetType": EC2_TYPE, "Count": 2}]),
        },
        Row {
            case: "a parameter naming the same instance twice",
            inputs: json!({"Automation": {"Parameters": {
                INSTANCE_PARAMETER: [REGISTERED_EC2, REGISTERED_EC2],
            }}}),
            counts: json!([{"TargetType": EC2_TYPE, "Count": 1}]),
        },
        Row {
            case: "a parameter naming a hybrid-activation node",
            inputs: json!({"Automation": {"Parameters": {
                INSTANCE_PARAMETER: [UNREGISTERED_HYBRID],
            }}}),
            counts: json!([{"TargetType": MANAGED_INSTANCE_TYPE, "Count": 1}]),
        },
        Row {
            case: "a parameter value that identifies no resource",
            inputs: json!({"Automation": {"Parameters": {INSTANCE_PARAMETER: [NOT_A_RESOURCE]}}}),
            counts: json!([]),
        },
        Row {
            case: "an identity target naming nodes of both forms",
            inputs: json!({"Automation": {
                "TargetParameterName": INSTANCE_PARAMETER,
                "Targets": [{"Key": "InstanceIds", "Values": [
                    REGISTERED_EC2, UNREGISTERED_EC2, REGISTERED_HYBRID,
                ]}],
            }}),
            counts: json!([
                {"TargetType": EC2_TYPE, "Count": 2},
                {"TargetType": MANAGED_INSTANCE_TYPE, "Count": 1},
            ]),
        },
        Row {
            case: "the wildcard over the Region's EC2 instances",
            inputs: json!({"Automation": {
                "TargetParameterName": INSTANCE_PARAMETER,
                "Targets": [{"Key": "AWS::EC2::Instance", "Values": ["*"]}],
            }}),
            counts: json!([{"TargetType": EC2_TYPE, "Count": 2}]),
        },
        Row {
            case: "a tag criterion spanning both resource types",
            inputs: json!({"Automation": {
                "TargetParameterName": INSTANCE_PARAMETER,
                "Targets": [{"Key": "tag:env", "Values": ["prod"]}],
            }}),
            counts: json!([
                {"TargetType": EC2_TYPE, "Count": 1},
                {"TargetType": MANAGED_INSTANCE_TYPE, "Count": 1},
            ]),
        },
        Row {
            // A criterion describes resources rather than naming them, so one no registered
            // node answers describes none.
            case: "a tag criterion no registered node answers",
            inputs: json!({"Automation": {
                "TargetParameterName": INSTANCE_PARAMETER,
                "Targets": [{"Key": "tag:env", "Values": ["nothing-is-tagged-this"]}],
            }}),
            counts: json!([]),
        },
        Row {
            // A target map binds the runbook's parameters to the resources of one
            // execution, so what the steps act on is what the map holds rather than what
            // the parameters supplied.
            case: "a target map binding the runbook's own parameter",
            inputs: json!({"Automation": {
                "Parameters": {INSTANCE_PARAMETER: [REGISTERED_HYBRID]},
                "TargetMaps": [{INSTANCE_PARAMETER: [REGISTERED_EC2, UNREGISTERED_EC2]}],
            }}),
            counts: json!([{"TargetType": EC2_TYPE, "Count": 2}]),
        },
    ];

    for row in rows {
        let case = row.case;
        let preview = previewed(&router, &engine, case, &row.inputs).await;
        assert_eq!(
            preview["TargetPreviews"], row.counts,
            "case {case:?}: expected the resources the execution would act on, got {preview}"
        );
    }
}

/// The resources a preview reports are the ones the endpoint it was started at holds or was
/// named at: a criterion resolves only against this Region's nodes, and the preview a start
/// persisted keeps reporting what it worked out.
#[tokio::test]
async fn a_preview_reports_the_resources_of_its_own_endpoint() {
    let (router, engine) = router_with_engine();
    seed_nodes(&engine);

    let case = "a tag criterion, read twice";
    let inputs = json!({"Automation": {
        "TargetParameterName": INSTANCE_PARAMETER,
        "Targets": [{"Key": "tag:env", "Values": ["qa"]}],
    }});
    let first = previewed(&router, &engine, case, &inputs).await;
    assert_eq!(
        first["TargetPreviews"],
        json!([{"TargetType": EC2_TYPE, "Count": 1}]),
        "case {case:?}: expected the one node tagged that way, got {first}"
    );

    // A node registered in ANOTHER Region answers no criterion of this endpoint.
    let other_region = "eu-central-1";
    let mut elsewhere =
        ManagedNodeRecord::registering("i-0eeeeeee", RESOURCE_TYPE_EC2_INSTANCE, SystemTime::now());
    elsewhere.tags.insert("env".to_string(), "qa".to_string());
    assert!(
        engine
            .state::<ComAmazonawsSsmAmazonssmState>()
            .lock()
            .insert_managed_node(other_region, elsewhere),
        "the node of the other Region must be stored under its id"
    );
    let case = "a tag criterion with a node of another Region registered";
    let again = previewed(&router, &engine, case, &inputs).await;
    assert_eq!(
        again["TargetPreviews"],
        json!([{"TargetType": EC2_TYPE, "Count": 1}]),
        "case {case:?}: expected only this Region's nodes to be counted, got {again}"
    );
}
