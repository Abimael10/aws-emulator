// SPDX-License-Identifier: CC0-1.0

//! Whether the customer managed key a request named for a session this plane MINTS is one
//! this account and Region HOLD — the question `CreateSession` asks before it writes the
//! session.
//!
//! `CreateSession`'s `encryptionKeyArn` is "the KMS key to use to encrypt the session data":
//! the key the session is encrypted under for as long as it exists, which `GetSession` reports
//! back. That member's `@length` and `@pattern` say only that the value is SHAPED like a key
//! ARN, never that a key is there, so whether the service can encrypt with it is not a
//! boundary check: it is AWS KMS's to answer.
//!
//! The two other members of this plane that carry a customer managed key —
//! `InvokeInlineAgent`'s `customerEncryptionKeyArn` and the `kmsKeyArn` of a
//! `RetrieveAndGenerate` session configuration — are NOT resolved here: neither call mints a
//! resource, each keeping a conversation under a `sessionId` the CALLER chose, and each
//! already answers its own refusals about such a conversation. What is asked here is the
//! question a create of a session asks.
//!
//! ## What the refusal is
//!
//! The service cannot ENCRYPT with a key it cannot use, and it reaches a key as the caller's
//! own role: an identifier this account and Region hold no key for is a key the role cannot
//! use, which is the `AccessDeniedException` class (HTTP 403) rather than an input-constraint
//! failure — "The request is denied because of missing access permissions". So the refusal
//! names the permissions to check ([`KEY_ACCESS_DENIED_MESSAGE`]), the sentence AWS words for
//! this class, and it is answered BEFORE the session is written: data encrypted under a key
//! the service cannot use could never be read back, and the refusal belongs on the call that
//! can still be corrected rather than on every later read of the session.
//!
//! ## What decides it
//!
//! The resolution is KMS's OWN
//! ([`kms_store::ComAmazonawsKmsTrentserviceState::resolve`] — the very lookup every KMS
//! operation taking a `KeyId` resolves its member through), so a key created through
//! `kms:CreateKey` can be used from the very next call and one scheduled for deletion stops
//! being usable exactly when KMS stops resolving it. Nothing is cached and nothing of the key
//! is copied: the session keeps the ARN the caller sent.
//!
//! Nothing is refused on anything that is NOT KMS stating this account and Region hold no such
//! key: a value this emulator's KMS cannot read as a key reference at all, and an engine
//! carrying no key store to ask, each establish nothing, and the write goes through — the same
//! bar the rest of this service's cross-service resolutions are held to. A key ARN naming
//! another Region or another account IS refused: KMS resolves no key of this account and
//! Region for it, and such a key is one this caller's role cannot use.
//!
//! LOCKING: this takes the KMS store's lock and no other, and nothing in KMS reaches into
//! this service's store, so the two can never be taken in opposite orders. Callers ask it
//! before taking this service's own guard, so one request holds one lock.

use ::service_com_amazonaws_kms_trentservice::state as kms_store;

use crate::ops::types::AccessDeniedException;

/// The sentence this service words a refusal of a customer managed key it cannot use with:
/// fixed text for the whole error class, naming the two places the key's access is granted —
/// the key's own policy and the caller's identity policy — and asking for the request to be
/// retried once they allow it.
///
/// It carries NO variable part: the key reference the caller sent is not echoed, and neither
/// is anything read out of the key store, so the refusal says nothing about which keys exist.
pub const KEY_ACCESS_DENIED_MESSAGE: &str =
    "Check that your IAM role has the necessary KMS permissions in both the key policy and \
     IAM policy, then retry your request.";

/// The refusal a request naming a key this account and Region hold none of earns, or `None`
/// when the value names a key KMS answered for (or names no key at all).
///
/// `region` is the Region the call was signed for, which is the endpoint the reference is
/// resolved at: a KMS key is a regional resource, so the same key id at another Region is a
/// different (absent) key.
pub fn unusable_encryption_key<Eng: Send + Sync + 'static>(
    engine: &Eng,
    region: &str,
    key_reference: Option<&str>,
) -> Option<AccessDeniedException> {
    let requested = key_reference.filter(|value| !value.is_empty())?;
    let Some(state) = kms_store::service_state(engine) else {
        // Visible to an operator: a reference nothing resolved is accepted, which without
        // this line is indistinguishable from one a held key answered for.
        ::tracing::warn!(
            "this engine carries no AWS KMS key store, so an encryption key reference is \
             accepted without resolving it"
        );
        return None;
    };
    let store = state.lock();
    match store.resolve(region, requested) {
        kms_store::KeyRef::Found(_) => None,
        // The two answers that are about the REFERENCE rather than about a key this account
        // and Region hold: an ARN this emulator's KMS cannot read as a key or alias ARN at
        // all, and one whose account segment is not an account id — in neither case was a key
        // looked up, so neither says anything about one.
        kms_store::KeyRef::MalformedArn | kms_store::KeyRef::UnauthorizedAccount => None,
        // KMS resolves no key of this account and Region for the reference — including one
        // whose ARN names another Region or another account, whose key this caller's role
        // cannot use either.
        kms_store::KeyRef::NotFound => Some(AccessDeniedException {
            message: Some(KEY_ACCESS_DENIED_MESSAGE.to_string()),
        }),
    }
}
