// SPDX-License-Identifier: CC0-1.0

//! The account's SignIn resource-based policy and its permission statements, end to end
//! over HTTP, plus the console-authorization store's bound and the wording of a
//! constraint refusal.
//!
//! Every assertion goes through the router, so each case exercises routing,
//! deserialization, the handler, the persisted state and serialization — the same path a
//! client takes.
//!
//! The rules under test here are the ones this surface states:
//!
//! * `PutResourcePermissionStatement` is an action AWS grants no caller: every request
//!   is refused with `AccessDeniedException`, whatever it carries, and nothing is
//!   created. A case that needs a statement in place therefore seeds one in the store
//!   ([`crate::signin_harness::seed_statement`]) and drives the reader over HTTP;
//! * the account's policy is the document its statements make up, so an account with no
//!   statement at all has NO policy: both readers of it answer
//!   `ResourceNotFoundException` rather than an empty document or an empty page;
//! * every reader projects exactly the records the store holds — the statement ids, the
//!   condition keys each one carries, the page `maxResults` cuts;
//! * a value outside a member's `@pattern` is refused quoting that pattern as the
//!   service publishes it, without the anchors the model wraps it in.

use std::sync::Arc;

use serde_json::{json, Map, Value};
use service_com_amazonaws_signin_signin::state::{
    CONSOLE_AUTHORIZATION_TARGET_QUOTA, PERMISSION_STATEMENT_QUOTA,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::signin_harness::{post, router_over, seed_statement, Answer};

/// The policy language version an AWS resource-based policy document declares.
const POLICY_VERSION: &str = "2012-10-17";

/// The fixed sentence AWS states for a resource-policy resource the account does not
/// have. Spelled out here rather than imported, so the crate's own constant is what this
/// compares against.
const RESOURCE_NOT_FOUND_MESSAGE: &str = "Requested resource not found";

/// The fixed sentence AWS states for the one action on this surface it grants no caller.
/// Its only variable part is the action's own name.
const NO_ACCESS_MESSAGE: &str = "No Access to PutResourcePermissionStatement API";

/// `OAuth2ErrorCode`'s member for a refusal of a resource that is not there.
const NOT_FOUND_CODE: &str = "RESOURCE_NOT_FOUND";

/// `OAuth2ErrorCode`'s member for a caller that may not perform the operation.
const INSUFFICIENT_PERMISSIONS_CODE: &str = "INSUFFICIENT_PERMISSIONS";

/// The condition operator and AWS global condition key each permission-statement member
/// is expressed with, as `(member, operator, condition key)`.
///
/// Spelled out here rather than imported from the crate, so a wrong mapping in
/// `src/state.rs` fails this test. Source: `ConditionBlock` is modelled
/// `map<ConditionType, map<String, list<String>>>`, which is the IAM policy-document
/// condition grammar `"Condition": {"<operator>": {"<condition key>": [values]}}`, and
/// the condition keys are the AWS global ones naming each member's subject. The SignIn
/// and the console VPC endpoint are two values of the one `aws:SourceVpce` key.
const CONDITION_MAPPING: &[(&str, &str, &str)] = &[
    ("sourceVpc", "StringEquals", "aws:SourceVpc"),
    ("signinSourceVpce", "StringEquals", "aws:SourceVpce"),
    ("consoleSourceVpce", "StringEquals", "aws:SourceVpce"),
    ("requestedRegion", "StringEquals", "aws:RequestedRegion"),
    ("vpcSourceIp", "IpAddress", "aws:VpcSourceIp"),
    ("sourceIp", "IpAddress", "aws:SourceIp"),
    ("excludedPrincipal", "ArnNotEquals", "aws:PrincipalArn"),
];

/// The condition members a statement can carry, as `(case, members)`.
///
/// The rows differ in which members are present, how many, and of which kind — the
/// dimensions the condition projection generalises over — and they are the same rows the
/// put is refused for, so one set of inputs covers both rules.
fn statement_cases() -> Vec<(&'static str, Value)> {
    vec![
        ("no condition member at all", json!({})),
        (
            "a VPC and its region",
            json!({"sourceVpc": "vpc-0123456789abcdef0", "requestedRegion": "us-east-1"}),
        ),
        (
            "a short-form VPC id and its region",
            json!({"sourceVpc": "vpc-0123abcd", "requestedRegion": "eu-west-1"}),
        ),
        (
            "both VPC endpoints and the region",
            json!({
                "signinSourceVpce": "vpce-0123abcd",
                "consoleSourceVpce": "vpce-abcdef0123456789",
                "requestedRegion": "ap-southeast-2",
            }),
        ),
        (
            "only the SignIn endpoint",
            json!({"signinSourceVpce": "vpce-9876543210", "requestedRegion": "us-west-2"}),
        ),
        ("only a source address", json!({"sourceIp": "203.0.113.0/24"})),
        (
            "both addresses",
            json!({"sourceIp": "198.51.100.7", "vpcSourceIp": "10.0.0.0/8"}),
        ),
        (
            "only an excluded principal",
            json!({"excludedPrincipal": "arn:aws:iam::123456789012:role/Breakglass"}),
        ),
        (
            "every condition member at once",
            json!({
                "sourceVpc": "vpc-00112233445566778",
                "signinSourceVpce": "vpce-aaaa1111bbbb",
                "consoleSourceVpce": "vpce-cccc2222dddd",
                "vpcSourceIp": "172.16.0.0/12",
                "sourceIp": "192.0.2.1",
                "requestedRegion": "ca-central-1",
                "excludedPrincipal": "arn:aws:iam::123456789012:root",
            }),
        ),
    ]
}

/// One emulator and the engine behind it, so a case can seed the store the handlers
/// answer from.
fn emulator() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    (router_over(Arc::clone(&engine)), engine)
}

async fn put_statement(router: &MultiServiceRouter, request: Value) -> Answer {
    post(router, "/put-resource-permission-statement", request).await
}

async fn list_statements(router: &MultiServiceRouter, request: Value) -> Answer {
    post(router, "/list-resource-permission-statements", request).await
}

async fn get_policy(router: &MultiServiceRouter) -> Answer {
    post(router, "/get-resource-policy", json!({})).await
}

async fn delete_statement(router: &MultiServiceRouter, request: Value) -> Answer {
    post(router, "/delete-resource-permission-statement", request).await
}

async fn put_configuration(router: &MultiServiceRouter, request: Value) -> Answer {
    post(router, "/put-console-authorization-configuration", request).await
}

async fn get_configuration(router: &MultiServiceRouter, request: Value) -> Answer {
    post(router, "/get-console-authorization-configuration", request).await
}

/// The `sid` of every statement a list page carries, in order.
fn sids(answer: &Answer) -> Vec<String> {
    answer.body["permissionStatements"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .map(|item| item["sid"].as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// The `nextToken` a list page carries, if any.
fn next_token(answer: &Answer) -> Option<String> {
    answer.body["nextToken"].as_str().map(str::to_string)
}

/// The `Statement` list of a rendered policy document.
fn policy_statements(answer: &Answer) -> Vec<Value> {
    answer.body["signinResourceBasedPolicy"]["Statement"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// The condition block a statement's members must be reported with: `Value::Null` when
/// it carries no condition member at all.
fn expected_condition(members: &Value) -> Value {
    let mut block: Map<String, Value> = Map::new();
    for (member, operator, key) in CONDITION_MAPPING {
        let Some(value) = members.get(*member).and_then(Value::as_str) else {
            continue;
        };
        let operator_entry = block
            .entry((*operator).to_string())
            .or_insert_with(|| Value::Object(Map::new()));
        let keys = operator_entry
            .as_object_mut()
            .expect("a condition operator holds a map of condition keys");
        keys.entry((*key).to_string())
            .or_insert_with(|| Value::Array(Vec::new()))
            .as_array_mut()
            .expect("a condition key holds a list of values")
            .push(json!(value));
    }
    if block.is_empty() {
        Value::Null
    } else {
        Value::Object(block)
    }
}

/// Every statement the store holds, listed with the largest page the model admits.
async fn all_statements(router: &MultiServiceRouter) -> Vec<Value> {
    let answer = list_statements(router, json!({"maxResults": 100})).await;
    assert_eq!(
        answer.status, 200,
        "a list must be served, got {} with {:?}",
        answer.status, answer.body
    );
    answer.body["permissionStatements"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// Assert that `answer` is the 404 `ResourceNotFoundException` a resource-policy
/// resource the account does not have earns, worded as AWS words it.
fn assert_resource_not_found(case: &str, answer: &Answer) {
    assert_eq!(
        (
            answer.status,
            answer.error_code(),
            answer.error_member(),
            answer.message()
        ),
        (
            404,
            "ResourceNotFoundException",
            NOT_FOUND_CODE,
            RESOURCE_NOT_FOUND_MESSAGE
        ),
        "case {case:?}: expected 404 ResourceNotFoundException / \
         {RESOURCE_NOT_FOUND_MESSAGE:?}, got {} with {:?}",
        answer.status,
        answer.body
    );
}

/// Writing a statement of the account's SignIn resource-based policy is an action AWS
/// grants no caller: every well-formed request is refused with `AccessDeniedException`
/// and no statement is created — so the account still has no policy afterwards.
///
/// The rows span what the refusal generalises over: which condition members the request
/// carries and how many, whether it carries a `clientToken` at all, and how many times
/// the same request is retried under the same token.
#[tokio::test]
async fn a_put_of_a_permission_statement_is_denied_for_every_request() {
    let (router, _engine) = emulator();

    // `ClientToken` is `@length(1..64)` with pattern `^[!-~]+$`, so the token rows span a
    // one-character token, a 64-character one, punctuation the pattern admits, and no
    // token at all — each retried a different number of times, since an
    // `@idempotencyToken` is exactly what a caller would expect to change the answer.
    let token_cases: Vec<(&str, Option<String>, usize)> = vec![
        ("no clientToken", None, 2),
        ("a one-character token", Some("t".to_string()), 2),
        ("a 64-character token", Some("k".repeat(64)), 3),
        ("punctuation only", Some("!~/+=".to_string()), 4),
        (
            "a token used once",
            Some("single-attempt".to_string()),
            1,
        ),
    ];

    for (statement_case, members) in statement_cases() {
        for (token_case, token, attempts) in &token_cases {
            let mut request = members.clone();
            if let Some(token) = token {
                request["clientToken"] = json!(token);
            }
            for attempt in 0..*attempts {
                let answer = put_statement(&router, request.clone()).await;
                assert_eq!(
                    (
                        answer.status,
                        answer.error_code(),
                        answer.error_member(),
                        answer.message()
                    ),
                    (
                        400,
                        "AccessDeniedException",
                        INSUFFICIENT_PERMISSIONS_CODE,
                        NO_ACCESS_MESSAGE
                    ),
                    "case {statement_case:?} with {token_case:?}, attempt {attempt}: the \
                     put must be denied with {NO_ACCESS_MESSAGE:?}, got {} with {:?}",
                    answer.status,
                    answer.body
                );
                assert!(
                    answer.body["statementId"].is_null(),
                    "case {statement_case:?} with {token_case:?}: a denied put must \
                     report no statementId, got {:?}",
                    answer.body
                );
            }
        }
    }

    // Nothing was created, so the account still has no policy at all: both readers of it
    // report the resource missing.
    assert_resource_not_found("the policy after every put was denied", &get_policy(&router).await);
    assert_resource_not_found(
        "the statement list after every put was denied",
        &list_statements(&router, json!({"maxResults": 25})).await,
    );
}

/// An account with no permission statement has no SignIn resource-based policy: both
/// readers of it refuse with `ResourceNotFoundException` rather than reporting a document
/// nothing created or a page that cannot be told from "no policy at all". Once a
/// statement is there both are served, and once the last one is deleted both refuse
/// again.
#[tokio::test]
async fn the_policy_readers_refuse_an_account_that_has_no_policy() {
    // The list rows span the page controls a caller can send: no `maxResults`, each
    // published bound of its `@range(min: 1, max: 100)`, a value in between, and a
    // `nextToken` — the resource has to exist before any of them means anything.
    let list_requests = [
        ("no page control at all", json!({})),
        ("the smallest page", json!({"maxResults": 1})),
        ("a page of twenty-five", json!({"maxResults": 25})),
        ("the largest page", json!({"maxResults": 100})),
        (
            "a continuation token",
            json!({"nextToken": "0".repeat(64)}),
        ),
    ];

    let (router, engine) = emulator();

    assert_resource_not_found("the policy of an account with no statement", &get_policy(&router).await);
    for (case, request) in &list_requests {
        assert_resource_not_found(case, &list_statements(&router, request.clone()).await);
    }

    // One statement is enough for the account to HAVE a policy.
    let statement_id = seed_statement(&engine, &json!({"sourceIp": "192.0.2.10"}));
    let policy = get_policy(&router).await;
    assert_eq!(
        (policy.status, policy_statements(&policy).len()),
        (200, 1),
        "with a statement stored the policy must be served, got {} with {:?}",
        policy.status,
        policy.body
    );
    let listed = list_statements(&router, json!({"maxResults": 25})).await;
    assert_eq!(
        (listed.status, sids(&listed)),
        (200, vec![statement_id.clone()]),
        "with a statement stored the list must be served, got {} with {:?}",
        listed.status,
        listed.body
    );

    // And deleting the last statement takes the policy away again — the absence is a
    // missing resource, not an empty document.
    assert_eq!(
        delete_statement(&router, json!({"statementId": statement_id}))
            .await
            .status,
        200,
        "the delete of the one stored statement must be accepted"
    );
    assert_resource_not_found("the policy after the last statement was deleted", &get_policy(&router).await);
    for (case, request) in &list_requests {
        assert_resource_not_found(
            &format!("{case} after the last statement was deleted"),
            &list_statements(&router, request.clone()).await,
        );
    }
}

/// Every reader projects the statements the policy holds: the list reports each one with
/// exactly the condition keys it carries, and `GetResourcePolicy` renders those same
/// condition blocks into the account's policy document.
#[tokio::test]
async fn every_reader_projects_the_statements_the_policy_holds() {
    let (router, engine) = emulator();
    let cases = statement_cases();
    let stored: Vec<(&str, Value, String)> = cases
        .iter()
        .map(|(case, members)| (*case, members.clone(), seed_statement(&engine, members)))
        .collect();

    let listed = all_statements(&router).await;
    assert_eq!(
        listed.len(),
        stored.len(),
        "the list must report exactly the {} statements the store holds, got {listed:?}",
        stored.len()
    );
    for (case, members, statement_id) in &stored {
        let summary = listed
            .iter()
            .find(|item| item["sid"].as_str() == Some(statement_id.as_str()))
            .unwrap_or_else(|| {
                panic!("case {case:?}: the list must report the statement {statement_id:?}")
            });
        assert_eq!(
            summary["condition"],
            expected_condition(members),
            "case {case:?}: expected the condition keys the statement carries, got {:?}",
            summary["condition"]
        );
    }

    // And the policy document is rendered from the same records: one statement each,
    // carrying the same condition blocks.
    let policy = get_policy(&router).await;
    assert_eq!(
        policy.status, 200,
        "the policy must be served, got {} with {:?}",
        policy.status, policy.body
    );
    assert_eq!(
        policy.body["signinResourceBasedPolicy"]["Version"],
        json!(POLICY_VERSION),
        "the policy must declare its language version, got {:?}",
        policy.body
    );
    let rendered = policy_statements(&policy);
    assert_eq!(
        rendered.len(),
        stored.len(),
        "the policy must carry one statement per stored permission statement, got \
         {rendered:?}"
    );
    for (case, members, _) in &stored {
        let expected = expected_condition(members);
        let matches = rendered
            .iter()
            .filter(|statement| statement["Condition"] == expected)
            .count();
        assert!(
            matches >= 1,
            "case {case:?}: the policy must carry a statement whose Condition is \
             {expected:?}, got {rendered:?}"
        );
    }
}

/// `maxResults` bounds a page and `nextToken` resumes exactly where the previous page
/// ended, with no token on the last page; paging reports every stored statement once, in
/// mint order, and listing is a read: it leaves the store alone.
#[tokio::test]
async fn the_list_pages_at_max_results() {
    // Rows differ in how many statements are stored and how large a page is asked for,
    // including the published bounds of `maxResults`' `@range(min: 1, max: 100)`. Every
    // row holds at least one statement, since an account with none has no policy to page
    // over at all (that case is its own test above).
    let cases = [
        ("one statement, smallest page", 1usize, Some(1)),
        ("five statements, smallest page", 5, Some(1)),
        ("five statements, page of two", 5, Some(2)),
        ("six statements, page of three", 6, Some(3)),
        ("four statements, largest page", 4, Some(100)),
        ("seven statements, no page size", 7, None),
    ];

    for (case, stored, max_results) in cases {
        let (router, engine) = emulator();
        let mut minted = Vec::new();
        for index in 0..stored {
            minted.push(seed_statement(
                &engine,
                &json!({"sourceIp": format!("198.51.100.{index}")}),
            ));
        }

        let mut seen: Vec<String> = Vec::new();
        let mut token: Option<String> = None;
        let mut pages = 0;
        loop {
            let mut request = Map::new();
            if let Some(max) = max_results {
                request.insert("maxResults".to_string(), json!(max));
            }
            if let Some(token) = token.as_ref() {
                request.insert("nextToken".to_string(), json!(token));
            }
            let answer = list_statements(&router, Value::Object(request)).await;
            assert_eq!(
                answer.status, 200,
                "case {case:?}: page {pages} must be served, got {} with {:?}",
                answer.status, answer.body
            );
            let page = sids(&answer);
            if let Some(max) = max_results {
                assert!(
                    page.len() <= usize::try_from(max).expect("a positive page size"),
                    "case {case:?}: page {pages} must hold at most {max} statements, got \
                     {page:?}"
                );
            }
            seen.extend(page);
            pages += 1;
            match next_token(&answer) {
                Some(next) => token = Some(next),
                None => break,
            }
            assert!(
                pages <= stored + 2,
                "case {case:?}: paging must terminate, saw {pages} pages for {stored} \
                 statements"
            );
        }

        assert_eq!(
            seen.len(),
            stored,
            "case {case:?}: paging must report each of the {stored} statements exactly \
             once, got {seen:?}"
        );
        assert_eq!(
            seen, minted,
            "case {case:?}: paging must report the statements in mint order, got {seen:?}"
        );

        // A list is a read: a later caller sees the same store.
        let again: Vec<String> = all_statements(&router)
            .await
            .iter()
            .map(|item| item["sid"].as_str().unwrap_or_default().to_string())
            .collect();
        assert_eq!(
            again, minted,
            "case {case:?}: listing must not change what a later caller sees"
        );
    }

    // A token this operation never issued is a bad request, not an empty page. The rows
    // span both ways a token can fail to be one of this operation's: a shape the ids it
    // mints never take, and a value its mint has not reached. The store holds two
    // statements here, so a page IS available — what is refused is the token, not the
    // query.
    let (router, engine) = emulator();
    for index in 0..2 {
        seed_statement(&engine, &json!({"sourceIp": format!("192.0.2.{index}")}));
    }
    for (case, token) in [
        ("not a statement id at all", "not-a-token".to_string()),
        ("one character short", "0".repeat(63)),
        ("one character too long", "0".repeat(65)),
        ("the zero value no mint issues", "0".repeat(64)),
        ("a value far past the ids minted", "f".repeat(64)),
        (
            "a character the mint never produces",
            format!("{}+", "0".repeat(63)),
        ),
        (
            "an upper-case id the mint never produces",
            format!("{}A", "0".repeat(63)),
        ),
    ] {
        let refused = list_statements(&router, json!({"nextToken": token})).await;
        assert_eq!(
            (refused.status, refused.error_code()),
            (400, "ValidationException"),
            "case {case:?}: an unissued nextToken must be refused, got {} with {:?}",
            refused.status,
            refused.body
        );
    }

    // And the token this operation DID issue is served.
    let first_page = list_statements(&router, json!({"maxResults": 1})).await;
    let issued = next_token(&first_page)
        .expect("a page bounded below the store size carries a nextToken");
    let resumed = list_statements(&router, json!({"nextToken": issued.clone()})).await;
    assert_eq!(
        resumed.status, 200,
        "the token {issued:?} this operation issued must be served, got {} with {:?}",
        resumed.status, resumed.body
    );
}

/// A delete removes the statement from every reader, and an id that names no stored
/// statement is refused by kind: a well-formed id nothing holds is a 404, one the model's
/// pattern does not admit is a 400, and a retry under the token that already deleted it is
/// the same success.
#[tokio::test]
async fn a_delete_removes_the_statement_from_every_reader() {
    let (router, engine) = emulator();
    let ids: Vec<String> = (0..4)
        .map(|index| {
            seed_statement(
                &engine,
                &json!({"sourceIp": format!("192.0.2.{index}"), "requestedRegion": "us-east-1"}),
            )
        })
        .collect();

    // Delete them one at a time, checking both readers after each.
    for (index, id) in ids.clone().into_iter().enumerate() {
        let answer = delete_statement(&router, json!({"statementId": id})).await;
        assert_eq!(
            answer.status, 200,
            "case {index}: a delete of a stored statement must be accepted, got {} with {:?}",
            answer.status, answer.body
        );

        let expected: Vec<String> = ids[index + 1..].to_vec();
        if expected.is_empty() {
            // The last statement is gone, so the account has no policy at all again.
            assert_resource_not_found("the list after the last delete", &list_statements(&router, json!({})).await);
            assert_resource_not_found("the policy after the last delete", &get_policy(&router).await);
        } else {
            let remaining: Vec<String> = all_statements(&router)
                .await
                .iter()
                .map(|item| item["sid"].as_str().unwrap_or_default().to_string())
                .collect();
            assert_eq!(
                remaining, expected,
                "case {index}: the list must report exactly the statements still stored"
            );
            assert_eq!(
                policy_statements(&get_policy(&router).await).len(),
                expected.len(),
                "case {index}: the policy must carry one statement per statement still stored"
            );
        }

        // A second delete of the same id, with no token to replay, is a 404.
        assert_resource_not_found(
            &format!("case {index}: deleting an already deleted statement"),
            &delete_statement(&router, json!({"statementId": id})).await,
        );
    }

    // A retried delete under the token that removed the statement answers the same
    // success rather than the 404 an unknown id earns.
    let doomed = seed_statement(&engine, &json!({"sourceIp": "192.0.2.9"}));
    for attempt in 0..3 {
        let answer = delete_statement(
            &router,
            json!({"statementId": doomed, "clientToken": "delete-once"}),
        )
        .await;
        assert_eq!(
            answer.status, 200,
            "attempt {attempt}: a retried delete under one clientToken must answer the \
             same success, got {} with {:?}",
            answer.status, answer.body
        );
    }

    // Refusals by kind. A well-formed id nothing holds is a 404; an id the pattern does
    // not admit is a 400.
    let cases = [
        ("a well-formed id nothing holds", "0".repeat(64), 404, "ResourceNotFoundException"),
        ("another well-formed id nothing holds", "f".repeat(64), 404, "ResourceNotFoundException"),
        ("a padded well-formed id nothing holds", format!("{}=", "A".repeat(64)), 404, "ResourceNotFoundException"),
        ("not a statement id at all", "nope".to_string(), 400, "ValidationException"),
        ("one character short", "a".repeat(63), 400, "ValidationException"),
        ("one character too long", "a".repeat(65), 400, "ValidationException"),
        ("a character outside the alphabet", format!("{}-", "a".repeat(63)), 400, "ValidationException"),
        ("empty", String::new(), 400, "ValidationException"),
    ];
    for (case, statement_id, status, code) in cases {
        let answer = delete_statement(&router, json!({"statementId": statement_id})).await;
        assert_eq!(
            (answer.status, answer.error_code()),
            (status, code),
            "case {case:?}: expected HTTP {status} {code} for {statement_id:?}, got {} with {:?}",
            answer.status,
            answer.body
        );
    }
}

/// `requestedRegion` documents the one cross-member rule the model cannot express:
/// "Required when sourceVpc or signinSourceVpce/consoleSourceVpce is provided". A put
/// that names a VPC or a VPC endpoint without it is refused as INVALID — validating a
/// request is not authorizing it — while a put that satisfies the rule reaches the
/// authorization decision and is denied. Either way nothing is stored.
#[tokio::test]
async fn a_vpc_member_requires_the_requested_region() {
    let cases = [
        (
            "a VPC alone",
            json!({"sourceVpc": "vpc-0123abcd"}),
            "ValidationException",
        ),
        (
            "the SignIn endpoint alone",
            json!({"signinSourceVpce": "vpce-0123abcd"}),
            "ValidationException",
        ),
        (
            "the console endpoint alone",
            json!({"consoleSourceVpce": "vpce-0123abcd"}),
            "ValidationException",
        ),
        (
            "a VPC and both endpoints, no region",
            json!({
                "sourceVpc": "vpc-0123abcd",
                "signinSourceVpce": "vpce-0123abcd",
                "consoleSourceVpce": "vpce-4567efgh",
            }),
            "ValidationException",
        ),
        (
            "a VPC with its region",
            json!({"sourceVpc": "vpc-0123abcd", "requestedRegion": "us-east-1"}),
            "AccessDeniedException",
        ),
        (
            "the console endpoint with its region",
            json!({"consoleSourceVpce": "vpce-0123abcd", "requestedRegion": "sa-east-1"}),
            "AccessDeniedException",
        ),
        (
            "an address, no region",
            json!({"sourceIp": "192.0.2.1"}),
            "AccessDeniedException",
        ),
        (
            "a VPC address, no region",
            json!({"vpcSourceIp": "10.1.2.3"}),
            "AccessDeniedException",
        ),
        (
            "an excluded principal, no region",
            json!({"excludedPrincipal": "arn:aws:iam::123456789012:user/Alice"}),
            "AccessDeniedException",
        ),
        (
            "a region on its own",
            json!({"requestedRegion": "ap-south-1"}),
            "AccessDeniedException",
        ),
        ("no member at all", json!({}), "AccessDeniedException"),
    ];

    let (router, _engine) = emulator();
    for (case, request, code) in cases {
        let answer = put_statement(&router, request.clone()).await;
        assert_eq!(
            (answer.status, answer.error_code()),
            (400, code),
            "case {case:?}: expected HTTP 400 {code}, got {} with {:?}",
            answer.status,
            answer.body
        );
        assert_resource_not_found(
            &format!("case {case:?}: no refused put may store a statement"),
            &list_statements(&router, json!({})).await,
        );
    }
}

/// The delete-retry record is bounded AND does not decay: however many deletes the store
/// has seen, a retry of a recent one still answers its original success, while a token
/// evicted by the bound answers the 404 an unknown id earns.
#[tokio::test]
async fn a_delete_retry_survives_the_bound_on_the_retry_record() {
    let (router, engine) = emulator();
    // More create/delete cycles than the record can hold, so the bound is exercised.
    let cycles = PERMISSION_STATEMENT_QUOTA + 5;
    let mut deleted: Vec<(String, String)> = Vec::with_capacity(cycles);
    for index in 0..cycles {
        let statement_id = seed_statement(&engine, &json!({"sourceIp": "192.0.2.1"}));
        let token = format!("delete-{index}");
        let answer = delete_statement(
            &router,
            json!({"statementId": statement_id, "clientToken": token}),
        )
        .await;
        assert_eq!(
            answer.status, 200,
            "cycle {index}: the delete must be accepted, got {} with {:?}",
            answer.status, answer.body
        );
        deleted.push((token, statement_id));
    }

    // The most recent deletes are still remembered, so their retries replay.
    for (token, statement_id) in deleted.iter().rev().take(3) {
        let retry = delete_statement(
            &router,
            json!({"statementId": statement_id, "clientToken": token}),
        )
        .await;
        assert_eq!(
            retry.status, 200,
            "a retry under the recent token {token:?} must answer the same success, got {} \
             with {:?}",
            retry.status, retry.body
        );
    }

    // The oldest token was evicted to keep the record bounded, so its retry is a 404 —
    // the same answer any id the store does not hold earns.
    let (oldest_token, oldest_id) = &deleted[0];
    assert_resource_not_found(
        &format!("a retry under the evicted token {oldest_token:?}"),
        &delete_statement(
            &router,
            json!({"statementId": oldest_id, "clientToken": oldest_token}),
        )
        .await,
    );
}

/// A value outside its member's `@pattern` is refused quoting the expression the service
/// publishes — the model's pattern WITHOUT the `^`/`$` it wraps it in to turn Smithy's
/// find into a whole-value match.
///
/// Each row is `(case, route, request, member path, the pattern as the message must
/// quote it)`. The patterns are the `@pattern` of each member as this service's Smithy
/// model declares it, with the anchors dropped — source: `smithy_query(query="operation",
/// operation="DeleteResourcePermissionStatement,PutResourcePermissionStatement,
/// ListResourcePermissionStatements,GetConsoleAuthorizationConfiguration")`. The rows span
/// several operations and every kind of pattern on this surface: an alphabet with a fixed
/// count, a character class, an alternation, a bounded repetition and a star, so the
/// rendering cannot be keyed to one member.
#[tokio::test]
async fn a_pattern_constrained_member_quotes_the_pattern_without_its_anchors() {
    let cases: Vec<(&str, &str, Value, &str, &str)> = vec![
        (
            "a statement id the alphabet does not admit",
            "/delete-resource-permission-statement",
            json!({"statementId": "not-a-valid-sid!!"}),
            "statementId",
            "[A-Za-z0-9+/]{64}=?",
        ),
        (
            "a client token with a space in it",
            "/delete-resource-permission-statement",
            json!({"statementId": "A".repeat(64), "clientToken": "bad token"}),
            "clientToken",
            "[!-~]+",
        ),
        (
            "a VPC id that is not hexadecimal",
            "/put-resource-permission-statement",
            json!({"sourceVpc": "vpc-zzzzzzzz", "requestedRegion": "us-east-1"}),
            "sourceVpc",
            "vpc-([0-9a-f]{8}|[0-9a-f]{17})",
        ),
        (
            "a VPC endpoint id in upper case",
            "/put-resource-permission-statement",
            json!({"consoleSourceVpce": "vpce-ABCDEFGH", "requestedRegion": "us-east-1"}),
            "consoleSourceVpce",
            "vpce-[a-z0-9]{8,20}",
        ),
        (
            "a region in upper case",
            "/put-resource-permission-statement",
            json!({"requestedRegion": "US-EAST-1"}),
            "requestedRegion",
            r"[a-z]{2}(-[a-z]+)+-\d+",
        ),
        (
            "an excluded principal that is not one of the admitted ARN forms",
            "/put-resource-permission-statement",
            json!({"excludedPrincipal": "arn:aws:iam::123456789012:group/Admins"}),
            "excludedPrincipal",
            "arn:aws:((iam::[0-9]{12}:role/[a-zA-Z0-9_+=,.@-]{1,64})|(iam::[0-9]{12}:user/[a-zA-Z0-9_+=,.@-]{1,64})|(sts::[0-9]{12}:federated-user/[a-zA-Z0-9_+=,.@-]{2,193})|(iam::[0-9]{12}:root))",
        ),
        (
            "a continuation token with a character the token alphabet excludes",
            "/list-resource-permission-statements",
            json!({"nextToken": "not a token"}),
            "nextToken",
            "[-a-zA-Z0-9+=/_]*",
        ),
    ];

    let (router, _engine) = emulator();
    for (case, route, request, member, pattern) in cases {
        let answer = post(&router, route, request.clone()).await;
        assert_eq!(
            (answer.status, answer.error_code()),
            (400, "ValidationException"),
            "case {case:?}: expected 400 ValidationException, got {} with {:?}",
            answer.status,
            answer.body
        );
        let value = request[member]
            .as_str()
            .expect("every row sends the offending member as a string");
        let expected = format!(
            "1 validation error detected: Value '{value}' at '{member}' failed to satisfy \
             constraint: Member must satisfy regular expression pattern: {pattern}"
        );
        assert_eq!(
            answer.message(),
            expected,
            "case {case:?}: expected {expected:?}, got {:?}",
            answer.message()
        );
    }
}

/// A `targetId` the model's pattern does not admit is refused, and nothing is stored for
/// it. The refusal quotes `TargetId`'s pattern unanchored, like every other
/// pattern-constrained member.
#[tokio::test]
async fn a_malformed_target_id_is_refused() {
    // `TargetId`'s own pattern, with the anchors dropped — the expression the refusal
    // must quote (source: `smithy_query(query="shape", shape="TargetId")`).
    const TARGET_ID_PATTERN: &str = r"(\d{12}|o-[a-z0-9]{10}|r-[0-9a-z]{4,32})";
    // Rows span the ways a value of a length `TargetId`'s `@length(12..32)` admits can
    // still fall outside its pattern, so the pattern is the one requirement broken and
    // the message carries exactly one finding.
    let cases = [
        ("thirteen digits", "1234567890123".to_string()),
        ("an organization id one character too long", "o-abc12345678".to_string()),
        ("an organization id with upper case", "o-ABC1234567".to_string()),
        ("a word", "not-a-target".to_string()),
        ("a root id with an upper-case suffix", "r-ABCDEFGHIJ".to_string()),
    ];

    let (router, _engine) = emulator();
    for (case, target_id) in cases {
        for (operation, answer) in [
            (
                "put",
                put_configuration(&router, json!({"targetId": target_id.clone()})).await,
            ),
            (
                "get",
                get_configuration(&router, json!({"targetId": target_id.clone()})).await,
            ),
        ] {
            assert_eq!(
                (answer.status, answer.error_code()),
                (400, "ValidationException"),
                "case {case:?}: {operation} of {target_id:?} must be refused, got {} with {:?}",
                answer.status,
                answer.body
            );
            assert_eq!(
                answer.message(),
                format!(
                    "1 validation error detected: Value '{target_id}' at 'targetId' failed \
                     to satisfy constraint: Member must satisfy regular expression pattern: \
                     {TARGET_ID_PATTERN}"
                ),
                "case {case:?}: the {operation}'s refusal must quote TargetId's pattern \
                 unanchored, got {:?}",
                answer.message()
            );
        }
    }
}

/// The console-authorization store is bounded: once it holds its quota of distinct
/// targets, a put naming a NEW target is refused with a modelled error and stores
/// nothing, while a put naming a target already configured conflicts with the state that
/// target is in.
///
/// The bound is imported because the test has to know where it lies; what is asserted is
/// the behaviour at it, not the number.
#[tokio::test]
async fn the_console_authorization_store_is_bounded_by_its_quota() {
    let (router, _engine) = emulator();
    // Rows are distinct 12-digit account ids, the `TargetId` form with the largest key
    // space, so each put takes on one more target.
    for index in 0..CONSOLE_AUTHORIZATION_TARGET_QUOTA {
        let target_id = format!("{:012}", index + 1);
        let answer = put_configuration(&router, json!({"targetId": target_id})).await;
        assert_eq!(
            answer.status, 200,
            "target {target_id} ({index} of {CONSOLE_AUTHORIZATION_TARGET_QUOTA}) must be \
             configured, got {} with {:?}",
            answer.status, answer.body
        );
    }

    // One target too many: refused, and nothing is stored for it.
    let over = format!("{:012}", CONSOLE_AUTHORIZATION_TARGET_QUOTA + 1);
    let refused = put_configuration(&router, json!({"targetId": over})).await;
    assert_eq!(
        (refused.status, refused.error_code()),
        (429, "TooManyRequestsError"),
        "a put naming a new target past the quota must be refused, got {} with {:?}",
        refused.status,
        refused.body
    );
    let missing = get_configuration(&router, json!({"targetId": over})).await;
    assert_eq!(
        missing.status, 404,
        "a refused put must store nothing for {over}, got {} with {:?}",
        missing.status, missing.body
    );

    // A target the store already holds is not a quota question at all: it conflicts with
    // the state that target is already in.
    for target_id in ["000000000001", "000000000100"] {
        let conflicted = put_configuration(&router, json!({"targetId": target_id})).await;
        assert_eq!(
            (conflicted.status, conflicted.error_code()),
            (409, "ConflictException"),
            "a put of the already configured {target_id} must conflict, got {} with {:?}",
            conflicted.status,
            conflicted.body
        );
        assert_eq!(
            get_configuration(&router, json!({"targetId": target_id}))
                .await
                .status,
            200,
            "the refused put must leave {target_id} configured"
        );
    }
}
