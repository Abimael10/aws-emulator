// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the Resolver rule policy slice:
//! `PutResolverRulePolicy` and `GetResolverRulePolicy`.
//!
//! The rule under test: a rule's resource policy is the share of that rule, so
//! the write attaches a document that asks for the share the service can make —
//! a policy document whose every statement delegates the rule operations the
//! model lists for the member that carries it, and no other — whoever the
//! document names as principal, and refuses every other document with
//! `InvalidPolicyDocument`, leaving the policy the rule already carried in
//! place. What a read reports is the service's own serialization of the document
//! that was attached, with each account named as a principal in the form IAM
//! converts an account id to; a rule nobody has written a policy for reports the
//! document that grants nothing. Each suite drives a TABLE of documents spanning
//! the forms the policy grammar admits, so no case is tied to one recorded
//! document.
//!
//! Every case goes over the wire the service actually speaks (awsJson1_1,
//! `POST /` with `X-Amz-Target: Route53Resolver.<OperationName>`), through the
//! same router the emulator mounts, so what is asserted is the HTTP behaviour
//! and not a handler called directly. The transport helpers are the shared ones
//! in [`super::harness`].

use serde_json::{json, Value};
use service_com_amazonaws_route53resolver_route53resolver::state::EMPTY_POLICY_DOCUMENT;
use smithy_server_runtime::MultiServiceRouter;

use super::harness::{call, emulator, error_type, expect_error, ok, string_member, ACCOUNT, REGION};

/// An account that is not the one this emulator's resources belong to, so a
/// policy naming it really does share the rule with somebody else.
const OTHER_ACCOUNT: &str = "123456789012";

/// The whole message a document this write will not attach is refused with:
/// the service error code for an invalid policy and the two words that name the
/// fault, and nothing else — written out here rather than built from the
/// emulator's own constants so the wording itself is what this suite pins.
const INVALID_POLICY_MESSAGE: &str = "[RSLVR-00312] Invalid Policy";

/// Create a SYSTEM Resolver rule and return `(Id, Arn)`. A SYSTEM rule needs no
/// Resolver endpoint and no target addresses, so a policy case needs nothing
/// else stored.
async fn system_rule(router: &MultiServiceRouter, creator: &str, domain: &str) -> (String, String) {
    let created = ok(
        router,
        "CreateResolverRule",
        json!({
            "CreatorRequestId": creator,
            "Name": creator,
            "RuleType": "SYSTEM",
            "DomainName": domain,
        }),
    )
    .await;
    let rule = created
        .get("ResolverRule")
        .expect("the create answers a ResolverRule");
    (string_member(rule, "Id"), string_member(rule, "Arn"))
}

/// The `ResolverRulePolicy` a read of `arn` reports.
async fn stored_policy(router: &MultiServiceRouter, arn: &str) -> String {
    let read = ok(router, "GetResolverRulePolicy", json!({ "Arn": arn })).await;
    string_member(&read, "ResolverRulePolicy")
}

/// Write `policy` for `arn` and assert the call reported success.
async fn put_policy(router: &MultiServiceRouter, case: &str, arn: &str, policy: &str) {
    let written = ok(
        router,
        "PutResolverRulePolicy",
        json!({ "Arn": arn, "ResolverRulePolicy": policy }),
    )
    .await;
    assert_eq!(
        written.get("ReturnValue"),
        Some(&Value::Bool(true)),
        "case {case:?}: PutResolverRulePolicy must report ReturnValue true, got {written}"
    );
}

/// Write `policy` for `arn` and require the refusal the service answers for a
/// document it will not attach: `InvalidPolicyDocument`, with the whole message
/// that names the fault.
async fn expect_policy_refused(
    router: &MultiServiceRouter,
    case: &str,
    arn: &str,
    policy: &str,
) {
    let (status, refusal) = call(
        router,
        "PutResolverRulePolicy",
        json!({ "Arn": arn, "ResolverRulePolicy": policy }),
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: the write must answer HTTP 400, got {status}: {refusal}"
    );
    assert!(
        error_type(&refusal).contains("InvalidPolicyDocument"),
        "case {case:?}: the write must answer InvalidPolicyDocument, got {refusal}"
    );
    let message = string_member(&refusal, "Message");
    assert_eq!(
        message, INVALID_POLICY_MESSAGE,
        "case {case:?}: the refusal must state the invalid-policy fault, got {message:?}"
    );
}

/// The ARN of the root user of the account `account` names — the form IAM
/// converts an account id named as a principal to.
fn account_root(account: &str) -> String {
    format!("arn:aws:iam::{account}:root")
}

/// The `Action` section of a rule-sharing statement: the operations a share of a
/// Resolver rule delegates.
///
/// Source: the model's documentation of the member that carries the document,
/// `smithy_query(query="doc",shape="PutResolverRulePolicyRequest",member="ResolverRulePolicy")`,
/// which lists the operations "you can specify ... in the `Action` section of
/// the statement".
fn shared_actions() -> Value {
    json!([
        "route53resolver:GetResolverRule",
        "route53resolver:AssociateResolverRule",
        "route53resolver:DisassociateResolverRule",
        "route53resolver:ListResolverRules",
        "route53resolver:ListResolverRuleAssociations",
    ])
}

/// One statement sharing `arn` with `principal`, in the grammar's statement
/// form: an `Effect`, the delegated `Action` set and the `Resource` the doc asks
/// for ("specify the ARN for the rule that you want to share with another
/// account").
fn share_statement(principal: Value, arn: &str) -> Value {
    json!({
        "Effect": "Allow",
        "Principal": principal,
        "Action": shared_actions(),
        "Resource": arn,
    })
}

/// A whole document carrying `statements`.
fn policy_document(statements: Value) -> String {
    json!({ "Version": "2012-10-17", "Statement": statements }).to_string()
}

/// A document that delegates the operations a share of a rule grants is
/// attached, and the read reports the service's serialization of it — whatever
/// form the grammar lets the document take.
#[tokio::test]
async fn a_delegating_policy_is_reported_back_as_attached() {
    let router = emulator();
    let (_, arn) = system_rule(&router, "policy-shared", "shared.example.com").await;

    // One statement for `principal`, as the request writes it.
    let shared = |principal: Value| policy_document(json!([share_statement(principal, &arn)]));

    // Each row is a different shape of sharing document — how the principal is
    // written, how many statements the document carries, which optional
    // elements it states — paired with the document a read of it reports.
    let cases: Vec<(&str, String, String)> = vec![
        (
            "principal by account id",
            shared(json!({ "AWS": OTHER_ACCOUNT })),
            shared(json!({ "AWS": account_root(OTHER_ACCOUNT) })),
        ),
        (
            "principal by account root ARN",
            shared(json!({ "AWS": account_root(OTHER_ACCOUNT) })),
            shared(json!({ "AWS": account_root(OTHER_ACCOUNT) })),
        ),
        (
            "principal list naming the owner and another account",
            shared(json!({ "AWS": [ACCOUNT, OTHER_ACCOUNT] })),
            shared(json!({ "AWS": [account_root(ACCOUNT), account_root(OTHER_ACCOUNT)] })),
        ),
        (
            "wildcard principal",
            shared(json!("*")),
            shared(json!("*")),
        ),
        (
            "service principal",
            shared(json!({ "Service": "ram.amazonaws.com" })),
            shared(json!({ "Service": "ram.amazonaws.com" })),
        ),
        // The emulator serves one account, so a caller sharing a rule has its
        // own account to name: that document asks for a share like any other.
        (
            "principal by the owning account's id",
            shared(json!({ "AWS": ACCOUNT })),
            shared(json!({ "AWS": account_root(ACCOUNT) })),
        ),
        (
            "two statements",
            policy_document(json!([
                share_statement(json!({ "AWS": OTHER_ACCOUNT }), &arn),
                share_statement(json!({ "AWS": "210987654321" }), &arn),
            ])),
            policy_document(json!([
                share_statement(json!({ "AWS": account_root(OTHER_ACCOUNT) }), &arn),
                share_statement(json!({ "AWS": account_root("210987654321") }), &arn),
            ])),
        ),
        (
            "one statement, not a list",
            policy_document(share_statement(json!({ "AWS": OTHER_ACCOUNT }), &arn)),
            policy_document(share_statement(
                json!({ "AWS": account_root(OTHER_ACCOUNT) }),
                &arn,
            )),
        ),
        (
            "statement with a Sid and a Condition",
            json!({
                "Statement": [{
                    "Sid": "ShareOneRule",
                    "Effect": "Allow",
                    "Principal": { "AWS": OTHER_ACCOUNT },
                    "Action": shared_actions(),
                    "Resource": arn,
                    "Condition": { "StringEquals": { "aws:PrincipalOrgID": "o-example" } },
                }],
            })
            .to_string(),
            json!({
                "Statement": [{
                    "Sid": "ShareOneRule",
                    "Effect": "Allow",
                    "Principal": { "AWS": account_root(OTHER_ACCOUNT) },
                    "Action": shared_actions(),
                    "Resource": arn,
                    "Condition": { "StringEquals": { "aws:PrincipalOrgID": "o-example" } },
                }],
            })
            .to_string(),
        ),
        (
            "no Version element",
            json!({ "Statement": [share_statement(json!({ "AWS": OTHER_ACCOUNT }), &arn)] })
                .to_string(),
            json!({
                "Statement": [share_statement(
                    json!({ "AWS": account_root(OTHER_ACCOUNT) }),
                    &arn,
                )],
            })
            .to_string(),
        ),
        (
            "a long document: one statement per shared account",
            policy_document(Value::Array(
                (0..40)
                    .map(|index| {
                        let account = format!("{:012}", 100_000_000_000u64 + index);
                        share_statement(json!({ "AWS": account }), &arn)
                    })
                    .collect(),
            )),
            policy_document(Value::Array(
                (0..40)
                    .map(|index| {
                        let account = format!("{:012}", 100_000_000_000u64 + index);
                        share_statement(json!({ "AWS": account_root(&account) }), &arn)
                    })
                    .collect(),
            )),
        ),
    ];

    for (case, submitted, attached) in cases {
        put_policy(&router, case, &arn, &submitted).await;
        let read = stored_policy(&router, &arn).await;
        assert_eq!(
            read, attached,
            "case {case:?}: GetResolverRulePolicy must report the attached document, got {read:?}"
        );
        // A read reports the policy without changing it: a second read of the
        // same rule answers the same document.
        let reread = stored_policy(&router, &arn).await;
        assert_eq!(
            reread, attached,
            "case {case:?}: a second GetResolverRulePolicy must report the same document, got {reread:?}"
        );
    }
}

/// A document that does not delegate the operations a share of a rule grants is
/// refused, and the policy the rule already carried stays.
#[tokio::test]
async fn a_policy_that_delegates_something_else_is_refused() {
    let router = emulator();
    let (_, arn) = system_rule(&router, "policy-undelegated", "undelegated.example.com").await;

    // The policy every row has to leave alone.
    let shared = policy_document(json!([share_statement(
        json!({ "AWS": OTHER_ACCOUNT }),
        &arn,
    )]));
    put_policy(&router, "the policy to preserve", &arn, &shared).await;
    let preserved = stored_policy(&router, &arn).await;

    // One statement delegating `action`, so the rows differ in the `Action`
    // section alone.
    let delegating = |action: Value| {
        policy_document(json!([{
            "Effect": "Allow",
            "Principal": { "AWS": OTHER_ACCOUNT },
            "Action": action,
            "Resource": arn,
        }]))
    };

    // Rows differ in HOW the document misses the delegated set: the read of the
    // rule alone (the document the service refuses with nothing else wrong with
    // it), the associations alone, the set with one operation too many, an
    // operation of another resource's share, the wildcard, none at all — and a
    // document with no statement to delegate anything.
    let cases: Vec<(&str, String)> = vec![
        (
            "the read of the rule alone",
            delegating(json!(["route53resolver:GetResolverRule"])),
        ),
        (
            "the read of the rule alone, as a single Action string",
            delegating(json!("route53resolver:GetResolverRule")),
        ),
        (
            "the association operations alone",
            delegating(json!([
                "route53resolver:AssociateResolverRule",
                "route53resolver:DisassociateResolverRule",
            ])),
        ),
        (
            "the delegated set and the tagging operation beside it",
            delegating(json!([
                "route53resolver:GetResolverRule",
                "route53resolver:AssociateResolverRule",
                "route53resolver:DisassociateResolverRule",
                "route53resolver:ListResolverRules",
                "route53resolver:ListResolverRuleAssociations",
                "route53resolver:TagResource",
            ])),
        ),
        (
            "the operations a DNS Firewall rule group's share delegates",
            delegating(json!([
                "route53resolver:GetFirewallRuleGroup",
                "route53resolver:ListFirewallRuleGroups",
            ])),
        ),
        (
            "every operation of the service, by wildcard",
            delegating(json!("route53resolver:*")),
        ),
        ("no operation at all", delegating(json!([]))),
        ("an empty statement list", policy_document(json!([]))),
        ("no Statement element", EMPTY_POLICY_DOCUMENT.to_string()),
    ];

    for (case, policy) in cases {
        expect_policy_refused(&router, case, &arn, &policy).await;
        let read = stored_policy(&router, &arn).await;
        assert_eq!(
            read, preserved,
            "case {case:?}: a refused document must leave the stored policy alone, got {read:?}"
        );
    }
}

/// A rule nobody has written a policy for reports the document that grants
/// nothing, rather than refusing the read.
#[tokio::test]
async fn a_rule_with_no_policy_reports_the_empty_document() {
    let router = emulator();
    let (_, arn) = system_rule(&router, "policy-absent", "absent.example.com").await;

    let read = stored_policy(&router, &arn).await;
    assert_eq!(
        read, EMPTY_POLICY_DOCUMENT,
        "a rule with no resource policy must report {EMPTY_POLICY_DOCUMENT}, got {read:?}"
    );
}

/// A document outside the policy grammar is refused with
/// `InvalidPolicyDocument`, and the policy the rule already carried stays.
#[tokio::test]
async fn an_invalid_document_is_refused_and_stores_nothing() {
    let router = emulator();
    let (_, arn) = system_rule(&router, "policy-invalid", "invalid.example.com").await;

    let shared = policy_document(json!([share_statement(
        json!({ "AWS": OTHER_ACCOUNT }),
        &arn,
    )]));
    put_policy(&router, "the policy to preserve", &arn, &shared).await;
    let preserved = stored_policy(&router, &arn).await;

    // Rows differ in which clause of the policy grammar the document breaks —
    // the one reading of it every service of this tree shares
    // (`service_common::iam_policy::resource_policy_defect`): it is not JSON at
    // all, it is JSON but not an object, or one of its statements is not a
    // statement or states no effect, no action or no principal.
    let cases: Vec<(&str, String)> = vec![
        ("the empty string", String::new()),
        ("not JSON", "this is not a policy".to_string()),
        ("truncated JSON", "{\"Statement\": [".to_string()),
        (
            "a JSON array",
            json!([share_statement(json!({ "AWS": OTHER_ACCOUNT }), &arn)]).to_string(),
        ),
        ("a JSON string", json!("Allow").to_string()),
        ("a JSON number", "17".to_string()),
        (
            "a statement that is not an object",
            policy_document(json!(["route53resolver:GetResolverRule"])),
        ),
        (
            "a statement with no Effect",
            policy_document(json!([{
                "Principal": { "AWS": OTHER_ACCOUNT },
                "Action": shared_actions(),
                "Resource": arn,
            }])),
        ),
        (
            "a statement with no Action",
            policy_document(json!([{
                "Effect": "Allow",
                "Principal": { "AWS": OTHER_ACCOUNT },
                "Resource": arn,
            }])),
        ),
        (
            "a statement with no Principal",
            policy_document(json!([{
                "Effect": "Allow",
                "Action": shared_actions(),
                "Resource": arn,
            }])),
        ),
        (
            "a second statement with no Effect",
            policy_document(json!([
                share_statement(json!({ "AWS": OTHER_ACCOUNT }), &arn),
                { "Action": shared_actions(), "Resource": arn },
            ])),
        ),
    ];

    for (case, policy) in cases {
        expect_policy_refused(&router, case, &arn, &policy).await;
        let read = stored_policy(&router, &arn).await;
        assert_eq!(
            read, preserved,
            "case {case:?}: a refused document must leave the stored policy alone, got {read:?}"
        );
    }
}

/// An `Arn` the store holds no Resolver rule under is refused with
/// `UnknownResourceException` by both operations, and the write persists
/// nothing for it.
#[tokio::test]
async fn an_arn_naming_no_rule_is_refused() {
    let router = emulator();
    let (deleted_id, deleted_arn) =
        system_rule(&router, "policy-deleted", "gone.example.com").await;
    let (live_id, live_arn) = system_rule(&router, "policy-live", "live.example.com").await;
    ok(
        &router,
        "DeleteResolverRule",
        json!({ "ResolverRuleId": deleted_id }),
    )
    .await;

    // Rows differ in how the ARN misses a stored rule: a rule id that was never
    // minted, a rule that has since been deleted, another resource type, another
    // region or account, and the rule's own id instead of its ARN.
    let cases: Vec<(&str, String)> = vec![
        (
            "a rule id nothing was created under",
            format!(
                "arn:aws:route53resolver:{REGION}:{ACCOUNT}:resolver-rule/rslvr-rr-0000000000000000"
            ),
        ),
        ("a deleted rule's ARN", deleted_arn),
        (
            "another resource type",
            live_arn.replace("resolver-rule/", "resolver-endpoint/"),
        ),
        ("another region", live_arn.replace(REGION, "eu-west-1")),
        (
            "another account",
            live_arn.replace(ACCOUNT, "123456789012"),
        ),
        ("the rule id, not its ARN", live_id.clone()),
    ];

    let policy = policy_document(json!([share_statement(
        json!({ "AWS": OTHER_ACCOUNT }),
        &live_arn,
    )]));

    for (case, arn) in cases {
        expect_error(
            &router,
            "PutResolverRulePolicy",
            json!({ "Arn": arn, "ResolverRulePolicy": policy }),
            case,
            "UnknownResourceException",
        )
        .await;
        expect_error(
            &router,
            "GetResolverRulePolicy",
            json!({ "Arn": arn }),
            case,
            "UnknownResourceException",
        )
        .await;
        // The refusal names the ARN that was asked for, so the caller can see
        // which resource the service found nothing under.
        let (_, refusal) = call(
            &router,
            "GetResolverRulePolicy",
            json!({ "Arn": arn.clone() }),
        )
        .await;
        let message = string_member(&refusal, "Message");
        assert!(
            message.contains(&arn),
            "case {case:?}: the refusal must name the ARN {arn:?}, got {message:?} ({})",
            error_type(&refusal)
        );
    }

    // The rule that does exist is untouched by every refusal above: it still
    // carries no policy.
    let read = stored_policy(&router, &live_arn).await;
    assert_eq!(
        read, EMPTY_POLICY_DOCUMENT,
        "a refused write must persist nothing, got {read:?}"
    );
}
