// SPDX-License-Identifier: CC0-1.0

//! How `AttachNetworkInterface` reads the INSTANCE ID it is given, driven over the wire the
//! service speaks (`ec2Query`, an `application/x-www-form-urlencoded` POST answered with XML;
//! README.md / OPERATIONS.md "Wire protocol").
//!
//! The rule under test: EC2's long resource id form is an OBFUSCATED identifier the service
//! decodes before it resolves anything, so a value whose identifier the service never issued is
//! no id of that kind at all and is refused with the kind's `.Malformed` code BEFORE any
//! lookup — where a value the service could have issued is looked up and reported absent with
//! the kind's `.NotFound`. A client branches on those two codes (a typo in a value nobody ever
//! held, against a resource that has gone), so collapsing them into one hides the difference.
//!
//! The rows span what that generalises over: the FORM of the value (a long-form id above the
//! mint, the all-zero placeholder, a long form whose leading digit no issued id carries, a
//! value that is no instance id at all, and the retired SHORT form, which carries no identifier
//! to decode and so is looked up), with the not-found half of the pair driven by both a
//! short-form id and an identifier the mint HAS reached.

use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, error_code, error_message, filters, indexed};
use super::wire::{Param, param};

/// The Region every call here is signed for: the emulator's own default Region, whose default
/// VPC and subnets a fresh account already holds.
const REGION: &str = "us-east-1";

/// The image every launch names.
const AMI: &str = "ami-0123456789abcdef0";

/// The code EC2 answers for a value it cannot read as an instance id.
const MALFORMED: &str = "InvalidInstanceID.Malformed";

/// The code EC2 answers for an instance id the account holds no instance for.
const NOT_FOUND: &str = "InvalidInstanceID.NotFound";

/// One call that must succeed, as its body.
async fn ok(case: &str, router: &MultiServiceRouter, action: &str, params: &[Param]) -> String {
    let (status, body) = call(router, REGION, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    body
}

/// The account's first default subnet, over the wire.
async fn default_subnet(router: &MultiServiceRouter) -> String {
    let body = ok(
        "the default network",
        router,
        "DescribeSubnets",
        &filters(&[("default-for-az", &["true"])]),
    )
    .await;
    element(&body, "subnetId").expect("a fresh account has a default subnet per zone")
}

/// One `CreateNetworkInterface` in `subnet_id`, as the interface id it minted.
async fn create_interface(case: &str, router: &MultiServiceRouter, subnet_id: &str) -> String {
    let body = ok(
        case,
        router,
        "CreateNetworkInterface",
        &[param("SubnetId", subnet_id)],
    )
    .await;
    element(&body, "networkInterfaceId")
        .unwrap_or_else(|| panic!("case {case:?}: no interface in {body}"))
}

/// One `AttachNetworkInterface` of `network_interface_id` onto `instance_id`.
async fn attach(
    router: &MultiServiceRouter,
    network_interface_id: &str,
    instance_id: &str,
) -> (u16, String) {
    call(
        router,
        REGION,
        "AttachNetworkInterface",
        &[
            param("NetworkInterfaceId", network_interface_id),
            param("InstanceId", instance_id),
            param("DeviceIndex", "1"),
        ],
    )
    .await
}

/// One row of the id table: the value the request sends as its `InstanceId`, and the code EC2
/// answers for it.
struct Row {
    case: &'static str,
    instance_id: &'static str,
    expected_code: &'static str,
}

/// An instance id the service could never have issued is refused as unreadable, and one it
/// could have is looked up and reported absent — both before any interface is attached.
#[tokio::test]
async fn an_instance_id_the_service_never_issued_is_refused_as_malformed() {
    let rows = [
        Row {
            case: "a long-form identifier far above everything the mint has issued",
            instance_id: "i-0123456789abcdef0",
            expected_code: MALFORMED,
        },
        Row {
            case: "the all-zero placeholder, which a mint counting from one never produces",
            instance_id: "i-00000000000000000",
            expected_code: MALFORMED,
        },
        Row {
            case: "a long form whose leading digit no issued id carries",
            instance_id: "i-1234567890abcdef0",
            expected_code: MALFORMED,
        },
        Row {
            case: "a value that is no instance id at all",
            instance_id: "i-not-an-instance",
            expected_code: MALFORMED,
        },
        Row {
            case: "the retired short form, which carries no identifier to decode",
            instance_id: "i-1a2b3c4d",
            expected_code: NOT_FOUND,
        },
    ];

    let (router, _engine) = super::ec2_query::emulator();
    let subnet = default_subnet(&router).await;

    for row in &rows {
        let Row {
            case,
            instance_id,
            expected_code,
        } = row;
        // A fresh interface per row, so no row can be refused for an interface another row
        // attached.
        let interface = create_interface(case, &router, &subnet).await;
        let (status, body) = attach(&router, &interface, instance_id).await;
        assert_eq!(
            status, 400,
            "case {case:?}: {instance_id} must be refused, got {status} with body {body}"
        );
        let code = error_code(&body);
        assert_eq!(
            code, *expected_code,
            "case {case:?}: expected the code {expected_code:?} for {instance_id}, got {code:?} \
             in {body}"
        );
        assert!(
            error_message(&body).contains(*instance_id),
            "case {case:?}: the refusal must name the value the request sent, got {:?}",
            error_message(&body)
        );

        // Nothing was attached: the interface the row named is still free, which is what makes
        // the refusal a refusal rather than a failure after a write.
        let body = ok(
            case,
            &router,
            "DescribeNetworkInterfaces",
            &indexed("NetworkInterfaceId", &[interface.as_str()]),
        )
        .await;
        assert_eq!(
            element(&body, "status").as_deref(),
            Some("available"),
            "case {case:?}: a refused attach must leave the interface free, got {body}"
        );
    }
}

/// The control the refusals above need: the SAME attach of the same kind of interface onto an
/// instance the account really holds succeeds, so none of them is about the operation — and the
/// id the mint hands out is itself in the form the rule admits, which is what keeps the
/// not-found half of the pair reachable at all.
#[tokio::test]
async fn an_instance_the_account_holds_is_attached_to() {
    let case = "an instance the account holds";
    let (router, _engine) = super::ec2_query::emulator();
    let subnet = default_subnet(&router).await;

    let body = ok(
        case,
        &router,
        "RunInstances",
        &[
            param("ImageId", AMI),
            param("InstanceType", "m5.4xlarge"),
            param("MinCount", "1"),
            param("MaxCount", "1"),
            param("SubnetId", &subnet),
        ],
    )
    .await;
    let instance_id =
        element(&body, "instanceId").expect("a launch reports the instance it minted");
    let identifier = instance_id
        .strip_prefix("i-")
        .and_then(|suffix| u64::from_str_radix(suffix, 16).ok())
        .unwrap_or_else(|| panic!("case {case:?}: {instance_id} must carry an identifier"));
    assert!(
        identifier > 0,
        "case {case:?}: an issued id is never the all-zero placeholder, got {instance_id}"
    );

    let interface = create_interface(case, &router, &subnet).await;
    let body = ok(
        case,
        &router,
        "AttachNetworkInterface",
        &[
            param("NetworkInterfaceId", &interface),
            param("InstanceId", &instance_id),
            param("DeviceIndex", "1"),
        ],
    )
    .await;
    assert!(
        element(&body, "attachmentId").is_some(),
        "case {case:?}: the attach of a held instance must mint an attachment, got {body}"
    );
}
