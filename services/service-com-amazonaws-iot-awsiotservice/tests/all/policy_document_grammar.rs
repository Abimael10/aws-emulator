// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of the POLICY DOCUMENT grammar: which documents AWS IoT
//! refuses, in what words, and when that refusal is decided.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a restJson1
//! request signed for the `iot` signing name — so what is asserted is what an AWS SDK
//! client would see, not an internal function's return value.
//!
//! The rules under test:
//!
//! * a document the policy grammar cannot read is refused `MalformedPolicyException`
//!   (HTTP 400) and the message is AWS's — the fixed prefix `Policy document is Malformed:
//!   ` followed by the clause naming the rule the document broke, and nothing of the
//!   emulator's own parser (no position in the submitted text);
//! * the clause per rule: `Invalid policy syntax.` for a text that is no policy object,
//!   `Unknown policy document version. It must be version 2012-10-17` for one declaring
//!   another language version or none, `Missing required field <Element>` for a statement
//!   element that has to be named, `Has prohibited field Principal` for an element an
//!   identity policy may not carry, `Invalid Condition type : <operator>` for a condition
//!   clause naming no condition operator, and `Invalid IP address in Conditions` for an
//!   address comparison that states no address;
//! * the document is read BEFORE the name is: a malformed document under a name that is
//!   already taken is the document's refusal, not the conflict;
//! * the SAME reading serves `CreatePolicy` and `CreatePolicyVersion`, so the two cannot
//!   drift apart on what they accept;
//! * a statement's `Effect` names one of the two decisions the policy language defines,
//!   read without regard to how it is capitalised, and only a value naming neither is
//!   refused;
//! * a document that satisfies the grammar is accepted and read back verbatim — including
//!   every condition operator the policy language defines, under a set qualifier and with
//!   the `IfExists` suffix.
//!
//! The tables span what the rules generalise over — one row per grammar rule, with the
//! condition-operator catalog driven from the operator families the IAM
//! condition-operator reference publishes (`state::policy_document_conditions`), on
//! operators the issue did not cite — rather than one recorded request.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_iot_awsiotservice::state::{
    CONDITION_OPERATORS, MALFORMED_POLICY_DOCUMENT_PREFIX, POLICY_EFFECTS,
};
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region this suite signs for.
const REGION: &str = "us-west-2";

/// The modelled error a document-shaped refusal carries (`@httpError 400`).
const MALFORMED_POLICY: &str = "MalformedPolicyException";

/// One emulator: a router with this service mounted on its own engine.
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
            .expect("a well-formed policy request");
        Reply::of(self.router.dispatch(request).await)
    }

    /// `POST /policies/{policyName}` — `CreatePolicy`.
    async fn create_policy(&self, policy_name: &str, document: &str) -> Reply {
        self.dispatch(
            "POST",
            &format!("/policies/{policy_name}"),
            Some(&json!({ "policyDocument": document })),
        )
        .await
    }

    /// `POST /policies/{policyName}/version` — `CreatePolicyVersion`.
    async fn create_version(&self, policy_name: &str, document: &str) -> Reply {
        self.dispatch(
            "POST",
            &format!("/policies/{policy_name}/version"),
            Some(&json!({ "policyDocument": document })),
        )
        .await
    }

    /// `GET /policies/{policyName}` — `GetPolicy`, the reader that proves what a create
    /// persisted and that a refused create persisted nothing.
    async fn get_policy(&self, policy_name: &str) -> Reply {
        self.dispatch("GET", &format!("/policies/{policy_name}"), None)
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
            .unwrap_or_else(|error| panic!("a response body is JSON ({error}): {}", self.text()))
    }

    fn member(&self, name: &str) -> String {
        self.json()[name]
            .as_str()
            .unwrap_or_else(|| panic!("no {name} in {}", self.text()))
            .to_string()
    }

    fn error_code(&self) -> String {
        self.json()["__type"]
            .as_str()
            .unwrap_or_else(|| panic!("no __type in {}", self.text()))
            .to_string()
    }
}

/// A document whose one statement carries `members`, under the policy-language version
/// this service reads.
fn statement(members: Value) -> String {
    json!({ "Version": "2012-10-17", "Statement": [members] }).to_string()
}

/// The statement most rows start from: one grant of one action on one resource.
fn allow_publish() -> Value {
    json!({
        "Effect": "Allow",
        "Action": "iot:Publish",
        "Resource": "arn:aws:iot:us-west-2:000000000000:topic/floor/1",
    })
}

/// The same statement, narrowed by `condition`.
fn conditioned(condition: Value) -> String {
    let mut members = allow_publish();
    members["Condition"] = condition;
    statement(members)
}

/// RULE: a document the grammar cannot read is refused 400 `MalformedPolicyException`,
/// with AWS's prefix and the clause naming the rule it broke, and nothing is persisted.
#[tokio::test]
async fn a_malformed_document_is_refused_in_aws_words_and_persists_nothing() {
    let emulator = Emulator::new();

    // (case, the document, the clause after the fixed prefix). One row per rule the
    // grammar states, spanning the forms each rule covers.
    let rows: Vec<(&str, String, String)> = vec![
        (
            "a text that is not JSON at all",
            "{\"Version\": \"2012-10-17\", \"Statement\": [".to_string(),
            "Invalid policy syntax.".to_string(),
        ),
        (
            "JSON that is not an object",
            json!([allow_publish()]).to_string(),
            "Invalid policy syntax.".to_string(),
        ),
        (
            "a document declaring another language version",
            json!({ "Version": "2008-10-17", "Statement": [allow_publish()] }).to_string(),
            "Unknown policy document version. It must be version 2012-10-17".to_string(),
        ),
        (
            "a document declaring no language version",
            json!({ "Statement": [allow_publish()] }).to_string(),
            "Unknown policy document version. It must be version 2012-10-17".to_string(),
        ),
        (
            "a document whose version is not even a string",
            json!({ "Version": 20_121_017, "Statement": [allow_publish()] }).to_string(),
            "Unknown policy document version. It must be version 2012-10-17".to_string(),
        ),
        (
            "a document carrying no Statement",
            json!({ "Version": "2012-10-17" }).to_string(),
            "Missing required field Statement".to_string(),
        ),
        (
            "a statement carrying no Effect",
            statement(json!({ "Action": "iot:Publish", "Resource": "*" })),
            "Missing required field Effect".to_string(),
        ),
        (
            "a statement naming no action",
            statement(json!({ "Effect": "Allow", "Resource": "*" })),
            "Missing required field Action".to_string(),
        ),
        (
            "a statement naming no resource",
            statement(json!({ "Effect": "Allow", "Action": "iot:Publish" })),
            "Missing required field Resource".to_string(),
        ),
        (
            "a statement naming a principal, which an identity policy may not",
            statement(json!({
                "Effect": "Allow",
                "Principal": { "AWS": "*" },
                "Action": "iot:Publish",
                "Resource": "*",
            })),
            "Has prohibited field Principal".to_string(),
        ),
        (
            "a statement excluding a principal",
            statement(json!({
                "Effect": "Allow",
                "NotPrincipal": { "AWS": "*" },
                "Action": "iot:Publish",
                "Resource": "*",
            })),
            "Has prohibited field NotPrincipal".to_string(),
        ),
        (
            "a condition clause naming no operator",
            conditioned(json!({ "StringEqualsNope": { "aws:SourceVpce": "vpce-0" } })),
            "Invalid Condition type : StringEqualsNope".to_string(),
        ),
        (
            "a condition clause under an unknown set qualifier",
            conditioned(json!({ "ForSomeValues:StringEquals": { "aws:TagKeys": ["a"] } })),
            "Invalid Condition type : ForSomeValues:StringEquals".to_string(),
        ),
        (
            // "You can add IfExists to the end of any condition operator name EXCEPT the
            // Null condition" (the IAM condition-operator reference), so this pair names
            // no comparison.
            "the existence operator with the IfExists suffix",
            conditioned(json!({ "NullIfExists": { "aws:SourceVpce": "true" } })),
            "Invalid Condition type : NullIfExists".to_string(),
        ),
        (
            "an address comparison stating text that is no address",
            conditioned(json!({ "IpAddress": { "aws:SourceIp": "not-an-address" } })),
            "Invalid IP address in Conditions".to_string(),
        ),
        (
            "a negated address comparison stating a number",
            conditioned(json!({ "NotIpAddress": { "aws:SourceIp": 10 } })),
            "Invalid IP address in Conditions".to_string(),
        ),
        (
            "an address comparison one of whose values is no address",
            conditioned(json!({
                "IpAddress": { "aws:SourceIp": ["198.51.100.0/24", "198.51.100.300"] },
            })),
            "Invalid IP address in Conditions".to_string(),
        ),
    ];

    for (index, (case, document, clause)) in rows.iter().enumerate() {
        let policy_name = format!("malformed-{index}");
        let refused = emulator.create_policy(&policy_name, document).await;
        assert_eq!(
            refused.status,
            400,
            "case {case:?}: a malformed document must be refused 400, got {} body {}",
            refused.status,
            refused.text()
        );
        assert_eq!(
            refused.error_code(),
            MALFORMED_POLICY,
            "case {case:?}: expected {MALFORMED_POLICY}, got {}",
            refused.text()
        );
        assert_eq!(
            refused.member("message"),
            format!("{MALFORMED_POLICY_DOCUMENT_PREFIX}{clause}"),
            "case {case:?}: got {}",
            refused.text()
        );

        // A refused create persists nothing: the name is still free.
        assert_eq!(
            emulator.get_policy(&policy_name).await.status,
            404,
            "case {case:?}: a refused create must persist nothing"
        );
    }
}

/// RULE: the document is read before the NAME is, so a malformed document under a name
/// that is already taken is refused for the document and the stored policy is untouched.
#[tokio::test]
async fn the_document_is_read_before_the_name_conflict() {
    let emulator = Emulator::new();
    let held = statement(allow_publish());
    assert_eq!(
        emulator.create_policy("taken", &held).await.status,
        200,
        "the policy under test must be creatable"
    );

    // The rows differ along which rule the second document breaks, so the ordering is not
    // keyed to one defect.
    let rows = [
        (
            "an unknown condition operator under a taken name",
            conditioned(json!({ "StringEqualsNope": { "aws:SourceVpce": "vpce-0" } })),
            "Invalid Condition type : StringEqualsNope",
        ),
        (
            "an unknown language version under a taken name",
            json!({ "Version": "2008-10-17", "Statement": [allow_publish()] }).to_string(),
            "Unknown policy document version. It must be version 2012-10-17",
        ),
        (
            "a text that is no policy at all, under a taken name",
            "not a policy".to_string(),
            "Invalid policy syntax.",
        ),
    ];
    for (case, document, clause) in rows {
        let refused = emulator.create_policy("taken", &document).await;
        assert_eq!(
            refused.status,
            400,
            "case {case:?}: the document decides, not the name, got {} body {}",
            refused.status,
            refused.text()
        );
        assert_eq!(
            refused.error_code(),
            MALFORMED_POLICY,
            "case {case:?}: expected {MALFORMED_POLICY}, got {}",
            refused.text()
        );
        assert_eq!(
            refused.member("message"),
            format!("{MALFORMED_POLICY_DOCUMENT_PREFIX}{clause}"),
            "case {case:?}: got {}",
            refused.text()
        );
    }

    // A WELL-FORMED document under the taken name is still the conflict, and the stored
    // policy is exactly what it was.
    let conflict = emulator.create_policy("taken", &held).await;
    assert_eq!(
        conflict.status,
        409,
        "a well-formed document under a taken name is the conflict, got {} body {}",
        conflict.status,
        conflict.text()
    );
    assert_eq!(
        emulator.get_policy("taken").await.member("policyDocument"),
        held,
        "the refusals above must leave the stored document exactly as it was"
    );
}

/// RULE: `CreatePolicyVersion` reads a document under the SAME grammar as the create, so
/// the two cannot drift apart on what they accept.
#[tokio::test]
async fn a_new_version_is_read_under_the_same_grammar() {
    let emulator = Emulator::new();
    assert_eq!(
        emulator
            .create_policy("versioned", &statement(allow_publish()))
            .await
            .status,
        200,
        "the policy under test must be creatable"
    );

    let rows = [
        (
            "an unknown condition operator",
            conditioned(json!({ "ArnLikeNope": { "iot:Connection.Thing.ThingName": "a" } })),
            "Invalid Condition type : ArnLikeNope",
        ),
        (
            "an address comparison stating no address",
            conditioned(json!({ "IpAddress": { "aws:SourceIp": "198.51.100" } })),
            "Invalid IP address in Conditions",
        ),
        (
            "a statement naming no resource",
            statement(json!({ "Effect": "Deny", "Action": "iot:Publish" })),
            "Missing required field Resource",
        ),
    ];
    for (case, document, clause) in rows {
        let refused = emulator.create_version("versioned", &document).await;
        assert_eq!(
            refused.status,
            400,
            "case {case:?}: expected 400, got {} body {}",
            refused.status,
            refused.text()
        );
        assert_eq!(
            refused.member("message"),
            format!("{MALFORMED_POLICY_DOCUMENT_PREFIX}{clause}"),
            "case {case:?}: got {}",
            refused.text()
        );
    }

    // The policy still holds exactly the one version its create made.
    assert_eq!(
        emulator.get_policy("versioned").await.member("defaultVersionId"),
        "1",
        "a refused version create must write nothing"
    );
}

/// RULE: a statement's `Effect` names one of the two DECISIONS the policy language
/// defines, read without regard to how it is capitalised — so a document spelling a
/// decision in any casing is accepted and stored verbatim, and only a value naming
/// neither decision is refused.
#[tokio::test]
async fn a_statements_effect_is_read_without_regard_to_case() {
    let emulator = Emulator::new();

    // The catalog is `state::POLICY_EFFECTS`, the decisions the IAM JSON policy reference
    // defines; each row spells one of them another way, including spellings no recording
    // cited (`DENY`, `dEnY`, `aLlOw`).
    let accepted: Vec<String> = POLICY_EFFECTS
        .iter()
        .flat_map(|effect| {
            [
                (*effect).to_string(),
                effect.to_lowercase(),
                effect.to_uppercase(),
                alternating_case(effect),
            ]
        })
        .collect();

    for (index, spelling) in accepted.iter().enumerate() {
        let document = statement(effected(json!(spelling)));
        let policy_name = format!("effect-{index}");
        let created = emulator.create_policy(&policy_name, &document).await;
        assert_eq!(
            created.status,
            200,
            "case {spelling:?}: a decision in any casing must be accepted, got {} body {}",
            created.status,
            created.text()
        );
        // Stored VERBATIM: the casing the caller wrote is the casing a reader gets back,
        // since nothing canonicalises the document.
        assert_eq!(
            emulator.get_policy(&policy_name).await.member("policyDocument"),
            document,
            "case {spelling:?}: the stored document must come back verbatim"
        );
    }

    // What the rule does NOT cover: a value naming neither decision, and a value that is
    // no string at all. The rows differ in the form the value takes.
    let refused: Vec<(&str, String)> = vec![
        (
            "a word that is no decision",
            statement(effected(json!("Permit"))),
        ),
        (
            "a decision with text around it",
            statement(effected(json!("Allowed"))),
        ),
        ("an empty effect", statement(effected(json!("")))),
        (
            "an effect that is not even a string",
            statement(effected(json!(true))),
        ),
        (
            "an effect stated as a list of decisions",
            statement(effected(json!(["Allow"]))),
        ),
    ];
    for (index, (case, document)) in refused.iter().enumerate() {
        let policy_name = format!("unreadable-effect-{index}");
        let reply = emulator.create_policy(&policy_name, document).await;
        assert_eq!(
            reply.status,
            400,
            "case {case:?}: a value naming no decision must be refused 400, got {} body {}",
            reply.status,
            reply.text()
        );
        assert_eq!(
            reply.error_code(),
            MALFORMED_POLICY,
            "case {case:?}: expected {MALFORMED_POLICY}, got {}",
            reply.text()
        );
        assert_eq!(
            emulator.get_policy(&policy_name).await.status,
            404,
            "case {case:?}: a refused create must persist nothing"
        );
    }

    // The SAME reading serves `CreatePolicyVersion`: a version whose decision is spelled
    // another way is accepted and the document it carries is the one submitted.
    let lowercase_deny = statement(effected(json!("deny")));
    let versioned = emulator.create_version("effect-0", &lowercase_deny).await;
    assert_eq!(
        versioned.status,
        200,
        "a version whose decision is spelled lowercase must be accepted, got {} body {}",
        versioned.status,
        versioned.text()
    );
    assert_eq!(
        versioned.member("policyVersionId"),
        "2",
        "the accepted version must be the policy's second, got {}",
        versioned.text()
    );
    assert_eq!(
        versioned.member("policyDocument"),
        lowercase_deny,
        "the accepted version must carry the document submitted, got {}",
        versioned.text()
    );
}

/// `text` with every other character upper-cased, so a row spells a decision in a casing
/// that is neither the documented one nor a uniform one.
fn alternating_case(text: &str) -> String {
    text.chars()
        .enumerate()
        .map(|(index, character)| {
            if index % 2 == 0 {
                character.to_ascii_lowercase()
            } else {
                character.to_ascii_uppercase()
            }
        })
        .collect()
}

/// The statement every row starts from, with `effect` as its `Effect`.
fn effected(effect: Value) -> Value {
    let mut members = allow_publish();
    members["Effect"] = effect;
    members
}

/// RULE: every condition operator the policy language defines is accepted — under a set
/// qualifier and with the `IfExists` suffix too — and the document is stored verbatim.
#[tokio::test]
async fn every_defined_condition_operator_is_accepted() {
    let emulator = Emulator::new();

    // The catalog comes from `state::policy_document_conditions::CONDITION_OPERATORS`,
    // the operator families the IAM condition-operator reference publishes, so the rows
    // cover operators no recording cited (`BinaryEquals`, `DateLessThanEquals`, …) beside
    // the ones that did.
    for (index, operator) in CONDITION_OPERATORS.iter().enumerate() {
        // The existence operator asks about a key's PRESENCE, so its value is the
        // assertion it makes; an address operator's value is an address; everything else
        // compares text.
        let value = match *operator {
            "Null" => json!("true"),
            "IpAddress" | "NotIpAddress" => json!("198.51.100.0/24"),
            _ => json!("vpce-0123456789abcdef0"),
        };
        // Written three ways: the operator alone, under a set qualifier, and with the
        // suffix that makes it hold for a key the request carries no value for (which the
        // existence operator is not defined for).
        let mut spellings = vec![(*operator).to_string(), format!("ForAnyValue:{operator}")];
        if *operator != "Null" {
            spellings.push(format!("{operator}IfExists"));
        }
        for (spelling_index, spelling) in spellings.iter().enumerate() {
            let mut clause = ::serde_json::Map::new();
            clause.insert(
                spelling.clone(),
                json!({ "aws:SourceVpce": value.clone() }),
            );
            let document = conditioned(Value::Object(clause));
            let policy_name = format!("operator-{index}-{spelling_index}");
            let created = emulator.create_policy(&policy_name, &document).await;
            assert_eq!(
                created.status,
                200,
                "case {spelling:?}: a defined condition operator must be accepted, got {} body \
                 {}",
                created.status,
                created.text()
            );
            assert_eq!(
                emulator.get_policy(&policy_name).await.member("policyDocument"),
                document,
                "case {spelling:?}: the stored document must come back verbatim"
            );
        }
    }
}
