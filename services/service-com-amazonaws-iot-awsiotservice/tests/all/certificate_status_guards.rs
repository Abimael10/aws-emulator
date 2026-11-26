// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of the two status moves `UpdateCertificate` REFUSES, and of
//! the moves it still makes.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a restJson1
//! request signed for the `iot` signing name — so what is asserted is what an AWS SDK
//! client would see, not an internal function's return value.
//!
//! The rule under test: a status move `UpdateCertificate` will not make is refused with the
//! class this operation models for a certificate operation it will not perform —
//! `CertificateStateException`, HTTP 406, "The certificate operation is not allowed"
//! (`smithy_query(query="operation",operation="UpdateCertificate")`) — and the certificate
//! keeps the status the store holds. Two moves are refused:
//!
//! * one TO a status AWS IoT keeps to itself — "Setting the status to PENDING_TRANSFER or
//!   PENDING_ACTIVATION will result in an exception being thrown. PENDING_TRANSFER and
//!   PENDING_ACTIVATION are statuses used internally by IoT"
//!   (`smithy_query(query="doc",shape="UpdateCertificateRequest",member="newStatus")`) —
//!   stated after the status the REQUEST named;
//! * one OUT OF a status a certificate cannot leave, which revocation is: a withdrawn
//!   device credential is never put back into service, so every update that would MOVE a
//!   revoked certificate is refused, stated after the status the STORE holds, while a call
//!   naming the status it already carries moves it nowhere and is served — "This operation
//!   is idempotent" — writing nothing.
//!
//! Every other move this operation admits is still made, so the guards are about the two
//! status sets and not about the operation.
//!
//! The tables span what the rule generalises over rather than one recorded request: each
//! internal status as the target, from every status a certificate can be in when the call
//! arrives; each status the modelled `CertificateStatus` enum carries as the target of an
//! update of a revoked certificate, `REVOKED` itself included — which is the row that
//! separates a refused MOVE from an idempotent repeat; and the revoked state reached by
//! several different paths, so what decides the refusal is the status the store holds
//! rather than the sequence of calls that reached it.

use std::sync::Arc;

use serde_json::Value;
use service_com_amazonaws_iot_awsiotservice::state::{
    CERTIFICATE_STATUS_ACTIVE, CERTIFICATE_STATUS_INACTIVE, CERTIFICATE_STATUS_REVOKED,
    INTERNAL_CERTIFICATE_STATUSES, TERMINAL_CERTIFICATE_STATUSES,
};
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region the harness signs for. None of these refusals is regional, so no assertion
/// depends on it.
const REGION: &str = "eu-central-1";

/// The `x-amzn-errortype` a refused certificate operation carries.
const CERTIFICATE_STATE: &str = "CertificateStateException";

/// The deprecated status the modelled `CertificateStatus` enum still carries: "The status
/// value REGISTER_INACTIVE is deprecated and should not be used"
/// (`UpdateCertificate$newStatus`) — deprecated is not refused, so it is one of the statuses
/// a caller may still ask for.
const CERTIFICATE_STATUS_REGISTER_INACTIVE: &str = "REGISTER_INACTIVE";

/// A SigV4 `Authorization` header whose credential scope names the `iot` signing name and
/// the region. The signature itself is never checked here — the harness turns request
/// authentication off, exactly as an unauthenticated emulator runs.
fn signed_for_iot() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDTESTIOT/20260101/{REGION}/iot/aws4_request, \
         SignedHeaders=host;x-amz-date, Signature=0000"
    )
}

/// One response, reduced to what a client branches on.
#[derive(Debug)]
struct Reply {
    status: u16,
    error_code: Option<String>,
    body: Value,
}

impl Reply {
    /// The `message` of an error body.
    fn message(&self) -> String {
        self.body
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }

    /// A string member of the reported `CertificateDescription`.
    fn member_str(&self, name: &str) -> Option<&str> {
        self.body
            .get("certificateDescription")
            .and_then(|value| value.get(name))
            .and_then(Value::as_str)
    }
}

/// The emulator under test: ONE shared engine with the IoT service mounted on it, reached
/// the way the production router reaches it.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(engine));
        Self { router }
    }

    async fn call(&self, method: &str, path: &str) -> Reply {
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://iot.{REGION}.amazonaws.com{path}"))
            .header("authorization", signed_for_iot())
            .header("content-type", "application/json")
            .body(bytes::Bytes::new())
            .expect("a well-formed request");

        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let error_code = response
            .headers()
            .get("x-amzn-errortype")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let bytes = response.into_body();
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        Reply {
            status,
            error_code,
            body,
        }
    }

    /// `CreateKeysAndCertificate`: `POST /keys-and-certificate`.
    async fn issue(&self, case: &str, set_as_active: bool) -> String {
        let issued = self
            .call(
                "POST",
                &format!("/keys-and-certificate?setAsActive={set_as_active}"),
            )
            .await;
        assert_eq!(
            issued.status, 200,
            "case {case:?}: CreateKeysAndCertificate must succeed, got {issued:?}"
        );
        issued
            .body
            .get("certificateId")
            .and_then(Value::as_str)
            .unwrap_or_else(|| {
                panic!("case {case:?}: the issue must report a certificateId, got {issued:?}")
            })
            .to_string()
    }

    /// `UpdateCertificate`: `PUT /certificates/{certificateId}`, with `newStatus` bound as
    /// an `@httpQuery` parameter.
    async fn update(&self, certificate_id: &str, new_status: &str) -> Reply {
        self.call(
            "PUT",
            &format!("/certificates/{certificate_id}?newStatus={new_status}"),
        )
        .await
    }

    /// `DescribeCertificate`: `GET /certificates/{certificateId}` — the reader that says
    /// which status a refused or accepted call left behind.
    async fn describe(&self, certificate_id: &str) -> Reply {
        self.call("GET", &format!("/certificates/{certificate_id}"))
            .await
    }

    /// The status the store holds for `certificate_id`, read back over the wire.
    async fn status_of(&self, case: &str, certificate_id: &str) -> String {
        let read = self.describe(certificate_id).await;
        read.member_str("status")
            .unwrap_or_else(|| {
                panic!("case {case:?}: DescribeCertificate must report a status, got {read:?}")
            })
            .to_string()
    }

    /// Take the certificate through `statuses`, asserting each hop is made: how a case
    /// REACHES the state it then tests.
    async fn walk(&self, case: &str, certificate_id: &str, statuses: &[&str]) {
        for status in statuses {
            let updated = self.update(certificate_id, status).await;
            assert_eq!(
                updated.status, 200,
                "case {case:?}: the move to {status} must succeed, got {updated:?}"
            );
            let reached = self.status_of(case, certificate_id).await;
            assert_eq!(
                reached, *status,
                "case {case:?}: expected the certificate at {status}, got {reached:?}"
            );
        }
    }
}

/// The statuses a CALLER may ask for: every value of the modelled `CertificateStatus` enum
/// (`smithy_query(query="shape",shape="CertificateStatus")`) less the two the same model
/// documents as internal to AWS IoT.
fn settable_statuses() -> Vec<&'static str> {
    vec![
        CERTIFICATE_STATUS_ACTIVE,
        CERTIFICATE_STATUS_INACTIVE,
        CERTIFICATE_STATUS_REVOKED,
        CERTIFICATE_STATUS_REGISTER_INACTIVE,
    ]
}

/// The sentence a refusal of a move TO a status no caller may set states.
fn unsettable_target_message(new_status: &str) -> String {
    format!("Not allowed to update to {new_status} status using UpdateCertificate call")
}

/// The sentence a refusal of a move OUT OF a status a certificate cannot leave states.
fn unmodifiable_current_message(current_status: &str) -> String {
    format!("Not allowed to update from {current_status} status using UpdateCertificate call")
}

/// RULE: a `newStatus` AWS IoT uses internally is refused `CertificateStateException` (406)
/// naming the status the request asked for, from EVERY status a certificate can be in when
/// the call arrives, and the certificate keeps the status it had.
#[tokio::test]
async fn a_status_only_the_service_sets_is_refused_whatever_the_certificate_holds() {
    // The rows are the states a caller can have the certificate in when the refused call
    // arrives: both statuses a create can leave it in, the deprecated one it can be moved
    // to, and the revoked one — paired with each internal status as the target.
    let starting_points: Vec<(&str, bool, Vec<&str>)> = vec![
        ("issued active", true, vec![]),
        ("issued inactive", false, vec![]),
        (
            "deactivated after being issued active",
            true,
            vec![CERTIFICATE_STATUS_INACTIVE],
        ),
        (
            "moved to the deprecated status",
            false,
            vec![CERTIFICATE_STATUS_REGISTER_INACTIVE],
        ),
        (
            "revoked",
            true,
            vec![CERTIFICATE_STATUS_INACTIVE, CERTIFICATE_STATUS_REVOKED],
        ),
    ];

    for (label, set_as_active, walk) in starting_points {
        for new_status in INTERNAL_CERTIFICATE_STATUSES {
            let case = format!("{label} -> {new_status}");
            let emulator = Emulator::new();
            let id = emulator.issue(&case, set_as_active).await;
            emulator.walk(&case, &id, &walk).await;
            let before = emulator.describe(&id).await;

            let refused = emulator.update(&id, new_status).await;
            assert_eq!(
                refused.status, 406,
                "case {case:?}: a status used internally must be refused 406, got {refused:?}"
            );
            assert_eq!(
                refused.error_code.as_deref(),
                Some(CERTIFICATE_STATE),
                "case {case:?}: expected {CERTIFICATE_STATE}, got {refused:?}"
            );
            let expected = unsettable_target_message(new_status);
            assert_eq!(
                refused.message(),
                expected,
                "case {case:?}: expected {expected:?}, got {refused:?}"
            );

            // The refusal is about the status the REQUEST named, so the record — the status
            // it holds included — is exactly the one the refused call found.
            let after = emulator.describe(&id).await;
            assert_eq!(
                after.body, before.body,
                "case {case:?}: a refused update must change nothing, got {after:?}"
            );
        }
    }
}

/// RULE: a certificate in a status it cannot leave refuses every update that would MOVE it,
/// naming the status the store holds, and keeps that status — whichever path took it there —
/// while a call naming the status it already carries is served and writes nothing.
#[tokio::test]
async fn a_certificate_in_a_terminal_status_refuses_every_update_that_would_move_it() {
    // The rows reach the terminal status by different paths, so what decides the refusal is
    // the status the store holds rather than the calls that reached it.
    let paths: Vec<(&str, bool, Vec<&str>)> = vec![
        (
            "revoked straight from the active status it was issued in",
            true,
            vec![CERTIFICATE_STATUS_REVOKED],
        ),
        (
            "revoked after being deactivated",
            true,
            vec![CERTIFICATE_STATUS_INACTIVE, CERTIFICATE_STATUS_REVOKED],
        ),
        (
            "revoked from the inactive status it was issued in",
            false,
            vec![CERTIFICATE_STATUS_REVOKED],
        ),
        (
            "revoked after a round trip through the deprecated status",
            false,
            vec![
                CERTIFICATE_STATUS_REGISTER_INACTIVE,
                CERTIFICATE_STATUS_ACTIVE,
                CERTIFICATE_STATUS_REVOKED,
            ],
        ),
    ];

    for (label, set_as_active, walk) in paths {
        // Every status a caller may ask for, the certificate's own included: the row that
        // names the status it already carries is the one the documented idempotence serves,
        // and every other one is a move the terminal status refuses.
        for new_status in settable_statuses() {
            let case = format!("{label}, then -> {new_status}");
            let emulator = Emulator::new();
            let id = emulator.issue(&case, set_as_active).await;
            emulator.walk(&case, &id, &walk).await;
            let before = emulator.describe(&id).await;
            let held = before.member_str("status").unwrap_or_else(|| {
                panic!("case {case:?}: DescribeCertificate must report a status, got {before:?}")
            });
            assert!(
                TERMINAL_CERTIFICATE_STATUSES.contains(&held),
                "case {case:?}: the path must leave the certificate in a terminal status, \
                 got {held:?}"
            );
            let moves = new_status != held;
            let expected = unmodifiable_current_message(held);

            let answered = emulator.update(&id, new_status).await;
            if moves {
                assert_eq!(
                    answered.status, 406,
                    "case {case:?}: a move out of a terminal status must be refused 406, \
                     got {answered:?}"
                );
                assert_eq!(
                    answered.error_code.as_deref(),
                    Some(CERTIFICATE_STATE),
                    "case {case:?}: expected {CERTIFICATE_STATE}, got {answered:?}"
                );
                assert_eq!(
                    answered.message(),
                    expected,
                    "case {case:?}: expected {expected:?}, got {answered:?}"
                );
            } else {
                // The repeat moves the certificate nowhere, so it is served and the record
                // is the one the revoking call left.
                assert_eq!(
                    answered.status, 200,
                    "case {case:?}: naming the status the certificate already carries must \
                     be served, got {answered:?}"
                );
            }

            // The credential stays withdrawn either way: the call left the whole record as
            // it found it, so no later reader reports it as usable again.
            let after = emulator.describe(&id).await;
            assert_eq!(
                after.body, before.body,
                "case {case:?}: the call must change nothing, got {after:?}"
            );
            assert_eq!(
                after.member_str("status"),
                Some(CERTIFICATE_STATUS_REVOKED),
                "case {case:?}: the certificate must still be revoked, got {after:?}"
            );
        }
    }
}

/// RULE: the guards are about those two status sets alone — every other move this operation
/// admits is still made, in both directions, however often it is repeated.
#[tokio::test]
async fn every_other_move_is_still_made_in_both_directions() {
    // Each row is a pair of statuses neither of which is terminal: the certificate goes
    // there, comes back, and goes there again, so a guard that refused one of these moves
    // would be caught whichever direction it refused.
    let pairs = [
        (CERTIFICATE_STATUS_ACTIVE, CERTIFICATE_STATUS_INACTIVE),
        (
            CERTIFICATE_STATUS_ACTIVE,
            CERTIFICATE_STATUS_REGISTER_INACTIVE,
        ),
        (
            CERTIFICATE_STATUS_INACTIVE,
            CERTIFICATE_STATUS_REGISTER_INACTIVE,
        ),
    ];

    for (first, second) in pairs {
        let case = format!("{first} <-> {second}");
        let emulator = Emulator::new();
        let id = emulator.issue(&case, true).await;
        emulator
            .walk(&case, &id, &[first, second, first, second, second])
            .await;

        // And the move INTO the terminal status is itself one the operation makes: the
        // guard is what the status refuses afterwards, not the revocation.
        emulator
            .walk(&case, &id, &[CERTIFICATE_STATUS_REVOKED])
            .await;
        let revoked = emulator.status_of(&case, &id).await;
        assert_eq!(
            revoked, CERTIFICATE_STATUS_REVOKED,
            "case {case:?}: the revocation itself must be recorded, got {revoked:?}"
        );
    }
}
