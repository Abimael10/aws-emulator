// SPDX-License-Identifier: CC0-1.0

//! The subscription an `Unsubscribe` BY SOMEONE OTHER THAN ITS OWNER deactivated, held so the
//! token SNS sent its endpoint can restore it.
//!
//! "If the `Unsubscribe` call does not require authentication and the requester is not the
//! subscription owner, a final cancellation message is delivered to the endpoint, so that the
//! endpoint owner can easily resubscribe to the topic if the `Unsubscribe` request was
//! unintended" (`Unsubscribe`). The cancellation message carries a token, and
//! `ConfirmSubscription` "verifies an endpoint owner's intent to receive messages by
//! validating the token sent to the endpoint by an earlier `Subscribe` action. If the token is
//! valid, the action creates a new subscription" — so the ONE thing the endpoint owner needs
//! to restore an unintended cancellation is that token, and this is where what it restores is
//! kept.
//!
//! The record is the subscription as it was, taken out of the live store by the call that
//! deactivated it: restoring it is therefore the subscription that was cancelled rather than
//! a new one built from the request, which is what "restore the subscription" means. A
//! cancellation is only kept for a subscription whose endpoint was SENT the token (an HTTP/S
//! endpoint), because nobody else can present it.
//!
//! Bounded like every other caller-grown store here ([`super::MAX_RECORDS_PER_STORE`]): a
//! cancellation is one call's work, so the oldest is dropped once the store is full — a
//! dropped one simply cannot be restored, exactly as an expired token cannot.

use super::*;

/// Whether the request `ctx` is serving presents NO Amazon Web Services credential at all.
///
/// SigV4 has two presentations and a request uses one of them, so both are read through the
/// tree's one reading of that question
/// ([`::service_common::sigv4_presentation::presents_a_credential`]).
///
/// A call one emulated service makes of another through the in-process callback is never
/// unauthenticated however it is signed: it is Amazon Web Services acting inside itself
/// ([`super::authorization::is_in_process_call`]), so a stack tearing a subscription down is
/// an authenticated call of the account's own and not an endpoint owner following a link.
#[must_use]
pub fn presents_no_credential(ctx: &::smithy_server_runtime::OperationContext<'_>) -> bool {
    if super::authorization::is_in_process_call(ctx) {
        return false;
    }
    let parts = ctx.parts();
    let authorization = parts
        .headers
        .get("authorization")
        .and_then(|value| value.to_str().ok());
    !::service_common::sigv4_presentation::presents_a_credential(authorization, parts.uri.query())
}

/// Whether the requester of an `Unsubscribe` is NOT the subscription's owner, which is the
/// condition the final cancellation is delivered under.
///
/// "If the subscription requires authentication for deletion, only the owner of the
/// subscription or the topic's owner can unsubscribe, and an Amazon Web Services signature is
/// required. If the Unsubscribe call does not require authentication and the requester is not
/// the subscription owner, a final cancellation message is delivered to the endpoint"
/// (`Unsubscribe`). So two kinds of requester are not the owner: one that authenticated NOBODY
/// (`unauthenticated`), which is what an endpoint owner following a link SNS delivered is, and
/// one speaking for ANOTHER principal than the one the subscription records as its own
/// (`owner_principal`, what `SubscriptionPrincipal` reports) — a role session, say. A request
/// signed by the principal that created the subscription is the owner's own.
#[must_use]
pub fn requester_is_not_the_owner(
    unauthenticated: bool,
    requester_principal: &str,
    owner_principal: &str,
) -> bool {
    unauthenticated || requester_principal != owner_principal
}

/// Whether the requester of an `Unsubscribe` may not delete a subscription that REQUIRES
/// authentication for deletion: "only the owner of the subscription or the topic's owner can
/// unsubscribe, and an Amazon Web Services signature is required" (`Unsubscribe`).
///
/// Two requesters are admitted, which is the other half of the sentence the cancellation rests
/// on: the principal the subscription records as its own, and the TOPIC's owner — the account
/// that holds the topic (`owner_account`, what a topic's `Owner` attribute reports), named by
/// whichever principal of it signed the call, since every principal of an account acts for
/// that account's ownership of its own topic. A call that authenticated nobody is neither, so
/// the requirement is exactly what the member asks for: "disallows unauthenticated
/// unsubscribes of the subscription".
#[must_use]
pub fn requester_may_not_delete(
    unauthenticated: bool,
    requester_principal: &str,
    owner_principal: &str,
    owner_account: &str,
) -> bool {
    if unauthenticated {
        return true;
    }
    if requester_principal == owner_principal {
        return false;
    }
    ::service_common::arn::parse(requester_principal)
        .is_none_or(|parsed| parsed.account != owner_account)
}

/// The subscriptions an `Unsubscribe` by someone other than their owner deactivated, keyed by
/// the token its cancellation message carried, oldest first.
#[derive(Debug, Default)]
pub struct Deactivated {
    held: HashMap<String, SubscriptionRecord>,
    order: VecDeque<String>,
}

impl Deactivated {
    /// Keep `subscription` as the one `token` restores, dropping the oldest cancellation
    /// once the store holds [`super::MAX_RECORDS_PER_STORE`] of them.
    pub fn keep(&mut self, token: String, subscription: SubscriptionRecord) {
        if self.held.insert(token.clone(), subscription).is_none() {
            self.order.push_back(token);
        }
        while self.order.len() > MAX_RECORDS_PER_STORE {
            if let Some(oldest) = self.order.pop_front() {
                self.held.remove(&oldest);
            }
        }
    }

    /// The subscription `token` restores on `topic_arn`, taken out of this store — so one
    /// cancellation is restored once and the token then confirms nothing.
    ///
    /// The topic is part of the match because `ConfirmSubscription` names one: a token
    /// presented against another topic restores nothing, exactly as a pending
    /// confirmation's does not.
    pub fn take(&mut self, topic_arn: &str, token: &str) -> Option<SubscriptionRecord> {
        let belongs = self
            .held
            .get(token)
            .is_some_and(|subscription| subscription.topic_arn == topic_arn);
        if !belongs {
            return None;
        }
        self.order.retain(|held| held != token);
        self.held.remove(token)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the requester of an `Unsubscribe` is not the subscription's owner when it
    /// authenticated nobody, or when it speaks for another principal than the one the
    /// subscription records. The rows differ along both: whether a credential was presented,
    /// and which principal it speaks for.
    #[test]
    fn only_another_requester_than_the_owner_is_sent_the_cancellation() {
        let owner = "arn:aws:iam::000000000000:user/emulator";
        let session = "arn:aws:sts::000000000000:assumed-role/teardown/a-session";
        // (case, whether the call authenticated nobody, the requester, whether it is not the
        // owner)
        let rows: &[(&str, bool, &str, bool)] = &[
            ("the owner's own signed call", false, owner, false),
            ("a call that authenticated nobody", true, owner, true),
            ("a signed call of another principal", false, session, true),
            (
                "a call that authenticated nobody, speaking for another principal",
                true,
                session,
                true,
            ),
        ];
        for (case, unauthenticated, requester, expected) in rows {
            let read = requester_is_not_the_owner(*unauthenticated, requester, owner);
            assert_eq!(
                read, *expected,
                "case {case:?}: expected {expected:?} for {requester:?}, got {read:?}"
            );
        }
    }

    /// RULE: a subscription that requires authentication for deletion admits two requesters —
    /// its own principal and the account that owns the topic — and nobody else. The rows
    /// differ along both: whether a credential was presented, and which account the
    /// requester's principal belongs to.
    #[test]
    fn only_the_subscriptions_own_account_may_delete_a_protected_subscription() {
        let owner = "arn:aws:iam::000000000000:user/emulator";
        let account = "000000000000";
        // (case, whether the call authenticated nobody, the requester, whether it may NOT
        // delete)
        let rows: &[(&str, bool, &str, bool)] = &[
            ("the subscription's own principal", false, owner, false),
            (
                "a session of the account that owns the topic",
                false,
                "arn:aws:sts::000000000000:assumed-role/teardown/a-session",
                false,
            ),
            (
                "a principal of another account",
                false,
                "arn:aws:iam::111122223333:user/elsewhere",
                true,
            ),
            ("a call that authenticated nobody", true, owner, true),
            (
                "a requester whose principal is no ARN at all",
                false,
                "nobody",
                true,
            ),
        ];
        for (case, unauthenticated, requester, expected) in rows {
            let read = requester_may_not_delete(*unauthenticated, requester, owner, account);
            assert_eq!(
                read, *expected,
                "case {case:?}: expected {expected:?} for {requester:?}, got {read:?}"
            );
        }
    }

    /// RULE: a cancellation is restored by the token it carried, once, and only against the
    /// topic the subscription was to.
    #[test]
    fn a_cancellation_is_restored_by_its_own_token_once() {
        let mut deactivated = Deactivated::default();
        let record = SubscriptionRecord {
            arn: "arn:aws:sns:us-east-1:000000000000:topic:0123".to_string(),
            topic_arn: "arn:aws:sns:us-east-1:000000000000:topic".to_string(),
            protocol: "https".to_string(),
            endpoint: Some("https://hooks.example.test/notify".to_string()),
            owner: "000000000000".to_string(),
            principal: "arn:aws:iam::000000000000:user/emulator".to_string(),
            attributes: HashMap::new(),
            pending_confirmation: false,
            confirmation_token: None,
            authenticate_on_unsubscribe: false,
            created_at: SystemTime::now(),
            replay: None,
        };
        deactivated.keep("a-token".to_string(), record.clone());
        assert!(
            deactivated
                .take("arn:aws:sns:us-east-1:000000000000:another", "a-token")
                .is_none(),
            "case 'another topic': a token restores nothing on a topic it is not of"
        );
        assert!(
            deactivated.take(&record.topic_arn, "not-the-token").is_none(),
            "case 'another token': only the token the cancellation carried restores it"
        );
        let restored = deactivated.take(&record.topic_arn, "a-token");
        assert_eq!(
            restored.map(|held| held.arn),
            Some(record.arn.clone()),
            "case 'the cancellation's own token': expected the subscription it cancelled"
        );
        assert!(
            deactivated.take(&record.topic_arn, "a-token").is_none(),
            "case 'the same token twice': a cancellation is restored once"
        );
    }
}
