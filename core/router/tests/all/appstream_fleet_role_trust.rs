// SPDX-License-Identifier: CC0-1.0

//! AppStream's fleet role check, across the two services it spans.
//!
//! RULE: an AppStream resource that RUNS AS a caller-supplied IAM role may only be created
//! under a role this account's IAM holds and whose trust policy lets
//! `appstream.amazonaws.com` assume it; a role that fails either test is refused with
//! `InvalidRoleException` (HTTP 400) carrying AWS's sentence for that class, before any
//! state is written, so the read that follows reports the resource missing. A fleet is such
//! a resource (`CreateFleet$IamRoleArn` is "the Amazon Resource Name (ARN) of the IAM role
//! to apply to the fleet"), exactly as an app block builder is — the sibling suite
//! `all/appstream_app_block_builder_role_trust.rs` asserts the same rule on the builder,
//! and this one asserts it on the fleet.
//!
//! The rows differ along whether IAM holds the role at all (a name nothing was created
//! under, a name under a path IAM holds nothing under, a name that is a prefix of one that
//! exists), along the form of the trust policy the held role carries (which service
//! principal it names, whether the grant is an `Allow` or a `Deny`, and which action it
//! names), along the KIND of fleet the request asks for (each `FleetType` the model
//! declares, with the members that fleet type carries), and along whether the request names
//! a role in the first place.
//!
//! The role is created through the real `CreateRole` on the same engine that serves
//! AppStream, so what the gate reads is the document IAM persisted and not a copy; that is
//! why this suite lives beside the router rather than in the service crate.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The harness that stands both services up on one engine, speaks awsJson1_1 to AppStream
// and awsQuery to IAM, and reads a failure body is the sibling suite's: a suite that writes
// its own drifts from it.
use crate::appstream_app_block_builder_role_trust::{
    appstream, emulator, error_code, error_message, invalid_role_sentence, role_with_trust,
    trust_allowing,
};

/// The account every emulated resource belongs to.
const ACCOUNT: &str = engine::EMULATOR_ACCOUNT_ID;

/// The service principal AppStream assumes a caller-supplied role as, as the crate states
/// it.
const SERVICE_PRINCIPAL: &str =
    service_com_amazonaws_appstream_photonadminproxyservice::state::SERVICE_PRINCIPAL;

/// AWS's sentence for a fleet this account does not hold, as the crate states it.
fn fleet_not_found_sentence(name: &str) -> String {
    service_com_amazonaws_appstream_photonadminproxyservice::state::resource_not_found_message(
        service_com_amazonaws_appstream_photonadminproxyservice::state::FLEET_KIND,
        name,
    )
}

/// One fleet shape: a case word and the `CreateFleet` members that shape carries beside the
/// name and the role.
///
/// The shapes span `FLEET_TYPE_VALUES` (`src/ops/types/shared_types.rs`: `ALWAYS_ON`,
/// `ELASTIC`, `ON_DEMAND`) plus the request that names no fleet type at all, so the rule is
/// asserted over the kinds of fleet a caller can ask for rather than over one of them.
struct Shape {
    case: &'static str,
    members: fn() -> Value,
}

/// The fleet shapes every role row below is asked for under.
fn shapes() -> Vec<Shape> {
    vec![
        Shape {
            case: "no fleet type, the required members only",
            members: || json!({ "InstanceType": "stream.standard.small" }),
        },
        Shape {
            case: "an elastic fleet",
            members: || {
                json!({
                    "InstanceType": "stream.standard.small",
                    "FleetType": "ELASTIC",
                    "Platform": "WINDOWS_SERVER_2019",
                    "MaxConcurrentSessions": 5,
                    "VpcConfig": { "SubnetIds": ["subnet-00000000", "subnet-11111111"] },
                })
            },
        },
        Shape {
            case: "an always-on fleet",
            members: || {
                json!({
                    "InstanceType": "stream.standard.medium",
                    "FleetType": "ALWAYS_ON",
                    "ComputeCapacity": { "DesiredInstances": 2 },
                })
            },
        },
        Shape {
            case: "an on-demand fleet",
            members: || {
                json!({
                    "InstanceType": "stream.standard.large",
                    "FleetType": "ON_DEMAND",
                    "ComputeCapacity": { "DesiredInstances": 1 },
                    "MaxUserDurationInSeconds": 3600,
                })
            },
        },
    ]
}

/// A `CreateFleet` request for `name` of `shape` that runs as `role_arn`, if any.
fn create_fleet_body(shape: &Shape, name: &str, role_arn: Option<&str>) -> Value {
    let mut body = (shape.members)();
    body["Name"] = json!(name);
    if let Some(role_arn) = role_arn {
        body["IamRoleArn"] = json!(role_arn);
    }
    body
}

/// Assert that this account holds no fleet under `name`, which is what a refused create
/// leaves behind.
async fn assert_no_fleet(router: &MultiServiceRouter, case: &str, name: &str) {
    let (status, payload) = appstream(router, "DescribeFleets", json!({ "Names": [name] })).await;
    assert_eq!(
        status, 400,
        "case {case:?}: a refused create must leave no fleet to describe, got {status} {payload}"
    );
    assert_eq!(
        error_code(&payload),
        "ResourceNotFoundException",
        "case {case:?}: expected the read to report the fleet missing, got {payload}"
    );
    assert_eq!(
        error_message(&payload),
        fleet_not_found_sentence(name),
        "case {case:?}: expected AWS's sentence for a fleet that does not exist, got {payload}"
    );
}

/// Assert that the fleet `name` is stored and runs as `role_arn`.
async fn assert_fleet_runs_as(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    role_arn: Option<&str>,
) {
    let (status, payload) = appstream(router, "DescribeFleets", json!({ "Names": [name] })).await;
    assert_eq!(
        status, 200,
        "case {case:?}: an accepted create must leave a fleet to describe, got {status} {payload}"
    );
    assert_eq!(
        payload.pointer("/Fleets/0/IamRoleArn").and_then(Value::as_str),
        role_arn,
        "case {case:?}: expected the stored fleet to run as {role_arn:?}, got {payload}"
    );
}

/// One role row: how the role is set up, and whether a fleet that runs as it is created.
struct Row {
    case: &'static str,
    /// The trust policy IAM holds the role under — `None` when IAM holds no such role, so
    /// the ARN names a role that does not exist.
    document: Option<String>,
    /// The role name the ARN addresses. For a held role this is the name IAM holds it
    /// under; for an absent one it is the name nothing was created under.
    role_name: &'static str,
    accepted: bool,
}

/// The role rows: the two halves of the question (does IAM hold the role, and does its
/// trust policy admit this service) and the forms each half takes.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a role whose trust policy names appstream",
            document: Some(trust_allowing(&[SERVICE_PRINCIPAL])),
            role_name: "fleet-role-allowed",
            accepted: true,
        },
        Row {
            case: "a role whose trust policy names appstream among several services",
            document: Some(trust_allowing(&["ec2.amazonaws.com", SERVICE_PRINCIPAL])),
            role_name: "fleet-role-allowed-among-several",
            accepted: true,
        },
        Row {
            case: "a role whose trust policy names another service only",
            document: Some(trust_allowing(&["ec2.amazonaws.com"])),
            role_name: "fleet-role-other-service",
            accepted: false,
        },
        Row {
            case: "a role whose trust policy denies appstream",
            document: Some(
                json!({
                    "Version": "2012-10-17",
                    "Statement": [{
                        "Effect": "Deny",
                        "Principal": { "Service": SERVICE_PRINCIPAL },
                        "Action": "sts:AssumeRole"
                    }]
                })
                .to_string(),
            ),
            role_name: "fleet-role-denied",
            accepted: false,
        },
        Row {
            // The statement names appstream, but for an action that is not assuming the
            // role, so it grants nothing: a trust policy governs `sts:AssumeRole`.
            case: "a role that allows appstream an action that is not assuming it",
            document: Some(
                json!({
                    "Version": "2012-10-17",
                    "Statement": [{
                        "Effect": "Allow",
                        "Principal": { "Service": SERVICE_PRINCIPAL },
                        "Action": "s3:GetObject"
                    }]
                })
                .to_string(),
            ),
            role_name: "fleet-role-other-action",
            accepted: false,
        },
        Row {
            case: "a role nothing was created under",
            document: None,
            role_name: "fleet-role-absent",
            accepted: false,
        },
    ]
}

#[tokio::test]
async fn create_fleet_refuses_a_role_this_service_cannot_assume() {
    let router = emulator();

    for (row_index, row) in rows().into_iter().enumerate() {
        let role_arn = match &row.document {
            Some(document) => role_with_trust(&router, row.role_name, document).await,
            // Nothing is created under the name, so the ARN is the one a request would
            // carry for a role this account does not hold.
            None => format!("arn:aws:iam::{ACCOUNT}:role/{}", row.role_name),
        };

        for (shape_index, shape) in shapes().into_iter().enumerate() {
            let case = format!("{} — {}", row.case, shape.case);
            let name = format!("role-fleet-{row_index}-{shape_index}");
            let (status, payload) = appstream(
                &router,
                "CreateFleet",
                create_fleet_body(&shape, &name, Some(&role_arn)),
            )
            .await;

            if row.accepted {
                assert_eq!(
                    status, 200,
                    "case {case:?}: a role this service may assume must be accepted, got \
                     {status} {payload}"
                );
                assert_eq!(
                    payload.pointer("/Fleet/IamRoleArn").and_then(Value::as_str),
                    Some(role_arn.as_str()),
                    "case {case:?}: the created fleet must carry the role it runs as, got \
                     {payload}"
                );
                assert_fleet_runs_as(&router, &case, &name, Some(&role_arn)).await;
                continue;
            }

            assert_eq!(
                status, 400,
                "case {case:?}: a role this service cannot assume must be refused, got \
                 {status} {payload}"
            );
            assert_eq!(
                error_code(&payload),
                "InvalidRoleException",
                "case {case:?}: expected the modelled role refusal, got {payload}"
            );
            assert_eq!(
                error_message(&payload),
                invalid_role_sentence(&role_arn),
                "case {case:?}: expected AWS's sentence for an invalid role, got {payload}"
            );
            assert_no_fleet(&router, &case, &name).await;
        }
    }
}

#[tokio::test]
async fn create_fleet_refuses_an_arn_naming_no_role_of_this_account() {
    let router = emulator();
    // A role this account's IAM DOES hold, so every row below differs from a name that
    // resolves only in how it addresses the role.
    role_with_trust(
        &router,
        "fleet-held-role",
        &trust_allowing(&[SERVICE_PRINCIPAL]),
    )
    .await;

    // Rows: ARNs that pass the modelled `Arn` pattern and name no role this account's IAM
    // holds — a name nothing was created under reached through a path, a name that is a
    // prefix of one that exists, and a name that extends one that exists.
    for (index, (case, role_arn)) in [
        (
            "a role under a path, named after no role at all",
            format!("arn:aws:iam::{ACCOUNT}:role/service-role/fleet-no-such-role"),
        ),
        (
            "a role name that is a prefix of one that exists",
            format!("arn:aws:iam::{ACCOUNT}:role/fleet-held-rol"),
        ),
        (
            "a role name that extends one that exists",
            format!("arn:aws:iam::{ACCOUNT}:role/fleet-held-role-2"),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let name = format!("absent-role-fleet-{index}");
        let (status, payload) = appstream(
            &router,
            "CreateFleet",
            create_fleet_body(&shapes()[1], &name, Some(&role_arn)),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: a role this account does not hold must be refused, got {status} \
             {payload}"
        );
        assert_eq!(
            error_code(&payload),
            "InvalidRoleException",
            "case {case:?}: expected the modelled role refusal, got {payload}"
        );
        assert_eq!(
            error_message(&payload),
            invalid_role_sentence(&role_arn),
            "case {case:?}: expected AWS's sentence for an invalid role, got {payload}"
        );
        assert_no_fleet(&router, case, &name).await;
    }
}

#[tokio::test]
async fn create_fleet_that_names_no_role_is_created() {
    let router = emulator();

    // The positive control, over every fleet shape: a request that names no role has no
    // role to resolve, so nothing about IAM can refuse it.
    for (index, shape) in shapes().into_iter().enumerate() {
        let case = format!("no role at all — {}", shape.case);
        let name = format!("no-role-fleet-{index}");
        let (status, payload) =
            appstream(&router, "CreateFleet", create_fleet_body(&shape, &name, None)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: a request that names no role must be created, got {status} {payload}"
        );
        assert_eq!(
            payload.pointer("/Fleet/IamRoleArn"),
            None,
            "case {case:?}: the created fleet carries no role, got {payload}"
        );
        assert_fleet_runs_as(&router, &case, &name, None).await;
    }
}
