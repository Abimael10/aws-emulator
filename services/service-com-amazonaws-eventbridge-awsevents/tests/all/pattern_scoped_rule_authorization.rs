// SPDX-License-Identifier: CC0-1.0

//! A call that WRITES a rule is authorized with the event-content condition keys the event
//! pattern it submits establishes — end to end over the wire through the real
//! `MultiServiceRouter`, with IAM and the token service mounted beside EventBridge.
//!
//! The RULE asserted here: writing a rule is the action `events:PutRule` on that rule's ARN,
//! and the call publishes this service's own event-content request condition keys —
//! `events:source`, `events:detail-type` and the keys under the event's `detail` document —
//! with the values the submitted pattern FIXES those fields to, one value per literal it
//! lists. So a statement scoped by one of them is DECIDED by the pattern the request carries:
//! it grants the calls whose pattern satisfies it, refuses only the calls whose pattern does
//! not, and a refused call leaves no rule behind. A field the pattern leaves free carries no
//! value for its key, so a clause asking about the absence of one is answerable; a field the
//! pattern constrains by a match expression, or inside a `$or` relationship, fixes no value
//! set at all, so a statement scoped by it stays undecided and grants nothing.
//!
//! The rows span the class rather than the one recorded case:
//!
//! * [`a_pattern_conditioned_statement_is_decided_by_the_submitted_event_pattern`] varies the
//!   condition KEY (the event's `source`, its `detail-type`, a member of its `detail`
//!   document, and a key the call publishes no value for), the condition OPERATOR (string
//!   equality, a string wildcard, the `IfExists` spelling, the existence operator), the SET
//!   qualifier (none, `ForAnyValue:`, `ForAllValues:`), the statement's EFFECT, how many
//!   literals the pattern lists for one field, how the pattern SPELLS a nested field path
//!   (nested object and dotted member name), and the two forms that fix no value set (a
//!   match expression, and a `$or` relationship — both over the field it relates and over a
//!   field named beside it).
//! * [`each_call_is_decided_against_the_pattern_that_call_submits`] holds one conditioned
//!   grant fixed and makes two calls under it, which is what makes the decision a fact of the
//!   request rather than of the principal.
//! * [`a_rule_that_submits_no_pattern_carries_no_value_for_the_event_content_keys`] writes a
//!   rule that fires on a schedule alone, which constrains no event field at all.
//!
//! The principal is an assumed ROLE, because that is the identity a least-privilege policy
//! ('this tenant may only subscribe to its own events') is attached to. Nothing is ever
//! published to the rules written here: this suite is about who may AUTHOR which rule.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The awsQuery form encoder and the XML element reader this suite needs are the ones
// `tests/all/target_dispatch.rs` owns for this test binary, so a request is built and read
// exactly one way across the crate.
use crate::target_dispatch::{form_encode, xml_text};

/// The Region every call below is signed for.
const REGION: &str = "us-east-2";

/// The access key id the account's own administrative calls present: the account holds no
/// record of it, so it resolves to the account's own published credential, which no statement
/// of this suite bounds.
const ACCOUNT_KEY: &str = "AKIDTESTTESTTEST";

/// The bus the rules below live on, and the rule the calls write.
const BUS: &str = "pattern-scoped-bus";
const RULE: &str = "subscription";

/// The role the scoped policy is attached to, the session name it is assumed under, and the
/// name of its inline policy.
const ROLE_NAME: &str = "rule-author";
const SESSION_NAME: &str = "probe";
const POLICY_NAME: &str = "author-allowed-patterns";

/// The event-content request condition keys a rule-writing call publishes, as AWS names
/// them: the event's own `source` and `detail-type`, and a member of its `detail` document.
const SOURCE_KEY: &str = "events:source";
const DETAIL_TYPE_KEY: &str = "events:detail-type";
const DETAIL_SERVICE_KEY: &str = "events:detail.service";

/// The action every call of this suite asks for.
const ACTION: &str = "events:PutRule";

/// The event source the grants below admit, and one outside them.
const ALLOWED_SOURCE: &str = "com.demo.orders";
const OTHER_SOURCE: &str = "com.other.orders";

/// The `detail-type` the grants on that key admit, and one outside them.
const ALLOWED_DETAIL_TYPE: &str = "Order Created";
const OTHER_DETAIL_TYPE: &str = "Order Cancelled";

/// EventBridge, IAM and the token service over ONE shared engine, as `core/router/src/lib.rs`
/// mounts them: the role a session is minted of is the record IAM holds, and this service
/// reads both.
struct Emulator {
    router: MultiServiceRouter,
}

fn emulator() -> Emulator {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // The suite signs nothing: the `Authorization` header carries only the credential scope,
    // which is where a service reads the Region and the caller's access key id.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_eventbridge_awsevents::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(engine),
    );
    Emulator { router }
}

/// The emulator's account id, which every ARN below carries.
fn account() -> String {
    engine::emulator_account_id().to_string()
}

/// The ARN of the rule `name` on the bus `bus`, the form this service mints — a rule on the
/// account's `default` bus carries no bus segment.
fn rule_arn(bus: &str, name: &str) -> String {
    if bus == "default" {
        format!("arn:aws:events:{REGION}:{}:rule/{name}", account())
    } else {
        format!("arn:aws:events:{REGION}:{}:rule/{bus}/{name}", account())
    }
}

/// The ARN of the IAM role the scoped policy is attached to.
fn role_arn() -> String {
    format!("arn:aws:iam::{}:role/{ROLE_NAME}", account())
}

/// One EventBridge `awsJson1_1` call presenting `access_key_id`, as its status and parsed
/// body. The access key id is what selects the principal the request speaks for.
async fn events_as(
    emulator: &Emulator,
    access_key_id: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("events.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("AWSEvents.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/{REGION}/events/\
                 aws4_request, SignedHeaders=host, Signature=0"
            ),
        )
        .body(bytes::Bytes::from(body.to_string()))
        .expect("a well-formed test request");
    let response = emulator.router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    let parsed = if payload.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&payload)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&payload).to_string()))
    };
    (status, parsed)
}

/// One EventBridge call the account's own credential makes, which must be served.
async fn events(emulator: &Emulator, case: &str, operation: &str, body: &Value) -> Value {
    let (status, answered) = events_as(emulator, ACCOUNT_KEY, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: {operation} by the account's own credential answered {status} {answered}"
    );
    answered
}

/// One awsQuery call against the service whose signing name is `signing_name`, presenting
/// `access_key_id`, as its status and raw body.
async fn query(
    emulator: &Emulator,
    access_key_id: &str,
    signing_name: &str,
    version: &str,
    action: &str,
    parameters: &[(&str, String)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={version}");
    for (name, value) in parameters {
        body.push('&');
        body.push_str(name);
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{signing_name}.amazonaws.com/"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/{REGION}/{signing_name}/\
                 aws4_request, SignedHeaders=host, Signature=0"
            ),
        )
        .body(bytes::Bytes::from(body))
        .expect("a well-formed awsQuery request");
    let response = emulator.router.dispatch(request).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).to_string(),
    )
}

/// Create the role carrying `policy` as its whole inline policy, assume it through the token
/// service, and answer `(access key id, the principal ARN STS reports)` — which is the
/// identity a refusal names.
async fn assumed_role(emulator: &Emulator, case: &str, policy: &Value) -> (String, String) {
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "AWS": format!("arn:aws:iam::{}:root", account()) },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    let (status, body) = query(
        emulator,
        ACCOUNT_KEY,
        "iam",
        "2010-05-08",
        "CreateRole",
        &[
            ("RoleName", ROLE_NAME.to_string()),
            ("AssumeRolePolicyDocument", trust),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateRole answered {status} {body}"
    );
    let (status, body) = query(
        emulator,
        ACCOUNT_KEY,
        "iam",
        "2010-05-08",
        "PutRolePolicy",
        &[
            ("RoleName", ROLE_NAME.to_string()),
            ("PolicyName", POLICY_NAME.to_string()),
            ("PolicyDocument", policy.to_string()),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: PutRolePolicy answered {status} {body}"
    );
    let (status, body) = query(
        emulator,
        ACCOUNT_KEY,
        "sts",
        "2011-06-15",
        "AssumeRole",
        &[
            ("RoleArn", role_arn()),
            ("RoleSessionName", SESSION_NAME.to_string()),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: AssumeRole answered {status} {body}"
    );
    let key = xml_text(&body, "AccessKeyId")
        .unwrap_or_else(|| panic!("case {case:?}: AssumeRole answered no AccessKeyId: {body}"));
    // The principal the session speaks for, read from the answer rather than spelled here: it
    // is what `sts:GetCallerIdentity` reports and what a refusal blames.
    let principal = xml_text(&body, "Arn")
        .unwrap_or_else(|| panic!("case {case:?}: AssumeRole answered no Arn: {body}"));
    (key, principal)
}

/// An identity-based document holding `statements`.
fn document(statements: Vec<Value>) -> Value {
    json!({ "Version": "2012-10-17", "Statement": statements })
}

/// One identity-based statement: `effect` on [`ACTION`] over every rule of the account, scoped
/// by `condition`. The resource element is left open because what the statement scopes is the
/// PATTERN rather than the rule's name.
fn statement(effect: &str, condition: Option<Value>) -> Value {
    let mut statement = json!({
        "Effect": effect,
        "Action": ACTION,
        "Resource": "*",
    });
    if let Some(condition) = condition {
        statement["Condition"] = condition;
    }
    statement
}

/// A document whose only statement is an `Allow` scoped by `condition`.
fn scoped_allow(condition: Option<Value>) -> Value {
    document(vec![statement("Allow", condition)])
}

/// A clause over `key` under `operator`, the way a policy writes one.
fn on_key(operator: &str, key: &str, value: &str) -> Value {
    json!({ operator: { key: value } })
}

/// A clause over `key` under `operator` listing several values.
fn on_key_values(operator: &str, key: &str, values: &[&str]) -> Value {
    json!({ operator: { key: values } })
}

/// Create the bus [`BUS`] the rules of this suite live on.
async fn given_bus(emulator: &Emulator, case: &str) {
    events(emulator, case, "CreateEventBus", &json!({ "Name": BUS })).await;
}

/// The wire error code of a failure body (`__type` on awsJson), without its namespace.
fn error_code(body: &Value) -> &str {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .rsplit('#')
        .next()
        .unwrap_or_default()
}

/// The `message` a failure body carries.
fn message(body: &Value) -> &str {
    body.get("message")
        .or_else(|| body.get("Message"))
        .and_then(Value::as_str)
        .unwrap_or_default()
}

/// The rule `name` of the bus `bus` as `DescribeRule` reports it, read with the account's own
/// credential so the read is never the call being refused, or `None` when no rule of that
/// name exists.
async fn stored_rule(emulator: &Emulator, case: &str, bus: &str, name: &str) -> Option<Value> {
    let (status, answered) = events_as(
        emulator,
        ACCOUNT_KEY,
        "DescribeRule",
        &json!({ "Name": name, "EventBusName": bus }),
    )
    .await;
    match status {
        200 => Some(answered),
        _ => {
            let code = error_code(&answered);
            assert_eq!(
                code, "ResourceNotFoundException",
                "case {case:?}: expected DescribeRule to report the rule or report it missing, \
                 got {status} {answered}"
            );
            None
        }
    }
}

/// Which of IAM's rules withheld the grant, which is what a refusal's closing clause names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Refusal {
    /// Nothing in force allowed the call.
    NoGrant,
    /// An explicit deny in the caller's identity-based policy.
    ExplicitDeny,
}

impl Refusal {
    /// The sentence this refusal carries for `principal` asking for [`ACTION`] on the rule
    /// `arn`.
    fn message(self, principal: &str, arn: &str) -> String {
        let reason = match self {
            Self::NoGrant => format!("because no identity-based policy allows the {ACTION} action"),
            Self::ExplicitDeny => "with an explicit deny in an identity-based policy".to_string(),
        };
        format!("User: {principal} is not authorized to perform: {ACTION} on resource: {arn} {reason}")
    }
}

/// One row: the document in force on the caller, the pattern the call submits, and the
/// verdict.
struct Row {
    case: &'static str,
    /// The caller's whole identity-based document.
    policy: Value,
    /// The `EventPattern` the call carries.
    pattern: &'static str,
    /// The refusal the call earns, or `None` when it is served.
    refusal: Option<Refusal>,
}

/// The rows. They differ along what the rule generalises over: which event-content key the
/// statement is scoped by, the condition operator, the set qualifier, the statement's effect,
/// how many literals the pattern lists for a field, how the pattern spells a nested field
/// path, and the pattern forms that fix no value set at all.
///
/// The operators are IAM's own (`StringEquals`/`StringLike`, the `IfExists` spelling, the
/// existence operator `Null`) and the set qualifiers `ForAllValues:`/`ForAnyValue:`.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "StringEquals on the one source the pattern names",
            policy: scoped_allow(Some(on_key("StringEquals", SOURCE_KEY, ALLOWED_SOURCE))),
            pattern: r#"{"source":["com.demo.orders"]}"#,
            refusal: None,
        },
        Row {
            case: "StringEquals on a pattern naming another source",
            policy: scoped_allow(Some(on_key("StringEquals", SOURCE_KEY, ALLOWED_SOURCE))),
            pattern: r#"{"source":["com.other.orders"]}"#,
            refusal: Some(Refusal::NoGrant),
        },
        Row {
            case: "StringLike over a source prefix the pattern satisfies",
            policy: scoped_allow(Some(on_key("StringLike", SOURCE_KEY, "com.demo.*"))),
            pattern: r#"{"source":["com.demo.orders"]}"#,
            refusal: None,
        },
        Row {
            case: "StringLike over a source prefix the pattern falls outside",
            policy: scoped_allow(Some(on_key("StringLike", SOURCE_KEY, "com.demo.*"))),
            pattern: r#"{"source":["com.other.orders"]}"#,
            refusal: Some(Refusal::NoGrant),
        },
        Row {
            case: "ForAllValues:StringEquals over two sources, one of them outside the grant",
            policy: scoped_allow(Some(on_key_values(
                "ForAllValues:StringEquals",
                SOURCE_KEY,
                &[ALLOWED_SOURCE],
            ))),
            pattern: r#"{"source":["com.demo.orders","com.other.orders"]}"#,
            refusal: Some(Refusal::NoGrant),
        },
        Row {
            case: "ForAllValues:StringEquals over two sources it covers",
            policy: scoped_allow(Some(on_key_values(
                "ForAllValues:StringEquals",
                SOURCE_KEY,
                &[ALLOWED_SOURCE, OTHER_SOURCE],
            ))),
            pattern: r#"{"source":["com.demo.orders","com.other.orders"]}"#,
            refusal: None,
        },
        Row {
            case: "ForAnyValue:StringEquals over two sources, one of which it covers",
            policy: scoped_allow(Some(on_key_values(
                "ForAnyValue:StringEquals",
                SOURCE_KEY,
                &[ALLOWED_SOURCE],
            ))),
            pattern: r#"{"source":["com.other.orders","com.demo.orders"]}"#,
            refusal: None,
        },
        Row {
            // Two literals give the key two values, which a single-valued comparison states
            // nothing about: the statement is undecided, and an undecided `Allow` is no grant.
            case: "an unqualified StringEquals over two sources, which it cannot compare",
            policy: scoped_allow(Some(on_key("StringEquals", SOURCE_KEY, ALLOWED_SOURCE))),
            pattern: r#"{"source":["com.demo.orders","com.other.orders"]}"#,
            refusal: Some(Refusal::NoGrant),
        },
        Row {
            case: "StringEquals on the detail-type the pattern names",
            policy: scoped_allow(Some(on_key(
                "StringEquals",
                DETAIL_TYPE_KEY,
                ALLOWED_DETAIL_TYPE,
            ))),
            pattern: r#"{"source":["com.demo.orders"],"detail-type":["Order Created"]}"#,
            refusal: None,
        },
        Row {
            case: "StringEquals on a pattern naming another detail-type",
            policy: scoped_allow(Some(on_key(
                "StringEquals",
                DETAIL_TYPE_KEY,
                OTHER_DETAIL_TYPE,
            ))),
            pattern: r#"{"source":["com.demo.orders"],"detail-type":["Order Created"]}"#,
            refusal: Some(Refusal::NoGrant),
        },
        Row {
            case: "StringEquals on a detail member the pattern nests as an object",
            policy: scoped_allow(Some(on_key("StringEquals", DETAIL_SERVICE_KEY, "orders"))),
            pattern: r#"{"detail":{"service":["orders"]}}"#,
            refusal: None,
        },
        Row {
            // The same constraint written as the path it spells: one member name carrying the
            // separator is the nested object, so the key takes the same value either way.
            case: "StringEquals on a detail member the pattern spells as a dotted path",
            policy: scoped_allow(Some(on_key("StringEquals", DETAIL_SERVICE_KEY, "orders"))),
            pattern: r#"{"detail.service":["orders"]}"#,
            refusal: None,
        },
        Row {
            case: "StringEquals on a pattern naming another detail member value",
            policy: scoped_allow(Some(on_key("StringEquals", DETAIL_SERVICE_KEY, "orders"))),
            pattern: r#"{"detail":{"service":["billing"]}}"#,
            refusal: Some(Refusal::NoGrant),
        },
        Row {
            // A match expression admits values the pattern does not list, so it fixes no value
            // set: the clause cannot be decided, and an undecided `Allow` grants nothing.
            case: "a source fixed by a match expression rather than by literals",
            policy: scoped_allow(Some(on_key("StringEquals", SOURCE_KEY, ALLOWED_SOURCE))),
            pattern: r#"{"source":[{"prefix":"com.demo."}]}"#,
            refusal: Some(Refusal::NoGrant),
        },
        Row {
            // Each branch of a `$or` constrains the field its own way, so the pattern fixes no
            // one value set for it either.
            case: "a source constrained inside a $or relationship",
            policy: scoped_allow(Some(on_key("StringEquals", SOURCE_KEY, ALLOWED_SOURCE))),
            pattern: r#"{"$or":[{"source":["com.demo.orders"]},{"detail-type":["Order Created"]}]}"#,
            refusal: Some(Refusal::NoGrant),
        },
        Row {
            // The relationship holds when ANY branch matches, so no field the pattern names
            // outside it is a requirement on every event it admits either: such a pattern
            // publishes no event-content key at all, which withholds a grant rather than
            // giving one.
            case: "a source fixed to a literal beside a $or relationship",
            policy: scoped_allow(Some(on_key("StringEquals", SOURCE_KEY, ALLOWED_SOURCE))),
            pattern: r#"{"source":["com.demo.orders"],"$or":[{"detail-type":["Order Created"]},{"detail":{"service":["orders"]}}]}"#,
            refusal: Some(Refusal::NoGrant),
        },
        Row {
            // A field the pattern leaves free carries no value for its key, and a plain
            // comparison over a key the request carries no value for does not hold.
            case: "StringEquals on a source the pattern leaves free",
            policy: scoped_allow(Some(on_key("StringEquals", SOURCE_KEY, ALLOWED_SOURCE))),
            pattern: r#"{"detail-type":["Order Created"]}"#,
            refusal: Some(Refusal::NoGrant),
        },
        Row {
            // "If the condition key is present in the request context, process the key as
            // specified in the policy; if the key is not present, evaluate the condition
            // element as true" — which is answerable only because the call publishes the key.
            case: "StringEqualsIfExists on a source the pattern leaves free",
            policy: scoped_allow(Some(on_key(
                "StringEqualsIfExists",
                SOURCE_KEY,
                ALLOWED_SOURCE,
            ))),
            pattern: r#"{"detail-type":["Order Created"]}"#,
            refusal: None,
        },
        Row {
            case: "Null asserting the source key is absent, over a pattern that leaves it free",
            policy: scoped_allow(Some(on_key("Null", SOURCE_KEY, "true"))),
            pattern: r#"{"detail-type":["Order Created"]}"#,
            refusal: None,
        },
        Row {
            case: "Null asserting the source key is absent, over a pattern that names a source",
            policy: scoped_allow(Some(on_key("Null", SOURCE_KEY, "true"))),
            pattern: r#"{"source":["com.demo.orders"]}"#,
            refusal: Some(Refusal::NoGrant),
        },
        Row {
            case: "Null asserting the source key is present, over a pattern that names a source",
            policy: scoped_allow(Some(on_key("Null", SOURCE_KEY, "false"))),
            pattern: r#"{"source":["com.demo.orders"]}"#,
            refusal: None,
        },
        Row {
            case: "an unconditioned Allow with a Deny scoped to one source, over another source",
            policy: document(vec![
                statement("Allow", None),
                statement(
                    "Deny",
                    Some(on_key("StringEquals", SOURCE_KEY, OTHER_SOURCE)),
                ),
            ]),
            pattern: r#"{"source":["com.demo.orders"]}"#,
            refusal: None,
        },
        Row {
            case: "an unconditioned Allow with a Deny scoped to the source the pattern names",
            policy: document(vec![
                statement("Allow", None),
                statement(
                    "Deny",
                    Some(on_key("StringEquals", SOURCE_KEY, OTHER_SOURCE)),
                ),
            ]),
            pattern: r#"{"source":["com.other.orders"]}"#,
            refusal: Some(Refusal::ExplicitDeny),
        },
        Row {
            // The keys this call publishes are the event-content ones; a grant conditioned on
            // another of this service's request keys is undecided, so it grants nothing.
            case: "a grant conditioned on a key this call publishes no value for",
            policy: scoped_allow(Some(on_key(
                "ArnLike",
                "events:TargetArn",
                "arn:aws:sqs:*",
            ))),
            pattern: r#"{"source":["com.demo.orders"]}"#,
            refusal: Some(Refusal::NoGrant),
        },
    ]
}

/// RULE — a statement scoped by an event-content condition key is decided by the pattern the
/// request submits: the call whose pattern satisfies it is served 200 and the rule is readable
/// back with that pattern, and the call whose pattern does not is refused
/// `AccessDeniedException` with AWS's wording while no rule of that name exists.
#[tokio::test]
async fn a_pattern_conditioned_statement_is_decided_by_the_submitted_event_pattern() {
    for row in rows() {
        let case = row.case;
        let emulator = emulator();
        given_bus(&emulator, case).await;
        let (key, principal) = assumed_role(&emulator, case, &row.policy).await;

        let (status, answered) = events_as(
            &emulator,
            &key,
            "PutRule",
            &json!({ "Name": RULE, "EventBusName": BUS, "EventPattern": row.pattern }),
        )
        .await;
        let stored = stored_rule(&emulator, case, BUS, RULE).await;
        let arn = rule_arn(BUS, RULE);
        match row.refusal {
            Some(refusal) => {
                assert_eq!(
                    status, 400,
                    "case {case:?}: expected the call to be refused, got {status} {answered}"
                );
                let code = error_code(&answered);
                assert_eq!(
                    code, "AccessDeniedException",
                    "case {case:?}: expected the code AccessDeniedException, got {code:?} in \
                     {answered}"
                );
                let expected = refusal.message(&principal, &arn);
                let reported = message(&answered);
                assert_eq!(
                    reported, expected,
                    "case {case:?}: expected the refusal {expected:?}, got {reported:?}"
                );
                assert!(
                    stored.is_none(),
                    "case {case:?}: a refused call must write no rule, got {stored:?}"
                );
            }
            None => {
                assert_eq!(
                    status, 200,
                    "case {case:?}: expected the call to be served, got {status} {answered}"
                );
                assert_eq!(
                    answered["RuleArn"],
                    json!(arn),
                    "case {case:?}: expected the rule ARN {arn:?}, got {answered}"
                );
                let stored = stored
                    .unwrap_or_else(|| panic!("case {case:?}: the served call wrote no rule"));
                assert_eq!(
                    stored["EventPattern"],
                    json!(row.pattern),
                    "case {case:?}: expected the stored pattern {:?}, got {stored}",
                    row.pattern
                );
            }
        }
    }
}

/// RULE — the decision is a fact of the CALL: under one conditioned grant held fixed, the call
/// whose pattern the grant covers is served and the next call whose pattern it does not is
/// refused, leaving exactly the rule the first call wrote.
#[tokio::test]
async fn each_call_is_decided_against_the_pattern_that_call_submits() {
    let case = "one grant, two calls";
    let emulator = emulator();
    given_bus(&emulator, case).await;
    let policy = scoped_allow(Some(on_key("StringEquals", SOURCE_KEY, ALLOWED_SOURCE)));
    let (key, principal) = assumed_role(&emulator, case, &policy).await;

    let covered = r#"{"source":["com.demo.orders"]}"#;
    let (status, answered) = events_as(
        &emulator,
        &key,
        "PutRule",
        &json!({ "Name": "own-events", "EventBusName": BUS, "EventPattern": covered }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the covered call to be served, got {status} {answered}"
    );
    let written = stored_rule(&emulator, case, BUS, "own-events").await;
    let written = written.unwrap_or_else(|| panic!("case {case:?}: the served call wrote no rule"));
    assert_eq!(
        written["EventPattern"],
        json!(covered),
        "case {case:?}: expected the stored pattern {covered:?}, got {written}"
    );

    let (status, answered) = events_as(
        &emulator,
        &key,
        "PutRule",
        &json!({
            "Name": "other-events",
            "EventBusName": BUS,
            "EventPattern": r#"{"source":["com.other.orders"]}"#,
        }),
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected the uncovered call to be refused, got {status} {answered}"
    );
    let expected = Refusal::NoGrant.message(&principal, &rule_arn(BUS, "other-events"));
    let reported = message(&answered);
    assert_eq!(
        reported, expected,
        "case {case:?}: expected the refusal {expected:?}, got {reported:?}"
    );
    let absent = stored_rule(&emulator, case, BUS, "other-events").await;
    assert!(
        absent.is_none(),
        "case {case:?}: a refused call must write no rule, got {absent:?}"
    );
}

/// RULE — a rule that fires on a SCHEDULE alone submits no pattern, so it constrains no event
/// field: the call carries no value for the event-content keys, which a comparison over one of
/// them does not satisfy while a clause asking for its absence does.
#[tokio::test]
async fn a_rule_that_submits_no_pattern_carries_no_value_for_the_event_content_keys() {
    // A scheduled rule lives on the account's own `default` bus, so these rows are decided
    // against the ARN form a rule of that bus carries.
    let rows: [(&str, Value, Option<Refusal>); 3] = [
        (
            "StringEquals on a source a scheduled rule names none of",
            scoped_allow(Some(on_key("StringEquals", SOURCE_KEY, ALLOWED_SOURCE))),
            Some(Refusal::NoGrant),
        ),
        (
            "Null asserting the source key is absent, which a scheduled rule satisfies",
            scoped_allow(Some(on_key("Null", SOURCE_KEY, "true"))),
            None,
        ),
        (
            "StringEqualsIfExists on a source a scheduled rule names none of",
            scoped_allow(Some(on_key(
                "StringEqualsIfExists",
                SOURCE_KEY,
                ALLOWED_SOURCE,
            ))),
            None,
        ),
    ];
    for (case, policy, refusal) in rows {
        let emulator = emulator();
        let (key, principal) = assumed_role(&emulator, case, &policy).await;
        let name = "nightly";
        let (status, answered) = events_as(
            &emulator,
            &key,
            "PutRule",
            &json!({ "Name": name, "ScheduleExpression": "rate(1 day)" }),
        )
        .await;
        let stored = stored_rule(&emulator, case, "default", name).await;
        match refusal {
            Some(refusal) => {
                assert_eq!(
                    status, 400,
                    "case {case:?}: expected the call to be refused, got {status} {answered}"
                );
                let expected = refusal.message(&principal, &rule_arn("default", name));
                let reported = message(&answered);
                assert_eq!(
                    reported, expected,
                    "case {case:?}: expected the refusal {expected:?}, got {reported:?}"
                );
                assert!(
                    stored.is_none(),
                    "case {case:?}: a refused call must write no rule, got {stored:?}"
                );
            }
            None => {
                assert_eq!(
                    status, 200,
                    "case {case:?}: expected the call to be served, got {status} {answered}"
                );
                let stored = stored
                    .unwrap_or_else(|| panic!("case {case:?}: the served call wrote no rule"));
                assert_eq!(
                    stored["ScheduleExpression"],
                    json!("rate(1 day)"),
                    "case {case:?}: expected the stored schedule, got {stored}"
                );
            }
        }
    }
}
