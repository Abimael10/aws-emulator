// SPDX-License-Identifier: CC0-1.0

//! The one-time passcode verifications `SendNotifyCodeVerification` mints and
//! `ValidateNotifyCodeVerification` decides on.
//!
//! A verification is the whole backing of both operations: the send persists the
//! passcode it generated together with the policy that was resolved for it (the
//! character set, the length, the validity window and the attempt ceiling), and the
//! validate reads that record back. Nothing about the outcome of a validation is
//! decided anywhere else, so one record has one meaning whichever operation looks
//! at it.
//!
//! The policy is captured at send time — `SendNotifyCodeVerification` documents
//! that a later change to the referenced configuration does not affect a
//! verification already in progress — which is why [`Verification`] carries its own
//! [`CodePolicy`] instead of reading one off the configuration when a code is
//! validated.

use std::collections::BTreeMap;
use std::time::{Duration, SystemTime};

use ::rand::Rng as _;

use super::ComAmazonawsEndusermessagingEndusermessagingState;
use crate::ops::types::CodeConfigurationParameters;

/// The most verifications this emulator keeps in flight for one account.
///
/// A ceiling, not a guess at AWS's published quota: it exists so a send loop
/// cannot grow the emulator's heap without bound. `SendNotifyCodeVerification`
/// models `ServiceQuotaExceededException`, which is the refusal once the store is
/// full of passcodes that are still inside their validity window.
pub const MAX_VERIFICATIONS: usize = 100;

/// The passcode character set used when neither the send request nor the
/// referenced configuration names one. `CodeConfigurationParameters$codeType`
/// states that a default is applied when a passcode is sent but names no value, so
/// this is the emulator's choice among `CODE_TYPE_VALUES` — digits, the character
/// set a delivered passcode normally takes.
pub const DEFAULT_CODE_TYPE: &str = "NUMERIC";

/// The passcode length used when neither the send request nor the referenced
/// configuration sets one. Inside `CodeLength`'s modelled `range 4..8`.
pub const DEFAULT_CODE_LENGTH: i32 = 6;

/// The validity window, in minutes, used when neither the send request nor the
/// referenced configuration sets one. Inside `ValidityPeriodMinutes`'s modelled
/// `range 1..60`.
pub const DEFAULT_VALIDITY_PERIOD_MINUTES: i32 = 30;

/// The attempt ceiling used when neither the send request nor the referenced
/// configuration sets one. Inside `MaxVerificationAttempts`'s modelled
/// `range 1..5`.
pub const DEFAULT_MAX_ATTEMPTS: i32 = 5;

/// The `VerificationStatus` token for a passcode that matched an active
/// verification, as [`crate::ops::types::VERIFICATION_STATUS_VALUES`] spells it.
pub const VALID_STATUS: &str = "VALID";

/// The `VerificationStatus` token for a passcode that did not match, expired, or
/// exceeded its attempt limit, as
/// [`crate::ops::types::VERIFICATION_STATUS_VALUES`] spells it.
pub const INVALID_STATUS: &str = "INVALID";

/// The values `CodeLength` accepts (`@range(4..8)`). The boundary validator refuses
/// anything outside it before a handler runs; a resolved policy is clamped to it as
/// well, so no stored policy can size a passcode draw past this bound however the
/// value reached it.
pub const CODE_LENGTH_RANGE: ::std::ops::RangeInclusive<i32> = 4..=8;

/// The values `ValidityPeriodMinutes` accepts (`@range(1..60)`), clamped for the
/// same reason: the window a stored policy names stays inside the modelled range.
pub const VALIDITY_PERIOD_MINUTES_RANGE: ::std::ops::RangeInclusive<i32> = 1..=60;

/// The values `MaxVerificationAttempts` accepts (`@range(1..5)`), clamped for the
/// same reason.
pub const MAX_ATTEMPTS_RANGE: ::std::ops::RangeInclusive<i32> = 1..=5;

/// The passcode policy a send resolved, member by member.
///
/// Every member of `CodeConfigurationParameters` is optional in both the send
/// request's overrides and the referenced configuration, so each one is resolved on
/// its own: the per-send override wins, the configuration's value comes next, and
/// the service default applies when neither sets it. Each numeric member is then
/// clamped to the range the model declares for it, so a resolved policy is inside
/// the modelled bounds whatever layer supplied it and no request can size the
/// passcode draw or the validity window past them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodePolicy {
    /// `codeType` — the character set the passcode is drawn from.
    pub code_type: String,
    /// `codeLength` — how many characters the passcode has.
    pub code_length: i32,
    /// `validityPeriodMinutes` — how long the passcode stays valid.
    pub validity_period_minutes: i32,
    /// `maxAttempts` — how many validation attempts the verification allows.
    pub max_attempts: i32,
}

impl CodePolicy {
    /// The policy for one send: `overrides` from the request, `configured` from the
    /// referenced notify code configuration, each member falling through to the
    /// next layer and finally to this service's default.
    pub fn resolve(
        overrides: Option<&CodeConfigurationParameters>,
        configured: Option<&CodeConfigurationParameters>,
    ) -> Self {
        Self {
            code_type: overrides
                .and_then(|parameters| parameters.code_type.clone())
                .or_else(|| configured.and_then(|parameters| parameters.code_type.clone()))
                .unwrap_or_else(|| DEFAULT_CODE_TYPE.to_string()),
            code_length: overrides
                .and_then(|parameters| parameters.code_length)
                .or_else(|| configured.and_then(|parameters| parameters.code_length))
                .unwrap_or(DEFAULT_CODE_LENGTH)
                .clamp(*CODE_LENGTH_RANGE.start(), *CODE_LENGTH_RANGE.end()),
            validity_period_minutes: overrides
                .and_then(|parameters| parameters.validity_period_minutes)
                .or_else(|| {
                    configured.and_then(|parameters| parameters.validity_period_minutes)
                })
                .unwrap_or(DEFAULT_VALIDITY_PERIOD_MINUTES)
                .clamp(
                    *VALIDITY_PERIOD_MINUTES_RANGE.start(),
                    *VALIDITY_PERIOD_MINUTES_RANGE.end(),
                ),
            max_attempts: overrides
                .and_then(|parameters| parameters.max_attempts)
                .or_else(|| configured.and_then(|parameters| parameters.max_attempts))
                .unwrap_or(DEFAULT_MAX_ATTEMPTS)
                .clamp(*MAX_ATTEMPTS_RANGE.start(), *MAX_ATTEMPTS_RANGE.end()),
        }
    }

    /// How long a passcode drawn under this policy stays valid. Bounded by
    /// [`VALIDITY_PERIOD_MINUTES_RANGE`], which [`Self::resolve`] clamps to.
    pub fn validity_period(&self) -> Duration {
        let minutes = u64::try_from(self.validity_period_minutes)
            .unwrap_or(DEFAULT_VALIDITY_PERIOD_MINUTES as u64);
        Duration::from_secs(minutes.saturating_mul(60))
    }

    /// The instant a passcode drawn at `sent_at` stops being valid.
    ///
    /// The window is at most an hour, so the addition cannot leave the platform's
    /// time representation; it is still checked rather than added, so no request can
    /// turn this into a panic inside the handler.
    pub fn expires_at(&self, sent_at: SystemTime) -> SystemTime {
        sent_at
            .checked_add(self.validity_period())
            .unwrap_or(sent_at)
    }

    /// Draw a fresh passcode under this policy: `code_length` characters, each
    /// picked uniformly from the character set `code_type` names.
    pub fn draw_code(&self) -> String {
        let alphabet = code_alphabet(&self.code_type);
        // Inside the modelled range whatever the request asked for: `resolve`
        // clamped it, and this clamp is what makes that an invariant of the draw
        // rather than of its one caller.
        let length = usize::try_from(
            self.code_length
                .clamp(*CODE_LENGTH_RANGE.start(), *CODE_LENGTH_RANGE.end()),
        )
        .unwrap_or(DEFAULT_CODE_LENGTH as usize);
        let mut generator = ::rand::rng();
        (0..length)
            .map(|_| char::from(alphabet[generator.random_range(0..alphabet.len())]))
            .collect()
    }
}

/// The characters a passcode of `code_type` is drawn from, as
/// `CodeConfigurationParameters$codeType` documents each value: `NUMERIC` is digits
/// only, `ALPHA` uppercase letters only, and `ALPHANUMERIC` uppercase letters and
/// digits.
///
/// `CodeType` is a modelled enum, so the boundary validator has already refused any
/// other token by the time a handler resolves a policy; the default arm is the
/// `NUMERIC` set, which is also this service's default character set.
fn code_alphabet(code_type: &str) -> &'static [u8] {
    match code_type {
        "ALPHA" => b"ABCDEFGHIJKLMNOPQRSTUVWXYZ",
        "ALPHANUMERIC" => b"ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789",
        _ => b"0123456789",
    }
}

/// One passcode verification in flight: everything the send persisted and the
/// validate decides on.
///
/// `Debug` is written out rather than derived: `VerificationCode`,
/// `DestinationIdentity` and the context map are `@sensitive` in the model, so this
/// record reports their presence and never their value — a derived `Debug` would
/// carry a live passcode into any formatter that ever sees the store.
#[derive(Clone)]
pub struct Verification {
    /// The minted `verificationId` this record is keyed by.
    pub id: String,
    /// The minted `messageId` of the message that delivers the passcode.
    pub message_id: String,
    /// `destinationIdentity` — the recipient the passcode was sent to.
    pub destination_identity: String,
    /// `referenceId` as the send supplied it, which a validate must match to
    /// locate this verification.
    pub reference_id: Option<String>,
    /// The generated passcode. Never reported by any operation: a validate
    /// compares the submitted value against it and reports only the outcome.
    pub code: String,
    /// `channel`, one of [`crate::ops::types::NOTIFY_CHANNEL_VALUES`].
    pub channel: String,
    /// `originationIdentity` the passcode was sent from.
    pub origination_identity: String,
    /// The id of the notify code configuration the policy was captured from, when
    /// the send referenced one.
    pub notify_code_configuration_id: Option<String>,
    /// `configurationSetName` as the send supplied it.
    pub configuration_set_name: Option<String>,
    /// `context` as the send supplied it, in a sorted map so one record has one
    /// rendering.
    pub context: BTreeMap<String, String>,
    /// The policy captured at send time: later configuration updates do not reach
    /// a verification already in progress.
    pub policy: CodePolicy,
    pub created_at: SystemTime,
    /// The instant the validity period elapses, `created_at` plus the policy's
    /// `validityPeriodMinutes`.
    pub expires_at: SystemTime,
    /// How many validation attempts this verification has already taken.
    pub attempts: i32,
}

impl ::std::fmt::Debug for Verification {
    fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        f.debug_struct("Verification")
            .field("id", &self.id)
            .field("message_id", &self.message_id)
            .field("reference_id", &self.reference_id)
            .field("channel", &self.channel)
            .field("origination_identity", &self.origination_identity)
            .field(
                "notify_code_configuration_id",
                &self.notify_code_configuration_id,
            )
            .field("configuration_set_name", &self.configuration_set_name)
            .field("context_entries", &self.context.len())
            .field("policy", &self.policy)
            .field("created_at", &self.created_at)
            .field("expires_at", &self.expires_at)
            .field("attempts", &self.attempts)
            .finish_non_exhaustive()
    }
}

impl Verification {
    /// Whether the validity period has elapsed by `now`.
    pub fn is_expired(&self, now: SystemTime) -> bool {
        now >= self.expires_at
    }

    /// Whether the attempt ceiling the policy sets has been reached, which locks
    /// the verification: every further submission is invalid, the matching passcode
    /// included.
    pub fn attempts_exhausted(&self) -> bool {
        self.attempts >= self.policy.max_attempts
    }
}

impl ComAmazonawsEndusermessagingEndusermessagingState {
    /// Mint a fresh, unused `verificationId`.
    ///
    /// `VerificationId` documents an opaque service-generated token, so it takes
    /// the form every id this state hands out takes — a resource prefix plus a body
    /// of [`super::MINTED_ID_BODY_LENGTH`] characters drawn from fresh entropy
    /// ([`super::minted_id`]) — which satisfies that shape's `@length(1..64)` and
    /// `^[A-Za-z0-9_:/-]+$` pattern. The loop re-draws rather than ever handing back
    /// an id already in use.
    pub fn mint_verification_id(&mut self) -> String {
        loop {
            let id = super::minted_id("ver");
            if !self.verifications.contains_key(&id) {
                return id;
            }
        }
    }

    /// Mint a `messageId` for the message that delivers a passcode.
    ///
    /// Drawn from fresh entropy like every other minted id, so two messages never
    /// share one. `MessageId` documents an opaque service-generated token and this
    /// form satisfies its `@length(1..256)` and `^[A-Za-z0-9_:/-]+$` pattern. No map
    /// is keyed by it — a message is not a resource this service holds — so, unlike
    /// the ids above, this one asks the store nothing and only borrows it to stay
    /// beside the mints it belongs with.
    pub fn mint_message_id(&self) -> String {
        super::minted_id("msg")
    }

    /// Drop every verification whose validity period has elapsed by `now`.
    ///
    /// The expiry is owed by the clock alone — it was due at that instant whether
    /// or not anybody called — so materialising it is what lets both operations
    /// read one store that never holds a passcode past its validity window.
    pub fn expire_verifications(&mut self, now: SystemTime) {
        self.verifications
            .retain(|_, verification| !verification.is_expired(now));
    }

    /// The verification in flight for `destination_identity` under
    /// `reference_id`, or `None` when no live record matches.
    ///
    /// `referenceId` is documented as the value that locates the verification and
    /// must match the one the send supplied, so an absent one on either side is
    /// part of the key rather than a wildcard.
    pub fn verification_in_flight(
        &self,
        destination_identity: &str,
        reference_id: Option<&str>,
    ) -> Option<&Verification> {
        self.verifications.values().find(|verification| {
            verification.destination_identity == destination_identity
                && verification.reference_id.as_deref() == reference_id
        })
    }

    /// The same record, for the operation that counts an attempt against it.
    pub fn verification_in_flight_mut(
        &mut self,
        destination_identity: &str,
        reference_id: Option<&str>,
    ) -> Option<&mut Verification> {
        self.verifications.values_mut().find(|verification| {
            verification.destination_identity == destination_identity
                && verification.reference_id.as_deref() == reference_id
        })
    }

    /// Store `verification`, superseding the one already in flight for the same
    /// recipient and `referenceId`.
    ///
    /// A fresh passcode for a key that already has one replaces it, so only the
    /// newest passcode sent for that key can validate and one key never holds more
    /// than one record.
    ///
    /// [`MAX_VERIFICATIONS`] is an invariant of the map rather than of its caller: a
    /// send past the ceiling is refused with the operation's quota error before it
    /// gets here, and if one ever did get here the oldest verification is dropped so
    /// the store cannot grow without bound.
    pub fn record_verification(&mut self, verification: Verification) {
        self.verifications.retain(|_, stored| {
            stored.destination_identity != verification.destination_identity
                || stored.reference_id != verification.reference_id
        });
        while self.verifications.len() >= MAX_VERIFICATIONS {
            let oldest = self
                .verifications
                .values()
                .min_by_key(|stored| stored.created_at)
                .map(|stored| stored.id.clone());
            match oldest {
                Some(id) => {
                    self.verifications.remove(&id);
                }
                None => break,
            }
        }
        self.verifications
            .insert(verification.id.clone(), verification);
    }

    /// Consume the verification `id` names, so a passcode validates once.
    pub fn consume_verification(&mut self, id: &str) -> Option<Verification> {
        self.verifications.remove(id)
    }
}
