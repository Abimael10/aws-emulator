// SPDX-License-Identifier: CC0-1.0

//! End-to-end wire tests for `UpdateEndpoint`: deploying a new endpoint configuration to
//! an endpoint that is already serving one.
//!
//! Every case goes over the wire (`POST /` with `X-Amz-Target: SageMaker.<Op>`,
//! `awsJson1_1`) through the same `MultiServiceRouter` the production router uses, reusing
//! the harness in [`crate::inference_deployment_lifecycle`], so what these tests exercise
//! is what a real SDK client gets.
//!
//! The tests assert RULES over TABLES of inputs rather than one recorded call:
//!
//! * a deployment asked for on a serving endpoint leaves the configuration that is
//!   serving in place and reports the one being deployed as the deployment in flight,
//!   until the instant the clock owes the deployment complete — from when the endpoint is
//!   `InService` on the new configuration, and nothing is pending;
//! * `RetainAllVariantProperties` keeps the instance count and weight the serving fleet
//!   runs, and each property type `ExcludeRetainedVariantProperties` names is taken from
//!   the new configuration after all;
//! * `RetainDeploymentConfig` reuses the deployment configuration of the last deployment,
//!   and any other call sets the one it carries itself;
//! * a deployment naming a resource the account does not hold, one asked for on an
//!   endpoint that is not serving, and one past the account's hosting quota are each
//!   refused, and none of them changes what the endpoint is running.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::endpoint_hosting_quota::over_quota;
use crate::inference_deployment_lifecycle::{
    error_code, f64_field, message, primary_container, str_field, Emulator,
};

/// How long a deployment is polled for before a case fails, and how often it is looked
/// at. What is waited for is the CONDITION, re-read in a loop, so a loaded host cannot
/// fail a case the clock would have served; the budget is far longer than any span this
/// state gives a deployment.
const SETTLE_DEADLINE: Duration = Duration::from_secs(180);
const SETTLE_POLL: Duration = Duration::from_millis(20);

/// Every member of `com.amazonaws.sagemaker#EndpointStatus`
/// (`smithy_query(query="shape",shape="EndpointStatus")`): an endpoint never reads a
/// status outside the modelled enum.
const ENDPOINT_STATUS_VALUES: [&str; 9] = [
    "OutOfService",
    "Creating",
    "Updating",
    "SystemUpdating",
    "RollingBack",
    "InService",
    "Deleting",
    "Failed",
    "UpdateRollbackFailed",
];

/// `VariantPropertyType`'s members, as `ExcludeRetainedVariantProperties` carries them
/// (`smithy_query(query="shape",shape="VariantPropertyType")`).
const DESIRED_INSTANCE_COUNT: &str = "DesiredInstanceCount";
const DESIRED_WEIGHT: &str = "DesiredWeight";
const DATA_CAPTURE_CONFIG: &str = "DataCaptureConfig";

/// The model every configuration below hosts.
const MODEL: &str = "deploy-model";

/// A production variant body: `weight` and `instances` are left out when `None`, so a row
/// can exercise the members the model documents a default for.
fn variant(name: &str, instances: Option<i64>, weight: Option<f64>, instance_type: &str) -> Value {
    let mut variant = json!({ "VariantName": name, "ModelName": MODEL });
    variant["InstanceType"] = json!(instance_type);
    if let Some(instances) = instances {
        variant["InitialInstanceCount"] = json!(instances);
    }
    if let Some(weight) = weight {
        variant["InitialVariantWeight"] = json!(weight);
    }
    variant
}

/// A serverless production variant: it names no instance type, so it holds no instances.
fn serverless_variant(name: &str) -> Value {
    json!({
        "VariantName": name,
        "ModelName": MODEL,
        "ServerlessConfig": { "MemorySizeInMB": 2048, "MaxConcurrency": 4 },
    })
}

/// Create `MODEL` and the endpoint configuration `name` with the given variants.
async fn given_config(emulator: &Emulator, name: &str, variants: &Value, shadow: Option<&Value>) {
    let mut body = json!({ "EndpointConfigName": name, "ProductionVariants": variants });
    if let Some(shadow) = shadow {
        body["ShadowProductionVariants"] = shadow.clone();
    }
    emulator.ok("CreateEndpointConfig", &body).await;
}

/// Create `MODEL`, the configuration `config` and an endpoint on it, polled to `InService`.
async fn given_serving_endpoint(
    emulator: &Emulator,
    case: &str,
    endpoint: &str,
    config: &str,
    variants: &Value,
) -> String {
    emulator
        .ok(
            "CreateModel",
            &json!({ "ModelName": MODEL, "PrimaryContainer": primary_container("demo:1") }),
        )
        .await;
    given_config(emulator, config, variants, None).await;
    let created = emulator
        .ok(
            "CreateEndpoint",
            &json!({ "EndpointName": endpoint, "EndpointConfigName": config }),
        )
        .await;
    poll_until(emulator, case, endpoint, "InService").await;
    str_field(&created, "EndpointArn")
}

/// Read `endpoint` until it reports `wanted`, answering that reading — the wait is on the
/// CONDITION, never on a span, so a slow host cannot fail a case the clock would serve.
async fn poll_until(emulator: &Emulator, case: &str, endpoint: &str, wanted: &str) -> Value {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    let body = json!({ "EndpointName": endpoint });
    loop {
        let read = emulator.ok("DescribeEndpoint", &body).await;
        let status = str_field(&read, "EndpointStatus");
        assert!(
            ENDPOINT_STATUS_VALUES.contains(&status.as_str()),
            "case {case:?}: EndpointStatus must be a modelled value, got {status:?}"
        );
        if status == wanted {
            return read;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the endpoint never reached {wanted}; last read {read}"
        );
        tokio::time::sleep(SETTLE_POLL).await;
    }
}

/// The weight a variant body asks for: `InitialVariantWeight`, whose model doc says "If
/// unspecified, it defaults to 1.0".
fn asked_weight(variant: &Value) -> f64 {
    variant
        .get("InitialVariantWeight")
        .and_then(Value::as_f64)
        .unwrap_or(1.0)
}

/// The summary of the variant named `name` in a reported summary list.
fn summary_of<'reported>(reported: &'reported Value, name: &str) -> &'reported Value {
    reported
        .as_array()
        .unwrap_or_else(|| panic!("a summary list, got {reported}"))
        .iter()
        .find(|summary| summary.get("VariantName").and_then(Value::as_str) == Some(name))
        .unwrap_or_else(|| panic!("a summary for variant {name} in {reported}"))
}

/// Assert that `reported` summarises exactly the variants `asked` declares, with the
/// instance count and weight each asked for running on both the current and the desired
/// side — which is what a settled deployment reports.
fn assert_running(case: &str, label: &str, asked: &Value, reported: Option<&Value>) {
    let asked = asked.as_array().expect("the row declares variants");
    let reported = reported
        .unwrap_or_else(|| panic!("case {case:?}: {label} must be reported for {asked:?}"));
    assert_eq!(
        reported.as_array().map(Vec::len),
        Some(asked.len()),
        "case {case:?}: {label} must carry one summary per variant, got {reported}"
    );
    for want in asked {
        let name = want
            .get("VariantName")
            .and_then(Value::as_str)
            .expect("a variant name");
        let got = summary_of(reported, name);
        // "The number of instances associated with the variant" (`TaskCount`, `@range min
        // 0`) is reported for every variant of a created endpoint: the instances it
        // provisions, and zero for a variant that provisions none.
        assert_eq!(
            got.get("CurrentInstanceCount").and_then(Value::as_i64),
            Some(
                want.get("InitialInstanceCount")
                    .and_then(Value::as_i64)
                    .unwrap_or(0)
            ),
            "case {case:?}: {label}/{name} CurrentInstanceCount, got {got}"
        );
        assert_eq!(
            got.get("DesiredInstanceCount"),
            want.get("InitialInstanceCount"),
            "case {case:?}: {label}/{name} DesiredInstanceCount, got {got}"
        );
        assert_eq!(
            f64_field(got, "CurrentWeight"),
            asked_weight(want),
            "case {case:?}: {label}/{name} CurrentWeight, got {got}"
        );
        assert_eq!(
            f64_field(got, "DesiredWeight"),
            asked_weight(want),
            "case {case:?}: {label}/{name} DesiredWeight, got {got}"
        );
        assert_eq!(
            got.get("CurrentServerlessConfig"),
            want.get("ServerlessConfig"),
            "case {case:?}: {label}/{name} CurrentServerlessConfig, got {got}"
        );
    }
}

/// Assert that `reported` summarises the variants `asked` declares as a deployment IN
/// FLIGHT: what the deployment asked for on the desired side, and nothing on the current
/// side, because none of its fleet is serving yet.
fn assert_pending(case: &str, label: &str, asked: &Value, reported: Option<&Value>) {
    let asked = asked.as_array().expect("the row declares variants");
    let reported = reported
        .unwrap_or_else(|| panic!("case {case:?}: {label} must be reported for {asked:?}"));
    assert_eq!(
        reported.as_array().map(Vec::len),
        Some(asked.len()),
        "case {case:?}: {label} must carry one summary per variant, got {reported}"
    );
    for want in asked {
        let name = want
            .get("VariantName")
            .and_then(Value::as_str)
            .expect("a variant name");
        let got = summary_of(reported, name);
        assert_eq!(
            got.get("DesiredInstanceCount"),
            want.get("InitialInstanceCount"),
            "case {case:?}: {label}/{name} DesiredInstanceCount, got {got}"
        );
        assert_eq!(
            f64_field(got, "DesiredWeight"),
            asked_weight(want),
            "case {case:?}: {label}/{name} DesiredWeight, got {got}"
        );
        assert_eq!(
            got.get("InstanceType"),
            want.get("InstanceType"),
            "case {case:?}: {label}/{name} InstanceType, got {got}"
        );
        assert_eq!(
            got.get("DesiredServerlessConfig"),
            want.get("ServerlessConfig"),
            "case {case:?}: {label}/{name} DesiredServerlessConfig, got {got}"
        );
        assert!(
            got.get("CurrentInstanceCount").is_none() && got.get("CurrentWeight").is_none(),
            "case {case:?}: {label}/{name} runs nothing while the deployment is in flight, \
             got {got}"
        );
    }
}

/// One row of the deployment table.
struct Row {
    case: &'static str,
    /// The `ProductionVariants` the endpoint is created on.
    serving: Value,
    /// The `ProductionVariants` the update deploys.
    deployed: Value,
    /// The `ShadowProductionVariants` the deployed configuration carries, if any.
    deployed_shadow: Option<Value>,
}

/// The deployment table. Rows differ along what the rule generalises over: how many
/// variants the deployment runs (fewer, more, the same), the instance count and weight
/// each asks for (including the members left out, which the model documents defaults
/// for), the instance type, whether a variant is serverless rather than instance-backed,
/// and whether the deployment carries shadow variants.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "one variant, more instances of a bigger type",
            serving: json!([variant("AllTraffic", Some(1), None, "ml.m5.large")]),
            deployed: json!([variant("AllTraffic", Some(3), Some(1.0), "ml.m5.xlarge")]),
            deployed_shadow: None,
        },
        Row {
            case: "one variant, split into two weighted ones",
            serving: json!([variant("AllTraffic", Some(2), None, "ml.m5.large")]),
            deployed: json!([
                variant("Blue", Some(1), Some(0.75), "ml.c5.xlarge"),
                variant("Green", Some(2), Some(0.25), "ml.c5.2xlarge"),
            ]),
            deployed_shadow: None,
        },
        Row {
            case: "two variants collapsed into one, with the count left out",
            serving: json!([
                variant("Blue", Some(1), Some(0.5), "ml.m5.large"),
                variant("Green", Some(1), Some(0.5), "ml.m5.large"),
            ]),
            deployed: json!([variant("AllTraffic", None, None, "ml.m5.2xlarge")]),
            deployed_shadow: None,
        },
        Row {
            case: "an instance-backed variant replaced by a serverless one",
            serving: json!([variant("AllTraffic", Some(1), None, "ml.m5.large")]),
            deployed: json!([serverless_variant("AllTraffic")]),
            deployed_shadow: None,
        },
        Row {
            case: "a deployment that adds a shadow variant",
            serving: json!([variant("AllTraffic", Some(1), None, "ml.m5.large")]),
            deployed: json!([variant("AllTraffic", Some(2), Some(1.0), "ml.m5.large")]),
            deployed_shadow: Some(json!([variant("Shadow", Some(1), Some(0.0), "ml.m5.large")])),
        },
    ]
}

/// THE RULE: `UpdateEndpoint` deploys the configuration it names to the endpoint it names
/// — the endpoint goes `Updating`, keeps reporting the configuration that is serving and
/// reports the new one as the deployment in flight, and from the instant the clock owes
/// the deployment complete it is `InService` on the new configuration with nothing
/// pending. The ARN it answers is the endpoint's own.
#[tokio::test]
async fn a_deployment_replaces_what_an_endpoint_serves_once_the_clock_owes_it_complete() {
    for row in rows() {
        let case = row.case;
        let emulator = Emulator::new();
        let endpoint_arn =
            given_serving_endpoint(&emulator, case, "deploy-ep", "serving-cfg", &row.serving).await;

        // The endpoint is serving the configuration it was created from.
        let serving = emulator
            .ok("DescribeEndpoint", &json!({ "EndpointName": "deploy-ep" }))
            .await;
        assert_eq!(
            str_field(&serving, "EndpointConfigName"),
            "serving-cfg",
            "case {case:?}: the created endpoint serves its own configuration, got {serving}"
        );
        assert_running(case, "ProductionVariants", &row.serving, serving.get("ProductionVariants"));
        assert!(
            serving.get("PendingDeploymentSummary").is_none(),
            "case {case:?}: a settled endpoint has no deployment in flight, got {serving}"
        );

        given_config(
            &emulator,
            "deployed-cfg",
            &row.deployed,
            row.deployed_shadow.as_ref(),
        )
        .await;

        let answered = emulator
            .ok(
                "UpdateEndpoint",
                &json!({ "EndpointName": "deploy-ep", "EndpointConfigName": "deployed-cfg" }),
            )
            .await;
        assert_eq!(
            str_field(&answered, "EndpointArn"),
            endpoint_arn,
            "case {case:?}: the deployment answers the endpoint's own ARN, got {answered}"
        );

        // While the deployment is in flight the endpoint still reports what is serving,
        // and the configuration being deployed is reported as pending.
        let updating = emulator
            .ok("DescribeEndpoint", &json!({ "EndpointName": "deploy-ep" }))
            .await;
        assert_eq!(
            str_field(&updating, "EndpointStatus"),
            "Updating",
            "case {case:?}: the endpoint is Updating until the deployment is due, got {updating}"
        );
        assert_eq!(
            str_field(&updating, "EndpointConfigName"),
            "serving-cfg",
            "case {case:?}: the configuration that is serving is unchanged, got {updating}"
        );
        assert_running(
            case,
            "ProductionVariants while updating",
            &row.serving,
            updating.get("ProductionVariants"),
        );
        let pending = updating
            .get("PendingDeploymentSummary")
            .unwrap_or_else(|| panic!("case {case:?}: a deployment in flight, got {updating}"));
        assert_eq!(
            str_field(pending, "EndpointConfigName"),
            "deployed-cfg",
            "case {case:?}: the pending deployment names the configuration being deployed, \
             got {pending}"
        );
        assert!(
            pending.get("StartTime").and_then(Value::as_f64).is_some(),
            "case {case:?}: the pending deployment reports when it started, got {pending}"
        );
        assert_pending(
            case,
            "PendingDeploymentSummary.ProductionVariants",
            &row.deployed,
            pending.get("ProductionVariants"),
        );
        match row.deployed_shadow.as_ref() {
            Some(shadow) => assert_pending(
                case,
                "PendingDeploymentSummary.ShadowProductionVariants",
                shadow,
                pending.get("ShadowProductionVariants"),
            ),
            None => assert!(
                pending.get("ShadowProductionVariants").is_none(),
                "case {case:?}: a deployment with no shadow variants reports none, got {pending}"
            ),
        }

        // The listing reports the same status the reader does, so a client that polls
        // either sees one deployment rather than two different endpoints.
        let listed = emulator
            .ok("ListEndpoints", &json!({ "StatusEquals": "Updating" }))
            .await;
        let summaries = listed
            .get("Endpoints")
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("case {case:?}: an endpoint list, got {listed}"));
        assert_eq!(
            summaries
                .iter()
                .map(|summary| str_field(summary, "EndpointName"))
                .collect::<Vec<String>>(),
            vec!["deploy-ep".to_string()],
            "case {case:?}: the endpoint being deployed to is the Updating one, got {listed}"
        );

        // Once the deployment the clock owed falls due, the new configuration is what the
        // endpoint runs and nothing is pending.
        let settled = poll_until(&emulator, case, "deploy-ep", "InService").await;
        assert_eq!(
            str_field(&settled, "EndpointConfigName"),
            "deployed-cfg",
            "case {case:?}: the deployed configuration is what the endpoint serves, got {settled}"
        );
        assert_running(
            case,
            "ProductionVariants after the deployment",
            &row.deployed,
            settled.get("ProductionVariants"),
        );
        match row.deployed_shadow.as_ref() {
            Some(shadow) => assert_running(
                case,
                "ShadowProductionVariants after the deployment",
                shadow,
                settled.get("ShadowProductionVariants"),
            ),
            None => assert!(
                settled.get("ShadowProductionVariants").is_none(),
                "case {case:?}: a configuration with no shadow variants reports none, got {settled}"
            ),
        }
        assert!(
            settled.get("PendingDeploymentSummary").is_none(),
            "case {case:?}: a completed deployment is no longer pending, got {settled}"
        );
        assert!(
            f64_field(&settled, "LastModifiedTime") >= f64_field(&settled, "CreationTime"),
            "case {case:?}: the deployment moved LastModifiedTime, got {settled}"
        );
    }
}

/// One row of the retention table: what the request asks to retain, and the instance count
/// and weight the endpoint must run once the deployment has settled.
struct RetentionRow {
    case: &'static str,
    /// `RetainAllVariantProperties`, left out of the request when `None`.
    retain_all: Option<bool>,
    /// The `VariantPropertyType`s `ExcludeRetainedVariantProperties` names.
    excluded: &'static [&'static str],
    instances: i64,
    weight: f64,
}

/// THE RULE: `RetainAllVariantProperties` keeps the instance count and the weight the
/// serving fleet is running instead of taking them from the configuration being deployed,
/// and each property type `ExcludeRetainedVariantProperties` names is taken from that
/// configuration after all.
#[tokio::test]
async fn retained_variant_properties_keep_what_the_serving_fleet_runs() {
    // Rows differ along what is retained: nothing (the documented default, and the
    // `false` the request may send), everything, and each property type excluded from the
    // retention — including `DataCaptureConfig`, which names no retained property, and
    // both of the ones that do.
    let rows = [
        RetentionRow {
            case: "the default, which retains nothing",
            retain_all: None,
            excluded: &[],
            instances: 4,
            weight: 0.25,
        },
        RetentionRow {
            case: "RetainAllVariantProperties false",
            retain_all: Some(false),
            excluded: &[],
            instances: 4,
            weight: 0.25,
        },
        RetentionRow {
            case: "RetainAllVariantProperties true",
            retain_all: Some(true),
            excluded: &[],
            instances: 2,
            weight: 0.75,
        },
        RetentionRow {
            case: "retained, with the instance count excluded",
            retain_all: Some(true),
            excluded: &[DESIRED_INSTANCE_COUNT],
            instances: 4,
            weight: 0.75,
        },
        RetentionRow {
            case: "retained, with the weight excluded",
            retain_all: Some(true),
            excluded: &[DESIRED_WEIGHT],
            instances: 2,
            weight: 0.25,
        },
        RetentionRow {
            case: "retained, with both excluded",
            retain_all: Some(true),
            excluded: &[DESIRED_INSTANCE_COUNT, DESIRED_WEIGHT],
            instances: 4,
            weight: 0.25,
        },
        RetentionRow {
            case: "retained, excluding a property type no variant property is retained for",
            retain_all: Some(true),
            excluded: &[DATA_CAPTURE_CONFIG],
            instances: 2,
            weight: 0.75,
        },
    ];

    for row in rows {
        let case = row.case;
        let emulator = Emulator::new();
        // The fleet that is serving runs 2 instances at weight 0.75; the configuration
        // being deployed asks for 4 at weight 0.25.
        let serving = json!([variant("AllTraffic", Some(2), Some(0.75), "ml.m5.large")]);
        given_serving_endpoint(&emulator, case, "retain-ep", "retain-serving", &serving).await;
        let deployed = json!([variant("AllTraffic", Some(4), Some(0.25), "ml.m5.large")]);
        given_config(&emulator, "retain-deployed", &deployed, None).await;

        let mut body = json!({
            "EndpointName": "retain-ep",
            "EndpointConfigName": "retain-deployed",
        });
        if let Some(retain_all) = row.retain_all {
            body["RetainAllVariantProperties"] = json!(retain_all);
        }
        if !row.excluded.is_empty() {
            body["ExcludeRetainedVariantProperties"] = Value::Array(
                row.excluded
                    .iter()
                    .map(|property| json!({ "VariantPropertyType": property }))
                    .collect(),
            );
        }
        emulator.ok("UpdateEndpoint", &body).await;

        let settled = poll_until(&emulator, case, "retain-ep", "InService").await;
        let summary = summary_of(
            settled
                .get("ProductionVariants")
                .unwrap_or_else(|| panic!("case {case:?}: variant summaries, got {settled}")),
            "AllTraffic",
        );
        assert_eq!(
            summary.get("CurrentInstanceCount").and_then(Value::as_i64),
            Some(row.instances),
            "case {case:?}: CurrentInstanceCount, got {summary}"
        );
        assert_eq!(
            summary.get("DesiredInstanceCount").and_then(Value::as_i64),
            Some(row.instances),
            "case {case:?}: DesiredInstanceCount, got {summary}"
        );
        assert_eq!(
            f64_field(summary, "CurrentWeight"),
            row.weight,
            "case {case:?}: CurrentWeight, got {summary}"
        );
        assert_eq!(
            f64_field(summary, "DesiredWeight"),
            row.weight,
            "case {case:?}: DesiredWeight, got {summary}"
        );
        // Whatever is retained, the endpoint serves the configuration that was deployed.
        assert_eq!(
            str_field(&settled, "EndpointConfigName"),
            "retain-deployed",
            "case {case:?}: the deployed configuration, got {settled}"
        );
    }
}

/// One row of the deployment-configuration table: what the call sends, whether it asks
/// for the last deployment configuration to be reused, and the one reported afterwards.
struct DeploymentConfigRow<'config> {
    case: &'static str,
    sent: Option<&'config Value>,
    retain: Option<bool>,
    expected: Option<&'config Value>,
}

/// THE RULE: a deployment carries the deployment configuration the request sent, which is
/// what the endpoint reports as its last one; `RetainDeploymentConfig` reuses the one the
/// last deployment carried instead, and a call that sends neither leaves the endpoint with
/// none.
#[tokio::test]
async fn the_last_deployment_config_is_the_one_the_last_deployment_carried() {
    let emulator = Emulator::new();
    let serving = json!([variant("AllTraffic", Some(1), None, "ml.m5.large")]);
    given_serving_endpoint(&emulator, "deployment config", "cfg-ep", "cfg-serving", &serving).await;
    given_config(&emulator, "cfg-deployed", &serving, None).await;

    // Two deployment configurations differing in strategy: a blue/green one and a rolling
    // one (`smithy_query(query="shape",shape="DeploymentConfig")`).
    let blue_green = json!({
        "BlueGreenUpdatePolicy": {
            "TrafficRoutingConfiguration": { "Type": "ALL_AT_ONCE", "WaitIntervalInSeconds": 60 },
            "TerminationWaitInSeconds": 120,
        },
    });
    let rolling = json!({
        "RollingUpdatePolicy": {
            "MaximumBatchSize": { "Type": "INSTANCE_COUNT", "Value": 1 },
            "WaitIntervalInSeconds": 30,
        },
    });

    // Rows walk the endpoint through successive deployments; each names the deployment
    // configuration it sends, whether it asks for the last one to be reused, and the one
    // the endpoint must report afterwards.
    let rows = [
        DeploymentConfigRow {
            case: "a deployment that sends one",
            sent: Some(&blue_green),
            retain: None,
            expected: Some(&blue_green),
        },
        DeploymentConfigRow {
            case: "a deployment that reuses it",
            sent: None,
            retain: Some(true),
            expected: Some(&blue_green),
        },
        DeploymentConfigRow {
            case: "a deployment that sends another",
            sent: Some(&rolling),
            retain: None,
            expected: Some(&rolling),
        },
        DeploymentConfigRow {
            case: "a deployment that sends none",
            sent: None,
            retain: None,
            expected: None,
        },
        DeploymentConfigRow {
            case: "a deployment that reuses none",
            sent: None,
            retain: Some(true),
            expected: None,
        },
    ];

    for row in rows {
        let case = row.case;
        let mut body = json!({ "EndpointName": "cfg-ep", "EndpointConfigName": "cfg-deployed" });
        if let Some(sent) = row.sent {
            body["DeploymentConfig"] = sent.clone();
        }
        if let Some(retain) = row.retain {
            body["RetainDeploymentConfig"] = json!(retain);
        }
        emulator.ok("UpdateEndpoint", &body).await;

        let settled = poll_until(&emulator, case, "cfg-ep", "InService").await;
        assert_eq!(
            settled.get("LastDeploymentConfig"),
            row.expected,
            "case {case:?}: LastDeploymentConfig, got {settled}"
        );
    }
}

/// THE RULE: a deployment naming an endpoint or an endpoint configuration the account does
/// not hold is refused with the 400 `ValidationException` this service words for a
/// resource it cannot find, and nothing it addressed is changed.
#[tokio::test]
async fn a_deployment_naming_a_resource_the_account_does_not_hold_is_refused() {
    // Rows differ along which reference is unresolvable and the shape of the name sent:
    // short, hyphenated, and the 63-character modelled maximum (`EndpointName` /
    // `EndpointConfigName` are both `len 0..63`).
    let longest = "a234567890123456789012345678901234567890123456789012345678901b";
    let rows: [(&str, &str, &str, String); 5] = [
        (
            "an endpoint the account does not hold",
            "absent-ep",
            "refusal-deployed",
            "Could not find endpoint \"absent-ep\".".to_string(),
        ),
        (
            "an endpoint named by a single character",
            "a",
            "refusal-deployed",
            "Could not find endpoint \"a\".".to_string(),
        ),
        (
            "an endpoint named at the modelled maximum",
            longest,
            "refusal-deployed",
            format!("Could not find endpoint \"{longest}\"."),
        ),
        (
            "a configuration the account does not hold",
            "refusal-ep",
            "absent-cfg",
            "Could not find endpoint configuration \"absent-cfg\".".to_string(),
        ),
        (
            "a configuration named at the modelled maximum",
            "refusal-ep",
            longest,
            format!("Could not find endpoint configuration \"{longest}\"."),
        ),
    ];

    let emulator = Emulator::new();
    let serving = json!([variant("AllTraffic", Some(1), None, "ml.m5.large")]);
    given_serving_endpoint(&emulator, "refusals", "refusal-ep", "refusal-serving", &serving).await;
    given_config(&emulator, "refusal-deployed", &serving, None).await;

    for (case, endpoint, config, expected) in rows {
        let (status, refused) = emulator
            .call(
                "UpdateEndpoint",
                &json!({ "EndpointName": endpoint, "EndpointConfigName": config }),
            )
            .await;
        assert_eq!(
            (status, error_code(&refused).as_str(), message(&refused)),
            (400, "ValidationException", expected),
            "case {case:?}: expected the not-found refusal, got {status} {refused}"
        );

        // The endpoint that does exist is untouched: it still serves its own
        // configuration and has nothing in flight.
        let read = emulator
            .ok("DescribeEndpoint", &json!({ "EndpointName": "refusal-ep" }))
            .await;
        assert_eq!(
            (
                str_field(&read, "EndpointStatus"),
                str_field(&read, "EndpointConfigName"),
            ),
            ("InService".to_string(), "refusal-serving".to_string()),
            "case {case:?}: a refused deployment changes nothing, got {read}"
        );
        assert!(
            read.get("PendingDeploymentSummary").is_none(),
            "case {case:?}: a refused deployment starts nothing, got {read}"
        );
    }
}

/// THE RULE: an endpoint that is not serving is part-way through a change of its own, so a
/// deployment asked for on it is refused with the 400 `ValidationException` naming it as
/// in progress — and the change it was already running is left alone.
#[tokio::test]
async fn a_deployment_on_an_endpoint_that_is_not_serving_is_refused() {
    let emulator = Emulator::new();
    let serving = json!([variant("AllTraffic", Some(1), None, "ml.m5.large")]);
    emulator
        .ok(
            "CreateModel",
            &json!({ "ModelName": MODEL, "PrimaryContainer": primary_container("demo:1") }),
        )
        .await;
    given_config(&emulator, "progress-serving", &serving, None).await;
    given_config(&emulator, "progress-first", &serving, None).await;
    given_config(&emulator, "progress-second", &serving, None).await;
    let created = emulator
        .ok(
            "CreateEndpoint",
            &json!({
                "EndpointName": "progress-ep",
                "EndpointConfigName": "progress-serving",
            }),
        )
        .await;
    let endpoint_arn = str_field(&created, "EndpointArn");
    let expected = format!("Cannot update in-progress endpoint \"{endpoint_arn}\".");

    // An endpoint that is still coming up is not serving, so a deployment over the create
    // is refused.
    let (status, refused) = emulator
        .call(
            "UpdateEndpoint",
            &json!({ "EndpointName": "progress-ep", "EndpointConfigName": "progress-first" }),
        )
        .await;
    assert_eq!(
        (status, error_code(&refused).as_str(), message(&refused)),
        (400, "ValidationException", expected.clone()),
        "case \"an endpoint that is still coming up\": got {status} {refused}"
    );

    // Once it is serving, the first deployment is accepted — and while THAT deployment is
    // in flight a second one is refused the same way, leaving the first running.
    poll_until(&emulator, "in progress", "progress-ep", "InService").await;
    emulator
        .ok(
            "UpdateEndpoint",
            &json!({ "EndpointName": "progress-ep", "EndpointConfigName": "progress-first" }),
        )
        .await;
    let (status, refused) = emulator
        .call(
            "UpdateEndpoint",
            &json!({ "EndpointName": "progress-ep", "EndpointConfigName": "progress-second" }),
        )
        .await;
    assert_eq!(
        (status, error_code(&refused).as_str(), message(&refused)),
        (400, "ValidationException", expected),
        "case \"an endpoint already being deployed to\": got {status} {refused}"
    );

    let settled = poll_until(&emulator, "in progress", "progress-ep", "InService").await;
    assert_eq!(
        str_field(&settled, "EndpointConfigName"),
        "progress-first",
        "case \"an endpoint already being deployed to\": the refused call left the running \
         deployment alone, got {settled}"
    );
}

/// THE RULE: the new fleet is stood up while the old one keeps serving, so a deployment's
/// instances are taken on top of what the account already hosts — past the hosting quota
/// of an instance type the deployment is refused `ResourceLimitExceeded` (HTTP 400) naming
/// that quota, its limit, the utilisation and the delta, and nothing is deployed.
#[tokio::test]
async fn a_deployment_past_the_hosting_quota_of_an_instance_type_is_refused() {
    // Rows differ along the instance family and the count asked for: a generation an
    // account is granted no hosting capacity on by default (so even one instance is past
    // the quota), another size of one, and a type that HAS capacity, asked for one
    // instance past the bound. Source for every value:
    // `smithy_query(query="shape",shape="ProductionVariantInstanceType")`.
    let quota = service_com_amazonaws_sagemaker_sagemaker::state::DEFAULT_ENDPOINT_USAGE_QUOTA;
    let rows: [(&str, &str, i64, i64); 3] = [
        ("a zero-capacity GPU generation", "ml.p5.48xlarge", 1, 0),
        ("another zero-capacity generation", "ml.trn1.32xlarge", 2, 0),
        (
            "a type with capacity, one instance past the bound",
            "ml.m5.large",
            quota + 1,
            quota,
        ),
    ];

    for (case, instance_type, instances, limit) in rows {
        let emulator = Emulator::new();
        let serving = json!([variant("AllTraffic", Some(1), None, "ml.m5.large")]);
        given_serving_endpoint(&emulator, case, "quota-ep", "quota-serving", &serving).await;
        let deployed = json!([variant("AllTraffic", Some(instances), None, instance_type)]);
        given_config(&emulator, "quota-deployed", &deployed, None).await;

        // The endpoint that is serving already holds one instance of its own type, which
        // is the utilisation a deployment of that same type is measured against.
        let in_use = i64::from(instance_type == "ml.m5.large");
        let (status, refused) = emulator
            .call(
                "UpdateEndpoint",
                &json!({ "EndpointName": "quota-ep", "EndpointConfigName": "quota-deployed" }),
            )
            .await;
        assert_eq!(
            (status, error_code(&refused).as_str(), message(&refused)),
            (
                400,
                "ResourceLimitExceeded",
                over_quota(instance_type, limit, in_use, instances)
            ),
            "case {case:?}: expected the quota, the limit, the utilisation and the delta, \
             got {status} {refused}"
        );

        // Nothing was deployed: the endpoint still serves what it was created on.
        let read = emulator
            .ok("DescribeEndpoint", &json!({ "EndpointName": "quota-ep" }))
            .await;
        assert_eq!(
            (
                str_field(&read, "EndpointStatus"),
                str_field(&read, "EndpointConfigName"),
            ),
            ("InService".to_string(), "quota-serving".to_string()),
            "case {case:?}: a refused deployment changes nothing, got {read}"
        );
        assert!(
            read.get("PendingDeploymentSummary").is_none(),
            "case {case:?}: a refused deployment starts nothing, got {read}"
        );
    }
}
