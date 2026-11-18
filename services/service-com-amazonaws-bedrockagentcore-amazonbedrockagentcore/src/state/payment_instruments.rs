// SPDX-License-Identifier: CC0-1.0

//! The PAYMENTS plane's instrument state: the payment instruments this service holds.
//!
//! A payment instrument is MINTED by `CreatePaymentInstrument` — the call that creates it
//! names the payment manager that owns it, the connector it draws on, the user it belongs
//! to and the instrument details it is made of, and this emulator mints the
//! `paymentInstrumentId` it is addressed by ([`mint_payment_instrument_id`]).
//! `ListPaymentInstruments` reports the instruments a manager owns, narrowed by the
//! connector and the user a request names, so an instrument that was never created is never
//! listed.
//!
//! An instrument belongs to a payment MANAGER, which the AgentCore CONTROL plane owns, so
//! the pre-condition every call of this plane keeps is the sessions' own
//! ([`super::payment_manager_resolved`]): a request naming a manager the account does not
//! hold is denied with the plane's one sentence
//! ([`super::PAYMENT_MANAGER_NOT_FOUND_MESSAGE`]) before anything is created or listed.
//!
//! An instrument's `status` is read off the CLOCK rather than stored: the linked account an
//! embedded wallet is created against is not linked by the instant the create answers, so a
//! fresh instrument is `INITIATED` and is `ACTIVE` once
//! [`PAYMENT_INSTRUMENT_LINKING_SPAN`] has passed, with no call needed to make it so
//! ([`PaymentInstrumentRecord::status_as_of`]) — the same way this plane's sibling resources
//! report a transition the clock already owes.
//!
//! The plane has an explicit ceiling: `CreatePaymentInstrument` is its growth site and a
//! caller chooses both the manager it creates under and the `clientToken` each insert is
//! recorded with, so it is bounded by [`MAX_PAYMENT_INSTRUMENTS`] and
//! [`MAX_PAYMENT_INSTRUMENT_IDEMPOTENCY_RECORDS`], each evicting its least recently written
//! record once that maximum is passed.

use std::time::{Duration, SystemTime};

use crate::ops::types::{PaymentInstrument, PaymentInstrumentDetails, PaymentInstrumentSummary};

use super::ComAmazonawsBedrockagentcoreAmazonbedrockagentcoreState;

/// The most payment instruments this emulator retains; the least recently created one is
/// evicted once the store passes it.
///
/// An EMULATOR ceiling, not an AWS quota: every `CreatePaymentInstrument` appends a record,
/// so without one a caller looping on the create would grow the process heap without end.
pub const MAX_PAYMENT_INSTRUMENTS: usize = 256;

/// The most `CreatePaymentInstrument` `clientToken` records this emulator retains.
pub const MAX_PAYMENT_INSTRUMENT_IDEMPOTENCY_RECORDS: usize = 1_000;

/// The prefix every `paymentInstrumentId` carries, fixed by `PaymentInstrumentId`'s pattern
/// `^payment-instrument-[0-9a-zA-Z-]{15}$` (source: smithy_query query="shape"
/// shape="PaymentInstrumentId").
pub const PAYMENT_INSTRUMENT_ID_PREFIX: &str = "payment-instrument-";

/// `PaymentInstrumentStatus` for an instrument whose linking has been started and not yet
/// completed (source: smithy_query query="shape" shape="PaymentInstrumentStatus", values
/// INITIATED | ACTIVE | FAILED | DELETED | BLOCKED).
pub const PAYMENT_INSTRUMENT_STATUS_INITIATED: &str = "INITIATED";

/// `PaymentInstrumentStatus` for an instrument that is ready to be paid with, from the same
/// enum.
pub const PAYMENT_INSTRUMENT_STATUS_ACTIVE: &str = "ACTIVE";

/// How long a created payment instrument stays `INITIATED` before it is `ACTIVE`.
///
/// AWS publishes no duration for the linking of the account an instrument is created
/// against, so this span is the EMULATOR's own: long enough that a caller which creates an
/// instrument and reads it back at once still observes the transient status, short enough
/// not to hold a test suite up. It is the span this service's sibling planes already use for
/// a transition the clock owes ([`super::AB_TEST_PROVISIONING_SPAN`]).
pub const PAYMENT_INSTRUMENT_LINKING_SPAN: Duration = super::AB_TEST_PROVISIONING_SPAN;

/// One payment instrument: what `CreatePaymentInstrument` persisted, which is what every
/// later report of the instrument is built from.
#[derive(Debug, Clone)]
pub struct PaymentInstrumentRecord {
    /// Minted by [`mint_payment_instrument_id`]; never client-supplied.
    pub payment_instrument_id: String,
    /// The manager that owns this instrument, as the creating request spelled it.
    pub payment_manager_arn: String,
    /// The connector this instrument draws on, as the creating request named it. A payment
    /// connector is no component's resource in this emulator, so the id is the caller's own
    /// value, carried rather than resolved.
    pub payment_connector_id: String,
    /// The user this instrument belongs to: the one the creating request named, and the
    /// empty value when it named none — `UserId` admits one (length ..120, no minimum) while
    /// `PaymentInstrument$userId` is `@required`, so an instrument created without a user is
    /// reported as belonging to none.
    pub user_id: String,
    /// `PaymentInstrumentType`, as the creating request asked for it.
    pub payment_instrument_type: String,
    /// The instrument details the creating request carried, reported back as submitted: what
    /// an instrument is made of is the caller's own, and nothing here provisions a wallet.
    pub payment_instrument_details: PaymentInstrumentDetails,
    /// The agent the creating request named, kept because the member is documented as
    /// carrying the agent "associated with this request, used for observability".
    pub agent_name: Option<String>,
    pub created_at: SystemTime,
    /// When this instrument was last changed by a call: its creation instant until one does.
    pub updated_at: SystemTime,
    /// The value [`ComAmazonawsBedrockagentcoreAmazonbedrockagentcoreState::next_seq`] handed
    /// this instrument: its eviction order, and what keeps two instruments created in the
    /// same millisecond apart.
    pub seq: u64,
}

impl PaymentInstrumentRecord {
    /// An instrument as the call that creates it leaves it: the connector, the user and the
    /// details that call asked for, created at `created_at`.
    #[allow(clippy::too_many_arguments)]
    pub fn created(
        seq: u64,
        payment_manager_arn: String,
        payment_connector_id: String,
        user_id: String,
        payment_instrument_type: String,
        payment_instrument_details: PaymentInstrumentDetails,
        agent_name: Option<String>,
        created_at: SystemTime,
    ) -> Self {
        Self {
            payment_instrument_id: mint_payment_instrument_id(created_at, seq),
            payment_manager_arn,
            payment_connector_id,
            user_id,
            payment_instrument_type,
            payment_instrument_details,
            agent_name,
            created_at,
            // Nothing has changed this instrument yet, so the last instant it changed is the
            // instant it came into being.
            updated_at: created_at,
            seq,
        }
    }

    /// The instant this instrument's linking is complete, or `None` when that instant is not
    /// representable (so nothing can be past it).
    pub fn active_at(&self) -> Option<SystemTime> {
        self.created_at.checked_add(PAYMENT_INSTRUMENT_LINKING_SPAN)
    }

    /// This instrument's `PaymentInstrumentStatus` at `now`: `INITIATED` until its linking is
    /// complete, `ACTIVE` from then on. Read off stored state and the clock, so a report of
    /// the instrument states the transition that was already due without writing anything.
    pub fn status_as_of(&self, now: SystemTime) -> &'static str {
        match self.active_at() {
            Some(active_at) if now >= active_at => PAYMENT_INSTRUMENT_STATUS_ACTIVE,
            _ => PAYMENT_INSTRUMENT_STATUS_INITIATED,
        }
    }

    /// This instrument as the modelled `PaymentInstrument`, which is what the create answers
    /// with, as of `now`.
    pub fn instrument(&self, now: SystemTime) -> PaymentInstrument {
        PaymentInstrument {
            payment_instrument_id: self.payment_instrument_id.clone(),
            payment_manager_arn: self.payment_manager_arn.clone(),
            payment_connector_id: self.payment_connector_id.clone(),
            user_id: self.user_id.clone(),
            payment_instrument_type: self.payment_instrument_type.clone(),
            payment_instrument_details: self.payment_instrument_details.clone(),
            created_at: ::aws_smithy_types::DateTime::from(self.created_at),
            status: self.status_as_of(now).to_string(),
            updated_at: ::aws_smithy_types::DateTime::from(self.updated_at),
        }
    }

    /// This instrument as the modelled `PaymentInstrumentSummary`, which is what a listing
    /// reports: the instrument's identity and status, without the details a create carries.
    pub fn summary(&self, now: SystemTime) -> PaymentInstrumentSummary {
        PaymentInstrumentSummary {
            payment_instrument_id: self.payment_instrument_id.clone(),
            payment_manager_arn: self.payment_manager_arn.clone(),
            payment_connector_id: self.payment_connector_id.clone(),
            user_id: self.user_id.clone(),
            payment_instrument_type: self.payment_instrument_type.clone(),
            status: self.status_as_of(now).to_string(),
            created_at: ::aws_smithy_types::DateTime::from(self.created_at),
            updated_at: ::aws_smithy_types::DateTime::from(self.updated_at),
        }
    }

    /// Whether this instrument is one the payment manager `payment_manager_arn` owns.
    ///
    /// The ARN is compared WHOLE, the way the manager's own existence is resolved: it names
    /// the partition, region and account the manager lives in, so an ARN that agrees only on
    /// the id addresses a manager somewhere else, whose instruments are not these.
    pub fn owned_by(&self, payment_manager_arn: &str) -> bool {
        self.payment_manager_arn == payment_manager_arn
    }

    /// Whether this instrument draws on the connector a listing filters by. The member is
    /// optional, so a request that names NO connector filters none out.
    pub fn drawn_on(&self, payment_connector_id: Option<&str>) -> bool {
        payment_connector_id.is_none_or(|named| self.payment_connector_id == named)
    }

    /// Whether this instrument belongs to the user a listing filters by, on the same rule as
    /// [`Self::drawn_on`].
    pub fn acts_for(&self, user_id: Option<&str>) -> bool {
        user_id.is_none_or(|named| self.user_id == named)
    }

    /// The total order payment instruments are reported in: newest first, with the creation
    /// sequence breaking a tie between two instruments created in the same millisecond, so
    /// one state always paginates the same way.
    pub fn order_key(&self) -> (SystemTime, u64) {
        (self.created_at, self.seq)
    }
}

/// Mint a `paymentInstrumentId` for an instrument created at `created_at`.
///
/// `PaymentInstrumentId` is `^payment-instrument-[0-9a-zA-Z-]{15}$` with length exactly 34,
/// so the id is the prefix the shape fixes followed by fifteen characters: the creation
/// instant in epoch milliseconds (its low 32 bits, eight lowercase hex digits) and the
/// engine-wide sequence (its low 28 bits, seven more), which keeps two instruments created
/// inside the same millisecond apart — the form [`super::mint_payment_session_id`] uses for
/// the sibling shape of this plane.
pub fn mint_payment_instrument_id(created_at: SystemTime, seq: u64) -> String {
    let millis = created_at
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |since_epoch| since_epoch.as_millis());
    format!(
        "{PAYMENT_INSTRUMENT_ID_PREFIX}{:08x}{:07x}",
        millis & 0xffff_ffff,
        seq & 0xfff_ffff
    )
}

impl ComAmazonawsBedrockagentcoreAmazonbedrockagentcoreState {
    /// The key a `clientToken` is remembered under: the manager it was used under and the
    /// token itself.
    ///
    /// The manager is part of the key, so a token is answered only for the manager it was
    /// used under and the same token used under two managers remembers BOTH instruments
    /// rather than one displacing the other.
    fn payment_instrument_token_key(
        payment_manager_arn: &str,
        client_token: &str,
    ) -> (String, String) {
        (
            payment_manager_arn.to_string(),
            client_token.to_string(),
        )
    }

    /// The payment instrument this `CreatePaymentInstrument` `clientToken` already created
    /// under `payment_manager_arn`, or `None` when it has created none this store still
    /// holds.
    pub fn payment_instrument_for_token(
        &self,
        payment_manager_arn: &str,
        client_token: &str,
    ) -> Option<&PaymentInstrumentRecord> {
        self.payment_instrument_idempotency
            .get(&Self::payment_instrument_token_key(
                payment_manager_arn,
                client_token,
            ))
            .and_then(|instrument_id| self.payment_instruments.get(instrument_id))
    }

    /// Store a created payment instrument, recording `client_token` as the token that created
    /// it, and hold every bound this plane declares.
    pub fn insert_payment_instrument(
        &mut self,
        record: PaymentInstrumentRecord,
        client_token: Option<String>,
    ) {
        if let Some(token) = client_token {
            self.payment_instrument_idempotency.insert(
                Self::payment_instrument_token_key(&record.payment_manager_arn, &token),
                record.payment_instrument_id.clone(),
            );
        }
        self.payment_instruments
            .insert(record.payment_instrument_id.clone(), record);

        while self.payment_instruments.len() > MAX_PAYMENT_INSTRUMENTS {
            // The least recently created instrument gives way first; the instruments a live
            // workflow just created are the ones that stay listable.
            let Some(oldest) = self
                .payment_instruments
                .values()
                .min_by_key(|held| held.seq)
                .map(|held| held.payment_instrument_id.clone())
            else {
                break;
            };
            self.payment_instruments.remove(&oldest);
            // The tokens that pointed at it go with it: a token whose instrument is gone
            // would otherwise let a replayed create mint a SECOND instrument instead of
            // answering with the one it already made.
            self.payment_instrument_idempotency
                .retain(|_, held| held != &oldest);
        }
        while self.payment_instrument_idempotency.len() > MAX_PAYMENT_INSTRUMENT_IDEMPOTENCY_RECORDS
        {
            // A token whose instrument is already gone is of no use to a replay, so it gives
            // way before one that can still be answered.
            let Some(oldest) = self
                .payment_instrument_idempotency
                .iter()
                .min_by_key(|(key, instrument_id)| {
                    (
                        self.payment_instruments
                            .get(*instrument_id)
                            .map_or(0, |held| held.seq),
                        (*key).clone(),
                    )
                })
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            self.payment_instrument_idempotency.remove(&oldest);
        }
    }

    /// The payment instruments `payment_manager_arn` owns, newest first
    /// ([`PaymentInstrumentRecord::order_key`]), narrowed to the connector a listing filters
    /// by ("the ID of the payment connector to filter by") and to the user it names ("the
    /// user ID associated with the payment instruments") when the request carries either.
    pub fn payment_instruments_newest_first(
        &self,
        payment_manager_arn: &str,
        payment_connector_id: Option<&str>,
        user_id: Option<&str>,
    ) -> Vec<&PaymentInstrumentRecord> {
        let mut instruments: Vec<&PaymentInstrumentRecord> = self
            .payment_instruments
            .values()
            .filter(|held| held.owned_by(payment_manager_arn))
            .filter(|held| held.drawn_on(payment_connector_id))
            .filter(|held| held.acts_for(user_id))
            .collect();
        instruments.sort_by_key(|held| std::cmp::Reverse(held.order_key()));
        instruments
    }
}
