// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for the IN-PLACE writes on a stored certificate revocation list:
//! `UpdateCrl` as a PARTIAL write, and the two enablement switches `DisableCrl` and
//! `EnableCrl`.
//!
//! The rule under test: an update addresses one stored record by id and writes only the
//! members the request carries — a member it omits keeps the value the import or an earlier
//! update stored, a member the operation has no input for is not its to change, `updatedAt`
//! becomes the instant of THIS write while `createdAt` never moves — and the single stored
//! record is what every later path reports. `UpdateCrlRequest` carries `crlId` (`@httpLabel`,
//! `@required`), `name` and `crlData`, and answers a `CrlDetail`
//! (`smithy_query(query="operation", operation="UpdateCrl",
//! model_path="services/service-com-amazonaws-rolesanywhere-rolesanywhere/smithy-model/com-amazonaws-rolesanywhere-rolesanywhere.json")`),
//! so `enabled`, `trustAnchorArn`, the ids and the resource's tag set are outside its reach.
//!
//! `DisableCrl` and `EnableCrl` are the same rule with ONE member each: they carry `crlId`
//! alone and write the addressed record's `enabled` flag — false and true respectively —
//! answering the whole `CrlDetail`
//! (`smithy_query(query="operation", operation="DisableCrl,EnableCrl")`), so every other
//! member, the tag set and the sibling records all survive, the flag is written rather than
//! toggled, and an id that resolves to no stored record is refused instead of answered.
//!
//! Three tables, each differing along what the rule generalises over.
//!
//! * A SEQUENCE of updates on one record, so each step has to compose with what the step
//!   before it wrote rather than with the import: the rows differ in which members they
//!   carry, in the value's size and bytes (both published `crlData` `@length` bounds, a
//!   non-UTF-8 blob) and name length (both published `ResourceName` `@length` bounds), and
//!   one row repeats its predecessor verbatim, so a re-sent write is observable only in
//!   `updatedAt`. Each step is checked on the write's own answer, on `GetCrl`, on the
//!   `ListCrls` projection and on `ListTagsForResource`.
//! * The members the operation has no input for, across rows that differ in the `enabled`
//!   state the record is in when the update lands (left to the import's default, imported
//!   off, imported on), in how many tags it holds and in which trust anchor it links to.
//! * Input outside the PUBLISHED constraints of the three input members, each bound tested
//!   at the bound and one past it, which must be refused and must store nothing.
//!
//! Instants are per-run values and are never asserted literally: what is asserted is that
//! `updatedAt` falls inside the window the test's own clock brackets the call with, which is
//! what makes it the write's instant rather than a copy of an earlier one.
//!
//! The harness below is local to this suite, as in each sibling suite.

use std::collections::BTreeMap;
use std::sync::Arc;

use aws_smithy_types::date_time::Format;
use aws_smithy_types::DateTime;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// A PEM bundle holding one real certificate authority: a self-signed CA certificate,
/// valid 2020-2100, written with `rcgen` and kept beside the suites that need one.
/// `CreateTrustAnchor` refuses certificate data it cannot read as PEM X.509 certificates,
/// so a suite that creates a trust anchor presents an authority a trust anchor can hold.
const CERTIFICATE_BUNDLE_PEM: &str = include_str!("../data/certificate_authority_a.pem");

/// A placeholder CRL in PEM form, as the import stores it.
const CRL_PEM: &str = "-----BEGIN X509 CRL-----\nZW11bGF0ZWQtdGVzdC1jcmw=\n-----END X509 CRL-----\n";

/// The Region the suite signs for.
const REGION: &str = "us-west-2";

/// `ResourceName` is `@length(1, 255)` with `@pattern("^[ a-zA-Z0-9-_]*$")`, and
/// `UpdateCrlRequest$crlData` is a `Blob` `@length(1, 300000)`
/// (`smithy_query(query="shape", shape="UpdateCrlRequest")`).
const NAME_MIN_LENGTH: usize = 1;
const NAME_MAX_LENGTH: usize = 255;
/// The expression a refusal quotes for that member: the published expression, which is the
/// model's text without the whole-value anchor pair the model wraps it in.
const NAME_PATTERN: &str = "[ a-zA-Z0-9-_]*";
const CRL_DATA_MIN_LENGTH: usize = 1;
const CRL_DATA_MAX_LENGTH: usize = 300_000;

/// `Uuid` is `@length(36, 36)` with
/// `@pattern("[a-f0-9]{8}-([a-z0-9]{4}-){3}[a-z0-9]{12}")`, and `crlId` targets it.
const UUID_LENGTH: usize = 36;
const UUID_PATTERN: &str = "[a-f0-9]{8}-([a-z0-9]{4}-){3}[a-z0-9]{12}";

/// The account every emulated ARN carries (`engine::emulator_account_id`).
const ACCOUNT: &str = "000000000000";

/// Ids shaped like the modelled `Uuid` that nothing in the store is ever imported under, so
/// the boundary validator accepts them and the handler has to resolve them.
const UNMINTED_IDS: [(&str, &str); 3] = [
    ("all-zero-id", "00000000-0000-0000-0000-000000000000"),
    ("hex-words-id", "deadbeef-dead-beef-dead-beefdeadbeef"),
    ("all-f-id", "ffffffff-ffff-ffff-ffff-ffffffffffff"),
];

/// Ids OUTSIDE the modelled `Uuid` form: too short by one, too long by one, uppercase hex,
/// no separators, and not a uuid at all.
const MALFORMED_IDS: [&str; 5] = [
    "0123abcd-4567-89ef-0123-456789abcde",
    "0123abcd-4567-89ef-0123-456789abcdef0",
    "0123ABCD-4567-89EF-0123-456789ABCDEF",
    "0123abcd456789ef0123456789abcdef0123",
    "not-a-uuid",
];

/// The two enablement switches a stored CRL has, one operation each.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Switch {
    Disable,
    Enable,
}

impl Switch {
    /// The last path segment the model binds the operation to: `POST /crl/{crlId}/disable`
    /// and `POST /crl/{crlId}/enable`
    /// (`smithy_query(query="operation", operation="DisableCrl,EnableCrl")`).
    fn path_suffix(self) -> &'static str {
        match self {
            Switch::Disable => "disable",
            Switch::Enable => "enable",
        }
    }

    /// The `enabled` flag the switch writes on the record it addresses.
    fn target(self) -> bool {
        matches!(self, Switch::Enable)
    }

    /// The operation's name, for the assertion messages.
    fn operation(self) -> &'static str {
        match self {
            Switch::Disable => "DisableCrl",
            Switch::Enable => "EnableCrl",
        }
    }
}

/// A client over one emulator: one engine, one mounted service, so every request in a test
/// observes the state the previous one wrote.
struct Client {
    router: MultiServiceRouter,
}

/// One HTTP answer: the status, the parsed JSON body and the wire error code.
#[derive(Debug)]
struct Answer {
    status: u16,
    body: Value,
    error_code: String,
}

impl Answer {
    fn message(&self) -> &str {
        self.body
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default()
    }

    /// The `crl` member of a CRL operation's answer.
    fn crl(&self) -> Value {
        self.body.get("crl").cloned().unwrap_or(Value::Null)
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

    async fn send(&self, method: &str, uri: &str, body: Option<Value>) -> Answer {
        self.send_in_region(method, uri, body, REGION).await
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
            // The SigV4 credential scope is how the router resolves the service and how the
            // handler learns the request's Region.
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

    async fn import_crl(&self, body: Value) -> Answer {
        self.send("POST", "/crls", Some(body)).await
    }

    async fn get_crl(&self, id: &str) -> Answer {
        self.send("GET", &format!("/crl/{id}"), None).await
    }

    async fn update_crl(&self, id: &str, body: Value) -> Answer {
        self.send("PATCH", &format!("/crl/{id}"), Some(body)).await
    }

    async fn delete_crl(&self, id: &str) -> Answer {
        self.send("DELETE", &format!("/crl/{id}"), None).await
    }

    /// Throw one of the two enablement switches on the CRL an id addresses.
    async fn switch_crl(&self, switch: Switch, id: &str) -> Answer {
        self.switch_crl_in_region(switch, id, REGION).await
    }

    async fn switch_crl_in_region(&self, switch: Switch, id: &str, region: &str) -> Answer {
        self.send_in_region(
            "POST",
            &format!("/crl/{id}/{}", switch.path_suffix()),
            None,
            region,
        )
        .await
    }

    /// The `ListCrls` element whose `crlId` is `id`, over every page.
    async fn listed_crl(&self, case: &str, id: &str) -> Value {
        let mut next: Option<String> = None;
        loop {
            let uri = match &next {
                Some(token) => format!("/crls?pageSize=2&nextToken={token}"),
                None => "/crls?pageSize=2".to_string(),
            };
            let answer = self.send("GET", &uri, None).await;
            assert_eq!(
                answer.status, 200,
                "case {case:?}: expected ListCrls 200, got {answer:?}"
            );
            let entries = answer
                .body
                .get("crls")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            if let Some(found) = entries
                .iter()
                .find(|entry| entry.get("crlId").and_then(Value::as_str) == Some(id))
            {
                return found.clone();
            }
            next = answer
                .body
                .get("nextToken")
                .and_then(Value::as_str)
                .map(str::to_string);
            if next.is_none() {
                panic!("case {case:?}: ListCrls never reported the CRL {id:?}");
            }
        }
    }

    /// The tag set `ListTagsForResource` reports for an ARN, keyed as the service holds it.
    async fn resource_tags(&self, case: &str, resource_arn: &str) -> BTreeMap<String, String> {
        let answer = self
            .send(
                "GET",
                &format!("/ListTagsForResource?resourceArn={resource_arn}"),
                None,
            )
            .await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected ListTagsForResource 200, got {answer:?}"
        );
        answer
            .body
            .get("tags")
            .and_then(Value::as_array)
            .map(|tags| {
                tags.iter()
                    .filter_map(|tag| {
                        let key = tag.get("key").and_then(Value::as_str)?;
                        let value = tag.get("value").and_then(Value::as_str)?;
                        Some((key.to_string(), value.to_string()))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// Create a trust anchor and answer its ARN.
async fn trust_anchor_arn(client: &Client, name: &str) -> String {
    let created = client.create_trust_anchor(name).await;
    assert_eq!(
        created.status, 201,
        "trust anchor {name:?}: expected CreateTrustAnchor 201, got {created:?}"
    );
    created
        .body
        .pointer("/trustAnchor/trustAnchorArn")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("trust anchor {name:?}: no trustAnchorArn in {created:?}"))
        .to_string()
}

/// A string member of a returned `CrlDetail`.
fn member(case: &str, crl: &Value, name: &str) -> Value {
    crl.get(name)
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: no {name} in {crl:?}"))
}

/// The seconds-since-epoch of an ISO-8601 `date-time` member.
fn instant_secs(case: &str, crl: &Value, name: &str) -> f64 {
    let text = member(case, crl, name);
    let text = text
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: {name} must be a date-time string, got {text:?}"));
    DateTime::from_str(text, Format::DateTime)
        .unwrap_or_else(|error| panic!("case {case:?}: {name} {text:?} must parse, got {error}"))
        .as_secs_f64()
}

/// The test's own clock, in the same units a returned instant parses to.
fn clock_secs() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("the test clock is after the epoch")
        .as_secs_f64()
}

/// Import one CRL and answer `(id, arn, the detail the import returned)`.
async fn import(client: &Client, case: &str, body: Value) -> (String, String, Value) {
    let imported = client.import_crl(body).await;
    assert_eq!(
        imported.status, 201,
        "case {case:?}: expected ImportCrl 201, got {imported:?}"
    );
    let crl = imported.crl();
    let id = crl
        .get("crlId")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: no crlId in {imported:?}"))
        .to_string();
    let arn = crl
        .get("crlArn")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: no crlArn in {imported:?}"))
        .to_string();
    (id, arn, crl)
}

/// One step of the update sequence: the body sent, and the members it replaces.
struct Step {
    case: &'static str,
    body: Value,
    name: Option<String>,
    data: Option<Vec<u8>>,
}

/// A SEQUENCE of updates on ONE record composes: each step writes the members it carries on
/// top of what the step before it left, every member it omits survives, and the write is
/// what the operation's own answer, `GetCrl` and the `ListCrls` projection all report.
#[tokio::test]
async fn update_crl_composes_in_place_across_a_sequence_of_writes() {
    let client = Client::new();
    let anchor = trust_anchor_arn(&client, "sequence-anchor").await;
    let (id, arn, imported) = import(
        &client,
        "import",
        json!({
            "name": "sequence crl",
            "crlData": aws_smithy_types::base64::encode(CRL_PEM.as_bytes()),
            "trustAnchorArn": anchor,
            "enabled": true,
            "tags": [{"key": "owner", "value": "platform"}],
        }),
    )
    .await;
    let imported_tags = client.resource_tags("import", &arn).await;

    let steps: Vec<Step> = vec![
        Step {
            case: "name-only",
            body: json!({"name": "renamed once"}),
            name: Some("renamed once".to_string()),
            data: None,
        },
        Step {
            case: "data-only-keeps-the-new-name",
            body: json!({"crlData": aws_smithy_types::base64::encode(b"second revision")}),
            name: None,
            data: Some(b"second revision".to_vec()),
        },
        Step {
            // A `Blob` is bytes, not text: every byte value has to survive a write.
            case: "binary-data",
            body: json!({
                "crlData": aws_smithy_types::base64::encode((0u8..=255).collect::<Vec<u8>>()),
            }),
            name: None,
            data: Some((0u8..=255).collect()),
        },
        Step {
            case: "both-members-at-the-published-ceilings",
            body: json!({
                "name": "c".repeat(NAME_MAX_LENGTH),
                "crlData": aws_smithy_types::base64::encode(vec![b'z'; CRL_DATA_MAX_LENGTH]),
            }),
            name: Some("c".repeat(NAME_MAX_LENGTH)),
            data: Some(vec![b'z'; CRL_DATA_MAX_LENGTH]),
        },
        Step {
            case: "both-members-at-the-published-floors",
            body: json!({
                "name": "c".repeat(NAME_MIN_LENGTH),
                "crlData": aws_smithy_types::base64::encode(vec![b'-'; CRL_DATA_MIN_LENGTH]),
            }),
            name: Some("c".repeat(NAME_MIN_LENGTH)),
            data: Some(vec![b'-'; CRL_DATA_MIN_LENGTH]),
        },
        Step {
            // The same body again: a re-sent write leaves the same values, so only
            // `updatedAt` can differ.
            case: "the-same-body-again",
            body: json!({
                "name": "c".repeat(NAME_MIN_LENGTH),
                "crlData": aws_smithy_types::base64::encode(vec![b'-'; CRL_DATA_MIN_LENGTH]),
            }),
            name: Some("c".repeat(NAME_MIN_LENGTH)),
            data: Some(vec![b'-'; CRL_DATA_MIN_LENGTH]),
        },
        Step {
            case: "neither-member",
            body: json!({}),
            name: None,
            data: None,
        },
    ];

    let mut expected_name = member("import", &imported, "name")
        .as_str()
        .expect("the import reports a name")
        .to_string();
    let mut expected_data = member("import", &imported, "crlData")
        .as_str()
        .expect("the import reports crlData")
        .to_string();
    let created_at = member("import", &imported, "createdAt");
    let mut previous_updated = instant_secs("import", &imported, "updatedAt");

    for step in &steps {
        let case = step.case;
        if let Some(name) = &step.name {
            expected_name = name.clone();
        }
        if let Some(data) = &step.data {
            expected_data = aws_smithy_types::base64::encode(data);
        }

        let before = clock_secs();
        let answer = client.update_crl(&id, step.body.clone()).await;
        let after = clock_secs();
        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected UpdateCrl 200, got {answer:?}"
        );
        let crl = answer.crl();

        assert_eq!(
            member(case, &crl, "name").as_str(),
            Some(expected_name.as_str()),
            "case {case:?}: expected the name {expected_name:?} after this step, got {:?}",
            member(case, &crl, "name")
        );
        assert_eq!(
            member(case, &crl, "crlData").as_str(),
            Some(expected_data.as_str()),
            "case {case:?}: expected {} base64 characters of crlData, got {}",
            expected_data.len(),
            member(case, &crl, "crlData")
                .as_str()
                .unwrap_or_default()
                .len()
        );

        // Members this operation has no input for are not its to change, and `createdAt`
        // belongs to the import.
        for untouched in ["crlId", "crlArn", "trustAnchorArn", "enabled", "createdAt"] {
            assert_eq!(
                member(case, &crl, untouched),
                member("import", &imported, untouched),
                "case {case:?}: {untouched} is not this operation's to change, got {crl:?}"
            );
        }
        assert_eq!(
            member(case, &crl, "createdAt"),
            created_at,
            "case {case:?}: createdAt must stay the instant of the import, got {crl:?}"
        );

        // `updatedAt` is "the ISO-8601 timestamp when the CRL was last updated", so it is
        // the instant of THIS call — inside the window the test brackets it with — and it
        // never goes backwards.
        let updated = instant_secs(case, &crl, "updatedAt");
        assert!(
            updated >= before && updated <= after,
            "case {case:?}: updatedAt must be the instant of this write, got {updated} outside \
             [{before}, {after}]"
        );
        assert!(
            updated >= previous_updated,
            "case {case:?}: updatedAt must not precede the previous write's, got {updated} \
             after {previous_updated}"
        );
        assert!(
            updated >= instant_secs(case, &crl, "createdAt"),
            "case {case:?}: updatedAt must not precede createdAt, got {crl:?}"
        );
        previous_updated = updated;

        // One stored record: the read and the list projection report what the write
        // answered, and the resource's tag set is untouched by it.
        let read = client.get_crl(&id).await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: expected GetCrl 200, got {read:?}"
        );
        assert_eq!(
            read.crl(),
            crl,
            "case {case:?}: GetCrl must report the record the update wrote"
        );
        assert_eq!(
            client.listed_crl(case, &id).await,
            crl,
            "case {case:?}: the ListCrls projection must report the record the update wrote"
        );
        assert_eq!(
            client.resource_tags(case, &arn).await,
            imported_tags,
            "case {case:?}: an update must not touch the resource's tag set"
        );
    }
}

/// An update writes NOTHING outside its own input members, whatever state the record is in:
/// its `enabled` flag, its link to a trust anchor, its ids and its tag set all survive.
///
/// The rows differ in the `enabled` state the update lands on, in how many tags the record
/// holds, and in which of two trust anchors it links to.
#[tokio::test]
async fn update_crl_keeps_the_members_it_has_no_input_for() {
    let client = Client::new();
    let first_anchor = trust_anchor_arn(&client, "sidecar-anchor-one").await;
    let second_anchor = trust_anchor_arn(&client, "sidecar anchor two").await;

    /// One row: how the record is imported, and which trust anchor it links to.
    struct Row {
        case: &'static str,
        anchor: String,
        imported_enabled: Option<bool>,
        tags: Vec<(&'static str, &'static str)>,
    }

    let rows: Vec<Row> = vec![
        Row {
            case: "enabled-left-to-the-default-no-tags",
            anchor: first_anchor.clone(),
            imported_enabled: None,
            tags: Vec::new(),
        },
        Row {
            case: "imported-on-one-tag",
            anchor: second_anchor.clone(),
            imported_enabled: Some(true),
            tags: vec![("owner", "platform")],
        },
        Row {
            case: "imported-off-two-tags",
            anchor: first_anchor.clone(),
            imported_enabled: Some(false),
            tags: vec![("owner", "platform"), ("tier", "prod")],
        },
        Row {
            case: "imported-on-three-tags",
            anchor: second_anchor.clone(),
            imported_enabled: Some(true),
            tags: vec![("owner", "platform"), ("tier", "prod"), ("team", "iam")],
        },
    ];

    for row in &rows {
        let case = row.case;
        let mut body = json!({
            "name": format!("sidecar {case}").replace('-', " "),
            "crlData": aws_smithy_types::base64::encode(CRL_PEM.as_bytes()),
            "trustAnchorArn": row.anchor,
            "tags": row
                .tags
                .iter()
                .map(|(key, value)| json!({"key": key, "value": value}))
                .collect::<Vec<Value>>(),
        });
        if let Some(enabled) = row.imported_enabled {
            body["enabled"] = json!(enabled);
        }
        let (id, arn, imported) = import(&client, case, body).await;

        let expected_enabled = member(case, &imported, "enabled");
        assert_eq!(
            expected_enabled,
            json!(row.imported_enabled.unwrap_or(false)),
            "case {case:?}: the import must store the enabled flag it carried, got {imported:?}"
        );
        let tags_before = client.resource_tags(case, &arn).await;
        assert_eq!(
            tags_before.len(),
            row.tags.len(),
            "case {case:?}: the import must store the tags it carried, got {tags_before:?}"
        );

        let new_name = format!("{case} renamed").replace('-', " ");
        let new_data = format!("revision for {case}").into_bytes();
        let answer = client
            .update_crl(
                &id,
                json!({
                    "name": new_name,
                    "crlData": aws_smithy_types::base64::encode(&new_data),
                }),
            )
            .await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected UpdateCrl 200, got {answer:?}"
        );
        let crl = answer.crl();

        assert_eq!(
            member(case, &crl, "name").as_str(),
            Some(new_name.as_str()),
            "case {case:?}: the update must write the submitted name, got {crl:?}"
        );
        assert_eq!(
            member(case, &crl, "crlData").as_str(),
            Some(aws_smithy_types::base64::encode(&new_data).as_str()),
            "case {case:?}: the update must write the submitted crlData, got {crl:?}"
        );
        assert_eq!(
            member(case, &crl, "enabled"),
            expected_enabled,
            "case {case:?}: the update must leave the enabled flag as it stood, got {crl:?}"
        );
        for untouched in ["crlId", "crlArn", "trustAnchorArn", "createdAt"] {
            assert_eq!(
                member(case, &crl, untouched),
                member(case, &imported, untouched),
                "case {case:?}: {untouched} is not this operation's to change, got {crl:?}"
            );
        }
        assert_eq!(
            client.resource_tags(case, &arn).await,
            tags_before,
            "case {case:?}: the update must leave the tag set as it stood"
        );
        assert_eq!(
            client.get_crl(&id).await.crl(),
            crl,
            "case {case:?}: GetCrl must report the record the update wrote"
        );
    }
}

/// Input outside a PUBLISHED constraint of an update's members is refused as the modelled
/// `ValidationException` (400) naming the member and the bound, and the stored record is
/// left exactly as it was: a refused write stores nothing.
///
/// Every bound of the three input members is covered at the bound (served, in the sequence
/// test above) and one past it here, in both directions where the model gives two.
#[tokio::test]
async fn update_crl_refuses_input_outside_the_published_constraints_and_stores_nothing() {
    let client = Client::new();
    let anchor = trust_anchor_arn(&client, "constraint-anchor").await;
    let (id, _, imported) = import(
        &client,
        "import",
        json!({
            "name": "constraint crl",
            "crlData": aws_smithy_types::base64::encode(CRL_PEM.as_bytes()),
            "trustAnchorArn": anchor,
        }),
    )
    .await;

    // Each row: the path id, the body, and the member plus requirement the refusal must
    // state — the wording the boundary validator words every constraint with.
    let cases: Vec<(&str, String, Value, String)> = vec![
        (
            "name-below-the-length-floor",
            id.clone(),
            json!({"name": ""}),
            format!(
                "at 'name' failed to satisfy constraint: Member must have length greater than or \
                 equal to {NAME_MIN_LENGTH}"
            ),
        ),
        (
            "name-one-past-the-length-ceiling",
            id.clone(),
            json!({"name": "n".repeat(NAME_MAX_LENGTH + 1)}),
            format!(
                "at 'name' failed to satisfy constraint: Member must have length less than or \
                 equal to {NAME_MAX_LENGTH}"
            ),
        ),
        (
            "name-outside-the-published-pattern",
            id.clone(),
            json!({"name": "no!punctuation"}),
            format!(
                "at 'name' failed to satisfy constraint: Member must satisfy regular expression \
                 pattern: {NAME_PATTERN}"
            ),
        ),
        (
            "crl-data-below-the-length-floor",
            id.clone(),
            json!({"crlData": aws_smithy_types::base64::encode(Vec::<u8>::new())}),
            format!(
                "at 'crlData' failed to satisfy constraint: Member must have length greater than \
                 or equal to {CRL_DATA_MIN_LENGTH}"
            ),
        ),
        (
            "crl-data-one-past-the-length-ceiling",
            id.clone(),
            json!({
                "crlData": aws_smithy_types::base64::encode(vec![b'y'; CRL_DATA_MAX_LENGTH + 1]),
            }),
            format!(
                "at 'crlData' failed to satisfy constraint: Member must have length less than or \
                 equal to {CRL_DATA_MAX_LENGTH}"
            ),
        ),
        (
            "crl-id-one-character-short",
            "0".repeat(UUID_LENGTH - 1),
            json!({"name": "renamed"}),
            format!(
                "at 'crlId' failed to satisfy constraint: Member must have length greater than or \
                 equal to {UUID_LENGTH}"
            ),
        ),
        (
            "crl-id-one-character-long",
            "0".repeat(UUID_LENGTH + 1),
            json!({"name": "renamed"}),
            format!(
                "at 'crlId' failed to satisfy constraint: Member must have length less than or \
                 equal to {UUID_LENGTH}"
            ),
        ),
        (
            "crl-id-outside-the-published-pattern",
            "0123ABCD-4567-89EF-0123-456789ABCDEF".to_string(),
            json!({"name": "renamed"}),
            format!(
                "at 'crlId' failed to satisfy constraint: Member must satisfy regular expression \
                 pattern: {UUID_PATTERN}"
            ),
        ),
    ];

    for (case, path_id, body, expected_clause) in &cases {
        let answer = client.update_crl(path_id, body.clone()).await;
        assert_eq!(
            (answer.status, answer.error_code.as_str()),
            (400, "ValidationException"),
            "case {case:?}: input outside a published constraint must be refused, got {answer:?}"
        );
        assert!(
            answer.message().contains(expected_clause.as_str()),
            "case {case:?}: the refusal must state {expected_clause:?}, got {:?}",
            answer.message()
        );
        let read = client.get_crl(&id).await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: the stored CRL must still read back, got {read:?}"
        );
        assert_eq!(
            read.crl(),
            imported,
            "case {case:?}: a refused update must store nothing"
        );
    }

    // The control row: a request inside every constraint is served, and only then does the
    // stored record change.
    let served = client
        .update_crl(
            &id,
            json!({
                "name": "n".repeat(NAME_MAX_LENGTH),
                "crlData": aws_smithy_types::base64::encode(vec![b'y'; CRL_DATA_MAX_LENGTH]),
            }),
        )
        .await;
    assert_eq!(
        served.status, 200,
        "case \"inside-every-constraint\": expected UpdateCrl 200, got {served:?}"
    );
    assert_eq!(
        member("inside-every-constraint", &served.crl(), "name").as_str(),
        Some("n".repeat(NAME_MAX_LENGTH).as_str()),
        "case \"inside-every-constraint\": the served update must write the submitted name"
    );
}

/// The ARN a CRL id carries in `region`, which is the ARN a refusal naming that id words:
/// `CrlDetail$crlArn` is `@pattern("^arn:aws(-[^:]+)?:rolesanywhere(:.*){2}(:crl.*)$")`
/// (`smithy_query(query="shape", shape="CrlDetail")`).
fn crl_arn(region: &str, crl_id: &str) -> String {
    format!("arn:aws:rolesanywhere:{region}:{ACCOUNT}:crl/{crl_id}")
}

/// The wording an ARN-keyed miss carries.
fn arn_not_found_message(resource_arn: &str) -> String {
    format!("The item doesn't exist. arn: {resource_arn} ; aborting.")
}

/// Whether a returned detail reports the CRL as enabled.
fn enabled_flag(case: &str, crl: &Value) -> bool {
    crl.get("enabled")
        .and_then(Value::as_bool)
        .unwrap_or_else(|| panic!("case {case:?}: enabled must be a boolean, got {crl:?}"))
}

/// One row of the enablement table: the shape and enablement the record is imported with,
/// so each switch lands on a record in a different state.
struct EnablementRow {
    case: &'static str,
    imported_enabled: Option<bool>,
    name: String,
    data: Vec<u8>,
    tags: Vec<(&'static str, &'static str)>,
}

/// The rows the enablement switches are driven over: the `enabled` state the import left
/// (omitted, on, off), the name length at both published bounds, and `crlData` spanning a
/// text PEM, every byte value and the published floor.
fn enablement_rows() -> Vec<EnablementRow> {
    vec![
        EnablementRow {
            case: "enabled-omitted-pem-data-no-tags",
            imported_enabled: None,
            name: "c".repeat(NAME_MIN_LENGTH),
            data: CRL_PEM.as_bytes().to_vec(),
            tags: Vec::new(),
        },
        EnablementRow {
            case: "imported-on-binary-data-one-tag",
            imported_enabled: Some(true),
            name: "imported on".to_string(),
            data: (0u8..=255).collect(),
            tags: vec![("owner", "platform")],
        },
        EnablementRow {
            case: "imported-off-name-at-the-ceiling-two-tags",
            imported_enabled: Some(false),
            name: "c".repeat(NAME_MAX_LENGTH),
            data: vec![b'z'; 4096],
            tags: vec![("owner", "platform"), ("tier", "prod")],
        },
        EnablementRow {
            case: "imported-on-data-at-the-floor-three-tags",
            imported_enabled: Some(true),
            name: "floor data".to_string(),
            data: vec![b'-'; CRL_DATA_MIN_LENGTH],
            tags: vec![("owner", "platform"), ("tier", "prod"), ("team", "iam")],
        },
    ]
}

/// A switch writes the addressed record's `enabled` flag and NOTHING else, and the single
/// stored record is what the operation's own answer, `GetCrl` and the `ListCrls` projection
/// all report afterwards.
///
/// Each row is driven through the SAME sequence of switches, so every step composes with what
/// the step before it wrote: both directions, and a repeat of each, which must report the same
/// state because the flag is written rather than toggled.
#[tokio::test]
async fn crl_enablement_switches_only_the_stored_flag_and_persists_it() {
    let sequence = [
        Switch::Disable,
        Switch::Disable,
        Switch::Enable,
        Switch::Enable,
        Switch::Disable,
        Switch::Enable,
    ];

    for row in enablement_rows() {
        let client = Client::new();
        let anchor = trust_anchor_arn(&client, "enablement anchor").await;
        // A sibling the case never addresses, so a write that reached the wrong record shows
        // up below.
        let sibling_case = format!("{}/sibling", row.case);
        let (sibling_id, _, sibling) = import(
            &client,
            &sibling_case,
            json!({
                "name": "untouched sibling",
                "crlData": aws_smithy_types::base64::encode(CRL_PEM.as_bytes()),
                "trustAnchorArn": anchor.clone(),
                "enabled": true,
            }),
        )
        .await;

        let mut body = json!({
            "name": row.name,
            "crlData": aws_smithy_types::base64::encode(&row.data),
            "trustAnchorArn": anchor,
            "tags": row
                .tags
                .iter()
                .map(|(key, value)| json!({"key": key, "value": value}))
                .collect::<Vec<Value>>(),
        });
        if let Some(enabled) = row.imported_enabled {
            body["enabled"] = json!(enabled);
        }
        let (id, arn, imported) = import(&client, row.case, body).await;
        let tags_before = client.resource_tags(row.case, &arn).await;
        let mut previous_updated = instant_secs(row.case, &imported, "updatedAt");

        for (index, switch) in sequence.iter().enumerate() {
            let case = format!("{}/{}-{index}", row.case, switch.path_suffix());
            let before = clock_secs();
            let answer = client.switch_crl(*switch, &id).await;
            let after = clock_secs();
            assert_eq!(
                answer.status,
                200,
                "case {case:?}: expected {} 200, got {answer:?}",
                switch.operation()
            );
            let crl = answer.crl();
            assert_eq!(
                enabled_flag(&case, &crl),
                switch.target(),
                "case {case:?}: {} must report enabled {}, got {crl:?}",
                switch.operation(),
                switch.target()
            );

            // Only the flag and the instant of the write may differ from what the import
            // stored: a switch has no input for any other member, and `createdAt` belongs
            // to the import.
            for untouched in [
                "crlId",
                "crlArn",
                "name",
                "crlData",
                "trustAnchorArn",
                "createdAt",
            ] {
                assert_eq!(
                    member(&case, &crl, untouched),
                    member(row.case, &imported, untouched),
                    "case {case:?}: {untouched} is not this operation's to change, got {crl:?}"
                );
            }

            // `updatedAt` is the instant of THIS write — inside the window the test brackets
            // the call with — and it never goes backwards.
            let updated = instant_secs(&case, &crl, "updatedAt");
            assert!(
                updated >= before && updated <= after,
                "case {case:?}: updatedAt must be the instant of this write, got {updated} \
                 outside [{before}, {after}]"
            );
            assert!(
                updated >= previous_updated,
                "case {case:?}: updatedAt must not precede the previous write's, got {updated} \
                 after {previous_updated}"
            );
            previous_updated = updated;

            // One stored record: the read and the list projection report what the write
            // answered, and the resource's tag set is untouched by it.
            let read = client.get_crl(&id).await;
            assert_eq!(
                read.status, 200,
                "case {case:?}: expected GetCrl 200, got {read:?}"
            );
            assert_eq!(
                read.crl(),
                crl,
                "case {case:?}: GetCrl must report the record the switch wrote"
            );
            assert_eq!(
                client.listed_crl(&case, &id).await,
                crl,
                "case {case:?}: the ListCrls projection must report the record the switch wrote"
            );
            assert_eq!(
                client.resource_tags(&case, &arn).await,
                tags_before,
                "case {case:?}: an enablement switch must not touch the resource's tag set"
            );
        }

        // The sibling is untouched, flag and instants alike.
        let sibling_read = client.get_crl(&sibling_id).await;
        assert_eq!(
            sibling_read.status, 200,
            "case {sibling_case:?}: expected GetCrl 200, got {sibling_read:?}"
        );
        assert_eq!(
            sibling_read.crl(),
            sibling,
            "case {sibling_case:?}: no other CRL may change"
        );
    }
}

/// Switching one CRL is confined to the CRL it addresses: with a store of many, exactly the
/// addressed ones report the flag their last switch wrote, on the read path and in the list
/// projection.
///
/// The rows differ in how many CRLs the store holds, how many of them are switched, in what
/// order, and in which direction.
#[tokio::test]
async fn crl_enablement_is_confined_to_the_crl_it_addresses() {
    /// One row: how many CRLs the store holds, and which of them are switched in what order.
    struct Row {
        case: &'static str,
        total: usize,
        switches: Vec<(usize, Switch)>,
    }

    let cases = [
        Row {
            case: "one-of-one-disabled",
            total: 1,
            switches: vec![(0, Switch::Disable)],
        },
        Row {
            case: "none-of-four",
            total: 4,
            switches: Vec::new(),
        },
        Row {
            case: "first-of-four-disabled",
            total: 4,
            switches: vec![(0, Switch::Disable)],
        },
        Row {
            case: "last-then-first-of-four-disabled",
            total: 4,
            switches: vec![(3, Switch::Disable), (0, Switch::Disable)],
        },
        Row {
            case: "all-of-three-in-reverse-then-one-back-on",
            total: 3,
            switches: vec![
                (2, Switch::Disable),
                (1, Switch::Disable),
                (0, Switch::Disable),
                (1, Switch::Enable),
            ],
        },
    ];

    for Row {
        case,
        total,
        switches,
    } in &cases
    {
        let (case, total) = (*case, *total);
        let client = Client::new();
        let anchor = trust_anchor_arn(&client, "confinement anchor").await;
        let mut ids: Vec<String> = Vec::new();
        for index in 0..total {
            let (id, _, _) = import(
                &client,
                &format!("{case}/import-{index}"),
                json!({
                    "name": format!("crl {index}"),
                    "crlData": aws_smithy_types::base64::encode(CRL_PEM.as_bytes()),
                    "trustAnchorArn": anchor.clone(),
                    // Imported on, so a CRL this case does not address has an enablement a
                    // stray write could still change.
                    "enabled": true,
                }),
            )
            .await;
            ids.push(id);
        }

        let mut expected = vec![true; total];
        for (index, switch) in switches {
            let labelled = format!("{case}/{}-{index}", switch.path_suffix());
            let answer = client.switch_crl(*switch, &ids[*index]).await;
            assert_eq!(
                answer.status,
                200,
                "case {labelled:?}: expected {} 200, got {answer:?}",
                switch.operation()
            );
            expected[*index] = switch.target();
        }

        for (index, id) in ids.iter().enumerate() {
            let labelled = format!("{case}/crl-{index}");
            let read = client.get_crl(id).await;
            assert_eq!(
                read.status, 200,
                "case {labelled:?}: expected GetCrl 200, got {read:?}"
            );
            assert_eq!(
                enabled_flag(&labelled, &read.crl()),
                expected[index],
                "case {labelled:?}: expected enabled {}, got {read:?}",
                expected[index]
            );
            let element = client.listed_crl(&labelled, id).await;
            assert_eq!(
                enabled_flag(&labelled, &element),
                expected[index],
                "case {labelled:?}: the ListCrls projection must report enabled {}, got \
                 {element:?}",
                expected[index]
            );
        }
    }
}

/// An id that resolves to no stored CRL is the modelled 404 carrying the ARN-keyed wording —
/// never a canned success — and an id outside the modelled `Uuid` form is a 400. Neither
/// switch writes anything on a refused call.
///
/// The rows span both switches, why the id does not resolve (never imported, deleted a moment
/// ago) and the Region the request is signed for, which is the only variable part of the
/// message.
#[tokio::test]
async fn crl_enablement_refuses_an_id_that_resolves_to_nothing() {
    let client = Client::new();
    let anchor = trust_anchor_arn(&client, "refusal anchor").await;
    let (survivor_id, _, survivor) = import(
        &client,
        "survivor",
        json!({
            "name": "survivor crl",
            "crlData": aws_smithy_types::base64::encode(CRL_PEM.as_bytes()),
            "trustAnchorArn": anchor.clone(),
            "enabled": true,
        }),
    )
    .await;
    let (deleted_id, _, _) = import(
        &client,
        "deleted",
        json!({
            "name": "deleted crl",
            "crlData": aws_smithy_types::base64::encode(CRL_PEM.as_bytes()),
            "trustAnchorArn": anchor,
        }),
    )
    .await;
    let removed = client.delete_crl(&deleted_id).await;
    assert_eq!(
        removed.status, 200,
        "case \"delete-the-deleted-fixture\": expected DeleteCrl 200, got {removed:?}"
    );

    let mut cases: Vec<(String, String, &str)> = Vec::new();
    for (label, id) in UNMINTED_IDS {
        cases.push((format!("unminted-{label}"), id.to_string(), REGION));
    }
    cases.push(("deleted-crl-id".to_string(), deleted_id.clone(), REGION));
    for region in ["eu-central-1", "ap-southeast-2", "us-east-1"] {
        cases.push((
            format!("unminted-in-{region}"),
            UNMINTED_IDS[1].1.to_string(),
            region,
        ));
    }

    for switch in [Switch::Disable, Switch::Enable] {
        for (label, id, region) in &cases {
            let case = format!("{}/{label}", switch.path_suffix());
            let expected = arn_not_found_message(&crl_arn(region, id));
            let answer = client.switch_crl_in_region(switch, id, region).await;
            assert_eq!(
                (answer.status, answer.error_code.as_str()),
                (404, "ResourceNotFoundException"),
                "case {case:?}: an unresolvable crlId must be refused, got {answer:?}"
            );
            assert_eq!(
                answer.message(),
                expected.as_str(),
                "case {case:?}: expected message {expected:?}, got {:?}",
                answer.message()
            );
        }

        for id in MALFORMED_IDS {
            let case = format!("{}/malformed-{id}", switch.path_suffix());
            let answer = client.switch_crl(switch, id).await;
            assert_eq!(
                (answer.status, answer.error_code.as_str()),
                (400, "ValidationException"),
                "case {case:?}: an id outside the modelled Uuid form must be refused, got \
                 {answer:?}"
            );
        }
    }

    // Not one refusal wrote anything: the stored CRL is still exactly as imported.
    let still_there = client.get_crl(&survivor_id).await;
    assert_eq!(
        still_there.status, 200,
        "case \"survivor\": expected GetCrl 200, got {still_there:?}"
    );
    assert_eq!(
        still_there.crl(),
        survivor,
        "case \"survivor\": a refused call must change nothing"
    );
}
