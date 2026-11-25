// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the DNS Firewall rule-group policy slice:
//! `PutFirewallRuleGroupPolicy` and `GetFirewallRuleGroupPolicy`.
//!
//! The rule under test: a rule group's resource policy is the share of that
//! group, so the write attaches a document that asks for the share the service
//! can make — a policy document whose every statement delegates the rule-group
//! operations a share grants and no other — whoever the document names as
//! principal, and refuses every other document, leaving the policy the group
//! already carried in place. What a read reports is the service's own
//! serialization of the document that was attached: the elements in the order
//! they were written, with each account named as a principal in the form IAM
//! converts an account id to. A group nobody has written a policy for reports
//! the document that grants nothing, and an `Arn` the store holds no group under
//! is refused by both operations.
//!
//! Each suite drives a TABLE of documents spanning the forms the policy grammar
//! admits, so no case is tied to one document.
//!
//! Every case goes over the wire the service actually speaks (awsJson1_1,
//! `POST /` with `X-Amz-Target: Route53Resolver.<OperationName>`), through the
//! same router the emulator mounts, so what is asserted is the HTTP behaviour
//! and not a handler called directly. The transport helpers are the shared ones
//! in [`super::harness`].

use serde_json::{json, Value};
use service_com_amazonaws_route53resolver_route53resolver::state::{
    EMPTY_POLICY_DOCUMENT, RESOURCE_TYPE_FIREWALL_RULE_GROUP, RSLVR_CODE_INVALID_IAM_POLICY,
    RSLVR_CODE_RESOURCE_NOT_FOUND,
};
use smithy_server_runtime::MultiServiceRouter;

use super::harness::{
    call, emulator, error_type, expect_error, ok, string_member, traced_message, ACCOUNT, REGION,
};

/// An account that is not the one this emulator's resources belong to, so a
/// policy naming it really does share the rule group with somebody else.
const OTHER_ACCOUNT: &str = "123456789012";

/// Create a DNS Firewall rule group and return `(Id, Arn)`. A group is created
/// empty, so a policy case needs nothing else stored.
async fn rule_group(router: &MultiServiceRouter, creator: &str) -> (String, String) {
    let created = ok(
        router,
        "CreateFirewallRuleGroup",
        json!({ "CreatorRequestId": creator, "Name": creator }),
    )
    .await;
    let group = created
        .get("FirewallRuleGroup")
        .expect("the create answers a FirewallRuleGroup");
    (string_member(group, "Id"), string_member(group, "Arn"))
}

/// The `FirewallRuleGroupPolicy` a read of `arn` reports.
async fn stored_policy(router: &MultiServiceRouter, arn: &str) -> String {
    let read = ok(router, "GetFirewallRuleGroupPolicy", json!({ "Arn": arn })).await;
    string_member(&read, "FirewallRuleGroupPolicy")
}

/// Write `policy` for `arn` and assert the call reported success.
async fn put_policy(router: &MultiServiceRouter, case: &str, arn: &str, policy: &str) {
    let written = ok(
        router,
        "PutFirewallRuleGroupPolicy",
        json!({ "Arn": arn, "FirewallRuleGroupPolicy": policy }),
    )
    .await;
    assert_eq!(
        written.get("ReturnValue"),
        Some(&Value::Bool(true)),
        "case {case:?}: PutFirewallRuleGroupPolicy must report ReturnValue true, got {written}"
    );
}

/// Write `policy` for `arn`, require the refusal the service answers for a
/// document it will not attach, and answer the refusal's message.
async fn refused_policy(
    router: &MultiServiceRouter,
    case: &str,
    arn: &str,
    policy: &str,
) -> String {
    let (status, refusal) = call(
        router,
        "PutFirewallRuleGroupPolicy",
        json!({ "Arn": arn, "FirewallRuleGroupPolicy": policy }),
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: the write must answer HTTP 400, got {status}: {refusal}"
    );
    assert!(
        error_type(&refusal).contains("ValidationException"),
        "case {case:?}: the write must answer ValidationException, got {refusal}"
    );
    string_member(&refusal, "Message")
}

/// The ARN of the root user of the account `account` names — the form IAM
/// converts an account id named as a principal to.
fn account_root(account: &str) -> String {
    format!("arn:aws:iam::{account}:root")
}

/// The `Action` section of a rule-group sharing statement: the operations a
/// share of a rule group delegates.
///
/// Source: AWS's sharing policy for a DNS Firewall rule group names the read of
/// the group and the listing, and nothing that changes it; the same two
/// operations the crate carries as the delegated set.
fn shared_actions() -> Value {
    json!([
        "route53resolver:GetFirewallRuleGroup",
        "route53resolver:ListFirewallRuleGroups",
    ])
}

/// One statement sharing `arn` with `principal`, in the grammar's statement
/// form: an `Effect`, the delegated `Action` set and the `Resource` the share is
/// for.
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

/// A document that delegates the operations a share of a rule group grants is
/// attached, and the read reports the service's serialization of it — whatever
/// form the grammar lets the document take.
#[tokio::test]
async fn a_delegating_policy_is_reported_back_as_attached() {
    let router = emulator();
    let (_, arn) = rule_group(&router, "policy-shared").await;

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
        // The emulator serves one account, so a caller sharing a group has its
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
                    "Sid": "ShareOneRuleGroup",
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
                    "Sid": "ShareOneRuleGroup",
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
            "case {case:?}: GetFirewallRuleGroupPolicy must report the attached document, got {read:?}"
        );
        // A read reports the policy without changing it: a second read of the
        // same group answers the same document.
        let reread = stored_policy(&router, &arn).await;
        assert_eq!(
            reread, attached,
            "case {case:?}: a second GetFirewallRuleGroupPolicy must report the same document, got {reread:?}"
        );
    }
}

/// A document that does not delegate the operations a share of a rule group
/// grants is refused, and the policy the group already carried stays.
#[tokio::test]
async fn a_policy_that_delegates_something_else_is_refused() {
    let router = emulator();
    let (_, arn) = rule_group(&router, "policy-undelegated").await;

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

    // Rows differ in HOW the document misses the delegated set: one operation
    // of it, the other, one too many, an operation of another resource's share,
    // the wildcard, none at all — and a document with no statement to delegate
    // anything.
    let cases: Vec<(&str, String)> = vec![
        (
            "the read of the group alone",
            delegating(json!(["route53resolver:GetFirewallRuleGroup"])),
        ),
        (
            "the listing alone",
            delegating(json!(["route53resolver:ListFirewallRuleGroups"])),
        ),
        (
            "the read of the group alone, as a single Action string",
            delegating(json!("route53resolver:GetFirewallRuleGroup")),
        ),
        (
            "the delegated set and the association operations beside it",
            delegating(json!([
                "route53resolver:GetFirewallRuleGroup",
                "route53resolver:AssociateFirewallRuleGroup",
                "route53resolver:DisassociateFirewallRuleGroup",
                "route53resolver:ListFirewallRuleGroups",
            ])),
        ),
        (
            "the operations a Resolver rule's share delegates",
            delegating(json!([
                "route53resolver:GetResolverRule",
                "route53resolver:ListResolverRules",
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
        let message = refused_policy(&router, case, &arn, &policy).await;
        // The refusal states the service's code for a policy it will not
        // attach, echoes the document it was handed so the caller sees which
        // document was refused, and closes with the trace clause of the
        // request.
        let (stated, _) = traced_message(&message).unwrap_or_else(|| {
            panic!("case {case:?}: the refusal must close with its trace clause, got {message:?}")
        });
        assert!(
            stated.contains(RSLVR_CODE_INVALID_IAM_POLICY) && stated.contains(&policy),
            "case {case:?}: the refusal must state {RSLVR_CODE_INVALID_IAM_POLICY} and echo the document, got {message:?}"
        );
        let read = stored_policy(&router, &arn).await;
        assert_eq!(
            read, preserved,
            "case {case:?}: a refused document must leave the stored policy alone, got {read:?}"
        );
    }
}

/// A rule group nobody has written a policy for reports the document that
/// grants nothing, rather than refusing the read.
#[tokio::test]
async fn a_rule_group_with_no_policy_reports_the_empty_document() {
    let router = emulator();
    let (_, arn) = rule_group(&router, "policy-absent").await;

    let read = stored_policy(&router, &arn).await;
    assert_eq!(
        read, EMPTY_POLICY_DOCUMENT,
        "a rule group with no resource policy must report {EMPTY_POLICY_DOCUMENT}, got {read:?}"
    );
    // The read changed nothing: a second one answers the same document.
    let reread = stored_policy(&router, &arn).await;
    assert_eq!(
        reread, EMPTY_POLICY_DOCUMENT,
        "a second read must report {EMPTY_POLICY_DOCUMENT}, got {reread:?}"
    );
}

/// A document outside the policy grammar is refused, and the policy the group
/// already carried stays.
#[tokio::test]
async fn an_invalid_document_is_refused_and_stores_nothing() {
    let router = emulator();
    let (_, arn) = rule_group(&router, "policy-invalid").await;

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
            policy_document(json!(["route53resolver:GetFirewallRuleGroup"])),
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
        let message = refused_policy(&router, case, &arn, &policy).await;
        let (stated, _) = traced_message(&message).unwrap_or_else(|| {
            panic!("case {case:?}: the refusal must close with its trace clause, got {message:?}")
        });
        assert!(
            stated.contains(RSLVR_CODE_INVALID_IAM_POLICY) && stated.contains(&policy),
            "case {case:?}: the refusal must state {RSLVR_CODE_INVALID_IAM_POLICY} and echo the document, got {message:?}"
        );
        let read = stored_policy(&router, &arn).await;
        assert_eq!(
            read, preserved,
            "case {case:?}: a refused document must leave the stored policy alone, got {read:?}"
        );
    }
}

/// An `Arn` the store holds no rule group under is refused with
/// `ResourceNotFoundException` by both operations, and the write persists
/// nothing for it.
#[tokio::test]
async fn an_arn_naming_no_rule_group_is_refused() {
    let router = emulator();
    let (live_id, live_arn) = rule_group(&router, "policy-live").await;

    // Rows differ in how the ARN misses a stored group: a group id that was
    // never minted, another resource type, another region or account, and the
    // group's own id instead of its ARN.
    let cases: Vec<(&str, String)> = vec![
        (
            "a rule group id nothing was created under",
            format!(
                "arn:aws:route53resolver:{REGION}:{ACCOUNT}:firewall-rule-group/rslvr-frg-0000000000000000"
            ),
        ),
        (
            "another resource type",
            live_arn.replace("firewall-rule-group/", "firewall-domain-list/"),
        ),
        ("another region", live_arn.replace(REGION, "eu-west-1")),
        (
            "another account",
            live_arn.replace(ACCOUNT, OTHER_ACCOUNT),
        ),
        ("the rule group id, not its ARN", live_id.clone()),
    ];

    let policy = policy_document(json!([share_statement(
        json!({ "AWS": OTHER_ACCOUNT }),
        &live_arn,
    )]));

    for (case, arn) in cases {
        expect_error(
            &router,
            "PutFirewallRuleGroupPolicy",
            json!({ "Arn": arn, "FirewallRuleGroupPolicy": policy }),
            case,
            "ResourceNotFoundException",
        )
        .await;
        expect_error(
            &router,
            "GetFirewallRuleGroupPolicy",
            json!({ "Arn": arn }),
            case,
            "ResourceNotFoundException",
        )
        .await;
        // The refusal names the resource by the id the ARN carries — never the
        // ARN as submitted — and the type of resource the service found nothing
        // of.
        let (_, refusal) = call(
            &router,
            "GetFirewallRuleGroupPolicy",
            json!({ "Arn": arn.clone() }),
        )
        .await;
        let message = string_member(&refusal, "Message");
        // The refusal closes with the trace clause of the request, whose id is a
        // different value on every call, so what is compared is the sentence
        // before it.
        let (stated, _) = traced_message(&message).unwrap_or_else(|| {
            panic!(
                "case {case:?}: the refusal must close with its trace clause, got {message:?}"
            )
        });
        let expected = format!(
            "[{RSLVR_CODE_RESOURCE_NOT_FOUND}] Can’t find the resource with ID \"{}\".",
            arn.rsplit_once('/').map_or(arn.as_str(), |(_, id)| id)
        );
        assert_eq!(
            stated,
            expected,
            "case {case:?}: the refusal must name the rule group id, got {message:?} ({})",
            error_type(&refusal)
        );
        assert_eq!(
            string_member(&refusal, "ResourceType"),
            RESOURCE_TYPE_FIREWALL_RULE_GROUP,
            "case {case:?}: the refusal must name the resource type, got {refusal}"
        );
    }

    // The group that does exist is untouched by every refusal above: it still
    // carries no policy.
    let read = stored_policy(&router, &live_arn).await;
    assert_eq!(
        read, EMPTY_POLICY_DOCUMENT,
        "a refused write must persist nothing, got {read:?}"
    );
}
