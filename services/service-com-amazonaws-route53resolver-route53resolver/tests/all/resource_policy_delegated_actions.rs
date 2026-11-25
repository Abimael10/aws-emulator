// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for what a Route 53 Resolver sharing policy has to
//! delegate, across BOTH operations that attach one: `PutFirewallRuleGroupPolicy`
//! and `PutResolverRulePolicy`.
//!
//! The rule under test: a resource policy on a Resolver resource is the share of
//! that resource, so a write attaches a document only when every statement of it
//! delegates the operations the service's share of that KIND of resource grants
//! and no other — a document that delegates fewer, more or other operations, or
//! that states no statement at all, asks for a share the service cannot make and
//! is refused with that operation's own invalid-policy class, leaving the policy
//! the resource already carried in place. Whom the document names as principal is
//! no part of the check; what a read reports is the service's own serialization
//! of the document that was attached, with each account named as a principal in
//! the form IAM converts an account id to.
//!
//! Each case is a row of a table that spans the two planes and, inside each, the
//! ways an `Action` section can differ from the delegated set: one operation
//! missing, one added, one substituted, the wildcard, the other plane's set, the
//! empty set and no statement at all. The delegated sets are written out here
//! rather than read from the crate, with the source of each named beside it, so
//! the catalogue itself is what the suite pins.
//!
//! Every case goes over the wire the service actually speaks (awsJson1_1,
//! `POST /` with `X-Amz-Target: Route53Resolver.<OperationName>`), through the
//! same router the emulator mounts, so what is asserted is the HTTP behaviour
//! and not a handler called directly. The transport helpers are the shared ones
//! in [`super::harness`].

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::harness::{call, emulator, error_type, ok, string_member, ACCOUNT};

/// An account other than the one this emulator's resources belong to, so a
/// document that names it shares the resource with somebody else.
const OTHER_ACCOUNT: &str = "123456789012";

/// The operations a share of one DNS Firewall rule group delegates.
///
/// Source: AWS's sharing policy for a DNS Firewall rule group names these two
/// operations of the service — the read of the group and the listing — in the
/// `Action` section of its statement; a share of a rule group delegates nothing
/// that changes it.
const FIREWALL_RULE_GROUP_ACTIONS: &[&str] = &[
    "route53resolver:GetFirewallRuleGroup",
    "route53resolver:ListFirewallRuleGroups",
];

/// The operations a share of one Resolver rule delegates.
///
/// Source: the model's documentation of the member that carries the document,
/// `smithy_query(query="doc",shape="PutResolverRulePolicyRequest",member="ResolverRulePolicy")`,
/// which lists the operations "you can specify ... in the `Action` section of
/// the statement".
const RESOLVER_RULE_ACTIONS: &[&str] = &[
    "route53resolver:GetResolverRule",
    "route53resolver:AssociateResolverRule",
    "route53resolver:DisassociateResolverRule",
    "route53resolver:ListResolverRules",
    "route53resolver:ListResolverRuleAssociations",
];

/// One sharing plane: the resource whose policy is written, the operations that
/// write and read it, the member that carries the document, the error class a
/// document the service will not attach is refused with, and the operations a
/// share of that resource delegates.
struct Plane {
    resource: &'static str,
    put: &'static str,
    get: &'static str,
    member: &'static str,
    refusal: &'static str,
    actions: &'static [&'static str],
}

/// Both planes, so every case below is driven over each of them.
const PLANES: [Plane; 2] = [
    Plane {
        resource: "a DNS Firewall rule group",
        put: "PutFirewallRuleGroupPolicy",
        get: "GetFirewallRuleGroupPolicy",
        member: "FirewallRuleGroupPolicy",
        refusal: "ValidationException",
        actions: FIREWALL_RULE_GROUP_ACTIONS,
    },
    Plane {
        resource: "a Resolver rule",
        put: "PutResolverRulePolicy",
        get: "GetResolverRulePolicy",
        member: "ResolverRulePolicy",
        refusal: "InvalidPolicyDocument",
        actions: RESOLVER_RULE_ACTIONS,
    },
];

/// Create the resource `plane` shares and answer its ARN.
///
/// A rule group is created empty and a SYSTEM rule needs no endpoint and no
/// target addresses, so a policy case needs nothing else stored.
async fn shareable_resource(router: &MultiServiceRouter, plane: &Plane, creator: &str) -> String {
    if plane.member == "FirewallRuleGroupPolicy" {
        let created = ok(
            router,
            "CreateFirewallRuleGroup",
            json!({ "CreatorRequestId": creator, "Name": creator }),
        )
        .await;
        let group = created
            .get("FirewallRuleGroup")
            .expect("the create answers a FirewallRuleGroup");
        return string_member(group, "Arn");
    }
    let created = ok(
        router,
        "CreateResolverRule",
        json!({
            "CreatorRequestId": creator,
            "Name": creator,
            "RuleType": "SYSTEM",
            "DomainName": format!("{creator}.example.com"),
        }),
    )
    .await;
    let rule = created
        .get("ResolverRule")
        .expect("the create answers a ResolverRule");
    string_member(rule, "Arn")
}

/// The ARN of the root user of the account `account` names — the form IAM
/// converts an account id named as a principal to.
fn account_root(account: &str) -> String {
    format!("arn:aws:iam::{account}:root")
}

/// A whole document whose one statement shares `arn` with `principal` and
/// delegates `actions`.
fn document(principal: Value, actions: Value, arn: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": principal,
            "Action": actions,
            "Resource": arn,
        }],
    })
    .to_string()
}

/// The `Action` section naming every operation of `actions`.
fn action_list(actions: &[&str]) -> Value {
    Value::Array(actions.iter().map(|action| json!(action)).collect())
}

/// The document a read of `plane`'s resource `arn` reports.
async fn stored_policy(router: &MultiServiceRouter, plane: &Plane, arn: &str) -> String {
    let read = ok(router, plane.get, json!({ "Arn": arn })).await;
    string_member(&read, plane.member)
}

/// The request body of a policy write: the resource's `Arn` and the document,
/// under the member name this plane carries it in.
fn write_body(plane: &Plane, arn: &str, policy: &str) -> Value {
    let mut body = serde_json::Map::new();
    body.insert("Arn".to_string(), json!(arn));
    body.insert(plane.member.to_string(), json!(policy));
    Value::Object(body)
}

/// Write `policy` for `arn` and assert the call reported success.
async fn put_policy(router: &MultiServiceRouter, plane: &Plane, case: &str, arn: &str, policy: &str) {
    let written = ok(router, plane.put, write_body(plane, arn, policy)).await;
    assert_eq!(
        written.get("ReturnValue"),
        Some(&Value::Bool(true)),
        "case {case:?}: {} must report ReturnValue true, got {written}",
        plane.put
    );
}

/// A document that delegates exactly the operations the share grants is
/// attached, whichever plane it is written on and however its `Action` section
/// is written.
#[tokio::test]
async fn the_delegated_operations_are_attached() {
    let router = emulator();

    for plane in &PLANES {
        let arn = shareable_resource(&router, plane, "delegated-accepted").await;

        // Rows differ in how the one delegated set is WRITTEN: in the
        // documented order, in another order, with an operation repeated — all
        // of them the same set of operations.
        let mut reordered: Vec<&str> = plane.actions.to_vec();
        reordered.reverse();
        let mut repeated: Vec<&str> = plane.actions.to_vec();
        repeated.push(plane.actions[0]);
        let cases: Vec<(&str, Value)> = vec![
            ("the delegated operations, as documented", action_list(plane.actions)),
            ("the delegated operations, in reverse order", action_list(&reordered)),
            ("the delegated operations, one of them twice", action_list(&repeated)),
        ];

        for (case, actions) in cases {
            let case = format!("{}: {case}", plane.resource);
            let policy = document(json!({ "AWS": OTHER_ACCOUNT }), actions.clone(), &arn);
            put_policy(&router, plane, &case, &arn, &policy).await;
            let read = stored_policy(&router, plane, &arn).await;
            let expected = document(
                json!({ "AWS": account_root(OTHER_ACCOUNT) }),
                actions,
                &arn,
            );
            assert_eq!(
                read, expected,
                "case {case:?}: the read must report the attached document, got {read:?}"
            );
        }
    }
}

/// A document whose statement delegates anything other than the operations the
/// share grants is refused, and the policy the resource already carried stays.
#[tokio::test]
async fn an_undelegated_operation_set_is_refused() {
    let router = emulator();

    for plane in &PLANES {
        let arn = shareable_resource(&router, plane, "delegated-refused").await;

        // The policy every row below has to leave in place.
        let attached = document(
            json!({ "AWS": OTHER_ACCOUNT }),
            action_list(plane.actions),
            &arn,
        );
        put_policy(&router, plane, "the policy to preserve", &arn, &attached).await;
        let preserved = stored_policy(&router, plane, &arn).await;

        // Rows differ in HOW the `Action` section misses the delegated set: an
        // operation dropped (each one in turn), one added, one substituted, the
        // wildcard that names them all, the other plane's set, no operation at
        // all, and a document with nothing to delegate.
        let other_plane_actions = if plane.actions == FIREWALL_RULE_GROUP_ACTIONS {
            RESOLVER_RULE_ACTIONS
        } else {
            FIREWALL_RULE_GROUP_ACTIONS
        };
        let mut cases: Vec<(String, String)> = Vec::new();
        for dropped in plane.actions {
            let kept: Vec<&str> = plane
                .actions
                .iter()
                .copied()
                .filter(|action| action != dropped)
                .collect();
            cases.push((
                format!("every delegated operation but {dropped}"),
                document(
                    json!({ "AWS": OTHER_ACCOUNT }),
                    action_list(&kept),
                    &arn,
                ),
            ));
        }
        let mut with_extra: Vec<&str> = plane.actions.to_vec();
        with_extra.push("route53resolver:TagResource");
        cases.push((
            "the delegated operations and one the share does not grant".to_string(),
            document(
                json!({ "AWS": OTHER_ACCOUNT }),
                action_list(&with_extra),
                &arn,
            ),
        ));
        let mut substituted: Vec<&str> = plane.actions[1..].to_vec();
        substituted.push("route53resolver:UntagResource");
        cases.push((
            "one delegated operation swapped for another operation".to_string(),
            document(
                json!({ "AWS": OTHER_ACCOUNT }),
                action_list(&substituted),
                &arn,
            ),
        ));
        cases.push((
            "every operation of the service, by wildcard".to_string(),
            document(
                json!({ "AWS": OTHER_ACCOUNT }),
                json!("route53resolver:*"),
                &arn,
            ),
        ));
        cases.push((
            "one delegated operation alone, as a single Action string".to_string(),
            document(
                json!({ "AWS": OTHER_ACCOUNT }),
                json!(plane.actions[0]),
                &arn,
            ),
        ));
        cases.push((
            "the set the OTHER resource's share delegates".to_string(),
            document(
                json!({ "AWS": OTHER_ACCOUNT }),
                action_list(other_plane_actions),
                &arn,
            ),
        ));
        cases.push((
            "no operation at all".to_string(),
            document(json!({ "AWS": OTHER_ACCOUNT }), json!([]), &arn),
        ));
        cases.push((
            "a second statement that delegates something else".to_string(),
            json!({
                "Version": "2012-10-17",
                "Statement": [
                    {
                        "Effect": "Allow",
                        "Principal": { "AWS": OTHER_ACCOUNT },
                        "Action": action_list(plane.actions),
                        "Resource": arn,
                    },
                    {
                        "Effect": "Allow",
                        "Principal": { "AWS": ACCOUNT },
                        "Action": ["route53resolver:TagResource"],
                        "Resource": arn,
                    },
                ],
            })
            .to_string(),
        ));
        cases.push((
            "a document with no statement to delegate anything".to_string(),
            json!({ "Version": "2012-10-17" }).to_string(),
        ));
        cases.push((
            "an empty statement list".to_string(),
            json!({ "Version": "2012-10-17", "Statement": [] }).to_string(),
        ));

        for (case, policy) in cases {
            let case = format!("{}: {case}", plane.resource);
            let (status, refusal) = call(&router, plane.put, write_body(plane, &arn, &policy)).await;
            assert_eq!(
                status, 400,
                "case {case:?}: the write must answer HTTP 400, got {status}: {refusal}"
            );
            assert!(
                error_type(&refusal).contains(plane.refusal),
                "case {case:?}: the write must answer {}, got {refusal}",
                plane.refusal
            );
            let read = stored_policy(&router, plane, &arn).await;
            assert_eq!(
                read, preserved,
                "case {case:?}: a refused document must leave the stored policy alone, got {read:?}"
            );
        }
    }
}

/// Every account a stored document names as a principal is reported in the
/// root-user ARN form, and nothing else about the document changes: a principal
/// of another kind, the wildcard and an identity named by its own ARN are
/// reported as written, and the whole document is reported in the service's own
/// serialization whatever whitespace the request wrote it with.
#[tokio::test]
async fn a_stored_document_reports_each_account_principal_as_its_root_user() {
    let router = emulator();

    for plane in &PLANES {
        let arn = shareable_resource(&router, plane, "delegated-principals").await;
        let actions = action_list(plane.actions);

        // Rows differ in the FORM the principal is written in: an account id, a
        // list of them, an account's root ARN, a principal naming an identity
        // inside an account, a service principal, the wildcard — and in the
        // whitespace the document carries.
        let cases: Vec<(&str, String, String)> = vec![
            (
                "an account id",
                document(json!({ "AWS": OTHER_ACCOUNT }), actions.clone(), &arn),
                document(
                    json!({ "AWS": account_root(OTHER_ACCOUNT) }),
                    actions.clone(),
                    &arn,
                ),
            ),
            (
                "two account ids, one of them this account's",
                document(
                    json!({ "AWS": [ACCOUNT, OTHER_ACCOUNT] }),
                    actions.clone(),
                    &arn,
                ),
                document(
                    json!({ "AWS": [account_root(ACCOUNT), account_root(OTHER_ACCOUNT)] }),
                    actions.clone(),
                    &arn,
                ),
            ),
            (
                "an account's root ARN, already in the converted form",
                document(
                    json!({ "AWS": account_root(OTHER_ACCOUNT) }),
                    actions.clone(),
                    &arn,
                ),
                document(
                    json!({ "AWS": account_root(OTHER_ACCOUNT) }),
                    actions.clone(),
                    &arn,
                ),
            ),
            (
                "a role inside another account",
                document(
                    json!({ "AWS": format!("arn:aws:iam::{OTHER_ACCOUNT}:role/auditor") }),
                    actions.clone(),
                    &arn,
                ),
                document(
                    json!({ "AWS": format!("arn:aws:iam::{OTHER_ACCOUNT}:role/auditor") }),
                    actions.clone(),
                    &arn,
                ),
            ),
            (
                "a service principal",
                document(
                    json!({ "Service": "ram.amazonaws.com" }),
                    actions.clone(),
                    &arn,
                ),
                document(
                    json!({ "Service": "ram.amazonaws.com" }),
                    actions.clone(),
                    &arn,
                ),
            ),
            (
                "the wildcard principal",
                document(json!("*"), actions.clone(), &arn),
                document(json!("*"), actions.clone(), &arn),
            ),
            (
                "a document written over several lines",
                format!(
                    "{{\n  \"Version\": \"2012-10-17\",\n  \"Statement\": [\n    {{\n      \
                     \"Effect\": \"Allow\",\n      \"Principal\": {{ \"AWS\": \
                     \"{OTHER_ACCOUNT}\" }},\n      \"Action\": {},\n      \"Resource\": \
                     \"{arn}\"\n    }}\n  ]\n}}\n",
                    actions.clone()
                ),
                document(
                    json!({ "AWS": account_root(OTHER_ACCOUNT) }),
                    actions.clone(),
                    &arn,
                ),
            ),
        ];

        for (case, submitted, attached) in cases {
            let case = format!("{}: {case}", plane.resource);
            put_policy(&router, plane, &case, &arn, &submitted).await;
            let read = stored_policy(&router, plane, &arn).await;
            assert_eq!(
                read, attached,
                "case {case:?}: the read must report the attached document, got {read:?}"
            );
            // The read changed nothing: a second one answers the same document.
            let reread = stored_policy(&router, plane, &arn).await;
            assert_eq!(
                reread, attached,
                "case {case:?}: a second read must report the same document, got {reread:?}"
            );
        }
    }
}
