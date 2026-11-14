// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the trace clause a DNS Firewall refusal that names a
//! resource the caller cannot address closes its coded sentence with.
//!
//! RULE: the `Message` of such a refusal is the service's coded sentence for
//! that fault followed by ` Trace Id: "<Root>"`, where `<Root>` is the trace the
//! REQUEST named in its `X-Amzn-Trace-Id` header and one the service minted for
//! the request when the caller named none. So the clause is always there, it is
//! read from the request and never from anything the caller asked ABOUT, and a
//! chain the caller started is reported under that chain's own id.
//!
//! The table spans what the rule generalises over: the DNS Firewall resource the
//! request could not address (a rule group, a domain list, a rule inside a
//! group), how the request addressed it (by its own id and by the ARN the policy
//! operations take), the operation that refused it (reads, writes, deletes and
//! the entries of the batch operations) and the tracing header the request
//! carried.
//!
//! Every case goes over the wire the service actually speaks (awsJson1_1,
//! `POST /` with `X-Amz-Target: Route53Resolver.<OperationName>`), through the
//! same router the emulator mounts. The transport helpers are the shared ones in
//! [`super::harness`].

use std::collections::BTreeSet;

use serde_json::{json, Value};

use service_com_amazonaws_route53resolver_route53resolver::state::{
    FIREWALL_RULE_DOMAIN_LIST_MEMBER, RESOURCE_TYPE_FIREWALL_DOMAIN_LIST, RESOURCE_TYPE_FIREWALL_RULE,
    RESOURCE_TYPE_FIREWALL_RULE_GROUP, RSLVR_CODE_FIREWALL_RULE_NOT_FOUND,
    RSLVR_CODE_RESOURCE_NOT_FOUND,
};

use super::harness::{
    call_tracing, emulator, error_type, ok, string_member, traced_message, ACCOUNT, REGION,
};

/// A `FirewallRuleGroupId` no create ever minted, well formed for the modelled
/// `ResourceId` (length 1..64).
const UNKNOWN_GROUP: &str = "rslvr-frg-0123456789abcdef0";

/// A `FirewallDomainListId` no create ever minted.
const UNKNOWN_DOMAIN_LIST: &str = "rslvr-fdl-0123456789abcdef0";

/// A VPC of the account the association row names, so its refusal is about the
/// rule group it could not address and never about the VPC.
const HELD_VPC: &str = "vpc-0123456789abcdef0";

/// An S3 address, so an import row reaches the domain-list lookup instead of
/// being refused for the file it names.
const DOMAIN_FILE_URL: &str = "s3://sim2real-domains/blocklist.txt";

/// The sentence the service answers for a resource the caller named but cannot
/// address, naming it by the bare id.
fn cannot_find(resource_id: &str) -> String {
    format!("[{RSLVR_CODE_RESOURCE_NOT_FOUND}] Can’t find the resource with ID \"{resource_id}\".")
}

/// The sentence the service answers when no rule of the group a request named
/// matches the source it named the rule by.
fn cannot_locate_rule(group_id: &str, match_source_id: &str) -> String {
    format!(
        "[{RSLVR_CODE_FIREWALL_RULE_NOT_FOUND}] Can’t locate the rule for the Firewall rule \
         group ID: \"{group_id}\" and {FIREWALL_RULE_DOMAIN_LIST_MEMBER} \"{match_source_id}\"."
    )
}

/// The ARN of this service naming the rule group `id`, which is how the
/// rule-group policy operations address one.
fn rule_group_arn(id: &str) -> String {
    format!("arn:aws:route53resolver:{REGION}:{ACCOUNT}:firewall-rule-group/{id}")
}

/// One tracing header a request may carry, and the trace the refusal it gets
/// back has to be reported under.
struct Tracing {
    case: &'static str,
    /// The `X-Amzn-Trace-Id` the request carries, or `None` for a caller that
    /// sends none.
    header: Option<&'static str>,
    /// The `Root` the clause must carry, or `None` where the caller named no
    /// trace and the service mints one for the request.
    expected_root: Option<&'static str>,
}

/// The rows differ along the tracing header the REQUEST carries. A header naming
/// a trace in X-Ray's own form has its `Root` reported back whatever else the
/// header carries beside it; a caller that names no trace, or names something
/// that is not a trace id at all, has one minted for the request instead — the
/// one reading of that form the services of this emulator share
/// (`service_common::xray_trace`).
const TRACINGS: &[Tracing] = &[
    Tracing {
        case: "no tracing header at all",
        header: None,
        expected_root: None,
    },
    Tracing {
        case: "a root and the sampling decision",
        header: Some("Root=1-6af7c539-609f12c15023464a806a7558;Sampled=0"),
        expected_root: Some("1-6af7c539-609f12c15023464a806a7558"),
    },
    Tracing {
        case: "a root alone",
        header: Some("Root=1-6b200001-3c4d5e6f708192a3b4c5d6e7"),
        expected_root: Some("1-6b200001-3c4d5e6f708192a3b4c5d6e7"),
    },
    Tracing {
        case: "a root, a parent span and a sampled request",
        header: Some("Root=1-6b200002-0624219a04ba4315978e3230;Parent=53995c3f42cd8ad8;Sampled=1"),
        expected_root: Some("1-6b200002-0624219a04ba4315978e3230"),
    },
    Tracing {
        case: "a header naming no root",
        header: Some("Sampled=0"),
        expected_root: None,
    },
    Tracing {
        case: "a root that is not a trace id",
        header: Some("Root=not-a-trace-id;Sampled=0"),
        expected_root: None,
    },
];

/// The DNS Firewall resources this suite's rows address successfully, so a
/// refusal is about the one resource the row could not address.
struct Held {
    group: String,
    /// The `Arn` the service minted for that group, which is how the policy
    /// operations address it.
    group_arn: String,
    domain_list: String,
}

/// One refused DNS Firewall call: the operation, the request it carries, and the
/// coded sentence its refusal states before the trace clause.
struct Refusal {
    case: &'static str,
    operation: &'static str,
    body: fn(&Held) -> Value,
    sentence: fn(&Held) -> String,
    /// The modelled resource name the refusal reports beside the message.
    resource_type: &'static str,
}

/// Every DNS Firewall path that refuses a resource it cannot address: the
/// operations that resolve a `FirewallRuleGroupId`, the two that resolve a rule
/// group by its `Arn`, the operations that resolve a `FirewallDomainListId`, and
/// the writes that address one stored rule inside a group.
const REFUSALS: &[Refusal] = &[
    Refusal {
        case: "a read of a rule group the store holds nothing under",
        operation: "GetFirewallRuleGroup",
        body: |_| json!({ "FirewallRuleGroupId": UNKNOWN_GROUP }),
        sentence: |_| cannot_find(UNKNOWN_GROUP),
        resource_type: RESOURCE_TYPE_FIREWALL_RULE_GROUP,
    },
    Refusal {
        case: "a delete of a rule group the store holds nothing under",
        operation: "DeleteFirewallRuleGroup",
        body: |_| json!({ "FirewallRuleGroupId": UNKNOWN_GROUP }),
        sentence: |_| cannot_find(UNKNOWN_GROUP),
        resource_type: RESOURCE_TYPE_FIREWALL_RULE_GROUP,
    },
    Refusal {
        case: "a listing of the rules of a rule group the store holds nothing under",
        operation: "ListFirewallRules",
        body: |_| json!({ "FirewallRuleGroupId": UNKNOWN_GROUP }),
        sentence: |_| cannot_find(UNKNOWN_GROUP),
        resource_type: RESOURCE_TYPE_FIREWALL_RULE_GROUP,
    },
    Refusal {
        case: "a policy read of a rule group addressed by an ARN naming no group",
        operation: "GetFirewallRuleGroupPolicy",
        body: |_| json!({ "Arn": rule_group_arn(UNKNOWN_GROUP) }),
        sentence: |_| cannot_find(UNKNOWN_GROUP),
        resource_type: RESOURCE_TYPE_FIREWALL_RULE_GROUP,
    },
    Refusal {
        case: "a policy write of a rule group addressed by an ARN naming no group",
        operation: "PutFirewallRuleGroupPolicy",
        body: |_| {
            let arn = rule_group_arn(UNKNOWN_GROUP);
            json!({
                "Arn": arn.clone(),
                "FirewallRuleGroupPolicy": json!({
                    "Version": "2012-10-17",
                    "Statement": [{
                        "Effect": "Allow",
                        "Principal": { "AWS": "123456789012" },
                        "Action": "route53resolver:GetFirewallRuleGroup",
                        "Resource": arn,
                    }],
                })
                .to_string(),
            })
        },
        sentence: |_| cannot_find(UNKNOWN_GROUP),
        resource_type: RESOURCE_TYPE_FIREWALL_RULE_GROUP,
    },
    Refusal {
        case: "an association naming a rule group the store holds nothing under",
        operation: "AssociateFirewallRuleGroup",
        body: |_| {
            json!({
                "CreatorRequestId": "traced-associate-unknown-group",
                "FirewallRuleGroupId": UNKNOWN_GROUP,
                "VpcId": HELD_VPC,
                "Priority": 733,
                "Name": "traced-associate-unknown-group",
            })
        },
        sentence: |_| cannot_find(UNKNOWN_GROUP),
        resource_type: RESOURCE_TYPE_FIREWALL_RULE_GROUP,
    },
    Refusal {
        case: "a rule create naming a rule group the store holds nothing under",
        operation: "CreateFirewallRule",
        body: |held| {
            json!({
                "CreatorRequestId": "traced-create-unknown-group",
                "FirewallRuleGroupId": UNKNOWN_GROUP,
                "FirewallDomainListId": held.domain_list.clone(),
                "Priority": 110,
                "Action": "ALLOW",
                "Name": "traced-create-unknown-group",
            })
        },
        sentence: |_| cannot_find(UNKNOWN_GROUP),
        resource_type: RESOURCE_TYPE_FIREWALL_RULE_GROUP,
    },
    Refusal {
        case: "a read of a domain list the store holds nothing under",
        operation: "GetFirewallDomainList",
        body: |_| json!({ "FirewallDomainListId": UNKNOWN_DOMAIN_LIST }),
        sentence: |_| cannot_find(UNKNOWN_DOMAIN_LIST),
        resource_type: RESOURCE_TYPE_FIREWALL_DOMAIN_LIST,
    },
    Refusal {
        case: "a delete of a domain list the store holds nothing under",
        operation: "DeleteFirewallDomainList",
        body: |_| json!({ "FirewallDomainListId": UNKNOWN_DOMAIN_LIST }),
        sentence: |_| cannot_find(UNKNOWN_DOMAIN_LIST),
        resource_type: RESOURCE_TYPE_FIREWALL_DOMAIN_LIST,
    },
    Refusal {
        case: "a domain write to a domain list the store holds nothing under",
        operation: "UpdateFirewallDomains",
        body: |_| {
            json!({
                "FirewallDomainListId": UNKNOWN_DOMAIN_LIST,
                "Operation": "ADD",
                "Domains": ["traced.example."],
            })
        },
        sentence: |_| cannot_find(UNKNOWN_DOMAIN_LIST),
        resource_type: RESOURCE_TYPE_FIREWALL_DOMAIN_LIST,
    },
    Refusal {
        case: "a domain listing of a domain list the store holds nothing under",
        operation: "ListFirewallDomains",
        body: |_| json!({ "FirewallDomainListId": UNKNOWN_DOMAIN_LIST }),
        sentence: |_| cannot_find(UNKNOWN_DOMAIN_LIST),
        resource_type: RESOURCE_TYPE_FIREWALL_DOMAIN_LIST,
    },
    Refusal {
        case: "an import into a domain list the store holds nothing under",
        operation: "ImportFirewallDomains",
        body: |_| {
            json!({
                "FirewallDomainListId": UNKNOWN_DOMAIN_LIST,
                "Operation": "REPLACE",
                "DomainFileUrl": DOMAIN_FILE_URL,
            })
        },
        sentence: |_| cannot_find(UNKNOWN_DOMAIN_LIST),
        resource_type: RESOURCE_TYPE_FIREWALL_DOMAIN_LIST,
    },
    Refusal {
        case: "a rule create naming a domain list the store holds nothing under",
        operation: "CreateFirewallRule",
        body: |held| {
            json!({
                "CreatorRequestId": "traced-create-unknown-list",
                "FirewallRuleGroupId": held.group.clone(),
                "FirewallDomainListId": UNKNOWN_DOMAIN_LIST,
                "Priority": 120,
                "Action": "ALLOW",
                "Name": "traced-create-unknown-list",
            })
        },
        sentence: |_| cannot_find(UNKNOWN_DOMAIN_LIST),
        resource_type: RESOURCE_TYPE_FIREWALL_DOMAIN_LIST,
    },
    Refusal {
        case: "a rule delete naming a rule the group holds none of",
        operation: "DeleteFirewallRule",
        body: |held| {
            json!({
                "FirewallRuleGroupId": held.group.clone(),
                "FirewallDomainListId": UNKNOWN_DOMAIN_LIST,
            })
        },
        sentence: |held| cannot_locate_rule(&held.group, UNKNOWN_DOMAIN_LIST),
        resource_type: RESOURCE_TYPE_FIREWALL_RULE,
    },
    Refusal {
        case: "a rule update naming a rule the group holds none of",
        operation: "UpdateFirewallRule",
        body: |held| {
            json!({
                "FirewallRuleGroupId": held.group.clone(),
                "FirewallDomainListId": UNKNOWN_DOMAIN_LIST,
                "Priority": 500,
            })
        },
        sentence: |held| cannot_locate_rule(&held.group, UNKNOWN_DOMAIN_LIST),
        resource_type: RESOURCE_TYPE_FIREWALL_RULE,
    },
];

/// Create one rule group and one domain list, so every row's refusal is about
/// the resource it could not address and not about an empty store.
async fn held(router: &super::harness::Emulator) -> Held {
    let group = ok(
        router,
        "CreateFirewallRuleGroup",
        json!({ "CreatorRequestId": "traced-held-group", "Name": "traced-held-group" }),
    )
    .await;
    let domain_list = ok(
        router,
        "CreateFirewallDomainList",
        json!({ "CreatorRequestId": "traced-held-list", "Name": "traced-held-list" }),
    )
    .await;
    Held {
        group: string_member(
            group
                .get("FirewallRuleGroup")
                .expect("the create answers a FirewallRuleGroup"),
            "Id",
        ),
        group_arn: string_member(
            group
                .get("FirewallRuleGroup")
                .expect("the create answers a FirewallRuleGroup"),
            "Arn",
        ),
        domain_list: string_member(
            domain_list
                .get("FirewallDomainList")
                .expect("the create answers a FirewallDomainList"),
            "Id",
        ),
    }
}

/// Every DNS Firewall refusal of a resource the caller cannot address states its
/// coded sentence and closes it with the trace clause of the request: the `Root`
/// the caller named where it named one, and a trace minted for the request where
/// it did not.
#[tokio::test]
async fn every_firewall_not_found_refusal_closes_with_the_trace_clause_of_the_request() {
    let router = emulator();
    router.hold_vpcs([HELD_VPC]);
    let held = held(&router).await;

    // Every trace the service minted for a request that named none, so two
    // requests are never reported under one minted chain.
    let mut minted: Vec<String> = Vec::new();

    for refusal in REFUSALS {
        for tracing in TRACINGS {
            let case = format!("{} / {}", refusal.case, tracing.case);
            let (status, body) = call_tracing(
                &router,
                refusal.operation,
                (refusal.body)(&held),
                tracing.header,
            )
            .await;
            assert_eq!(
                status, 400,
                "case {case:?}: {} must answer HTTP 400, got {status}: {body}",
                refusal.operation
            );
            assert!(
                error_type(&body).contains("ResourceNotFoundException"),
                "case {case:?}: the refusal must name ResourceNotFoundException, got {body}"
            );
            assert_eq!(
                string_member(&body, "ResourceType"),
                refusal.resource_type,
                "case {case:?}: the refusal must name the resource that is missing, got {body}"
            );

            let message = string_member(&body, "Message");
            let (stated, trace) = traced_message(&message).unwrap_or_else(|| {
                panic!(
                    "case {case:?}: the refusal must close with its trace clause, carrying a \
                     trace id in quotes, got {message:?}"
                )
            });
            assert_eq!(
                stated,
                (refusal.sentence)(&held),
                "case {case:?}: the refusal must state its coded sentence before the clause, got \
                 {message:?}"
            );
            match tracing.expected_root {
                // The caller named a chain, so the refusal is reported under
                // that chain and not under one of the service's own.
                Some(root) => assert_eq!(
                    trace, root,
                    "case {case:?}: the clause must carry the Root the request named, got \
                     {message:?}"
                ),
                // The caller named none, so the service minted one for THIS
                // request: nothing the request carried is echoed into it.
                None => {
                    assert!(
                        !message.contains("not-a-trace-id"),
                        "case {case:?}: a Root that is not a trace id must be replaced, not \
                         echoed, got {message:?}"
                    );
                    minted.push(trace.to_string());
                }
            }
        }
    }

    let distinct = minted.iter().collect::<BTreeSet<&String>>().len();
    assert_eq!(
        distinct,
        minted.len(),
        "a trace the service mints is this request's own, so no two minted clauses may repeat, \
         got {minted:?}"
    );
}

/// One entry of a batch operation reports the refusal the single operation
/// answers for that very entry, so the message it carries in the response body
/// closes with the trace clause of the request too.
#[tokio::test]
async fn a_batch_entry_reports_the_refusal_with_the_trace_clause_of_the_request() {
    let router = emulator();
    let held = held(&router).await;
    let header = "Root=1-6b200003-9a8b7c6d5e4f3a2b1c0d9e8f;Sampled=0";
    let root = "1-6b200003-9a8b7c6d5e4f3a2b1c0d9e8f";

    for (case, operation, body, member, sentence) in [
        (
            "a batch create naming a rule group the store holds nothing under",
            "BatchCreateFirewallRule",
            json!({
                "CreateFirewallRuleEntries": [{
                    "CreatorRequestId": "traced-batch-create",
                    "FirewallRuleGroupId": UNKNOWN_GROUP,
                    "FirewallDomainListId": held.domain_list.clone(),
                    "Priority": 130,
                    "Action": "ALLOW",
                    "Name": "traced-batch-create",
                }],
            }),
            "CreateErrors",
            cannot_find(UNKNOWN_GROUP),
        ),
        (
            "a batch update naming a rule the group holds none of",
            "BatchUpdateFirewallRule",
            json!({
                "UpdateFirewallRuleEntries": [{
                    "FirewallRuleGroupId": held.group.clone(),
                    "FirewallDomainListId": UNKNOWN_DOMAIN_LIST,
                    "Priority": 140,
                }],
            }),
            "UpdateErrors",
            cannot_locate_rule(&held.group, UNKNOWN_DOMAIN_LIST),
        ),
        (
            "a batch delete naming a rule the group holds none of",
            "BatchDeleteFirewallRule",
            json!({
                "DeleteFirewallRuleEntries": [{
                    "FirewallRuleGroupId": held.group.clone(),
                    "FirewallDomainListId": UNKNOWN_DOMAIN_LIST,
                }],
            }),
            "DeleteErrors",
            cannot_locate_rule(&held.group, UNKNOWN_DOMAIN_LIST),
        ),
    ] {
        let (status, answered) = call_tracing(&router, operation, body, Some(header)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: {operation} must answer HTTP 200 and report the entry's error, got \
             {status}: {answered}"
        );
        let errors = answered
            .get(member)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_else(|| {
                panic!("case {case:?}: the response must carry {member} as a list, got {answered}")
            });
        assert_eq!(
            errors.len(),
            1,
            "case {case:?}: the batch must report exactly one error for its one entry, got \
             {answered}"
        );
        assert_eq!(
            string_member(&errors[0], "Code"),
            "ResourceNotFoundException",
            "case {case:?}: the entry's error must name the exception the single call answers, \
             got {answered}"
        );
        let message = string_member(&errors[0], "Message");
        let (stated, trace) = traced_message(&message).unwrap_or_else(|| {
            panic!("case {case:?}: the entry's error must close with its trace clause, got {message:?}")
        });
        assert_eq!(
            stated, sentence,
            "case {case:?}: the entry's error must state the sentence the single call answers for \
             it, got {message:?}"
        );
        assert_eq!(
            trace, root,
            "case {case:?}: the clause must carry the Root the request named, got {message:?}"
        );
    }
}

/// A SERVED DNS Firewall call carries no refusal and so no clause: the trace
/// clause belongs to the refusal and is not appended to anything the service
/// answers, so the tracing header changes nothing about a call that succeeds.
#[tokio::test]
async fn a_served_firewall_call_carries_no_trace_clause() {
    let router = emulator();
    let held = held(&router).await;

    for (case, header) in [
        (
            "a caller naming a chain",
            Some("Root=1-6b200004-333333333333333333333333;Sampled=0"),
        ),
        ("a caller naming none", None),
    ] {
        for (operation, body) in [
            (
                "GetFirewallRuleGroup",
                json!({ "FirewallRuleGroupId": held.group.clone() }),
            ),
            (
                "GetFirewallRuleGroupPolicy",
                json!({ "Arn": held.group_arn.clone() }),
            ),
            (
                "GetFirewallDomainList",
                json!({ "FirewallDomainListId": held.domain_list.clone() }),
            ),
        ] {
            let (status, served) = call_tracing(&router, operation, body, header).await;
            assert_eq!(
                status, 200,
                "case {case:?}: {operation} must be served, got {status}: {served}"
            );
            assert!(
                !serde_json::to_string(&served)
                    .expect("the answer serializes")
                    .contains("Trace Id"),
                "case {case:?}: {operation} must carry no trace clause, got {served}"
            );
        }
    }
}
