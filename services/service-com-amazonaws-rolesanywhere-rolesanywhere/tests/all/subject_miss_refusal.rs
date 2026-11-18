// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for the body of the modelled `ResourceNotFoundException` the
//! SUBJECT read path answers with.
//!
//! The rule under test: which members a not-found refusal body carries is decided per
//! operation by what this service is recorded answering that path with, not by one shared
//! message helper — and the subject read path carries NO `message` member at all
//! (`ResourceNotFoundException$message` is optional:
//! `smithy_query(query="shape", shape="ResourceNotFoundException")`), so the error class in
//! the response's `x-amzn-errortype` header is the whole of the refusal and nothing in the
//! body names the audit record that was never written. The paths this service IS recorded
//! wording a miss on keep their wording, which the last test here pins so the rule stays
//! the narrow one.
//!
//! Each table's row differs along what the rule generalises over: WHICH id fails to resolve
//! and why (never minted, an id minted for each other resource type this service holds),
//! the Region the request is signed for, and how much the store holds when the refusal is
//! answered (nothing at all, or every other resource type).
//!
//! The harness below is local to this suite, as in each sibling suite.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// A PEM bundle holding one real certificate authority: a self-signed CA certificate,
/// valid 2020-2100, written with `rcgen` and kept beside the suites that need one.
/// `CreateTrustAnchor` refuses certificate data it cannot read as PEM X.509 certificates,
/// so a suite that creates a trust anchor presents an authority a trust anchor can hold.
const CERTIFICATE_BUNDLE_PEM: &str = include_str!("../data/certificate_authority_a.pem");

/// A placeholder PEM a CRL import carries.
const CRL_PEM: &str = "-----BEGIN X509 CRL-----\nZW11bGF0ZWQtdGVzdC1jcmw=\n-----END X509 CRL-----\n";

/// The Region the suite signs for unless a case varies it.
const REGION: &str = "us-west-2";

/// The account every emulated ARN carries (`engine::emulator_account_id`).
const ACCOUNT: &str = "000000000000";

/// Ids shaped like the modelled `Uuid` — `@length(36, 36)`,
/// `@pattern("[a-f0-9]{8}-([a-z0-9]{4}-){3}[a-z0-9]{12}")`
/// (`smithy_query(query="operation", operation="GetSubject")`) — that nothing is ever
/// recorded under, so the boundary validator accepts them and the handler has to resolve
/// them.
const UNMINTED_IDS: [(&str, &str); 4] = [
    ("all-zero-id", "00000000-0000-0000-0000-000000000000"),
    ("hex-words-id", "deadbeef-dead-beef-dead-beefdeadbeef"),
    ("all-f-id", "ffffffff-ffff-ffff-ffff-ffffffffffff"),
    ("mixed-id", "4a84382e-363e-4b06-9067-216a0c1c0638"),
];

/// The Regions a case signs for besides [`REGION`].
const OTHER_REGIONS: [&str; 3] = ["eu-central-1", "ap-southeast-2", "us-east-1"];

struct Client {
    router: MultiServiceRouter,
}

/// One HTTP answer: the status, the parsed JSON body and the wire error code.
#[derive(Debug)]
struct Answer {
    status: u16,
    body: Value,
    /// The raw body bytes as text, so a test can assert what the refusal does NOT disclose.
    body_text: String,
    error_code: String,
}

impl Answer {
    fn message(&self) -> Option<&str> {
        self.body.get("message").and_then(Value::as_str)
    }

    /// Whether the body carries a `message` member at all, under any value.
    fn carries_message_member(&self) -> bool {
        self.body.get("message").is_some()
    }
}

impl Client {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(
            service_com_amazonaws_rolesanywhere_rolesanywhere::EmulatedService::new(engine),
        );
        Self { router }
    }

    async fn send_in_region(
        &self,
        method: &str,
        uri: &str,
        body: Option<Value>,
        region: &str,
    ) -> Answer {
        let payload = body
            .map(|value| bytes::Bytes::from(serde_json::to_vec(&value).expect("serialize body")))
            .unwrap_or_default();
        let request = http::Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/json")
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDTEST/20260101/{region}/rolesanywhere/aws4_request, \
                     SignedHeaders=host, Signature=deadbeef"
                ),
            )
            .body(payload)
            .expect("build request");

        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let header_code = response
            .headers()
            .get("x-amzn-errortype")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .rsplit(['#', '/'])
            .next()
            .unwrap_or_default()
            .to_string();
        let bytes = response.into_body();
        let body_text = String::from_utf8_lossy(&bytes).to_string();
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        let error_code = if header_code.is_empty() {
            body.get("__type")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .rsplit(['#', '/'])
                .next()
                .unwrap_or_default()
                .to_string()
        } else {
            header_code
        };
        Answer {
            status,
            body,
            body_text,
            error_code,
        }
    }

    async fn send(&self, method: &str, uri: &str, body: Option<Value>) -> Answer {
        self.send_in_region(method, uri, body, REGION).await
    }

    async fn get_subject_in_region(&self, id: &str, region: &str) -> Answer {
        self.send_in_region("GET", &format!("/subject/{id}"), None, region)
            .await
    }

    async fn get_profile_in_region(&self, id: &str, region: &str) -> Answer {
        self.send_in_region("GET", &format!("/profile/{id}"), None, region)
            .await
    }

    async fn get_trust_anchor_in_region(&self, id: &str, region: &str) -> Answer {
        self.send_in_region("GET", &format!("/trustanchor/{id}"), None, region)
            .await
    }

    async fn create_profile(&self, name: &str) -> Answer {
        self.send(
            "POST",
            "/profiles",
            Some(json!({
                "name": name,
                "roleArns": [format!("arn:aws:iam::{ACCOUNT}:role/DemoRole")],
            })),
        )
        .await
    }

    async fn create_trust_anchor(&self, name: &str) -> Answer {
        self.send(
            "POST",
            "/trustanchors",
            Some(json!({
                "name": name,
                "source": {
                    "sourceType": "CERTIFICATE_BUNDLE",
                    "sourceData": {"x509CertificateData": CERTIFICATE_BUNDLE_PEM},
                },
            })),
        )
        .await
    }

    async fn import_crl(&self, name: &str, trust_anchor_arn: &str) -> Answer {
        self.send(
            "POST",
            "/crls",
            Some(json!({
                "name": name,
                "crlData": aws_smithy_types::base64::encode(CRL_PEM.as_bytes()),
                "trustAnchorArn": trust_anchor_arn,
            })),
        )
        .await
    }
}

/// The wording an ARN-keyed miss carries, which the profile read path is recorded answering
/// with and the subject read path is not.
fn arn_not_found_message(resource_arn: &str) -> String {
    format!("The item doesn't exist. arn: {resource_arn} ; aborting.")
}

/// The ARN a profile id carries in `region`.
fn profile_arn(region: &str, profile_id: &str) -> String {
    format!("arn:aws:rolesanywhere:{region}:{ACCOUNT}:profile/{profile_id}")
}

/// The wholly fixed wording a trust anchor addressed by `trustAnchorId` misses with.
const TRUST_ANCHOR_NOT_FOUND_MESSAGE: &str = "TrustAnchor not found";

/// Create a profile and answer its id.
async fn profile_id(client: &Client, name: &str) -> String {
    let created = client.create_profile(name).await;
    assert_eq!(
        created.status, 201,
        "profile {name:?}: expected CreateProfile 201, got {created:?}"
    );
    created
        .body
        .pointer("/profile/profileId")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("profile {name:?}: no profileId in {created:?}"))
        .to_string()
}

/// Create a trust anchor and answer `(id, arn)`.
async fn trust_anchor(client: &Client, name: &str) -> (String, String) {
    let created = client.create_trust_anchor(name).await;
    assert_eq!(
        created.status, 201,
        "trust anchor {name:?}: expected CreateTrustAnchor 201, got {created:?}"
    );
    let id = created
        .body
        .pointer("/trustAnchor/trustAnchorId")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("trust anchor {name:?}: no trustAnchorId in {created:?}"))
        .to_string();
    let arn = created
        .body
        .pointer("/trustAnchor/trustAnchorArn")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("trust anchor {name:?}: no trustAnchorArn in {created:?}"))
        .to_string();
    (id, arn)
}

/// Import a CRL and answer its id.
async fn crl_id(client: &Client, name: &str, trust_anchor_arn: &str) -> String {
    let imported = client.import_crl(name, trust_anchor_arn).await;
    assert_eq!(
        imported.status, 201,
        "crl {name:?}: expected ImportCrl 201, got {imported:?}"
    );
    imported
        .body
        .pointer("/crl/crlId")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("crl {name:?}: no crlId in {imported:?}"))
        .to_string()
}

/// Assert one subject refusal is the modelled 404 whose body carries no `message` member and
/// discloses nothing about the id that was asked for.
fn assert_miss_without_message(case: &str, id: &str, region: &str, answer: &Answer) {
    assert_eq!(
        answer.status, 404,
        "case {case:?}: expected status 404, got {answer:?}"
    );
    assert_eq!(
        answer.error_code, "ResourceNotFoundException",
        "case {case:?}: expected ResourceNotFoundException, got {:?}",
        answer.error_code
    );
    assert!(
        !answer.carries_message_member(),
        "case {case:?}: the subject miss carries no message member, got {:?}",
        answer.message()
    );
    assert!(
        !answer.body_text.contains(id),
        "case {case:?}: the refusal must not name the requested id, got body {:?}",
        answer.body_text
    );
    assert!(
        !answer.body_text.contains("arn:") && !answer.body_text.contains(region),
        "case {case:?}: the refusal must compose no ARN for a subject that does not exist, \
         got body {:?}",
        answer.body_text
    );
}

/// Every id the subject store does not hold is refused 404 with a body that carries no
/// `message` member, whatever the id is, whichever Region the request is signed for, and
/// however much the rest of the service holds.
#[tokio::test]
async fn subject_misses_refuse_with_no_message_member() {
    // Row dimension: how much the store holds. An empty engine, and one holding one of
    // every other resource type this service keeps — so a miss is answered the same way
    // whether or not the account has any resources at all.
    for populated in [false, true] {
        let client = Client::new();
        let mut cases: Vec<(String, String, &str)> = Vec::new();
        for (label, id) in UNMINTED_IDS {
            cases.push((format!("unminted-{label}"), id.to_string(), REGION));
        }
        for region in OTHER_REGIONS {
            cases.push((
                format!("unminted-in-{region}"),
                UNMINTED_IDS[1].1.to_string(),
                region,
            ));
        }
        if populated {
            // Ids that WERE minted, for another resource type: a subject is a different
            // resource, so none of them resolves on this path either.
            let profile = profile_id(&client, "sibling-profile").await;
            let (anchor, anchor_arn) = trust_anchor(&client, "sibling-anchor").await;
            let crl = crl_id(&client, "sibling-crl", &anchor_arn).await;
            cases.push(("profile-id-as-subject-id".to_string(), profile, REGION));
            cases.push(("trust-anchor-id-as-subject-id".to_string(), anchor, REGION));
            cases.push(("crl-id-as-subject-id".to_string(), crl, REGION));
        }

        let store = if populated { "populated" } else { "empty" };
        for (case, id, region) in cases {
            let labelled = format!("GetSubject/{store}-store/{case}");
            let answer = client.get_subject_in_region(&id, region).await;
            assert_miss_without_message(&labelled, &id, region, &answer);

            // A read reports: the refused read recorded no subject under the id it asked
            // for, so asking again is refused exactly the same way.
            let again = client.get_subject_in_region(&id, region).await;
            assert_miss_without_message(&format!("{labelled}/again"), &id, region, &again);
        }
    }
}

/// The empty refusal body belongs to the subject path alone: the paths this service is
/// recorded wording a miss on still carry their own wording, so the rule stays per
/// operation rather than becoming "no not-found refusal carries a message".
#[tokio::test]
async fn the_recorded_wording_of_other_miss_paths_is_untouched() {
    let client = Client::new();

    for (label, id) in UNMINTED_IDS {
        for region in [REGION, OTHER_REGIONS[0]] {
            let profile = client.get_profile_in_region(id, region).await;
            let expected = arn_not_found_message(&profile_arn(region, id));
            let case = format!("GetProfile/{label}/{region}");
            assert_eq!(
                profile.status, 404,
                "case {case:?}: expected status 404, got {profile:?}"
            );
            assert_eq!(
                profile.message(),
                Some(expected.as_str()),
                "case {case:?}: expected message {expected:?}, got {:?}",
                profile.message()
            );

            let anchor = client.get_trust_anchor_in_region(id, region).await;
            let case = format!("GetTrustAnchor/{label}/{region}");
            assert_eq!(
                anchor.status, 404,
                "case {case:?}: expected status 404, got {anchor:?}"
            );
            assert_eq!(
                anchor.message(),
                Some(TRUST_ANCHOR_NOT_FOUND_MESSAGE),
                "case {case:?}: expected message {TRUST_ANCHOR_NOT_FOUND_MESSAGE:?}, got {:?}",
                anchor.message()
            );
        }
    }
}
