// SPDX-License-Identifier: CC0-1.0

//! The two halves of attaching a function to a VPC, driven end to end over Lambda, Amazon EC2
//! and IAM on ONE engine: what the function's EXECUTION ROLE has to be allowed before the
//! attachment is made at all, and what the attached function then REPORTS about it.
//!
//! RULE 1: a `CreateFunction` carrying a `VpcConfig` that names a subnet or a security group is
//! refused with the modelled `InvalidParameterValueException` (HTTP 400) — "The provided
//! execution role does not have permissions to call CreateNetworkInterface on EC2" — when the
//! identity-based policies in force on the named execution role do not allow the EC2 network
//! interface calls the attachment takes. The refusal is COMPLETE: no function is stored, so a
//! following `GetFunction` answers `ResourceNotFoundException` naming the ARN the request
//! addressed, and no network interface of the account carries the security group the request
//! named. Widening the role's grant and sending the same request then stands the attachment up,
//! which is the documented repair-and-retry path.
//!
//! RULE 2: the attachment Lambda does make reports the VPC its subnets belong to as
//! `VpcConfigResponse$VpcId` ("The ID of the VPC") on EVERY read of the configuration — the
//! create's own answer, `GetFunctionConfiguration` and `GetFunction` — resolved from the
//! subnets the configuration names rather than taken from the request, which carries no such
//! member (the request shape `VpcConfig` has no `VpcId`).
//!
//! Every role here is created through IAM's own `CreateRole` and `AttachRolePolicy`, naming the
//! AWS managed execution-role policies by their published ARNs, so the gate reads the documents
//! IAM holds. Every VPC, subnet, security group and network interface is minted and read back
//! through Amazon EC2's own API, so nothing here passes off a record of its own as one the
//! account holds.
//!
//! The rows differ along what the rules generalise over: WHICH managed policy the role carries
//! (none, the basic execution policy, the VPC access policy, both), how many subnets the
//! configuration names, and how many security groups it names.

use std::collections::BTreeSet;
use std::sync::Arc;

use aws_sdk_ec2::types::{Filter, NetworkInterface};
use serde_json::{json, Value};
use smithy_server_runtime::{EmbeddedEmulator, MultiServiceRouter};

use crate::event_source_mapping_role_access::{given_role, iam};
use crate::function_lifecycle::{
    create_function, function_arn, json_call, make_activation_due, package_bytes, zip_file, Answer,
};
use crate::vpc_attachment_interfaces::{ec2_client, function_interfaces, network, Network};

/// The AWS managed execution-role policy that grants a function only its log writes, by the
/// ARN AWS publishes it under. It says nothing about EC2.
const BASIC_EXECUTION: &str = "arn:aws:iam::aws:policy/service-role/AWSLambdaBasicExecutionRole";

/// The AWS managed execution-role policy published for a function configured to run inside a
/// VPC, which grants the network interface calls the attachment takes.
const VPC_ACCESS: &str = "arn:aws:iam::aws:policy/service-role/AWSLambdaVPCAccessExecutionRole";

/// The wire code of the modelled 400 an unattachable configuration is refused with.
const INVALID_PARAMETER: &str = "InvalidParameterValueException";

/// The wire code of the modelled 404 a read of a function the account does not hold answers.
const NOT_FOUND: &str = "ResourceNotFoundException";

/// The first of the calls the attachment takes, which is the one a role carrying no EC2 grant
/// at all is refused on.
const FIRST_CALL: &str = "CreateNetworkInterface";

/// How Lambda words the refusal of a role that may not make `action` on EC2: fixed text apart
/// from the call itself.
fn refusal(action: &str) -> String {
    format!("The provided execution role does not have permissions to call {action} on EC2")
}

/// Lambda, the Amazon EC2 service that owns VPCs, subnets, security groups and network
/// interfaces, and the IAM service that owns the execution role, on ONE shared engine — what
/// `core/router` assembles — plus an EC2 client over the router's own in-process callback.
struct Emulator {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
    ec2: aws_sdk_ec2::Client,
}

fn emulator() -> Emulator {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Pinned off so the suite does not depend on how the emulator process was started; these
    // tests assert the attachment and its gate, not the emulator's SigV4 check.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    let callback = EmbeddedEmulator::from_router(router.clone());
    Emulator {
        ec2: ec2_client(&callback),
        engine,
        router,
    }
}

/// Attach the AWS managed policy `policy_arn` to the role `role_name`, through IAM's own
/// `AttachRolePolicy`.
async fn attach(router: &MultiServiceRouter, case: &str, role_name: &str, policy_arn: &str) {
    let (status, body) = iam(
        router,
        "AttachRolePolicy",
        &[("RoleName", role_name), ("PolicyArn", policy_arn)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected AttachRolePolicy {policy_arn:?} on {role_name:?} to answer 200, \
         got {status}: {body}"
    );
}

/// A second (and further) security group of `vpc_id`, for the rows whose configuration names
/// more than one.
///
/// `pub(crate)` because the suite over looking a function's interfaces up in EC2
/// (`tests/all/vpc_attachment_ec2_lookups.rs`) needs a sibling group of the SAME VPC and mints
/// it the same way, through EC2's own `CreateSecurityGroup`.
pub(crate) async fn another_group(
    ec2: &aws_sdk_ec2::Client,
    case: &str,
    vpc_id: &str,
    index: usize,
) -> String {
    let created = ec2
        .create_security_group()
        .vpc_id(vpc_id)
        .group_name(format!("fn-access-extra-{index}-{vpc_id}"))
        .description("a further group a VPC-attached function carries")
        .send()
        .await
        .unwrap_or_else(|error| {
            panic!("case {case:?}: EC2 must create the extra group: {error:?}")
        });
    created
        .group_id()
        .unwrap_or_else(|| panic!("case {case:?}: CreateSecurityGroup must report the group"))
        .to_string()
}

/// One VPC with `subnets` subnets and `groups` security groups, all minted by EC2 itself.
async fn network_with(
    ec2: &aws_sdk_ec2::Client,
    case: &str,
    prefix: u8,
    subnets: usize,
    groups: usize,
) -> (Network, Vec<String>) {
    let held = network(ec2, case, prefix, subnets).await;
    let mut group_ids = vec![held.group_id.clone()];
    while group_ids.len() < groups {
        let index = group_ids.len();
        group_ids.push(another_group(ec2, case, &held.vpc_id, index).await);
    }
    (held, group_ids)
}

/// Every network interface of the account that CARRIES the security group `group_id`, read
/// through EC2's own `DescribeNetworkInterfaces` and the `group-id` filter it documents.
async fn interfaces_carrying(ec2: &aws_sdk_ec2::Client, group_id: &str) -> Vec<NetworkInterface> {
    let described = ec2
        .describe_network_interfaces()
        .filters(Filter::builder().name("group-id").values(group_id).build())
        .send()
        .await
        .expect("EC2 must report the account's network interfaces");
    described.network_interfaces().to_vec()
}

/// `CreateFunction` attaching a function of `name`, running as `role_arn`, to `subnet_ids` with
/// `group_ids`.
async fn create_attached(
    router: &MultiServiceRouter,
    name: &str,
    role_arn: &str,
    subnet_ids: &[String],
    group_ids: &[String],
) -> Answer {
    create_function(
        router,
        &json!({
            "FunctionName": name,
            "Role": role_arn,
            "Runtime": "python3.12",
            "Handler": "index.handler",
            "Code": { "ZipFile": zip_file(&package_bytes(64)) },
            "VpcConfig": { "SubnetIds": subnet_ids, "SecurityGroupIds": group_ids },
        }),
    )
    .await
}

/// `GetFunctionConfiguration` on the function `name`.
async fn get_configuration(router: &MultiServiceRouter, name: &str) -> Answer {
    json_call(
        router,
        "GET",
        &format!("/2015-03-31/functions/{name}/configuration"),
        &Value::Null,
    )
    .await
}

/// `GetFunction` on the function `name`.
async fn get_function(router: &MultiServiceRouter, name: &str) -> Answer {
    json_call(
        router,
        "GET",
        &format!("/2015-03-31/functions/{name}"),
        &Value::Null,
    )
    .await
}

/// The error message of a refusal, whichever casing the wire carries it under.
fn message(answer: &Answer) -> String {
    answer
        .string("message")
        .or_else(|| answer.string("Message"))
        .unwrap_or_default()
}

/// The `VpcConfig` of a `FunctionConfiguration` body, or `Value::Null` when it reports none.
fn reported_config(body: &Value) -> Value {
    body.get("VpcConfig").cloned().unwrap_or(Value::Null)
}

/// Assert that the body `label` names reports the attachment the request asked for, VPC id
/// included — the rule every read of the configuration is held to.
fn assert_reports_attachment(
    case: &str,
    label: &str,
    body: &Value,
    expected_vpc: &str,
    subnet_ids: &[String],
    group_ids: &[String],
) {
    let reported = reported_config(body);
    assert_eq!(
        reported.get("VpcId").and_then(Value::as_str),
        Some(expected_vpc),
        "case {case:?}: expected {label} to report VpcId {expected_vpc:?}, got {reported:?}"
    );
    assert_eq!(
        reported.get("SubnetIds").cloned(),
        Some(json!(subnet_ids)),
        "case {case:?}: expected {label} to report the subnets {subnet_ids:?}, got {reported:?}"
    );
    assert_eq!(
        reported.get("SecurityGroupIds").cloned(),
        Some(json!(group_ids)),
        "case {case:?}: expected {label} to report the groups {group_ids:?}, got {reported:?}"
    );
}

/// Assert that the account holds one interface per named subnet, each carrying every named
/// group, in the VPC those subnets belong to.
async fn assert_interfaces_stood_up(
    case: &str,
    ec2: &aws_sdk_ec2::Client,
    name: &str,
    expected_vpc: &str,
    subnet_ids: &[String],
    group_ids: &[String],
) {
    let interfaces = function_interfaces(ec2, name).await;
    assert_eq!(
        interfaces.len(),
        subnet_ids.len(),
        "case {case:?}: expected one interface per named subnet ({}), got {interfaces:?}",
        subnet_ids.len()
    );
    let sat_in: BTreeSet<String> = interfaces
        .iter()
        .filter_map(|interface| interface.subnet_id().map(str::to_string))
        .collect();
    let asked: BTreeSet<String> = subnet_ids.iter().cloned().collect();
    assert_eq!(
        sat_in, asked,
        "case {case:?}: expected the interfaces in {asked:?}, got {sat_in:?}"
    );
    for interface in &interfaces {
        assert_eq!(
            interface.vpc_id(),
            Some(expected_vpc),
            "case {case:?}: expected the interface in {expected_vpc:?}, got {interface:?}"
        );
        let carried: BTreeSet<String> = interface
            .groups()
            .iter()
            .filter_map(|group| group.group_id().map(str::to_string))
            .collect();
        let named: BTreeSet<String> = group_ids.iter().cloned().collect();
        assert_eq!(
            carried, named,
            "case {case:?}: expected the interface to carry {named:?}, got {carried:?}"
        );
    }
    // And the account's own inventory finds them by the group they carry, which is how a
    // client looks a function's interfaces up.
    for group in group_ids {
        let carrying = interfaces_carrying(ec2, group).await;
        assert_eq!(
            carrying.len(),
            subnet_ids.len(),
            "case {case:?}: expected {} interface(s) carrying {group}, got {carrying:?}",
            subnet_ids.len()
        );
    }
}

/// One row: which managed policies the execution role carries, how large a configuration the
/// create asks for, and whether that create must be refused.
struct Row {
    case: &'static str,
    policies: &'static [&'static str],
    subnets: usize,
    groups: usize,
    refused: bool,
}

/// The catalog: the grant the role carries against the shape of the configuration asked for.
const ROWS: &[Row] = &[
    Row {
        // The recorded case: a role carrying only the log writes.
        case: "only the basic execution policy, one subnet and one group",
        policies: &[BASIC_EXECUTION],
        subnets: 1,
        groups: 1,
        refused: true,
    },
    Row {
        case: "only the basic execution policy, two subnets and two groups",
        policies: &[BASIC_EXECUTION],
        subnets: 2,
        groups: 2,
        refused: true,
    },
    Row {
        // A role carrying no policy at all grants nothing, so it is short of the same call.
        case: "no policy attached at all",
        policies: &[],
        subnets: 1,
        groups: 1,
        refused: true,
    },
    Row {
        // The positive control: the published VPC access policy alone carries the whole set.
        case: "the VPC access policy alone, one subnet and one group",
        policies: &[VPC_ACCESS],
        subnets: 1,
        groups: 1,
        refused: false,
    },
    Row {
        case: "the basic and the VPC access policy, two subnets and two groups",
        policies: &[BASIC_EXECUTION, VPC_ACCESS],
        subnets: 2,
        groups: 2,
        refused: false,
    },
    Row {
        case: "the VPC access policy, three subnets and one group",
        policies: &[VPC_ACCESS],
        subnets: 3,
        groups: 1,
        refused: false,
    },
];

#[tokio::test]
async fn a_vpc_create_is_refused_unless_the_role_may_create_the_interfaces() {
    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let emulator = emulator();
        let prefix = u8::try_from(60 + index).unwrap_or(60);
        let (held, group_ids) =
            network_with(&emulator.ec2, case, prefix, row.subnets, row.groups).await;

        let role_name = format!("vpc-grant-role-{index}");
        let role = given_role(&emulator.router, case, &role_name, None).await;
        for policy in row.policies {
            attach(&emulator.router, case, &role_name, policy).await;
        }

        let name = format!("vpc-grant-probe-{index}");
        let answer = create_attached(
            &emulator.router,
            &name,
            &role,
            &held.subnet_ids,
            &group_ids,
        )
        .await;

        if row.refused {
            assert_eq!(
                answer.status,
                400,
                "case {case:?}: expected HTTP 400 from CreateFunction, got {} {}",
                answer.status,
                String::from_utf8_lossy(&answer.body)
            );
            assert!(
                answer.error_code().contains(INVALID_PARAMETER),
                "case {case:?}: expected {INVALID_PARAMETER}, got {}",
                answer.error_code()
            );
            let worded = message(&answer);
            assert_eq!(
                worded,
                refusal(FIRST_CALL),
                "case {case:?}: expected the message {:?}, got {worded:?}",
                refusal(FIRST_CALL)
            );
            // The refusal is complete: the function the request named does not exist, and the
            // read of it answers the modelled 404 naming the ARN it addressed.
            let reading = get_function(&emulator.router, &name).await;
            assert_eq!(
                reading.status,
                404,
                "case {case:?}: a refused create must store no function, got {} {}",
                reading.status,
                String::from_utf8_lossy(&reading.body)
            );
            assert!(
                reading.error_code().contains(NOT_FOUND),
                "case {case:?}: expected {NOT_FOUND}, got {}",
                reading.error_code()
            );
            let expected = format!("Function not found: {}", function_arn(&name));
            let worded = message(&reading);
            assert_eq!(
                worded, expected,
                "case {case:?}: expected the message {expected:?}, got {worded:?}"
            );
            // ... and no interface of the account carries the groups the request named.
            for group in &group_ids {
                let carrying = interfaces_carrying(&emulator.ec2, group).await;
                assert!(
                    carrying.is_empty(),
                    "case {case:?}: a refused create must leave no interface carrying {group}, \
                     got {carrying:?}"
                );
            }
            continue;
        }

        assert_eq!(
            answer.status,
            201,
            "case {case:?}: expected HTTP 201 from CreateFunction, got {} {}",
            answer.status,
            String::from_utf8_lossy(&answer.body)
        );
        // The create's own answer already reports the VPC its subnets belong to, which the
        // request never named.
        assert_reports_attachment(
            case,
            "the create's answer",
            &answer.json(),
            &held.vpc_id,
            &held.subnet_ids,
            &group_ids,
        );
        // ... and so does every read of the configuration, once the function has settled into
        // the state a caller that waited would see.
        make_activation_due(&emulator.engine);
        let reading = get_configuration(&emulator.router, &name).await;
        assert_eq!(
            reading.status,
            200,
            "case {case:?}: expected HTTP 200 from GetFunctionConfiguration, got {} {}",
            reading.status,
            String::from_utf8_lossy(&reading.body)
        );
        assert_eq!(
            reading.string("State").as_deref(),
            Some("Active"),
            "case {case:?}: expected the settled function to be Active, got {}",
            String::from_utf8_lossy(&reading.body)
        );
        assert_reports_attachment(
            case,
            "GetFunctionConfiguration",
            &reading.json(),
            &held.vpc_id,
            &held.subnet_ids,
            &group_ids,
        );
        let whole = get_function(&emulator.router, &name).await;
        assert_eq!(
            whole.status,
            200,
            "case {case:?}: expected HTTP 200 from GetFunction, got {} {}",
            whole.status,
            String::from_utf8_lossy(&whole.body)
        );
        assert_reports_attachment(
            case,
            "GetFunction's Configuration",
            whole.json().get("Configuration").unwrap_or(&Value::Null),
            &held.vpc_id,
            &held.subnet_ids,
            &group_ids,
        );
        assert_interfaces_stood_up(
            case,
            &emulator.ec2,
            &name,
            &held.vpc_id,
            &held.subnet_ids,
            &group_ids,
        )
        .await;
    }
}

/// RULE: the refusal is a repair point, not a dead end — widening the SAME role's grant and
/// sending the SAME request stands the attachment up, and the function then reports the VPC
/// its subnets belong to.
#[tokio::test]
async fn the_same_create_succeeds_once_the_role_carries_the_vpc_access_policy() {
    let case = "a role repaired with the published VPC access policy";
    let emulator = emulator();
    let (held, group_ids) = network_with(&emulator.ec2, case, 70, 2, 1).await;
    let role_name = "vpc-repaired-role";
    let role = given_role(&emulator.router, case, role_name, None).await;
    attach(&emulator.router, case, role_name, BASIC_EXECUTION).await;
    let name = "vpc-repaired-probe";

    let refused = create_attached(
        &emulator.router,
        name,
        &role,
        &held.subnet_ids,
        &group_ids,
    )
    .await;
    assert_eq!(
        refused.status,
        400,
        "case {case:?}: expected the first create to be refused with HTTP 400, got {} {}",
        refused.status,
        String::from_utf8_lossy(&refused.body)
    );
    assert_eq!(
        message(&refused),
        refusal(FIRST_CALL),
        "case {case:?}: expected the refusal {:?}, got {:?}",
        refusal(FIRST_CALL),
        message(&refused)
    );

    // The documented repair: attach the published VPC access policy to the same role.
    attach(&emulator.router, case, role_name, VPC_ACCESS).await;

    let created = create_attached(
        &emulator.router,
        name,
        &role,
        &held.subnet_ids,
        &group_ids,
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "case {case:?}: expected the retried create to answer HTTP 201, got {} {}",
        created.status,
        String::from_utf8_lossy(&created.body)
    );
    make_activation_due(&emulator.engine);
    let reading = get_configuration(&emulator.router, name).await;
    assert_eq!(
        reading.string("State").as_deref(),
        Some("Active"),
        "case {case:?}: expected the retried function to reach Active, got {}",
        String::from_utf8_lossy(&reading.body)
    );
    assert_reports_attachment(
        case,
        "GetFunctionConfiguration after the repair",
        &reading.json(),
        &held.vpc_id,
        &held.subnet_ids,
        &group_ids,
    );
    assert_interfaces_stood_up(
        case,
        &emulator.ec2,
        name,
        &held.vpc_id,
        &held.subnet_ids,
        &group_ids,
    )
    .await;
}
