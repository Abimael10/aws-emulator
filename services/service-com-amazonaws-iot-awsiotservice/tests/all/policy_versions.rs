// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of IoT POLICY VERSIONS: `CreatePolicyVersion`, the
//! `ListPolicyVersions` and `GetPolicyVersion` readers that prove what it persisted,
//! and the cap a policy's versions are held under.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a
//! restJson1 request signed for the `iot` signing name — so what is asserted is what
//! an AWS SDK client would see, not an internal function's return value.
//!
//! The rules under test:
//!
//! * a policy holds at most [`MAX_POLICY_VERSIONS`] versions: each create adds one
//!   under a fresh id, and a create past the cap is refused
//!   `VersionsLimitExceededException` (409) with nothing added — so however many
//!   callers add versions at once, a policy never holds more than the cap;
//! * a version is read back with the document it was created with, and which version
//!   is in EFFECT is the policy's own pointer, moved only by a create that asked for
//!   it;
//! * a document that is not an IoT policy document is `MalformedPolicyException`
//!   (400), and a policy the account does not hold is `ResourceNotFoundException`
//!   (404);
//! * a version carries the document BYTES the request supplied — the stored document is
//!   the caller's text, never a reparsed or reformatted copy of it — so whatever form
//!   the JSON was written in comes back from the create, from the reader of that
//!   version and from the reader of the policy while that version is in effect;
//! * a version id is one past the highest the policy has ever held, so no id is reused,
//!   and the cap counts the versions a policy HOLDS: deleting one makes room for
//!   another;
//! * the version in EFFECT moves on a create only when the request asks for it, which
//!   is what `setAsDefault`'s modelled default of `false` means for a request that
//!   omits it.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_iot_awsiotservice::state::MAX_POLICY_VERSIONS;
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region this suite signs for.
const REGION: &str = "ap-northeast-1";

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

    async fn dispatch(&self, method: &str, path: &str, body: Option<&Value>) -> Reply {
        let host = format!("iot.{REGION}.amazonaws.com");
        let payload = body.map_or_else(Bytes::new, |value| Bytes::from(value.to_string()));
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://{host}{path}"))
            .header("host", host)
            .header("content-type", "application/json")
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/iot/aws4_request, \
                     SignedHeaders=host, Signature=0123456789abcdef"
                ),
            )
            .body(payload)
            .expect("the policy request is well formed");
        Reply::of(self.router.dispatch(request).await)
    }

    async fn create_policy(&self, policy_name: &str, document: &str) -> Reply {
        self.dispatch(
            "POST",
            &format!("/policies/{policy_name}"),
            Some(&json!({ "policyDocument": document })),
        )
        .await
    }

    async fn create_version(
        &self,
        policy_name: &str,
        document: &str,
        set_as_default: bool,
    ) -> Reply {
        self.dispatch(
            "POST",
            &format!("/policies/{policy_name}/version?setAsDefault={set_as_default}"),
            Some(&json!({ "policyDocument": document })),
        )
        .await
    }

    async fn list_versions(&self, policy_name: &str) -> Reply {
        self.dispatch("GET", &format!("/policies/{policy_name}/version"), None)
            .await
    }

    async fn get_version(&self, policy_name: &str, version_id: &str) -> Reply {
        self.dispatch(
            "GET",
            &format!("/policies/{policy_name}/version/{version_id}"),
            None,
        )
        .await
    }

    async fn get_policy(&self, policy_name: &str) -> Reply {
        self.dispatch("GET", &format!("/policies/{policy_name}"), None)
            .await
    }

    /// `CreatePolicyVersion` with the `setAsDefault` query parameter OMITTED — how a
    /// client that passes no such member calls it. The member's modelled default is
    /// `false` (`smithy_query(query="operation",operation="CreatePolicyVersion")`), so
    /// this request asks for no re-pointing.
    async fn create_version_without_set_as_default(
        &self,
        policy_name: &str,
        document: &str,
    ) -> Reply {
        self.dispatch(
            "POST",
            &format!("/policies/{policy_name}/version"),
            Some(&json!({ "policyDocument": document })),
        )
        .await
    }

    /// `DeletePolicyVersion`, which frees the slot the deleted version held.
    async fn delete_version(&self, policy_name: &str, version_id: &str) -> Reply {
        self.dispatch(
            "DELETE",
            &format!("/policies/{policy_name}/version/{version_id}"),
            None,
        )
        .await
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

    fn error_code(&self) -> String {
        self.json()["__type"]
            .as_str()
            .unwrap_or_else(|| panic!("no __type in {}", self.text()))
            .to_string()
    }

    /// The string member `name` of the response, byte for byte — not a re-encoded
    /// value, so a document that came back reformatted fails the comparison. Owned
    /// rather than the sibling suites' `Option<&str>` because this reply keeps the
    /// response BYTES, so there is no parsed value to borrow the member out of, and a
    /// member the response must carry is worth panicking over with the body in hand.
    fn string_member(&self, name: &str) -> String {
        self.json()[name]
            .as_str()
            .unwrap_or_else(|| panic!("no string {name} in {}", self.text()))
            .to_string()
    }
}

/// An IoT policy document allowing one action, numbered so each version differs.
fn document(index: usize) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": ["iot:Publish"],
            "Resource": [format!("arn:aws:iot:{REGION}:000000000000:topic/version-{index}")],
        }],
    })
    .to_string()
}

/// RULE: versions accumulate up to the cap, each under a fresh id, and the create past
/// the cap adds nothing.
#[tokio::test]
async fn versions_accumulate_up_to_the_cap_and_no_further() {
    let emulator = Emulator::new();
    assert_eq!(
        emulator.create_policy("fleet-policy", &document(1)).await.status,
        200,
        "the policy a case adds versions to must be creatable"
    );

    // The create of the policy is its first version, so the cap leaves one fewer to
    // add. Each row asks for a different `setAsDefault`, so the pointer's rule is
    // exercised in both directions.
    for index in 2..=MAX_POLICY_VERSIONS {
        let set_as_default = index % 2 == 0;
        let created = emulator
            .create_version("fleet-policy", &document(index), set_as_default)
            .await;
        assert_eq!(
            created.status,
            200,
            "case version {index}: expected 200, got {} body {}",
            created.status,
            created.text()
        );
        let body = created.json();
        assert_eq!(
            body["policyVersionId"],
            json!(index.to_string()),
            "case version {index}: expected a fresh id, got {body}"
        );
        assert_eq!(
            body["isDefaultVersion"],
            json!(set_as_default),
            "case version {index}: expected isDefaultVersion {set_as_default}, got {body}"
        );
        assert_eq!(
            body["policyDocument"],
            json!(document(index)),
            "case version {index}: expected the stored document, got {body}"
        );
    }

    // One past the cap: refused, and the policy still holds exactly the cap.
    let over = emulator
        .create_version("fleet-policy", &document(99), false)
        .await;
    assert_eq!(
        over.status,
        409,
        "case one past the cap: expected 409, got {} body {}",
        over.status,
        over.text()
    );
    assert_eq!(
        over.error_code(),
        "VersionsLimitExceededException",
        "case one past the cap: got {}",
        over.text()
    );

    let listed = emulator.list_versions("fleet-policy").await.json();
    let versions = listed["policyVersions"]
        .as_array()
        .unwrap_or_else(|| panic!("no policyVersions in {listed}"));
    assert_eq!(
        versions.len(),
        MAX_POLICY_VERSIONS,
        "case the listing: expected the cap and no more, got {listed}"
    );
    let ids: Vec<String> = versions
        .iter()
        .map(|version| version["versionId"].as_str().unwrap_or_default().to_string())
        .collect();
    let expected_ids: Vec<String> = (1..=MAX_POLICY_VERSIONS)
        .rev()
        .map(|index| index.to_string())
        .collect();
    assert_eq!(
        ids, expected_ids,
        "case the listing: expected the versions newest-first, got {ids:?}"
    );
    let defaults: Vec<&Value> = versions
        .iter()
        .filter(|version| version["isDefaultVersion"] == json!(true))
        .collect();
    assert_eq!(
        defaults.len(),
        1,
        "case the listing: exactly one version is in effect, got {listed}"
    );

    // Each version reads back the document it was created with, and the one in effect
    // is the one the policy's own reader reports.
    for index in 1..=MAX_POLICY_VERSIONS {
        let read = emulator
            .get_version("fleet-policy", &index.to_string())
            .await;
        assert_eq!(
            read.status,
            200,
            "case reading version {index}: expected 200, got {} body {}",
            read.status,
            read.text()
        );
        let body = read.json();
        assert_eq!(
            body["policyDocument"],
            json!(document(index)),
            "case reading version {index}: expected its own document, got {body}"
        );
        assert_eq!(
            body["policyName"],
            json!("fleet-policy"),
            "case reading version {index}: expected the policy named, got {body}"
        );
    }
    let policy = emulator.get_policy("fleet-policy").await.json();
    let default_version_id = policy["defaultVersionId"]
        .as_str()
        .unwrap_or_else(|| panic!("no defaultVersionId in {policy}"));
    let in_effect = emulator
        .get_version("fleet-policy", default_version_id)
        .await
        .json();
    assert_eq!(
        in_effect["isDefaultVersion"],
        json!(true),
        "case the version in effect: the two readers must agree, got {in_effect}"
    );
    assert_eq!(
        policy["policyDocument"], in_effect["policyDocument"],
        "case the version in effect: the policy reports that version's document, got {policy}"
    );
}

/// RULE: a version is only created for a policy that exists, out of a document that is
/// an IoT policy document.
#[tokio::test]
async fn a_version_of_no_policy_or_of_no_policy_document_is_refused() {
    let emulator = Emulator::new();
    assert_eq!(
        emulator.create_policy("fleet-policy", &document(1)).await.status,
        200
    );

    let missing = emulator
        .create_version("no-such-policy", &document(2), false)
        .await;
    assert_eq!(
        missing.status,
        404,
        "case a policy the account does not hold: expected 404, got {} body {}",
        missing.status,
        missing.text()
    );
    assert_eq!(
        missing.error_code(),
        "ResourceNotFoundException",
        "case a policy the account does not hold: got {}",
        missing.text()
    );

    // The rows break the document rule in a different way each: not JSON at all, JSON
    // that is not an object, an object with no statement, a statement with no effect,
    // and a statement naming no action.
    let malformed = [
        "not json at all",
        "[]",
        "{}",
        r#"{"Statement":[{"Action":["iot:Publish"]}]}"#,
        r#"{"Statement":[{"Effect":"Allow"}]}"#,
        r#"{"Statement":[{"Effect":"Maybe","Action":["iot:Publish"]}]}"#,
    ];
    for document in malformed {
        let refused = emulator
            .create_version("fleet-policy", document, false)
            .await;
        assert_eq!(
            refused.status,
            400,
            "case {document:?}: expected 400, got {} body {}",
            refused.status,
            refused.text()
        );
        assert_eq!(
            refused.error_code(),
            "MalformedPolicyException",
            "case {document:?}: got {}",
            refused.text()
        );
    }

    // Nothing above was written: the policy still holds only the version its create
    // wrote.
    let listed = emulator.list_versions("fleet-policy").await.json();
    assert_eq!(
        listed["policyVersions"].as_array().map(Vec::len),
        Some(1),
        "case after the refusals: expected the one version the create wrote, got {listed}"
    );

    // A version id the policy does not hold, and one that is no version id at all.
    for version_id in ["7", "0"] {
        let read = emulator.get_version("fleet-policy", version_id).await;
        assert_eq!(
            read.status,
            404,
            "case version {version_id}: expected 404, got {} body {}",
            read.status,
            read.text()
        );
    }
    let listed_missing = emulator.list_versions("no-such-policy").await;
    assert_eq!(
        listed_missing.status,
        404,
        "case listing a policy the account does not hold: expected 404, got {} body {}",
        listed_missing.status,
        listed_missing.text()
    );
}

/// The FORMS one IoT policy document can be written in, each a document the grammar
/// admits: the elements an IoT policy document carries (`Version`, `Statement` as one
/// object or a list, `Effect`, `Action`/`NotAction`, `Resource`/`NotResource`, `Sid`,
/// `Condition` under a condition operator) are the model's and this service's own
/// (`smithy_query(query="doc",shape="CreatePolicyVersion",member="policyDocument")`).
/// The rows differ in the one thing the rule generalises over — the TEXT the same
/// grant is written as: spacing, member order, a single statement rather than a list,
/// a narrowing condition, non-ASCII text, and padding around the whole document.
const DOCUMENT_FORMS: &[(&str, &str)] = &[
    (
        "compact, no whitespace at all",
        r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"iot:Connect","Resource":"*"}]}"#,
    ),
    (
        "pretty-printed over several lines",
        "{\n    \"Version\": \"2012-10-17\",\n    \"Statement\": [\n        {\n            \"Effect\": \"Allow\",\n            \"Action\": [\n                \"iot:Publish\"\n            ],\n            \"Resource\": [\n                \"arn:aws:iot:ap-northeast-1:000000000000:topic/telemetry\"\n            ]\n        }\n    ]\n}",
    ),
    (
        "indented with tabs",
        "{\n\t\"Version\": \"2012-10-17\",\n\t\"Statement\": {\n\t\t\"Effect\": \"Deny\",\n\t\t\"NotAction\": \"iot:Subscribe\",\n\t\t\"NotResource\": \"*\"\n\t}\n}",
    ),
    (
        "the statement a single object rather than a list",
        r#"{"Version":"2012-10-17","Statement":{"Effect":"Allow","Action":"iot:Receive","Resource":"*"}}"#,
    ),
    (
        "the members in the other order",
        r#"{"Statement":[{"Resource":"*","Action":"iot:Receive","Effect":"Allow","Sid":"MembersOutOfOrder"}],"Version":"2012-10-17"}"#,
    ),
    (
        "a statement narrowed by a condition clause",
        r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"iot:Connect","Resource":"*","Condition":{"StringEquals":{"aws:SourceVpce":"vpce-1a2b3c4d"}}}]}"#,
    ),
    (
        "non-ASCII text in a statement id",
        r#"{"Version":"2012-10-17","Statement":[{"Sid":"Übersicht-政策","Effect":"Allow","Action":"iot:Connect","Resource":"*"}]}"#,
    ),
    (
        "padded with whitespace outside and inside",
        "  \n\t{ \"Version\" : \"2012-10-17\" , \"Statement\" : [ { \"Effect\" : \"Allow\" , \"Action\" : \"iot:Connect\" , \"Resource\" : \"*\" } ] }\n  ",
    ),
];

/// RULE: a version carries the document bytes the request supplied, so every reader of
/// it hands back the caller's own text rather than a reformatted copy.
#[tokio::test]
async fn a_version_keeps_the_document_bytes_the_request_carried() {
    let emulator = Emulator::new();

    for (index, (case, form)) in DOCUMENT_FORMS.iter().enumerate() {
        let policy_name = format!("document-form-{index}");
        let created_policy = emulator.create_policy(&policy_name, &document(1)).await;
        assert_eq!(
            created_policy.status,
            200,
            "case {case}: the policy the version hangs off must be creatable, got {} body {}",
            created_policy.status,
            created_policy.text()
        );
        let policy_arn = created_policy.string_member("policyArn");

        // The version is asked for as the operative one, so the policy's own reader
        // resolves through it too.
        let created = emulator.create_version(&policy_name, form, true).await;
        assert_eq!(
            created.status,
            200,
            "case {case}: expected 200, got {} body {}",
            created.status,
            created.text()
        );
        let echoed = created.string_member("policyDocument");
        assert_eq!(
            echoed, *form,
            "case {case}: the create must echo the document bytes, got {echoed:?}"
        );
        assert_eq!(
            created.string_member("policyVersionId"),
            "2",
            "case {case}: expected the second version of the policy, got {}",
            created.text()
        );
        assert_eq!(
            created.json()["isDefaultVersion"],
            json!(true),
            "case {case}: the request asked for the operative version, got {}",
            created.text()
        );
        assert_eq!(
            created.string_member("policyArn"),
            policy_arn,
            "case {case}: every version reports the policy's own ARN, got {}",
            created.text()
        );

        let read = emulator.get_version(&policy_name, "2").await;
        let stored = read.string_member("policyDocument");
        assert_eq!(
            stored, *form,
            "case {case}: the reader of the version must hand back the same bytes, got \
             {stored:?}"
        );

        let policy = emulator.get_policy(&policy_name).await;
        let in_effect = policy.string_member("policyDocument");
        assert_eq!(
            in_effect, *form,
            "case {case}: the policy reports the operative version's bytes, got {in_effect:?}"
        );
        assert_eq!(
            policy.string_member("defaultVersionId"),
            "2",
            "case {case}: expected the new version in effect, got {}",
            policy.text()
        );

        // The version the policy's create wrote is untouched by the one added beside it.
        assert_eq!(
            emulator
                .get_version(&policy_name, "1")
                .await
                .string_member("policyDocument"),
            document(1),
            "case {case}: the first version keeps its own document"
        );
    }
}

/// RULE: a version id is one past the highest the policy has ever held, and the cap
/// counts the versions it HOLDS — so a delete makes room for another, which takes a
/// fresh id rather than the freed one.
#[tokio::test]
async fn version_ids_are_never_reused_and_the_cap_counts_versions_held() {
    let emulator = Emulator::new();
    assert_eq!(
        emulator
            .create_policy("retained-ids", &document(1))
            .await
            .status,
        200,
        "the policy the case adds versions to must be creatable"
    );

    // Fill the policy to the cap, leaving the version its create wrote in effect.
    for index in 2..=MAX_POLICY_VERSIONS {
        let created = emulator
            .create_version("retained-ids", &document(index), false)
            .await;
        assert_eq!(
            created.status,
            200,
            "case version {index}: expected 200, got {} body {}",
            created.status,
            created.text()
        );
    }
    let at_cap = emulator
        .create_version("retained-ids", &document(99), false)
        .await;
    assert_eq!(
        at_cap.status,
        409,
        "case at the cap: expected 409, got {} body {}",
        at_cap.status,
        at_cap.text()
    );

    // Free one slot. Version 2 is not the one in effect, so it is deletable.
    let deleted = emulator.delete_version("retained-ids", "2").await;
    assert_eq!(
        deleted.status,
        200,
        "case deleting version 2: expected 200, got {} body {}",
        deleted.status,
        deleted.text()
    );
    let gone = emulator.get_version("retained-ids", "2").await;
    assert_eq!(
        gone.status,
        404,
        "case reading the deleted version: expected 404, got {} body {}",
        gone.status,
        gone.text()
    );

    // The freed slot takes a version under a FRESH id, one past the highest ever held.
    let next_id = (MAX_POLICY_VERSIONS + 1).to_string();
    let created = emulator
        .create_version("retained-ids", &document(MAX_POLICY_VERSIONS + 1), false)
        .await;
    assert_eq!(
        created.status,
        200,
        "case the create after the delete: expected 200, got {} body {}",
        created.status,
        created.text()
    );
    assert_eq!(
        created.string_member("policyVersionId"),
        next_id,
        "case the create after the delete: expected the id {next_id}, got {}",
        created.text()
    );

    let listed = emulator.list_versions("retained-ids").await.json();
    let ids: Vec<String> = listed["policyVersions"]
        .as_array()
        .unwrap_or_else(|| panic!("no policyVersions in {listed}"))
        .iter()
        .map(|version| {
            version["versionId"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect();
    let expected_ids: Vec<String> = (1..=MAX_POLICY_VERSIONS + 1)
        .rev()
        .filter(|index| *index != 2)
        .map(|index| index.to_string())
        .collect();
    assert_eq!(
        ids, expected_ids,
        "case after the delete and the create: expected {expected_ids:?}, got {ids:?}"
    );

    // And the policy is at the cap again, so the next create is refused.
    let refused = emulator
        .create_version("retained-ids", &document(99), false)
        .await;
    assert_eq!(
        refused.status,
        409,
        "case at the cap again: expected 409, got {} body {}",
        refused.status,
        refused.text()
    );
}

/// RULE: a create moves the version in EFFECT only when the request asks it to, which
/// is what `setAsDefault`'s modelled default of `false` means for a request that omits
/// it.
#[tokio::test]
async fn the_operative_version_moves_only_when_the_create_asks() {
    let emulator = Emulator::new();
    assert_eq!(
        emulator
            .create_policy("operative-version", &document(1))
            .await
            .status,
        200,
        "the policy the case adds versions to must be creatable"
    );

    // Row 1: the parameter omitted. Row 2: the parameter sent as `false`. Neither asks
    // for the operative version, so both leave the pointer where the policy's create
    // put it.
    let omitted = emulator
        .create_version_without_set_as_default("operative-version", &document(2))
        .await;
    let explicit_false = emulator
        .create_version("operative-version", &document(3), false)
        .await;
    for (case, version_id, reply) in [
        ("setAsDefault omitted", "2", &omitted),
        ("setAsDefault false", "3", &explicit_false),
    ] {
        assert_eq!(
            reply.status,
            200,
            "case {case}: expected 200, got {} body {}",
            reply.status,
            reply.text()
        );
        assert_eq!(
            reply.string_member("policyVersionId"),
            version_id,
            "case {case}: expected version {version_id}, got {}",
            reply.text()
        );
        assert_eq!(
            reply.json()["isDefaultVersion"],
            json!(false),
            "case {case}: expected the version not to be in effect, got {}",
            reply.text()
        );
        let read = emulator.get_version("operative-version", version_id).await;
        assert_eq!(
            read.json()["isDefaultVersion"],
            json!(false),
            "case {case}: the reader of the version must agree, got {}",
            read.text()
        );
        let policy = emulator.get_policy("operative-version").await;
        assert_eq!(
            policy.string_member("defaultVersionId"),
            "1",
            "case {case}: expected the pointer left where it was, got {}",
            policy.text()
        );
        assert_eq!(
            policy.string_member("policyDocument"),
            document(1),
            "case {case}: the policy still reports the operative version's document, got {}",
            policy.text()
        );
    }

    // Row 3: the parameter sent as `true` — the one request that moves the pointer.
    let asked = emulator
        .create_version("operative-version", &document(4), true)
        .await;
    assert_eq!(
        asked.json()["isDefaultVersion"],
        json!(true),
        "case setAsDefault true: expected the new version in effect, got {}",
        asked.text()
    );
    let policy = emulator.get_policy("operative-version").await;
    assert_eq!(
        policy.string_member("defaultVersionId"),
        "4",
        "case setAsDefault true: expected the pointer moved, got {}",
        policy.text()
    );
    assert_eq!(
        policy.string_member("policyDocument"),
        document(4),
        "case setAsDefault true: expected that version's document, got {}",
        policy.text()
    );
    assert_eq!(
        emulator
            .get_version("operative-version", "1")
            .await
            .json()["isDefaultVersion"],
        json!(false),
        "case setAsDefault true: the version it moved off is no longer in effect"
    );
}
