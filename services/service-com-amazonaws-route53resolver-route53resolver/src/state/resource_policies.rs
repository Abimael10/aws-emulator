// SPDX-License-Identifier: CC0-1.0

//! The resource-policy plane of this service: what a sharing policy has to say for the
//! service to attach it, and what a read of an attached one reports.
//!
//! Three operations attach one — `PutFirewallRuleGroupPolicy`, `PutResolverRulePolicy` and
//! `PutResolverQueryLogConfigPolicy` — and each words its own refusal, so what lives here is
//! what the document itself means to THIS service: whether it delegates the operations the
//! share of that kind of resource grants ([`attachable_sharing_policy`]), and the bodies the
//! two sharing writes refuse a document with.
//!
//! Whether the text is an IAM resource-based policy at all is not this service's reading:
//! that is the tree's one grammar ([`::service_common::iam_policy::resource_policy_defect`]),
//! and so is the FORM an attached document is reported in
//! ([`::service_common::iam_policy_rendering`]) — the service's own rendering of the policy,
//! not the bytes the caller spelled.

use super::*;

/// The `ValidationException` a DNS Firewall rule-group policy write answers for
/// an IAM policy document the service will not attach.
///
/// `PutFirewallRuleGroupPolicy` models no `InvalidPolicyDocument`
/// (`smithy_query(query="operation",operation="PutFirewallRuleGroupPolicy")`
/// lists `ValidationException` instead), so the faults the Resolver rule policy
/// reports through that shape are reported here through the exception this
/// operation does model.
///
/// The body carries the service's code for an invalid IAM policy
/// ([`RSLVR_CODE_INVALID_IAM_POLICY`]) and echoes the document it was handed, so
/// the caller sees exactly which document was refused, then the remediation
/// sentence that states what a document the service WOULD attach has to do
/// ([`FIREWALL_RULE_GROUP_POLICY_REMEDIATION`]), and finally the trace clause of
/// the request ([`traced_message`]) — the only part of the body that is not
/// fixed text beside the document itself. WHY the document was refused is not
/// part of it, so the one body covers a document outside the policy grammar and
/// one that does not delegate the rule-group operations a share grants.
pub fn invalid_iam_policy(policy: &str, trace_root: &str) -> ValidationException {
    ValidationException {
        message: Some(traced_message(
            rslvr_message_sentences(
                RSLVR_CODE_INVALID_IAM_POLICY,
                &[
                    &format!("The specified IAM policy isn't valid: \"{policy}\""),
                    FIREWALL_RULE_GROUP_POLICY_REMEDIATION,
                ],
            ),
            trace_root,
        )),
    }
}

/// The remediation sentence a DNS Firewall rule-group policy refusal closes
/// with: what a document the service would attach has to do.
///
/// It is the only part of the refusal that says what to send instead, and it is
/// fixed text — it echoes nothing from the request or the store. Carried as a
/// constant, WITHOUT its final period (as every sentence handed to
/// [`rslvr_message_sentences`] is), so the refusals of both classes of document
/// the rule-group policy write rejects cannot word it differently. The model
/// states no message text for `ValidationException`
/// (`smithy_query(query="shape",shape="ValidationException")` carries `Message`
/// alone), so this wording is the recorded behaviour of the operation.
pub const FIREWALL_RULE_GROUP_POLICY_REMEDIATION: &str =
    "Provide a valid policy that allows a DNS Firewall rule group to be shared with another AWS \
     account";

/// The Route 53 Resolver service error code for a resource-policy document the
/// service will not attach.
///
/// One code for the fault, whichever sharing plane reports it: the Resolver rule
/// policy write states it alone ([`invalid_resolver_rule_policy`]) and the
/// query-logging write states it behind that slice's own sentence
/// ([`RSLVR_CODE_QUERY_LOG_CONFIG_POLICY_INVALID`], which is this code), so the
/// digits are written once here.
pub const RSLVR_CODE_INVALID_POLICY: &str = "RSLVR-00312";

/// The `InvalidPolicyDocument` the Resolver rule policy write answers for a
/// document it will not attach — one outside the policy grammar, and one that
/// does not delegate the rule operations a share grants.
///
/// The model documents the shape as "the specified Resolver rule policy is
/// invalid"; the recorded body states the service's code for that fault and the
/// two words that name it, and nothing else: it echoes no part of the document
/// it refused, names no resource, and closes with no trace clause. Rendered here
/// rather than through [`rslvr_message`] because that formatter terminates its
/// sentence with a period and this body carries none — the same reason
/// [`query_log_access_denied_message`] is written out.
pub fn invalid_resolver_rule_policy() -> InvalidPolicyDocument {
    InvalidPolicyDocument {
        message: Some(format!("[{RSLVR_CODE_INVALID_POLICY}] Invalid Policy")),
    }
}

/// The policy document a resource with no stored resource policy reports: the
/// empty JSON object, a document that states no statement and so shares the
/// resource with nobody. Inside the modelled length of both members that report
/// one — `ResolverRulePolicy` and `FirewallRuleGroupPolicy`, each 0..30000
/// (`smithy_query(query="shape",shape="ResolverRulePolicy,FirewallRuleGroupPolicy")`).
pub const EMPTY_POLICY_DOCUMENT: &str = "{}";

/// The operations a share of one DNS Firewall rule group delegates, as the
/// `Action` section of its sharing policy names them.
///
/// A rule group is shared so another account can READ it — and AWS's sharing
/// policy for a rule group names exactly these two operations of this service's
/// model, `GetFirewallRuleGroup` and `ListFirewallRuleGroups`: a share of a rule
/// group grants no operation that changes it, and none that associates it, which
/// the account the group is shared WITH performs on its own VPC.
pub const FIREWALL_RULE_GROUP_SHARED_ACTIONS: &[&str] = &[
    "route53resolver:GetFirewallRuleGroup",
    "route53resolver:ListFirewallRuleGroups",
];

/// The operations a share of one Resolver rule delegates, as the `Action`
/// section of its sharing policy names them.
///
/// The model lists them for the member that carries the document:
/// `smithy_query(query="doc",shape="PutResolverRulePolicyRequest",member="ResolverRulePolicy")`
/// — "you can specify the following operations in the `Action` section of the
/// statement" — so a rule's share delegates the read of the rule, both sides of
/// associating it with a VPC, and the two listings.
pub const RESOLVER_RULE_SHARED_ACTIONS: &[&str] = &[
    "route53resolver:GetResolverRule",
    "route53resolver:AssociateResolverRule",
    "route53resolver:DisassociateResolverRule",
    "route53resolver:ListResolverRules",
    "route53resolver:ListResolverRuleAssociations",
];

/// Whether `policy` is a document a sharing write attaches to a resource whose
/// share delegates `delegated`: an IAM resource-based policy document that names
/// those operations in its action elements and no other.
///
/// Two readings, in order. Whether the text is a policy document at all is the
/// tree's one grammar ([`::service_common::iam_policy::resource_policy_defect`]):
/// a resource-based document is a JSON object declaring at least one statement,
/// each with an effect, an action and the principal the grant reaches. What the
/// document then has to SAY is this service's own: a resource policy on a
/// Resolver resource is read as the share it asks for, so the service attaches it
/// only when it asks for the share the service can make. A document that
/// delegates FEWER operations than the share grants, or that names an operation
/// the share does not cover, asks for a share the service cannot make and is
/// refused. The comparison is on the SET of the actions every statement names
/// ([`::service_common::iam_policy::action_values`], which reads `Action` and its
/// complement in both the string and the list form), so the order they are
/// written in and a repeated action change nothing.
///
/// Whom the document names as principal is NO part of this: the principal is the
/// account the resource is being shared with, and the service attaches whichever
/// account that is.
pub fn attachable_sharing_policy(policy: &str, delegated: &[&str]) -> bool {
    if ::service_common::iam_policy::resource_policy_defect(policy).is_some() {
        return false;
    }
    let named = ::service_common::iam_policy::action_values(policy);
    named.iter().map(String::as_str).collect::<HashSet<&str>>()
        == delegated.iter().copied().collect::<HashSet<&str>>()
}

/// The document the service holds for `policy`, which is what a read of the
/// resource's policy reports.
///
/// A resource policy is not reported as the bytes the caller spelled: what a read
/// answers is the service's own rendering of the policy the document expresses,
/// which is the tree's one rendering of an IAM policy document
/// ([`::service_common::iam_policy_rendering::rendered`]) walked with the one
/// spelling these reads add — an account named as a principal by its id alone is
/// written as that account's ROOT ARN
/// ([`::service_common::iam_policy_principals::account_root_principal`]), the
/// other spelling of the very same principal. Everything else is the caller's own
/// data and survives untouched, member order included.
///
/// A fixed point: rendering a rendered document returns it unchanged, so a caller
/// that puts back the document it read stores the same bytes again. A document
/// that is no policy never reaches here — each write refuses it
/// ([`attachable_sharing_policy`], and the query-logging write's own reading of
/// the same grammar) before the store is touched.
pub fn stored_policy_document(policy: &str) -> String {
    ::service_common::iam_policy_rendering::rendered_rewriting(
        policy,
        &::service_common::iam_policy_rendering::ElementValues {
            element: ACCOUNT_PRINCIPAL,
            written_as: &::service_common::iam_policy_principals::account_root_principal,
        },
    )
}

/// The key a policy names an ACCOUNT principal under — the `AWS` type of a
/// `Principal`/`NotPrincipal` map, as opposed to the `Service`, `Federated` and
/// `CanonicalUser` types, whose values name no account and are reported as they
/// were submitted.
const ACCOUNT_PRINCIPAL: &str = "AWS";
