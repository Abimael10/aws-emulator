// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of the CONDITION CONTEXT one `TestAuthorization` question is
//! decided in: which condition keys the IoT authorizer answers for the connection being
//! simulated, and what a statement conditioned on a key it cannot answer for decides.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a restJson1
//! request signed for the `iot` signing name — so what is asserted is the JSON an AWS SDK
//! client parses, not an internal function's return value.
//!
//! The rule under test: the authorizer answers for the connection it is deciding about —
//! the MQTT client id the request named and the address the request was made from — and for
//! nothing else, so
//!
//! * a clause over a key it ANSWERS for is compared against that value, which makes a
//!   clause the value does not satisfy a plain non-match: the statement does not apply, the
//!   decision is reached from the statements that do, and `missingContextValues` is empty,
//!   because nothing was missing;
//! * a clause over any OTHER key — `aws:SourceVpce` for a request that did not arrive over a
//!   VPC endpoint — can be read neither as holding nor as failing, whatever the operator's
//!   polarity or spelling, so the statement carrying it neither grants the question nor
//!   refuses it: the question falls to the IMPLICIT deny (a refusal a further grant fixes,
//!   not one a `Deny` statement pronounced), the policy is reported under
//!   `denied.implicitDeny`, and the key is reported under `missingContextValues`, which is
//!   what tells a caller to supply the key rather than to rewrite the policy;
//! * a question that cannot be decided STOPS the batch: the answers run up to and including
//!   the first undecidable question, so every entry a caller zips against its `authInfos` is
//!   a question this evaluation reached.
//!
//! Each table spans the class the rule generalises over rather than one request: the
//! operator families of the policy language's condition-operator reference (string equality,
//! string wildcard, string negation, the absence-tolerant spelling, the address comparison
//! and its negation, the boolean and the existence operator), both answered keys and five
//! unanswered ones, both effects a statement may state, the address read off a forwarded hop
//! and the loopback address read when nothing forwarded the request, and batches of one to
//! four questions.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The account id every emulated ARN carries (`emulator_server::EMULATOR_ACCOUNT_ID`).
const ACCOUNT_ID: &str = "000000000000";
/// The region the harness signs for.
const REGION: &str = "us-east-1";

/// The address the suite's requests are forwarded for, which is the address the simulated
/// connection is then decided against. From RFC 5737's documentation range, so it names no
/// real host.
const CALLER_ADDRESS: &str = "203.0.113.9";

/// A network [`CALLER_ADDRESS`] lies in.
const CALLER_NETWORK: &str = "203.0.113.0/24";

/// A network it does not lie in.
const OTHER_NETWORK: &str = "10.40.0.0/16";

/// The address a request carries when nothing forwarded it: this emulator serves its API
/// over the loopback interface.
const LOOPBACK_NETWORK: &str = "127.0.0.0/8";

/// The MQTT client id the questions below are asked about.
const CLIENT_ID: &str = "lamp-9";

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

    /// The first `authResults` entry, read as a decision.
    fn decision(&self, case: &str) -> Decided {
        let results = self.results();
        let first = results
            .first()
            .unwrap_or_else(|| panic!("case {case}: expected one authResult, got {:?}", self.body));
        Decided::of(case, first)
    }
}

/// One `AuthResult`, reduced to the four things a caller branches on.
#[derive(Debug)]
struct Decided {
    decision: String,
    missing_context_values: Vec<String>,
    allowed: Vec<String>,
    explicit_deny: Vec<String>,
    implicit_deny: Vec<String>,
}

impl Decided {
    /// Read one result, asserting both `denied` branches and `allowed` are present
    /// structures carrying a present `policies` list — the composition of an evaluated
    /// result, never an absent member.
    fn of(case: &str, result: &Value) -> Self {
        Self {
            decision: result
                .get("authDecision")
                .and_then(Value::as_str)
                .unwrap_or_else(|| {
                    panic!("case {case}: a result carries an authDecision, got {result:?}")
                })
                .to_string(),
            missing_context_values: result
                .get("missingContextValues")
                .and_then(Value::as_array)
                .unwrap_or_else(|| {
                    panic!("case {case}: a result carries missingContextValues, got {result:?}")
                })
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect(),
            allowed: branch_policies(case, result, &["allowed"]),
            explicit_deny: branch_policies(case, result, &["denied", "explicitDeny"]),
            implicit_deny: branch_policies(case, result, &["denied", "implicitDeny"]),
        }
    }
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
            policy
                .get("policyName")
                .and_then(Value::as_str)
                .unwrap_or_else(|| {
                    panic!("case {case}: a reported policy carries a policyName, got {policy:?}")
                })
                .to_string()
        })
        .collect()
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

    /// One request, optionally stating the client it was forwarded for — the hop that
    /// decides the address the simulated connection is authorized from.
    async fn call(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
        forwarded_for: Option<&str>,
    ) -> Reply {
        let payload = body.map_or_else(Vec::new, |value| {
            serde_json::to_vec(value).expect("a request body serializes")
        });
        let mut request = http::Request::builder()
            .method(method)
            .uri(format!("https://iot.{REGION}.amazonaws.com{path}"))
            .header("authorization", signed_for_iot())
            .header("content-type", "application/json");
        if let Some(address) = forwarded_for {
            request = request.header("x-forwarded-for", address);
        }
        let request = request
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
            None,
        )
        .await
    }

    /// `CreateKeysAndCertificate`: `POST /keys-and-certificate`, which issues the
    /// certificate a policy is attached to.
    async fn create_certificate(&self) -> Reply {
        self.call("POST", "/keys-and-certificate?setAsActive=true", None, None)
            .await
    }

    /// `AttachPolicy`: `PUT /target-policies/{policyName}` carrying the target.
    async fn attach_policy(&self, policy_name: &str, target: &str) -> Reply {
        self.call(
            "PUT",
            &format!("/target-policies/{policy_name}"),
            Some(&json!({ "target": target })),
            None,
        )
        .await
    }

    /// `TestAuthorization`: `POST /test-authorization`, whose `clientId` is `@httpQuery`,
    /// asked over a connection forwarded for `forwarded_for`.
    async fn test_authorization(
        &self,
        request: &Value,
        client_id: Option<&str>,
        forwarded_for: Option<&str>,
    ) -> Reply {
        let path = match client_id {
            Some(client_id) => format!("/test-authorization?clientId={client_id}"),
            None => "/test-authorization".to_string(),
        };
        self.call("POST", &path, Some(request), forwarded_for).await
    }

    /// One question about `principal`, asked over a connection forwarded for
    /// `forwarded_for`.
    async fn ask(
        &self,
        principal: &str,
        action_type: &str,
        resources: &[String],
        client_id: Option<&str>,
        forwarded_for: Option<&str>,
    ) -> Reply {
        self.test_authorization(
            &json!({
                "principal": principal,
                "authInfos": [{ "actionType": action_type, "resources": resources }],
            }),
            client_id,
            forwarded_for,
        )
        .await
    }
}

/// The ARN of an MQTT client id, the namespace a connection is authorized against.
fn client_arn(client_id: &str) -> String {
    format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:client/{client_id}")
}

/// The ARN of an MQTT topic, the namespace a publication or a delivery is authorized
/// against.
fn topic_arn(topic: &str) -> String {
    format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:topic/{topic}")
}

/// The ARN of an MQTT topic filter, the namespace a subscription is authorized against.
fn topic_filter_arn(filter: &str) -> String {
    format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:topicfilter/{filter}")
}

/// The four actions an `ActionType` asks about, so one document covers every question the
/// tables below ask.
fn every_action() -> Value {
    json!(["iot:Connect", "iot:Publish", "iot:Subscribe", "iot:Receive"])
}

/// A document granting every action on every resource, under `condition` where the row
/// states one.
fn allow_with(condition: Option<&Value>) -> Value {
    let mut statement = json!({
        "Effect": "Allow",
        "Action": every_action(),
        "Resource": "*",
    });
    if let Some(condition) = condition {
        statement["Condition"] = condition.clone();
    }
    json!({ "Version": "2012-10-17", "Statement": [statement] })
}

/// A document pairing an UNCONDITIONAL grant of every action with a `Deny` of the same
/// actions under `condition`: the shape a policy that pins a connection to a condition is
/// written in, and the one that tells an explicit refusal from an undecidable one.
fn allow_plus_deny_when(condition: &Value) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [
            { "Effect": "Allow", "Action": every_action(), "Resource": "*" },
            {
                "Effect": "Deny",
                "Action": every_action(),
                "Resource": "*",
                "Condition": condition.clone(),
            },
        ],
    })
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

/// One row of the answered-key table: a condition over a key the authorizer answers for,
/// the connection it is decided over, and the decision the rule gives it.
struct Answered {
    label: &'static str,
    /// The `Condition` block the attached statement carries.
    condition: Value,
    /// The document the condition is written into: a grant alone, or a grant plus a `Deny`
    /// the condition narrows.
    deny: bool,
    /// `TestAuthorization$clientId`.
    client_id: Option<&'static str>,
    /// The client the request states it was forwarded for, or `None` for a request nothing
    /// forwarded.
    forwarded_for: Option<&'static str>,
    /// The `authDecision` the rule gives it.
    decision: &'static str,
}

/// A clause over a key the authorizer ANSWERS for is compared against that value: the
/// decision follows the comparison, and `missingContextValues` is empty whichever way the
/// comparison went.
#[tokio::test]
async fn a_condition_over_an_answered_key_is_decided_against_its_value() {
    let rows = vec![
        Answered {
            label: "the address the connection was forwarded for, in range",
            condition: json!({ "IpAddress": { "aws:SourceIp": CALLER_NETWORK } }),
            deny: false,
            client_id: None,
            forwarded_for: Some(CALLER_ADDRESS),
            decision: "ALLOWED",
        },
        Answered {
            label: "the address the connection was forwarded for, out of range",
            condition: json!({ "IpAddress": { "aws:SourceIp": OTHER_NETWORK } }),
            deny: false,
            client_id: None,
            forwarded_for: Some(CALLER_ADDRESS),
            decision: "IMPLICIT_DENY",
        },
        Answered {
            label: "an address list naming the range the connection is in",
            condition: json!({ "IpAddress": { "aws:SourceIp": [OTHER_NETWORK, CALLER_NETWORK] } }),
            deny: false,
            client_id: None,
            forwarded_for: Some(CALLER_ADDRESS),
            decision: "ALLOWED",
        },
        Answered {
            label: "the negated address comparison over a range the connection is outside",
            condition: json!({ "NotIpAddress": { "aws:SourceIp": OTHER_NETWORK } }),
            deny: false,
            client_id: None,
            forwarded_for: Some(CALLER_ADDRESS),
            decision: "ALLOWED",
        },
        Answered {
            label: "the negated address comparison over the range the connection is in",
            condition: json!({ "NotIpAddress": { "aws:SourceIp": CALLER_NETWORK } }),
            deny: false,
            client_id: None,
            forwarded_for: Some(CALLER_ADDRESS),
            decision: "IMPLICIT_DENY",
        },
        Answered {
            label: "the loopback address a request nothing forwarded is made from",
            condition: json!({ "IpAddress": { "aws:SourceIp": LOOPBACK_NETWORK } }),
            deny: false,
            client_id: None,
            forwarded_for: None,
            decision: "ALLOWED",
        },
        Answered {
            label: "a forwarded request is not decided from the loopback address",
            condition: json!({ "IpAddress": { "aws:SourceIp": LOOPBACK_NETWORK } }),
            deny: false,
            client_id: None,
            forwarded_for: Some(CALLER_ADDRESS),
            decision: "IMPLICIT_DENY",
        },
        Answered {
            label: "string equality over the client id the request names, matching",
            condition: json!({ "StringEquals": { "iot:ClientId": CLIENT_ID } }),
            deny: false,
            client_id: Some(CLIENT_ID),
            forwarded_for: Some(CALLER_ADDRESS),
            decision: "ALLOWED",
        },
        Answered {
            label: "string equality over the client id the request names, not matching",
            condition: json!({ "StringEquals": { "iot:ClientId": "lamp-8" } }),
            deny: false,
            client_id: Some(CLIENT_ID),
            forwarded_for: Some(CALLER_ADDRESS),
            decision: "IMPLICIT_DENY",
        },
        Answered {
            label: "string negation over the client id the request names",
            condition: json!({ "StringNotEquals": { "iot:ClientId": "lamp-8" } }),
            deny: false,
            client_id: Some(CLIENT_ID),
            forwarded_for: Some(CALLER_ADDRESS),
            decision: "ALLOWED",
        },
        Answered {
            label: "string wildcard over the client id the request names",
            condition: json!({ "StringLike": { "iot:ClientId": "lamp-*" } }),
            deny: false,
            client_id: Some(CLIENT_ID),
            forwarded_for: Some(CALLER_ADDRESS),
            decision: "ALLOWED",
        },
        Answered {
            label: "the absence-tolerant spelling over an answered key, matching",
            condition: json!({ "StringEqualsIfExists": { "iot:ClientId": CLIENT_ID } }),
            deny: false,
            client_id: Some(CLIENT_ID),
            forwarded_for: Some(CALLER_ADDRESS),
            decision: "ALLOWED",
        },
        Answered {
            label: "the existence operator asserting an answered key is present",
            condition: json!({ "Null": { "aws:SourceIp": "false" } }),
            deny: false,
            client_id: None,
            forwarded_for: Some(CALLER_ADDRESS),
            decision: "ALLOWED",
        },
        Answered {
            label: "the existence operator asserting an answered key is absent",
            condition: json!({ "Null": { "aws:SourceIp": "true" } }),
            deny: false,
            client_id: None,
            forwarded_for: Some(CALLER_ADDRESS),
            decision: "IMPLICIT_DENY",
        },
        // The same comparisons under a `Deny` the grant sits beside: a deny whose clause
        // HOLDS is the explicit refusal, and one whose clause demonstrably does not refuses
        // nothing — neither is undecidable, so neither reports a missing key.
        Answered {
            label: "a deny whose address clause holds",
            condition: json!({ "IpAddress": { "aws:SourceIp": CALLER_NETWORK } }),
            deny: true,
            client_id: None,
            forwarded_for: Some(CALLER_ADDRESS),
            decision: "EXPLICIT_DENY",
        },
        Answered {
            label: "a deny whose address clause does not hold",
            condition: json!({ "IpAddress": { "aws:SourceIp": OTHER_NETWORK } }),
            deny: true,
            client_id: None,
            forwarded_for: Some(CALLER_ADDRESS),
            decision: "ALLOWED",
        },
        Answered {
            label: "a deny whose negated address clause holds",
            condition: json!({ "NotIpAddress": { "aws:SourceIp": OTHER_NETWORK } }),
            deny: true,
            client_id: None,
            forwarded_for: Some(CALLER_ADDRESS),
            decision: "EXPLICIT_DENY",
        },
        Answered {
            label: "a deny whose client-id clause does not hold",
            condition: json!({ "StringEquals": { "iot:ClientId": "lamp-8" } }),
            deny: true,
            client_id: Some(CLIENT_ID),
            forwarded_for: Some(CALLER_ADDRESS),
            decision: "ALLOWED",
        },
    ];

    for row in &rows {
        // One account per row, so each decision is computed from exactly one attached
        // policy and nothing a previous row left behind.
        let emulator = Emulator::new();
        let certificate = given_certificate(&emulator).await;
        let document = if row.deny {
            allow_plus_deny_when(&row.condition)
        } else {
            allow_with(Some(&row.condition))
        };
        given_attached_policy(&emulator, "pinned", &document, &certificate).await;

        let answer = emulator
            .ask(
                &certificate,
                "CONNECT",
                &[client_arn(CLIENT_ID)],
                row.client_id,
                row.forwarded_for,
            )
            .await;
        assert_eq!(
            answer.status, 200,
            "case {}: TestAuthorization must answer 200, got {} {:?}",
            row.label, answer.status, answer.body
        );
        let decided = answer.decision(row.label);
        assert_eq!(
            decided.decision, row.decision,
            "case {}: expected authDecision {:?}, got {:?} ({:?})",
            row.label, row.decision, decided.decision, answer.body
        );
        assert!(
            decided.missing_context_values.is_empty(),
            "case {}: a key the authorizer answers for is never missing, got {:?}",
            row.label,
            decided.missing_context_values
        );
        match row.decision {
            "ALLOWED" => assert_eq!(
                decided.allowed,
                vec!["pinned".to_string()],
                "case {}: expected the attached policy to allow it, got {:?}",
                row.label,
                decided.allowed
            ),
            "EXPLICIT_DENY" => assert_eq!(
                decided.explicit_deny,
                vec!["pinned".to_string()],
                "case {}: expected the attached policy under the explicit deny, got {:?}",
                row.label,
                decided.explicit_deny
            ),
            _ => assert_eq!(
                decided.implicit_deny,
                vec!["pinned".to_string()],
                "case {}: expected the attached policy under the implicit deny, got {:?}",
                row.label,
                decided.implicit_deny
            ),
        }
    }
}

/// One row of the unanswered-key table: a condition over a key the authorizer cannot answer
/// for, and the key it must report.
struct Unanswered {
    label: &'static str,
    /// The `Condition` block the attached statement carries.
    condition: Value,
    /// Whether the condition narrows a `Deny` the grant sits beside, rather than the grant
    /// itself.
    deny: bool,
    /// `TestAuthorization$clientId`.
    client_id: Option<&'static str>,
    /// The key `missingContextValues` must name.
    missing: &'static str,
}

/// A clause over a key the authorizer does NOT answer for decides nothing — whatever the
/// operator's polarity or spelling, and whichever effect the statement states: the question
/// is the implicit deny, the policy is reported under `denied.implicitDeny` with an empty
/// explicit branch, nothing is reported as allowing it, and the key is reported as missing.
#[tokio::test]
async fn a_condition_over_a_key_the_authorizer_cannot_answer_for_decides_nothing() {
    // The keys are AWS global condition keys a connection to the IoT device gateway does
    // not carry a value for (a request that did not arrive over a VPC endpoint, an action
    // no other service called, a principal in no organization), plus the IoT connection key
    // and the MQTT client id of a request that named none.
    let rows = vec![
        Unanswered {
            label: "a grant pinned to a source VPC endpoint",
            condition: json!({ "StringEquals": { "aws:SourceVpce": "vpce-0a1b2c3d4e5f67890" } }),
            deny: false,
            client_id: Some(CLIENT_ID),
            missing: "aws:SourceVpce",
        },
        Unanswered {
            label: "a deny pinned to a source VPC endpoint",
            condition: json!({ "StringEquals": { "aws:SourceVpce": "vpce-0a1b2c3d4e5f67890" } }),
            deny: true,
            client_id: Some(CLIENT_ID),
            missing: "aws:SourceVpce",
        },
        Unanswered {
            label: "a deny whose negated string clause names a source VPC endpoint",
            condition: json!({
                "StringNotEquals": { "aws:SourceVpce": "vpce-0a1b2c3d4e5f67890" },
            }),
            deny: true,
            client_id: Some(CLIENT_ID),
            missing: "aws:SourceVpce",
        },
        Unanswered {
            label: "a deny whose negated wildcard clause names every source VPC endpoint",
            condition: json!({ "StringNotLike": { "aws:SourceVpce": "vpce-*" } }),
            deny: true,
            client_id: Some(CLIENT_ID),
            missing: "aws:SourceVpce",
        },
        Unanswered {
            label: "a grant whose negated string clause names a source VPC endpoint",
            condition: json!({
                "StringNotEquals": { "aws:SourceVpce": "vpce-0a1b2c3d4e5f67890" },
            }),
            deny: false,
            client_id: Some(CLIENT_ID),
            missing: "aws:SourceVpce",
        },
        Unanswered {
            label: "a grant whose absence-tolerant clause names a source VPC endpoint",
            condition: json!({
                "StringEqualsIfExists": { "aws:SourceVpce": "vpce-0a1b2c3d4e5f67890" },
            }),
            deny: false,
            client_id: Some(CLIENT_ID),
            missing: "aws:SourceVpce",
        },
        Unanswered {
            label: "a grant asserting the source VPC endpoint key is absent",
            condition: json!({ "Null": { "aws:SourceVpce": "true" } }),
            deny: false,
            client_id: Some(CLIENT_ID),
            missing: "aws:SourceVpce",
        },
        Unanswered {
            label: "a grant pinned to a source VPC",
            condition: json!({ "StringEquals": { "aws:SourceVpc": "vpc-0123456789abcdef0" } }),
            deny: false,
            client_id: Some(CLIENT_ID),
            missing: "aws:SourceVpc",
        },
        Unanswered {
            label: "a grant pinned to the resource an action came from",
            condition: json!({
                "ArnEquals": {
                    "aws:SourceArn": format!("arn:aws:sns:{REGION}:{ACCOUNT_ID}:telemetry"),
                },
            }),
            deny: false,
            client_id: Some(CLIENT_ID),
            missing: "aws:SourceArn",
        },
        Unanswered {
            label: "a deny whose negated ARN clause names the resource an action came from",
            condition: json!({
                "ArnNotEquals": {
                    "aws:SourceArn": format!("arn:aws:sns:{REGION}:{ACCOUNT_ID}:telemetry"),
                },
            }),
            deny: true,
            client_id: Some(CLIENT_ID),
            missing: "aws:SourceArn",
        },
        Unanswered {
            label: "a grant pinned to a thing attached to the connection",
            condition: json!({ "Bool": { "iot:Connection.Thing.IsAttached": "true" } }),
            deny: false,
            client_id: Some(CLIENT_ID),
            missing: "iot:Connection.Thing.IsAttached",
        },
        Unanswered {
            label: "a grant over the client id of a request that named none",
            condition: json!({ "StringEquals": { "iot:ClientId": CLIENT_ID } }),
            deny: false,
            client_id: None,
            missing: "iot:ClientId",
        },
        Unanswered {
            label: "a deny over the client id of a request that named none",
            condition: json!({ "StringNotEquals": { "iot:ClientId": CLIENT_ID } }),
            deny: true,
            client_id: None,
            missing: "iot:ClientId",
        },
    ];

    for row in &rows {
        let emulator = Emulator::new();
        let certificate = given_certificate(&emulator).await;
        let document = if row.deny {
            allow_plus_deny_when(&row.condition)
        } else {
            allow_with(Some(&row.condition))
        };
        given_attached_policy(&emulator, "pinned", &document, &certificate).await;

        let answer = emulator
            .ask(
                &certificate,
                "PUBLISH",
                &[topic_arn("telemetry/lamp-9")],
                row.client_id,
                Some(CALLER_ADDRESS),
            )
            .await;
        assert_eq!(
            answer.status, 200,
            "case {}: TestAuthorization must answer 200, got {} {:?}",
            row.label, answer.status, answer.body
        );
        let decided = answer.decision(row.label);
        assert_eq!(
            decided.decision, "IMPLICIT_DENY",
            "case {}: expected authDecision IMPLICIT_DENY, got {:?} ({:?})",
            row.label, decided.decision, answer.body
        );
        assert_eq!(
            decided.missing_context_values,
            vec![row.missing.to_string()],
            "case {}: expected missingContextValues [{:?}], got {:?}",
            row.label,
            row.missing,
            decided.missing_context_values
        );
        assert!(
            decided.allowed.is_empty(),
            "case {}: an undecidable question is allowed by nothing, got {:?}",
            row.label,
            decided.allowed
        );
        assert!(
            decided.explicit_deny.is_empty(),
            "case {}: an undecidable statement pronounces no explicit deny, got {:?}",
            row.label,
            decided.explicit_deny
        );
        assert_eq!(
            decided.implicit_deny,
            vec!["pinned".to_string()],
            "case {}: expected the policy under the implicit deny, got {:?}",
            row.label,
            decided.implicit_deny
        );
    }
}

/// One row of the batch table: the questions one request asks, and how many answers it gets.
struct Batch {
    label: &'static str,
    /// `(actionType, resources)` per `authInfo`, in the order the request asks them.
    questions: Vec<(&'static str, Vec<String>)>,
    /// How many `authResults` entries the rule returns.
    answered: usize,
    /// The `authDecision` of the LAST entry returned.
    last_decision: &'static str,
    /// The keys the last entry reports as missing.
    last_missing: Vec<&'static str>,
}

/// The answers STOP at the first question that cannot be decided: a request whose first
/// question is undecidable is answered once however many questions it asked, one whose
/// earlier questions are decidable is answered up to and including the undecidable one, and
/// one whose every question is decidable is answered one per question.
#[tokio::test]
async fn the_batch_stops_at_the_first_question_that_cannot_be_decided() {
    let four_questions = vec![
        ("CONNECT", vec![client_arn(CLIENT_ID)]),
        ("PUBLISH", vec![topic_arn("telemetry/lamp-9")]),
        ("SUBSCRIBE", vec![topic_filter_arn("commands/lamp-9")]),
        ("RECEIVE", vec![topic_arn("commands/lamp-9")]),
    ];

    let rows = vec![
        Batch {
            label: "four questions against a policy pinned to a key nothing answers",
            questions: four_questions.clone(),
            answered: 1,
            last_decision: "IMPLICIT_DENY",
            last_missing: vec!["aws:SourceVpce"],
        },
        Batch {
            label: "two questions against a policy pinned to a key nothing answers",
            questions: four_questions[..2].to_vec(),
            answered: 1,
            last_decision: "IMPLICIT_DENY",
            last_missing: vec!["aws:SourceVpce"],
        },
    ];

    for row in &rows {
        let emulator = Emulator::new();
        let certificate = given_certificate(&emulator).await;
        given_attached_policy(
            &emulator,
            "pinned",
            &allow_with(Some(&json!({
                "StringEquals": { "aws:SourceVpce": "vpce-0a1b2c3d4e5f67890" },
            }))),
            &certificate,
        )
        .await;

        let answer = asked(&emulator, &certificate, &row.questions).await;
        assert_batch(row, &answer);
    }

    // Every question decidable: one answer per question, the 1:1 correspondence a caller
    // zips against its own list.
    let emulator = Emulator::new();
    let certificate = given_certificate(&emulator).await;
    given_attached_policy(&emulator, "open", &allow_with(None), &certificate).await;
    let decidable = Batch {
        label: "four decidable questions",
        questions: four_questions.clone(),
        answered: 4,
        last_decision: "ALLOWED",
        last_missing: Vec::new(),
    };
    let answer = asked(&emulator, &certificate, &decidable.questions).await;
    assert_batch(&decidable, &answer);

    // A decidable question AHEAD of an undecidable one: the batch stops at the undecidable
    // one, so the answers run up to and including it.
    let emulator = Emulator::new();
    let certificate = given_certificate(&emulator).await;
    given_attached_policy(
        &emulator,
        "connect-open",
        &json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Effect": "Allow",
                "Action": "iot:Connect",
                "Resource": client_arn(CLIENT_ID),
            }],
        }),
        &certificate,
    )
    .await;
    given_attached_policy(
        &emulator,
        "publish-pinned",
        &json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Effect": "Allow",
                "Action": "iot:Publish",
                "Resource": topic_arn("telemetry/*"),
                "Condition": { "StringEquals": { "aws:SourceVpce": "vpce-0a1b2c3d4e5f67890" } },
            }],
        }),
        &certificate,
    )
    .await;
    let mixed = Batch {
        label: "a decidable question ahead of an undecidable one",
        questions: four_questions,
        answered: 2,
        last_decision: "IMPLICIT_DENY",
        last_missing: vec!["aws:SourceVpce"],
    };
    let answer = asked(&emulator, &certificate, &mixed.questions).await;
    assert_batch(&mixed, &answer);
    // The question ahead of it was answered on its own terms.
    let results = answer.results();
    let first = Decided::of(mixed.label, &results[0]);
    assert_eq!(
        first.decision, "ALLOWED",
        "case {}: the question ahead of the undecidable one is decided, got {:?} ({:?})",
        mixed.label, first.decision, answer.body
    );
}

/// Ask `questions` as one `TestAuthorization` request.
async fn asked(
    emulator: &Emulator,
    certificate: &str,
    questions: &[(&'static str, Vec<String>)],
) -> Reply {
    let auth_infos: Vec<Value> = questions
        .iter()
        .map(|(action_type, resources)| json!({
            "actionType": action_type,
            "resources": resources,
        }))
        .collect();
    emulator
        .test_authorization(
            &json!({ "principal": certificate, "authInfos": auth_infos }),
            Some(CLIENT_ID),
            Some(CALLER_ADDRESS),
        )
        .await
}

/// Assert one batch answer against the row it answers.
fn assert_batch(row: &Batch, answer: &Reply) {
    assert_eq!(
        answer.status, 200,
        "case {}: TestAuthorization must answer 200, got {} {:?}",
        row.label, answer.status, answer.body
    );
    let results = answer.results();
    assert_eq!(
        results.len(),
        row.answered,
        "case {}: expected {} authResults for {} authInfos, got {:?}",
        row.label,
        row.answered,
        row.questions.len(),
        answer.body
    );
    let last = results
        .last()
        .unwrap_or_else(|| panic!("case {}: an answer carries a result, got {:?}", row.label, answer.body));
    let decided = Decided::of(row.label, last);
    assert_eq!(
        decided.decision, row.last_decision,
        "case {}: expected the last authDecision {:?}, got {:?} ({:?})",
        row.label, row.last_decision, decided.decision, answer.body
    );
    let expected_missing: Vec<String> = row
        .last_missing
        .iter()
        .map(|key| (*key).to_string())
        .collect();
    assert_eq!(
        decided.missing_context_values, expected_missing,
        "case {}: expected the last missingContextValues {:?}, got {:?}",
        row.label, expected_missing, decided.missing_context_values
    );
    // The questions BEHIND the first undecidable one are not answered at all, and the ones
    // ahead of it each carry the whole projection.
    for (at, result) in results.iter().enumerate() {
        let decided = Decided::of(row.label, result);
        assert!(
            ["ALLOWED", "EXPLICIT_DENY", "IMPLICIT_DENY"].contains(&decided.decision.as_str()),
            "case {}: result {at} carries a modelled authDecision, got {:?}",
            row.label,
            decided.decision
        );
    }
}

/// Every evaluated result carries BOTH deny branches, each with its own (possibly empty)
/// policies list: a question a policy's resource element does not name comes back with the
/// policy under `implicitDeny` and an EMPTY `explicitDeny`, which is how a caller tells a
/// refusal a grant fixes from one a `Deny` pronounced.
#[tokio::test]
async fn a_refusal_the_policy_never_named_carries_an_empty_explicit_deny_branch() {
    let emulator = Emulator::new();
    let certificate = given_certificate(&emulator).await;
    given_attached_policy(
        &emulator,
        "connect-as-lamp-9",
        &json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Effect": "Allow",
                "Action": "iot:Connect",
                "Resource": client_arn(CLIENT_ID),
            }],
        }),
        &certificate,
    )
    .await;

    // The positive control: the client id the policy names is granted, and nothing is
    // denied or missing.
    let granted = emulator
        .ask(
            &certificate,
            "CONNECT",
            &[client_arn(CLIENT_ID)],
            Some(CLIENT_ID),
            Some(CALLER_ADDRESS),
        )
        .await;
    let case = "the client id the policy names";
    let decided = granted.decision(case);
    assert_eq!(
        decided.decision, "ALLOWED",
        "case {case}: expected ALLOWED, got {:?} ({:?})",
        decided.decision, granted.body
    );
    assert_eq!(
        decided.allowed,
        vec!["connect-as-lamp-9".to_string()],
        "case {case}: expected the policy to allow it, got {:?}",
        decided.allowed
    );
    assert!(
        decided.explicit_deny.is_empty() && decided.implicit_deny.is_empty(),
        "case {case}: a granted question is denied by nothing, got {:?} / {:?}",
        decided.explicit_deny,
        decided.implicit_deny
    );

    // Another client id: the same policy states nothing about it, so the refusal is the
    // implicit one — with the explicit branch present and empty.
    for other in ["lamp-8", "lamp-90", "LAMP-9"] {
        let refused = emulator
            .ask(
                &certificate,
                "CONNECT",
                &[client_arn(other)],
                Some(other),
                Some(CALLER_ADDRESS),
            )
            .await;
        let case = format!("the client id {other:?} the policy does not name");
        let decided = refused.decision(&case);
        assert_eq!(
            decided.decision, "IMPLICIT_DENY",
            "case {case}: expected IMPLICIT_DENY, got {:?} ({:?})",
            decided.decision, refused.body
        );
        assert!(
            decided.explicit_deny.is_empty(),
            "case {case}: nothing explicitly denied it, got {:?}",
            decided.explicit_deny
        );
        assert_eq!(
            decided.implicit_deny,
            vec!["connect-as-lamp-9".to_string()],
            "case {case}: expected the policy under the implicit deny, got {:?}",
            decided.implicit_deny
        );
        assert!(
            decided.missing_context_values.is_empty(),
            "case {case}: no condition key was needed, got {:?}",
            decided.missing_context_values
        );
        assert_eq!(
            refused.results()[0].get("denied"),
            Some(&json!({
                "implicitDeny": { "policies": [{
                    "policyName": "connect-as-lamp-9",
                    "policyArn": policy_arn("connect-as-lamp-9"),
                }] },
                "explicitDeny": { "policies": [] },
            })),
            "case {case}: expected both branches present, got {:?}",
            refused.body
        );
    }
}

/// The ARN `CreatePolicy` mints for a policy of this account.
fn policy_arn(policy_name: &str) -> String {
    format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:policy/{policy_name}")
}
