// SPDX-License-Identifier: CC0-1.0

//! The two KINDS of network interface attachment, and which of them the account may manage —
//! driven over `ec2Query` (README.md / OPERATIONS.md "Wire protocol").
//!
//! RULE: an interface attached to an INSTANCE of the account and one a service attached for a
//! resource of its own (no instance holds it) are attachments of different kinds, and EC2
//! prints them in different id forms — `eni-attach-…` for the first, `ela-attach-…` for the
//! second. Both forms are ids EC2 issues, so a value of either is LOOKED UP rather than
//! refused as malformed; but an attachment of the service-managed kind is not the account's to
//! take down, so `DetachNetworkInterface` refuses it HTTP 400 `OperationNotPermitted` with the
//! sentence naming the KIND the caller may not manage, and the attachment does not move.
//!
//! The rows differ along what the rule generalises over: the kind of the attachment, whether
//! the account really holds one under the id, which of the two published id widths the value
//! carries (8 and 17 hexadecimal digits), and the forms outside the printed one that name no
//! attachment that could ever have existed.
//!
//! Sibling: [`super::detach_network_interface`] covers what a detach of an attachment the
//! account DOES hold does, on the clock.

use ec2::ops::types::NetworkInterfaceAttachment;
use ec2::state::{
    ComAmazonawsEc2Amazonec2State as Ec2State, INVALID_ATTACHMENT_ID_NOT_FOUND,
    INVALID_NETWORK_INTERFACE_ATTACHMENT_ID_MALFORMED, NETWORK_INTERFACE_ATTACHMENT_ID_PREFIX,
    NETWORK_INTERFACE_TYPE_INTERFACE, NetworkInterfaceSpec,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, error_code, error_message, filters, indexed};
use super::wire::{Param, child, child_text, document, param};

/// The Region every call here is signed for.
const REGION: &str = "us-east-1";

/// The id form of an attachment NO instance of the account holds — one a service made for a
/// resource of its own. Written out rather than taken from the crate, so the assertion is
/// independent of the constant the implementation carries.
const SERVICE_ATTACHMENT_PREFIX: &str = "ela-attach";

/// "The Amazon Web Services account ID of the owner of the instance"
/// (`NetworkInterfaceAttachment$InstanceOwnerId`) for an attachment AWS itself made, which is
/// what one no instance of the account holds carries in place of an account id.
const SERVICE_ATTACHMENT_OWNER: &str = "amazon-aws";

/// The instant a seeded attachment was made, as a fixed point rather than a clock read.
const SEEDED_ATTACH_TIME_SECS: i64 = 1_767_225_600;

/// The error code EC2 answers for an action the caller may not take on the resource it named.
const OPERATION_NOT_PERMITTED: &str = "OperationNotPermitted";

/// One call that must succeed, as its body.
async fn ok(case: &str, router: &MultiServiceRouter, action: &str, params: &[Param]) -> String {
    let (status, body) = call(router, REGION, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    body
}

/// One `DetachNetworkInterface` call for `attachment_id`.
async fn detach(router: &MultiServiceRouter, attachment_id: &str) -> (u16, String) {
    call(
        router,
        REGION,
        "DetachNetworkInterface",
        &[param("AttachmentId", attachment_id)],
    )
    .await
}

/// The account's first default subnet, read back over the wire.
async fn default_subnet(router: &MultiServiceRouter) -> String {
    let vpcs = ok(
        "the default network",
        router,
        "DescribeVpcs",
        &filters(&[("is-default", &["true"])]),
    )
    .await;
    let vpc_id = element(&vpcs, "vpcId").unwrap_or_default();
    let body = ok(
        "the default network",
        router,
        "DescribeSubnets",
        &filters(&[("vpc-id", &[vpc_id.as_str()])]),
    )
    .await;
    element(&body, "subnetId").expect("a fresh account has a default subnet")
}

/// Seed one interface in `subnet_id` already attached, through the one writer seam every
/// interface of this store is written by, and answer its interface id and attachment id.
///
/// `service_managed` picks the KIND of attachment: a service attachment, whose id carries the
/// service-managed prefix and which names no instance, or an ordinary instance attachment.
/// Seeded rather than made over the wire so the suite stands on an attachment that exists
/// however it came to exist — what a detach does to one is independent of the path that made
/// it.
fn seed_attached_interface(
    engine: &engine::InMemoryEngine,
    subnet_id: &str,
    service_managed: bool,
) -> (String, String) {
    let state = engine.state::<Ec2State>();
    let mut store = state.lock();
    let subnet = store
        .subnets
        .get(subnet_id)
        .cloned()
        .unwrap_or_else(|| panic!("the store holds the subnet {subnet_id}"));
    let cidr_block = subnet.cidr_block.clone().unwrap_or_default();
    let address = store
        .mint_private_ip_in_cidr(&cidr_block)
        .unwrap_or_else(|| panic!("the subnet {subnet_id} has a free address to seed with"));
    let prefix = if service_managed {
        SERVICE_ATTACHMENT_PREFIX
    } else {
        NETWORK_INTERFACE_ATTACHMENT_ID_PREFIX
    };
    let attachment_id = store.next_id(prefix);
    let network_interface_id = store
        .insert_network_interface(NetworkInterfaceSpec {
            subnet_id: subnet_id.to_string(),
            vpc_id: subnet.vpc_id.clone().unwrap_or_default(),
            availability_zone: subnet.availability_zone.clone().unwrap_or_default(),
            availability_zone_id: subnet.availability_zone_id.clone().unwrap_or_default(),
            region: REGION.to_string(),
            interface_type: NETWORK_INTERFACE_TYPE_INTERFACE.to_string(),
            private_ip_addresses: vec![address],
            attachment: Some(NetworkInterfaceAttachment {
                attach_time: Some(::aws_smithy_types::DateTime::from_secs(
                    SEEDED_ATTACH_TIME_SECS,
                )),
                attachment_id: Some(attachment_id.clone()),
                delete_on_termination: Some(false),
                // Never the primary device index, which is refused for its own reason.
                device_index: Some(1),
                network_card_index: Some(0),
                instance_id: (!service_managed).then(|| "i-0123456789abcdef0".to_string()),
                instance_owner_id: Some(if service_managed {
                    SERVICE_ATTACHMENT_OWNER.to_string()
                } else {
                    engine::emulator_account_id().to_string()
                }),
                status: Some("attached".to_string()),
                ena_srd_specification: None,
                ena_queue_count: None,
            }),
            ..NetworkInterfaceSpec::default()
        })
        .expect("the store holds the seeded network interface");
    (network_interface_id, attachment_id)
}

/// The attachment id and status `DescribeNetworkInterfaces` reports for one interface.
async fn reported_attachment(
    case: &str,
    router: &MultiServiceRouter,
    network_interface_id: &str,
) -> (Option<String>, Option<String>) {
    let body = ok(
        case,
        router,
        "DescribeNetworkInterfaces",
        &indexed("NetworkInterfaceId", &[network_interface_id]),
    )
    .await;
    let parsed = document(case, &body);
    let item = parsed
        .descendants()
        .find(|node| node.is_element() && child(*node, "networkInterfaceId").is_some())
        .unwrap_or_else(|| panic!("case {case:?}: no interface in {body}"));
    let attachment = child(item, "attachment");
    (
        attachment.and_then(|attachment| child_text(attachment, "attachmentId")),
        attachment.and_then(|attachment| child_text(attachment, "status")),
    )
}

/// RULE: an attachment of the service-managed kind is refused `OperationNotPermitted` naming
/// that kind and does not move, while an attachment of the account's own instance kind is
/// taken down.
#[tokio::test]
async fn an_attachment_the_account_does_not_hold_is_refused_naming_its_kind() {
    let (router, engine) = super::ec2_query::emulator();
    let subnet_id = default_subnet(&router).await;
    let (service_interface, service_attachment) =
        seed_attached_interface(&engine, &subnet_id, true);
    let (instance_interface, instance_attachment) =
        seed_attached_interface(&engine, &subnet_id, false);

    // The service attachment reads as the kind its id form says, where a client reads it.
    let (reported_id, reported_status) =
        reported_attachment("the seeded service attachment", &router, &service_interface).await;
    assert_eq!(
        reported_id.as_deref(),
        Some(service_attachment.as_str()),
        "the describe must report the seeded attachment, got {reported_id:?}"
    );
    assert!(
        service_attachment.starts_with(&format!("{SERVICE_ATTACHMENT_PREFIX}-")),
        "the service attachment's id must carry the service-managed form, got \
         {service_attachment:?}"
    );

    let case = "a service attachment the account holds";
    let (status, body) = detach(&router, &service_attachment).await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400, got {status} with body {body}"
    );
    assert_eq!(
        error_code(&body),
        OPERATION_NOT_PERMITTED,
        "case {case:?}: expected {OPERATION_NOT_PERMITTED}, got {body}"
    );
    let message = error_message(&body);
    assert_eq!(
        message,
        format!("You are not allowed to manage '{SERVICE_ATTACHMENT_PREFIX}' attachments."),
        "case {case:?}: expected the refusal naming the kind the caller may not manage, got \
         {message:?}"
    );
    let after = reported_attachment(case, &router, &service_interface).await;
    assert_eq!(
        after,
        (reported_id.clone(), reported_status.clone()),
        "case {case:?}: the refused detach must leave the attachment exactly as it was"
    );

    // And the account's own instance attachment, of the other kind, is taken down: the refusal
    // above is about the KIND, not about every attachment.
    let case = "an instance attachment of the account";
    let (status, body) = detach(&router, &instance_attachment).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200, got {status} with body {body}"
    );
    let (_, moved) = reported_attachment(case, &router, &instance_interface).await;
    assert_eq!(
        moved.as_deref(),
        Some("detaching"),
        "case {case:?}: expected the attachment on its way down, got {moved:?}"
    );
}

/// One row of the id-form table: the case, the value the request carries, and the code it is
/// refused with.
type FormRow = (&'static str, String, &'static str);

/// RULE: a value of EITHER printed attachment-id form that names no attachment of the account
/// is looked up and answered `InvalidAttachmentID.NotFound`, while a value outside those forms
/// is refused `InvalidNetworkInterfaceAttachmentId.Malformed` before anything is looked up.
#[tokio::test]
async fn both_attachment_id_forms_are_looked_up_and_everything_else_is_malformed() {
    let (router, _engine) = super::ec2_query::emulator();

    // The two widths EC2 issues ids in, for each of the two kinds, plus the forms around them.
    let rows: Vec<FormRow> = vec![
        (
            "a service attachment id of the long form naming nothing",
            format!("{SERVICE_ATTACHMENT_PREFIX}-0123456789abcdef0"),
            INVALID_ATTACHMENT_ID_NOT_FOUND,
        ),
        (
            "a service attachment id of the short form naming nothing",
            format!("{SERVICE_ATTACHMENT_PREFIX}-1a2b3c4d"),
            INVALID_ATTACHMENT_ID_NOT_FOUND,
        ),
        (
            "an instance attachment id of the long form naming nothing",
            format!("{NETWORK_INTERFACE_ATTACHMENT_ID_PREFIX}-0123456789abcdef0"),
            INVALID_ATTACHMENT_ID_NOT_FOUND,
        ),
        (
            "an instance attachment id of the short form naming nothing",
            format!("{NETWORK_INTERFACE_ATTACHMENT_ID_PREFIX}-1a2b3c4d"),
            INVALID_ATTACHMENT_ID_NOT_FOUND,
        ),
        (
            "a service attachment id one digit short of either width",
            format!("{SERVICE_ATTACHMENT_PREFIX}-1a2b3c4"),
            INVALID_NETWORK_INTERFACE_ATTACHMENT_ID_MALFORMED,
        ),
        (
            "a service attachment id with a digit outside hexadecimal",
            format!("{SERVICE_ATTACHMENT_PREFIX}-0123456789abcdefg"),
            INVALID_NETWORK_INTERFACE_ATTACHMENT_ID_MALFORMED,
        ),
        (
            "another resource's id",
            "eni-0123456789abcdef0".to_string(),
            INVALID_NETWORK_INTERFACE_ATTACHMENT_ID_MALFORMED,
        ),
        (
            "not an id at all",
            "the-attachment".to_string(),
            INVALID_NETWORK_INTERFACE_ATTACHMENT_ID_MALFORMED,
        ),
    ];

    for (case, attachment_id, expected) in rows {
        let (status, body) = detach(&router, &attachment_id).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400 for {attachment_id}, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            expected,
            "case {case:?}: expected {expected} for {attachment_id}, got {body}"
        );
    }
}
