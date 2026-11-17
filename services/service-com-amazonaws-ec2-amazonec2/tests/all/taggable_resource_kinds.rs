// SPDX-License-Identifier: CC0-1.0

//! Which resource ids a tagging call resolves, over the resource KINDS beside the few the
//! tag suites next to this one drive — end to end over the wire the service speaks
//! (`ec2Query`, through the shared [`super::ec2_query`] harness).
//!
//! The rules under test are:
//!
//! 1. `CreateTags` takes "the IDs of the resources" of ANY kind this account holds a
//!    taggable resource of, writes the tags onto the resource the id names, and that write
//!    reads back through the resource's OWN describe and through `DescribeTags` under the
//!    kind's `ResourceType`. `DeleteTags` removes from the same resource, so the two calls
//!    resolve exactly the same ids;
//! 2. a WELL-FORMED id of such a kind that names no resource is refused with that kind's own
//!    not-found refusal — the same one every other operation of the resource answers for the
//!    id — and `InvalidID` is reserved for a value that is no EC2 resource id at all.
//!
//! The rows of each table are distinct inputs along what the rule generalises over: which
//! kind of resource the id names, how that kind's store keeps the resource's tags, and what
//! makes an id unresolvable. No row is the specification: the kinds here are the ones the
//! tagging suites beside this one do not drive, so the rule is asserted away from the
//! resources it was first reported against.

use std::collections::{BTreeMap, BTreeSet};

use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, error_code, tag_report, tagging, DescribedTag};

/// The Region every call here is signed for.
const REGION: &str = "us-west-2";

/// A `(key, value)` request parameter.
fn p(key: &str, value: impl Into<String>) -> (String, String) {
    (key.to_string(), value.into())
}

fn emulator() -> MultiServiceRouter {
    super::ec2_query::emulator().0
}

/// The tags each row writes: a plain value, an EMPTY value (the value a tag given without
/// one is stored with) and a key that differs from the first only in case, so an overwrite
/// and a removal below each have a tag they must leave alone.
const STORED_TAGS: [(&str, &str); 3] = [("env", "prod"), ("blank", ""), ("Env", "staging")];

/// One resource kind this suite tags.
struct Kind {
    /// What the kind is called in the assertions, and the key `create_resource` reads.
    name: &'static str,
    /// The `ResourceType` `DescribeTags` reports a resource of this kind under, as the
    /// model's own catalog of taggable kinds spells it
    /// (`smithy_query(query="shape",shape="ResourceType")`).
    resource_type: &'static str,
    /// The describe that reports this kind's resources, and the request member that narrows
    /// it to one of them.
    describe: (&'static str, &'static str),
    /// A well-formed id of this kind — `<prefix>-` and 17 lowercase hexadecimal digits, the
    /// long form this service mints ids in — that names no resource of this account.
    absent_id: &'static str,
    /// The error code this kind answers for an id of its own that names no resource.
    absent_code: &'static str,
}

/// Every kind this suite drives. They differ in how the store keeps the resource's tags
/// (the record IS the modelled shape for a network ACL and a Traffic Mirror target, and
/// keeps it on a member for a key pair, an event window and a security group rule) and in
/// the id form the request carries.
const KINDS: [Kind; 5] = [
    Kind {
        name: "network-acl",
        resource_type: "network-acl",
        describe: ("DescribeNetworkAcls", "NetworkAclId.1"),
        absent_id: "acl-00000000000000999",
        absent_code: "InvalidNetworkAclID.NotFound",
    },
    Kind {
        name: "key-pair",
        resource_type: "key-pair",
        describe: ("DescribeKeyPairs", "KeyPairId.1"),
        absent_id: "key-00000000000000999",
        absent_code: "InvalidKeyPair.NotFound",
    },
    Kind {
        name: "instance-event-window",
        resource_type: "instance-event-window",
        describe: ("DescribeInstanceEventWindows", "InstanceEventWindowId.1"),
        absent_id: "iew-00000000000000999",
        absent_code: "InvalidInstanceEventWindowIDNotFound",
    },
    Kind {
        name: "security-group-rule",
        resource_type: "security-group-rule",
        describe: ("DescribeSecurityGroupRules", "SecurityGroupRuleId.1"),
        absent_id: "sgr-00000000000000999",
        absent_code: "InvalidSecurityGroupRuleId.NotFound",
    },
    Kind {
        name: "traffic-mirror-target",
        resource_type: "traffic-mirror-target",
        describe: ("DescribeTrafficMirrorTargets", "TrafficMirrorTargetId.1"),
        absent_id: "tmt-00000000000000999",
        absent_code: "InvalidTrafficMirrorTargetId.NotFound",
    },
];

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
    element(&body, id_element)
        .unwrap_or_else(|| panic!("case {case:?}: expected a <{id_element}> in {body}"))
}

/// One VPC, as the owner of the kinds that need one.
async fn create_vpc(router: &MultiServiceRouter, case: &str, cidr: &str) -> String {
    created_id(router, case, "CreateVpc", &[p("CidrBlock", cidr)], "vpcId").await
}

/// Create one resource of `kind` through that kind's own create, and answer its resource id
/// — so every id this suite tags is one the emulator itself minted.
async fn create_resource(router: &MultiServiceRouter, case: &str, kind: &str) -> String {
    match kind {
        "network-acl" => {
            let vpc_id = create_vpc(router, case, "10.6.0.0/16").await;
            created_id(
                router,
                case,
                "CreateNetworkAcl",
                &[p("VpcId", &vpc_id)],
                "networkAclId",
            )
            .await
        }
        "key-pair" => {
            created_id(
                router,
                case,
                "CreateKeyPair",
                &[p("KeyName", "tagged-key")],
                "keyPairId",
            )
            .await
        }
        "instance-event-window" => {
            created_id(
                router,
                case,
                "CreateInstanceEventWindow",
                &[
                    p("Name", "tagged-window"),
                    p("CronExpression", "* 0-4 * * 1"),
                ],
                "instanceEventWindowId",
            )
            .await
        }
        "security-group-rule" => {
            let vpc_id = create_vpc(router, case, "10.4.0.0/16").await;
            let group_id = created_id(
                router,
                case,
                "CreateSecurityGroup",
                &[
                    p("GroupName", "tagged-group"),
                    p("GroupDescription", "a group whose rule this suite tags"),
                    p("VpcId", &vpc_id),
                ],
                "groupId",
            )
            .await;
            created_id(
                router,
                case,
                "AuthorizeSecurityGroupIngress",
                &[
                    p("GroupId", &group_id),
                    p("IpPermissions.1.IpProtocol", "tcp"),
                    p("IpPermissions.1.FromPort", "443"),
                    p("IpPermissions.1.ToPort", "443"),
                    p("IpPermissions.1.IpRanges.1.CidrIp", "10.4.0.0/16"),
                ],
                "securityGroupRuleId",
            )
            .await
        }
        "traffic-mirror-target" => {
            let vpc_id = create_vpc(router, case, "10.5.0.0/16").await;
            let subnet_id = created_id(
                router,
                case,
                "CreateSubnet",
                &[p("VpcId", &vpc_id), p("CidrBlock", "10.5.1.0/24")],
                "subnetId",
            )
            .await;
            let interface_id = created_id(
                router,
                case,
                "CreateNetworkInterface",
                &[p("SubnetId", &subnet_id)],
                "networkInterfaceId",
            )
            .await;
            created_id(
                router,
                case,
                "CreateTrafficMirrorTarget",
                &[p("NetworkInterfaceId", &interface_id)],
                "trafficMirrorTargetId",
            )
            .await
        }
        other => panic!("case {case:?}: this suite knows no resource kind {other:?}"),
    }
}

/// Every `tagSet` entry of a response body, as key -> value. The describes below are
/// narrowed to one resource, so this is that resource's tags as the service reports them.
fn reported_tags(case: &str, body: &str) -> BTreeMap<String, String> {
    let document = roxmltree::Document::parse(body)
        .unwrap_or_else(|error| panic!("case {case:?}: the body must be XML ({error}): {body}"));
    document
        .descendants()
        .filter(|node| node.is_element() && node.tag_name().name() == "tagSet")
        .flat_map(|set| {
            set.children()
                .filter(|node| node.is_element() && node.tag_name().name() == "item")
                .collect::<Vec<_>>()
        })
        .filter_map(|item| {
            let text = |name: &str| {
                item.children()
                    .find(|node| node.is_element() && node.tag_name().name() == name)
                    .map(|node| node.text().unwrap_or_default().to_string())
            };
            Some((text("key")?, text("value").unwrap_or_default()))
        })
        .collect()
}

/// The tags `kind`'s OWN describe reports for `resource_id` — the round trip every
/// assertion about a write reads through.
async fn described_tags(
    router: &MultiServiceRouter,
    case: &str,
    kind: &Kind,
    resource_id: &str,
) -> BTreeMap<String, String> {
    let (action, narrow) = kind.describe;
    let (status, body) = call(router, REGION, action, &[p(narrow, resource_id)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: {action} for {resource_id} must succeed, got {status}: {body}"
    );
    reported_tags(case, &body)
}

/// Rule 1: a resource of any kind this account holds is tagged, reports its tags through
/// its own describe and through `DescribeTags`, has a value overwritten by a second write,
/// and is untagged by `DeleteTags`.
#[tokio::test]
async fn tags_round_trip_through_every_taggable_resource_kind() {
    for kind in &KINDS {
        let case = format!("{}: a tag written, overwritten and removed", kind.name);
        let router = emulator();
        let resource_id = create_resource(&router, &case, kind.name).await;

        let (status, body) = call(
            &router,
            REGION,
            "CreateTags",
            &tagging(&resource_id, &STORED_TAGS),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateTags on {resource_id} must succeed, got {status}: {body}"
        );

        let mut expected: BTreeMap<String, String> = STORED_TAGS
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect();
        let reported = described_tags(&router, &case, kind, &resource_id).await;
        assert_eq!(
            reported, expected,
            "case {case:?}: the {} must report the tags CreateTags wrote, got {reported:?}",
            kind.name
        );

        let expected_report: BTreeSet<DescribedTag> = expected
            .iter()
            .map(|(key, value)| {
                (
                    resource_id.clone(),
                    kind.resource_type.to_string(),
                    key.clone(),
                    value.clone(),
                )
            })
            .collect();
        let actual_report = tag_report(&router, REGION, &case, &resource_id).await;
        assert_eq!(
            actual_report, expected_report,
            "case {case:?}: expected the tag report {expected_report:?}, got {actual_report:?}"
        );

        // "When you specify an existing tag key, the value is overwritten with the new
        // value" (`CreateTags`), and only that key's.
        let (status, body) = call(
            &router,
            REGION,
            "CreateTags",
            &tagging(&resource_id, &[("env", "review")]),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the overwriting CreateTags must succeed, got {status}: {body}"
        );
        expected.insert("env".to_string(), "review".to_string());
        let reported = described_tags(&router, &case, kind, &resource_id).await;
        assert_eq!(
            reported, expected,
            "case {case:?}: expected the tags {expected:?} after the overwrite, got \
             {reported:?}"
        );

        // `DeleteTags` resolves the id the same way, so what one call wrote the other
        // removes — and only the key it names.
        let (status, body) = call(
            &router,
            REGION,
            "DeleteTags",
            &[p("ResourceId.1", &resource_id), p("Tag.1.Key", "env")],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: DeleteTags must succeed, got {status}: {body}"
        );
        expected.remove("env");
        let reported = described_tags(&router, &case, kind, &resource_id).await;
        assert_eq!(
            reported, expected,
            "case {case:?}: expected the tags {expected:?} to be left, got {reported:?}"
        );
    }
}

/// Rule 2: an id of a kind this service publishes that names no resource is refused with
/// that kind's own not-found refusal, by both tagging calls, and nothing of the request is
/// written.
#[tokio::test]
async fn an_id_naming_no_resource_is_refused_with_its_kinds_not_found() {
    for kind in &KINDS {
        for action in ["CreateTags", "DeleteTags"] {
            let case = format!("{}: {action} on an id that names nothing", kind.name);
            let router = emulator();
            // A resource of the kind EXISTS, so the refusal below is about the id the
            // request named rather than about the store holding no resource of its kind.
            let existing = create_resource(&router, &case, kind.name).await;
            assert_ne!(
                existing, kind.absent_id,
                "case {case:?}: the absent id must name no resource this suite created"
            );

            let (status, body) = call(
                &router,
                REGION,
                action,
                &tagging(kind.absent_id, &[("env", "prod")]),
            )
            .await;
            assert_eq!(
                (status, error_code(&body)),
                (400, kind.absent_code.to_string()),
                "case {case:?}: expected {} for {}, got {status}: {body}",
                kind.absent_code,
                kind.absent_id
            );

            let reported = described_tags(&router, &case, kind, &existing).await;
            assert!(
                reported.is_empty(),
                "case {case:?}: a refused {action} must write nothing, got {reported:?}"
            );
        }
    }
}

/// Rule 2's other side: a value that is no EC2 resource id at all is refused as the id it is
/// not (`InvalidID`), whatever kind's prefix it starts with.
#[tokio::test]
async fn a_value_that_is_no_resource_id_is_refused_as_such() {
    // Each row is a distinct way of being outside the two published id forms — 8 or 17
    // lowercase hexadecimal digits behind the kind's prefix — not one of them a kind this
    // service does not hold.
    let rows: [(&str, &str); 7] = [
        ("no prefix and no digits at all", "not-an-id"),
        ("a network ACL prefix with no digits", "acl-"),
        ("a key pair prefix with non-hexadecimal digits", "key-zzzzzzzz"),
        ("an event window prefix with too few digits", "iew-012345"),
        (
            "a Traffic Mirror target prefix with too many digits",
            "tmt-0123456789abcdef01",
        ),
        (
            "a security group rule prefix with an upper-case digit",
            "sgr-0123456789ABCDEF0",
        ),
        ("a bare kind name", "network-acl"),
    ];

    for (case, resource_id) in rows {
        let router = emulator();
        let (status, body) = call(
            &router,
            REGION,
            "CreateTags",
            &tagging(resource_id, &[("env", "prod")]),
        )
        .await;
        assert_eq!(
            (status, error_code(&body)),
            (400, "InvalidID".to_string()),
            "case {case:?}: expected InvalidID for {resource_id:?}, got {status}: {body}"
        );
    }
}
