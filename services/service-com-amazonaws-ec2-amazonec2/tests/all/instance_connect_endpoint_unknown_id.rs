// SPDX-License-Identifier: CC0-1.0

//! How this service WORDS an unresolvable resource id, over the EC2 Instance Connect Endpoint —
//! driven over the wire the service actually speaks: `ec2Query` — an
//! `application/x-www-form-urlencoded` POST whose body carries `Action=…`, answered with XML
//! (README.md / OPERATIONS.md "Wire protocol").
//!
//! RULE: a request naming a well-formed resource id this account holds no resource for is
//! refused with that resource's own `Invalid<Kind>.NotFound` code and EC2's one sentence for
//! that class — `The <resource type> ID '<id>' does not exist`, where `<resource type>` is the
//! resource's modelled `ResourceType` spelling and `<id>` is the id the REQUEST named, quoted.
//! The sentence is a fact of the service's template rather than of one operation, so every
//! operation that cannot resolve such an id words it identically.
//!
//! The rows differ along what the rule generalises over: WHICH operation names the id (the
//! modify, the delete, the describe and the tag plane, which refuses an id of this kind through
//! the same seam), WHICH id form it names (the short and the long id form EC2 issues), and what
//! else the request carries. The per-resource half of the rule is covered beside this by every
//! other suite that asserts one of these refusals; this suite is about the one WORDING.

use ec2::state::{
    INVALID_INSTANCE_CONNECT_ENDPOINT_ID_NOT_FOUND, INSTANCE_CONNECT_ENDPOINT_ID_PREFIX,
    RESOURCE_TYPE_INSTANCE_CONNECT_ENDPOINT,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;

use super::ec2_query::{call, error_code, error_message, filters, tagging};
use super::wire::{param, Param};

/// The Region every call here is signed for; it only selects the ec2Query envelope.
const REGION: &str = "us-east-1";

/// Two ids no endpoint of the account has, in the two id forms EC2 issues: 8 and 17 hexadecimal
/// digits after the resource's prefix ([`INSTANCE_CONNECT_ENDPOINT_ID_PREFIX`]).
fn unissued_ids() -> Vec<String> {
    vec![
        format!("{INSTANCE_CONNECT_ENDPOINT_ID_PREFIX}0a1b2c3d"),
        format!("{INSTANCE_CONNECT_ENDPOINT_ID_PREFIX}0123456789abcdef0"),
    ]
}

/// The operations that can be asked about one such id, each as the action and the fields that
/// name the id in that operation's own request.
fn naming(id: &str) -> Vec<(&'static str, Vec<Param>)> {
    vec![
        (
            "ModifyInstanceConnectEndpoint",
            vec![
                param("InstanceConnectEndpointId", id),
                param("SecurityGroupId.1", "sg-0123456789abcdef0"),
            ],
        ),
        (
            "DeleteInstanceConnectEndpoint",
            vec![param("InstanceConnectEndpointId", id)],
        ),
        (
            "DescribeInstanceConnectEndpoints",
            vec![param("InstanceConnectEndpointId.1", id)],
        ),
        (
            "CreateTags",
            tagging(id, &[("Name", "a tag for an endpoint that does not exist")]),
        ),
        (
            "DeleteTags",
            tagging(id, &[("Name", "a tag for an endpoint that does not exist")]),
        ),
    ]
}

/// The rule: every operation that cannot resolve an EC2 Instance Connect Endpoint id refuses it
/// with the resource's `NotFound` code and EC2's one sentence for an id that names nothing.
#[tokio::test]
async fn an_unresolvable_endpoint_id_is_refused_in_ec2s_one_wording() {
    let (router, _engine) = super::ec2_query::emulator();
    for id in unissued_ids() {
        for (action, params) in naming(&id) {
            let case = format!("{action} naming {id}");
            let (status, body) = call(&router, REGION, action, &params).await;
            assert_eq!(
                status, 400,
                "case {case:?}: expected HTTP 400, got {status} with body {body}"
            );
            let code = error_code(&body);
            assert_eq!(
                code, INVALID_INSTANCE_CONNECT_ENDPOINT_ID_NOT_FOUND,
                "case {case:?}: expected the code \
                 {INVALID_INSTANCE_CONNECT_ENDPOINT_ID_NOT_FOUND}, got {code} with body {body}"
            );
            let expected =
                format!("The {RESOURCE_TYPE_INSTANCE_CONNECT_ENDPOINT} ID '{id}' does not exist");
            let actual = error_message(&body);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected the message {expected:?}, got {actual:?}"
            );
        }
    }
}

/// The rule, the other way round: a FILTER naming such an id resolves nothing rather than being
/// refused — "the ID of the EC2 Instance Connect Endpoint" as a filter value narrows the report,
/// so a value no endpoint holds reports no endpoint at all.
#[tokio::test]
async fn a_filter_naming_an_unresolvable_endpoint_id_reports_nothing() {
    let (router, _engine) = super::ec2_query::emulator();
    for id in unissued_ids() {
        let case = format!("a describe filtered on {id}");
        let (status, body) = call(
            &router,
            REGION,
            "DescribeInstanceConnectEndpoints",
            &filters(&[("instance-connect-endpoint-id", &[id.as_str()])]),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: a filter is not an id to resolve, got {status} with body {body}"
        );
        assert!(
            !body.contains(INSTANCE_CONNECT_ENDPOINT_ID_PREFIX),
            "case {case:?}: expected no endpoint reported, got {body}"
        );
    }
}
