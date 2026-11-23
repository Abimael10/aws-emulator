// SPDX-License-Identifier: CC0-1.0

//! The Spot Fleet request's fleet-role check, across the two services it spans.
//!
//! RULE: a Spot Fleet ACTS UNDER the role the request hands EC2
//! (`SpotFleetRequestConfigData$IamFleetRole` is "the Amazon Resource Name (ARN) of an
//! Identity and Access Management (IAM) role that grants the Spot Fleet the permission to
//! request, launch, terminate, and tag instances on your behalf"), so a role this account's
//! IAM does not hold, or one whose trust policy does not let `spotfleet.amazonaws.com`
//! assume it, is a role the fleet could never act under: the request is refused
//! `InvalidSpotFleetRequestConfig` (HTTP 400) naming the member at fault, before anything is
//! written and before the launch specifications are resolved, so the read that follows
//! reports no fleet.
//!
//! The role is created through the real `CreateRole` on the same engine that serves EC2, so
//! what the check reads is the document IAM persisted and not a copy; that is why this suite
//! lives beside the router rather than in the service crate, whose own suite
//! (`tests/all/refusal_class_before_resource_lookup.rs`) asserts the class, the sentence and
//! the ORDER against an emulator with no IAM mounted.
//!
//! The rows differ along whether IAM holds the role at all (a name nothing was created
//! under, a name that is a prefix of one that exists), along the trust policy a held role
//! carries (which service principal it names, whether it names several, whether the grant is
//! an `Allow`, which action it names), and along whether the fleet the request describes
//! would otherwise have been served.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::json;
use smithy_server_runtime::MultiServiceRouter;

// The percent-encoder that builds the form body of a query call and the readers that pull an
// element's text out of the XML answer are the test crate's own, written once in
// `all/query_wire.rs`: a suite that writes its own drifts from the others.
use crate::query_wire::{element, form_encode, opt_element};

/// The Region every request below is signed for.
const REGION: &str = "us-west-2";

/// The EC2 API version a client sends in an ec2Query body.
const EC2_API_VERSION: &str = "2016-11-15";

/// The IAM API version a client sends in an awsQuery body.
const IAM_API_VERSION: &str = "2010-05-08";

/// The service principal a Spot Fleet assumes its fleet role as, as the EC2 crate states it.
const SERVICE_PRINCIPAL: &str =
    service_com_amazonaws_ec2_amazonec2::ops::request_spot_fleet::SPOT_FLEET_SERVICE_PRINCIPAL;

/// EC2's code for a `SpotFleetRequestConfig` the service cannot act on, as the crate states
/// it.
const CONFIG_CODE: &str =
    service_com_amazonaws_ec2_amazonec2::ops::request_spot_fleet::INVALID_SPOT_FLEET_REQUEST_CONFIG;

/// One emulator carrying both services over ONE shared engine, exactly as the production
/// router builds them.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        engine,
    ));
    router
}

/// The `authorization` header a query call carries: these tests sign nothing, and the
/// envelope only reads the credential scope's Region and service.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=abc"
    )
}

/// One query call to `host`, answered with the HTTP status and the XML body.
async fn query(
    router: &MultiServiceRouter,
    host: &str,
    service: &str,
    body: String,
) -> (u16, String) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", host)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(service))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).to_string();
    (status, text)
}

/// The form body of one query call: the action, the version and `params`.
fn form_body(action: &str, version: &str, params: &[(&str, &str)]) -> String {
    let mut body = format!("Action={action}&Version={version}");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    body
}

/// One IAM call, answered with the HTTP status and the XML body.
async fn iam(router: &MultiServiceRouter, action: &str, params: &[(&str, &str)]) -> (u16, String) {
    query(
        router,
        "iam.amazonaws.com",
        "iam",
        form_body(action, IAM_API_VERSION, params),
    )
    .await
}

/// One EC2 call, answered with the HTTP status and the XML body.
async fn ec2(router: &MultiServiceRouter, action: &str, params: &[(&str, &str)]) -> (u16, String) {
    query(
        router,
        &format!("ec2.{REGION}.amazonaws.com"),
        "ec2",
        form_body(action, EC2_API_VERSION, params),
    )
    .await
}

/// The error code of an ec2Query failure body.
fn error_code(body: &str) -> String {
    opt_element(body, "Code").unwrap_or_else(|| "<no Code>".to_string())
}

/// The error message of an ec2Query failure body.
fn error_message(body: &str) -> String {
    opt_element(body, "Message").unwrap_or_else(|| "<no Message>".to_string())
}

/// A trust policy naming `principals` in one statement with `effect`.
fn trust(effect: &str, principals: &[&str], action: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": effect,
            "Principal": { "Service": principals },
            "Action": action
        }]
    })
    .to_string()
}

/// Create a role in IAM carrying `document`, and hand back the ARN IAM minted for it.
async fn role_with_trust(router: &MultiServiceRouter, name: &str, document: &str) -> String {
    let (status, created) = iam(
        router,
        "CreateRole",
        &[("RoleName", name), ("AssumeRolePolicyDocument", document)],
    )
    .await;
    assert_eq!(
        status, 200,
        "CreateRole {name:?}: expected the role to be persisted, got {status}: {created}"
    );
    element(&created, "Arn")
}

/// An AMI of the published catalog this account can already see, read off `DescribeImages`
/// rather than written down here: a launch specification must name an image the account can
/// resolve, so the suite cannot invent an id.
async fn visible_image(router: &MultiServiceRouter) -> String {
    let (status, body) = ec2(router, "DescribeImages", &[]).await;
    assert_eq!(
        status, 200,
        "DescribeImages must succeed, got {status}: {body}"
    );
    element(&body, "imageId")
}

/// The `RequestSpotFleet` form fields for a fleet acting under `role` and launching `image`.
fn fleet_params<'a>(role: &'a str, image: &'a str) -> Vec<(&'a str, &'a str)> {
    vec![
        ("SpotFleetRequestConfig.IamFleetRole", role),
        ("SpotFleetRequestConfig.TargetCapacity", "1"),
        ("SpotFleetRequestConfig.LaunchSpecifications.1.ImageId", image),
    ]
}

/// How many Spot Fleet requests this account holds, read through the describe.
async fn fleet_count(router: &MultiServiceRouter) -> usize {
    let (status, body) = ec2(router, "DescribeSpotFleetRequests", &[]).await;
    assert_eq!(
        status, 200,
        "DescribeSpotFleetRequests must succeed, got {status}: {body}"
    );
    body.matches("<spotFleetRequestId>").count()
}

/// RULE: the fleet role is resolved against THIS account's IAM records, and a role IAM
/// answers for — held and trusting the Spot Fleet service — is what lets the request through,
/// while every other answer refuses it in the configuration's own class and writes nothing.
#[tokio::test]
async fn a_spot_fleet_runs_only_under_a_role_iam_holds_and_trusts() {
    let router = emulator();
    let image = visible_image(&router).await;
    let account = engine::EMULATOR_ACCOUNT_ID;

    // A role this account holds that trusts the Spot Fleet service: the one row the request
    // is served under.
    let served = role_with_trust(
        &router,
        "aws-ec2-spot-fleet-tagging-role",
        &trust("Allow", &[SERVICE_PRINCIPAL], "sts:AssumeRole"),
    )
    .await;
    // A role this account holds whose trust policy names the Spot Fleet service beside
    // another: naming several principals is still naming this one.
    let served_among_others = role_with_trust(
        &router,
        "fleet-and-ecs",
        &trust(
            "Allow",
            &["ecs.amazonaws.com", SERVICE_PRINCIPAL],
            "sts:AssumeRole",
        ),
    )
    .await;
    // Roles this account holds that the Spot Fleet service cannot assume.
    let another_principal = role_with_trust(
        &router,
        "ecs-only",
        &trust("Allow", &["ecs.amazonaws.com"], "sts:AssumeRole"),
    )
    .await;
    let denied = role_with_trust(
        &router,
        "fleet-denied",
        &trust("Deny", &[SERVICE_PRINCIPAL], "sts:AssumeRole"),
    )
    .await;
    let another_action = role_with_trust(
        &router,
        "fleet-tag-session-only",
        &trust("Allow", &[SERVICE_PRINCIPAL], "sts:TagSession"),
    )
    .await;

    // (case, the fleet role, whether the request is served)
    let rows: Vec<(&str, String, bool)> = vec![
        ("a role IAM holds that trusts the fleet", served, true),
        (
            "a role whose trust names the fleet beside another principal",
            served_among_others,
            true,
        ),
        (
            "a name IAM holds no role under",
            format!("arn:aws:iam::{account}:role/no-such-fleet-role"),
            false,
        ),
        (
            // A name that is a PREFIX of one IAM holds is still a name it holds no role
            // under: the lookup is by the whole name.
            "a name that is a prefix of a held one",
            format!("arn:aws:iam::{account}:role/aws-ec2-spot-fleet"),
            false,
        ),
        (
            "a held role trusting another service only",
            another_principal,
            false,
        ),
        ("a held role whose grant is a Deny", denied, false),
        (
            "a held role trusting the fleet for another action",
            another_action,
            false,
        ),
    ];

    let mut served_fleets = 0usize;
    for (case, role, is_served) in rows {
        let before = fleet_count(&router).await;
        let (status, body) = ec2(&router, "RequestSpotFleet", &fleet_params(&role, &image)).await;
        if is_served {
            assert_eq!(
                status, 200,
                "case {case:?}: expected HTTP 200 from RequestSpotFleet, got {status}: {body}"
            );
            assert!(
                opt_element(&body, "spotFleetRequestId").is_some(),
                "case {case:?}: a served request must report a fleet id, got {body}"
            );
            served_fleets += 1;
        } else {
            assert_eq!(
                status, 400,
                "case {case:?}: expected HTTP 400 from RequestSpotFleet, got {status}: {body}"
            );
            let code = error_code(&body);
            assert_eq!(
                code, CONFIG_CODE,
                "case {case:?}: expected the code {CONFIG_CODE:?}, got {code:?} in {body}"
            );
            let message = error_message(&body);
            assert_eq!(
                message, "Parameter: SpotFleetRequestConfig.IamFleetRole is invalid.",
                "case {case:?}: expected the sentence naming the member, got {message:?}"
            );
        }
        let after = fleet_count(&router).await;
        let expected = before + usize::from(is_served);
        assert_eq!(
            after, expected,
            "case {case:?}: expected {expected} fleet(s) stored, got {after}"
        );
    }
    assert_eq!(
        served_fleets, 2,
        "the rows must include the requests a held, trusting role serves, got {served_fleets}"
    );
}
