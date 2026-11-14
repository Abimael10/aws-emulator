// SPDX-License-Identifier: CC0-1.0

//! Whether the caller may READ WHAT A DATA PROTECTION POLICY MASKED — the decision an event
//! read takes when, and only when, it is asked to lift the masking.
//!
//! `GetLogEventsRequest$unmask` and `FilterLogEventsRequest$unmask` state the rule in the
//! same words: "Specify true to display the log event fields with all sensitive data unmasked
//! and visible. The default is false. To use this operation with this parameter, you must be
//! signed into an account with the logs:Unmask permission."
//! (`smithy_query(query="doc",shape="GetLogEventsRequest",member="unmask")`). So revealing
//! masked data is an action of its own, performed under a permission of its own, and it is
//! asked for by one request member rather than by calling a different operation:
//!
//! * a read that does NOT ask to unmask performs only the operation's own action, and this
//!   module decides nothing about it — a caller allowed to read the group keeps reading the
//!   de-identified events;
//! * a read that DOES ask to unmask performs `logs:Unmask` as well, so a caller whose
//!   identity-based policies do not allow that action on the resource the read addresses is
//!   refused and is served no event at all, masked or not.
//!
//! The decision itself is this service's one identity decision
//! ([`super::identity_authorization::refusal`]) — the same principal resolution, the same
//! documents, the same evaluator and the same sentence as the action every operation is judged
//! under in front of its handler — asked here about the `logs:Unmask` action
//! ([`UNMASK_ACTION`]) instead of about the operation's own name. Nothing of IAM's wording,
//! and nothing of how a principal's policies are found, is restated here.
//!
//! ## The resource such a read is decided against
//!
//! The resource is the one the READ addresses, written in the ARN form the IAM Service
//! Authorization Reference gives this service's `log-group` resource type — the only resource
//! type its event actions take:
//!
//! * a read of ONE log stream's events is a stream-qualified read of the group, so the
//!   resource carries the `log-stream:` qualifier ([`stream_scoped_resource`]). The qualifier
//!   is EMPTY: a log stream is not separately authorized — a grant is written over the group
//!   and the `:*` that covers what is under it — so the decision is about the group the stream
//!   belongs to and no stream name enters it;
//! * a read scoped to the whole log group — one that selects across its streams rather than
//!   naming one — is decided against the bare log-group ARN ([`group_scoped_resource`]).
//!
//! Both are built from the group the request named and the Region its credential scope names,
//! before anything is looked up, so the decision does not depend on whether that group exists
//! and a refused caller learns nothing about what the account holds.

/// The IAM action a read performs by asking for masked data to be revealed.
///
/// A permission-only action — no operation is named `Unmask` — which is why it is stated here
/// rather than taken from an operation's name; the `logs:` prefix it is spelled under is this
/// service's own ([`super::identity_authorization::action_for`]).
pub const UNMASK_ACTION: &str = "Unmask";

/// `Some(message)` when `unmask` asks for masked data that IAM does not allow this caller to
/// see on `resource` — the sentence the refusal carries — and `None` when the read may be
/// served.
///
/// `None` for every read that did not ask to unmask, whatever the caller holds: such a read
/// performs no `logs:Unmask` and is left to the operation's own action.
///
/// Asked with NO store guard held, and before the operation reads anything.
#[must_use]
pub fn refusal<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    unmask: bool,
    resource: &str,
) -> Option<String> {
    if !unmask {
        return None;
    }
    super::identity_authorization::refusal(engine, ctx, UNMASK_ACTION, resource)
}

/// The resource a read of ONE log stream's events is decided against: the log group's ARN
/// carrying the empty `log-stream:` qualifier (see the module docs).
#[must_use]
pub fn stream_scoped_resource(region: &str, log_group_name: &str) -> String {
    super::log_stream_arn(region, log_group_name, "")
}

/// The resource a read scoped to a whole log group is decided against: that group's ARN (see
/// the module docs).
#[must_use]
pub fn group_scoped_resource(region: &str, log_group_name: &str) -> String {
    super::log_group_arn(region, log_group_name, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the two resource forms are the log group the read addresses — stream-qualified
    /// for a read of one stream, bare for a read of the group — and neither carries a stream
    /// name or the `:*` a policy's own wildcard adds.
    #[test]
    fn a_read_is_decided_against_the_log_group_it_addresses() {
        let account = ::engine::emulator_account_id();
        // The rows differ along what the rule generalises over: the Region the credential
        // scope names, and the shape of the group name (a path-like name, a flat one, one
        // carrying the punctuation `LogGroupName` admits).
        let rows: &[(&str, &str, &str)] = &[
            ("a path-like group name", "eu-west-2", "/aws/lambda/handler"),
            ("a flat group name", "us-west-2", "orders"),
            ("a name carrying punctuation", "ap-south-1", "team.one_logs-9#a"),
        ];
        for (case, region, group) in rows {
            let stream_scoped = stream_scoped_resource(region, group);
            let expected =
                format!("arn:aws:logs:{region}:{account}:log-group:{group}:log-stream:");
            assert_eq!(
                stream_scoped, expected,
                "case {case:?}: expected the stream-qualified resource {expected:?}, got \
                 {stream_scoped:?}"
            );
            let group_scoped = group_scoped_resource(region, group);
            let expected = format!("arn:aws:logs:{region}:{account}:log-group:{group}");
            assert_eq!(
                group_scoped, expected,
                "case {case:?}: expected the group's own resource {expected:?}, got \
                 {group_scoped:?}"
            );
        }
    }

    /// RULE: the action asked about is `logs:Unmask`, so a statement granting the read's own
    /// action does not grant it and one granting this action does.
    #[test]
    fn revealing_masked_data_is_its_own_action() {
        let action = super::super::identity_authorization::action_for(UNMASK_ACTION);
        assert_eq!(
            action, "logs:Unmask",
            "case \"the action a read asking to unmask performs\": got {action:?}"
        );
        for other in ["GetLogEvents", "FilterLogEvents"] {
            let operation_action = super::super::identity_authorization::action_for(other);
            assert_ne!(
                action, operation_action,
                "case {other:?}: revealing masked data is not the read's own action"
            );
        }
    }
}
