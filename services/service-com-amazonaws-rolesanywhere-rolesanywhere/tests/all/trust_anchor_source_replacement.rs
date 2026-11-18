// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for the certificate source `UpdateTrustAnchor` writes.
//!
//! The rule under test: an update writes the members its request carries onto the
//! addressed trust anchor and nothing else — a submitted `source` replaces the stored
//! one whatever `sourceType` either names, every member the request did not carry
//! (the id, the ARN, `enabled`, the notification settings, the tags, `createdAt`)
//! keeps the value the record held, and a submitted `source` is judged exactly as the
//! create path judges one, before anything is written, so a source that describes no
//! certificate authority leaves the stored record untouched.
//!
//! A source describes no certificate authority when its members do not pair up the way the
//! model pairs them AND when the certificate data it carries is not certificate data:
//! `x509CertificateData` is "The PEM-encoded data for the certificate anchor"
//! (`smithy_query(query="shape", shape="SourceData")`), so a value that does not read as
//! PEM X.509 certificates is refused 400 `ValidationException` by both writes — a trust
//! anchor holding it could authenticate nothing — and nothing is stored.
//!
//! Each test therefore spans a TABLE of inputs — which `sourceType` the stored anchor
//! carries and which the update names, and each way a source can fail to describe a
//! certificate authority — rather than one recorded request.
//!
//! The harness below is local to this suite: each sibling suite's client is private to
//! its own module, and the targets are compiled into one test binary only as separate
//! modules.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// A PEM bundle holding one real certificate authority: a self-signed CA certificate,
/// valid 2020-2100, written with `rcgen` and kept beside the suites that need one.
/// `CreateTrustAnchor` refuses certificate data it cannot read as PEM X.509 certificates,
/// so a suite that creates a trust anchor presents an authority a trust anchor can hold.
const CERTIFICATE_BUNDLE_PEM: &str = include_str!("../data/certificate_authority_a.pem");

/// A second, different authority, so replacing one bundle with another is observable.
const OTHER_CERTIFICATE_BUNDLE_PEM: &str = include_str!("../data/certificate_authority_b.pem");

/// The Region the suite signs for.
const REGION: &str = "us-west-2";

/// Every value the modelled `TrustAnchorType` enum accepts, in the order the model lists
/// them (`smithy_query(query="shape", shape="TrustAnchorType")`). The wording of the
/// service's enum refusal lists them in this order.
const TRUST_ANCHOR_TYPES: [&str; 3] = ["AWS_ACM_PCA", "CERTIFICATE_BUNDLE", "SELF_SIGNED_REPOSITORY"];

/// A client over one emulator: one engine, one mounted service, so every request in a
/// test observes the state the previous one wrote.
struct Client {
    router: MultiServiceRouter,
}

/// One HTTP answer: the status, the parsed JSON body (`Value::Null` for an empty body)
/// and the wire error code when the answer is a failure.
#[derive(Debug)]
struct Answer {
    status: u16,
    body: Value,
    error_code: String,
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

    async fn send(&self, method: &str, uri: &str, body: Option<Value>) -> Answer {
        let payload = body
            .map(|value| bytes::Bytes::from(serde_json::to_vec(&value).expect("serialize body")))
            .unwrap_or_default();
        let request = http::Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/json")
            // The SigV4 credential scope is how the router resolves the service and how
            // the handler learns the request's Region.
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDTEST/20260101/{REGION}/rolesanywhere/aws4_request, \
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
            error_code,
        }
    }

    async fn create_trust_anchor(&self, body: Value) -> Answer {
        self.send("POST", "/trustanchors", Some(body)).await
    }

    async fn get_trust_anchor(&self, id: &str) -> Answer {
        self.send("GET", &format!("/trustanchor/{id}"), None).await
    }

    async fn update_trust_anchor(&self, id: &str, body: Value) -> Answer {
        self.send("PATCH", &format!("/trustanchor/{id}"), Some(body))
            .await
    }

    async fn tag_resource(&self, resource_arn: &str, tags: Value) -> Answer {
        self.send(
            "POST",
            "/TagResource",
            Some(json!({"resourceArn": resource_arn, "tags": tags})),
        )
        .await
    }

    async fn list_tags(&self, resource_arn: &str) -> Answer {
        self.send(
            "GET",
            &format!("/ListTagsForResource?resourceArn={resource_arn}"),
            None,
        )
        .await
    }
}

/// The ARN of a Private CA, in the form `SourceData$acmPcaArn` names for a trust anchor
/// of type `AWS_ACM_PCA`. The suffix distinguishes one from another.
fn acm_pca_arn(suffix: &str) -> String {
    format!("arn:aws:acm-pca:{REGION}:000000000000:certificate-authority/{suffix}")
}

/// A `Source` of this type carrying the certificate data member the model ties to it:
/// `x509CertificateData` is "Included for trust anchors of type `CERTIFICATE_BUNDLE`"
/// and `acmPcaArn` "Included for trust anchors of type `AWS_ACM_PCA`"
/// (`smithy_query(query="shape", shape="SourceData")`); the model ties neither member to
/// `SELF_SIGNED_REPOSITORY`, so a bundle stands in there.
fn source(source_type: &str, data: &str) -> Value {
    let source_data = if source_type == "AWS_ACM_PCA" {
        json!({"acmPcaArn": data})
    } else {
        json!({"x509CertificateData": data})
    };
    json!({"sourceType": source_type, "sourceData": source_data})
}

/// The `trustAnchor` a successful call answered with.
fn anchor_of(case: &str, expected_status: u16, answer: &Answer) -> Value {
    assert_eq!(
        answer.status, expected_status,
        "case {case:?}: expected HTTP {expected_status}, got {answer:?}"
    );
    answer
        .body
        .get("trustAnchor")
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: no trustAnchor in {answer:?}"))
}

/// The id of a trust anchor a call answered with.
fn id_of(case: &str, anchor: &Value) -> String {
    anchor
        .get("trustAnchorId")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: no trustAnchorId in {anchor:?}"))
        .to_string()
}

/// The ARN of a trust anchor a call answered with.
fn arn_of(case: &str, anchor: &Value) -> String {
    anchor
        .get("trustAnchorArn")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: no trustAnchorArn in {anchor:?}"))
        .to_string()
}

/// The `message` an answer carries.
fn message_of(answer: &Answer) -> String {
    answer
        .body
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// How this service words a `ValidationException` about one member that failed one
/// constraint, where the member carried no value to quote.
///
/// The sentence frame ("N validation error(s) detected: Value at '<member>' failed to
/// satisfy constraint: ...") is the one the boundary validator words its own refusals
/// with, so a refusal a handler raises reads identically to one raised before the
/// handler ran. The CONSTRAINT clause of a type/data pairing names the member the model
/// ties to the submitted `sourceType`, which is what makes each case below a different
/// expectation rather than a repeated literal: the point of asserting it is that the
/// update path words the refusal the way the create path already does.
fn validation_message(member: &str, constraint: &str) -> String {
    format!(
        "1 validation error detected: Value at '{member}' failed to satisfy constraint: {constraint}"
    )
}

/// The same wording for a member that DID carry a value, which the refusal quotes.
fn validation_message_for(value: &str, member: &str, constraint: &str) -> String {
    format!(
        "1 validation error detected: Value '{value}' at '{member}' failed to satisfy \
         constraint: {constraint}"
    )
}

/// How a refusal lists the values `TrustAnchorType` accepts.
fn enum_value_set_constraint() -> String {
    format!(
        "Member must satisfy enum value set: [{}]",
        TRUST_ANCHOR_TYPES.join(", ")
    )
}

/// An update's submitted `source` replaces the stored one for every pairing of modelled
/// `sourceType`s, and the members the request did not carry are left exactly as they
/// were — including the tags the anchor was carrying, which no trust-anchor projection
/// reports and which therefore have to be read back through `ListTagsForResource`.
///
/// The rows differ along what the rule generalises over: the stored `sourceType` and the
/// submitted one (every ordered pair over the modelled enum, so replacing a type with
/// itself and with each other type are both covered) and the certificate data each
/// carries.
#[tokio::test]
async fn update_trust_anchor_replaces_the_stored_source_for_every_modelled_source_type() {
    let client = Client::new();
    for stored_type in TRUST_ANCHOR_TYPES {
        for submitted_type in TRUST_ANCHOR_TYPES {
            let case = format!("{stored_type}-to-{submitted_type}");
            let case = case.as_str();

            let stored_source = if stored_type == "AWS_ACM_PCA" {
                source(stored_type, &acm_pca_arn("stored-ca"))
            } else {
                source(stored_type, CERTIFICATE_BUNDLE_PEM)
            };
            let submitted_source = if submitted_type == "AWS_ACM_PCA" {
                source(submitted_type, &acm_pca_arn("submitted-ca"))
            } else {
                source(submitted_type, OTHER_CERTIFICATE_BUNDLE_PEM)
            };

            let created = anchor_of(
                case,
                201,
                &client
                    .create_trust_anchor(json!({
                        "name": "source replacement anchor",
                        "source": stored_source.clone(),
                    }))
                    .await,
            );
            let id = id_of(case, &created);
            let arn = arn_of(case, &created);
            let tags = json!([{"key": "owner", "value": "platform"}]);
            let tagged = client.tag_resource(&arn, tags.clone()).await;
            assert_eq!(
                tagged.status, 201,
                "case {case:?}: expected TagResource 201, got {tagged:?}"
            );

            // The source the create stored is what a read reports before the update, so
            // the assertions below are about the update and not about the create.
            let before = anchor_of(case, 200, &client.get_trust_anchor(&id).await);
            assert_eq!(
                before.get("source"),
                Some(&stored_source),
                "case {case:?}: the stored source must be the one the create carried, got \
                 {before:?}"
            );

            let updated = anchor_of(
                case,
                200,
                &client
                    .update_trust_anchor(&id, json!({"source": submitted_source.clone()}))
                    .await,
            );
            let read_back = anchor_of(case, 200, &client.get_trust_anchor(&id).await);

            for (path, anchor) in [("UpdateTrustAnchor", &updated), ("GetTrustAnchor", &read_back)] {
                assert_eq!(
                    anchor.get("source"),
                    Some(&submitted_source),
                    "case {case:?}: {path} must report the source the update wrote, got \
                     {anchor:?}"
                );
                // Every member the request did not carry keeps the value it had.
                for member in [
                    "trustAnchorId",
                    "trustAnchorArn",
                    "name",
                    "enabled",
                    "createdAt",
                    "notificationSettings",
                ] {
                    assert_eq!(
                        anchor.get(member),
                        before.get(member),
                        "case {case:?}: {path} must leave {member} as the record held it \
                         ({:?}), got {:?}",
                        before.get(member),
                        anchor.get(member)
                    );
                }
            }

            let listed = client.list_tags(&arn).await;
            assert_eq!(
                listed.status, 200,
                "case {case:?}: expected ListTagsForResource 200, got {listed:?}"
            );
            assert_eq!(
                listed.body.get("tags"),
                Some(&tags),
                "case {case:?}: the update must leave the anchor's tags alone, got {listed:?}"
            );
        }
    }
}

/// A `source` that describes no certificate authority is refused as the operation's
/// modelled `ValidationException` (400), and the stored record is left untouched: the
/// anchor keeps the source AND the name it had, even when the refused request carried a
/// new name too.
///
/// The rows differ along what the rule generalises over: which part of the source is
/// missing, unknown or paired with the wrong certificate data — covering a `sourceType`
/// the enum does not list and, for each type the model ties to a data member, the data
/// member of the other one.
#[tokio::test]
async fn an_update_whose_source_describes_no_certificate_authority_stores_nothing() {
    let stored_name = "unchanged anchor";
    let cases: Vec<(&str, Value, String)> = vec![
        (
            "no-source-type",
            json!({"sourceData": {"x509CertificateData": OTHER_CERTIFICATE_BUNDLE_PEM}}),
            validation_message("source.sourceType", "Member must not be null"),
        ),
        (
            "empty-source-type",
            json!({"sourceType": "", "sourceData": {"acmPcaArn": acm_pca_arn("ca")}}),
            validation_message_for("", "source.sourceType", &enum_value_set_constraint()),
        ),
        (
            "unknown-source-type",
            json!({"sourceType": "CERTIFICATE_CHAIN",
                   "sourceData": {"x509CertificateData": OTHER_CERTIFICATE_BUNDLE_PEM}}),
            validation_message_for(
                "CERTIFICATE_CHAIN",
                "source.sourceType",
                &enum_value_set_constraint(),
            ),
        ),
        (
            "no-source-data",
            json!({"sourceType": "CERTIFICATE_BUNDLE"}),
            validation_message("source.sourceData", "Member must not be null"),
        ),
        ("empty-source", json!({}),
            validation_message("source.sourceType", "Member must not be null")),
        (
            "bundle-type-with-pca-data",
            json!({"sourceType": "CERTIFICATE_BUNDLE",
                   "sourceData": {"acmPcaArn": acm_pca_arn("ca")}}),
            validation_message(
                "source.sourceData",
                "Member must be x509CertificateData for a trust anchor of type \
                 CERTIFICATE_BUNDLE",
            ),
        ),
        (
            "pca-type-with-bundle-data",
            json!({"sourceType": "AWS_ACM_PCA",
                   "sourceData": {"x509CertificateData": OTHER_CERTIFICATE_BUNDLE_PEM}}),
            validation_message(
                "source.sourceData",
                "Member must be acmPcaArn for a trust anchor of type AWS_ACM_PCA",
            ),
        ),
    ];

    let client = Client::new();
    let created = anchor_of(
        "setup",
        201,
        &client
            .create_trust_anchor(json!({
                "name": stored_name,
                "source": source("CERTIFICATE_BUNDLE", CERTIFICATE_BUNDLE_PEM),
            }))
            .await,
    );
    let id = id_of("setup", &created);
    let stored_source = created
        .get("source")
        .cloned()
        .expect("the create reports the stored source");

    for (case, bad_source, expected_message) in cases {
        // The request also carries a new name, so a handler that wrote before judging
        // the source would be caught by the read below even where the name alone is
        // legal.
        let answer = client
            .update_trust_anchor(
                &id,
                json!({"name": "renamed by a refused update", "source": bad_source}),
            )
            .await;
        assert_eq!(
            answer.status, 400,
            "case {case:?}: an unusable source must be refused with HTTP 400, got {answer:?}"
        );
        assert_eq!(
            answer.error_code, "ValidationException",
            "case {case:?}: the refusal must be the operation's modelled error, got {answer:?}"
        );
        assert_eq!(
            message_of(&answer),
            expected_message,
            "case {case:?}: the refusal must be worded the way this service words a member \
             that failed a constraint, got {answer:?}"
        );

        let read_back = anchor_of(case, 200, &client.get_trust_anchor(&id).await);
        assert_eq!(
            read_back.get("source"),
            Some(&stored_source),
            "case {case:?}: a refused update must leave the stored source alone, got \
             {read_back:?}"
        );
        assert_eq!(
            read_back.get("name").and_then(Value::as_str),
            Some(stored_name),
            "case {case:?}: a refused update must not write the name it carried, got \
             {read_back:?}"
        );
    }
}

/// The `sourceType`s whose source data is a certificate bundle: the one the model ties
/// `x509CertificateData` to, and the one it ties neither member to, where a bundle stands
/// in ([`source`]).
const BUNDLE_SOURCE_TYPES: [&str; 2] = ["CERTIFICATE_BUNDLE", "SELF_SIGNED_REPOSITORY"];

/// A third authority, so a bundle can hold more than two.
const THIRD_CERTIFICATE_BUNDLE_PEM: &str = include_str!("../data/certificate_authority_c.pem");

/// The base64 body of one authority's PEM, without the framing lines.
fn certificate_body(pem: &str) -> String {
    pem.lines()
        .filter(|line| !line.starts_with("-----"))
        .collect::<Vec<&str>>()
        .join("\n")
}

/// Values that are NOT PEM-encoded X.509 certificates, as `(case, x509CertificateData)`.
///
/// The rows differ along HOW the value fails to be certificate data: it is not PEM at all
/// (plain text, whitespace, a JSON document), it is PEM framing around nothing, around
/// base64 that is not a DER certificate, or around half of one, it is a certificate's
/// base64 without the framing, it is a PEM block of another kind, and it is one real
/// authority followed by a block that is not a certificate — a bundle holds certificates,
/// so one unreadable block makes the bundle unreadable.
fn unreadable_certificate_data() -> Vec<(&'static str, String)> {
    let body = certificate_body(CERTIFICATE_BUNDLE_PEM);
    let half = &body[..body.len() / 2];
    vec![
        ("plain-text", "not a valid PEM certificate".to_string()),
        ("one-space", " ".to_string()),
        ("json-document", "{\"certificate\": \"none\"}".to_string()),
        (
            "framing-around-nothing",
            "-----BEGIN CERTIFICATE-----\n-----END CERTIFICATE-----\n".to_string(),
        ),
        (
            "framed-base64-that-is-not-der",
            "-----BEGIN CERTIFICATE-----\nZW11bGF0ZWQtdGVzdC1jZXJ0\n-----END CERTIFICATE-----\n"
                .to_string(),
        ),
        ("base64-without-framing", body.clone()),
        (
            "half-a-certificate",
            format!("-----BEGIN CERTIFICATE-----\n{half}\n-----END CERTIFICATE-----\n"),
        ),
        (
            "another-kind-of-pem-block",
            CERTIFICATE_BUNDLE_PEM.replace("CERTIFICATE", "PUBLIC KEY"),
        ),
        (
            "one-authority-then-a-block-that-is-not-one",
            format!(
                "{CERTIFICATE_BUNDLE_PEM}-----BEGIN CERTIFICATE-----\n\
                 bm90LWEtY2VydGlmaWNhdGU=\n-----END CERTIFICATE-----\n"
            ),
        ),
    ]
}

/// Values that ARE PEM-encoded X.509 certificates, as `(case, x509CertificateData)`: a
/// bundle holds one or more certificate authorities, so the rows differ along HOW MANY it
/// holds.
fn readable_certificate_data() -> Vec<(&'static str, String)> {
    vec![
        ("one-authority", CERTIFICATE_BUNDLE_PEM.to_string()),
        (
            "two-authorities",
            format!("{CERTIFICATE_BUNDLE_PEM}{OTHER_CERTIFICATE_BUNDLE_PEM}"),
        ),
        (
            "three-authorities",
            format!(
                "{CERTIFICATE_BUNDLE_PEM}{OTHER_CERTIFICATE_BUNDLE_PEM}\
                 {THIRD_CERTIFICATE_BUNDLE_PEM}"
            ),
        ),
    ]
}

/// The fixed wording a trust-anchor write refuses certificate data it cannot read with:
/// it names the write and nothing the request carried.
fn bad_certificate_data_message(action: &str) -> String {
    format!("Error {action} TrustAnchor. Bad certificate data")
}

/// The names the trust anchors a `ListTrustAnchors` page reports carry.
fn listed_names(answer: &Answer) -> Vec<String> {
    answer
        .body
        .get("trustAnchors")
        .and_then(Value::as_array)
        .map(|anchors| {
            anchors
                .iter()
                .filter_map(|anchor| anchor.get("name").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// A create whose `x509CertificateData` does not read as PEM X.509 certificates is refused
/// 400 `ValidationException` with the service's fixed certificate-data wording and mints
/// no trust anchor, while one that does read as certificates is stored verbatim.
///
/// The rows differ along how the value fails to be certificate data
/// ([`unreadable_certificate_data`]), how many authorities an accepted bundle holds
/// ([`readable_certificate_data`]), and which `sourceType` carries it — the member the
/// data travels in decides the rule, not the type beside it.
#[tokio::test]
async fn create_trust_anchor_refuses_certificate_data_it_cannot_read() {
    let client = Client::new();

    for source_type in BUNDLE_SOURCE_TYPES {
        for (case, data) in unreadable_certificate_data() {
            let case = format!("{source_type}/{case}");
            let case = case.as_str();
            let name = format!("refused {case}").replace('/', "-");
            let answer = client
                .create_trust_anchor(json!({"name": name, "source": source(source_type, &data)}))
                .await;
            assert_eq!(
                answer.status, 400,
                "case {case:?}: certificate data that is not certificate data must be \
                 refused with HTTP 400, got {answer:?}"
            );
            assert_eq!(
                answer.error_code, "ValidationException",
                "case {case:?}: the refusal must be the operation's modelled error, got \
                 {answer:?}"
            );
            assert_eq!(
                message_of(&answer),
                bad_certificate_data_message("creating"),
                "case {case:?}: expected the service's certificate-data wording, got \
                 {answer:?}"
            );
            assert!(
                answer.body.get("trustAnchor").is_none(),
                "case {case:?}: a refused create must report no trust anchor, got {answer:?}"
            );

            let listed = listed_names(&client.send("GET", "/trustanchors", None).await);
            assert!(
                !listed.contains(&name),
                "case {case:?}: a refused create must mint nothing, got {listed:?}"
            );
        }

        for (case, data) in readable_certificate_data() {
            let case = format!("{source_type}/{case}");
            let case = case.as_str();
            let name = format!("stored {case}").replace('/', "-");
            let created = anchor_of(
                case,
                201,
                &client
                    .create_trust_anchor(
                        json!({"name": name, "source": source(source_type, &data)}),
                    )
                    .await,
            );
            let id = id_of(case, &created);
            let read_back = anchor_of(case, 200, &client.get_trust_anchor(&id).await);
            assert_eq!(
                read_back.get("source"),
                Some(&source(source_type, &data)),
                "case {case:?}: a stored bundle must read back exactly as submitted, got \
                 {read_back:?}"
            );

            let listed = listed_names(&client.send("GET", "/trustanchors", None).await);
            assert!(
                listed.contains(&name),
                "case {case:?}: an accepted create must mint a trust anchor, got {listed:?}"
            );
        }
    }
}

/// An update whose `x509CertificateData` does not read as PEM X.509 certificates is
/// refused the same way — worded for the update — and leaves the stored source alone,
/// while one that does read as certificates replaces it.
#[tokio::test]
async fn update_trust_anchor_refuses_certificate_data_it_cannot_read() {
    let client = Client::new();
    let created = anchor_of(
        "setup",
        201,
        &client
            .create_trust_anchor(json!({
                "name": "certificate data under update",
                "source": source("CERTIFICATE_BUNDLE", CERTIFICATE_BUNDLE_PEM),
            }))
            .await,
    );
    let id = id_of("setup", &created);
    let stored_source = created
        .get("source")
        .cloned()
        .expect("the create reports the stored source");

    for source_type in BUNDLE_SOURCE_TYPES {
        for (case, data) in unreadable_certificate_data() {
            let case = format!("{source_type}/{case}");
            let case = case.as_str();
            let answer = client
                .update_trust_anchor(&id, json!({"source": source(source_type, &data)}))
                .await;
            assert_eq!(
                answer.status, 400,
                "case {case:?}: certificate data that is not certificate data must be \
                 refused with HTTP 400, got {answer:?}"
            );
            assert_eq!(
                answer.error_code, "ValidationException",
                "case {case:?}: the refusal must be the operation's modelled error, got \
                 {answer:?}"
            );
            assert_eq!(
                message_of(&answer),
                bad_certificate_data_message("updating"),
                "case {case:?}: expected the service's certificate-data wording for an \
                 update, got {answer:?}"
            );

            let read_back = anchor_of(case, 200, &client.get_trust_anchor(&id).await);
            assert_eq!(
                read_back.get("source"),
                Some(&stored_source),
                "case {case:?}: a refused update must leave the stored source alone, got \
                 {read_back:?}"
            );
        }
    }

    // And the same member carrying certificate data it CAN read replaces the source, so
    // the refusals above are about the data rather than about the update path.
    for (case, data) in readable_certificate_data() {
        let submitted = source("CERTIFICATE_BUNDLE", &data);
        let updated = anchor_of(
            case,
            200,
            &client.update_trust_anchor(&id, json!({"source": submitted})).await,
        );
        assert_eq!(
            updated.get("source"),
            Some(&source("CERTIFICATE_BUNDLE", &data)),
            "case {case:?}: an accepted update must store what it carried, got {updated:?}"
        );
    }
}
