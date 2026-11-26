// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the `/responder-gateway/{gatewayId}/certificate` surface:
//! `AssociateCertificate`, `GetCertificateAssociation` and `DisassociateCertificate`,
//! driven over the wire through the router the production emulator uses.
//!
//! The rules under test are about state, not about one recorded request:
//!
//! * a certificate is associated only with a responder gateway whose stored
//!   `gatewayType` is `EXTERNAL` — the type that terminates traffic for callers
//!   outside the fabric — and any other gateway is refused with the service's fixed
//!   wording, storing nothing;
//! * an association is a record addressed by the (gateway, certificate) pair, so
//!   the read reports exactly what the write left, a repeat of the association
//!   answers that one record rather than making a second, the clock settles each
//!   pending status to its terminal one, and the record dies with its gateway;
//! * a record whose removal this service has already accepted still holds the
//!   (gateway, certificate) pair until the clock completes that removal, so a call
//!   asking to associate the certificate again is refused with the modelled
//!   `ConflictException` (409) and changes nothing, and the accepted removal still
//!   completes;
//! * every refusal for a gateway or a certificate this store does not hold is the
//!   service's one not-found: HTTP 404 with fixed text naming nothing from the
//!   request;
//! * `clientToken` is an `@idempotencyToken`, so a token already presented to
//!   `AssociateCertificate` and carrying DIFFERENT parameters is refused with the
//!   service's `ValidationException` (400) and stores nothing, while the same token
//!   restating the same parameters is answered as the first call was.
//!
//! The tables span what the rules generalise over: every member of the modelled
//! `GatewayType` enum plus a create that names none, and certificate ARNs spanning
//! each partition the modelled `AcmCertificateArn` pattern admits
//! (`smithy_query(query="shape", shape="AcmCertificateArn,GatewayType")`).

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use serde_json::{json, Value};
use service_com_amazonaws_rtbfabric_rtbfabric::ops::types::GATEWAY_TYPE_VALUES;
use service_com_amazonaws_rtbfabric_rtbfabric::state::{
    CERTIFICATE_EXTERNAL_ONLY_MESSAGE, CERTIFICATE_STATUS_ASSOCIATED,
    CERTIFICATE_STATUS_DISASSOCIATED, CERTIFICATE_STATUS_PENDING_ASSOCIATION,
    CERTIFICATE_STATUS_PENDING_DISASSOCIATION, RESOURCE_NOT_FOUND_MESSAGE,
};
use service_com_amazonaws_rtbfabric_rtbfabric::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region the test signs for.
const REGION: &str = "us-west-2";

/// How long a wait for a clock-owed transition is given before the test gives up.
/// Generous on purpose: the test waits for the CONDITION, and only a host that never
/// settles the record at all reaches this.
const SETTLE_DEADLINE: Duration = Duration::from_secs(60);

/// How long the poll loop pauses between two reads of the same record.
const POLL_PAUSE: Duration = Duration::from_millis(20);

/// How many times the removal-then-association pair is driven on a fresh record
/// before the test gives up.
///
/// The two in-process calls land inside the span the clock owes the accepted removal,
/// so the refusal is what an attempt observes; an attempt whose removal had already
/// settled by the time the second call ran saw no outstanding removal at all, which
/// proves nothing either way, so it is made again on a fresh record rather than
/// asserted on.
const PENDING_REMOVAL_ATTEMPTS: usize = 8;

/// ACM certificate ARNs spanning every partition the modelled `AcmCertificateArn`
/// pattern admits (`^arn:(aws|aws-cn|aws-us-gov):acm:…`), each inside the member's
/// published length bounds (75..256).
const CERTIFICATE_ARNS: &[(&str, &str)] = &[
    (
        "the aws partition",
        "arn:aws:acm:us-west-2:123456789012:certificate/0123abcd-4567-89ef-0123-456789abcdef",
    ),
    (
        "the aws-cn partition",
        "arn:aws-cn:acm:cn-north-1:210987654321:certificate/fedcba98-7654-3210-fedc-ba9876543210",
    ),
    (
        "the aws-us-gov partition",
        "arn:aws-us-gov:acm:us-gov-west-1:111122223333:certificate/00000000-1111-2222-3333-444455556666",
    ),
];

fn text(body: &Value, member: &str) -> String {
    body[member].as_str().unwrap_or_default().to_string()
}

/// One answered call: the status, the wire error code (absent on a success) and the
/// parsed body.
struct Answer {
    status: u16,
    code: String,
    body: Value,
}

impl Answer {
    /// The `message` the error envelope states.
    fn message(&self) -> String {
        text(&self.body, "message")
    }
}

/// The one emulated service, behind the same `MultiServiceRouter` the production
/// router assembles, so these calls take the real deserialize -> validate -> handle
/// -> serialize path.
struct Fabric {
    router: MultiServiceRouter,
}

impl Fabric {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.register_service(EmulatedService::new(engine));
        Self { router }
    }

    /// Send one restJson1 request and return what came back.
    async fn send(&self, method: &str, path: &str, body: Option<Value>) -> Answer {
        let bytes = match body {
            Some(body) => Bytes::from(serde_json::to_vec(&body).expect("serialize the body")),
            None => Bytes::new(),
        };
        let request = ::http::Request::builder()
            .method(method)
            .uri(format!("https://rtbfabric.{REGION}.amazonaws.com{path}"))
            .header("content-type", "application/json")
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/rtbfabric/aws4_request, \
                     SignedHeaders=host, Signature=00"
                ),
            )
            .body(bytes)
            .expect("build the request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let code = response
            .headers()
            .get("x-amzn-errortype")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();
        let body = if response.body().is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(response.body()).unwrap_or(Value::Null)
        };
        Answer { status, code, body }
    }

    /// Create a responder gateway of `gateway_type` (`None` naming none at all) and
    /// return its minted id.
    async fn responder_gateway(&self, case: &str, token: &str, gateway_type: Option<&str>) -> String {
        let mut body = json!({
            "vpcId": "vpc-0123456789abcdef0",
            "subnetIds": ["subnet-0123456789abcdef0"],
            "securityGroupIds": ["sg-0123456789abcdef0"],
            "port": 443,
            "protocol": "HTTPS",
            "domainName": "responder.example.com",
            "clientToken": token,
        });
        if let Some(gateway_type) = gateway_type {
            body["gatewayType"] = json!(gateway_type);
        }
        let answered = self.send("POST", "/responder-gateway", Some(body)).await;
        assert_eq!(
            answered.status, 200,
            "case {case:?}: expected CreateResponderGateway to answer 200, got {} {:?}",
            answered.status, answered.body
        );
        text(&answered.body, "gatewayId")
    }

    /// Create a requester gateway and return its minted id.
    async fn requester_gateway(&self, case: &str, token: &str) -> String {
        let answered = self
            .send(
                "POST",
                "/requester-gateway",
                Some(json!({
                    "vpcId": "vpc-0123456789abcdef0",
                    "subnetIds": ["subnet-0123456789abcdef0"],
                    "securityGroupIds": ["sg-0123456789abcdef0"],
                    "clientToken": token,
                })),
            )
            .await;
        assert_eq!(
            answered.status, 200,
            "case {case:?}: expected CreateRequesterGateway to answer 200, got {} {:?}",
            answered.status, answered.body
        );
        text(&answered.body, "gatewayId")
    }

    async fn associate(&self, gateway_id: &str, arn: &str, token: &str) -> Answer {
        self.send(
            "POST",
            &format!("/responder-gateway/{gateway_id}/certificate"),
            Some(json!({ "acmCertificateArn": arn, "clientToken": token })),
        )
        .await
    }

    async fn get_association(&self, gateway_id: &str, arn: &str) -> Answer {
        self.send("GET", &certificate_path(gateway_id, arn), None).await
    }

    async fn disassociate(&self, gateway_id: &str, arn: &str) -> Answer {
        self.send("DELETE", &certificate_path(gateway_id, arn), None).await
    }

    /// Read the association until it reports `expected`, or a generous deadline
    /// passes: the transition is one the clock owes, so the test waits for the
    /// CONDITION rather than for a span of time.
    async fn settled(&self, case: &str, gateway_id: &str, arn: &str, expected: &str) -> Value {
        let give_up_at = Instant::now() + SETTLE_DEADLINE;
        loop {
            let answered = self.get_association(gateway_id, arn).await;
            assert_eq!(
                answered.status, 200,
                "case {case:?}: expected GetCertificateAssociation to answer 200, got {} {:?}",
                answered.status, answered.body
            );
            if text(&answered.body, "status") == expected {
                return answered.body;
            }
            assert!(
                Instant::now() < give_up_at,
                "case {case:?}: expected the association to settle to {expected:?}, got {:?}",
                answered.body["status"]
            );
            tokio::time::sleep(POLL_PAUSE).await;
        }
    }
}

/// The certificate surface's path for one certificate: the ARN travels in the
/// `acmCertificateArn` query parameter, encoded as an SDK encodes it.
fn certificate_path(gateway_id: &str, arn: &str) -> String {
    let encoded = utf8_percent_encode(arn, NON_ALPHANUMERIC).to_string();
    format!("/responder-gateway/{gateway_id}/certificate?acmCertificateArn={encoded}")
}

/// Assert `answered` is this service's one not-found: the modelled code, HTTP 404,
/// and fixed text that names nothing the request carried.
fn assert_not_found(case: &str, answered: &Answer, request_values: &[&str]) {
    assert_eq!(
        answered.status, 404,
        "case {case:?}: expected HTTP 404, got {} {:?}",
        answered.status, answered.body
    );
    assert_eq!(
        answered.code, "ResourceNotFoundException",
        "case {case:?}: expected ResourceNotFoundException, got {:?}",
        answered.code
    );
    assert_eq!(
        answered.message(),
        RESOURCE_NOT_FOUND_MESSAGE,
        "case {case:?}: expected {RESOURCE_NOT_FOUND_MESSAGE:?}, got {:?}",
        answered.message()
    );
    for value in request_values {
        assert!(
            !answered.message().contains(value),
            "case {case:?}: expected the refusal to name nothing from the request, got {:?}",
            answered.message()
        );
    }
}

#[tokio::test]
async fn a_certificate_associates_only_with_an_external_gateway() {
    // Every member of the modelled `GatewayType` enum, plus a create that names
    // none: the member is optional, so "no type at all" is a third input the rule
    // has to answer for.
    let types: Vec<Option<&str>> = GATEWAY_TYPE_VALUES
        .iter()
        .map(|value| Some(*value))
        .chain([None])
        .collect();

    for gateway_type in types {
        let case = gateway_type.unwrap_or("no gatewayType at all");
        let fabric = Fabric::new();
        let gateway_id = fabric.responder_gateway(case, "token-responder", gateway_type).await;
        let (_, arn) = CERTIFICATE_ARNS[0];

        let answered = fabric.associate(&gateway_id, arn, "token-associate").await;

        if gateway_type == Some("EXTERNAL") {
            assert_eq!(
                answered.status, 200,
                "case {case:?}: expected AssociateCertificate to answer 200, got {} {:?}",
                answered.status, answered.body
            );
            assert_eq!(
                text(&answered.body, "status"),
                CERTIFICATE_STATUS_PENDING_ASSOCIATION,
                "case {case:?}: expected a fresh association to be pending, got {:?}",
                answered.body["status"]
            );
            continue;
        }

        assert_eq!(
            answered.status, 400,
            "case {case:?}: expected AssociateCertificate to be refused with 400, got {} {:?}",
            answered.status, answered.body
        );
        assert_eq!(
            answered.code, "ValidationException",
            "case {case:?}: expected ValidationException, got {:?}",
            answered.code
        );
        assert_eq!(
            answered.message(),
            CERTIFICATE_EXTERNAL_ONLY_MESSAGE,
            "case {case:?}: expected {CERTIFICATE_EXTERNAL_ONLY_MESSAGE:?}, got {:?}",
            answered.message()
        );
        // The refused call stored nothing: the certificate is not on the gateway.
        let read_back = fabric.get_association(&gateway_id, arn).await;
        assert_not_found(case, &read_back, &[&gateway_id, arn]);
    }
}

#[tokio::test]
async fn an_association_settles_round_trips_and_disassociates_for_every_partition() {
    for (case, arn) in CERTIFICATE_ARNS {
        let fabric = Fabric::new();
        let gateway_id = fabric
            .responder_gateway(case, "token-responder", Some("EXTERNAL"))
            .await;

        let associated = fabric.associate(&gateway_id, arn, "token-associate").await;
        assert_eq!(
            associated.status, 200,
            "case {case:?}: expected AssociateCertificate to answer 200, got {} {:?}",
            associated.status, associated.body
        );
        for (member, expected) in [("gatewayId", gateway_id.as_str()), ("acmCertificateArn", arn)] {
            assert_eq!(
                text(&associated.body, member),
                expected,
                "case {case:?}: expected {member} {expected:?}, got {:?}",
                associated.body[member]
            );
        }
        assert_eq!(
            text(&associated.body, "status"),
            CERTIFICATE_STATUS_PENDING_ASSOCIATION,
            "case {case:?}: expected a fresh association to be pending, got {:?}",
            associated.body["status"]
        );

        // The read reports the record the write left, including the transient
        // status, and then the status the clock makes due.
        let pending = fabric.get_association(&gateway_id, arn).await;
        assert_eq!(
            text(&pending.body, "status"),
            CERTIFICATE_STATUS_PENDING_ASSOCIATION,
            "case {case:?}: expected the pending status to be readable, got {:?}",
            pending.body["status"]
        );
        let settled = fabric
            .settled(case, &gateway_id, arn, CERTIFICATE_STATUS_ASSOCIATED)
            .await;
        assert_eq!(
            settled["associatedAt"], pending.body["associatedAt"],
            "case {case:?}: expected associatedAt to stay the instant the association was asked for, got {:?}",
            settled["associatedAt"]
        );

        // A repeat of the association — a fresh client token and all — asks for the
        // record that is already there rather than a second one.
        let repeat = fabric.associate(&gateway_id, arn, "token-associate-again").await;
        assert_eq!(
            repeat.status, 200,
            "case {case:?}: expected a repeated AssociateCertificate to answer 200, got {} {:?}",
            repeat.status, repeat.body
        );
        assert_eq!(
            text(&repeat.body, "status"),
            CERTIFICATE_STATUS_ASSOCIATED,
            "case {case:?}: expected the repeat to answer the stored association, got {:?}",
            repeat.body["status"]
        );
        let unchanged = fabric.get_association(&gateway_id, arn).await;
        assert_eq!(
            unchanged.body["associatedAt"], settled["associatedAt"],
            "case {case:?}: expected the repeat to leave the record alone, got {:?}",
            unchanged.body["associatedAt"]
        );

        // Removal is asynchronous and terminal, and a repeat of it asks for the
        // state the association is already in.
        let removing = fabric.disassociate(&gateway_id, arn).await;
        assert_eq!(
            removing.status, 200,
            "case {case:?}: expected DisassociateCertificate to answer 200, got {} {:?}",
            removing.status, removing.body
        );
        assert_eq!(
            text(&removing.body, "status"),
            CERTIFICATE_STATUS_PENDING_DISASSOCIATION,
            "case {case:?}: expected the removal to be pending, got {:?}",
            removing.body["status"]
        );
        fabric
            .settled(case, &gateway_id, arn, CERTIFICATE_STATUS_DISASSOCIATED)
            .await;
        let removed_again = fabric.disassociate(&gateway_id, arn).await;
        assert_eq!(
            text(&removed_again.body, "status"),
            CERTIFICATE_STATUS_DISASSOCIATED,
            "case {case:?}: expected a repeated removal to answer the terminal status, got {:?}",
            removed_again.body["status"]
        );

        // A certificate that was removed can be associated again: the pair names one
        // record, and this is a fresh association of it.
        let again = fabric.associate(&gateway_id, arn, "token-associate-third").await;
        assert_eq!(
            text(&again.body, "status"),
            CERTIFICATE_STATUS_PENDING_ASSOCIATION,
            "case {case:?}: expected a removed certificate to associate afresh, got {:?}",
            again.body["status"]
        );
    }
}

#[tokio::test]
async fn two_certificates_on_one_gateway_are_two_records() {
    let fabric = Fabric::new();
    let case = "two certificates on one gateway";
    let gateway_id = fabric
        .responder_gateway(case, "token-responder", Some("EXTERNAL"))
        .await;
    let (_, first) = CERTIFICATE_ARNS[0];
    let (_, second) = CERTIFICATE_ARNS[1];

    for (index, arn) in [first, second].iter().enumerate() {
        let answered = fabric
            .associate(&gateway_id, arn, &format!("token-associate-{index}"))
            .await;
        assert_eq!(
            answered.status, 200,
            "case {case:?}: expected AssociateCertificate for {arn:?} to answer 200, got {} {:?}",
            answered.status, answered.body
        );
    }
    fabric
        .settled(case, &gateway_id, first, CERTIFICATE_STATUS_ASSOCIATED)
        .await;
    fabric
        .settled(case, &gateway_id, second, CERTIFICATE_STATUS_ASSOCIATED)
        .await;

    // Removing one leaves the other exactly as it was.
    fabric.disassociate(&gateway_id, first).await;
    fabric
        .settled(case, &gateway_id, first, CERTIFICATE_STATUS_DISASSOCIATED)
        .await;
    let kept = fabric.get_association(&gateway_id, second).await;
    assert_eq!(
        text(&kept.body, "status"),
        CERTIFICATE_STATUS_ASSOCIATED,
        "case {case:?}: expected the other certificate to be untouched, got {:?}",
        kept.body["status"]
    );
}

#[tokio::test]
async fn the_certificate_surface_answers_not_found_for_anything_the_store_does_not_hold() {
    let fabric = Fabric::new();
    let external = fabric
        .responder_gateway("the stored gateway", "token-responder", Some("EXTERNAL"))
        .await;
    let requester = fabric.requester_gateway("the requester gateway", "token-requester").await;
    let (_, arn) = CERTIFICATE_ARNS[0];
    let (_, other_arn) = CERTIFICATE_ARNS[1];
    fabric.associate(&external, arn, "token-associate").await;

    // (case, the gateway addressed, the certificate addressed)
    for (case, gateway_id, certificate) in [
        ("a gateway id nothing minted", "rtb-gw-0123456789abcdef", arn),
        ("a requester gateway, which has no certificate surface", requester.as_str(), arn),
        (
            "a stored gateway and a certificate it never carried",
            external.as_str(),
            other_arn,
        ),
    ] {
        let read = fabric.get_association(gateway_id, certificate).await;
        assert_not_found(case, &read, &[gateway_id, certificate]);
        let removed = fabric.disassociate(gateway_id, certificate).await;
        assert_not_found(case, &removed, &[gateway_id, certificate]);
    }

    // AssociateCertificate on a gateway nothing minted is the same not-found; on the
    // requester gateway it is too, since the surface addresses responder gateways.
    for (case, gateway_id) in [
        ("associating on a gateway id nothing minted", "rtb-gw-0123456789abcdef"),
        ("associating on a requester gateway", requester.as_str()),
    ] {
        let answered = fabric.associate(gateway_id, other_arn, "token-associate-unknown").await;
        assert_not_found(case, &answered, &[gateway_id, other_arn]);
    }
}

#[tokio::test]
async fn an_association_dies_with_the_gateway_it_is_on() {
    let fabric = Fabric::new();
    let case = "the gateway carrying the certificate is deleted";
    let gateway_id = fabric
        .responder_gateway(case, "token-responder", Some("EXTERNAL"))
        .await;
    let (_, arn) = CERTIFICATE_ARNS[0];
    fabric.associate(&gateway_id, arn, "token-associate").await;
    fabric
        .settled(case, &gateway_id, arn, CERTIFICATE_STATUS_ASSOCIATED)
        .await;

    let deleted = fabric
        .send("DELETE", &format!("/responder-gateway/{gateway_id}"), None)
        .await;
    assert_eq!(
        deleted.status, 200,
        "case {case:?}: expected DeleteResponderGateway to answer 200, got {} {:?}",
        deleted.status, deleted.body
    );
    assert_eq!(
        text(&deleted.body, "status"),
        "DELETED",
        "case {case:?}: expected the released gateway to report DELETED, got {:?}",
        deleted.body["status"]
    );

    // Nothing the store held because of that gateway survives it.
    let association = fabric.get_association(&gateway_id, arn).await;
    assert_not_found(case, &association, &[&gateway_id, arn]);
    let gateway = fabric
        .send("GET", &format!("/responder-gateway/{gateway_id}"), None)
        .await;
    assert_not_found(case, &gateway, &[&gateway_id]);
    let listed = fabric.send("GET", "/responder-gateways", None).await;
    assert_eq!(
        listed.body["gatewayIds"],
        json!([]),
        "case {case:?}: expected the listing to report no responder gateway, got {:?}",
        listed.body["gatewayIds"]
    );
}

#[tokio::test]
async fn associating_again_is_refused_while_an_accepted_removal_is_outstanding() {
    // The rule is about the record's status, not about one certificate, so it is
    // driven for a certificate of every partition the ARN pattern admits.
    for (case, arn) in CERTIFICATE_ARNS {
        let mut observed = false;
        for _ in 0..PENDING_REMOVAL_ATTEMPTS {
            let fabric = Fabric::new();
            let gateway_id = fabric
                .responder_gateway(case, "token-responder", Some("EXTERNAL"))
                .await;
            let associated = fabric.associate(&gateway_id, arn, "token-associate").await;
            assert_eq!(
                associated.status, 200,
                "case {case:?}: expected AssociateCertificate to answer 200, got {} {:?}",
                associated.status, associated.body
            );
            let before = fabric.get_association(&gateway_id, arn).await;

            let removing = fabric.disassociate(&gateway_id, arn).await;
            assert_eq!(
                removing.status, 200,
                "case {case:?}: expected DisassociateCertificate to answer 200, got {} {:?}",
                removing.status, removing.body
            );
            assert_eq!(
                text(&removing.body, "status"),
                CERTIFICATE_STATUS_PENDING_DISASSOCIATION,
                "case {case:?}: expected the removal to be accepted as pending, got {:?}",
                removing.body["status"]
            );

            // The removal is accepted and the clock has not completed it, so this
            // call asks for a transition the record's status does not admit.
            let answered = fabric
                .associate(&gateway_id, arn, "token-associate-again")
                .await;
            if answered.status == 200 {
                // The removal settled before this call reached the handler, so
                // there was no outstanding removal for it to conflict with: this
                // attempt observed nothing about the rule.
                continue;
            }
            observed = true;
            assert_eq!(
                answered.status, 409,
                "case {case:?}: expected HTTP 409, got {} {:?}",
                answered.status, answered.body
            );
            assert_eq!(
                answered.code, "ConflictException",
                "case {case:?}: expected ConflictException, got {:?}",
                answered.code
            );
            let expected_message = format!(
                "certificate {arn} on responder gateway {gateway_id} is \
                 {CERTIFICATE_STATUS_PENDING_DISASSOCIATION} and cannot be associated"
            );
            assert_eq!(
                answered.message(),
                expected_message,
                "case {case:?}: expected {expected_message:?}, got {:?}",
                answered.message()
            );

            // The refused call wrote nothing: the record still carries the removal
            // this service accepted — or the terminal status the clock has since
            // made due — and the instant the association was asked for has not
            // moved.
            let during = fabric.get_association(&gateway_id, arn).await;
            let status = text(&during.body, "status");
            assert!(
                status == CERTIFICATE_STATUS_PENDING_DISASSOCIATION
                    || status == CERTIFICATE_STATUS_DISASSOCIATED,
                "case {case:?}: expected the accepted removal to stand, got {status:?}"
            );
            assert_eq!(
                during.body["associatedAt"], before.body["associatedAt"],
                "case {case:?}: expected the refused call to leave the record alone, got {:?}",
                during.body["associatedAt"]
            );

            // And the removal the caller was told was pending still completes.
            fabric
                .settled(case, &gateway_id, arn, CERTIFICATE_STATUS_DISASSOCIATED)
                .await;
            // Once it has, the pair takes a fresh association: the refusal belongs
            // to the outstanding removal, not to the certificate.
            let again = fabric
                .associate(&gateway_id, arn, "token-associate-third")
                .await;
            assert_eq!(
                again.status, 200,
                "case {case:?}: expected a settled removal to admit a fresh association, got {} {:?}",
                again.status, again.body
            );
            assert_eq!(
                text(&again.body, "status"),
                CERTIFICATE_STATUS_PENDING_ASSOCIATION,
                "case {case:?}: expected the fresh association to be pending, got {:?}",
                again.body["status"]
            );
            break;
        }
        assert!(
            observed,
            "case {case:?}: expected AssociateCertificate to be refused while the accepted \
             removal was outstanding, but all {PENDING_REMOVAL_ATTEMPTS} attempts answered 200"
        );
    }
}

#[tokio::test]
async fn what_a_repeated_association_does_follows_the_record_status() {
    // (case, wait for the association to be in place first, remove it and let the
    //  removal settle first) — the statuses of the stored record that DO admit the
    //  repeat, and what each one answers it with.
    for (case, settle_first, remove_first) in [
        ("the association is still settling in", false, false),
        ("the association is in place", true, false),
        ("the association was removed and the removal settled", true, true),
    ] {
        let fabric = Fabric::new();
        let gateway_id = fabric
            .responder_gateway(case, "token-responder", Some("EXTERNAL"))
            .await;
        let (_, arn) = CERTIFICATE_ARNS[2];
        let associated = fabric.associate(&gateway_id, arn, "token-associate").await;
        assert_eq!(
            associated.status, 200,
            "case {case:?}: expected AssociateCertificate to answer 200, got {} {:?}",
            associated.status, associated.body
        );
        if settle_first {
            fabric
                .settled(case, &gateway_id, arn, CERTIFICATE_STATUS_ASSOCIATED)
                .await;
        }
        if remove_first {
            fabric.disassociate(&gateway_id, arn).await;
            fabric
                .settled(case, &gateway_id, arn, CERTIFICATE_STATUS_DISASSOCIATED)
                .await;
        }
        let before = fabric.get_association(&gateway_id, arn).await;

        // A fresh client token and all: the (gateway, certificate) pair IS the
        // record's identity.
        let repeat = fabric
            .associate(&gateway_id, arn, "token-associate-again")
            .await;
        assert_eq!(
            repeat.status, 200,
            "case {case:?}: expected a repeated AssociateCertificate to answer 200, got {} {:?}",
            repeat.status, repeat.body
        );
        let after = fabric.get_association(&gateway_id, arn).await;

        if remove_first {
            // Nothing is outstanding on a record that has reached the terminal
            // status, so this is an association of its own.
            assert_eq!(
                text(&repeat.body, "status"),
                CERTIFICATE_STATUS_PENDING_ASSOCIATION,
                "case {case:?}: expected a removed certificate to associate afresh, got {:?}",
                repeat.body["status"]
            );
            assert_ne!(
                after.body["associatedAt"], before.body["associatedAt"],
                "case {case:?}: expected the fresh association to date from this call, got {:?}",
                after.body["associatedAt"]
            );
            continue;
        }

        // The association the caller asks for is the one already in force, so the
        // record is answered as it stands and is left exactly as it was.
        let status = text(&repeat.body, "status");
        assert!(
            status == CERTIFICATE_STATUS_PENDING_ASSOCIATION
                || status == CERTIFICATE_STATUS_ASSOCIATED,
            "case {case:?}: expected the repeat to answer the association in force, got {status:?}"
        );
        assert_eq!(
            after.body["associatedAt"], before.body["associatedAt"],
            "case {case:?}: expected the repeat to leave the record alone, got {:?}",
            after.body["associatedAt"]
        );
    }
}

#[tokio::test]
async fn one_client_token_carrying_a_different_certificate_is_refused() {
    // One token is presented twice in every row; what differs between the two calls
    // is the certificate, spanning the partitions the modelled `AcmCertificateArn`
    // pattern admits — beside the gateway the call addresses, the one parameter this
    // operation takes.
    const TOKEN: &str = "token-associate-reused";
    for (first, second) in [(0usize, 1usize), (1, 2), (2, 0)] {
        let (first_case, first_arn) = CERTIFICATE_ARNS[first];
        let (second_case, second_arn) = CERTIFICATE_ARNS[second];
        let case = format!("{first_case} then {second_case} under one token");
        let case = case.as_str();
        let fabric = Fabric::new();
        let gateway_id = fabric
            .responder_gateway(case, "token-responder", Some("EXTERNAL"))
            .await;

        let associated = fabric.associate(&gateway_id, first_arn, TOKEN).await;
        assert_eq!(
            associated.status, 200,
            "case {case:?}: expected AssociateCertificate to answer 200, got {} {:?}",
            associated.status, associated.body
        );
        let stored = fabric.get_association(&gateway_id, first_arn).await;

        // The same token restating the same parameters is the retry a client token
        // is there for: it is answered with the record the first call left.
        let replay = fabric.associate(&gateway_id, first_arn, TOKEN).await;
        assert_eq!(
            replay.status, 200,
            "case {case:?}: expected the retry under the same token to answer 200, got {} {:?}",
            replay.status, replay.body
        );
        assert_eq!(
            text(&replay.body, "acmCertificateArn"),
            first_arn,
            "case {case:?}: expected the retry to answer for {first_arn:?}, got {:?}",
            replay.body["acmCertificateArn"]
        );

        // The same token carrying a DIFFERENT certificate is refused, rather than
        // associating a second certificate under a token already spent. The service
        // models no `IdempotentParameterMismatch`, so the refusal is its
        // `ValidationException` (400).
        let refused = fabric.associate(&gateway_id, second_arn, TOKEN).await;
        assert_eq!(
            refused.status, 400,
            "case {case:?}: expected the reused token to be refused with 400, got {} {:?}",
            refused.status, refused.body
        );
        assert_eq!(
            refused.code, "ValidationException",
            "case {case:?}: expected ValidationException, got {:?}",
            refused.code
        );
        let expected_message = format!(
            "clientToken {TOKEN} was already used for a AssociateCertificate request \
             with different parameters"
        );
        assert_eq!(
            refused.message(),
            expected_message,
            "case {case:?}: expected {expected_message:?}, got {:?}",
            refused.message()
        );

        // The refused call stored nothing for the certificate it carried, and left
        // the association the token WAS presented for exactly as it was.
        let unstored = fabric.get_association(&gateway_id, second_arn).await;
        assert_not_found(case, &unstored, &[&gateway_id, second_arn]);
        let unchanged = fabric.get_association(&gateway_id, first_arn).await;
        assert_eq!(
            unchanged.body["associatedAt"], stored.body["associatedAt"],
            "case {case:?}: expected the refused call to leave the first record alone, got {:?}",
            unchanged.body["associatedAt"]
        );

        // The refusal belongs to the spent token, not to the certificate: under a
        // token of its own the second certificate associates.
        let accepted = fabric
            .associate(&gateway_id, second_arn, "token-associate-second")
            .await;
        assert_eq!(
            accepted.status, 200,
            "case {case:?}: expected a token of its own to associate {second_arn:?}, got {} {:?}",
            accepted.status, accepted.body
        );
        assert_eq!(
            text(&accepted.body, "status"),
            CERTIFICATE_STATUS_PENDING_ASSOCIATION,
            "case {case:?}: expected the fresh association to be pending, got {:?}",
            accepted.body["status"]
        );
    }
}
