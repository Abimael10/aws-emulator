// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of the TRANSFER STATE a described certificate reports, and of
//! the status and customer version beside it.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a restJson1
//! request signed for the `iot` signing name — so what is asserted is what an AWS SDK
//! client would see, not an internal function's return value.
//!
//! The rule under test: a read of one certificate projects every member that describes a
//! STATE the certificate always has, so a structure member describing such a state is
//! answered as an empty structure rather than left out when nothing has happened to it.
//! Here that member is `transferData`, "The transfer data"
//! (`smithy_query(query="shape",shape="CertificateDescription")`): a certificate no
//! transfer has touched is described with the structure present and each of its five
//! members — `transferMessage`, `rejectReason`, `transferDate`, `acceptDate`,
//! `rejectDate` — unset, so a client branching on a transfer's dates or its reject reason
//! reaches an empty structure and not an absent one. The members that describe a
//! relationship the certificate does NOT have — the CA that signed it, the account that
//! owned it before — stay absent, which is what keeps this about the transfer state
//! rather than about projecting every optional member.
//!
//! Two neighbouring rules are asserted on the same reads, because they are what a caller
//! reads the description FOR: `UpdateCertificate`'s `newStatus` is reported by the next
//! read and takes the certificate to its next `customerVersion` (modelled `range min 1`),
//! and a certificate `DeleteCertificate` removed is not described at all — the read is
//! refused `ResourceNotFoundException` (404) with no description in the body.
//!
//! The table spans what the rule generalises over rather than one request: the three forms
//! `setAsActive` takes on the wire (absent — the modelled `default false` — explicitly
//! `false`, explicitly `true`), every status a caller may move a certificate to, several
//! certificates held at once, the region the call is signed for, and a certificate read
//! twice over.

use std::sync::Arc;

use serde_json::Value;
use service_com_amazonaws_iot_awsiotservice::state::{
    CERTIFICATE_STATUS_ACTIVE, CERTIFICATE_STATUS_INACTIVE, CERTIFICATE_STATUS_REVOKED,
    FIRST_CERTIFICATE_CUSTOMER_VERSION,
};
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region the harness signs for unless a case names another.
const REGION: &str = "us-east-1";

/// The members of the modelled `TransferData` structure
/// (`smithy_query(query="shape",shape="TransferData")`): the five a reader of a
/// certificate's transfer state walks, written out here so the set is pinned by this suite
/// rather than by the code under test.
const TRANSFER_DATA_MEMBERS: [&str; 5] = [
    "transferMessage",
    "rejectReason",
    "transferDate",
    "acceptDate",
    "rejectDate",
];

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

    /// The reported `certificateDescription`, or `None` when the body carries none.
    fn description(&self) -> Option<&Value> {
        self.body.get("certificateDescription")
    }

    /// A member of the reported `certificateDescription`.
    fn member(&self, name: &str) -> Option<&Value> {
        self.description().and_then(|value| value.get(name))
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

    async fn call(&self, method: &str, path: &str, region: &str) -> Reply {
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://iot.{region}.amazonaws.com{path}"))
            .header("authorization", signed_for_iot(region))
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

    /// `CreateKeysAndCertificate`: `POST /keys-and-certificate`, with `setAsActive` bound
    /// as an `@httpQuery` parameter — left out when the case names none.
    async fn create(&self, set_as_active: Option<bool>, region: &str) -> Reply {
        let path = match set_as_active {
            None => "/keys-and-certificate".to_string(),
            Some(active) => format!("/keys-and-certificate?setAsActive={active}"),
        };
        self.call("POST", &path, region).await
    }

    /// `DescribeCertificate`: `GET /certificates/{certificateId}`.
    async fn describe(&self, certificate_id: &str, region: &str) -> Reply {
        self.call("GET", &format!("/certificates/{certificate_id}"), region)
            .await
    }

    /// `UpdateCertificate`: `PUT /certificates/{certificateId}` with the required
    /// `newStatus` bound to the query string.
    async fn update(&self, certificate_id: &str, new_status: &str, region: &str) -> Reply {
        self.call(
            "PUT",
            &format!("/certificates/{certificate_id}?newStatus={new_status}"),
            region,
        )
        .await
    }

    /// `DeleteCertificate`: `DELETE /certificates/{certificateId}`.
    async fn delete(&self, certificate_id: &str, region: &str) -> Reply {
        self.call("DELETE", &format!("/certificates/{certificate_id}"), region)
            .await
    }

    /// The id of a certificate issued for a case.
    async fn issue(&self, case: &str, set_as_active: Option<bool>, region: &str) -> String {
        let issued = self.create(set_as_active, region).await;
        assert_eq!(
            issued.status, 200,
            "case {case:?}: CreateKeysAndCertificate must succeed, got {issued:?}"
        );
        issued.text("certificateId").unwrap_or_else(|| {
            panic!("case {case:?}: the issue must report a certificateId, got {issued:?}")
        })
    }
}

/// Assert that `reply` describes a certificate whose transfer state is present and empty.
fn asserts_an_empty_transfer_state(case: &str, reply: &Reply) {
    let transfer_data = reply.member("transferData").unwrap_or_else(|| {
        panic!("case {case:?}: the description must report transferData, got {reply:?}")
    });
    let members = transfer_data.as_object().unwrap_or_else(|| {
        panic!(
            "case {case:?}: transferData must be a structure, got {:?}",
            transfer_data
        )
    });
    assert!(
        members.is_empty(),
        "case {case:?}: a certificate no transfer has touched must report every \
         transferData member unset, got {members:?}"
    );
    for member in TRANSFER_DATA_MEMBERS {
        assert!(
            transfer_data.get(member).is_none(),
            "case {case:?}: transferData.{member} must be unset, got {transfer_data:?}"
        );
    }
}

/// One certificate a case issues: how it was created, and the status that resolves to.
#[derive(Debug)]
struct IssuedCertificate {
    /// What this row is, for the assertion messages.
    case: &'static str,
    /// The `setAsActive` the create carried, or `None` for a create that carried none.
    set_as_active: Option<bool>,
    /// The region the call is signed for.
    region: &'static str,
    /// The status the create resolves to.
    status: &'static str,
}

/// The creates this suite spans: the three forms `setAsActive` takes on the wire, and the
/// region the call is signed for.
fn issued_certificates() -> Vec<IssuedCertificate> {
    vec![
        IssuedCertificate {
            case: "a create carrying no setAsActive",
            set_as_active: None,
            region: REGION,
            status: CERTIFICATE_STATUS_INACTIVE,
        },
        IssuedCertificate {
            case: "a create carrying setAsActive=false",
            set_as_active: Some(false),
            region: REGION,
            status: CERTIFICATE_STATUS_INACTIVE,
        },
        IssuedCertificate {
            case: "a create carrying setAsActive=true",
            set_as_active: Some(true),
            region: REGION,
            status: CERTIFICATE_STATUS_ACTIVE,
        },
        IssuedCertificate {
            case: "a create signed for another region",
            set_as_active: Some(true),
            region: "eu-west-2",
            status: CERTIFICATE_STATUS_ACTIVE,
        },
    ]
}

/// RULE: every described certificate reports its transfer state as a structure whose
/// members are unset while no transfer has touched it — whatever the certificate's status,
/// however it was created, and however many certificates the account holds — while the
/// members describing a relationship it does not have stay absent.
#[tokio::test]
async fn every_described_certificate_reports_an_empty_transfer_state() {
    // ONE emulator for the whole table, so each read is made with several certificates in
    // the store: the projection is a property of the record, not of the store holding one.
    let emulator = Emulator::new();
    let mut issued = Vec::new();
    for row in issued_certificates() {
        let certificate_id = emulator
            .issue(row.case, row.set_as_active, row.region)
            .await;
        issued.push((row.case, row.region, row.status, certificate_id));
    }

    for (case, region, status, certificate_id) in &issued {
        // Read twice: the description is a pure read, so the second read reports the same
        // transfer state as the first.
        for pass in ["the first read", "the second read"] {
            let case = format!("{case} ({pass})");
            let read = emulator.describe(certificate_id, region).await;
            assert_eq!(
                read.status, 200,
                "case {case:?}: DescribeCertificate of an issued id must succeed, got {read:?}"
            );
            asserts_an_empty_transfer_state(&case, &read);
            assert_eq!(
                read.member("status").and_then(Value::as_str),
                Some(*status),
                "case {case:?}: expected status {status:?}, got {read:?}"
            );
            assert_eq!(
                read.member("customerVersion").and_then(Value::as_i64),
                Some(i64::from(FIRST_CERTIFICATE_CUSTOMER_VERSION)),
                "case {case:?}: expected customer version \
                 {FIRST_CERTIFICATE_CUSTOMER_VERSION}, got {read:?}"
            );
            for absent in ["caCertificateId", "previousOwnedBy"] {
                assert!(
                    read.member(absent).is_none(),
                    "case {case:?}: {absent} must stay absent on a certificate with no such \
                     relation, got {read:?}"
                );
            }
        }
    }
}

/// RULE: `UpdateCertificate`'s `newStatus` is what the next read reports, and a customer's
/// change takes the certificate to its next `customerVersion` — with the transfer state
/// still projected, since a status change is no transfer.
#[tokio::test]
async fn an_updated_certificate_reports_its_new_status_and_keeps_its_transfer_state() {
    // Every status a caller may move a certificate to
    // (`smithy_query(query="operation",operation="UpdateCertificate")` admits ACTIVE,
    // INACTIVE and REVOKED; PENDING_TRANSFER and PENDING_ACTIVATION are IoT's own), each
    // reached from a certificate created the other way round so no row's expected status
    // is the one the create already left.
    let rows = [
        ("deactivated", Some(true), CERTIFICATE_STATUS_INACTIVE),
        ("activated", Some(false), CERTIFICATE_STATUS_ACTIVE),
        ("revoked", Some(true), CERTIFICATE_STATUS_REVOKED),
        ("revoked from inactive", None, CERTIFICATE_STATUS_REVOKED),
    ];

    for (case, set_as_active, new_status) in rows {
        let emulator = Emulator::new();
        let certificate_id = emulator.issue(case, set_as_active, REGION).await;

        let updated = emulator.update(&certificate_id, new_status, REGION).await;
        assert_eq!(
            updated.status, 200,
            "case {case:?}: UpdateCertificate to {new_status:?} must succeed, got {updated:?}"
        );

        let read = emulator.describe(&certificate_id, REGION).await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: DescribeCertificate must succeed after the update, got {read:?}"
        );
        assert_eq!(
            read.member("status").and_then(Value::as_str),
            Some(new_status),
            "case {case:?}: expected the status the update set ({new_status:?}), got {read:?}"
        );
        assert_eq!(
            read.member("customerVersion").and_then(Value::as_i64),
            Some(i64::from(FIRST_CERTIFICATE_CUSTOMER_VERSION) + 1),
            "case {case:?}: expected the next customer version after a customer's change, \
             got {read:?}"
        );
        asserts_an_empty_transfer_state(case, &read);
    }
}

/// RULE: a certificate `DeleteCertificate` removed is described no more — the read is
/// refused `ResourceNotFoundException` (404) naming the id the request carried, and the
/// body carries no description at all.
#[tokio::test]
async fn a_deleted_certificate_is_described_no_more() {
    // Both ways a deletable certificate reaches its non-ACTIVE status: created that way,
    // and moved there by an update ("The certificate must not have any policies attached
    // to it", so none is attached in either row).
    let rows = [
        ("a certificate created inactive", None, None),
        (
            "a certificate deactivated by an update",
            Some(true),
            Some(CERTIFICATE_STATUS_INACTIVE),
        ),
        (
            "a certificate revoked by an update",
            Some(false),
            Some(CERTIFICATE_STATUS_REVOKED),
        ),
    ];

    for (case, set_as_active, new_status) in rows {
        let emulator = Emulator::new();
        // A second certificate the case never deletes: the delete must remove the one it
        // names and leave the account's others describable.
        let kept = emulator.issue(case, Some(true), REGION).await;
        let certificate_id = emulator.issue(case, set_as_active, REGION).await;
        if let Some(new_status) = new_status {
            let updated = emulator.update(&certificate_id, new_status, REGION).await;
            assert_eq!(
                updated.status, 200,
                "case {case:?}: UpdateCertificate to {new_status:?} must succeed, got {updated:?}"
            );
        }

        let deleted = emulator.delete(&certificate_id, REGION).await;
        assert_eq!(
            deleted.status, 200,
            "case {case:?}: DeleteCertificate must succeed, got {deleted:?}"
        );

        let read = emulator.describe(&certificate_id, REGION).await;
        assert_eq!(
            read.status, 404,
            "case {case:?}: a deleted certificate must not be described, got {read:?}"
        );
        // The SHAPE the refusal names. Whether AWS IoT qualifies this shape's name with
        // its API doc namespace the way it qualifies `InvalidRequestException` is a
        // question about the error-type header that a report of its own carries, so what
        // is asserted here is the shape and not the qualification.
        assert!(
            read.error_code
                .as_deref()
                .is_some_and(|named| named.starts_with("ResourceNotFoundException")),
            "case {case:?}: expected the ResourceNotFoundException shape, got {read:?}"
        );
        let expected = format!("CertificateId {certificate_id} does not exist");
        assert_eq!(
            read.message(),
            expected,
            "case {case:?}: expected message {expected:?}, got {:?}",
            read.message()
        );
        assert!(
            read.description().is_none(),
            "case {case:?}: a refused read must carry no description, got {read:?}"
        );

        let other = emulator.describe(&kept, REGION).await;
        assert_eq!(
            other.status, 200,
            "case {case:?}: the certificate the delete did not name must still be described, \
             got {other:?}"
        );
        asserts_an_empty_transfer_state(case, &other);
    }
}
