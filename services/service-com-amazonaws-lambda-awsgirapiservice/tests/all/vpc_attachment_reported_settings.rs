// SPDX-License-Identifier: CC0-1.0

//! What a function's VPC attachment REPORTS about itself — the settings its
//! `VpcConfigResponse` carries, and the form of the attachment its interfaces hold in the
//! account's Amazon EC2 inventory.
//!
//! Two rules are under test.
//!
//! 1. A SETTING OF THE ATTACHMENT IS REPORTED, NOT INFERRED FROM A MISSING MEMBER: "Allows
//!    outbound IPv6 traffic on VPC functions that are connected to dual-stack subnets"
//!    (`VpcConfigResponse$Ipv6AllowedForDualStack`) is a setting every attachment has — off on
//!    one whose request did not turn it on — so every read of an attached function's
//!    configuration carries it, as the value the configuration holds. The rows differ along
//!    how the configuration spells the setting (not at all, off, on) and along the write that
//!    applied it (the create, and an update that changes it).
//! 2. AN ATTACHMENT NO INSTANCE OF THE ACCOUNT HOLDS IS OF THE SERVICE-MANAGED KIND: the
//!    interfaces Lambda stands up for a VPC attachment are attached to the service rather than
//!    to an instance, so EC2 prints their `AttachmentId` in the service-managed form
//!    (`ela-attach-…`), and `DetachNetworkInterface` refuses to take one down —
//!    `OperationNotPermitted`, naming the kind the caller may not manage — leaving the
//!    interface in place. That is why a subnet a function is attached to keeps being held
//!    while the attachment stands. The rows differ in how many subnets the attachment spans,
//!    and so in how many interfaces carry it.
//!
//! Every VPC and subnet below was MINTED by Amazon EC2 itself and every interface is read back
//! through its own `DescribeNetworkInterfaces`, on the one shared engine the router mounts both
//! services on — so nothing here passes off a record of its own as one the account holds.

use std::sync::Arc;

use aws_sdk_ec2::error::ProvideErrorMetadata as _;
use aws_sdk_ec2::types::NetworkInterface;
use serde_json::json;
use service_com_amazonaws_lambda_awsgirapiservice::EmulatedService;
use smithy_server_runtime::{EmbeddedEmulator, MultiServiceRouter};

use crate::function_lifecycle::{
    create_function, json_call, make_update_due, package_bytes, zip_file, ROLE,
};
use crate::vpc_attachment_interfaces::{ec2_client, function_interfaces, network};

/// The id form EC2 prints an attachment no instance of the account holds in. Written out
/// rather than taken from the EC2 crate, so the assertion is independent of the constant the
/// implementation carries.
const SERVICE_ATTACHMENT_PREFIX: &str = "ela-attach";

/// The error code EC2 answers for an action the caller may not take on the resource it named.
const OPERATION_NOT_PERMITTED: &str = "OperationNotPermitted";

/// Lambda and the Amazon EC2 service that owns VPCs, subnets and network interfaces, on ONE
/// shared engine, plus an EC2 client over the router's own in-process callback.
struct Emulator {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
    ec2: aws_sdk_ec2::Client,
}

fn emulator() -> Emulator {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Pinned off so the suite does not depend on how the emulator process was started; these
    // tests assert the attachment, not the emulator's SigV4 check.
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(Arc::clone(&engine)));
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
    let callback = EmbeddedEmulator::from_router(router.clone());
    Emulator {
        ec2: ec2_client(&callback),
        engine,
        router,
    }
}

/// `CreateFunction` attaching `name` to `subnet_ids` with `group_ids`, carrying the VPC
/// configuration `vpc_config` names on top of those two members.
async fn create_attached(
    router: &MultiServiceRouter,
    name: &str,
    subnet_ids: &[String],
    group_ids: &[String],
    extra_vpc_members: &[(&str, serde_json::Value)],
) -> crate::function_lifecycle::Answer {
    let mut vpc_config = json!({ "SubnetIds": subnet_ids, "SecurityGroupIds": group_ids });
    for (member, value) in extra_vpc_members {
        vpc_config[*member] = value.clone();
    }
    let body = json!({
        "FunctionName": name,
        "Role": ROLE,
        "Runtime": "python3.12",
        "Handler": "index.handler",
        "Code": { "ZipFile": zip_file(&package_bytes(64)) },
        "VpcConfig": vpc_config,
    });
    create_function(router, &body).await
}

/// The `VpcConfig` a read of the function's configuration reports.
async fn read_vpc_config(router: &MultiServiceRouter, name: &str) -> serde_json::Value {
    let read = json_call(
        router,
        "GET",
        &format!("/2015-03-31/functions/{name}/configuration"),
        &serde_json::Value::Null,
    )
    .await;
    assert_eq!(
        read.status,
        200,
        "expected 200 from GetFunctionConfiguration for {name}, got {} {}",
        read.status,
        String::from_utf8_lossy(&read.body)
    );
    read.json()
        .get("VpcConfig")
        .cloned()
        .unwrap_or(serde_json::Value::Null)
}

/// One row of the setting table: its name, the extra `VpcConfig` members the create carries,
/// and the setting every read of the function must report.
type SettingRow = (&'static str, Vec<(&'static str, serde_json::Value)>, bool);

/// RULE 1: every read of an attached function's configuration reports the dual-stack egress
/// setting the attachment holds — off when the configuration did not turn it on.
#[tokio::test]
async fn an_attachment_reports_its_dual_stack_setting_however_the_request_spelled_it() {
    let emulator = emulator();
    let net = network(&emulator.ec2, "the network the rows attach to", 41, 1).await;

    // (case, the extra VpcConfig members the create carries, the setting every read must
    // report)
    let rows: Vec<SettingRow> = vec![
        ("no word about the setting", Vec::new(), false),
        (
            "the setting spelled off",
            vec![("Ipv6AllowedForDualStack", json!(false))],
            false,
        ),
        (
            "the setting spelled on",
            vec![("Ipv6AllowedForDualStack", json!(true))],
            true,
        ),
    ];

    for (index, (case, extra, expected)) in rows.into_iter().enumerate() {
        let name = format!("dual-stack-{index}");
        let answer = create_attached(
            &emulator.router,
            &name,
            &net.subnet_ids,
            std::slice::from_ref(&net.group_id),
            &extra,
        )
        .await;
        assert_eq!(
            answer.status,
            201,
            "case {case:?}: expected 201 from CreateFunction, got {} {}",
            answer.status,
            String::from_utf8_lossy(&answer.body)
        );
        // The create's own answer, and the round trip through the read — both are built from
        // what was persisted, so both carry the setting.
        for (reader, vpc_config) in [
            ("CreateFunction", answer.json().get("VpcConfig").cloned()),
            (
                "GetFunctionConfiguration",
                Some(read_vpc_config(&emulator.router, &name).await),
            ),
        ] {
            let reported = vpc_config
                .as_ref()
                .and_then(|config| config.get("Ipv6AllowedForDualStack"))
                .and_then(serde_json::Value::as_bool);
            assert_eq!(
                reported,
                Some(expected),
                "case {case:?}: expected {reader} to report Ipv6AllowedForDualStack \
                 {expected}, got {vpc_config:?}"
            );
        }
    }

    // And the other write path: an update that changes the setting is reported with its new
    // value, so the member follows the configuration rather than the create that made it.
    let name = "dual-stack-updated";
    let answer = create_attached(
        &emulator.router,
        name,
        &net.subnet_ids,
        std::slice::from_ref(&net.group_id),
        &[("Ipv6AllowedForDualStack", json!(true))],
    )
    .await;
    assert_eq!(
        answer.status,
        201,
        "the update row: expected 201 from CreateFunction, got {} {}",
        answer.status,
        String::from_utf8_lossy(&answer.body)
    );
    for (case, asked, expected) in [
        ("an update turning it off", json!(false), false),
        ("an update turning it on again", json!(true), true),
    ] {
        // "you can't modify the function" while its last write is still settling: the settle
        // the clock owes is brought FORWARD rather than waited out, so the suite does not
        // depend on how fast the host runs.
        make_update_due(&emulator.engine);
        let updated = json_call(
            &emulator.router,
            "PUT",
            &format!("/2015-03-31/functions/{name}/configuration"),
            &json!({
                "VpcConfig": {
                    "SubnetIds": net.subnet_ids.clone(),
                    "SecurityGroupIds": [net.group_id.clone()],
                    "Ipv6AllowedForDualStack": asked,
                }
            }),
        )
        .await;
        assert_eq!(
            updated.status,
            200,
            "case {case:?}: expected 200 from UpdateFunctionConfiguration, got {} {}",
            updated.status,
            String::from_utf8_lossy(&updated.body)
        );
        let vpc_config = read_vpc_config(&emulator.router, name).await;
        let reported = vpc_config
            .get("Ipv6AllowedForDualStack")
            .and_then(serde_json::Value::as_bool);
        assert_eq!(
            reported,
            Some(expected),
            "case {case:?}: expected the read to report {expected}, got {vpc_config:?}"
        );
    }
}

/// RULE 2: the interfaces a VPC attachment stands up carry an attachment of the
/// service-managed kind, which the account may not take down.
#[tokio::test]
async fn a_function_interface_carries_a_service_attachment_the_account_cannot_detach() {
    let emulator = emulator();
    let one = network(&emulator.ec2, "one subnet", 42, 1).await;
    let two = network(&emulator.ec2, "two subnets", 43, 2).await;

    // (case, the subnets the attachment spans, the group it carries)
    let rows: Vec<(&str, Vec<String>, String)> = vec![
        ("one subnet, so one interface", one.subnet_ids.clone(), one.group_id.clone()),
        (
            "two subnets, so one interface in each",
            two.subnet_ids.clone(),
            two.group_id.clone(),
        ),
    ];

    for (index, (case, subnet_ids, group_id)) in rows.into_iter().enumerate() {
        let name = format!("service-attached-{index}");
        let answer = create_attached(
            &emulator.router,
            &name,
            &subnet_ids,
            std::slice::from_ref(&group_id),
            &[],
        )
        .await;
        assert_eq!(
            answer.status,
            201,
            "case {case:?}: expected 201 from CreateFunction, got {} {}",
            answer.status,
            String::from_utf8_lossy(&answer.body)
        );

        let interfaces = function_interfaces(&emulator.ec2, &name).await;
        assert_eq!(
            interfaces.len(),
            subnet_ids.len(),
            "case {case:?}: expected one interface per subnet, got {interfaces:?}"
        );
        for interface in &interfaces {
            let interface_id = interface
                .network_interface_id()
                .unwrap_or_else(|| panic!("case {case:?}: an interface must report its id"));
            let attachment = interface
                .attachment()
                .unwrap_or_else(|| panic!("case {case:?}: expected an attachment on {interface:?}"));
            let attachment_id = attachment
                .attachment_id()
                .unwrap_or_else(|| panic!("case {case:?}: the attachment must report its id"));
            assert!(
                attachment_id.starts_with(&format!("{SERVICE_ATTACHMENT_PREFIX}-")),
                "case {case:?}: expected an attachment of the service-managed kind, got \
                 {attachment_id:?}"
            );
            assert_eq!(
                attachment.instance_id(),
                None,
                "case {case:?}: no instance of the account holds the interface, got \
                 {attachment:?}"
            );

            // The account may not take it down: the refusal names the kind, and the interface
            // is still attached afterwards.
            let error = emulator
                .ec2
                .detach_network_interface()
                .attachment_id(attachment_id)
                .send()
                .await
                .expect_err("EC2 must refuse to detach a service attachment");
            let service_error = error.into_service_error();
            assert_eq!(
                service_error.code(),
                Some(OPERATION_NOT_PERMITTED),
                "case {case:?}: expected {OPERATION_NOT_PERMITTED}, got {service_error:?}"
            );
            assert_eq!(
                service_error.message(),
                Some(
                    format!("You are not allowed to manage '{SERVICE_ATTACHMENT_PREFIX}' attachments.")
                        .as_str()
                ),
                "case {case:?}: expected the refusal naming the kind the caller may not \
                 manage, got {service_error:?}"
            );
            assert_eq!(
                still_attached(&emulator.ec2, interface_id).await,
                Some(attachment_id.to_string()),
                "case {case:?}: the refused detach must leave {interface_id} attached"
            );
        }
    }
}

/// The attachment id the one interface `network_interface_id` still holds, as EC2's own
/// describe reports it.
async fn still_attached(ec2: &aws_sdk_ec2::Client, network_interface_id: &str) -> Option<String> {
    let described = ec2
        .describe_network_interfaces()
        .network_interface_ids(network_interface_id)
        .send()
        .await
        .expect("EC2 must report the interface");
    described
        .network_interfaces()
        .iter()
        .find(|interface| {
            interface.network_interface_id() == Some(network_interface_id)
        })
        .and_then(NetworkInterface::attachment)
        .and_then(|attachment| attachment.attachment_id().map(str::to_string))
}
