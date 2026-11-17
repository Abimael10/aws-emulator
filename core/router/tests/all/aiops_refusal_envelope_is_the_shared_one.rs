// SPDX-License-Identifier: CC0-1.0

//! The `ValidationException` envelope an AIOps refusal states its clauses in.
//!
//! RULE under test: **the envelope AWS wraps a refusal's clauses in — the count of
//! violated constraints, its count word, and what joins two clauses — is ONE rendering,
//! so a clause a handler states itself and a clause the request-boundary walk found are
//! put in exactly the same envelope.** The service crate no longer spells that envelope:
//! `state::validation_envelope` delegates to `service-common`'s shared renderer, which
//! reads the count word and the separator back out of the framework's own rendering.
//!
//! The rows drive both halves of that rule over the wire, so what is asserted is what a
//! signed AWS SDK request observes:
//! - the FRAMEWORK-rendered half — the centralized constraint walk refusing a scalar
//!   member and a map value on `CreateInvestigationGroup`;
//! - the HANDLER-stated half — the map-KEY rule no boundary applies (`TagResource` and
//!   `CreateInvestigationGroup`) and the page-token rule the model does not express
//!   (`ListInvestigationGroups`).
//!
//! Each half is driven with ONE violated constraint and with TWO, which is what the
//! envelope generalises over: how many clauses there are, the count word that states that
//! number, and the separator between two of them. The envelope is DERIVED from each
//! answer — the text before the first clause, and the text between two clauses — and the
//! two halves' envelopes are then required to be equal, so a re-spelled envelope on
//! either path fails this suite even if both spellings read plausibly.
//!
//! Why it lives beside the router's suites rather than in one service crate: the
//! invariant spans three crates — the vendored framework that renders a boundary refusal,
//! the `service-common` renderer that now words a handler-stated one, and the service
//! whose operations expose both — and this is the test target that may observe all three
//! at once.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region the group the `TagResource` rows address lives in.
const REGION: &str = "us-west-2";

/// The Region every REFUSED `CreateInvestigationGroup` row signs for: its own, so a
/// refusal can only have come from the rule under test and never from the per-Region
/// group quota.
const CREATE_REGION: &str = "eu-central-1";

/// A role ARN that satisfies the modelled `RoleArn`
/// (`smithy_query(query="shape",shape="RoleArn")`: `@length` 20..2048, `^arn:.*$`).
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/AIOpsAssistantRole";

/// A role ARN below `RoleArn`'s published minimum, which the boundary walk refuses.
const SHORT_ROLE_ARN: &str = "arn:aws:iam";

/// The requirement a `roleArn` below that minimum states.
const ROLE_ARN_MIN_REQUIREMENT: &str = "Member must have length greater than or equal to 20";

/// A Chatbot configuration ARN a `chatbotNotificationChannel` entry's list may hold,
/// satisfying the modelled `ChatConfigurationArn` (`@length` 20..2048, `^arn:.*$`).
const CHAT_CONFIGURATION_ARN: &str =
    "arn:aws:chatbot::000000000000:chat-configuration/slack-channel/ops";

/// The requirements a tag KEY states, from `TagKey`'s own constraints
/// (`smithy_query(query="shape",shape="Tags")`: the map's key targets `TagKey`, `@length`
/// 1..128, `@pattern` `^([\p{L}\p{Z}\p{N}_.:/=+\-@]+)$`, reported without the anchors a
/// model spells a whole-string match with).
const TAG_KEY_REQUIREMENTS: &[&str] = &[
    "Member must have length less than or equal to 128",
    "Member must have length greater than or equal to 1",
    r"Member must satisfy regular expression pattern: ([\p{L}\p{Z}\p{N}_.:/=+\-@]+)",
];

/// The requirements a tag VALUE states, from `TagValue`
/// (`smithy_query(query="shape",shape="TagValue")`: `@length` 1..256, `@pattern`
/// `^([\p{L}\p{Z}\p{N}_.:/=+\-@]*)$`).
const TAG_VALUE_REQUIREMENTS: &[&str] = &[
    "Member must have length less than or equal to 256",
    "Member must have length greater than or equal to 1",
    r"Member must satisfy regular expression pattern: ([\p{L}\p{Z}\p{N}_.:/=+\-@]*)",
];

/// The requirements a `chatbotNotificationChannel` KEY states, from `SNSTopicArn`
/// (`smithy_query(query="shape",shape="ChatbotNotificationChannel")`: the map's key
/// targets `SNSTopicArn`, `@length` 20..2048, `@pattern` `^arn:.*$`).
const TOPIC_KEY_REQUIREMENTS: &[&str] = &[
    "Member must have length less than or equal to 2048",
    "Member must have length greater than or equal to 20",
    "Member must satisfy regular expression pattern: arn:.*",
];

/// The requirement clause a refused `nextToken` is worded with: the token is opaque, so
/// the only requirement it can state is where a valid one comes from.
const NEXT_TOKEN_REQUIREMENT: &str =
    "Member must be a pagination token returned by a previous ListInvestigationGroups call";

/// The error code every row below is refused with.
const VALIDATION_EXCEPTION: &str = "ValidationException";

/// AWS's envelope over ONE violated constraint, up to the clause itself.
const ONE_VIOLATION_PREFIX: &str = "1 validation error detected: ";

/// AWS's envelope over TWO violated constraints, up to the first clause.
const TWO_VIOLATIONS_PREFIX: &str = "2 validation errors detected: ";

/// What AWS joins two clauses of one envelope with.
const CLAUSE_SEPARATOR: &str = "; ";

/// Which rendering stated a row's clauses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Path {
    /// The clauses the centralized constraint walk found at the request boundary.
    FrameworkRendered,
    /// The clauses a handler stated itself, for a rule the model does not express.
    HandlerStated,
}

/// One row: the request, which rendering states its clauses, and the clauses it must
/// state — in any order, because the envelope and not the ordering is what is under test
/// here (`services/service-com-amazonaws-aiops-aiops/tests/all/modelled_map_key_constraints.rs`
/// owns the order).
struct Case {
    name: &'static str,
    path: Path,
    region: &'static str,
    method: &'static str,
    uri: String,
    body: Option<Value>,
    clauses: Vec<String>,
}

/// One emulator with an empty store, mounted as the router mounts it.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_aiops_aiops::EmulatedService::new(
        engine,
    ));
    router
}

/// A request signed for `aiops` in `region`, the signal the router routes on.
fn request(
    region: &str,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> http::Request<bytes::Bytes> {
    let mut builder = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("aiops.{region}.amazonaws.com"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDAIOPSTEST/20260101/{region}/aiops/aws4_request, \
                 SignedHeaders=host, Signature=0000"
            ),
        );
    let payload = match body {
        Some(value) => {
            builder = builder.header("content-type", "application/json");
            bytes::Bytes::from(serde_json::to_vec(value).expect("serialize the request body"))
        }
        None => bytes::Bytes::new(),
    };
    builder.body(payload).expect("a well-formed test request")
}

/// Status, error code, `message` and parsed body of one call.
async fn call(
    router: &MultiServiceRouter,
    region: &str,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> (u16, String, String, Value) {
    let response = router.dispatch(request(region, method, uri, body)).await;
    let status = response.status().as_u16();
    let header_type = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let raw = response.into_body();
    let parsed: Value = if raw.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&raw)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&raw).to_string()))
    };
    let code = parsed
        .get("__type")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or(header_type)
        .unwrap_or_default()
        // An error `__type` may be namespaced (`com.amazonaws.aiops#Foo`).
        .rsplit('#')
        .next()
        .unwrap_or_default()
        .to_string();
    let message = parsed
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    (status, code, message, parsed)
}

/// Percent-encode a value for use as one `@httpLabel` path segment, which is what an SDK
/// does: a non-greedy label may not carry a raw `/`.
fn label(value: &str) -> String {
    value
        .chars()
        .map(|c| match c {
            '/' => "%2F".to_string(),
            ' ' => "%20".to_string(),
            ':' => "%3A".to_string(),
            other => other.to_string(),
        })
        .collect()
}

/// One clause naming a member and the single requirement it violated.
fn member_clause(member_path: &str, requirement: &str) -> String {
    format!("Value at '{member_path}' failed to satisfy constraint: {requirement}")
}

/// One clause naming a map member whose entry KEYS violated their key shape's bundle.
fn map_key_clause(member_path: &str, requirements: &[&str]) -> String {
    member_clause(
        member_path,
        &format!("Map keys must satisfy constraint: [{}]", requirements.join(", ")),
    )
}

/// One clause naming a map member whose entry VALUES violated their value shape's bundle.
fn map_value_clause(member_path: &str, requirements: &[&str]) -> String {
    member_clause(
        member_path,
        &format!(
            "Map value must satisfy constraint: [{}]",
            requirements.join(", ")
        ),
    )
}

/// A `CreateInvestigationGroup` body carrying `members` beside the required ones.
fn create_body(members: Vec<(&str, Value)>) -> Value {
    let mut body = Map::new();
    body.insert("name".to_string(), json!("envelope-rules"));
    body.insert("roleArn".to_string(), json!(ROLE_ARN));
    for (member, value) in members {
        body.insert(member.to_string(), value);
    }
    Value::Object(body)
}

/// The envelope a refusal put `clauses` in, read off the ANSWER rather than spelled here:
/// the text before the first clause, and the text joining two of them (empty for one).
///
/// Every clause has to appear exactly where the envelope leaves room for it and nothing
/// may trail the last one, so a message carrying an extra clause, a clause twice or a
/// word after the last clause fails here rather than being mistaken for a longer
/// separator.
fn envelope_of(case: &str, message: &str, clauses: &[String]) -> (String, String) {
    let mut located: Vec<(usize, &str)> = clauses
        .iter()
        .map(|clause| {
            let at = message.find(clause.as_str()).unwrap_or_else(|| {
                panic!(
                    "case {case:?}: expected the clause {clause:?} in the message, got {message:?}"
                )
            });
            (at, clause.as_str())
        })
        .collect();
    located.sort_by_key(|(at, _)| *at);

    let (first_at, _) = located[0];
    let mut separator = String::new();
    for pair in located.windows(2) {
        let (at, clause) = pair[0];
        let (next_at, _) = pair[1];
        let ends = at + clause.len();
        assert!(
            next_at >= ends,
            "case {case:?}: two clauses overlap in {message:?}"
        );
        let between = message[ends..next_at].to_string();
        if separator.is_empty() {
            separator = between;
        } else {
            assert_eq!(
                separator, between,
                "case {case:?}: one envelope joins its clauses one way, got {separator:?} and \
                 {between:?} in {message:?}"
            );
        }
    }
    let (last_at, last) = located[located.len() - 1];
    assert_eq!(
        &message[last_at + last.len()..],
        "",
        "case {case:?}: the envelope states nothing after its last clause, got {message:?}"
    );
    (message[..first_at].to_string(), separator)
}

/// The prefix AWS's envelope over `clauses` many violated constraints opens with.
fn expected_prefix(clause_count: usize) -> &'static str {
    match clause_count {
        1 => ONE_VIOLATION_PREFIX,
        _ => TWO_VIOLATIONS_PREFIX,
    }
}

#[tokio::test]
async fn a_handler_stated_refusal_and_a_boundary_refusal_share_one_validation_envelope() {
    let router = emulator();

    // One stored group, so the `TagResource` rows address a resource that exists and are
    // refused by the rule under test rather than by resolution.
    let (status, code, message, created) = call(
        &router,
        REGION,
        "POST",
        "/investigationGroups",
        Some(&create_body(Vec::new())),
    )
    .await;
    assert_eq!(
        status, 201,
        "creating the group: expected 201, got {status} / {code} / {message:?}"
    );
    let arn = created
        .get("arn")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("creating the group: no arn in {created:?}"))
        .to_string();

    let cases = vec![
        // The framework-rendered half: one violated constraint, then two.
        Case {
            name: "CreateInvestigationGroup with a roleArn below its minimum",
            path: Path::FrameworkRendered,
            region: CREATE_REGION,
            method: "POST",
            uri: "/investigationGroups".to_string(),
            body: Some(json!({ "name": "envelope-rules", "roleArn": SHORT_ROLE_ARN })),
            clauses: vec![member_clause("roleArn", ROLE_ARN_MIN_REQUIREMENT)],
        },
        Case {
            name: "CreateInvestigationGroup with a tag value past its maximum",
            path: Path::FrameworkRendered,
            region: CREATE_REGION,
            method: "POST",
            uri: "/investigationGroups".to_string(),
            body: Some(create_body(vec![(
                "tags",
                json!({ "CostCentre-4711": "v".repeat(257) }),
            )])),
            clauses: vec![map_value_clause("tags", TAG_VALUE_REQUIREMENTS)],
        },
        Case {
            name: "CreateInvestigationGroup breaking a scalar member and a map value",
            path: Path::FrameworkRendered,
            region: CREATE_REGION,
            method: "POST",
            uri: "/investigationGroups".to_string(),
            body: Some(json!({
                "name": "envelope-rules",
                "roleArn": SHORT_ROLE_ARN,
                "tags": { "CostCentre-4711": "v".repeat(257) },
            })),
            clauses: vec![
                member_clause("roleArn", ROLE_ARN_MIN_REQUIREMENT),
                map_value_clause("tags", TAG_VALUE_REQUIREMENTS),
            ],
        },
        // The handler-stated half: the same two counts, over rules the model does not
        // express at the boundary.
        Case {
            name: "TagResource with a tag key outside its key shape",
            path: Path::HandlerStated,
            region: REGION,
            method: "POST",
            uri: format!("/tags/{}", label(&arn)),
            body: Some(json!({ "tags": { "bad key!": "v" } })),
            clauses: vec![map_key_clause("tags", TAG_KEY_REQUIREMENTS)],
        },
        Case {
            name: "ListInvestigationGroups with a token no page handed out",
            path: Path::HandlerStated,
            region: REGION,
            method: "GET",
            uri: "/investigationGroups?nextToken=not-a-token".to_string(),
            body: None,
            clauses: vec![member_clause("nextToken", NEXT_TOKEN_REQUIREMENT)],
        },
        Case {
            name: "CreateInvestigationGroup breaking both maps' key shapes",
            path: Path::HandlerStated,
            region: CREATE_REGION,
            method: "POST",
            uri: "/investigationGroups".to_string(),
            body: Some(create_body(vec![
                ("tags", json!({ "bad key!": "v" })),
                (
                    "chatbotNotificationChannel",
                    json!({ "not-an-arn-but-long-enough": [CHAT_CONFIGURATION_ARN] }),
                ),
            ])),
            clauses: vec![
                map_key_clause("tags", TAG_KEY_REQUIREMENTS),
                map_key_clause("chatbotNotificationChannel", TOPIC_KEY_REQUIREMENTS),
            ],
        },
    ];

    // The envelope each half states a given number of clauses in, as the answers show it.
    let mut observed: BTreeMap<(usize, &'static str), (String, String)> = BTreeMap::new();

    for case in &cases {
        let Case {
            name,
            path,
            region,
            method,
            uri,
            body,
            clauses,
        } = case;
        let (status, code, message, _) = call(&router, region, method, uri, body.as_ref()).await;
        assert_eq!(
            status, 400,
            "case {name:?}: expected 400, got {status} / {code} / {message:?}"
        );
        assert_eq!(
            code, VALIDATION_EXCEPTION,
            "case {name:?}: expected {VALIDATION_EXCEPTION}, got {code} / {message:?}"
        );

        let (prefix, separator) = envelope_of(name, &message, clauses);
        let expected = expected_prefix(clauses.len());
        assert_eq!(
            prefix, expected,
            "case {name:?}: expected the envelope to open with {expected:?}, got {prefix:?} in \
             {message:?}"
        );
        let expected_separator = if clauses.len() > 1 { CLAUSE_SEPARATOR } else { "" };
        assert_eq!(
            separator, expected_separator,
            "case {name:?}: expected {expected_separator:?} between two clauses, got \
             {separator:?} in {message:?}"
        );

        let half = match path {
            Path::FrameworkRendered => "the boundary walk",
            Path::HandlerStated => "a handler",
        };
        if let Some((seen_prefix, seen_separator)) = observed.get(&(clauses.len(), half)) {
            assert_eq!(
                (seen_prefix.as_str(), seen_separator.as_str()),
                (prefix.as_str(), separator.as_str()),
                "case {name:?}: {half} words one envelope over {} clauses one way, got \
                 {seen_prefix:?}/{seen_separator:?} and {prefix:?}/{separator:?}",
                clauses.len()
            );
        }
        observed.insert((clauses.len(), half), (prefix, separator));
    }

    // The rule itself: for each number of clauses, both halves answered in the SAME
    // envelope — neither the count word nor the separator depends on which rendering
    // stated the clauses.
    for clause_count in [1_usize, 2] {
        let framework = observed
            .get(&(clause_count, "the boundary walk"))
            .unwrap_or_else(|| panic!("no boundary refusal over {clause_count} clause(s) ran"));
        let handler = observed
            .get(&(clause_count, "a handler"))
            .unwrap_or_else(|| panic!("no handler-stated refusal over {clause_count} clause(s) ran"));
        assert_eq!(
            framework, handler,
            "case {clause_count} clause(s): a handler-stated refusal must state its clauses in \
             the envelope the boundary walk uses, got {handler:?} against {framework:?}"
        );
    }

    // The refusals changed nothing: the group still carries no tags and no notification
    // channel, and the listing still answers its one group.
    let (status, code, message, body) = call(
        &router,
        REGION,
        "GET",
        &format!("/tags/{}", label(&arn)),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "reading the tags back: expected 200, got {status} / {code} / {message:?}"
    );
    assert_eq!(
        body.get("tags"),
        Some(&json!({})),
        "after the refusals: expected no stored tags, got {body:?}"
    );
    let (status, code, message, body) = call(&router, REGION, "GET", "/investigationGroups", None).await;
    assert_eq!(
        status, 200,
        "listing the groups: expected 200, got {status} / {code} / {message:?}"
    );
    let listed = body
        .get("investigationGroups")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    assert_eq!(
        listed, 1,
        "after the refusals: expected the one created group, got {body:?}"
    );
}
