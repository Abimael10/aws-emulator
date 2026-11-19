// SPDX-License-Identifier: CC0-1.0

//! How the tag plane RESOLVES the resource ids a request names — `CreateTags` and
//! `DeleteTags`, end to end over the wire the service speaks (`ec2Query`, through the shared
//! [`super::ec2_query`] harness).
//!
//! The rule under test: reading an id's SYNTAX and LOOKING that id UP are separate steps. A
//! value in the printed form of a resource kind EC2 publishes — the kind's prefix, a `-`, and
//! either 8 or 17 lowercase hexadecimal digits — that this account holds no resource for is
//! refused with THAT KIND's own `Invalid<Kind>.NotFound` code and that kind's not-found
//! sentence, naming the id the request carried; whether the resource never existed or existed
//! and was deleted makes no difference, because both are "no such resource". Only a value
//! OUTSIDE that form is an id the call cannot read at all, which is the malformed-id refusal
//! `InvalidID` / "The ID '<value>' is not valid".
//!
//! A client keys its teardown verification, its idempotent deletes and its drift detection on
//! that distinction: collapsing a resource that is gone into "you sent me garbage" makes a
//! successful destroy indistinguishable from a client bug.
//!
//! The rows of each table are distinct inputs along what the rule generalises over: which
//! resource KIND the id names, which of the two published id FORMS it carries, and — for the
//! malformed half — which way the value leaves that form (alphabet, length, separator, case).
//! No row is the specification.

use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, error_code, error_message, tagging};

/// The Region every call here is signed for.
const REGION: &str = "us-west-2";

/// The tag a refused request would have written or removed. Its contents are immaterial to
/// every assertion here: each row is refused at the id, before any tag is looked at.
const TAG: [(&str, &str); 1] = [("env", "prod")];

/// The tagging operations that resolve a resource id without being told its kind. Both reach
/// the ONE id-resolution step, so every row below is asked of both: neither may read an id a
/// way the other does not.
const TAGGING_OPERATIONS: [&str; 2] = ["CreateTags", "DeleteTags"];

/// The refusal a tagging call must answer for one resource id: the HTTP status, the error code
/// and the message, with the id the REQUEST named interpolated into it.
struct Refusal {
    /// What distinguishes this row from the others, named in every assertion.
    case: &'static str,
    /// The resource id the request carries.
    resource_id: String,
    /// The error code EC2 answers.
    code: &'static str,
    /// The message EC2 answers, already carrying `resource_id`.
    message: String,
}

/// Drive `resource_id` through both tagging operations and assert the refusal.
async fn assert_refused(router: &MultiServiceRouter, expected: &Refusal) {
    let Refusal {
        case,
        resource_id,
        code,
        message,
    } = expected;
    for action in TAGGING_OPERATIONS {
        let (status, body) = call(router, REGION, action, &tagging(resource_id, &TAG)).await;
        assert_eq!(
            status, 400,
            "case {case:?}: {action} for {resource_id} must be refused with HTTP 400, \
             got {status}: {body}"
        );
        assert_eq!(
            error_code(&body),
            *code,
            "case {case:?}: {action} for {resource_id} must answer the error code {code}, \
             got {body}"
        );
        assert_eq!(
            error_message(&body),
            *message,
            "case {case:?}: {action} for {resource_id} must answer the message {message:?}, \
             got {:?}",
            error_message(&body)
        );
    }
}

/// An id that READS as one of a published kind but names no resource of the account is that
/// kind's own not-found refusal, for every kind the tag plane resolves and in BOTH published id
/// forms.
///
/// The kinds and their sentences are the ones `crate::state::untaggable_resource_id` catalogues
/// — each kind's `Invalid<Kind>.NotFound` refusal in `crate::state`, the same one that kind's
/// own describes and deletes answer for an id nothing holds. The two id forms are the ones
/// `crate::state::prefixed_id_is_well_formed` reads: a prefix, a `-`, and 8 or 17 lowercase
/// hexadecimal digits (the long form is AWS's documented `i-xxxxxxxxxxxxxxxxx`; the model puts
/// no `@pattern` on a resource id — `smithy_query(query="shape",shape="SubnetId")` is a plain
/// string).
#[tokio::test]
async fn an_unknown_but_well_formed_id_is_refused_as_its_kinds_not_found() {
    // `(case, the id nothing holds, the error code, the message template's fixed text around
    // the id)`
    let rows: [(&str, &str, &str, &str, &str); 12] = [
        (
            "a long-form subnet id nothing was created for",
            "subnet-0a1b2c3d4e5f60718",
            "InvalidSubnetID.NotFound",
            "The subnet ID '",
            "' does not exist",
        ),
        (
            // The SHORT form is a form EC2 still READS, so an id in it is LOOKED UP rather
            // than called unreadable.
            "a short-form subnet id, the other form EC2 reads",
            "subnet-0a1b2c3d",
            "InvalidSubnetID.NotFound",
            "The subnet ID '",
            "' does not exist",
        ),
        (
            "a VPC id nothing was created for",
            "vpc-00000000000000999",
            "InvalidVpcID.NotFound",
            "The vpc ID '",
            "' does not exist",
        ),
        (
            "a short-form security group id nothing was created for",
            "sg-1f2e3d4c",
            "InvalidGroup.NotFound",
            "The security group '",
            "' does not exist",
        ),
        (
            "a network interface id nothing was created for",
            "eni-0123456789abcdef0",
            "InvalidNetworkInterfaceID.NotFound",
            "The networkInterface ID '",
            "' does not exist",
        ),
        (
            // A kind whose sentence names the resource rather than its ID member, and the one
            // that ends in a full stop: the wording is per kind, not one template.
            "a volume id nothing was created for",
            "vol-00000000000000999",
            "InvalidVolume.NotFound",
            "The volume '",
            "' does not exist.",
        ),
        (
            "an internet gateway id nothing was created for",
            "igw-0f1e2d3c",
            "InvalidInternetGatewayID.NotFound",
            "The internetGateway ID '",
            "' does not exist",
        ),
        (
            "a route table id nothing was created for",
            "rtb-00000000000000777",
            "InvalidRouteTableID.NotFound",
            "The routeTable ID '",
            "' does not exist",
        ),
        (
            "a network ACL id nothing was created for",
            "acl-0abcdef1234567890",
            "InvalidNetworkAclID.NotFound",
            "The networkAcl ID '",
            "' does not exist",
        ),
        (
            "a DHCP options id nothing was created for",
            "dopt-0a0b0c0d",
            "InvalidDhcpOptionID.NotFound",
            "The dhcpOptions ID '",
            "' does not exist",
        ),
        (
            "an instance id nothing was launched for",
            "i-00000000000000999",
            "InvalidInstanceID.NotFound",
            "The instance ID '",
            "' does not exist",
        ),
        (
            "a launch template id nothing was created for",
            "lt-00000000000000999",
            "InvalidLaunchTemplateId.NotFound",
            "The specified launch template, with template ID ",
            ", does not exist.",
        ),
    ];

    // Nothing is created here, so one emulator serves every row: each id names no resource
    // whichever row asked for it.
    let router = emulator();
    for (case, resource_id, code, before, after) in rows {
        assert_refused(
            &router,
            &Refusal {
                case,
                resource_id: resource_id.to_string(),
                code,
                message: format!("{before}{resource_id}{after}"),
            },
        )
        .await;
    }
}

/// The other half of the rule: a value OUTSIDE the published id form is refused as an id the
/// call cannot read, whatever kind's prefix it starts with — one refusal for every way a value
/// leaves that form.
#[tokio::test]
async fn a_value_outside_the_published_id_form_is_refused_as_an_unreadable_id() {
    // `(case, the value the request carries)` — the code and the message are the same for
    // every row, so they are asserted once below rather than repeated per row.
    let rows: [(&str, &str); 8] = [
        ("a value that is no resource id at all", "not-an-id"),
        (
            "a subnet id whose digits are not hexadecimal",
            "subnet-zzzzzzzz",
        ),
        (
            "a subnet id shorter than either published form",
            "subnet-0a1b2c",
        ),
        (
            "a subnet id one digit above the long form",
            "subnet-0a1b2c3d4e5f607189",
        ),
        ("a subnet prefix carrying no digits at all", "subnet-"),
        (
            "a subnet id in uppercase hexadecimal, which EC2 does not issue",
            "subnet-0A1B2C3D",
        ),
        (
            "a VPC id one digit below the short form",
            "vpc-0a1b2c3",
        ),
        ("a prefix with no separator before its digits", "vpc0a1b2c3d"),
    ];

    let router = emulator();
    for (case, value) in rows {
        assert_refused(
            &router,
            &Refusal {
                case,
                resource_id: value.to_string(),
                code: "InvalidID",
                message: format!("The ID '{value}' is not valid"),
            },
        )
        .await;
    }
}

/// The rule where the issue meets it: a resource this account CREATED and then DELETED is no
/// different from one that never existed — tagging it answers the kind's own not-found refusal,
/// not the malformed-id one, so a client can prove a destroyed estate is gone.
///
/// Each row creates the resource, tags it while it exists (so the refusal below is about the
/// resource being gone rather than about the request), deletes it through the delete EC2
/// publishes for the kind, and then asks both tagging calls for it again.
#[tokio::test]
async fn a_resource_the_account_deleted_is_tagged_with_its_kinds_not_found() {
    // `(case, the kind, the delete that takes it away, the member that names it there, the
    // error code, the message's fixed text around the id)`
    let rows: [(&str, Kind, &str, &str, &str, &str, &str); 4] = [
        (
            "a subnet the account deleted",
            Kind::Subnet,
            "DeleteSubnet",
            "SubnetId",
            "InvalidSubnetID.NotFound",
            "The subnet ID '",
            "' does not exist",
        ),
        (
            "a VPC the account deleted",
            Kind::Vpc,
            "DeleteVpc",
            "VpcId",
            "InvalidVpcID.NotFound",
            "The vpc ID '",
            "' does not exist",
        ),
        (
            "a security group the account deleted",
            Kind::SecurityGroup,
            "DeleteSecurityGroup",
            "GroupId",
            "InvalidGroup.NotFound",
            "The security group '",
            "' does not exist",
        ),
        (
            "an internet gateway the account deleted",
            Kind::InternetGateway,
            "DeleteInternetGateway",
            "InternetGatewayId",
            "InvalidInternetGatewayID.NotFound",
            "The internetGateway ID '",
            "' does not exist",
        ),
    ];

    for (case, kind, delete, id_member, code, before, after) in rows {
        let router = emulator();
        let resource_id = create_resource(&router, case, kind).await;

        // Tagged while it exists: the write succeeds and the tag plane reports it, so the
        // id resolved before the delete.
        let (status, body) = call(
            &router,
            REGION,
            "CreateTags",
            &tagging(&resource_id, &TAG),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: tagging {resource_id} while it exists must succeed, \
             got {status}: {body}"
        );
        let reported = super::ec2_query::tag_report(&router, REGION, case, &resource_id).await;
        assert!(
            reported
                .iter()
                .any(|(id, _, key, value)| id == &resource_id
                    && key.as_str() == TAG[0].0
                    && value.as_str() == TAG[0].1),
            "case {case:?}: DescribeTags must report {:?} on {resource_id}, got {reported:?}",
            TAG[0]
        );

        let (status, body) = call(
            &router,
            REGION,
            delete,
            &[(id_member.to_string(), resource_id.clone())],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: {delete} for {resource_id} must succeed, got {status}: {body}"
        );

        assert_refused(
            &router,
            &Refusal {
                case,
                resource_id: resource_id.clone(),
                code,
                message: format!("{before}{resource_id}{after}"),
            },
        )
        .await;

        // The resource is gone, so the tag plane reports nothing for its id either: the
        // refusal above is the account holding no such resource, not a tag left behind.
        let reported = super::ec2_query::tag_report(&router, REGION, case, &resource_id).await;
        assert!(
            reported.is_empty(),
            "case {case:?}: DescribeTags must report nothing for the deleted {resource_id}, \
             got {reported:?}"
        );
    }
}

/// The resource kinds the round trip above creates and deletes, each through the create EC2
/// publishes for it.
#[derive(Debug, Clone, Copy)]
enum Kind {
    Vpc,
    Subnet,
    SecurityGroup,
    InternetGateway,
}

fn emulator() -> MultiServiceRouter {
    super::ec2_query::emulator().0
}

/// A `(key, value)` request parameter.
fn p(key: &str, value: impl Into<String>) -> (String, String) {
    (key.to_string(), value.into())
}

/// Create one resource of `kind` and answer the id EC2 minted for it.
async fn create_resource(router: &MultiServiceRouter, case: &str, kind: Kind) -> String {
    match kind {
        Kind::Vpc => create_vpc(router, case).await,
        Kind::Subnet => {
            let vpc_id = create_vpc(router, case).await;
            created_id(
                router,
                case,
                "CreateSubnet",
                &[p("VpcId", &vpc_id), p("CidrBlock", "10.3.1.0/24")],
                "subnetId",
            )
            .await
        }
        Kind::SecurityGroup => {
            let vpc_id = create_vpc(router, case).await;
            created_id(
                router,
                case,
                "CreateSecurityGroup",
                &[
                    p("GroupName", "a-group-this-suite-deletes"),
                    p("GroupDescription", "a group this suite deletes"),
                    p("VpcId", &vpc_id),
                ],
                "groupId",
            )
            .await
        }
        Kind::InternetGateway => {
            created_id(
                router,
                case,
                "CreateInternetGateway",
                &[],
                "internetGatewayId",
            )
            .await
        }
    }
}

/// One VPC, as the owner of the kinds that need one.
async fn create_vpc(router: &MultiServiceRouter, case: &str) -> String {
    created_id(
        router,
        case,
        "CreateVpc",
        &[p("CidrBlock", "10.3.0.0/16")],
        "vpcId",
    )
    .await
}

/// Call `action`, require HTTP 200, and answer the `id_element` of the response.
async fn created_id(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    params: &[(String, String)],
    id_element: &str,
) -> String {
    let (status, body) = call(router, REGION, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: {action} must succeed, got {status}: {body}"
    );
    super::ec2_query::element(&body, id_element)
        .unwrap_or_else(|| panic!("case {case:?}: expected a <{id_element}> in {body}"))
}
