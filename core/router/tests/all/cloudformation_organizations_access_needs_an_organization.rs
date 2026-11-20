// SPDX-License-Identifier: CC0-1.0

//! CloudFormation's StackSets TRUSTED ACCESS switch is about an ORGANIZATION, and the
//! organization is AWS Organizations' own resource — so the standing the switch judges by is
//! reachable over the wire: a client calls `Organizations:CreateOrganization` and the switch
//! opens up behind it.
//!
//! The rule under test: **an account that is in no organization has no StackSets trusted
//! access to report on, activate or deactivate, so each of
//! `DescribeOrganizationsAccess`, `ActivateOrganizationsAccess` and
//! `DeactivateOrganizationsAccess` is refused `400` `Sender`/`ValidationError` with "You must
//! have an organization setup before operating a SERVICE_MANAGED stack set" and writes
//! nothing; once an organization exists, every one of them is served and the status a read
//! reports is the one the writes left (`DISABLED` until an activation, `ENABLED` after one).**
//! Whether the account is in an organization is read from Organizations' store when the
//! decision is made, so only a cross-service suite can pin it: each crate's own tests can
//! assert one half.
//!
//! Why it matters: a client probing whether SERVICE_MANAGED stack sets are usable is told they
//! are, then every service-managed StackSet operation it goes on to attempt fails later and
//! for a different reason.
//!
//! The rows differ along what the rule generalises over: which of the three calls is made,
//! how a read addresses the administrator account it is made from (naming no `CallAs`, naming
//! `SELF`), and the feature set the organization is created with.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::query_wire::{form_encode, opt_element};

/// The Region every call here is signed for. Trusted access is organization-wide, so the
/// Region selects nothing; one is needed because the credential scope is what routes a
/// request.
const REGION: &str = "us-east-1";

/// The sentence every call of the switch is refused with for an account in no organization.
/// Spelled out here rather than read from the crate, because the wording IS the requirement.
const NO_ORGANIZATION: &str =
    "You must have an organization setup before operating a SERVICE_MANAGED stack set";

/// This service's one wire code for a request it refuses on input grounds, and the `Type` its
/// envelope carries for a refusal the CALLER is answerable for.
const VALIDATION_ERROR: &str = "ValidationError";
const SENDER: &str = "Sender";

/// The two modelled `OrganizationStatus` values a call of this API can leave the setting in.
const DISABLED: &str = "DISABLED";
const ENABLED: &str = "ENABLED";

/// A SigV4 `Authorization` header whose credential scope names `service` in [`REGION`], which
/// is what routes the request. The signature is never checked: this router runs with request
/// authentication off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0"
    )
}

/// CloudFormation and Organizations mounted on ONE engine, as `core/router/src/lib.rs` mounts
/// them, over the state a freshly booted emulator holds: an account in no organization.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_organizations_awsorganizationsv20161128::EmulatedService::new(engine),
    );
    router
}

/// One CloudFormation call over its awsQuery wire.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-05-15");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("cloudformation.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("cloudformation"))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = String::from_utf8_lossy(response.body()).to_string();
    (status, payload)
}

/// One Organizations call over its awsJson1_1 wire.
async fn organizations(
    router: &MultiServiceRouter,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("organizations.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header(
            "x-amz-target",
            format!("AWSOrganizationsV20161128.{operation}"),
        )
        .header("authorization", authorization("organizations"))
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let decoded = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, decoded)
}

/// One call of the switch: what it is, the operation it names, and the members it sends.
struct Call {
    case: &'static str,
    action: &'static str,
    members: &'static [(&'static str, &'static str)],
}

/// Every call of the switch, with the members a row sends it: the read addressed each way a
/// management account can address it, and the two writes.
const CALLS: &[Call] = &[
    Call {
        case: "the read naming no CallAs",
        action: "DescribeOrganizationsAccess",
        members: &[],
    },
    Call {
        case: "the read naming CallAs=SELF",
        action: "DescribeOrganizationsAccess",
        members: &[("CallAs", "SELF")],
    },
    Call {
        case: "the activation",
        action: "ActivateOrganizationsAccess",
        members: &[],
    },
    Call {
        case: "the deactivation",
        action: "DeactivateOrganizationsAccess",
        members: &[],
    },
];

/// Assert one refused call: the status, the envelope's `Type`, its code and the whole
/// sentence.
fn assert_refused(case: &str, answered: &(u16, String)) {
    let (status, body) = answered;
    assert_eq!(
        *status, 400,
        "case {case:?}: expected the refusal, got {status} {body}"
    );
    assert_eq!(
        (
            opt_element(body, "Type").unwrap_or_default(),
            opt_element(body, "Code").unwrap_or_default(),
            opt_element(body, "Message").unwrap_or_default(),
        ),
        (
            SENDER.to_string(),
            VALIDATION_ERROR.to_string(),
            NO_ORGANIZATION.to_string()
        ),
        "case {case:?}: expected the no-organization refusal, got {body}"
    );
}

/// The `Status` a read reports, which must be served.
async fn reported_status(router: &MultiServiceRouter, case: &str) -> String {
    let (status, body) = cloudformation(router, "DescribeOrganizationsAccess", &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the read to be served, got {status} {body}"
    );
    opt_element(&body, "Status")
        .unwrap_or_else(|| panic!("case {case:?}: expected a Status, got {body}"))
}

#[tokio::test]
async fn the_trusted_access_switch_needs_an_organization() {
    // The feature set the organization is created with, which the switch does not turn on:
    // the member's own value set, plus the request that omits it.
    for feature_set in [None, Some("ALL"), Some("CONSOLIDATED_BILLING")] {
        let world = match feature_set {
            None => "an organization created with no FeatureSet".to_string(),
            Some(value) => format!("an organization created with FeatureSet={value}"),
        };
        let router = build_router();

        // Before the organization exists every call of the switch is refused, and none of
        // them writes: the read that follows the refused ACTIVATION still reports the
        // setting a fresh account has.
        for call in CALLS {
            let case = format!("{}, before {world}", call.case);
            let answered = cloudformation(&router, call.action, call.members).await;
            assert_refused(&case, &answered);
        }

        // The one request that establishes the fact, sent by a client that speaks only HTTP.
        let request = match feature_set {
            None => json!({}),
            Some(value) => json!({ "FeatureSet": value }),
        };
        let (status, created) = organizations(&router, "CreateOrganization", &request).await;
        assert_eq!(
            status, 200,
            "case {world:?}: expected the organization to be created, got {status} {created:?}"
        );

        // Behind it the read is served, and reports the setting of an organization nobody has
        // activated trusted access for.
        let reported = reported_status(&router, &format!("the first read after {world}")).await;
        assert_eq!(
            reported, DISABLED,
            "case {world:?}: expected {DISABLED} for an organization nobody activated trusted \
             access for, got {reported:?}"
        );

        // ...and so are the writes, which is what moves what the read reports.
        for (what, action, expected) in [
            ("the activation", "ActivateOrganizationsAccess", ENABLED),
            ("the deactivation", "DeactivateOrganizationsAccess", DISABLED),
        ] {
            let case = format!("{what}, after {world}");
            let (status, body) = cloudformation(&router, action, &[]).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected the write to be served, got {status} {body}"
            );
            let reported = reported_status(&router, &case).await;
            assert_eq!(
                reported, expected,
                "case {case:?}: expected the read to report {expected:?}, got {reported:?}"
            );
        }
    }
}
