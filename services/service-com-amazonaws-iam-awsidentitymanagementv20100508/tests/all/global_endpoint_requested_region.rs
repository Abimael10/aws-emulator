// SPDX-License-Identifier: CC0-1.0

//! The Region a request to IAM was made TO, as the authorization gate answers
//! `aws:RequestedRegion` with, end to end over the awsQuery wire.
//!
//! The rule under test: IAM is a GLOBAL service — one endpoint for the whole partition, in
//! `us-east-1` — so every request to it was made to that one endpoint whatever Region the
//! caller was configured for, and `aws:RequestedRegion` answers with that endpoint's Region
//! rather than with the Region the request named in its SigV4 credential scope. A statement
//! conditioned on the key therefore decides the same way for the same call from any client:
//! one requiring the global Region grants, and one requiring another Region grants nothing,
//! however the signing client was pointed.
//!
//! Each test is a TABLE whose rows differ along what the rule generalises over: the
//! condition operator (`StringEquals`, `StringNotEquals`, `StringLike`), whether the clause
//! names one value or a list, which Region it names, and which Region the request's own
//! credential scope names — the knob `call_signed_in_region` turns.
//!
//! Every row drives the call with an access key minted for a fresh IAM user moments
//! earlier, so each row also exercises the bootstrap a least-privilege client makes:
//! `CreateUser`, `PutUserPolicy`, `CreateAccessKey`, then a request signed with that key.
//!
//! The awsQuery harness and the fixture principal are the ones
//! `tests/all/user_lifecycle.rs` and `tests/all/identity_policy_enforcement.rs` own, so a
//! request is built and read exactly one way.

use crate::identity_policy_enforcement::principal;
use crate::user_lifecycle::{
    account_id, assert_access_denied_message, call_signed_in_region, element, error_code, router,
};

/// The Region IAM's one endpoint lives in, which is what the key answers with. Written out
/// here rather than taken from the service under test ("AWS Identity and Access Management
/// endpoints and quotas", AWS General Reference).
const GLOBAL_REGION: &str = "us-east-1";

/// Regions a client can be configured for that are NOT the global endpoint's, used both as
/// the Region a request's credential scope names and as the Region a clause names.
const OTHER_REGION: &str = "eu-west-1";
const FAR_REGION: &str = "ap-southeast-2";

/// One row: the condition clause the caller's only grant for `iam:GetUser` carries, the
/// Region the request's credential scope names, and whether the call must be served.
struct Row {
    case: &'static str,
    /// The JSON of the statement's `Condition` element.
    condition: String,
    signed_in_region: &'static str,
    served: bool,
}

/// The one grant each row puts on its caller: `iam:GetUser` on every resource, under
/// `condition`.
fn conditioned_grant(condition: &str) -> String {
    format!(
        r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow","Action":"iam:GetUser","Resource":"*","Condition":{condition}}}]}}"#
    )
}

/// The rows.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "StringEquals the global Region, signed for it",
            condition: format!(r#"{{"StringEquals":{{"aws:RequestedRegion":"{GLOBAL_REGION}"}}}}"#),
            signed_in_region: GLOBAL_REGION,
            served: true,
        },
        // The rule's own row: the same grant, from a client configured for another Region.
        // The request still reached the one global endpoint, so it is still served.
        Row {
            case: "StringEquals the global Region, signed for another",
            condition: format!(r#"{{"StringEquals":{{"aws:RequestedRegion":"{GLOBAL_REGION}"}}}}"#),
            signed_in_region: OTHER_REGION,
            served: true,
        },
        Row {
            case: "StringEquals another Region, signed for that Region",
            condition: format!(r#"{{"StringEquals":{{"aws:RequestedRegion":"{OTHER_REGION}"}}}}"#),
            signed_in_region: OTHER_REGION,
            served: false,
        },
        Row {
            case: "StringEquals another Region, signed for the global one",
            condition: format!(r#"{{"StringEquals":{{"aws:RequestedRegion":"{FAR_REGION}"}}}}"#),
            signed_in_region: GLOBAL_REGION,
            served: false,
        },
        Row {
            case: "StringEquals a list carrying the global Region",
            condition: format!(
                r#"{{"StringEquals":{{"aws:RequestedRegion":["{FAR_REGION}","{GLOBAL_REGION}"]}}}}"#
            ),
            signed_in_region: FAR_REGION,
            served: true,
        },
        Row {
            case: "StringNotEquals the global Region",
            condition: format!(
                r#"{{"StringNotEquals":{{"aws:RequestedRegion":"{GLOBAL_REGION}"}}}}"#
            ),
            signed_in_region: OTHER_REGION,
            served: false,
        },
        Row {
            case: "StringNotEquals another Region",
            condition: format!(
                r#"{{"StringNotEquals":{{"aws:RequestedRegion":"{OTHER_REGION}"}}}}"#
            ),
            signed_in_region: OTHER_REGION,
            served: true,
        },
        Row {
            case: "StringLike a pattern the global Region matches",
            condition: r#"{"StringLike":{"aws:RequestedRegion":"us-*"}}"#.to_string(),
            signed_in_region: FAR_REGION,
            served: true,
        },
        Row {
            case: "StringLike a pattern the global Region does not match",
            condition: r#"{"StringLike":{"aws:RequestedRegion":"ap-*"}}"#.to_string(),
            signed_in_region: FAR_REGION,
            served: false,
        },
    ]
}

/// RULE: `aws:RequestedRegion` answers with the Region of IAM's one global endpoint for
/// every request, so a clause over the key decides a call the same way whatever Region the
/// signing client named — and a call it does not allow is refused with AWS's `AccessDenied`
/// sentence.
#[tokio::test]
async fn the_requested_region_of_every_iam_call_is_the_global_endpoints() {
    let router = router();

    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        let user_name = format!("region-caller-{index}");
        // The bootstrap a least-privilege client makes: a user, its one policy, and an
        // access key minted for it, which is what the call below is signed with.
        let access_key_id = principal(
            &router,
            case,
            &user_name,
            &[(
                "region-conditioned".to_string(),
                conditioned_grant(&row.condition),
            )],
        )
        .await;

        let (status, body) = call_signed_in_region(
            &router,
            Some(&access_key_id),
            row.signed_in_region,
            "GetUser",
            &[],
        )
        .await;

        if row.served {
            assert_eq!(
                status, 200,
                "case {case:?}: expected the call to be served, got {status}: {body}"
            );
            assert_eq!(
                element(&body, "UserName"),
                user_name,
                "case {case:?}: expected the signing user to be reported, got {:?}",
                element(&body, "UserName")
            );
        } else {
            assert_eq!(
                status, 403,
                "case {case:?}: expected the call to be refused, got {status}: {body}"
            );
            assert_eq!(
                error_code(&body),
                "AccessDenied",
                "case {case:?}: expected the error code AccessDenied, got {:?}",
                error_code(&body)
            );
            assert_access_denied_message(
                case,
                &element(&body, "Message"),
                &format!("arn:aws:iam::{}:user/{user_name}", account_id()),
                "iam:GetUser",
                &format!("user {user_name}"),
                "because no identity-based policy allows the iam:GetUser action",
            );
        }
    }
}
