// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of `AttachPolicy` for a policy whose statements are
//! CONDITIONED — the case an IoT policy pinned to a network address, a VPC endpoint or
//! another service's resource is written for.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a restJson1
//! request signed for the `iot` signing name — so what is asserted is what an AWS SDK
//! client would see, not an internal function's return value.
//!
//! The rule under test: an attach records the pair (policy, target) in the one persisted
//! relation and answers 200 with an empty body, whatever the policy DOCUMENT states — the
//! `Condition` clauses of its statements, and the resources of other services their values
//! name, are neither resolved nor validated by the attach, so a clause naming a resource
//! that exists nowhere is recorded and stored exactly as readily as one naming a live
//! resource; the document is then reported verbatim by every reader of it (`GetPolicy`, and
//! `GetEffectivePolicies` for the principal the pair was recorded for), and its conditions
//! decide every authorization question asked about that principal: a clause over a key the
//! question carries no value for does not hold, so an `Allow` so conditioned grants nothing
//! and the key is reported under `missingContextValues`, while the same `Allow` carrying no
//! condition, a negated clause, or one spelled to hold in the key's absence, grants it. A
//! pair is still only ever recorded for resources that exist, so the IoT error classes
//! refuse an unknown policy name and an unknown target.
//!
//! The tables span the class the rule generalises over rather than one recorded request:
//! the condition operators the policy language groups by family (string equality, string
//! wildcard, string negation, the absence-tolerant spelling, address and ARN comparison and
//! the boolean), the global condition keys those clauses are written over, values that name
//! a resource of another service in each of its published id forms, each target form the
//! modelled member documents, and a key the question DOES carry a value for, both matching
//! and not.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The account id every emulated ARN carries (`emulator_server::EMULATOR_ACCOUNT_ID`).
const ACCOUNT_ID: &str = "000000000000";
/// The region the harness signs for.
const REGION: &str = "us-east-1";

/// `ResourceNotFoundException` as this service NAMES it in `x-amzn-errortype`: the modelled
/// shape's name, which is what a restJson1 client resolves the refusal shape from. A client
/// branches on this, which is why the refusals below assert it rather than the status alone.
const NOT_FOUND_ERROR_CODE: &str = "ResourceNotFoundException";

/// A SigV4 `Authorization` header whose credential scope names the `iot` signing name and
/// the region: the scope is how the router picks the claiming service, and how the service
/// picks the region its ARNs are scoped to. The signature itself is never checked here —
/// the harness turns request authentication off, exactly as an unauthenticated emulator
/// runs.
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

    /// Whether the body carries no member at all — what an operation whose output shape is
    /// `Unit` answers with.
    fn is_empty_body(&self) -> bool {
        match &self.body {
            Value::Null => true,
            Value::Object(members) => members.is_empty(),
            _ => false,
        }
    }

    /// The `policyName` of every entry of a `ListAttachedPolicies` answer, in the order the
    /// listing reported them.
    fn policy_names(&self) -> Vec<String> {
        string_members(self.body.get("policies"), "policyName")
    }

    /// The `targets` of a `ListTargetsForPolicy` answer.
    fn targets(&self) -> Vec<String> {
        self.body
            .get("targets")
            .and_then(Value::as_array)
            .map(|entries| {
                entries
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Every `effectivePolicies` entry of a `GetEffectivePolicies` answer, as
    /// `policyName -> policyDocument`.
    fn effective_documents(&self) -> BTreeMap<String, String> {
        self.body
            .get("effectivePolicies")
            .and_then(Value::as_array)
            .map(|entries| {
                entries
                    .iter()
                    .filter_map(|entry| {
                        let name = entry.get("policyName").and_then(Value::as_str)?;
                        let document = entry.get("policyDocument").and_then(Value::as_str)?;
                        Some((name.to_string(), document.to_string()))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The single `authResults` entry an answer to one question carries.
    fn decision(&self) -> Decision {
        let entries = self
            .body
            .get("authResults")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            entries.len(),
            1,
            "one authInfo must answer with exactly one authResult, got {:?}",
            self.body
        );
        Decision::read(&entries[0])
    }
}

/// One `AuthResult`, reduced to what a client branches on.
#[derive(Debug)]
struct Decision {
    /// `authDecision`.
    decision: String,
    /// The `policyName` of every entry of `allowed.policies`.
    allowed: Vec<String>,
    /// The `policyName` of every entry of `denied.implicitDeny.policies`.
    implicit_deny: Vec<String>,
    /// `missingContextValues`.
    missing_context_values: Vec<String>,
}

impl Decision {
    fn read(entry: &Value) -> Self {
        Self {
            decision: entry
                .get("authDecision")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            allowed: string_members(
                entry.get("allowed").and_then(|allowed| allowed.get("policies")),
                "policyName",
            ),
            implicit_deny: string_members(
                entry
                    .get("denied")
                    .and_then(|denied| denied.get("implicitDeny"))
                    .and_then(|implicit| implicit.get("policies")),
                "policyName",
            ),
            missing_context_values: entry
                .get("missingContextValues")
                .and_then(Value::as_array)
                .map(|values| {
                    values
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default(),
        }
    }
}

/// One string `member` of every entry of the list `entries` names, in order.
fn string_members(entries: Option<&Value>, member: &str) -> Vec<String> {
    entries
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| entry.get(member).and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
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

    async fn call(&self, method: &str, path: &str, body: Option<&Value>) -> Reply {
        let payload = body.map_or_else(Vec::new, |value| {
            serde_json::to_vec(value).expect("a request body serializes")
        });
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://iot.{REGION}.amazonaws.com{path}"))
            .header("authorization", signed_for_iot())
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

    /// `CreatePolicy`: `POST /policies/{policyName}`.
    async fn create_policy(&self, policy_name: &str, document: &str) -> Reply {
        self.call(
            "POST",
            &format!("/policies/{policy_name}"),
            Some(&json!({ "policyDocument": document })),
        )
        .await
    }

    /// `GetPolicy`: `GET /policies/{policyName}`.
    async fn get_policy(&self, policy_name: &str) -> Reply {
        self.call("GET", &format!("/policies/{policy_name}"), None)
            .await
    }

    /// `CreateKeysAndCertificate`: `POST /keys-and-certificate`, which issues the
    /// certificate a policy is attached to.
    async fn create_certificate(&self) -> Reply {
        self.call("POST", "/keys-and-certificate?setAsActive=true", None)
            .await
    }

    /// `CreateThingGroup`: `POST /thing-groups/{thingGroupName}`.
    async fn create_thing_group(&self, name: &str) -> Reply {
        self.call("POST", &format!("/thing-groups/{name}"), Some(&json!({})))
            .await
    }

    /// `AttachPolicy`: `PUT /target-policies/{policyName}` carrying the target.
    async fn attach_policy(&self, policy_name: &str, target: &str) -> Reply {
        self.call(
            "PUT",
            &format!("/target-policies/{policy_name}"),
            Some(&json!({ "target": target })),
        )
        .await
    }

    /// `ListAttachedPolicies`: `POST /attached-policies/{target}`, whose target is ONE URI
    /// path segment.
    async fn list_attached_policies(&self, target: &str) -> Reply {
        let segment = target
            .replace('%', "%25")
            .replace(':', "%3A")
            .replace('/', "%2F");
        self.call("POST", &format!("/attached-policies/{segment}"), None)
            .await
    }

    /// `ListTargetsForPolicy`: `POST /policy-targets/{policyName}`.
    async fn list_targets_for_policy(&self, policy_name: &str) -> Reply {
        self.call("POST", &format!("/policy-targets/{policy_name}"), None)
            .await
    }

    /// `GetEffectivePolicies`: `POST /effective-policies`, whose identity travels in the
    /// body.
    async fn get_effective_policies(&self, principal: &str) -> Reply {
        self.call(
            "POST",
            "/effective-policies",
            Some(&json!({ "principal": principal })),
        )
        .await
    }

    /// `TestAuthorization`: `POST /test-authorization`, whose `clientId` is `@httpQuery`.
    async fn test_authorization(&self, request: &Value, client_id: Option<&str>) -> Reply {
        let path = match client_id {
            Some(client_id) => format!("/test-authorization?clientId={client_id}"),
            None => "/test-authorization".to_string(),
        };
        self.call("POST", &path, Some(request)).await
    }
}

/// The ARN of an MQTT client id, the namespace a connection is authorized against.
fn client_arn(client_id: &str) -> String {
    format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:client/{client_id}")
}

/// The ARN of a certificate in this suite's region, for a certificate id this account may
/// hold no certificate under.
fn certificate_arn(certificate_id: &str) -> String {
    format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:cert/{certificate_id}")
}

/// An IoT policy document granting the four data-plane actions on every resource, carrying
/// `condition` as its statement's `Condition` block when one is given.
///
/// The grant itself is constant across the table, so what a row varies is only the
/// condition — which is the whole of what the rule generalises over.
fn conditioned_document(condition: Option<&Value>) -> String {
    let mut statement = serde_json::Map::new();
    statement.insert("Effect".to_string(), json!("Allow"));
    statement.insert(
        "Action".to_string(),
        json!(["iot:Connect", "iot:Publish", "iot:Subscribe", "iot:Receive"]),
    );
    statement.insert("Resource".to_string(), json!("*"));
    if let Some(condition) = condition {
        statement.insert("Condition".to_string(), condition.clone());
    }
    json!({
        "Version": "2012-10-17",
        "Statement": [Value::Object(statement)],
    })
    .to_string()
}

/// One row of the condition table: a `Condition` block a statement may carry, and what the
/// row is called in an assertion.
struct Clause {
    label: &'static str,
    condition: Option<Value>,
}

/// The condition blocks every case in this suite is written over.
///
/// The OPERATORS are written one per family of the policy language's condition-operator
/// reference (*IAM JSON policy elements: Condition operators*): string equality, string
/// wildcard, string negation, the absence-tolerant `IfExists` spelling, the address
/// comparison and the ARN comparison. The KEYS are AWS global condition keys, and each
/// value names a resource or an address of another service — in both published
/// VPC-endpoint id forms — which is what makes every row a reference this service cannot
/// resolve.
fn condition_clauses() -> Vec<Clause> {
    vec![
        Clause {
            label: "no condition at all",
            condition: None,
        },
        Clause {
            label: "string equality over the source VPC endpoint, long id form",
            condition: Some(json!({
                "StringEquals": { "aws:SourceVpce": "vpce-0a1b2c3d4e5f67890" },
            })),
        },
        Clause {
            label: "string equality over the source VPC endpoint, short id form",
            condition: Some(json!({
                "StringEquals": { "aws:SourceVpce": "vpce-1a2b3c4d" },
            })),
        },
        Clause {
            label: "string wildcard over every source VPC endpoint",
            condition: Some(json!({ "StringLike": { "aws:SourceVpce": "vpce-*" } })),
        },
        Clause {
            label: "string negation over the source VPC endpoint",
            condition: Some(json!({
                "StringNotEquals": { "aws:SourceVpce": "vpce-0a1b2c3d4e5f67890" },
            })),
        },
        Clause {
            label: "the absence-tolerant spelling over the source VPC endpoint",
            condition: Some(json!({
                "StringEqualsIfExists": { "aws:SourceVpce": "vpce-0a1b2c3d4e5f67890" },
            })),
        },
        Clause {
            label: "string equality over the source VPC",
            condition: Some(json!({
                "StringEquals": { "aws:SourceVpc": "vpc-0123456789abcdef0" },
            })),
        },
        Clause {
            label: "address comparison over the source address range",
            condition: Some(json!({ "IpAddress": { "aws:SourceIp": "203.0.113.0/24" } })),
        },
        Clause {
            label: "ARN comparison over another service's resource",
            condition: Some(json!({
                "ArnEquals": {
                    "aws:SourceArn": format!("arn:aws:sns:{REGION}:{ACCOUNT_ID}:telemetry"),
                },
            })),
        },
    ]
}

/// Create `policy_name` with `document`, asserting the create succeeded.
async fn given_policy(emulator: &Emulator, policy_name: &str, document: &str) {
    let created = emulator.create_policy(policy_name, document).await;
    assert_eq!(
        created.status, 200,
        "case {policy_name:?}: CreatePolicy must answer 200, got {} {:?}",
        created.status, created.body
    );
}

/// Issue a certificate, answering its `certificateArn`.
async fn given_certificate(emulator: &Emulator) -> String {
    let issued = emulator.create_certificate().await;
    assert_eq!(
        issued.status, 200,
        "CreateKeysAndCertificate must answer 200, got {} {:?}",
        issued.status, issued.body
    );
    issued
        .text("certificateArn")
        .expect("an issued certificate carries a certificateArn")
}

/// Create a thing group, answering its `thingGroupArn`.
async fn given_thing_group(emulator: &Emulator, name: &str) -> String {
    let created = emulator.create_thing_group(name).await;
    assert_eq!(
        created.status, 200,
        "case {name:?}: CreateThingGroup must answer 200, got {} {:?}",
        created.status, created.body
    );
    created
        .text("thingGroupArn")
        .expect("a created thing group carries a thingGroupArn")
}

/// A conditioned policy attaches to every target form and is STORED verbatim: the attach
/// resolves the pair and not the document, so a clause naming a VPC endpoint, a VPC, an
/// address range or another service's resource that exists nowhere is recorded exactly as
/// readily as any other, and every reader of the document reports it character for
/// character.
#[tokio::test]
async fn a_conditioned_policy_attaches_to_every_target_form_and_is_stored_verbatim() {
    let emulator = Emulator::new();

    // One target per form `ListAttachedPolicies$target` documents: "Valid principals are
    // CertificateArn (arn:aws:iot:region:accountId:cert/certificateId), thingGroupArn
    // (arn:aws:iot:region:accountId:thinggroup/groupName) and CognitoId (region:id)".
    let certificate = given_certificate(&emulator).await;
    let group = given_thing_group(&emulator, "plant-floor").await;
    let cognito = format!("{REGION}:1a2b3c4d-5e6f-7081-9293-a4b5c6d7e8f9");
    let targets = [certificate.clone(), group.clone(), cognito.clone()];

    let clauses = condition_clauses();
    let mut documents: BTreeMap<String, String> = BTreeMap::new();

    for (index, clause) in clauses.iter().enumerate() {
        let policy_name = format!("pinned-{index}");
        let document = conditioned_document(clause.condition.as_ref());
        given_policy(&emulator, &policy_name, &document).await;

        for target in &targets {
            let attached = emulator.attach_policy(&policy_name, target).await;
            assert_eq!(
                attached.status, 200,
                "case {} -> {target:?}: AttachPolicy must answer 200, got {} {:?}",
                clause.label, attached.status, attached.body
            );
            assert!(
                attached.is_empty_body(),
                "case {} -> {target:?}: AttachPolicy must answer an empty body, got {:?}",
                clause.label,
                attached.body
            );
        }

        // The policy's end of the relation: every target the pair was recorded for, and
        // nothing else.
        let listed = emulator.list_targets_for_policy(&policy_name).await;
        assert_eq!(
            listed.status, 200,
            "case {}: ListTargetsForPolicy must answer 200, got {} {:?}",
            clause.label, listed.status, listed.body
        );
        let mut reported = listed.targets();
        reported.sort();
        let mut expected = targets.to_vec();
        expected.sort();
        assert_eq!(
            reported, expected,
            "case {}: expected the targets {expected:?}, got {reported:?}",
            clause.label
        );

        // The document is stored as written: the attach neither rewrote it nor refused it
        // for what its condition names.
        let read = emulator.get_policy(&policy_name).await;
        assert_eq!(
            read.status, 200,
            "case {}: GetPolicy must answer 200, got {} {:?}",
            clause.label, read.status, read.body
        );
        assert_eq!(
            read.text("policyDocument").as_deref(),
            Some(document.as_str()),
            "case {}: expected the document as written, got {:?}",
            clause.label,
            read.text("policyDocument")
        );

        documents.insert(policy_name, document);
    }

    // Each target's end of the relation: every policy attached to it, however its document
    // is conditioned.
    let expected_names: Vec<String> = documents.keys().cloned().collect();
    for target in &targets {
        let listed = emulator.list_attached_policies(target).await;
        assert_eq!(
            listed.status, 200,
            "case {target:?}: ListAttachedPolicies must answer 200, got {} {:?}",
            listed.status, listed.body
        );
        let mut reported = listed.policy_names();
        reported.sort();
        assert_eq!(
            reported, expected_names,
            "case {target:?}: expected the policies {expected_names:?}, got {reported:?}"
        );
    }

    // What the data plane is told governs the certificate: every attached policy, each with
    // its own document verbatim — the read the authorization of a connecting device is
    // computed from.
    let effective = emulator.get_effective_policies(&certificate).await;
    assert_eq!(
        effective.status, 200,
        "GetEffectivePolicies must answer 200, got {} {:?}",
        effective.status, effective.body
    );
    assert_eq!(
        effective.effective_documents(),
        documents,
        "the effective policies must report every attached policy with its own document, \
         got {:?}",
        effective.body
    );
}

/// One row of the decision table: a condition, the client id the question carries, and what
/// the rule decides.
struct Question {
    /// What the row is called in an assertion.
    label: &'static str,
    /// The `Condition` block the attached policy's statement carries.
    condition: Option<Value>,
    /// `TestAuthorization$clientId`, the one context value a simulated connection carries.
    client_id: Option<&'static str>,
    /// The `authDecision` the rule gives it.
    decision: &'static str,
    /// The keys the decision must report under `missingContextValues`.
    missing: Vec<&'static str>,
}

/// The conditions of an attached policy DECIDE every question asked about the principal it
/// is attached to: a clause over a key the question carries no value for does not hold, so
/// the `Allow` grants nothing and the key is named under `missingContextValues`; a clause
/// over a key the question DOES carry is decided against that value and names no missing
/// key; and an unconditioned, negated or absence-tolerant clause grants the question.
#[tokio::test]
async fn the_conditions_of_an_attached_policy_decide_every_question() {
    // The key a `TestAuthorization` question carries a value for is the MQTT client id
    // (`TestAuthorization$clientId`, "The MQTT client ID"); every other key named below is
    // one a simulated connection carries no value for.
    let rows = vec![
        Question {
            label: "no condition at all",
            condition: None,
            client_id: None,
            decision: "ALLOWED",
            missing: Vec::new(),
        },
        Question {
            label: "string equality over the source VPC endpoint",
            condition: Some(json!({
                "StringEquals": { "aws:SourceVpce": "vpce-0a1b2c3d4e5f67890" },
            })),
            client_id: None,
            decision: "IMPLICIT_DENY",
            missing: vec!["aws:SourceVpce"],
        },
        Question {
            label: "string equality over another source VPC endpoint",
            condition: Some(json!({ "StringEquals": { "aws:SourceVpce": "vpce-1a2b3c4d" } })),
            client_id: None,
            decision: "IMPLICIT_DENY",
            missing: vec!["aws:SourceVpce"],
        },
        Question {
            label: "string wildcard over every source VPC endpoint",
            condition: Some(json!({ "StringLike": { "aws:SourceVpce": "vpce-*" } })),
            client_id: None,
            decision: "IMPLICIT_DENY",
            missing: vec!["aws:SourceVpce"],
        },
        Question {
            label: "string negation over the source VPC endpoint",
            condition: Some(json!({
                "StringNotEquals": { "aws:SourceVpce": "vpce-0a1b2c3d4e5f67890" },
            })),
            client_id: None,
            decision: "ALLOWED",
            missing: Vec::new(),
        },
        Question {
            label: "the absence-tolerant spelling over the source VPC endpoint",
            condition: Some(json!({
                "StringEqualsIfExists": { "aws:SourceVpce": "vpce-0a1b2c3d4e5f67890" },
            })),
            client_id: None,
            decision: "ALLOWED",
            missing: Vec::new(),
        },
        Question {
            label: "string equality over the source VPC",
            condition: Some(json!({
                "StringEquals": { "aws:SourceVpc": "vpc-0123456789abcdef0" },
            })),
            client_id: None,
            decision: "IMPLICIT_DENY",
            missing: vec!["aws:SourceVpc"],
        },
        Question {
            label: "address comparison over the source address range",
            condition: Some(json!({ "IpAddress": { "aws:SourceIp": "203.0.113.0/24" } })),
            client_id: None,
            decision: "IMPLICIT_DENY",
            missing: vec!["aws:SourceIp"],
        },
        Question {
            label: "ARN comparison over another service's resource",
            condition: Some(json!({
                "ArnEquals": {
                    "aws:SourceArn": format!("arn:aws:sns:{REGION}:{ACCOUNT_ID}:telemetry"),
                },
            })),
            client_id: None,
            decision: "IMPLICIT_DENY",
            missing: vec!["aws:SourceArn"],
        },
        Question {
            label: "string equality over the client id the question carries, matching",
            condition: Some(json!({ "StringEquals": { "iot:ClientId": "lamp-9" } })),
            client_id: Some("lamp-9"),
            decision: "ALLOWED",
            missing: Vec::new(),
        },
        Question {
            label: "string equality over the client id the question carries, not matching",
            condition: Some(json!({ "StringEquals": { "iot:ClientId": "lamp-9" } })),
            client_id: Some("lamp-8"),
            decision: "IMPLICIT_DENY",
            missing: Vec::new(),
        },
    ];

    for row in &rows {
        // One account per row, so each decision is computed from exactly one attached
        // policy and nothing a previous row left behind.
        let emulator = Emulator::new();
        let certificate = given_certificate(&emulator).await;
        let document = conditioned_document(row.condition.as_ref());
        given_policy(&emulator, "pinned", &document).await;
        let attached = emulator.attach_policy("pinned", &certificate).await;
        assert_eq!(
            attached.status, 200,
            "case {}: AttachPolicy must answer 200, got {} {:?}",
            row.label, attached.status, attached.body
        );

        let answer = emulator
            .test_authorization(
                &json!({
                    "principal": certificate,
                    "authInfos": [{
                        "actionType": "CONNECT",
                        "resources": [client_arn("lamp-9")],
                    }],
                }),
                row.client_id,
            )
            .await;
        assert_eq!(
            answer.status, 200,
            "case {}: TestAuthorization must answer 200, got {} {:?}",
            row.label, answer.status, answer.body
        );
        let decided = answer.decision();
        assert_eq!(
            decided.decision, row.decision,
            "case {}: expected authDecision {:?}, got {:?} ({:?})",
            row.label, row.decision, decided.decision, answer.body
        );
        assert_eq!(
            decided.missing_context_values, row.missing,
            "case {}: expected missingContextValues {:?}, got {:?}",
            row.label, row.missing, decided.missing_context_values
        );
        if row.decision == "ALLOWED" {
            assert_eq!(
                decided.allowed,
                vec!["pinned".to_string()],
                "case {}: expected the attached policy to allow it, got {:?}",
                row.label,
                decided.allowed
            );
        } else {
            assert!(
                decided.allowed.is_empty(),
                "case {}: a denied decision reports no allowing policy, got {:?}",
                row.label,
                decided.allowed
            );
            assert_eq!(
                decided.implicit_deny,
                vec!["pinned".to_string()],
                "case {}: a policy that states nothing about the question is reported under \
                 the implicit deny, got {:?}",
                row.label,
                decided.implicit_deny
            );
        }
    }
}

/// A pair is only ever recorded for resources that EXIST, however the document is
/// conditioned: an unknown policy name and a well-formed target naming a certificate this
/// account does not hold are refused with the IoT `ResourceNotFoundException` class (404),
/// and nothing is recorded either way.
#[tokio::test]
async fn attaching_a_conditioned_policy_to_a_resource_that_does_not_exist_is_refused() {
    let emulator = Emulator::new();

    let certificate = given_certificate(&emulator).await;
    let document = conditioned_document(Some(&json!({
        "StringEquals": { "aws:SourceVpce": "vpce-0a1b2c3d4e5f67890" },
    })));
    given_policy(&emulator, "pinned", &document).await;

    // A certificate id this account holds no certificate under: 64 hex characters, the
    // modelled `CertificateId` form (`length 64..64, pattern ^(0x)?[a-fA-F0-9]+$`).
    let dangling_certificate = certificate_arn(&"a1".repeat(32));

    let refusals = [
        (
            "an unknown policy name",
            "no-such-policy",
            certificate.as_str(),
            "Policy not found",
        ),
        (
            "a certificate this account does not hold",
            "pinned",
            dangling_certificate.as_str(),
            "The certificate given in the principal does not exist.",
        ),
    ];

    for (label, policy_name, target, message) in &refusals {
        let refused = emulator.attach_policy(policy_name, target).await;
        assert_eq!(
            refused.status, 404,
            "case {label}: AttachPolicy must refuse it 404, got {} {:?}",
            refused.status, refused.body
        );
        assert_eq!(
            refused.error_code.as_deref(),
            Some(NOT_FOUND_ERROR_CODE),
            "case {label}: expected ResourceNotFoundException, got {:?}",
            refused.error_code
        );
        assert_eq!(
            refused.message(),
            *message,
            "case {label}: expected the API sentence {message:?}, got {:?}",
            refused.message()
        );
    }

    // Nothing was recorded by either refusal: the relation is still empty at both ends.
    let targets = emulator.list_targets_for_policy("pinned").await;
    assert_eq!(
        targets.targets(),
        Vec::<String>::new(),
        "a refused attach records no target, got {:?}",
        targets.body
    );
    let attached = emulator.list_attached_policies(&certificate).await;
    assert_eq!(
        attached.policy_names(),
        Vec::<String>::new(),
        "a refused attach records no policy, got {:?}",
        attached.body
    );
}
