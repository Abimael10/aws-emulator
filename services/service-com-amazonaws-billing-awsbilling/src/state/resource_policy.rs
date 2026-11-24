// SPDX-License-Identifier: CC0-1.0

//! The resource-based policy document a SHARED billing view carries, as
//! `GetResourcePolicy` reports it.
//!
//! This service's model declares no operation that attaches a policy to a
//! billing view — there is no `PutResourcePolicy` — because a custom view comes
//! to carry one when the account SHARES it, which is AWS Resource Access
//! Manager's action. So the document is not a field of a stored view: it is
//! derived, at the instant the read is answered, from the shares that resource
//! sharing holds over the view's ARN, the same way the organization gate reads
//! the organization out of AWS Organizations' store
//! ([`super::organization_management_account_id`]). Nothing is copied into this
//! service's state, so a view's policy cannot go stale against the shares that
//! produce it, and a share torn down stops the view carrying one.
//!
//! A resource carries ONE policy document, while a resource may be shared by
//! several shares, so the document this reports is the one the statements of
//! those shares' documents make up — see [`attached_resource_policy`].

use std::time::SystemTime;

use service_com_amazonaws_ram_amazonresourcesharing::state as resource_sharing;

/// The policy-language version element of a policy document, and the element
/// carrying its statements: the two elements a resource policy is made of.
const POLICY_VERSION_ELEMENT: &str = "Version";
const POLICY_STATEMENT_ELEMENT: &str = "Statement";

/// The resource-based policy document attached to the billing view `view_arn`,
/// or `None` while nothing has attached one.
///
/// A billing view carries a policy exactly while a live resource share of the
/// account shares it with a principal that needs one to reach it: resource
/// sharing instantiates the managed permission governing each share on the
/// resources that share shares, and those instantiated statements ARE the
/// document attached to the resource. A view no share shares, one whose shares
/// instantiate no statement, and a view read on an engine holding no resource
/// sharing store at all, all carry none — the case the read answers with its
/// modelled `ResourceNotFoundException`.
///
/// `engine` is the handler's own `&*self.engine`, so this reads the one store
/// the emulator's resource sharing service writes. Every live share there is a
/// share this account owns — the only account the emulator serves, and the
/// account every ARN this read is asked about belongs to (the caller-account
/// boundary is decided before the read) — so the shares that hold the view's
/// ARN are the owner's own.
pub fn attached_resource_policy<Eng: Send + Sync + 'static>(
    engine: &Eng,
    view_arn: &str,
) -> Option<String> {
    let shares = resource_sharing::service_state(engine)?;
    let documents = {
        let mut shares = shares.lock();
        // What the clock already owed that store is materialised before it is
        // read, exactly as its own reads do it: an association that has fallen
        // due has stopped sharing the view whether or not anybody asked, and
        // that is what decides whether the view still carries a policy.
        shares.settle_due_associations(super::epoch_millis(SystemTime::now()));
        // Every principal the view is shared with, not one the caller named:
        // the document attached to the resource is the whole policy, which is
        // what this service's single `policy` member reports.
        shares.resource_policies(view_arn, None)?
    };
    merged_policy(&documents)
}

/// The ONE document a resource carries, built from the documents its shares
/// contribute: their statements, in the order the shares are reported, under
/// the policy language version those statements are written for.
///
/// `None` when the shares contribute no statement at all, which is a resource
/// that is shared but carries no grant anything could read.
fn merged_policy(documents: &[String]) -> Option<String> {
    let mut statements: Vec<::serde_json::Value> = Vec::new();
    for document in documents {
        // A document that cannot be read as a policy contributes no statement,
        // rather than erasing the statements of the shares that can: a resource
        // carries whatever is attached to it, not nothing.
        let Ok(parsed) = ::serde_json::from_str::<::serde_json::Value>(document) else {
            continue;
        };
        // A policy's statements are written as a list, or as the one statement
        // itself where there is only one.
        match parsed.get(POLICY_STATEMENT_ELEMENT) {
            Some(::serde_json::Value::Array(entries)) => statements.extend(entries.iter().cloned()),
            Some(statement) => statements.push(statement.clone()),
            None => {}
        }
    }
    if statements.is_empty() {
        return None;
    }
    let mut document = ::serde_json::Map::new();
    document.insert(
        POLICY_VERSION_ELEMENT.to_string(),
        ::serde_json::Value::from(resource_sharing::POLICY_LANGUAGE_VERSION),
    );
    document.insert(
        POLICY_STATEMENT_ELEMENT.to_string(),
        ::serde_json::Value::Array(statements),
    );
    Some(::serde_json::Value::Object(document).to_string())
}
