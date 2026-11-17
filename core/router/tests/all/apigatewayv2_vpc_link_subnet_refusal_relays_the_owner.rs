// SPDX-License-Identifier: CC0-1.0

//! The id a relayed refusal of API Gateway v2 names the refused request by.
//!
//! RULE: where an operation resolves an identifier against the AWS service that OWNS that
//! kind of resource and that service refuses, the `BadRequestException` API Gateway answers
//! carries the owner's own sentence followed by the parenthesised clause an AWS SDK renders a
//! relayed service exception with — the refusing service, the HTTP status it refused with, its
//! error code, the id of the refused request, and the trailing proxy field. The id in that
//! clause is the REFUSED REQUEST's own: it has the shape AWS reports a request id in, and it
//! is a fresh value per call, which is what lets a caller correlate the one refusal it got.
//!
//! WHICH `CreateVpcLink` requests are refused, and that the refusal carries the owner's
//! sentence at all, is [`crate::apigatewayv2_vpc_link_subnets_resolve_in_ec2`]'s rule (the
//! sentence's own fixed text is pinned in the owning crate, by
//! `services/service-com-amazonaws-apigatewayv2-apigatewayv2/tests/all/vpc_link_subnet_resolution.rs`);
//! this suite reaches for that suite's harness — the shared-engine router, the API Gateway
//! wire, EC2's own VPC creation and the id forms EC2 publishes — so neither can drift, and
//! adds only what neither pins: the FORM of the relayed id and its per-call freshness.
//!
//! The rows span what the rule generalises over: the form of the value that names no subnet
//! (EC2's 17-hex id form and its older 8-hex form) and whether the unresolvable identifier is
//! the only one named or follows one EC2 holds, which is also what shows the relayed sentence
//! names the identifier the owner would not resolve rather than the first one named.

use crate::apigatewayv2_vpc_link_subnets_resolve_in_ec2::{
    apigateway, create_body, ec2_vpc, emulator, unresolvable_subnet_ids, VPC_LINKS,
};

/// The SERVICE a relayed clause names when the owner that refused is Amazon EC2, as the AWS
/// SDK for EC2 names itself in an exception it renders.
const OWNER_SERVICE: &str = "AmazonEC2";

/// The EC2 error code for an identifier the account holds no subnet for, and the HTTP status
/// EC2 refuses such a read with.
const OWNER_ERROR_CODE: &str = "InvalidSubnetID.NotFound";
const OWNER_STATUS: u16 = 400;

/// The fixed tail of the relayed clause, after the id of the refused request.
const RELAY_TAIL: &str = "; Proxy: null)";

/// The fixed head of the relayed clause for an identifier the owner of subnets holds nothing
/// for: that service's own sentence about the id the REQUEST sent, then everything of the
/// clause up to the id of the refused request.
fn relay_head(subnet_id: &str) -> String {
    format!(
        "The subnet ID '{subnet_id}' does not exist (Service: {OWNER_SERVICE}; Status Code: \
         {OWNER_STATUS}; Error Code: {OWNER_ERROR_CODE}; Request ID: "
    )
}

/// The id of the refused request `message` names, or `None` for a message that carries no
/// relayed clause at all.
fn relayed_request_id(message: &str, subnet_id: &str) -> Option<String> {
    let head = relay_head(subnet_id);
    let rest = message.strip_prefix(head.as_str())?;
    rest.strip_suffix(RELAY_TAIL).map(str::to_string)
}

/// Whether `id` is spelled the way AWS reports the id of a request: a v4 UUID, so the
/// 8-4-4-4-12 grouping of lowercase hexadecimal digits and nothing else.
fn is_request_id_shaped(id: &str) -> bool {
    let mut groups = id.split('-');
    for width in [8usize, 4, 4, 4, 12] {
        let Some(group) = groups.next() else {
            return false;
        };
        if group.len() != width
            || !group
                .chars()
                .all(|character| matches!(character, '0'..='9' | 'a'..='f'))
        {
            return false;
        }
    }
    groups.next().is_none()
}

#[tokio::test]
async fn a_relayed_refusal_names_the_refused_request_by_a_fresh_id_of_its_own() {
    let (router, _engine) = emulator();
    let held = ec2_vpc(&router, 90, &["us-west-2a"]).await.subnets;

    // (case, the subnets the create names, the id the relayed sentence must be worded around)
    let mut rows: Vec<(String, Vec<String>, String)> = Vec::new();
    for (form, unknown) in unresolvable_subnet_ids() {
        rows.push((
            format!("one subnet the owner does not hold, in {form}"),
            vec![unknown.clone()],
            unknown.clone(),
        ));
        rows.push((
            format!("an unknown subnet AFTER one the owner holds, in {form}"),
            vec![held[0].clone(), unknown.clone()],
            unknown.clone(),
        ));
    }

    let mut relayed_ids: Vec<String> = Vec::new();
    for (index, (case, subnets, named)) in rows.iter().enumerate() {
        let refused = apigateway(
            &router,
            "POST",
            VPC_LINKS,
            Some(create_body(&format!("relayed-{index}"), subnets)),
        )
        .await;
        assert_eq!(
            (refused.status, refused.error_code()),
            (400, "BadRequestException"),
            "case {case:?}: expected a 400 BadRequestException, got {} with {}",
            refused.status,
            refused.body
        );
        let answered = refused.message();
        let Some(request_id) = relayed_request_id(answered, named) else {
            panic!(
                "case {case:?}: expected the owner's sentence then the relayed clause \
                 {:?}<request id>{RELAY_TAIL:?}, got {answered:?}",
                relay_head(named)
            );
        };
        assert!(
            is_request_id_shaped(&request_id),
            "case {case:?}: expected the relayed clause to name the refused request by a \
             request id, got {request_id:?} in {answered:?}"
        );
        relayed_ids.push(request_id);
    }

    // The id is the REFUSED REQUEST's own, one value per call: no two of the refusals above
    // may name the same request.
    for left in 0..relayed_ids.len() {
        for right in (left + 1)..relayed_ids.len() {
            assert_ne!(
                relayed_ids[left], relayed_ids[right],
                "cases {:?} and {:?}: the relayed clause must name each refused request by \
                 its own id",
                rows[left].0, rows[right].0
            );
        }
    }
}
