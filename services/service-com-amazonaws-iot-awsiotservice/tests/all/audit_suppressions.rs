// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of DEVICE DEFENDER AUDIT SUPPRESSIONS: `CreateAuditSuppression`
//! and the `DescribeAuditSuppression` reader that proves what it persisted.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a restJson1
//! request signed for the `iot` signing name — so what is asserted is what an AWS SDK
//! client would see, not an internal function's return value.
//!
//! The rules under test, none of them specific to one suppression:
//!
//! * a suppression is identified by the PAIR `checkName` + `resourceIdentifier` and by
//!   nothing else — the family models no name, id or ARN — so the create persists the
//!   record under that pair and every later read of the SAME pair reports THAT record
//!   member for member, whichever member of the identifier names the resource;
//! * a `resourceIdentifier` is canonical: a nested identifier left out entirely and one
//!   sent with no member set name the same resource, and a reader reports only the parts
//!   that identify it;
//! * `checkName` is "an audit check name", so a name outside the account's catalog of audit
//!   checks is refused `InvalidRequestException` (400), persisting nothing — while EVERY
//!   name in that catalog is suppressible, whatever the account has enabled;
//! * a `resourceIdentifier` that identifies no resource at all is refused
//!   `InvalidRequestException` (400), persisting nothing;
//! * the pair is the identity, so a second create for a pair the account already holds a
//!   suppression for is refused `ResourceAlreadyExistsException` (409) and leaves the stored
//!   record exactly as it was, while the same check over a DIFFERENT resource is a
//!   suppression of its own;
//! * "each audit supression must have a unique client request token": a create under a
//!   token the account already holds a suppression under is refused
//!   `ResourceAlreadyExistsException` (409);
//! * a pair the account holds no suppression for is `ResourceNotFoundException` (404) to the
//!   reader, never a hollow 200, and reading a suppression changes nothing a later reader
//!   sees;
//! * the store is bounded: past the emulator's ceiling the create is refused
//!   `LimitExceededException` (410) rather than growing without bound.
//!
//! The table spans what those rules generalise over — every member of the modelled
//! `ResourceIdentifier` including both nested identifiers and a nested one only partly
//! named, which audit check is suppressed, whether the suppression carries an
//! `expirationDate`, a `suppressIndefinitely` flag, both or neither, whether it carries a
//! description, and the form and length of the `clientRequestToken` within its modelled
//! 1..64 — rather than one recorded request.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Map, Value};
use service_com_amazonaws_iot_awsiotservice::state::{
    AUDIT_CHECK_NAMES, MAX_AUDIT_SUPPRESSIONS,
};
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region this suite signs for.
const REGION: &str = "eu-west-2";

/// Every member of the modelled `ResourceIdentifier`, read off
/// `smithy_query(query="shape",shape="ResourceIdentifier")`: the identifier members a
/// suppression can be about, which the table below covers in full.
const RESOURCE_IDENTIFIER_MEMBERS: &[&str] = &[
    "deviceCertificateId",
    "caCertificateId",
    "cognitoIdentityPoolId",
    "clientId",
    "policyVersionIdentifier",
    "account",
    "iamRoleArn",
    "roleAliasArn",
    "issuerCertificateIdentifier",
    "deviceCertificateArn",
];

/// A certificate id of the modelled form: `CertificateId` is `length 64..64`,
/// `pattern ^(0x)?[a-fA-F0-9]+$`.
const DEVICE_CERTIFICATE_ID: &str =
    "1f2e3d4c5b6a79880f1e2d3c4b5a69780f1e2d3c4b5a69781f2e3d4c5b6a7988";

/// A second certificate id of that same form, for the CA certificate half of the
/// identifier.
const CA_CERTIFICATE_ID: &str =
    "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";

/// One row of the table: the suppression a create asks for.
struct Row {
    /// The name this case is reported under.
    case: &'static str,
    /// The audit check whose findings the suppression filters out.
    check_name: &'static str,
    /// The `resourceIdentifier` the request sends.
    identifier: Value,
    /// The `resourceIdentifier` a reader has to report back — the one sent, canonicalised:
    /// a nested identifier that names nothing is not part of the identity.
    expected_identifier: Value,
    /// "The epoch timestamp in seconds at which this suppression expires", when the row
    /// carries one.
    expiration_date: Option<i64>,
    /// "Indicates whether a suppression should exist indefinitely or not", when the row
    /// carries the flag.
    suppress_indefinitely: Option<bool>,
    /// "The description of the audit suppression", when the row carries one.
    description: Option<&'static str>,
    /// The `clientRequestToken` the create is made under.
    token: &'static str,
}

/// The suppressions this suite creates and reads back.
///
/// The rows differ along what the rules generalise over: which member of the modelled
/// `ResourceIdentifier` names the resource (every one of them, including both nested
/// identifiers, a nested identifier only partly named, and an identifier naming several
/// members at once), which audit check is suppressed, which of `expirationDate` and
/// `suppressIndefinitely` the suppression carries, whether it carries a description, and the
/// form and length of the token within `ClientRequestToken`'s modelled 1..64.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "device-certificate",
            check_name: "DEVICE_CERTIFICATE_EXPIRING_CHECK",
            identifier: json!({ "deviceCertificateId": DEVICE_CERTIFICATE_ID }),
            expected_identifier: json!({ "deviceCertificateId": DEVICE_CERTIFICATE_ID }),
            expiration_date: Some(1_900_000_000),
            suppress_indefinitely: None,
            description: Some("expiring certificate reviewed by the fleet team"),
            token: "a",
        },
        Row {
            case: "ca-certificate",
            check_name: "CA_CERTIFICATE_EXPIRING_CHECK",
            identifier: json!({ "caCertificateId": CA_CERTIFICATE_ID }),
            expected_identifier: json!({ "caCertificateId": CA_CERTIFICATE_ID }),
            expiration_date: None,
            suppress_indefinitely: Some(true),
            description: None,
            token: "ca-certificate-suppression-token",
        },
        Row {
            case: "cognito-identity-pool",
            check_name: "AUTHENTICATED_COGNITO_ROLE_OVERLY_PERMISSIVE_CHECK",
            identifier: json!({
                "cognitoIdentityPoolId": "eu-west-2:4bd1f0a1-1c7c-4a40-9d1f-6a5f9e0b2c31",
            }),
            expected_identifier: json!({
                "cognitoIdentityPoolId": "eu-west-2:4bd1f0a1-1c7c-4a40-9d1f-6a5f9e0b2c31",
            }),
            expiration_date: Some(2_000_000_000),
            suppress_indefinitely: Some(false),
            description: Some(""),
            token: "cognito_identity_pool-0",
        },
        Row {
            case: "client-id",
            check_name: "CONFLICTING_CLIENT_IDS_CHECK",
            identifier: json!({ "clientId": "shop-floor-sensor-7" }),
            expected_identifier: json!({ "clientId": "shop-floor-sensor-7" }),
            expiration_date: None,
            suppress_indefinitely: None,
            description: None,
            token: "CLIENT-ID-TOKEN",
        },
        Row {
            case: "policy-version",
            check_name: "IOT_POLICY_OVERLY_PERMISSIVE_CHECK",
            identifier: json!({
                "policyVersionIdentifier": {
                    "policyName": "fleet-wide-publish",
                    "policyVersionId": "3",
                },
            }),
            expected_identifier: json!({
                "policyVersionIdentifier": {
                    "policyName": "fleet-wide-publish",
                    "policyVersionId": "3",
                },
            }),
            expiration_date: Some(1_950_000_000),
            suppress_indefinitely: None,
            description: Some("legacy policy, migration tracked"),
            token: "policy-version-3",
        },
        Row {
            case: "policy-name-only",
            check_name: "IOT_POLICY_POTENTIAL_MISCONFIGURATION_CHECK",
            identifier: json!({
                "policyVersionIdentifier": { "policyName": "legacy-subscribe" },
                // A nested identifier with no member set names nothing, so the reader
                // reports it not at all.
                "issuerCertificateIdentifier": {},
            }),
            expected_identifier: json!({
                "policyVersionIdentifier": { "policyName": "legacy-subscribe" },
            }),
            expiration_date: None,
            suppress_indefinitely: Some(true),
            description: None,
            token: "policy_name_only",
        },
        Row {
            case: "account",
            check_name: "LOGGING_DISABLED_CHECK",
            identifier: json!({ "account": "000000000000" }),
            expected_identifier: json!({ "account": "000000000000" }),
            expiration_date: None,
            suppress_indefinitely: Some(true),
            description: Some("logging handled by the platform account"),
            token: "account-wide-logging",
        },
        Row {
            case: "iam-role",
            check_name: "UNAUTHENTICATED_COGNITO_ROLE_OVERLY_PERMISSIVE_CHECK",
            identifier: json!({
                "iamRoleArn": "arn:aws:iam::000000000000:role/unauthenticated-devices",
            }),
            expected_identifier: json!({
                "iamRoleArn": "arn:aws:iam::000000000000:role/unauthenticated-devices",
            }),
            expiration_date: Some(1_980_000_000),
            suppress_indefinitely: None,
            description: None,
            token: "iam-role-arn-token",
        },
        Row {
            case: "role-alias",
            check_name: "IOT_ROLE_ALIAS_OVERLY_PERMISSIVE_CHECK",
            identifier: json!({
                "roleAliasArn": "arn:aws:iot:eu-west-2:000000000000:rolealias/credential-alias",
            }),
            expected_identifier: json!({
                "roleAliasArn": "arn:aws:iot:eu-west-2:000000000000:rolealias/credential-alias",
            }),
            expiration_date: None,
            suppress_indefinitely: None,
            description: Some("alias scoped by a session policy"),
            token: "role_alias-arn",
        },
        Row {
            case: "issuer-certificate",
            check_name: "INTERMEDIATE_CA_REVOKED_FOR_ACTIVE_DEVICE_CERTIFICATES_CHECK",
            identifier: json!({
                "issuerCertificateIdentifier": {
                    "issuerCertificateSubject": "CN=fleet intermediate, O=emulator",
                    "issuerId": "0a1b2c3d4e5f6071",
                    "issuerCertificateSerialNumber": "0A:1B:2C:3D",
                },
            }),
            expected_identifier: json!({
                "issuerCertificateIdentifier": {
                    "issuerCertificateSubject": "CN=fleet intermediate, O=emulator",
                    "issuerId": "0a1b2c3d4e5f6071",
                    "issuerCertificateSerialNumber": "0A:1B:2C:3D",
                },
            }),
            expiration_date: Some(2_100_000_000),
            suppress_indefinitely: Some(false),
            description: None,
            token: "issuer-certificate-identifier-token",
        },
        Row {
            case: "device-certificate-arn",
            check_name: "DEVICE_CERTIFICATE_SHARED_CHECK",
            identifier: json!({
                "deviceCertificateArn": format!(
                    "arn:aws:iot:eu-west-2:000000000000:cert/{DEVICE_CERTIFICATE_ID}"
                ),
            }),
            expected_identifier: json!({
                "deviceCertificateArn": format!(
                    "arn:aws:iot:eu-west-2:000000000000:cert/{DEVICE_CERTIFICATE_ID}"
                ),
            }),
            expiration_date: None,
            suppress_indefinitely: None,
            description: Some("shared certificate retired next quarter"),
            token: "device-certificate-arn",
        },
        Row {
            case: "several-members",
            check_name: "DEVICE_CERTIFICATE_KEY_QUALITY_CHECK",
            identifier: json!({
                "deviceCertificateId": DEVICE_CERTIFICATE_ID,
                "caCertificateId": CA_CERTIFICATE_ID,
                "clientId": "gateway-1",
            }),
            expected_identifier: json!({
                "deviceCertificateId": DEVICE_CERTIFICATE_ID,
                "caCertificateId": CA_CERTIFICATE_ID,
                "clientId": "gateway-1",
            }),
            expiration_date: Some(1_920_000_000),
            suppress_indefinitely: Some(true),
            description: Some("key quality waived for the pilot gateway"),
            // The longest token the model allows: `ClientRequestToken` is length 1..64.
            token: "0123456789012345678901234567890123456789012345678901234567890123",
        },
    ]
}

impl Row {
    /// The create request body this row asks for.
    fn body(&self) -> Value {
        let mut request = Map::new();
        request.insert("checkName".to_string(), json!(self.check_name));
        request.insert("resourceIdentifier".to_string(), self.identifier.clone());
        if let Some(expiration_date) = self.expiration_date {
            request.insert("expirationDate".to_string(), json!(expiration_date));
        }
        if let Some(suppress_indefinitely) = self.suppress_indefinitely {
            request.insert(
                "suppressIndefinitely".to_string(),
                json!(suppress_indefinitely),
            );
        }
        if let Some(description) = self.description {
            request.insert("description".to_string(), json!(description));
        }
        request.insert("clientRequestToken".to_string(), json!(self.token));
        Value::Object(request)
    }

    /// The describe request body addressing this row's suppression: the pair alone.
    fn lookup(&self) -> Value {
        json!({
            "checkName": self.check_name,
            "resourceIdentifier": self.identifier.clone(),
        })
    }

    /// What `DescribeAuditSuppression` has to report for this row, apart from the
    /// `expirationDate` (a timestamp, compared numerically).
    fn expected_description(&self) -> Value {
        let mut expected = Map::new();
        expected.insert("checkName".to_string(), json!(self.check_name));
        expected.insert(
            "resourceIdentifier".to_string(),
            self.expected_identifier.clone(),
        );
        if let Some(suppress_indefinitely) = self.suppress_indefinitely {
            expected.insert(
                "suppressIndefinitely".to_string(),
                json!(suppress_indefinitely),
            );
        }
        if let Some(description) = self.description {
            expected.insert("description".to_string(), json!(description));
        }
        Value::Object(expected)
    }
}

/// How this service states that a pair names no audit suppression this account holds: the
/// Device Defender configuration families' sentence, which names the input member the
/// request carried the check at.
fn not_found_message(check_name: &str) -> String {
    format!("Resource checkName={check_name} was not found")
}

/// How this service states that the account already holds a suppression for a pair.
fn already_exists_message(check_name: &str) -> String {
    format!("Audit suppression for checkName={check_name} already exists for this account")
}

/// How this service states that the account already holds a suppression under a token.
fn token_already_exists_message(client_request_token: &str) -> String {
    format!(
        "Audit suppression with clientRequestToken={client_request_token} already exists for \
         this account"
    )
}

/// One emulator: the shared engine plus a router with this service mounted.
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

    async fn dispatch(&self, path: &str, body: &Value) -> Reply {
        let host = format!("iot.{REGION}.amazonaws.com");
        let request = http::Request::builder()
            .method("POST")
            .uri(format!("https://{host}{path}"))
            .header("host", host)
            .header("content-type", "application/json")
            // The credential scope names the `iot` signing name the model's
            // `aws.auth#sigv4` trait declares, which is what routes the request.
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/iot/aws4_request, \
                     SignedHeaders=host, Signature=0123456789abcdef"
                ),
            )
            .body(Bytes::from(body.to_string()))
            .expect("the audit-suppression request is well formed");
        Reply::of(self.router.dispatch(request).await)
    }

    async fn create(&self, body: &Value) -> Reply {
        self.dispatch("/audit/suppressions/create", body).await
    }

    async fn describe(&self, body: &Value) -> Reply {
        self.dispatch("/audit/suppressions/describe", body).await
    }

    /// Create `row` and require it to succeed.
    async fn created(&self, row: &Row) {
        let reply = self.create(&row.body()).await;
        assert_eq!(
            reply.status, 200,
            "case {}: expected CreateAuditSuppression to answer 200, got {} body {}",
            row.case,
            reply.status,
            reply.text()
        );
    }

    /// Read `row`'s suppression back and require it to be the record the create persisted,
    /// answering the reported body.
    async fn read_back(&self, row: &Row) -> Value {
        let reply = self.describe(&row.lookup()).await;
        assert_eq!(
            reply.status, 200,
            "case {}: expected DescribeAuditSuppression to answer 200, got {} body {}",
            row.case,
            reply.status,
            reply.text()
        );
        let reported = reply.json();
        let expiration_date = reported.get("expirationDate").and_then(Value::as_f64);
        assert_eq!(
            expiration_date,
            row.expiration_date.map(|seconds| seconds as f64),
            "case {}: expected the stored expirationDate, got {}",
            row.case,
            reply.text()
        );
        let mut without_timestamp = reported
            .as_object()
            .unwrap_or_else(|| panic!("case {}: expected an object, got {}", row.case, reply.text()))
            .clone();
        without_timestamp.remove("expirationDate");
        assert_eq!(
            Value::Object(without_timestamp),
            row.expected_description(),
            "case {}: expected the created suppression reported member for member, got {}",
            row.case,
            reply.text()
        );
        reported
    }
}

/// A dispatched response, reduced to what the assertions read.
struct Reply {
    status: u16,
    body: Bytes,
}

impl Reply {
    fn of(response: http::Response<Bytes>) -> Self {
        Self {
            status: response.status().as_u16(),
            body: response.into_body(),
        }
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    fn json(&self) -> Value {
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|e| panic!("response body is not JSON ({e}): {}", self.text()))
    }

    /// The error code a restJson1 error response carries.
    fn error_code(&self) -> String {
        self.json()["__type"]
            .as_str()
            .unwrap_or_else(|| panic!("no __type in {}", self.text()))
            .to_string()
    }

    /// The message a restJson1 error response carries.
    fn error_message(&self) -> String {
        self.json()["message"]
            .as_str()
            .unwrap_or_else(|| panic!("no message in {}", self.text()))
            .to_string()
    }
}

/// The table covers every member of the modelled `ResourceIdentifier`, so a member the
/// model gains is a row this suite demands rather than a case nothing exercises.
#[test]
fn the_table_spans_every_resource_identifier_member() {
    let covered: std::collections::BTreeSet<String> = rows()
        .iter()
        .flat_map(|row| {
            row.identifier
                .as_object()
                .into_iter()
                .flat_map(|identifier| identifier.keys().cloned().collect::<Vec<String>>())
        })
        .collect();
    let modeled: std::collections::BTreeSet<String> = RESOURCE_IDENTIFIER_MEMBERS
        .iter()
        .map(|member| (*member).to_string())
        .collect();
    assert_eq!(
        covered, modeled,
        "expected one row per modelled ResourceIdentifier member \
         {RESOURCE_IDENTIFIER_MEMBERS:?}, got {covered:?}"
    );
}

/// Every check the table names is an audit check of this account's catalog, so no row
/// asserts behaviour for a check the service does not have.
#[test]
fn every_check_the_table_names_is_in_the_catalog() {
    for row in rows() {
        assert!(
            AUDIT_CHECK_NAMES.contains(&row.check_name),
            "case {}: expected {} to be one of the catalog's checks {AUDIT_CHECK_NAMES:?}",
            row.case,
            row.check_name
        );
    }
}

/// RULE: a created suppression is read back member for member under the pair that identifies
/// it — whichever member of the identifier names the resource, whichever check it is about,
/// and whether or not it carries an expiry, the indefinite flag or a description.
#[tokio::test]
async fn a_created_suppression_is_read_back_member_for_member() {
    let emulator = Emulator::new();

    for row in rows() {
        emulator.created(&row).await;
        emulator.read_back(&row).await;
    }
}

/// RULE: reading a suppression changes nothing a later caller sees, and the record is the one
/// the create persisted however often it is read.
#[tokio::test]
async fn reading_a_suppression_changes_nothing() {
    let emulator = Emulator::new();

    for row in rows() {
        emulator.created(&row).await;
        let first = emulator.read_back(&row).await;
        let second = emulator.read_back(&row).await;
        assert_eq!(
            first, second,
            "case {}: expected two reads of one suppression to report one record, got {first} \
             then {second}",
            row.case
        );
    }
}

/// RULE: every audit check in the account's catalog is a check a suppression can be created
/// for — the family's condition on `checkName` is the catalog itself — and the check a
/// suppression was created for is the check it reports.
#[tokio::test]
async fn every_audit_check_in_the_catalog_can_be_suppressed() {
    let emulator = Emulator::new();

    for (index, check_name) in AUDIT_CHECK_NAMES.iter().enumerate() {
        let body = json!({
            "checkName": check_name,
            "resourceIdentifier": { "clientId": format!("catalog-device-{index}") },
            "suppressIndefinitely": true,
            "clientRequestToken": format!("catalog-token-{index}"),
        });
        let reply = emulator.create(&body).await;
        assert_eq!(
            reply.status, 200,
            "case {check_name}: expected CreateAuditSuppression to answer 200, got {} body {}",
            reply.status,
            reply.text()
        );

        let lookup = json!({
            "checkName": check_name,
            "resourceIdentifier": { "clientId": format!("catalog-device-{index}") },
        });
        let read = emulator.describe(&lookup).await;
        assert_eq!(
            read.status, 200,
            "case {check_name}: expected DescribeAuditSuppression to answer 200, got {} body {}",
            read.status,
            read.text()
        );
        assert_eq!(
            read.json(),
            json!({
                "checkName": check_name,
                "resourceIdentifier": { "clientId": format!("catalog-device-{index}") },
                "suppressIndefinitely": true,
            }),
            "case {check_name}: expected the created suppression reported back, got {}",
            read.text()
        );
    }
}

/// RULE: a `checkName` outside the catalog of audit checks is refused, and nothing is
/// persisted for it.
#[tokio::test]
async fn a_check_outside_the_catalog_is_refused() {
    let emulator = Emulator::new();

    // Each case is a name the catalog does not hold: a misspelling of one it does, a
    // lower-cased one, the empty name, and a name of another service's vocabulary.
    for check_name in [
        "LOGGING_DISABLED",
        "logging_disabled_check",
        "",
        "DEVICE_CERTIFICATE_EXPIRY_CHECK",
    ] {
        let body = json!({
            "checkName": check_name,
            "resourceIdentifier": { "clientId": "unknown-check-device" },
            "suppressIndefinitely": true,
            "clientRequestToken": "unknown-check-token",
        });
        let reply = emulator.create(&body).await;
        assert_eq!(
            reply.status, 400,
            "case {check_name:?}: expected 400 for a check outside the catalog, got {} body {}",
            reply.status,
            reply.text()
        );
        assert_eq!(
            reply.error_code(),
            "InvalidRequestException",
            "case {check_name:?}: expected InvalidRequestException, got {}",
            reply.text()
        );
        assert!(
            reply.error_message().contains(check_name),
            "case {check_name:?}: expected the refusal to name the check, got {:?}",
            reply.error_message()
        );
    }

    // Nothing was persisted: the catalog check the cases are about holds no suppression.
    let read = emulator
        .describe(&json!({
            "checkName": "LOGGING_DISABLED_CHECK",
            "resourceIdentifier": { "clientId": "unknown-check-device" },
        }))
        .await;
    assert_eq!(
        read.status, 404,
        "expected the refused creates to persist nothing, got {} body {}",
        read.status,
        read.text()
    );
}

/// RULE: a `resourceIdentifier` that identifies no resource is refused, and nothing is
/// persisted for it.
#[tokio::test]
async fn a_resource_identifier_naming_nothing_is_refused() {
    let emulator = Emulator::new();

    // Each case identifies nothing: an identifier with no member at all, and ones whose
    // only members are nested identifiers that name nothing themselves.
    let cases: Vec<(&str, Value)> = vec![
        ("no-member", json!({})),
        (
            "empty-policy-version",
            json!({ "policyVersionIdentifier": {} }),
        ),
        (
            "empty-nested-identifiers",
            json!({
                "policyVersionIdentifier": {},
                "issuerCertificateIdentifier": {},
            }),
        ),
    ];

    for (case, identifier) in cases {
        let body = json!({
            "checkName": "LOGGING_DISABLED_CHECK",
            "resourceIdentifier": identifier,
            "suppressIndefinitely": true,
            "clientRequestToken": format!("unidentified-{case}"),
        });
        let reply = emulator.create(&body).await;
        assert_eq!(
            reply.status, 400,
            "case {case}: expected 400 for an identifier naming no resource, got {} body {}",
            reply.status,
            reply.text()
        );
        assert_eq!(
            reply.error_code(),
            "InvalidRequestException",
            "case {case}: expected InvalidRequestException, got {}",
            reply.text()
        );
        assert!(
            reply.error_message().contains("resourceIdentifier"),
            "case {case}: expected the refusal to name the member, got {:?}",
            reply.error_message()
        );

        // The same pair is refused to the reader for the same reason, rather than reported
        // as a record that is merely absent.
        let read = emulator
            .describe(&json!({
                "checkName": "LOGGING_DISABLED_CHECK",
                "resourceIdentifier": identifier,
            }))
            .await;
        assert_eq!(
            read.status, 400,
            "case {case}: expected the reader to refuse the same identifier, got {} body {}",
            read.status,
            read.text()
        );
    }
}

/// RULE: the pair is the identity, so a second create for a pair the account already holds a
/// suppression for is a conflict and leaves the stored record exactly as it was.
#[tokio::test]
async fn a_second_create_for_one_pair_is_refused_and_changes_nothing() {
    let emulator = Emulator::new();

    for row in rows() {
        emulator.created(&row).await;
        let stored = emulator.read_back(&row).await;

        // The repeat differs in every member BUT the pair — a different expiry, a different
        // description and a token of its own — so what is refused is the identity and not
        // the rest of the request.
        let mut repeat = Map::new();
        repeat.insert("checkName".to_string(), json!(row.check_name));
        repeat.insert("resourceIdentifier".to_string(), row.identifier.clone());
        repeat.insert("suppressIndefinitely".to_string(), json!(true));
        repeat.insert(
            "description".to_string(),
            json!("a second create of the same pair"),
        );
        repeat.insert(
            "clientRequestToken".to_string(),
            json!(format!("repeat-{}", row.case)),
        );
        let reply = emulator.create(&Value::Object(repeat)).await;
        assert_eq!(
            reply.status, 409,
            "case {}: expected 409 for a pair already suppressed, got {} body {}",
            row.case,
            reply.status,
            reply.text()
        );
        assert_eq!(
            reply.error_code(),
            "ResourceAlreadyExistsException",
            "case {}: expected ResourceAlreadyExistsException, got {}",
            row.case,
            reply.text()
        );
        assert_eq!(
            reply.error_message(),
            already_exists_message(row.check_name),
            "case {}: expected this family's already-exists sentence, got {:?}",
            row.case,
            reply.error_message()
        );
        assert_eq!(
            emulator.read_back(&row).await,
            stored,
            "case {}: expected the refused create to leave the stored record alone",
            row.case
        );
    }
}

/// RULE: a suppression is identified by the PAIR, so one check over two different resources
/// is two suppressions, each reporting its own record.
#[tokio::test]
async fn one_check_suppresses_each_resource_separately() {
    let emulator = Emulator::new();
    let check_name = "CONFLICTING_CLIENT_IDS_CHECK";

    // The two identifiers differ in ONE part: the same member with another value, and the
    // same value under another member — so neither pair is the other.
    let cases: Vec<(&str, Value, &str)> = vec![
        ("first-client", json!({ "clientId": "device-a" }), "the first"),
        (
            "second-client",
            json!({ "clientId": "device-b" }),
            "the second",
        ),
        (
            "same-value-other-member",
            json!({ "cognitoIdentityPoolId": "device-b" }),
            "the third",
        ),
    ];

    for (case, identifier, description) in &cases {
        let body = json!({
            "checkName": check_name,
            "resourceIdentifier": identifier.clone(),
            "suppressIndefinitely": true,
            "description": description,
            "clientRequestToken": format!("separate-{case}"),
        });
        let reply = emulator.create(&body).await;
        assert_eq!(
            reply.status, 200,
            "case {case}: expected CreateAuditSuppression to answer 200, got {} body {}",
            reply.status,
            reply.text()
        );
    }

    for (case, identifier, description) in &cases {
        let read = emulator
            .describe(&json!({
                "checkName": check_name,
                "resourceIdentifier": identifier.clone(),
            }))
            .await;
        assert_eq!(
            read.status, 200,
            "case {case}: expected DescribeAuditSuppression to answer 200, got {} body {}",
            read.status,
            read.text()
        );
        assert_eq!(
            read.json(),
            json!({
                "checkName": check_name,
                "resourceIdentifier": identifier.clone(),
                "suppressIndefinitely": true,
                "description": description,
            }),
            "case {case}: expected this resource's own suppression, got {}",
            read.text()
        );
    }
}

/// RULE: "each audit supression must have a unique client request token" — a create under a
/// token the account already holds a suppression under is refused, whichever pair it is
/// about.
#[tokio::test]
async fn a_repeated_client_request_token_is_refused() {
    let emulator = Emulator::new();
    let token = "one-token-per-suppression";

    let first = json!({
        "checkName": "LOGGING_DISABLED_CHECK",
        "resourceIdentifier": { "account": "000000000000" },
        "suppressIndefinitely": true,
        "clientRequestToken": token,
    });
    let reply = emulator.create(&first).await;
    assert_eq!(
        reply.status, 200,
        "case first: expected CreateAuditSuppression to answer 200, got {} body {}",
        reply.status,
        reply.text()
    );

    // Each case is a DIFFERENT pair under the same token: another resource of the same
    // check, and another check entirely.
    let cases: Vec<(&str, Value)> = vec![
        (
            "other-resource",
            json!({
                "checkName": "LOGGING_DISABLED_CHECK",
                "resourceIdentifier": { "clientId": "token-reuse-device" },
                "suppressIndefinitely": true,
                "clientRequestToken": token,
            }),
        ),
        (
            "other-check",
            json!({
                "checkName": "DEVICE_CERTIFICATE_SHARED_CHECK",
                "resourceIdentifier": { "deviceCertificateId": DEVICE_CERTIFICATE_ID },
                "suppressIndefinitely": true,
                "clientRequestToken": token,
            }),
        ),
    ];

    for (case, body) in cases {
        let reply = emulator.create(&body).await;
        assert_eq!(
            reply.status, 409,
            "case {case}: expected 409 for a token already used, got {} body {}",
            reply.status,
            reply.text()
        );
        assert_eq!(
            reply.error_code(),
            "ResourceAlreadyExistsException",
            "case {case}: expected ResourceAlreadyExistsException, got {}",
            reply.text()
        );
        assert_eq!(
            reply.error_message(),
            token_already_exists_message(token),
            "case {case}: expected this family's repeated-token sentence, got {:?}",
            reply.error_message()
        );
    }
}

/// RULE: a pair the account holds no suppression for is a not-found to the reader, never a
/// hollow 200, in this family's own sentence.
#[tokio::test]
async fn a_pair_with_no_suppression_is_not_found() {
    let emulator = Emulator::new();

    // One suppression exists; each case asks for a pair that is not it — the same check over
    // another resource, another check over the same resource, and a pair nothing created.
    let created = json!({
        "checkName": "LOGGING_DISABLED_CHECK",
        "resourceIdentifier": { "account": "000000000000" },
        "suppressIndefinitely": true,
        "clientRequestToken": "not-found-setup",
    });
    let reply = emulator.create(&created).await;
    assert_eq!(
        reply.status, 200,
        "case setup: expected CreateAuditSuppression to answer 200, got {} body {}",
        reply.status,
        reply.text()
    );

    let cases: Vec<(&str, &str, Value)> = vec![
        (
            "other-resource",
            "LOGGING_DISABLED_CHECK",
            json!({ "account": "000000000001" }),
        ),
        (
            "other-check",
            "DEVICE_CERTIFICATE_AGE_CHECK",
            json!({ "account": "000000000000" }),
        ),
        (
            "nothing-created",
            "REVOKED_CA_CERTIFICATE_STILL_ACTIVE_CHECK",
            json!({ "caCertificateId": CA_CERTIFICATE_ID }),
        ),
    ];

    for (case, check_name, identifier) in cases {
        let read = emulator
            .describe(&json!({
                "checkName": check_name,
                "resourceIdentifier": identifier,
            }))
            .await;
        assert_eq!(
            read.status, 404,
            "case {case}: expected 404 for a pair holding no suppression, got {} body {}",
            read.status,
            read.text()
        );
        assert_eq!(
            read.error_code(),
            "ResourceNotFoundException",
            "case {case}: expected ResourceNotFoundException, got {}",
            read.text()
        );
        assert_eq!(
            read.error_message(),
            not_found_message(check_name),
            "case {case}: expected this family's not-found sentence, got {:?}",
            read.error_message()
        );
    }
}

/// RULE: the store is bounded — past the ceiling the create is refused rather than growing
/// it, and every suppression already recorded is still readable.
#[tokio::test]
async fn the_store_is_bounded() {
    let emulator = Emulator::new();
    let check_name = "CONFLICTING_CLIENT_IDS_CHECK";

    for index in 0..MAX_AUDIT_SUPPRESSIONS {
        let body = json!({
            "checkName": check_name,
            "resourceIdentifier": { "clientId": format!("bounded-device-{index}") },
            "suppressIndefinitely": true,
            "clientRequestToken": format!("bounded-token-{index}"),
        });
        let reply = emulator.create(&body).await;
        assert_eq!(
            reply.status, 200,
            "case {index}: expected CreateAuditSuppression to answer 200 below the ceiling, \
             got {} body {}",
            reply.status,
            reply.text()
        );
    }

    let body = json!({
        "checkName": check_name,
        "resourceIdentifier": { "clientId": "one-too-many" },
        "suppressIndefinitely": true,
        "clientRequestToken": "one-too-many-token",
    });
    let reply = emulator.create(&body).await;
    assert_eq!(
        reply.status, 410,
        "case past the ceiling: expected 410, got {} body {}",
        reply.status,
        reply.text()
    );
    assert_eq!(
        reply.error_code(),
        "LimitExceededException",
        "case past the ceiling: expected LimitExceededException, got {}",
        reply.text()
    );

    // The refused create persisted nothing, and the suppression recorded first is still
    // the record it was.
    let refused = emulator
        .describe(&json!({
            "checkName": check_name,
            "resourceIdentifier": { "clientId": "one-too-many" },
        }))
        .await;
    assert_eq!(
        refused.status, 404,
        "case past the ceiling: expected the refused create to persist nothing, got {} body {}",
        refused.status,
        refused.text()
    );
    let first = emulator
        .describe(&json!({
            "checkName": check_name,
            "resourceIdentifier": { "clientId": "bounded-device-0" },
        }))
        .await;
    assert_eq!(
        first.status, 200,
        "case past the ceiling: expected the first suppression still readable, got {} body {}",
        first.status,
        first.text()
    );
}
