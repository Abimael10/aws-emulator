// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for WHAT A CLIENT TOKEN TIES: the request as the handler
//! reads it.
//!
//! The rule under test: a `clientToken` exists so that the retry of ONE request
//! is replayed instead of acted on twice, so the fingerprint the token ledger
//! compares is taken over the request as the handler READS it — every member the
//! handler normalises before acting carries the normalised value. Two spellings
//! of one request are therefore replayed under one token, while a request the
//! service acts on differently stays a different request and is refused
//! `IdempotentParameterMismatchException` (409) with the one fixed message that
//! refusal carries, having changed nothing.
//!
//! The table spans the class rather than one request: every member this service
//! normalises — `accountIds`, read as the accounts it names each once and in the
//! order it first names them, driven through BOTH association operations, the
//! `tagKeys` of `UntagResource`, read by membership, and the optional members a
//! rule create MATERIALISES a value for when the request leaves them out
//! (`description`, `priority` and `tags`) — the kind of re-spelling (an id or
//! key repeated, repeats interleaved, a list of the documented maximum size
//! naming one account, a permuted key list, an omitted member spelled as exactly
//! the value the service materialises for it) and both directions, since either
//! spelling may be the one a token is first used with. The counterpart rows are
//! requests that really do differ, including the REORDERED account list, whose
//! order an association answer reports back, and a value a create stores as
//! sent.
//!
//! Every case drives the service the way a client does — an awsJson1_0 `POST /`
//! through the same router the emulator binary mounts, using the harness
//! [`crate::automation_rule_lifecycle`] owns.

use serde_json::{json, Value};
use service_com_amazonaws_computeoptimizerautomation_computeoptimizerautomationservice::state;

use crate::account_association::stored_enrollment;
use crate::account_id_list_rules::{
    first_named_once, reported_account_ids, ASSOCIATION_OPERATIONS,
};
use crate::automation_rule_lifecycle::{
    call, call_ok, emulator, emulator_with_engine, error_code, member, minimal_rule, read_rule,
    string_member,
};
use crate::error_payloads::error_message;

/// Member account ids, each matching `AccountId`'s modelled `^[0-9]{12}$`, that
/// are not the calling account.
const MEMBER_A: &str = "111111111111";
const MEMBER_B: &str = "222222222222";
const MEMBER_C: &str = "333333333333";

/// A client token in the modelled `ClientToken` form
/// (`^[a-zA-Z0-9_-]{1,64}$`). Every row below runs on its own fresh store, so
/// one spelling of a token serves them all.
const TOKEN: &str = "request-spelling-token";

/// `n` distinct member account ids in the modelled `^[0-9]{12}$` form, for the
/// rows that re-spell a list of the documented maximum size.
fn member_account_ids(n: usize) -> Vec<String> {
    (0..n)
        .map(|index| format!("{:012}", 500_000_000_000u64 + index as u64))
        .collect()
}

fn owned(account_ids: &[&str]) -> Vec<String> {
    account_ids.iter().map(|id| (*id).to_string()).collect()
}

/// Every entry of `account_ids` spelled twice, one straight after the other —
/// the same accounts, in the same order of first naming.
fn each_twice(account_ids: &[String]) -> Vec<String> {
    account_ids
        .iter()
        .flat_map(|account_id| [account_id.clone(), account_id.clone()])
        .collect()
}

/// Pairs of `accountIds` spellings that name ONE request, as `(case, one
/// spelling, the other)`: the accounts each spelling names, and the order it
/// first names them in, are the same, so the two differ only in repeats.
///
/// The rows differ along what the rule generalises over: how many accounts are
/// named, where the repeats fall, how many times an id is repeated and how long
/// the list is — up to the documented maximum of `accountIds` ("You can specify
/// up to 50 account IDs", `state::MAX_ACCOUNT_IDS_PER_REQUEST`). The last row is
/// the plain retry, which must keep working.
fn one_request_two_spellings() -> Vec<(&'static str, Vec<String>, Vec<String>)> {
    let half_the_maximum = member_account_ids(state::MAX_ACCOUNT_IDS_PER_REQUEST / 2);
    vec![
        (
            "the-same-id-twice",
            owned(&[MEMBER_A, MEMBER_A]),
            owned(&[MEMBER_A]),
        ),
        (
            "repeats-interleaved",
            owned(&[MEMBER_A, MEMBER_B, MEMBER_A, MEMBER_B]),
            owned(&[MEMBER_A, MEMBER_B]),
        ),
        (
            "a-repeat-in-the-middle",
            owned(&[MEMBER_A, MEMBER_B, MEMBER_B, MEMBER_C]),
            owned(&[MEMBER_A, MEMBER_B, MEMBER_C]),
        ),
        (
            "the-documented-maximum-of-one-repeated-id",
            vec![MEMBER_A.to_string(); state::MAX_ACCOUNT_IDS_PER_REQUEST],
            owned(&[MEMBER_A]),
        ),
        (
            "the-documented-maximum-of-each-id-twice",
            each_twice(&half_the_maximum),
            half_the_maximum,
        ),
        (
            "the-identical-list",
            owned(&[MEMBER_A, MEMBER_B]),
            owned(&[MEMBER_A, MEMBER_B]),
        ),
    ]
}

/// Pairs of `accountIds` the service acts on DIFFERENTLY, as `(case, one
/// request, the other)`: a different set of accounts, or the same accounts in
/// another order of first naming — which an association answers in, so those
/// two really are two requests.
fn requests_that_really_differ() -> Vec<(&'static str, Vec<String>, Vec<String>)> {
    vec![
        (
            "the-accounts-reordered",
            owned(&[MEMBER_A, MEMBER_B]),
            owned(&[MEMBER_B, MEMBER_A]),
        ),
        (
            "the-accounts-reordered-with-repeats",
            owned(&[MEMBER_A, MEMBER_A, MEMBER_B]),
            owned(&[MEMBER_B, MEMBER_B, MEMBER_A]),
        ),
        (
            "an-account-added",
            owned(&[MEMBER_A]),
            owned(&[MEMBER_A, MEMBER_C]),
        ),
        (
            "another-account",
            owned(&[MEMBER_A, MEMBER_A]),
            owned(&[MEMBER_C]),
        ),
    ]
}

/// The accounts `account_ids` names, associated over the wire with no client
/// token, so a disassociation row has an association to remove and stamp.
///
/// A row's PRECONDITION: `AssociateAccounts`'s own behaviour is asserted in
/// [`crate::account_association`].
async fn associate_first(
    router: &smithy_server_runtime::MultiServiceRouter,
    account_ids: &[String],
    case: &str,
) {
    call_ok(
        router,
        "AssociateAccounts",
        &json!({"accountIds": account_ids}),
        case,
    )
    .await;
}

/// The stamp the store holds for `account_id`, as the rows compare it.
fn stamp_of(engine: &std::sync::Arc<engine::InMemoryEngine>, account_id: &str) -> Option<f64> {
    stored_enrollment(engine, account_id)
        .last_updated_timestamp
        .map(|at| at.as_secs_f64())
}

/// Two spellings of one association request are ONE request under one client
/// token: the second is replayed — the same accounts reported, in the same
/// order, and nothing acted on or stamped a second time — whichever of the two
/// spellings the token is first used with, and for BOTH operations that read an
/// `accountIds` list.
#[tokio::test]
async fn one_association_request_spelled_two_ways_replays_under_one_client_token() {
    for (row, one, other) in one_request_two_spellings() {
        for (first, retry) in [(&one, &other), (&other, &one)] {
            for operation in ASSOCIATION_OPERATIONS {
                let case = format!(
                    "{operation}/{row}: {} ids then {} ids",
                    first.len(),
                    retry.len()
                );
                let case = case.as_str();
                // One fresh store per row, so no row is judged against what
                // another left behind.
                let (router, engine) = emulator_with_engine();
                let expected = first_named_once(first);
                if operation == "DisassociateAccounts" {
                    associate_first(&router, first, case).await;
                }

                let answered = call_ok(
                    &router,
                    operation,
                    &json!({"accountIds": first, "clientToken": TOKEN}),
                    case,
                )
                .await;
                assert_eq!(
                    reported_account_ids(&answered),
                    expected,
                    "case {case:?}: expected each named account once in the order the request \
                     first names it, got {answered}"
                );
                let stamped = stamp_of(&engine, &expected[0]);

                // The retry: the same token, the other spelling of the same
                // request.
                let replay = call_ok(
                    &router,
                    operation,
                    &json!({"accountIds": retry, "clientToken": TOKEN}),
                    case,
                )
                .await;
                assert_eq!(
                    reported_account_ids(&replay),
                    expected,
                    "case {case:?}: the retry must answer the first call's result, expected \
                     {expected:?}, got {replay}"
                );
                assert_eq!(
                    stamp_of(&engine, &expected[0]),
                    stamped,
                    "case {case:?}: a replayed token must not act on the account a second time, \
                     expected the stamp {stamped:?}, got {:?}",
                    stamp_of(&engine, &expected[0])
                );
            }
        }
    }
}

/// A token first used with a request the service acts on differently — other
/// accounts, or the same accounts in another order — is still refused 409
/// `IdempotentParameterMismatchException` with the fixed message that refusal
/// carries, and the refused call stores nothing.
#[tokio::test]
async fn a_client_token_still_refuses_an_association_request_that_really_differs() {
    for (row, one, other) in requests_that_really_differ() {
        for (first, retry) in [(&one, &other), (&other, &one)] {
            for operation in ASSOCIATION_OPERATIONS {
                let case = format!("{operation}/{row}");
                let case = case.as_str();
                let (router, engine) = emulator_with_engine();
                let mut union: Vec<String> = first_named_once(first);
                for account_id in first_named_once(retry) {
                    if !union.contains(&account_id) {
                        union.push(account_id);
                    }
                }
                if operation == "DisassociateAccounts" {
                    associate_first(&router, &union, case).await;
                }

                call_ok(
                    &router,
                    operation,
                    &json!({"accountIds": first, "clientToken": TOKEN}),
                    case,
                )
                .await;
                let before: Vec<Option<f64>> = union
                    .iter()
                    .map(|account_id| stamp_of(&engine, account_id))
                    .collect();

                let (status, body) = call(
                    &router,
                    operation,
                    &json!({"accountIds": retry, "clientToken": TOKEN}),
                )
                .await;
                assert_eq!(
                    status, 409,
                    "case {case:?}: a token first used with another request is a conflict, got \
                     {status} and {body}"
                );
                assert_eq!(
                    error_code(&body),
                    "IdempotentParameterMismatchException",
                    "case {case:?}: expected the parameter-mismatch refusal, got {body}"
                );
                assert_eq!(
                    error_message(&body),
                    state::IDEMPOTENT_PARAMETER_MISMATCH_MESSAGE,
                    "case {case:?}: expected the fixed message \
                     {:?}, got {body}",
                    state::IDEMPOTENT_PARAMETER_MISMATCH_MESSAGE
                );
                let after: Vec<Option<f64>> = union
                    .iter()
                    .map(|account_id| stamp_of(&engine, account_id))
                    .collect();
                assert_eq!(
                    after, before,
                    "case {case:?}: the refused call must have acted on nothing, expected the \
                     stamps {before:?} of {union:?}, got {after:?}"
                );
            }
        }
    }
}

/// The tags every untagging row starts from: three distinct keys inside the
/// modelled `Tag` constraints, so a row can remove one, two or another two.
fn three_tags() -> Value {
    json!([
        {"key": "team", "value": "platform"},
        {"key": "env", "value": "prod"},
        {"key": "tier", "value": "gold"},
    ])
}

/// A rule carrying [`three_tags`], as `(ruleArn, ruleRevision)`.
async fn tagged_rule(
    router: &smithy_server_runtime::MultiServiceRouter,
    name: &str,
    case: &str,
) -> (String, i64) {
    let mut request = minimal_rule(name);
    request["tags"] = three_tags();
    let created = call_ok(router, "CreateAutomationRule", &request, case).await;
    (
        string_member(&created, "ruleArn"),
        member(&created, "ruleRevision")
            .as_i64()
            .expect("a created rule carries a revision"),
    )
}

/// The tag keys `ListTagsForResource` still reports for `rule_arn`, in the order
/// it reports them.
async fn remaining_tag_keys(
    router: &smithy_server_runtime::MultiServiceRouter,
    rule_arn: &str,
    case: &str,
) -> Vec<String> {
    let body = call_ok(
        router,
        "ListTagsForResource",
        &json!({"resourceArn": rule_arn}),
        case,
    )
    .await;
    member(&body, "tags")
        .as_array()
        .map(|tags| {
            tags.iter()
                .map(|tag| {
                    tag.get("key")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string()
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Pairs of `tagKeys` spellings, as `(case, one spelling, the other, whether the
/// two are one request)`.
///
/// `UntagResource` keeps every tag whose key the list does not CONTAIN, so the
/// list names a set: neither its order nor a key repeated in it changes what the
/// call removes or answers. A list naming other keys removes other tags and is
/// another request.
fn tag_key_spellings() -> Vec<(&'static str, Vec<String>, Vec<String>, bool)> {
    vec![
        (
            "the-keys-permuted",
            owned(&["team", "env"]),
            owned(&["env", "team"]),
            true,
        ),
        (
            "a-key-repeated",
            owned(&["team", "env"]),
            owned(&["env", "team", "team", "env"]),
            true,
        ),
        (
            "the-same-key-twice",
            owned(&["team", "team"]),
            owned(&["team"]),
            true,
        ),
        (
            "a-key-added",
            owned(&["team"]),
            owned(&["team", "env"]),
            false,
        ),
        ("other-keys", owned(&["team"]), owned(&["env"]), false),
    ]
}

/// The same rule over `tagKeys`: two spellings of one key SET are one request
/// and the second is replayed, while a list naming other keys is another request
/// and is refused — in both directions, and with the stored tag list unchanged
/// by the second call either way.
#[tokio::test]
async fn one_untagging_spelled_two_ways_replays_under_one_client_token() {
    for (row, one, other, same_request) in tag_key_spellings() {
        for (first, retry) in [(&one, &other), (&other, &one)] {
            let case = format!("UntagResource/{row}: {first:?} then {retry:?}");
            let case = case.as_str();
            let router = emulator();
            let (rule_arn, revision) = tagged_rule(&router, "token-spellings", case).await;

            call_ok(
                &router,
                "UntagResource",
                &json!({
                    "resourceArn": &rule_arn,
                    "ruleRevision": revision,
                    "tagKeys": first,
                    "clientToken": TOKEN,
                }),
                case,
            )
            .await;
            let left = remaining_tag_keys(&router, &rule_arn, case).await;

            let (status, body) = call(
                &router,
                "UntagResource",
                &json!({
                    "resourceArn": &rule_arn,
                    "ruleRevision": revision,
                    "tagKeys": retry,
                    "clientToken": TOKEN,
                }),
            )
            .await;
            if same_request {
                assert_eq!(
                    status, 200,
                    "case {case:?}: the two spellings name one key set, so the second call is a \
                     replay, got {status} and {body}"
                );
                // `UntagResourceOutput` models no members, so a replay answers
                // the empty document this operation always answers with.
                assert_eq!(
                    body,
                    json!({}),
                    "case {case:?}: the replay must answer the operation's modelled empty output, \
                     got {body}"
                );
            } else {
                assert_eq!(
                    status, 409,
                    "case {case:?}: a token first used to remove other keys is a conflict, got \
                     {status} and {body}"
                );
                assert_eq!(
                    error_code(&body),
                    "IdempotentParameterMismatchException",
                    "case {case:?}: expected the parameter-mismatch refusal, got {body}"
                );
                assert_eq!(
                    error_message(&body),
                    state::IDEMPOTENT_PARAMETER_MISMATCH_MESSAGE,
                    "case {case:?}: expected the fixed message {:?}, got {body}",
                    state::IDEMPOTENT_PARAMETER_MISMATCH_MESSAGE
                );
            }
            let now_left = remaining_tag_keys(&router, &rule_arn, case).await;
            assert_eq!(
                now_left, left,
                "case {case:?}: the second call must leave the first call's tag list, expected \
                 {left:?}, got {now_left:?}"
            );
        }
    }
}

/// A create request under [`TOKEN`]: the minimal rule, with `members` written
/// over it — a row states only the members it is about, so an absent one is a
/// member the request really does leave out.
fn create_request(name: &str, members: &Value) -> Value {
    let mut request = minimal_rule(name);
    for (name, value) in members
        .as_object()
        .expect("a row states its members as an object")
    {
        request[name.as_str()] = value.clone();
    }
    request["clientToken"] = json!(TOKEN);
    request
}

/// A tag inside the modelled `Tag` constraints, for the rows that carry one.
fn one_tag(value: &str) -> Value {
    json!([{"key": "team", "value": value}])
}

/// Pairs of create requests, as `(case, one request, the other, whether the two
/// are one request)`.
///
/// A create may leave `description`, `priority` and `tags` out, and the service
/// reports a value for all three for every rule it holds, so a create that
/// leaves one out mints the same rule as a create that spells it as exactly the
/// value the service materialises for it — the empty description
/// (`state::DEFAULT_RULE_DESCRIPTION`), the bottom of the precedence order
/// (`state::DEFAULT_RULE_PRIORITY`) and the empty tag list. Any other value of
/// those members is stored and reported in the spelling it arrived in, so it
/// mints a different rule and is a different request.
///
/// The rows differ along what the rule generalises over: which of the three
/// members is re-spelled, how many of them at once, whether the request carries
/// other optional members beside the re-spelled one, and — on the counterpart
/// side — a value that merely renders the same number (`"1.0"`), a value added
/// and a value replaced.
fn create_member_spellings() -> Vec<(&'static str, Value, Value, bool)> {
    let description = json!(state::DEFAULT_RULE_DESCRIPTION);
    let priority = json!(state::DEFAULT_RULE_PRIORITY);
    vec![
        (
            "description-omitted-or-spelled-as-the-materialised-default",
            json!({}),
            json!({"description": description}),
            true,
        ),
        (
            "priority-omitted-or-spelled-as-the-materialised-default",
            json!({}),
            json!({"priority": priority}),
            true,
        ),
        (
            "tags-omitted-or-spelled-as-the-empty-list",
            json!({}),
            json!({"tags": []}),
            true,
        ),
        (
            "all-three-omitted-or-spelled-as-the-materialised-defaults",
            json!({}),
            json!({"description": description, "priority": priority, "tags": []}),
            true,
        ),
        (
            "one-default-spelled-beside-members-the-request-does-carry",
            json!({"priority": "0.25", "tags": one_tag("platform")}),
            json!({
                "description": description,
                "priority": "0.25",
                "tags": one_tag("platform"),
            }),
            true,
        ),
        (
            "the-identical-request",
            json!({"description": "notes", "priority": "0.5"}),
            json!({"description": "notes", "priority": "0.5"}),
            true,
        ),
        (
            "another-description",
            json!({}),
            json!({"description": "notes"}),
            false,
        ),
        (
            "another-priority",
            json!({}),
            json!({"priority": "0.5"}),
            false,
        ),
        // A decimal the member admits that renders the same number as the
        // materialised default but is NOT its spelling: a rule created with it
        // reports `"1.0"`, so the two creates mint different rules.
        (
            "the-default-priority-spelled-with-a-fraction",
            json!({}),
            json!({"priority": "1.0"}),
            false,
        ),
        (
            "a-tag-added",
            json!({}),
            json!({"tags": one_tag("platform")}),
            false,
        ),
        (
            "another-tag-value",
            json!({"tags": one_tag("platform")}),
            json!({"tags": one_tag("core")}),
            false,
        ),
    ]
}

/// How many rules the store holds, read back over the wire.
async fn stored_rule_count(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
) -> usize {
    let listed = call_ok(
        router,
        "ListAutomationRules",
        &json!({"maxResults": 100}),
        case,
    )
    .await;
    member(&listed, "automationRules")
        .as_array()
        .map(Vec::len)
        .unwrap_or_default()
}

/// The same rule over a rule create: two spellings of one create — one that
/// omits a member the service materialises a value for, one that spells it as
/// exactly that value — are ONE request, so the second is replayed with the rule
/// the first minted, whichever of the two the token is first used with. A create
/// carrying a value the service stores as sent stays a different request and is
/// still refused 409 with the one fixed message that refusal carries. Either
/// way the store ends up holding exactly the first call's rule.
#[tokio::test]
async fn one_create_spelled_two_ways_replays_under_one_client_token() {
    for (row, one, other, same_request) in create_member_spellings() {
        for (first, retry) in [(&one, &other), (&other, &one)] {
            let case = format!("CreateAutomationRule/{row}: {first} then {retry}");
            let case = case.as_str();
            // One fresh store per row, so no row is judged against what another
            // left behind.
            let router = emulator();

            let created = call_ok(
                &router,
                "CreateAutomationRule",
                &create_request("spelled-rule", first),
                case,
            )
            .await;

            let (status, body) = call(
                &router,
                "CreateAutomationRule",
                &create_request("spelled-rule", retry),
            )
            .await;
            if same_request {
                assert_eq!(
                    status, 200,
                    "case {case:?}: the two spellings mint the identical rule, so the second call \
                     is a replay, got {status} and {body}"
                );
                assert_eq!(
                    body, created,
                    "case {case:?}: the retry must answer the rule the first call minted, \
                     expected {created}, got {body}"
                );
            } else {
                assert_eq!(
                    status, 409,
                    "case {case:?}: a token first used for a create the service acts on \
                     differently is a conflict, got {status} and {body}"
                );
                assert_eq!(
                    error_code(&body),
                    "IdempotentParameterMismatchException",
                    "case {case:?}: expected the parameter-mismatch refusal, got {body}"
                );
                assert_eq!(
                    error_message(&body),
                    state::IDEMPOTENT_PARAMETER_MISMATCH_MESSAGE,
                    "case {case:?}: expected the fixed message {:?}, got {body}",
                    state::IDEMPOTENT_PARAMETER_MISMATCH_MESSAGE
                );
            }

            let held = stored_rule_count(&router, case).await;
            assert_eq!(
                held, 1,
                "case {case:?}: the second call must neither mint a second rule nor remove the \
                 first, expected 1 stored rule, got {held}"
            );
            let rule_arn = string_member(&created, "ruleArn");
            let stored = read_rule(&router, &rule_arn, case).await;
            for name in ["description", "priority", "tags"] {
                assert_eq!(
                    member(&stored, name),
                    member(&created, name),
                    "case {case:?}: the stored rule's {name} must still be the one the first \
                     call answered, expected {:?}, got {:?}",
                    member(&created, name),
                    member(&stored, name)
                );
            }
        }
    }
}

/// The ordinary retry: a client creates a rule, reads the response, and retries
/// the create under the same token from the values it read back — so the retry
/// SPELLS the members the first request left out. That is one request and is
/// replayed, for every subset of those members the first call may have omitted.
#[tokio::test]
async fn a_create_retried_from_the_values_it_answered_replays() {
    let omitted: [&[&str]; 4] = [
        &["description"],
        &["priority"],
        &["tags"],
        &["description", "priority", "tags"],
    ];
    for row in omitted {
        let case = format!("CreateAutomationRule/retried-from-the-answer: omitting {row:?}");
        let case = case.as_str();
        let router = emulator();

        // A create that carries every member BUT the row's, so the response
        // reports a materialised value for exactly those.
        let mut sent = json!({
            "description": "notes",
            "priority": "0.25",
            "tags": one_tag("platform"),
        });
        for name in row {
            sent.as_object_mut()
                .expect("the request members are an object")
                .remove(*name);
        }
        let created = call_ok(
            &router,
            "CreateAutomationRule",
            &create_request("answered-rule", &sent),
            case,
        )
        .await;

        // The retry as a client writes it: every member the answer reported.
        let mut read_back = sent.clone();
        for name in row {
            read_back[*name] = member(&created, name).clone();
        }
        let (status, body) = call(
            &router,
            "CreateAutomationRule",
            &create_request("answered-rule", &read_back),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: a retry spelling the values the first call answered is that same \
             create, so it replays, got {status} and {body}"
        );
        assert_eq!(
            body, created,
            "case {case:?}: the retry must answer the rule the first call minted, expected \
             {created}, got {body}"
        );
        let held = stored_rule_count(&router, case).await;
        assert_eq!(
            held, 1,
            "case {case:?}: the replayed retry must not mint a second rule, expected 1 stored \
             rule, got {held}"
        );
    }
}
