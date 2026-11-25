// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the BODY of the refusal `PutFirewallRuleGroupPolicy`
//! answers for an IAM policy document it will not attach.
//!
//! The rule under test: the refusal carries the whole wording the service words
//! that fault with — its service error code, the fault sentence echoing the
//! submitted document byte for byte, the remediation sentence that states what a
//! document the service would attach has to do, AND the trace clause of the
//! request — so no part of it is dropped, whichever class of document was
//! refused and whatever the document looks like.
//!
//! The table spans both classes of refused document (one outside the policy
//! grammar, one that does not delegate the operations a share of a rule group
//! grants) and, inside each, the forms a document takes: its type, its size, its
//! internal whitespace and the characters it carries.
//!
//! The message is asserted in FULL: the service answers this refusal with the
//! code, the fault sentence, the remediation sentence and the trace clause of
//! the request and nothing else, so no part of it is cut before the comparison —
//! and the trace the clause reports is one the case itself chose, so the only
//! per-request value in the message is pinned too.
//!
//! Every case goes over the wire the service actually speaks (awsJson1_1,
//! `POST /` with `X-Amz-Target: Route53Resolver.<OperationName>`), through the
//! same router the emulator mounts, so what is asserted is the HTTP behaviour
//! and not a handler called directly. The transport helpers are the shared ones
//! in [`super::harness`].

use serde_json::{json, Value};
use service_com_amazonaws_route53resolver_route53resolver::state::EMPTY_POLICY_DOCUMENT;

use super::harness::{
    call_tracing, emulator, error_type, ok, string_member, tracing_chain, ACCOUNT,
};

/// An account that is not the one this emulator's resources belong to, so a
/// policy naming it really does share the rule group with somebody else.
const OTHER_ACCOUNT: &str = "123456789012";

/// The whole message the service answers for a document it will not attach,
/// written out here rather than built from the emulator's own constants so the
/// wording itself is what this suite pins: the code in square brackets, the
/// fault sentence with the submitted document double-quoted inside it, the
/// remediation sentence, and the trace clause naming the trace of the request.
fn expected_message(policy: &str, trace_root: &str) -> String {
    format!(
        "[RSLVR-02035] The specified IAM policy isn't valid: \"{policy}\". Provide a valid policy \
         that allows a DNS Firewall rule group to be shared with another AWS account. Trace Id: \
         \"{trace_root}\""
    )
}

/// The `Action` section of a rule-group sharing statement: the operations a
/// share of a rule group delegates.
///
/// Source: AWS's sharing policy for a DNS Firewall rule group names the read of
/// the group and the listing, and nothing that changes it.
fn shared_actions() -> Value {
    json!([
        "route53resolver:GetFirewallRuleGroup",
        "route53resolver:ListFirewallRuleGroups",
    ])
}

/// A document that shares `arn` with `OTHER_ACCOUNT` and delegates the
/// operations a share grants, which the service does attach: the policy every
/// row below has to leave in place.
fn sharing_document(arn: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "AWS": OTHER_ACCOUNT },
            "Action": shared_actions(),
            "Resource": arn,
        }],
    })
    .to_string()
}

/// The document a read of that attached policy reports: the same document with
/// the account it names written as that account's root user, which is how the
/// service reports a stored policy.
fn attached_document(arn: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "AWS": format!("arn:aws:iam::{OTHER_ACCOUNT}:root") },
            "Action": shared_actions(),
            "Resource": arn,
        }],
    })
    .to_string()
}

/// The documents the write refuses, and what makes each one refusable.
///
/// Rows 1-5 miss the policy grammar; rows 6-10 are documents the grammar admits
/// that do not delegate the operations a share of a rule group grants. Within
/// each class they differ in the document's JSON type, its size, its internal
/// whitespace and the characters it carries.
fn refused_documents(arn: &str) -> Vec<(&'static str, String)> {
    vec![
        ("the empty string", String::new()),
        ("not JSON at all", "this is not a policy".to_string()),
        ("a JSON number", "17".to_string()),
        (
            "a JSON string carrying a quote and a non-ASCII character",
            json!("Allow \"everyone\" — really").to_string(),
        ),
        (
            "a statement that is not an object",
            json!({ "Version": "2012-10-17", "Statement": ["route53resolver:*"] }).to_string(),
        ),
        (
            "a document with no Statement element",
            EMPTY_POLICY_DOCUMENT.to_string(),
        ),
        (
            "one delegated operation only, pretty-printed over several lines",
            format!(
                "{{\n  \"Version\": \"2012-10-17\",\n  \"Statement\": [\n    {{\n      \
                 \"Effect\": \"Allow\",\n      \"Principal\": {{ \"AWS\": \
                 \"{OTHER_ACCOUNT}\" }},\n      \"Action\": \
                 \"route53resolver:GetFirewallRuleGroup\",\n      \"Resource\": \"{arn}\"\n    \
                 }}\n  ]\n}}\n"
            ),
        ),
        (
            "the other delegated operation only, with tabs between the elements",
            format!(
                "{{\t\"Version\":\t\"2012-10-17\",\t\"Statement\":\t[{{\t\"Effect\":\t\"Allow\",\
                 \t\"Principal\":\t{{\t\"AWS\":\t\"{ACCOUNT}\"\t}},\t\"Action\":\t\
                 \"route53resolver:ListFirewallRuleGroups\",\t\"Resource\":\t\"{arn}\"\t}}]\t}}"
            ),
        ),
        ("an empty statement list", {
            json!({ "Version": "2012-10-17", "Statement": [] }).to_string()
        }),
        (
            "many statements, every one of them delegating the tagging operation",
            json!({
                "Version": "2012-10-17",
                "Statement": Value::Array(
                    (0..60)
                        .map(|index| {
                            json!({
                                "Sid": format!("TagOnly{index}"),
                                "Effect": "Allow",
                                "Principal": { "AWS": OTHER_ACCOUNT },
                                "Action": "route53resolver:TagResource",
                                "Resource": arn,
                            })
                        })
                        .collect(),
                ),
            })
            .to_string(),
        ),
    ]
}

/// Every refused document is answered with the whole message — the two
/// sentences, the document echoed inside them exactly as it was submitted, and
/// the trace clause of the request — and the rule group keeps the policy it
/// already carried.
#[tokio::test]
async fn a_refused_policy_document_is_answered_with_the_whole_message() {
    let router = emulator();
    let created = ok(
        &router,
        "CreateFirewallRuleGroup",
        json!({ "CreatorRequestId": "policy-message", "Name": "policy-message" }),
    )
    .await;
    let group = created
        .get("FirewallRuleGroup")
        .expect("the create answers a FirewallRuleGroup");
    let arn = string_member(group, "Arn");

    // The policy the group carries while every row below is refused.
    let written = ok(
        &router,
        "PutFirewallRuleGroupPolicy",
        json!({ "Arn": arn, "FirewallRuleGroupPolicy": sharing_document(&arn) }),
    )
    .await;
    assert_eq!(
        written.get("ReturnValue"),
        Some(&Value::Bool(true)),
        "a document delegating the operations a share grants must be attached, got {written}"
    );

    for (index, (case, policy)) in refused_documents(&arn).into_iter().enumerate() {
        // Each row names its own trace, so the clause the refusal closes with
        // is pinned rather than read back out of the message.
        let (tracing, root) = tracing_chain(index as u32);
        let message = refusal_message(&router, case, &arn, &policy, &tracing).await;
        let expected = expected_message(&policy, &root);
        assert_eq!(
            message, expected,
            "case {case:?}: expected message {expected:?}, got {message:?}"
        );

        let read = ok(&router, "GetFirewallRuleGroupPolicy", json!({ "Arn": arn })).await;
        let stored = string_member(&read, "FirewallRuleGroupPolicy");
        assert_eq!(
            stored,
            attached_document(&arn),
            "case {case:?}: a refused document must leave the stored policy alone, got {stored:?}"
        );
    }
}

/// Read the refusal `PutFirewallRuleGroupPolicy` answers for `policy` on a
/// request carrying the tracing header `tracing`, requiring the status and the
/// error shape the service refuses a document it will not attach with.
async fn refusal_message(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    arn: &str,
    policy: &str,
    tracing: &str,
) -> String {
    let (status, refusal) = call_tracing(
        router,
        "PutFirewallRuleGroupPolicy",
        json!({ "Arn": arn, "FirewallRuleGroupPolicy": policy }),
        Some(tracing),
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: the write must answer HTTP 400, got {status}: {refusal}"
    );
    let answered = error_type(&refusal);
    assert!(
        answered.contains("ValidationException"),
        "case {case:?}: the write must answer ValidationException, got {answered:?}: {refusal}"
    );
    string_member(&refusal, "Message")
}

/// The message states the document it was handed and the trace of the request,
/// and nothing else: two groups refusing the same document under the same trace
/// answer the same wording, and one group refusing two documents answers two
/// messages differing only in the echoed document.
#[tokio::test]
async fn the_echoed_document_is_the_messages_only_variable_part() {
    let router = emulator();

    // Two groups, so the refusal cannot be carrying the group it was asked
    // about: the ARNs differ, the documents do not.
    let mut arns = Vec::new();
    for name in ["message-group-a", "message-group-b"] {
        let created = ok(
            &router,
            "CreateFirewallRuleGroup",
            json!({ "CreatorRequestId": name, "Name": name }),
        )
        .await;
        let group = created
            .get("FirewallRuleGroup")
            .expect("the create answers a FirewallRuleGroup");
        arns.push(string_member(group, "Arn"));
    }

    // Each group carries a document the service DID attach, so the read-back
    // below tells a refusal that persisted nothing apart from a store that
    // never held anything.
    for arn in &arns {
        let written = ok(
            &router,
            "PutFirewallRuleGroupPolicy",
            json!({ "Arn": arn, "FirewallRuleGroupPolicy": sharing_document(arn) }),
        )
        .await;
        assert_eq!(
            written.get("ReturnValue"),
            Some(&Value::Bool(true)),
            "a document delegating the operations a share of {arn} grants must be attached, got {written}"
        );
    }

    let shared_case = "a document with no statement, refused for two groups";
    let document = json!({ "Version": "2012-10-17" }).to_string();
    let (tracing, root) = tracing_chain(100);
    let mut messages = Vec::new();
    for arn in &arns {
        messages.push(refusal_message(&router, shared_case, arn, &document, &tracing).await);
    }
    assert_eq!(
        messages[0], messages[1],
        "case {shared_case:?}: the wording must not depend on the group, got {messages:?}"
    );
    assert_eq!(
        messages[0],
        expected_message(&document, &root),
        "case {shared_case:?}: got {:?}",
        messages[0]
    );

    // The same group, two documents, one trace: only the echoed document
    // differs, so replacing it in one message yields the other.
    let case = "two documents refused for one group";
    let other_document = "not a policy at all".to_string();
    let other_message =
        refusal_message(&router, case, &arns[0], &other_document, &tracing).await;
    assert_eq!(
        other_message,
        messages[0].replace(&document, &other_document),
        "case {case:?}: got {other_message:?}"
    );

    // No refusal attached anything: each group still reports the document that
    // was attached before them.
    for arn in &arns {
        let read = ok(&router, "GetFirewallRuleGroupPolicy", json!({ "Arn": arn })).await;
        let stored = string_member(&read, "FirewallRuleGroupPolicy");
        assert_eq!(
            stored,
            attached_document(arn),
            "case {case:?}: a refused write must leave {arn}'s policy alone, got {stored:?}"
        );
    }
}
