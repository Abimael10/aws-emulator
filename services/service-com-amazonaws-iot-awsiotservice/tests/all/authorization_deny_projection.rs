// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of the `denied` projection `TestAuthorization` reports on every
//! `authResults` entry.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a restJson1
//! request signed for the `iot` signing name — so what is asserted is the JSON an AWS SDK
//! client parses, not an internal function's return value.
//!
//! The rule under test: `AuthResult` reports the COMPOSITION of one evaluated decision, so
//! every result carries `allowed` and `denied`, and `denied` carries both `implicitDeny`
//! and `explicitDeny`, each with its `policies` list — an EMPTY list, never an absent
//! member, is how a branch reports that nothing was denied that way. The presence of a
//! branch therefore says nothing about the decision: a reader distinguishes an explicit
//! deny (a `Deny` statement, which no further grant overrides) from an implicit one (no
//! matching statement, which a grant fixes) by reading the two lists, and never by probing
//! for a member.
//!
//! The table spans the class the rule generalises over rather than one recorded request:
//! all four `ActionType` values, the resource namespaces each is granted in, every one of
//! the three `AuthDecision` values, a question over several resources, a question over
//! none, several policies in effect at once, no policy in effect at all, and several
//! `authInfos` answered by one request.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The account id every emulated ARN carries (`emulator_server::EMULATOR_ACCOUNT_ID`).
const ACCOUNT_ID: &str = "000000000000";
/// The region the harness signs for.
const REGION: &str = "us-east-1";

/// A SigV4 `Authorization` header whose credential scope names the `iot` signing name and
/// the region: the scope is how the router picks the claiming service. The signature itself
/// is never checked here — the harness turns request authentication off, exactly as an
/// unauthenticated emulator runs.
fn signed_for_iot() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDTESTIOT/20260101/{REGION}/iot/aws4_request, \
         SignedHeaders=host;x-amz-date, Signature=0000"
    )
}

/// One response, reduced to what this suite reads.
#[derive(Debug)]
struct Reply {
    status: u16,
    body: Value,
}

impl Reply {
    /// Every `authResults` entry, in the order the answer reported them.
    fn results(&self) -> Vec<Value> {
        self.body
            .get("authResults")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
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
        let bytes = response.into_body();
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        Reply { status, body }
    }

    /// `CreatePolicy`: `POST /policies/{policyName}`.
    async fn create_policy(&self, policy_name: &str, document: &Value) -> Reply {
        self.call(
            "POST",
            &format!("/policies/{policy_name}"),
            Some(&json!({ "policyDocument": document.to_string() })),
        )
        .await
    }

    /// `CreateKeysAndCertificate`: `POST /keys-and-certificate`, which issues the
    /// certificate a policy is attached to.
    async fn create_certificate(&self) -> Reply {
        self.call("POST", "/keys-and-certificate?setAsActive=true", None)
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

    /// `TestAuthorization`: `POST /test-authorization`.
    async fn test_authorization(&self, request: &Value) -> Reply {
        self.call("POST", "/test-authorization", Some(request)).await
    }

    /// One question about `principal`.
    async fn ask(&self, principal: &str, action_type: &str, resources: &[String]) -> Reply {
        self.test_authorization(&json!({
            "principal": principal,
            "authInfos": [{ "actionType": action_type, "resources": resources }],
        }))
        .await
    }
}

/// An IoT policy document allowing or denying `action` on `resource`.
fn statement(effect: &str, action: Value, resource: Value) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{ "Effect": effect, "Action": action, "Resource": resource }],
    })
}

/// The ARN of an MQTT topic filter, the namespace a subscription is authorized against.
fn topic_filter_arn(filter: &str) -> String {
    format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:topicfilter/{filter}")
}

/// The ARN of an MQTT topic, the namespace a publication or a delivery is authorized
/// against.
fn topic_arn(topic: &str) -> String {
    format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:topic/{topic}")
}

/// The ARN of an MQTT client id, the namespace a connection is authorized against.
fn client_arn(client_id: &str) -> String {
    format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:client/{client_id}")
}

/// Create `policy_name` with `document` and attach it to `target`, asserting both calls
/// succeeded.
async fn given_attached_policy(
    emulator: &Emulator,
    policy_name: &str,
    document: &Value,
    target: &str,
) {
    let created = emulator.create_policy(policy_name, document).await;
    assert_eq!(
        created.status, 200,
        "case {policy_name:?}: CreatePolicy must answer 200, got {} {:?}",
        created.status, created.body
    );
    let attached = emulator.attach_policy(policy_name, target).await;
    assert_eq!(
        attached.status, 200,
        "case {policy_name:?}->{target:?}: AttachPolicy must answer 200, got {} {:?}",
        attached.status, attached.body
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
        .body
        .get("certificateArn")
        .and_then(Value::as_str)
        .expect("an issued certificate carries a certificateArn")
        .to_string()
}

/// The `policyName` of every entry of `result`'s `path` → `policies` list, asserting the
/// structures along the way are present objects and the list is present.
fn branch_policies(case: &str, result: &Value, path: &[&str]) -> Vec<String> {
    let mut current = result;
    let mut walked = String::new();
    for step in path {
        walked.push('.');
        walked.push_str(step);
        current = current.get(step).unwrap_or_else(|| {
            panic!("case {case}: {walked} must be present on the result, got {result:?}")
        });
        assert!(
            current.is_object(),
            "case {case}: {walked} must be a structure, got {current:?}"
        );
    }
    let policies = current
        .get("policies")
        .and_then(Value::as_array)
        .unwrap_or_else(|| {
            panic!("case {case}: {walked}.policies must be a present list, got {current:?}")
        });
    policies
        .iter()
        .map(|policy| {
            let name = policy
                .get("policyName")
                .and_then(Value::as_str)
                .unwrap_or_else(|| {
                    panic!("case {case}: a reported policy carries a policyName, got {policy:?}")
                });
            name.to_string()
        })
        .collect()
}

/// One row of the projection table: a question, the decision the rule gives it, and the
/// policies each branch of the projection reports for it.
struct Row {
    /// What the row is called in an assertion.
    label: &'static str,
    /// `AuthInfo$actionType`.
    action_type: &'static str,
    /// `AuthInfo$resources`.
    resources: Vec<String>,
    /// The `authDecision` the rule gives it.
    decision: &'static str,
    /// The `policyName` of every policy under `allowed.policies`.
    allowed: Vec<&'static str>,
    /// The `policyName` of every policy under `denied.explicitDeny.policies`.
    explicit_deny: Vec<&'static str>,
    /// The `policyName` of every policy under `denied.implicitDeny.policies`.
    implicit_deny: Vec<&'static str>,
}

/// Assert one `AuthResult` against the row it answers: both branches of `denied` present,
/// each carrying exactly the policies the row names.
fn assert_projection(row: &Row, result: &Value) {
    let case = row.label;
    let decision = result
        .get("authDecision")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert_eq!(
        decision, row.decision,
        "case {case}: expected authDecision {:?}, got {decision:?} ({result:?})",
        row.decision
    );
    assert_eq!(
        branch_policies(case, result, &["allowed"]),
        row.allowed,
        "case {case}: expected allowed.policies {:?}, got {result:?}",
        row.allowed
    );
    assert_eq!(
        branch_policies(case, result, &["denied", "explicitDeny"]),
        row.explicit_deny,
        "case {case}: expected denied.explicitDeny.policies {:?}, got {result:?}",
        row.explicit_deny
    );
    assert_eq!(
        branch_policies(case, result, &["denied", "implicitDeny"]),
        row.implicit_deny,
        "case {case}: expected denied.implicitDeny.policies {:?}, got {result:?}",
        row.implicit_deny
    );
}

/// The policy set every row of the table below is decided against, attached to one
/// certificate: a grant, a deny and a grant of another action in another namespace.
async fn given_policy_set(emulator: &Emulator) -> String {
    let certificate = given_certificate(emulator).await;
    given_attached_policy(
        emulator,
        "allow-connect-robot",
        &statement("Allow", json!("iot:Connect"), json!(client_arn("robot-1"))),
        &certificate,
    )
    .await;
    given_attached_policy(
        emulator,
        "allow-publish-open",
        &statement("Allow", json!("iot:Publish"), json!(topic_arn("open/*"))),
        &certificate,
    )
    .await;
    given_attached_policy(
        emulator,
        "deny-publish-secret",
        &statement("Deny", json!("iot:Publish"), json!(topic_arn("secret/*"))),
        &certificate,
    )
    .await;
    certificate
}

/// Every row of the table, over all four `ActionType` values and all three `AuthDecision`
/// values: `denied` is present with both branches, each carrying the (possibly empty) list
/// of the policies that put the question on that side.
#[tokio::test]
async fn every_result_projects_both_deny_branches() {
    let emulator = Emulator::new();
    let certificate = given_policy_set(&emulator).await;
    // Every policy in effect, in the order a decision reports them.
    let in_effect = vec![
        "allow-connect-robot",
        "allow-publish-open",
        "deny-publish-secret",
    ];

    let rows = vec![
        Row {
            label: "a granted publication",
            action_type: "PUBLISH",
            resources: vec![topic_arn("open/news")],
            decision: "ALLOWED",
            allowed: vec!["allow-publish-open"],
            explicit_deny: Vec::new(),
            implicit_deny: Vec::new(),
        },
        Row {
            label: "a granted connection",
            action_type: "CONNECT",
            resources: vec![client_arn("robot-1")],
            decision: "ALLOWED",
            allowed: vec!["allow-connect-robot"],
            explicit_deny: Vec::new(),
            implicit_deny: Vec::new(),
        },
        Row {
            label: "a publication a Deny names",
            action_type: "PUBLISH",
            resources: vec![topic_arn("secret/keys")],
            decision: "EXPLICIT_DENY",
            allowed: Vec::new(),
            explicit_deny: vec!["deny-publish-secret"],
            implicit_deny: Vec::new(),
        },
        Row {
            label: "a granted and a denied topic in one question",
            action_type: "PUBLISH",
            resources: vec![topic_arn("open/news"), topic_arn("secret/keys")],
            decision: "EXPLICIT_DENY",
            allowed: Vec::new(),
            explicit_deny: vec!["deny-publish-secret"],
            implicit_deny: Vec::new(),
        },
        Row {
            label: "a subscription no statement bears on",
            action_type: "SUBSCRIBE",
            resources: vec![topic_filter_arn("home/kitchen")],
            decision: "IMPLICIT_DENY",
            allowed: Vec::new(),
            explicit_deny: Vec::new(),
            implicit_deny: in_effect.clone(),
        },
        Row {
            label: "a delivery no statement bears on",
            action_type: "RECEIVE",
            resources: vec![topic_arn("open/news")],
            decision: "IMPLICIT_DENY",
            allowed: Vec::new(),
            explicit_deny: Vec::new(),
            implicit_deny: in_effect.clone(),
        },
        Row {
            label: "a question naming no resource",
            action_type: "PUBLISH",
            resources: Vec::new(),
            decision: "IMPLICIT_DENY",
            allowed: Vec::new(),
            explicit_deny: Vec::new(),
            implicit_deny: in_effect.clone(),
        },
    ];

    for row in &rows {
        let answer = emulator
            .ask(&certificate, row.action_type, &row.resources)
            .await;
        assert_eq!(
            answer.status, 200,
            "case {}: TestAuthorization must answer 200, got {} {:?}",
            row.label, answer.status, answer.body
        );
        let results = answer.results();
        assert_eq!(
            results.len(),
            1,
            "case {}: one authInfo answers with exactly one authResult, got {:?}",
            row.label,
            answer.body
        );
        assert_projection(row, &results[0]);
    }
}

/// A principal with no policy in effect: the projection is the two branches with two empty
/// lists, which is how a result says nothing denied it explicitly and no policy failed to
/// state anything — never an absent `denied`.
#[tokio::test]
async fn a_principal_with_no_policy_projects_two_empty_branches() {
    let emulator = Emulator::new();
    let certificate = given_certificate(&emulator).await;

    for action_type in ["CONNECT", "PUBLISH", "RECEIVE", "SUBSCRIBE"] {
        let answer = emulator
            .ask(&certificate, action_type, &[topic_arn("open/news")])
            .await;
        assert_eq!(
            answer.status, 200,
            "case {action_type}: TestAuthorization must answer 200, got {} {:?}",
            answer.status, answer.body
        );
        let results = answer.results();
        let result = results
            .first()
            .unwrap_or_else(|| panic!("case {action_type}: one authResult, got {:?}", answer.body));
        assert_eq!(
            result.get("denied"),
            Some(&json!({ "implicitDeny": { "policies": [] }, "explicitDeny": { "policies": [] } })),
            "case {action_type}: expected both branches present with empty policy lists, got \
             {result:?}"
        );
    }
}

/// One request asking several questions: every entry of `authResults` carries the
/// projection, whatever its own decision, and in the order the questions were asked.
#[tokio::test]
async fn every_entry_of_a_multi_question_answer_projects_denied() {
    let emulator = Emulator::new();
    let certificate = given_policy_set(&emulator).await;
    let in_effect = vec![
        "allow-connect-robot",
        "allow-publish-open",
        "deny-publish-secret",
    ];

    let rows = [
        Row {
            label: "question 1, granted",
            action_type: "PUBLISH",
            resources: vec![topic_arn("open/news")],
            decision: "ALLOWED",
            allowed: vec!["allow-publish-open"],
            explicit_deny: Vec::new(),
            implicit_deny: Vec::new(),
        },
        Row {
            label: "question 2, explicitly denied",
            action_type: "PUBLISH",
            resources: vec![topic_arn("secret/keys")],
            decision: "EXPLICIT_DENY",
            allowed: Vec::new(),
            explicit_deny: vec!["deny-publish-secret"],
            implicit_deny: Vec::new(),
        },
        Row {
            label: "question 3, implicitly denied",
            action_type: "SUBSCRIBE",
            resources: vec![topic_filter_arn("home/kitchen")],
            decision: "IMPLICIT_DENY",
            allowed: Vec::new(),
            explicit_deny: Vec::new(),
            implicit_deny: in_effect,
        },
    ];

    let auth_infos: Vec<Value> = rows
        .iter()
        .map(|row| json!({ "actionType": row.action_type, "resources": row.resources }))
        .collect();
    let answer = emulator
        .test_authorization(&json!({
            "principal": certificate,
            "authInfos": auth_infos,
        }))
        .await;
    assert_eq!(
        answer.status, 200,
        "case three questions: TestAuthorization must answer 200, got {} {:?}",
        answer.status, answer.body
    );
    let results = answer.results();
    assert_eq!(
        results.len(),
        rows.len(),
        "case three questions: one authResult per authInfo, got {:?}",
        answer.body
    );
    for (row, result) in rows.iter().zip(&results) {
        assert_projection(row, result);
    }
}
