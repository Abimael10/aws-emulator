// SPDX-License-Identifier: CC0-1.0

//! What a least-privilege caller gets back from this service, end to end over the wire through
//! the real `MultiServiceRouter`, with IAM and the token service mounted beside EventBridge on
//! ONE engine — the arrangement a function's execution role reaches this service in, since a
//! handler's own AWS SDK call is signed with a session of that role.
//!
//! * [`every_call_that_addresses_a_rule_is_authorized`] — a call that ADDRESSES a rule is the
//!   action `events:<OperationName>` on that rule's ARN, whether it reads the rule or writes
//!   it, so the identity-based policies in force on the calling principal decide it: a call no
//!   readable document allows is refused with `AccessDeniedException` and the rule is not
//!   reported. The rows vary the OPERATION (the reads of a rule beside one of its writers) and
//!   the STATEMENT in force (no grant for the service at all, a grant on this rule, a grant on
//!   another rule, a wildcard grant, an explicit deny, and a grant scoped by the rule's own
//!   tag).
//! * [`a_caller_holding_no_grant_is_refused_before_the_rule_is_resolved`] — IAM decides before
//!   the service looks at the resource, so the refusal is the same whether or not the rule the
//!   call named exists.
//! * [`a_publication_naming_a_bus_the_account_does_not_hold_is_routed_nowhere`] — a publication
//!   reports on the ENTRY rather than on its destination, so an entry naming a bus this account
//!   does not hold is ingested all the same and merely routed nowhere. The rows vary the
//!   SPELLING of the destination (omitted, a name, an ARN), whether a bus of that name exists
//!   (never created, deleted, present) and the COUNT of entries.
//! * [`a_caller_holding_no_grant_is_refused_before_the_bus_is_resolved`] — the same for a
//!   publication: `events:PutEvents` on the bus the entry names is decided before the bus is
//!   looked up, so a caller holding no grant reads that it is not authorized whether or not
//!   that bus exists.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_eventbridge_awsevents::state::ComAmazonawsEventbridgeAwseventsState;
use smithy_server_runtime::MultiServiceRouter;

// The awsQuery form encoder and the XML element reader this suite needs for its IAM and token
// service calls are the ones `tests/all/target_dispatch.rs` owns for this test binary, so a
// request is built and read exactly one way across the crate.
use crate::target_dispatch::{form_encode, xml_text};

/// The Region every call below is signed for.
const REGION: &str = "eu-west-2";

/// The access key id the account's own administrative calls present: the account holds no
/// record of it, so it resolves to the account's own published credential.
const ACCOUNT_KEY: &str = "AKIDTESTTESTTEST";

/// The bus the rule under test lives on, and the rule itself.
const BUS: &str = "orders-bus";
const RULE: &str = "on-orders";

/// A second rule name, used only as another statement's `Resource`.
const OTHER_RULE: &str = "elsewhere";

/// The tag the rule carries, which a tag-scoped statement is decided against.
const TAG_KEY: &str = "team";
const TAG_VALUE: &str = "alpha";

/// The role the policies below are attached to, the session it is assumed under, and the name
/// of its inline policy.
const ROLE_NAME: &str = "handler-execution-role";
const SESSION_NAME: &str = "probe";
const POLICY_NAME: &str = "scoped";

/// The pattern the rule under test carries.
const PATTERN: &str = r#"{"source":["com.demo.orders"]}"#;

/// EventBridge, IAM and the token service over ONE shared engine, as
/// `core/router/src/lib.rs` mounts them.
struct Emulator {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
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
        service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    Emulator { router, engine }
}

/// The emulator's account id, which every ARN below carries.
fn account() -> String {
    engine::emulator_account_id().to_string()
}

/// The ARN of the rule `name` on [`BUS`], the form this service mints.
fn rule_arn(name: &str) -> String {
    format!("arn:aws:events:{REGION}:{}:rule/{BUS}/{name}", account())
}

/// The ARN of the event bus `name`, the form this service mints.
fn bus_arn(name: &str) -> String {
    format!("arn:aws:events:{REGION}:{}:event-bus/{name}", account())
}

/// The ARN of the IAM role the policies below are attached to.
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

/// One awsQuery call against the service whose signing name is `signing_name`, as its status
/// and raw body.
async fn query(
    emulator: &Emulator,
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
                "AWS4-HMAC-SHA256 Credential={ACCOUNT_KEY}/20260101/{REGION}/{signing_name}/\
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

/// Create the role and assume it through the token service, answering `(access key id, the
/// principal ARN STS reports)` — which is the identity a refusal names.
async fn assumed_role(emulator: &Emulator) -> (String, String) {
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
        "iam",
        "2010-05-08",
        "CreateRole",
        &[
            ("RoleName", ROLE_NAME.to_string()),
            ("AssumeRolePolicyDocument", trust),
        ],
    )
    .await;
    assert_eq!(status, 200, "CreateRole answered {status} {body}");
    let (status, body) = query(
        emulator,
        "sts",
        "2011-06-15",
        "AssumeRole",
        &[
            ("RoleArn", role_arn()),
            ("RoleSessionName", SESSION_NAME.to_string()),
        ],
    )
    .await;
    assert_eq!(status, 200, "AssumeRole answered {status} {body}");
    let key = xml_text(&body, "AccessKeyId")
        .unwrap_or_else(|| panic!("AssumeRole answered no AccessKeyId: {body}"));
    // The principal the session speaks for, read from the answer rather than spelled here: it
    // is what `sts:GetCallerIdentity` reports and what a refusal blames.
    let principal =
        xml_text(&body, "Arn").unwrap_or_else(|| panic!("AssumeRole answered no Arn: {body}"));
    (key, principal)
}

/// Make `policy` the role's whole inline policy from now on.
async fn put_role_policy(emulator: &Emulator, case: &str, policy: &Value) {
    let (status, body) = query(
        emulator,
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
}

/// An identity-based document holding `statements`.
fn document(statements: Vec<Value>) -> Value {
    json!({ "Version": "2012-10-17", "Statement": statements })
}

/// One identity-based statement: `effect` on `action` over `resource`, scoped by `condition`.
fn statement(effect: &str, action: &str, resource: &str, condition: Option<Value>) -> Value {
    let mut statement = json!({
        "Effect": effect,
        "Action": action,
        "Resource": resource,
    });
    if let Some(condition) = condition {
        statement["Condition"] = condition;
    }
    statement
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

/// What the policies in force make of one call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    /// The call is served.
    Served,
    /// Nothing in force allowed it.
    NoGrant,
    /// An explicit deny in the caller's identity-based policy refused it.
    ExplicitDeny,
}

impl Outcome {
    /// The sentence this outcome carries for `principal` asking for `action` on `resource`,
    /// composed here rather than imported from the implementation.
    fn message(self, principal: &str, action: &str, resource: &str) -> String {
        let reason = match self {
            Self::Served => return String::new(),
            Self::NoGrant => format!("because no identity-based policy allows the {action} action"),
            Self::ExplicitDeny => "with an explicit deny in an identity-based policy".to_string(),
        };
        format!(
            "User: {principal} is not authorized to perform: {action} on resource: {resource} \
             {reason}"
        )
    }
}

/// Assert `answered` is the refusal `outcome` states for `action` on `resource`.
fn assert_refused(
    case: &str,
    status: u16,
    answered: &Value,
    outcome: Outcome,
    principal: &str,
    action: &str,
    resource: &str,
) {
    assert_eq!(
        status, 400,
        "case {case:?}: expected the call to be refused, got {status} {answered}"
    );
    let code = error_code(answered);
    assert_eq!(
        code, "AccessDeniedException",
        "case {case:?}: expected the code AccessDeniedException, got {code:?} in {answered}"
    );
    let expected = outcome.message(principal, action, resource);
    let reported = message(answered);
    assert_eq!(
        reported, expected,
        "case {case:?}: expected {expected:?}, got {reported:?}"
    );
}

/// RULE: a call that ADDRESSES a rule is the action `events:<OperationName>` on that rule's
/// ARN, a read of the rule as much as a write of it, so the identity-based policies in force on
/// the calling principal decide it — evaluated with the addressed rule's own tags. A call no
/// readable document allows is refused with `AccessDeniedException`, naming the principal, the
/// action and the rule, and nothing of the rule is reported.
///
/// The rows differ along the OPERATION and along the STATEMENT in force.
#[tokio::test]
async fn every_call_that_addresses_a_rule_is_authorized() {
    let emulator = emulator();
    events(&emulator, "setup", "CreateEventBus", &json!({ "Name": BUS })).await;
    events(
        &emulator,
        "setup",
        "PutRule",
        &json!({
            "Name": RULE,
            "EventBusName": BUS,
            "EventPattern": PATTERN,
            "Tags": [{ "Key": TAG_KEY, "Value": TAG_VALUE }],
        }),
    )
    .await;
    events(
        &emulator,
        "setup",
        "PutTargets",
        &json!({
            "Rule": RULE,
            "EventBusName": BUS,
            "Targets": [{ "Id": "t1", "Arn": format!("arn:aws:sqs:{REGION}:{}:probe", account()) }],
        }),
    )
    .await;
    let (session_key, principal) = assumed_role(&emulator).await;

    // (operation, the request it carries, the resource IAM judges it against). Every row
    // addresses the SAME rule: the reads of a rule, and one of its writers as the control that
    // the gate is one gate and not a second copy.
    let calls: Vec<(&str, Value, String)> = vec![
        (
            "DescribeRule",
            json!({ "Name": RULE, "EventBusName": BUS }),
            rule_arn(RULE),
        ),
        (
            "ListTargetsByRule",
            json!({ "Rule": RULE, "EventBusName": BUS }),
            rule_arn(RULE),
        ),
        (
            "ListTagsForResource",
            json!({ "ResourceARN": rule_arn(RULE) }),
            rule_arn(RULE),
        ),
        (
            "DisableRule",
            json!({ "Name": RULE, "EventBusName": BUS }),
            rule_arn(RULE),
        ),
    ];

    // (case, the statements in force — `None` where the call's own action is substituted —,
    // the outcome). The statement rows span: a role carrying grants for another service only,
    // a grant for exactly this call, a wildcard over the service's actions, a grant on another
    // rule, an explicit deny beside a full grant, a grant scoped to the rule's own tag, and one
    // scoped to a tag value the rule does not carry.
    enum Statements {
        /// Grants naming another service's actions only — the least-privilege role of a
        /// function that was given log permissions and nothing else.
        LogsOnly,
        /// An `Allow` for the call's own action on `resource`.
        AllowOn(fn() -> String),
        /// An `Allow` for every action of this service on the rule.
        AllowEveryEventsAction,
        /// An `Allow` for the call's own action on the rule, plus an explicit `Deny` for it.
        AllowAndDeny,
        /// An `Allow` for the call's own action on the rule, scoped to a resource tag.
        AllowScopedToTag(&'static str),
    }

    let rows: Vec<(&str, Statements, Outcome)> = vec![
        ("a role granting only another service", Statements::LogsOnly, Outcome::NoGrant),
        (
            "a grant for this call on this rule",
            Statements::AllowOn(|| rule_arn(RULE)),
            Outcome::Served,
        ),
        (
            "a grant for this call on every rule",
            Statements::AllowOn(|| format!("arn:aws:events:{REGION}:{}:rule/*", account())),
            Outcome::Served,
        ),
        (
            "a grant for every action of this service",
            Statements::AllowEveryEventsAction,
            Outcome::Served,
        ),
        (
            "a grant for this call on ANOTHER rule",
            Statements::AllowOn(|| rule_arn(OTHER_RULE)),
            Outcome::NoGrant,
        ),
        (
            "an explicit deny beside the grant",
            Statements::AllowAndDeny,
            Outcome::ExplicitDeny,
        ),
        (
            "a grant scoped to the tag the rule carries",
            Statements::AllowScopedToTag(TAG_VALUE),
            Outcome::Served,
        ),
        (
            "a grant scoped to a tag value the rule does not carry",
            Statements::AllowScopedToTag("beta"),
            Outcome::NoGrant,
        ),
    ];

    for (operation, body, resource) in &calls {
        let action = format!("events:{operation}");
        for (row, statements, outcome) in &rows {
            let case = format!("{operation}: {row}");
            let policy = match statements {
                Statements::LogsOnly => document(vec![statement(
                    "Allow",
                    "logs:PutLogEvents",
                    "*",
                    None,
                )]),
                Statements::AllowOn(resource_of) => {
                    document(vec![statement("Allow", &action, &resource_of(), None)])
                }
                Statements::AllowEveryEventsAction => {
                    document(vec![statement("Allow", "events:*", resource, None)])
                }
                Statements::AllowAndDeny => document(vec![
                    statement("Allow", "events:*", "*", None),
                    statement("Deny", &action, resource, None),
                ]),
                Statements::AllowScopedToTag(value) => document(vec![statement(
                    "Allow",
                    &action,
                    resource,
                    Some(json!({ "StringEquals": { format!("aws:ResourceTag/{TAG_KEY}"): value } })),
                )]),
            };
            put_role_policy(&emulator, &case, &policy).await;
            let (status, answered) = events_as(&emulator, &session_key, operation, body).await;
            match outcome {
                Outcome::Served => assert_eq!(
                    status, 200,
                    "case {case:?}: expected the call to be served, got {status} {answered}"
                ),
                refusal => {
                    assert_refused(
                        &case,
                        status,
                        &answered,
                        *refusal,
                        &principal,
                        &action,
                        resource,
                    );
                    // Nothing of the rule travels with a refusal: a caller that may not read
                    // the rule learns neither its configuration nor its targets or tags.
                    for member in ["Arn", "State", "EventPattern", "Targets", "Tags"] {
                        assert!(
                            answered.get(member).is_none(),
                            "case {case:?}: expected no {member} in a refusal, got {answered}"
                        );
                    }
                }
            }
        }
    }
}

/// RULE: IAM decides before the service looks at the resource, so a caller holding no grant
/// reads that it is not authorized whether or not the rule it named exists — the refusal names
/// the ARN such a rule would carry and never says whether the account holds one.
#[tokio::test]
async fn a_caller_holding_no_grant_is_refused_before_the_rule_is_resolved() {
    let emulator = emulator();
    events(&emulator, "setup", "CreateEventBus", &json!({ "Name": BUS })).await;
    events(
        &emulator,
        "setup",
        "PutRule",
        &json!({ "Name": RULE, "EventBusName": BUS, "EventPattern": PATTERN }),
    )
    .await;
    let (session_key, principal) = assumed_role(&emulator).await;
    put_role_policy(
        &emulator,
        "no grant",
        &document(vec![statement("Allow", "logs:PutLogEvents", "*", None)]),
    )
    .await;

    // (case, the rule the call names). The rows differ along whether the account holds it.
    let rows = [
        ("a rule that exists", RULE),
        ("a rule that was never created", "never-created"),
    ];
    for (case, rule) in rows {
        let (status, answered) = events_as(
            &emulator,
            &session_key,
            "DescribeRule",
            &json!({ "Name": rule, "EventBusName": BUS }),
        )
        .await;
        assert_refused(
            case,
            status,
            &answered,
            Outcome::NoGrant,
            &principal,
            "events:DescribeRule",
            &rule_arn(rule),
        );
    }
}

/// The ids in this service's routing log — what `PutEvents` appended for the events it really
/// published, read from state so that "nothing was published" is observed rather than inferred.
fn routed_ids(emulator: &Emulator) -> Vec<String> {
    emulator
        .engine
        .state::<ComAmazonawsEventbridgeAwseventsState>()
        .lock()
        .delivered
        .iter()
        .map(|event| event.event_id.clone())
        .collect()
}

/// RULE: a publication reports on the ENTRY rather than on its destination, so an entry naming
/// a bus this account does not hold is INGESTED all the same — 200, no failed entry, its own
/// `EventId` — and is simply routed nowhere: nothing of it reaches the routing log, while an
/// entry naming a bus that exists is routed on that bus even in the same request.
///
/// The rows differ along the spelling of the destination (omitted, a name, an ARN), whether a
/// bus of that name exists (never created, deleted, present) and the COUNT of entries.
#[tokio::test]
async fn a_publication_naming_a_bus_the_account_does_not_hold_is_routed_nowhere() {
    let emulator = emulator();
    events(&emulator, "setup", "CreateEventBus", &json!({ "Name": BUS })).await;
    // A rule matching every event, on each bus a row publishes onto, so a published event
    // reaches the routing log and "routed nowhere" is a fact that could have been otherwise.
    // The rules carry no target: this suite is about what is INGESTED and ROUTED, not about
    // delivery.
    for bus in [None, Some(BUS)] {
        let mut request = json!({
            "Name": "catch-all",
            "EventPattern": r#"{"source":[{"exists":true}]}"#,
        });
        if let Some(bus) = bus {
            request["EventBusName"] = json!(bus);
        }
        events(&emulator, "setup", "PutRule", &request).await;
    }
    // A bus that existed and was deleted: the destination is gone, which is the same fact as
    // one that never existed.
    events(
        &emulator,
        "setup",
        "CreateEventBus",
        &json!({ "Name": "temporary-bus" }),
    )
    .await;
    events(
        &emulator,
        "setup",
        "DeleteEventBus",
        &json!({ "Name": "temporary-bus" }),
    )
    .await;

    let entry = |bus: Option<&str>| {
        let mut entry = json!({
            "Source": "com.demo.orders",
            "DetailType": "Order Created",
            "Detail": r#"{"amount":1}"#,
        });
        if let Some(bus) = bus {
            entry["EventBusName"] = json!(bus);
        }
        entry
    };

    // (case, the destinations the request's entries name, how many of those entries the
    // routing log must gain).
    let orders_arn = bus_arn(BUS);
    let ghost_arn = bus_arn("ghost-bus");
    let rows: Vec<(&str, Vec<Option<&str>>, usize)> = vec![
        ("the default bus, by omission", vec![None], 1),
        ("a bus that exists, by name", vec![Some(BUS)], 1),
        ("that bus by ARN", vec![Some(orders_arn.as_str())], 1),
        ("a bus that was never created", vec![Some("ghost-bus")], 0),
        (
            "an absent bus addressed by ARN",
            vec![Some(ghost_arn.as_str())],
            0,
        ),
        ("a bus that was deleted", vec![Some("temporary-bus")], 0),
        (
            "an absent bus beside one that exists",
            vec![Some(BUS), Some("ghost-bus")],
            1,
        ),
        (
            "an absent bus before one that exists",
            vec![Some("ghost-bus"), Some(BUS)],
            1,
        ),
    ];

    for (case, destinations, expected_routed) in rows {
        let before = routed_ids(&emulator);
        let entries: Vec<Value> = destinations.iter().map(|bus| entry(*bus)).collect();
        let expected_entries = entries.len();
        let (status, answered) =
            events_as(&emulator, ACCOUNT_KEY, "PutEvents", &json!({ "Entries": entries })).await;
        let after = routed_ids(&emulator);
        assert_eq!(
            status, 200,
            "case {case:?}: expected the publication to be served, got {status} {answered}"
        );
        let results = answered["Entries"].as_array().cloned().unwrap_or_default();
        assert_eq!(
            results.len(),
            expected_entries,
            "case {case:?}: expected one result per entry ({expected_entries}), got {answered}"
        );
        assert_eq!(
            answered["FailedEntryCount"],
            json!(0),
            "case {case:?}: expected no failed entry, got {answered}"
        );
        for result in &results {
            assert!(
                result["EventId"].as_str().is_some_and(|id| !id.is_empty()),
                "case {case:?}: expected every ingested entry to carry an EventId, got {answered}"
            );
        }
        assert_eq!(
            after.len(),
            before.len() + expected_routed,
            "case {case:?}: expected the routing log to gain {expected_routed} event(s), got \
             {after:?} after {before:?}"
        );
    }
}

/// RULE: the authorization verdict is taken before the destination is resolved, which is IAM's
/// own order. A caller whose policies do not allow it to publish onto the bus it named is told
/// it is not authorized, whether or not that bus exists, so a refusal never says which buses
/// the account holds.
#[tokio::test]
async fn a_caller_holding_no_grant_is_refused_before_the_bus_is_resolved() {
    let emulator = emulator();
    events(&emulator, "setup", "CreateEventBus", &json!({ "Name": BUS })).await;
    let (session_key, principal) = assumed_role(&emulator).await;
    put_role_policy(
        &emulator,
        "no grant",
        &document(vec![statement("Allow", "logs:PutLogEvents", "*", None)]),
    )
    .await;

    // (case, the bus the entry names). The rows differ along whether the account holds it.
    let rows = [
        ("a bus that exists", BUS),
        ("a bus that was never created", "ghost-bus"),
    ];
    for (case, bus) in rows {
        let (status, answered) = events_as(
            &emulator,
            &session_key,
            "PutEvents",
            &json!({
                "Entries": [{
                    "Source": "com.demo.orders",
                    "DetailType": "Order Created",
                    "Detail": r#"{"amount":1}"#,
                    "EventBusName": bus,
                }],
            }),
        )
        .await;
        assert_refused(
            case,
            status,
            &answered,
            Outcome::NoGrant,
            &principal,
            "events:PutEvents",
            &bus_arn(bus),
        );
    }
}
