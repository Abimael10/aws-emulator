// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of a device certificate's TRANSFER to another Amazon Web
//! Services account: `TransferCertificate`, the `CancelCertificateTransfer` that takes a
//! pending transfer back, the `ListOutgoingCertificates` view of the transfers still
//! pending, and the `DescribeCertificate` reader that proves what each of them persisted.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a restJson1
//! request signed for the `iot` signing name — so what is asserted is what an AWS SDK
//! client would see, not an internal function's return value.
//!
//! The rule under test: a transfer is a STATE the source account's certificate record
//! enters rather than an event, so starting one records the account the certificate is on
//! its way to and the instant it started, moves the certificate to `PENDING_TRANSFER`, and
//! answers the ARN of that record; the two preconditions the operation documents — the
//! certificate "must not be in the ACTIVE state" and "must not have any policies attached
//! to it" — are read from the account's own state and refused with the class the model
//! declares for each, leaving the record exactly as it was; and cancelling a pending
//! transfer is the reverse write, after which "the status of the certificate changes from
//! PENDING_TRANSFER to INACTIVE" and no transfer is left to describe, while a certificate
//! in no pending transfer has none to cancel and is refused `ResourceNotFoundException`
//! (404) naming the id the request carried, exactly as an id the account holds no
//! certificate under is.
//!
//! The tables span what that rule generalises over rather than one request: every form
//! `transferMessage` takes on the wire (absent, empty, at its modelled 128-character
//! maximum and one character past it), several target accounts across the modelled
//! `^[0-9]+$` 12-digit form, each status a transferable certificate can be in, the region
//! the call is signed for, a second certificate the case never transfers, a transfer
//! started again after a cancellation, and both directions and every page size the
//! outgoing listing takes.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_iot_awsiotservice::state::{
    CERTIFICATE_STATUS_ACTIVE, CERTIFICATE_STATUS_INACTIVE, CERTIFICATE_STATUS_PENDING_TRANSFER,
    CERTIFICATE_STATUS_REVOKED,
};
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The account id every emulated ARN carries (`emulator_server::EMULATOR_ACCOUNT_ID`).
const ACCOUNT_ID: &str = "000000000000";
/// The region the harness signs for unless a case names another.
const REGION: &str = "us-east-1";
/// The maximum length of a `transferMessage`: the member is modelled `length 0..128`
/// (`smithy_query(query="operation",operation="TransferCertificate")`).
const MAX_TRANSFER_MESSAGE: usize = 128;
/// An IoT policy document the one policy this suite attaches is created with: the document
/// is not what this suite is about, so one valid document serves it.
const POLICY_DOCUMENT: &str =
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"iot:*","Resource":"*"}]}"#;

/// A SigV4 `Authorization` header whose credential scope names the `iot` signing name and
/// `region`: the scope is how the router picks the claiming service, and how the service
/// picks the region its ARNs are scoped to. The signature itself is never checked here —
/// the harness turns request authentication off, exactly as an unauthenticated emulator
/// runs.
fn signed_for_iot(region: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDTESTIOT/20260101/{region}/iot/aws4_request, \
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
    /// A string member of the body, or `None` when it is absent or not a string.
    fn text(&self, name: &str) -> Option<String> {
        self.body
            .get(name)
            .and_then(Value::as_str)
            .map(str::to_string)
    }

    /// The `message` of an error body.
    fn message(&self) -> String {
        self.text("message").unwrap_or_default()
    }

    /// A member of the reported `certificateDescription`.
    fn member(&self, name: &str) -> Option<&Value> {
        self.body
            .get("certificateDescription")
            .and_then(|description| description.get(name))
    }

    /// The described certificate's status.
    fn status_member(&self) -> Option<&str> {
        self.member("status").and_then(Value::as_str)
    }

    /// A member of the described certificate's `transferData` structure.
    fn transfer_member(&self, name: &str) -> Option<&Value> {
        self.member("transferData")
            .and_then(|transfer| transfer.get(name))
    }

    /// The `outgoingCertificates` of a `ListOutgoingCertificates` answer, in the order the
    /// listing reported them.
    fn outgoing(&self) -> Vec<&Value> {
        self.body
            .get("outgoingCertificates")
            .and_then(Value::as_array)
            .map(|entries| entries.iter().collect())
            .unwrap_or_default()
    }

    /// The `certificateId` of every row of a `ListOutgoingCertificates` answer.
    fn outgoing_ids(&self) -> Vec<String> {
        self.outgoing()
            .iter()
            .map(|entry| {
                entry
                    .get("certificateId")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string()
            })
            .collect()
    }

    /// Whether the described certificate's transfer state is present and EMPTY: the state
    /// of a certificate no transfer is touching.
    fn has_empty_transfer_data(&self) -> bool {
        self.member("transferData")
            .and_then(Value::as_object)
            .is_some_and(serde_json::Map::is_empty)
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

    async fn call(&self, method: &str, path: &str, region: &str, body: Option<&Value>) -> Reply {
        let payload = body.map_or_else(Vec::new, |value| {
            serde_json::to_vec(value).expect("a request body serializes")
        });
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://iot.{region}.amazonaws.com{path}"))
            .header("authorization", signed_for_iot(region))
            .header("content-type", "application/json")
            .body(bytes::Bytes::from(payload))
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

    /// `CreateKeysAndCertificate`: `POST /keys-and-certificate`, with `setAsActive` bound
    /// as an `@httpQuery` parameter.
    async fn create(&self, set_as_active: bool, region: &str) -> Reply {
        self.call(
            "POST",
            &format!("/keys-and-certificate?setAsActive={set_as_active}"),
            region,
            None,
        )
        .await
    }

    /// `DescribeCertificate`: `GET /certificates/{certificateId}`.
    async fn describe(&self, certificate_id: &str, region: &str) -> Reply {
        self.call(
            "GET",
            &format!("/certificates/{certificate_id}"),
            region,
            None,
        )
        .await
    }

    /// `UpdateCertificate`: `PUT /certificates/{certificateId}` with the required
    /// `newStatus` bound to the query string.
    async fn update(&self, certificate_id: &str, new_status: &str, region: &str) -> Reply {
        self.call(
            "PUT",
            &format!("/certificates/{certificate_id}?newStatus={new_status}"),
            region,
            None,
        )
        .await
    }

    /// `TransferCertificate`: `PATCH /transfer-certificate/{certificateId}` with
    /// `targetAwsAccount` bound to the query string and `transferMessage` — when the case
    /// carries one — in the body.
    async fn transfer(
        &self,
        certificate_id: &str,
        target_aws_account: &str,
        transfer_message: Option<&str>,
        region: &str,
    ) -> Reply {
        let body = transfer_message.map(|message| json!({ "transferMessage": message }));
        self.call(
            "PATCH",
            &format!("/transfer-certificate/{certificate_id}?targetAwsAccount={target_aws_account}"),
            region,
            body.as_ref(),
        )
        .await
    }

    /// `CancelCertificateTransfer`: `PATCH /cancel-certificate-transfer/{certificateId}`.
    async fn cancel_transfer(&self, certificate_id: &str, region: &str) -> Reply {
        self.call(
            "PATCH",
            &format!("/cancel-certificate-transfer/{certificate_id}"),
            region,
            None,
        )
        .await
    }

    /// `ListOutgoingCertificates`: `GET /certificates-out-going`, with `isAscendingOrder`
    /// and `pageSize` bound to the query string.
    async fn list_outgoing(
        &self,
        ascending_order: Option<bool>,
        page_size: Option<i32>,
        marker: Option<&str>,
        region: &str,
    ) -> Reply {
        let mut query: Vec<String> = Vec::new();
        if let Some(ascending_order) = ascending_order {
            query.push(format!("isAscendingOrder={ascending_order}"));
        }
        if let Some(page_size) = page_size {
            query.push(format!("pageSize={page_size}"));
        }
        if let Some(marker) = marker {
            query.push(format!("marker={marker}"));
        }
        let path = if query.is_empty() {
            "/certificates-out-going".to_string()
        } else {
            format!("/certificates-out-going?{}", query.join("&"))
        };
        self.call("GET", &path, region, None).await
    }

    /// `CreatePolicy`: `POST /policies/{policyName}`.
    async fn create_policy(&self, policy_name: &str, region: &str) -> Reply {
        self.call(
            "POST",
            &format!("/policies/{policy_name}"),
            region,
            Some(&json!({ "policyDocument": POLICY_DOCUMENT })),
        )
        .await
    }

    /// `AttachPolicy`: `PUT /target-policies/{policyName}` carrying the target ARN.
    async fn attach_policy(&self, policy_name: &str, target: &str, region: &str) -> Reply {
        self.call(
            "PUT",
            &format!("/target-policies/{policy_name}"),
            region,
            Some(&json!({ "target": target })),
        )
        .await
    }

    /// The id of a certificate issued for a case, moved to `status` when that is not the
    /// status the create resolves to.
    async fn issue(&self, case: &str, status: &str, region: &str) -> String {
        let issued = self.create(status == CERTIFICATE_STATUS_ACTIVE, region).await;
        assert_eq!(
            issued.status, 200,
            "case {case:?}: CreateKeysAndCertificate must succeed, got {issued:?}"
        );
        let certificate_id = issued.text("certificateId").unwrap_or_else(|| {
            panic!("case {case:?}: the issue must report a certificateId, got {issued:?}")
        });
        if status == CERTIFICATE_STATUS_REVOKED {
            let updated = self.update(&certificate_id, status, region).await;
            assert_eq!(
                updated.status, 200,
                "case {case:?}: UpdateCertificate to {status:?} must succeed, got {updated:?}"
            );
        }
        certificate_id
    }
}

/// The ARN a certificate of this account carries in `region`
/// (`arn:aws:iot:<region>:<account>:cert/<certificateId>`).
fn certificate_arn(region: &str, certificate_id: &str) -> String {
    format!("arn:aws:iot:{region}:{ACCOUNT_ID}:cert/{certificate_id}")
}

/// One transfer a case starts: the status the certificate is in, the account it is
/// transferred to, and the `transferMessage` the request carries.
#[derive(Debug)]
struct StartedTransfer {
    /// What this row is, for the assertion messages.
    case: &'static str,
    /// The status the certificate is in before the transfer — every status a transferable
    /// certificate can be in (`CertificateStatus` admits ACTIVE, INACTIVE and REVOKED to a
    /// caller, and the operation refuses only ACTIVE).
    status: &'static str,
    /// "The Amazon Web Services account", modelled `length 12..12, pattern ^[0-9]+$`.
    target_aws_account: &'static str,
    /// The `transferMessage` the request carries, and `None` for one that carries none.
    transfer_message: Option<String>,
    /// The region the call is signed for.
    region: &'static str,
}

/// The transfers this suite spans: every form `transferMessage` takes within its modelled
/// bound, several 12-digit target accounts, both statuses a transfer is allowed from, and
/// the region the call is signed for.
fn started_transfers() -> Vec<StartedTransfer> {
    vec![
        StartedTransfer {
            case: "an inactive certificate transferred with no message",
            status: CERTIFICATE_STATUS_INACTIVE,
            target_aws_account: "123456789012",
            transfer_message: None,
            region: REGION,
        },
        StartedTransfer {
            case: "an inactive certificate transferred with a message",
            status: CERTIFICATE_STATUS_INACTIVE,
            target_aws_account: "210987654321",
            transfer_message: Some("moving this device to the factory account".to_string()),
            region: REGION,
        },
        StartedTransfer {
            case: "a transfer carrying an empty message",
            status: CERTIFICATE_STATUS_INACTIVE,
            target_aws_account: "000000000001",
            transfer_message: Some(String::new()),
            region: REGION,
        },
        StartedTransfer {
            case: "a transfer carrying a message at its 128-character maximum",
            status: CERTIFICATE_STATUS_INACTIVE,
            target_aws_account: "999999999999",
            transfer_message: Some("m".repeat(MAX_TRANSFER_MESSAGE)),
            region: REGION,
        },
        StartedTransfer {
            case: "a revoked certificate transferred",
            status: CERTIFICATE_STATUS_REVOKED,
            target_aws_account: "123456789012",
            transfer_message: Some("revoked, handed back to the owner".to_string()),
            region: REGION,
        },
        StartedTransfer {
            case: "a transfer signed for another region",
            status: CERTIFICATE_STATUS_INACTIVE,
            target_aws_account: "555555555555",
            transfer_message: Some("another region".to_string()),
            region: "eu-west-2",
        },
    ]
}

/// RULE: starting a transfer records it on the certificate — the next read reports
/// `PENDING_TRANSFER`, the transfer date the write stamped and the message the request
/// carried (and none when it carried none) — and answers the ARN of that certificate.
#[tokio::test]
async fn a_started_transfer_is_recorded_on_the_certificate_it_names() {
    for row in started_transfers() {
        let emulator = Emulator::new();
        // A second certificate the case never transfers: the write must touch the one it
        // names and leave the account's others in the state they were in.
        let untouched = emulator
            .issue(row.case, CERTIFICATE_STATUS_INACTIVE, row.region)
            .await;
        let certificate_id = emulator.issue(row.case, row.status, row.region).await;

        let transferred = emulator
            .transfer(
                &certificate_id,
                row.target_aws_account,
                row.transfer_message.as_deref(),
                row.region,
            )
            .await;
        assert_eq!(
            transferred.status, 200,
            "case {:?}: TransferCertificate must succeed, got {transferred:?}",
            row.case
        );
        let expected_arn = certificate_arn(row.region, &certificate_id);
        assert_eq!(
            transferred.text("transferredCertificateArn"),
            Some(expected_arn.clone()),
            "case {:?}: expected transferredCertificateArn {expected_arn:?}, got {transferred:?}",
            row.case
        );

        let read = emulator.describe(&certificate_id, row.region).await;
        assert_eq!(
            read.status, 200,
            "case {:?}: DescribeCertificate must succeed after the transfer, got {read:?}",
            row.case
        );
        assert_eq!(
            read.status_member(),
            Some(CERTIFICATE_STATUS_PENDING_TRANSFER),
            "case {:?}: expected status {CERTIFICATE_STATUS_PENDING_TRANSFER:?}, got {read:?}",
            row.case
        );
        assert!(
            read.transfer_member("transferDate")
                .and_then(Value::as_f64)
                .is_some(),
            "case {:?}: a pending transfer must report its transferDate, got {read:?}",
            row.case
        );
        assert_eq!(
            read.transfer_member("transferMessage")
                .and_then(Value::as_str),
            row.transfer_message.as_deref(),
            "case {:?}: expected transferMessage {:?}, got {read:?}",
            row.case,
            row.transfer_message
        );
        for ended in ["acceptDate", "rejectDate", "rejectReason"] {
            assert!(
                read.transfer_member(ended).is_none(),
                "case {:?}: {ended} describes an ENDED transfer and must stay unset while one \
                 is pending, got {read:?}",
                row.case
            );
        }

        let other = emulator.describe(&untouched, row.region).await;
        assert_eq!(
            other.status_member(),
            Some(CERTIFICATE_STATUS_INACTIVE),
            "case {:?}: the certificate the transfer did not name must keep its status, got \
             {other:?}",
            row.case
        );
        assert!(
            other.has_empty_transfer_data(),
            "case {:?}: the certificate the transfer did not name must be in no transfer, got \
             {other:?}",
            row.case
        );
    }
}

/// RULE: a transfer the certificate's own state or its attached policies hold back is
/// refused with the class the model declares for that precondition, and the certificate is
/// left exactly as it was — never a 200 that records a transfer the service would not make.
#[tokio::test]
async fn a_transfer_its_preconditions_hold_back_is_refused_and_writes_nothing() {
    // Each row: what holds the transfer back, the status the certificate is in, the status
    // it must still be in afterwards, the error shape and the HTTP status the model
    // declares for that refusal, and whether a policy is attached first.
    let rows = [
        (
            "an ACTIVE certificate",
            CERTIFICATE_STATUS_ACTIVE,
            CERTIFICATE_STATUS_ACTIVE,
            false,
            "CertificateStateException",
            406_u16,
        ),
        (
            "a certificate with a policy attached",
            CERTIFICATE_STATUS_INACTIVE,
            CERTIFICATE_STATUS_INACTIVE,
            true,
            "TransferConflictException",
            409,
        ),
        (
            "a revoked certificate with a policy attached",
            CERTIFICATE_STATUS_REVOKED,
            CERTIFICATE_STATUS_REVOKED,
            true,
            "TransferConflictException",
            409,
        ),
    ];

    for (case, status, expected_status, attach_policy, shape, http_status) in rows {
        let emulator = Emulator::new();
        let certificate_id = emulator.issue(case, status, REGION).await;
        let arn = certificate_arn(REGION, &certificate_id);
        if attach_policy {
            let policy_name = "transfer-blocking-policy";
            let created = emulator.create_policy(policy_name, REGION).await;
            assert_eq!(
                created.status, 200,
                "case {case:?}: CreatePolicy must succeed, got {created:?}"
            );
            let attached = emulator.attach_policy(policy_name, &arn, REGION).await;
            assert_eq!(
                attached.status, 200,
                "case {case:?}: AttachPolicy must succeed, got {attached:?}"
            );
        }

        let refused = emulator
            .transfer(&certificate_id, "123456789012", Some("please take it"), REGION)
            .await;
        assert_eq!(
            refused.status, http_status,
            "case {case:?}: expected status {http_status}, got {refused:?}"
        );
        assert!(
            refused
                .error_code
                .as_deref()
                .is_some_and(|named| named.starts_with(shape)),
            "case {case:?}: expected the {shape} shape, got {refused:?}"
        );
        let expected_message = if attach_policy {
            format!("Certificate policies must be detached before transfer (arn: {arn})")
        } else {
            format!(
                "Certificate must be deactivated (not ACTIVE) before transfer. \
                 Id: {certificate_id}"
            )
        };
        assert_eq!(
            refused.message(),
            expected_message,
            "case {case:?}: expected message {expected_message:?}, got {:?}",
            refused.message()
        );

        let read = emulator.describe(&certificate_id, REGION).await;
        assert_eq!(
            read.status_member(),
            Some(expected_status),
            "case {case:?}: a refused transfer must leave the status alone, got {read:?}"
        );
        assert!(
            read.has_empty_transfer_data(),
            "case {case:?}: a refused transfer must record no transfer, got {read:?}"
        );
    }
}

/// RULE: a transfer of a certificate this account does not hold is refused
/// `ResourceNotFoundException` (404) naming the id the request carried, and a
/// `transferMessage` past its modelled maximum is refused before any transfer is recorded.
#[tokio::test]
async fn a_transfer_of_an_unknown_certificate_or_an_oversized_message_records_nothing() {
    let emulator = Emulator::new();
    // 64 hex characters, the modelled form of a `certificateId`, under which this account
    // holds nothing.
    let unknown = "a".repeat(64);
    let refused = emulator
        .transfer(&unknown, "123456789012", None, REGION)
        .await;
    assert_eq!(
        refused.status, 404,
        "case \"an unknown certificate id\": expected status 404, got {refused:?}"
    );
    assert!(
        refused
            .error_code
            .as_deref()
            .is_some_and(|named| named.starts_with("ResourceNotFoundException")),
        "case \"an unknown certificate id\": expected the ResourceNotFoundException shape, got \
         {refused:?}"
    );
    let expected = format!("CertificateId {unknown} does not exist");
    assert_eq!(
        refused.message(),
        expected,
        "case \"an unknown certificate id\": expected message {expected:?}, got {:?}",
        refused.message()
    );

    // One character past the modelled `length 0..128` of `transferMessage`: the request is
    // refused by the modelled constraint, so nothing about the certificate changes.
    let case = "a transferMessage one character past its maximum";
    let certificate_id = emulator
        .issue(case, CERTIFICATE_STATUS_INACTIVE, REGION)
        .await;
    let oversized = "m".repeat(MAX_TRANSFER_MESSAGE + 1);
    let refused = emulator
        .transfer(&certificate_id, "123456789012", Some(&oversized), REGION)
        .await;
    assert_eq!(
        refused.status, 400,
        "case {case:?}: expected status 400, got {refused:?}"
    );
    let read = emulator.describe(&certificate_id, REGION).await;
    assert_eq!(
        read.status_member(),
        Some(CERTIFICATE_STATUS_INACTIVE),
        "case {case:?}: a refused request must leave the status alone, got {read:?}"
    );
    assert!(
        read.has_empty_transfer_data(),
        "case {case:?}: a refused request must record no transfer, got {read:?}"
    );
}

/// RULE: cancelling a pending transfer returns the certificate to INACTIVE with no transfer
/// left to describe, whatever status it was transferred from and whatever message the
/// transfer carried — and the certificate can be transferred again afterwards, because the
/// cancellation really cleared the transfer.
#[tokio::test]
async fn a_cancelled_transfer_returns_the_certificate_inactive_with_no_transfer() {
    for row in started_transfers() {
        let emulator = Emulator::new();
        let certificate_id = emulator.issue(row.case, row.status, row.region).await;
        let started = emulator
            .transfer(
                &certificate_id,
                row.target_aws_account,
                row.transfer_message.as_deref(),
                row.region,
            )
            .await;
        assert_eq!(
            started.status, 200,
            "case {:?}: TransferCertificate must succeed, got {started:?}",
            row.case
        );

        let cancelled = emulator.cancel_transfer(&certificate_id, row.region).await;
        assert_eq!(
            cancelled.status, 200,
            "case {:?}: CancelCertificateTransfer must succeed, got {cancelled:?}",
            row.case
        );

        let read = emulator.describe(&certificate_id, row.region).await;
        assert_eq!(
            read.status_member(),
            Some(CERTIFICATE_STATUS_INACTIVE),
            "case {:?}: a cancelled transfer must leave the certificate \
             {CERTIFICATE_STATUS_INACTIVE:?}, got {read:?}",
            row.case
        );
        assert!(
            read.has_empty_transfer_data(),
            "case {:?}: a cancelled transfer must leave no transfer to describe, got {read:?}",
            row.case
        );

        // The transfer state really is clear: the certificate can start another transfer,
        // which an unchanged PENDING_TRANSFER record would refuse.
        let again = emulator
            .transfer(&certificate_id, row.target_aws_account, None, row.region)
            .await;
        assert_eq!(
            again.status, 200,
            "case {:?}: a cancelled certificate must be transferable again, got {again:?}",
            row.case
        );
        let read = emulator.describe(&certificate_id, row.region).await;
        assert_eq!(
            read.status_member(),
            Some(CERTIFICATE_STATUS_PENDING_TRANSFER),
            "case {:?}: the second transfer must be recorded, got {read:?}",
            row.case
        );
    }
}

/// RULE: a certificate in NO pending transfer has none to cancel — the cancellation is
/// refused `ResourceNotFoundException` (404) naming the id the request carried, exactly as
/// an id this account holds no certificate under is, and the certificate keeps the status it
/// had; a certificate already on its way to an account is refused a SECOND transfer because
/// its state forbids it.
#[tokio::test]
async fn cancelling_a_transfer_that_is_not_in_progress_is_refused() {
    // Each row: the certificate's state when the cancellation arrives, and the status it
    // must still carry afterwards.
    let rows = [
        (
            "a certificate no transfer has touched",
            CERTIFICATE_STATUS_INACTIVE,
            false,
            CERTIFICATE_STATUS_INACTIVE,
        ),
        (
            "an active certificate",
            CERTIFICATE_STATUS_ACTIVE,
            false,
            CERTIFICATE_STATUS_ACTIVE,
        ),
        (
            "a revoked certificate",
            CERTIFICATE_STATUS_REVOKED,
            false,
            CERTIFICATE_STATUS_REVOKED,
        ),
        (
            "a transfer already cancelled",
            CERTIFICATE_STATUS_INACTIVE,
            true,
            CERTIFICATE_STATUS_INACTIVE,
        ),
    ];

    for (case, status, cancel_first, expected_status) in rows {
        let emulator = Emulator::new();
        let certificate_id = emulator.issue(case, status, REGION).await;
        if cancel_first {
            let started = emulator
                .transfer(&certificate_id, "123456789012", None, REGION)
                .await;
            assert_eq!(
                started.status, 200,
                "case {case:?}: TransferCertificate must succeed, got {started:?}"
            );
            let cancelled = emulator.cancel_transfer(&certificate_id, REGION).await;
            assert_eq!(
                cancelled.status, 200,
                "case {case:?}: the first CancelCertificateTransfer must succeed, got \
                 {cancelled:?}"
            );
        }

        let refused = emulator.cancel_transfer(&certificate_id, REGION).await;
        assert_eq!(
            refused.status, 404,
            "case {case:?}: expected status 404, got {refused:?}"
        );
        assert!(
            refused
                .error_code
                .as_deref()
                .is_some_and(|named| named.starts_with("ResourceNotFoundException")),
            "case {case:?}: expected the ResourceNotFoundException shape, got {refused:?}"
        );
        let expected = format!("CertificateId {certificate_id} does not exist");
        assert_eq!(
            refused.message(),
            expected,
            "case {case:?}: expected message {expected:?}, got {:?}",
            refused.message()
        );

        let read = emulator.describe(&certificate_id, REGION).await;
        assert_eq!(
            read.status_member(),
            Some(expected_status),
            "case {case:?}: a refused cancellation must leave the status alone, got {read:?}"
        );
    }

    // An id this account holds no certificate under at all: the same refusal, because what
    // the call resolves is a pending transfer and this id has none either.
    let case = "an unknown certificate id";
    let emulator = Emulator::new();
    let unknown = "b".repeat(64);
    let refused = emulator.cancel_transfer(&unknown, REGION).await;
    assert_eq!(
        refused.status, 404,
        "case {case:?}: expected status 404, got {refused:?}"
    );
    let expected = format!("CertificateId {unknown} does not exist");
    assert_eq!(
        refused.message(),
        expected,
        "case {case:?}: expected message {expected:?}, got {:?}",
        refused.message()
    );

    // A certificate already on its way to an account is refused a second transfer: the
    // state the first transfer left is what forbids it, so the refusal names that state and
    // the transfer already recorded is untouched.
    let case = "a certificate already pending transfer";
    let emulator = Emulator::new();
    let certificate_id = emulator
        .issue(case, CERTIFICATE_STATUS_INACTIVE, REGION)
        .await;
    let started = emulator
        .transfer(&certificate_id, "123456789012", Some("the first"), REGION)
        .await;
    assert_eq!(
        started.status, 200,
        "case {case:?}: TransferCertificate must succeed, got {started:?}"
    );
    let refused = emulator
        .transfer(&certificate_id, "210987654321", Some("the second"), REGION)
        .await;
    assert_eq!(
        refused.status, 406,
        "case {case:?}: expected status 406, got {refused:?}"
    );
    assert!(
        refused
            .error_code
            .as_deref()
            .is_some_and(|named| named.starts_with("CertificateStateException")),
        "case {case:?}: expected the CertificateStateException shape, got {refused:?}"
    );
    let expected = format!("Certificate transfer is already in progress. Id: {certificate_id}");
    assert_eq!(
        refused.message(),
        expected,
        "case {case:?}: expected message {expected:?}, got {:?}",
        refused.message()
    );
    let read = emulator.describe(&certificate_id, REGION).await;
    assert_eq!(
        read.transfer_member("transferMessage").and_then(Value::as_str),
        Some("the first"),
        "case {case:?}: the refused transfer must leave the recorded one untouched, got {read:?}"
    );
}

/// RULE: the outgoing listing reports exactly the certificates a transfer is pending for —
/// each row carrying the account the transfer was made to, the date it was initiated and
/// the message it carried — and a certificate no transfer is pending for, including one
/// whose transfer was cancelled, is no row of it.
#[tokio::test]
async fn the_outgoing_listing_reports_exactly_the_certificates_a_transfer_is_pending_for() {
    let emulator = Emulator::new();
    let case = "three of five certificates transferred";
    // Two certificates no transfer ever touches, and three transferred to three different
    // accounts: with a message, with none, and with an empty one.
    let mut kept = Vec::new();
    for _ in 0..2 {
        kept.push(
            emulator
                .issue(case, CERTIFICATE_STATUS_INACTIVE, REGION)
                .await,
        );
    }
    let transfers = [
        ("123456789012", Some("to the factory account".to_string())),
        ("210987654321", None),
        ("999999999999", Some(String::new())),
    ];
    let mut transferred = Vec::new();
    for (target, message) in &transfers {
        let certificate_id = emulator
            .issue(case, CERTIFICATE_STATUS_INACTIVE, REGION)
            .await;
        let started = emulator
            .transfer(&certificate_id, target, message.as_deref(), REGION)
            .await;
        assert_eq!(
            started.status, 200,
            "case {case:?}: TransferCertificate must succeed, got {started:?}"
        );
        transferred.push((certificate_id, *target, message.clone()));
    }

    let listed = emulator.list_outgoing(None, None, None, REGION).await;
    assert_eq!(
        listed.status, 200,
        "case {case:?}: ListOutgoingCertificates must succeed, got {listed:?}"
    );
    let listed_ids = listed.outgoing_ids();
    for (certificate_id, target, message) in &transferred {
        assert!(
            listed_ids.contains(certificate_id),
            "case {case:?}: expected {certificate_id} among the outgoing certificates, got \
             {listed_ids:?}"
        );
        let row = listed
            .outgoing()
            .into_iter()
            .find(|row| row.get("certificateId").and_then(Value::as_str) == Some(certificate_id))
            .unwrap_or_else(|| panic!("case {case:?}: the row just asserted must be reachable"));
        assert_eq!(
            row.get("transferredTo").and_then(Value::as_str),
            Some(*target),
            "case {case:?}: expected transferredTo {target:?}, got {row:?}"
        );
        assert_eq!(
            row.get("transferMessage").and_then(Value::as_str),
            message.as_deref(),
            "case {case:?}: expected transferMessage {message:?}, got {row:?}"
        );
        let read = emulator.describe(certificate_id, REGION).await;
        assert_eq!(
            row.get("transferDate").and_then(Value::as_f64),
            read.transfer_member("transferDate").and_then(Value::as_f64),
            "case {case:?}: the listing and the description must report one transfer date, got \
             {row:?} and {read:?}"
        );
        assert_eq!(
            row.get("certificateArn").and_then(Value::as_str),
            Some(certificate_arn(REGION, certificate_id).as_str()),
            "case {case:?}: expected the certificate's own ARN, got {row:?}"
        );
        assert_eq!(
            row.get("creationDate").and_then(Value::as_f64),
            read.member("creationDate").and_then(Value::as_f64),
            "case {case:?}: the listing and the description must report one creation date, got \
             {row:?} and {read:?}"
        );
    }
    for certificate_id in &kept {
        assert!(
            !listed_ids.contains(certificate_id),
            "case {case:?}: a certificate in no transfer must be no row of the outgoing \
             listing, got {listed_ids:?}"
        );
    }

    // A cancelled transfer is no longer pending, so its certificate leaves the listing
    // while the other two stay.
    let (cancelled, _, _) = &transferred[0];
    let reply = emulator.cancel_transfer(cancelled, REGION).await;
    assert_eq!(
        reply.status, 200,
        "case {case:?}: CancelCertificateTransfer must succeed, got {reply:?}"
    );
    let listed = emulator.list_outgoing(None, None, None, REGION).await;
    let listed_ids = listed.outgoing_ids();
    assert!(
        !listed_ids.contains(cancelled),
        "case {case:?}: a cancelled transfer must leave the outgoing listing, got {listed_ids:?}"
    );
    for (certificate_id, _, _) in &transferred[1..] {
        assert!(
            listed_ids.contains(certificate_id),
            "case {case:?}: the transfers still pending must stay listed, got {listed_ids:?}"
        );
    }
}

/// RULE: the outgoing listing is ordered by the certificates' creation date in the
/// direction the request names and pages through that order, so walking every page of any
/// page size reports each pending transfer exactly once; a `pageSize` outside the modelled
/// 1..250 range is refused.
#[tokio::test]
async fn the_outgoing_listing_pages_through_its_order_in_the_direction_requested() {
    let case = "five pending transfers";
    let emulator = Emulator::new();
    let mut expected = Vec::new();
    for index in 0..5 {
        let certificate_id = emulator
            .issue(case, CERTIFICATE_STATUS_INACTIVE, REGION)
            .await;
        let started = emulator
            .transfer(
                &certificate_id,
                "123456789012",
                Some(&format!("transfer {index}")),
                REGION,
            )
            .await;
        assert_eq!(
            started.status, 200,
            "case {case:?}: TransferCertificate must succeed, got {started:?}"
        );
        expected.push(certificate_id);
    }
    expected.sort();

    // Both directions the modelled `isAscendingOrder` takes, including the request that
    // names none (the member is modelled `default false`), and every page size from one
    // row per page to the modelled maximum.
    for ascending_order in [None, Some(false), Some(true)] {
        for page_size in [None, Some(1), Some(2), Some(5), Some(250)] {
            let case = format!("{case}, isAscendingOrder={ascending_order:?}, pageSize={page_size:?}");
            let mut walked: Vec<String> = Vec::new();
            let mut dates: Vec<f64> = Vec::new();
            let mut marker: Option<String> = None;
            // One page per pending transfer at most, plus the page that ends the walk.
            for _ in 0..=expected.len() {
                let page = emulator
                    .list_outgoing(ascending_order, page_size, marker.as_deref(), REGION)
                    .await;
                assert_eq!(
                    page.status, 200,
                    "case {case:?}: ListOutgoingCertificates must succeed, got {page:?}"
                );
                for row in page.outgoing() {
                    dates.push(
                        row.get("transferDate")
                            .and_then(Value::as_f64)
                            .unwrap_or_else(|| {
                                panic!("case {case:?}: every row must report a transferDate")
                            }),
                    );
                }
                walked.extend(page.outgoing_ids());
                marker = page.text("nextMarker");
                if marker.is_none() {
                    break;
                }
            }
            assert!(
                marker.is_none(),
                "case {case:?}: the walk must end, got a marker after every page"
            );
            let mut reported = walked.clone();
            reported.sort();
            assert_eq!(
                reported, expected,
                "case {case:?}: walking every page must report each pending transfer exactly \
                 once, got {walked:?}"
            );
            // The order is the creation date, and the transfers were started in creation
            // order, so the dates run the way the direction names.
            let ordered = if ascending_order == Some(true) {
                dates.windows(2).all(|pair| pair[0] <= pair[1])
            } else {
                dates.windows(2).all(|pair| pair[0] >= pair[1])
            };
            assert!(
                ordered,
                "case {case:?}: expected the rows in the order the direction names, got {dates:?}"
            );
        }
    }

    // One past each published bound of `pageSize` (modelled `range min 1, max 250`).
    for page_size in [0, 251] {
        let refused = emulator
            .list_outgoing(None, Some(page_size), None, REGION)
            .await;
        assert_eq!(
            refused.status, 400,
            "case \"pageSize={page_size}\": expected status 400, got {refused:?}"
        );
    }
}
