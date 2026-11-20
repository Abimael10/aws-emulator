// SPDX-License-Identifier: CC0-1.0

//! WHICH call a target-ARN condition decides, and WITH WHAT: the request condition key an
//! attaching call publishes is read off the target list THAT call submits, one value per
//! entry, beside the addressed rule's own tags — end to end over the wire (`awsJson1_1`,
//! `POST /` with `X-Amz-Target: AWSEvents.<Operation>`) through the real
//! `MultiServiceRouter` with IAM mounted beside EventBridge on one engine, so the caller's
//! documents are read where `iam:PutUserPolicy` wrote them.
//!
//! The RULE asserted here: an operation's authorization publishes that operation's OWN
//! request condition keys out of the members the request carries — for attaching targets,
//! the ARN of every entry of the submitted target list as `events:TargetArn` — so a
//! statement scoped by such a key is decided against what THAT call submits, together with
//! the resource half of the context, and a call carrying no value for the key leaves such a
//! statement undecided, which grants nothing.
//!
//! The rows span the axes `tests/all/target_arn_scoped_authorization.rs` holds fixed, so
//! the two suites together cover the class:
//!
//! * [`every_submitted_entry_publishes_its_own_target_arn_value`] varies the NUMBER of
//!   entries, from one up to the targets a rule may hold, and the POSITION of the entry the
//!   grant does not cover (first, middle, last, none).
//! * [`repointing_a_target_id_is_decided_by_the_arn_the_call_submits`] varies what is
//!   already STORED: re-pointing a target id the rule already holds is decided against the
//!   ARN the new call submits, not against the one the rule carries.
//! * [`a_target_arn_condition_is_decided_beside_the_rules_own_tags`] varies both halves of
//!   the condition context at once — the rule's `aws:ResourceTag/<key>` value and the
//!   submitted target's service — under one statement scoped by both.
//! * [`the_target_arn_key_is_published_only_by_the_call_that_carries_targets`] varies the
//!   OPERATION: the removal call names its targets by id and so carries no target ARN,
//!   leaving a statement scoped by that key undecided until an unscoped grant allows it.
//!
//! The principal is an IAM USER presenting a key `iam:CreateAccessKey` minted for it,
//! because that is the identity a least-privilege policy ('this identity may only wire
//! rules to queues') is attached to; every setup call is the account's own credential, which
//! these documents never bound. Nothing is published to the attached targets: the ARNs below
//! name resources no delivery of this suite reaches.
//!
//! The harness (the router with EventBridge and IAM on one engine) is
//! `tests/all/target_dispatch.rs`'s.

use serde_json::{json, Value};

use crate::target_dispatch::{emulator, events, events_as, iam, xml_text, Emulator, REGION};

/// The bus every rule below lives on, and the rule the targets are attached to.
const BUS: &str = "target-arn-scope-bus";
const RULE: &str = "wiring";

/// The IAM user every decided call is made as, and the name of its inline policy.
const USER: &str = "target-wirer";
const POLICY_NAME: &str = "wire-allowed-targets";

/// The pattern every rule below is written with.
const PATTERN: &str = r#"{"source":["com.demo.orders"]}"#;

/// The request condition key an attaching call publishes, as AWS names it: "the ARN of a
/// target that can be invoked by a rule".
const TARGET_ARN_KEY: &str = "events:TargetArn";

/// The actions this suite asks for: attaching targets to a rule, and removing them again.
const PUT_TARGETS: &str = "events:PutTargets";
const REMOVE_TARGETS: &str = "events:RemoveTargets";

/// The ARN pattern every grant below admits: the queue service, whichever queue.
const ANY_QUEUE: &str = "arn:aws:sqs:*";

/// The tag key the rule-tag half of the context is written against, and the two values a
/// rule of that table carries.
const TAG_KEY: &str = "tier";
const GRANTED_TIER: &str = "orders";
const OTHER_TIER: &str = "audit";

/// The account every ARN below belongs to.
fn account() -> String {
    engine::emulator_account_id().to_string()
}

/// The ARN of the rule the targets are attached to, the form this service mints.
fn rule_arn() -> String {
    format!("arn:aws:events:{REGION}:{}:rule/{BUS}/{RULE}", account())
}

/// The ARN of the IAM user the decided calls are made as, the spelling a refusal blames.
fn user_arn() -> String {
    format!("arn:aws:iam::{}:user/{USER}", account())
}

/// The ARN of an Amazon SQS queue named `name` — the target class the grants below admit.
fn queue_arn(name: &str) -> String {
    format!("arn:aws:sqs:{REGION}:{}:{name}", account())
}

/// The ARN of an Amazon SNS topic named `name` — a target of another service, which needs no
/// role either, so what decides the call is the authorization and nothing else.
fn topic_arn(name: &str) -> String {
    format!("arn:aws:sns:{REGION}:{}:{name}", account())
}

/// The ARN of a Lambda function named `name`, a third target service.
fn function_arn(name: &str) -> String {
    format!("arn:aws:lambda:{REGION}:{}:function:{name}", account())
}

/// An identity-based document holding `statements`.
fn document(statements: Vec<Value>) -> Value {
    json!({ "Version": "2012-10-17", "Statement": statements })
}

/// One identity-based statement: `effect` of `action` over the rule this suite writes,
/// scoped by `condition` when the row states one.
fn statement(effect: &str, action: &str, condition: Option<Value>) -> Value {
    let mut statement = json!({
        "Effect": effect,
        "Action": action,
        "Resource": rule_arn(),
    });
    if let Some(condition) = condition {
        statement["Condition"] = condition;
    }
    statement
}

/// A clause over the target-ARN request key under `operator`.
fn on_target_arn(operator: &str, value: &str) -> Value {
    json!({ operator: { TARGET_ARN_KEY: value } })
}

/// A document whose only statement allows attaching targets under `condition`.
fn attaching_allow(condition: Value) -> Value {
    document(vec![statement("Allow", PUT_TARGETS, Some(condition))])
}

/// Create the IAM user holding `policy` and answer the access key id a call presents as it.
async fn given_user(emulator: &Emulator, case: &str, policy: &Value) -> String {
    let (status, body) = iam(emulator, "CreateUser", &[("UserName", USER)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateUser answered {status}: {body}"
    );
    put_user_policy(emulator, case, policy).await;
    let (status, body) = iam(emulator, "CreateAccessKey", &[("UserName", USER)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateAccessKey answered {status}: {body}"
    );
    xml_text(&body, "AccessKeyId")
        .unwrap_or_else(|| panic!("case {case:?}: the minted credential carries no AccessKeyId"))
}

/// Make `policy` the user's whole inline policy from now on.
async fn put_user_policy(emulator: &Emulator, case: &str, policy: &Value) {
    let (status, body) = iam(
        emulator,
        "PutUserPolicy",
        &[
            ("UserName", USER),
            ("PolicyName", POLICY_NAME),
            ("PolicyDocument", &policy.to_string()),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: PutUserPolicy answered {status}: {body}"
    );
}

/// One EventBridge call the account's own credential makes, which must be served.
async fn served(emulator: &Emulator, case: &str, operation: &str, body: &Value) -> Value {
    let (status, answered) = events(emulator, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: {operation} by the account's own credential answered {status}: \
         {answered}"
    );
    answered
}

/// Create [`BUS`] and the rule [`RULE`] on it carrying `tags`, with no targets yet.
async fn given_rule(emulator: &Emulator, case: &str, tags: &[(&str, &str)]) {
    served(emulator, case, "CreateEventBus", &json!({ "Name": BUS })).await;
    let tag_list: Vec<Value> = tags
        .iter()
        .map(|(key, value)| json!({ "Key": key, "Value": value }))
        .collect();
    served(
        emulator,
        case,
        "PutRule",
        &json!({
            "Name": RULE,
            "EventBusName": BUS,
            "EventPattern": PATTERN,
            "Tags": tag_list,
        }),
    )
    .await;
}

/// The `(Id, Arn)` of every target the rule carries, in the order it pages them — read with
/// the account's own credential, so the read is never the call being decided.
async fn attached_targets(emulator: &Emulator, case: &str) -> Vec<(String, String)> {
    let listed = served(
        emulator,
        case,
        "ListTargetsByRule",
        &json!({ "Rule": RULE, "EventBusName": BUS }),
    )
    .await;
    let targets = listed["Targets"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: ListTargetsByRule answered no Targets: {listed}"))
        .clone();
    targets
        .iter()
        .map(|target| {
            let text = |member: &str| {
                target[member]
                    .as_str()
                    .unwrap_or_else(|| {
                        panic!("case {case:?}: a listed target carries no {member}: {target}")
                    })
                    .to_string()
            };
            (text("Id"), text("Arn"))
        })
        .collect()
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

/// The message a failure body carries.
fn message(body: &Value) -> &str {
    body.get("message")
        .or_else(|| body.get("Message"))
        .and_then(Value::as_str)
        .unwrap_or_default()
}

/// The sentence a call no readable identity-based document allows is refused with.
fn no_grant_message(action: &str) -> String {
    format!(
        "User: {} is not authorized to perform: {action} on resource: {} because no \
         identity-based policy allows the {action} action",
        user_arn(),
        rule_arn()
    )
}

/// The `Targets` member of a `PutTargets` request carrying `targets`.
fn targets_member(targets: &[(String, String)]) -> Value {
    Value::Array(
        targets
            .iter()
            .map(|(id, arn)| json!({ "Id": id, "Arn": arn }))
            .collect(),
    )
}

/// The `PutTargets` request attaching `targets` to the rule.
fn put_targets_request(targets: &[(String, String)]) -> Value {
    json!({
        "Rule": RULE,
        "EventBusName": BUS,
        "Targets": targets_member(targets),
    })
}

/// Assert the call `answered` with `status` was refused as not authorized to perform
/// `action`.
fn assert_refused(case: &str, action: &str, status: u16, answered: &Value) {
    assert_eq!(
        status, 400,
        "case {case:?}: expected the call to be refused, got {status}: {answered}"
    );
    let code = error_code(answered);
    assert_eq!(
        code, "AccessDeniedException",
        "case {case:?}: expected the code AccessDeniedException, got {code:?} in {answered}"
    );
    let expected = no_grant_message(action);
    let reported = message(answered);
    assert_eq!(
        reported, expected,
        "case {case:?}: expected the refusal {expected:?}, got {reported:?}"
    );
}

/// One row of the entry-count table: the targets the call submits and whether the grant
/// covers every one of them.
struct Entries {
    case: &'static str,
    targets: Vec<(String, String)>,
    covered: bool,
}

/// The rows: one entry up to the five a rule may hold, with the entry the grant does not
/// cover placed first, in the middle and last — and sets it covers whole.
///
/// Five is the targets a rule may carry ("each rule can have up to five (5) targets
/// associated with it at one time", `smithy_query(query="doc",shape="PutTargets")`), so
/// every row below is a request the rule's own quota admits.
fn entry_rows() -> Vec<Entries> {
    let queue = |index: usize| (format!("q{index}"), queue_arn(&format!("orders-{index}")));
    let covered = |count: usize| (1..=count).map(queue).collect::<Vec<_>>();
    let with_uncovered = |count: usize, at: usize, arn: String| {
        let mut targets = covered(count);
        targets[at] = ("other".to_string(), arn);
        targets
    };
    vec![
        Entries {
            case: "one entry the grant covers",
            targets: covered(1),
            covered: true,
        },
        Entries {
            case: "three entries the grant covers",
            targets: covered(3),
            covered: true,
        },
        Entries {
            case: "five entries the grant covers, the targets a rule may hold",
            targets: covered(5),
            covered: true,
        },
        Entries {
            case: "three entries, the FIRST outside the grant",
            targets: with_uncovered(3, 0, topic_arn("announcements")),
            covered: false,
        },
        Entries {
            case: "three entries, the MIDDLE one outside the grant",
            targets: with_uncovered(3, 1, topic_arn("announcements")),
            covered: false,
        },
        Entries {
            case: "five entries, the LAST outside the grant",
            targets: with_uncovered(5, 4, function_arn("handler")),
            covered: false,
        },
        Entries {
            case: "two entries, both outside the grant",
            targets: vec![
                ("sns".to_string(), topic_arn("announcements")),
                ("lambda".to_string(), function_arn("handler")),
            ],
            covered: false,
        },
    ]
}

/// RULE — the key carries one value per entry of the submitted target list, so a grant
/// written under `ForAllValues:` serves the call whose every entry it covers and refuses the
/// one carrying an entry it does not, wherever in the list that entry sits.
#[tokio::test]
async fn every_submitted_entry_publishes_its_own_target_arn_value() {
    for row in entry_rows() {
        let case = row.case;
        let emulator = emulator();
        given_rule(&emulator, case, &[]).await;
        let policy = attaching_allow(on_target_arn("ForAllValues:ArnLike", ANY_QUEUE));
        let key = given_user(&emulator, case, &policy).await;

        let (status, answered) =
            events_as(&emulator, &key, "PutTargets", &put_targets_request(&row.targets)).await;
        let attached = attached_targets(&emulator, case).await;
        if row.covered {
            assert_eq!(
                status, 200,
                "case {case:?}: expected the call to be served, got {status}: {answered}"
            );
            assert_eq!(
                answered["FailedEntryCount"],
                json!(0),
                "case {case:?}: expected no failed entry, got {answered}"
            );
            assert_eq!(
                attached, row.targets,
                "case {case:?}: expected the rule to carry {:?}, got {attached:?}",
                row.targets
            );
        } else {
            assert_refused(case, PUT_TARGETS, status, &answered);
            assert!(
                attached.is_empty(),
                "case {case:?}: a refused call must attach nothing, got {attached:?}"
            );
        }
    }
}

/// RULE — the values published are the ones the CALL submits: re-pointing a target id the
/// rule already holds at an ARN the grant does not cover is refused and leaves the stored
/// target as it was, while re-pointing it at another covered ARN is served and replaces that
/// target in place.
#[tokio::test]
async fn repointing_a_target_id_is_decided_by_the_arn_the_call_submits() {
    let case = "repointing one target id";
    let emulator = emulator();
    given_rule(&emulator, case, &[]).await;
    let policy = attaching_allow(on_target_arn("ForAnyValue:ArnLike", ANY_QUEUE));
    let key = given_user(&emulator, case, &policy).await;

    let first = vec![("t1".to_string(), queue_arn("orders"))];
    let (status, answered) =
        events_as(&emulator, &key, "PutTargets", &put_targets_request(&first)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the covered call to be served, got {status}: {answered}"
    );
    let attached = attached_targets(&emulator, case).await;
    assert_eq!(
        attached, first,
        "case {case:?}: expected the rule to carry {first:?}, got {attached:?}"
    );

    // The same id, now pointed at a target outside the grant: the stored ARN is covered,
    // the submitted one is not, and it is the submitted one the decision reads.
    let outside = vec![("t1".to_string(), topic_arn("announcements"))];
    let (status, answered) =
        events_as(&emulator, &key, "PutTargets", &put_targets_request(&outside)).await;
    assert_refused(case, PUT_TARGETS, status, &answered);
    let attached = attached_targets(&emulator, case).await;
    assert_eq!(
        attached, first,
        "case {case:?}: a refused re-pointing must change nothing, expected {first:?}, got \
         {attached:?}"
    );

    // The same id pointed at another ARN the grant covers: served, and the rule still holds
    // exactly one target under that id.
    let repointed = vec![("t1".to_string(), queue_arn("shipments"))];
    let (status, answered) =
        events_as(&emulator, &key, "PutTargets", &put_targets_request(&repointed)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the covered re-pointing to be served, got {status}: {answered}"
    );
    let attached = attached_targets(&emulator, case).await;
    assert_eq!(
        attached, repointed,
        "case {case:?}: expected the rule to carry {repointed:?}, got {attached:?}"
    );
}

/// One row of the two-halves table: the tag the addressed rule carries, the target the call
/// submits, and whether the statement's whole condition holds.
struct Context {
    case: &'static str,
    tier: &'static str,
    target: (String, String),
    granted: bool,
}

/// RULE — a statement is decided with the resource half of the context (the addressed rule's
/// tags) AND the request half (what the call itself submits) at once, so a grant scoped by
/// both holds only where both do.
#[tokio::test]
async fn a_target_arn_condition_is_decided_beside_the_rules_own_tags() {
    let queue = ("q".to_string(), queue_arn("orders"));
    let topic = ("sns".to_string(), topic_arn("announcements"));
    let rows = vec![
        Context {
            case: "the granted tier, a queue target",
            tier: GRANTED_TIER,
            target: queue.clone(),
            granted: true,
        },
        Context {
            case: "the granted tier, a topic target outside the allowed service",
            tier: GRANTED_TIER,
            target: topic.clone(),
            granted: false,
        },
        Context {
            case: "another tier, a queue target",
            tier: OTHER_TIER,
            target: queue.clone(),
            granted: false,
        },
        Context {
            case: "another tier, a topic target",
            tier: OTHER_TIER,
            target: topic.clone(),
            granted: false,
        },
    ];

    for row in rows {
        let case = row.case;
        let emulator = emulator();
        given_rule(&emulator, case, &[(TAG_KEY, row.tier)]).await;
        // One statement, both halves: the rule's tag and the submitted target's ARN.
        let condition = json!({
            "StringEquals": { format!("aws:ResourceTag/{TAG_KEY}"): GRANTED_TIER },
            "ForAllValues:ArnLike": { TARGET_ARN_KEY: ANY_QUEUE },
        });
        let key = given_user(&emulator, case, &attaching_allow(condition)).await;

        let targets = vec![row.target.clone()];
        let (status, answered) =
            events_as(&emulator, &key, "PutTargets", &put_targets_request(&targets)).await;
        let attached = attached_targets(&emulator, case).await;
        if row.granted {
            assert_eq!(
                status, 200,
                "case {case:?}: expected the call to be served, got {status}: {answered}"
            );
            assert_eq!(
                attached, targets,
                "case {case:?}: expected the rule to carry {targets:?}, got {attached:?}"
            );
        } else {
            assert_refused(case, PUT_TARGETS, status, &answered);
            assert!(
                attached.is_empty(),
                "case {case:?}: a refused call must attach nothing, got {attached:?}"
            );
        }
    }
}

/// RULE — a request condition key is published only by the call that carries the member it
/// is named after: the removal call names its targets by id and carries no target ARN, so a
/// statement scoped by that key is undecided for it and grants it nothing until a statement
/// that is not scoped by it allows the removal.
#[tokio::test]
async fn the_target_arn_key_is_published_only_by_the_call_that_carries_targets() {
    let case = "one conditioned grant, two operations";
    let emulator = emulator();
    given_rule(&emulator, case, &[]).await;
    // Both actions allowed, both scoped by the key only the attaching call publishes.
    let scoped = |action: &str| {
        statement(
            "Allow",
            action,
            Some(on_target_arn("ForAnyValue:ArnLike", ANY_QUEUE)),
        )
    };
    let policy = document(vec![scoped(PUT_TARGETS), scoped(REMOVE_TARGETS)]);
    let key = given_user(&emulator, case, &policy).await;

    let targets = vec![("t1".to_string(), queue_arn("orders"))];
    let (status, answered) =
        events_as(&emulator, &key, "PutTargets", &put_targets_request(&targets)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the attaching call to be served, got {status}: {answered}"
    );
    let attached = attached_targets(&emulator, case).await;
    assert_eq!(
        attached, targets,
        "case {case:?}: expected the rule to carry {targets:?}, got {attached:?}"
    );

    let removal = json!({ "Rule": RULE, "EventBusName": BUS, "Ids": ["t1"] });
    let (status, answered) = events_as(&emulator, &key, "RemoveTargets", &removal).await;
    assert_refused(case, REMOVE_TARGETS, status, &answered);
    let attached = attached_targets(&emulator, case).await;
    assert_eq!(
        attached, targets,
        "case {case:?}: a refused removal must remove nothing, expected {targets:?}, got \
         {attached:?}"
    );

    // The same removal under a grant that is not scoped by the key the call carries no value
    // for: nothing else changed, and it is served.
    put_user_policy(
        &emulator,
        case,
        &document(vec![
            scoped(PUT_TARGETS),
            statement("Allow", REMOVE_TARGETS, None),
        ]),
    )
    .await;
    let (status, answered) = events_as(&emulator, &key, "RemoveTargets", &removal).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the unscoped grant to serve the removal, got {status}: \
         {answered}"
    );
    assert_eq!(
        answered["FailedEntryCount"],
        json!(0),
        "case {case:?}: expected no failed entry, got {answered}"
    );
    let attached = attached_targets(&emulator, case).await;
    assert!(
        attached.is_empty(),
        "case {case:?}: expected the removal to leave no target, got {attached:?}"
    );
}
