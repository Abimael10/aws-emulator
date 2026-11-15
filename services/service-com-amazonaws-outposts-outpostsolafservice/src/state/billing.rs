// SPDX-License-Identifier: CC0-1.0

//! What this service BILLS: the subscriptions an Outpost's own orders and renewal
//! contracts create, and the contract term in force on it now.
//!
//! One file because every figure here follows from the same two records — the orders
//! placed on the Outpost ([`super::OrderRecord`]) and the renewals bought for it
//! ([`super::RenewalRecord`]) — so what the billing report says about a subscription
//! and what the pricing read ([`super::quotes`]) says about the same hardware cannot
//! disagree: both apply the one subscription-pricing rule
//! ([`subscription_price`]) to capacity the store already holds. The store itself is
//! [`super::store`]; the capacity those figures are applied to is
//! [`super::capacity`].

use std::time::SystemTime;

use super::*;

/// The `SubscriptionType` of the subscription the FIRST order placed on an Outpost
/// creates — the model's "`ORIGINAL` - the first order on the Amazon Web Services
/// Outposts" — from the modelled value set
/// [`crate::ops::types::SUBSCRIPTION_TYPE_VALUES`].
pub const SUBSCRIPTION_TYPE_ORIGINAL: &str = "ORIGINAL";

/// The `SubscriptionType` of the subscription every LATER order on the same Outpost
/// creates: the model's "`CAPACITY_INCREASE` - capacity scaling orders", which is
/// what an order placed on an Outpost that already has hardware under subscription
/// buys. See [`SUBSCRIPTION_TYPE_ORIGINAL`].
pub const SUBSCRIPTION_TYPE_CAPACITY_INCREASE: &str = "CAPACITY_INCREASE";

/// The `SubscriptionType` of the subscription a renewal contract creates: the model's
/// "`RENEWAL` - renewal requests, both month to month and longer term", which is what
/// `CreateRenewal` records against an Outpost. See [`SUBSCRIPTION_TYPE_ORIGINAL`].
pub const SUBSCRIPTION_TYPE_RENEWAL: &str = "RENEWAL";

/// The `SubscriptionStatus` of a subscription that exists but is not being billed
/// yet: the model's "`PENDING` - subscription has been created but billing has not
/// yet commenced because the subscription begin date has not been reached", which is
/// what a subscription whose hardware has not been installed yet holds. From the
/// modelled value set [`crate::ops::types::SUBSCRIPTION_STATUS_VALUES`].
pub const SUBSCRIPTION_STATUS_PENDING: &str = "PENDING";

/// The `SubscriptionStatus` of a subscription being billed now: the model's
/// "`ACTIVE` - subscription requests that are in progress and have an end date in the
/// future". See [`SUBSCRIPTION_STATUS_PENDING`].
pub const SUBSCRIPTION_STATUS_ACTIVE: &str = "ACTIVE";

/// The `SubscriptionStatus` of a subscription whose term has run out — the model's
/// "`INACTIVE` - subscription requests that are inactive". See
/// [`SUBSCRIPTION_STATUS_PENDING`].
pub const SUBSCRIPTION_STATUS_INACTIVE: &str = "INACTIVE";

/// The `SubscriptionStatus` of a subscription that was cancelled rather than billed:
/// the model's "`CANCELLED` - subscription requests that are cancelled", which is
/// what the subscription of a cancelled order is. See
/// [`SUBSCRIPTION_STATUS_PENDING`].
pub const SUBSCRIPTION_STATUS_CANCELLED: &str = "CANCELLED";

/// The prefix a `SubscriptionId` this service reports carries.
///
/// A subscription is not a resource any call of this API creates: it is the billing
/// record of an order or of a renewal contract, and the model constrains its id only
/// as a non-empty string ("the ID of the subscription that appears on the Amazon Web
/// Services Billing Center console"), publishing no form for it. So the id is
/// DERIVED from the record the subscription reports ([`subscription_id`]) rather than
/// minted on a read, which is what makes every read of one store report the same id
/// for the same subscription.
pub const SUBSCRIPTION_ID_PREFIX: &str = "sub";

/// The KIND a `GetOutpostBillingInformation` page token is stamped with.
///
/// What that report pages through is the subscriptions of ONE Outpost, so the list
/// has no existence apart from that Outpost: the stamp such a token carries is this
/// kind together with the Outpost the report was cut from
/// ([`nested_page_kind`]), and a token minted while paging another Outpost's
/// subscriptions is refused rather than honoured here. The boundary it names is a
/// place in the report's own derivation order — a reported subscription is the
/// billing record of an order or a renewal rather than a resource this store created,
/// so there is no creation sequence of one to name — exactly as
/// [`CONFIGURED_INSTANCE_TYPE_PAGE`] names a place in that report's order.
pub const BILLING_SUBSCRIPTION_PAGE: &str = "billingSubscriptions";

/// How the modelled `ContractEndDate` renders the date a contract term ends.
///
/// The member is a STRING rather than one of this API's `ISO8601Timestamp` members —
/// it is "the date the current contract term ends", a calendar date and not an
/// instant — so it is rendered as ISO 8601 writes a date: four-digit year, month and
/// day. The rendering satisfies the modelled pattern (`^[\S \n]+$`) and is well
/// inside its length.
const CONTRACT_END_DATE_FORMAT: &str = "%Y-%m-%d";

/// The billing information of ONE Outpost, as `GetOutpostBillingInformation` reports
/// it.
///
/// Derived by [`ComAmazonawsOutpostsOutpostsolafserviceState::outpost_billing`] from
/// the orders placed on that Outpost and the renewals bought for it, so every member
/// follows from what is persisted for it and a read answers the same report as the
/// next one.
#[derive(Debug, Clone, Default)]
pub struct OutpostBilling {
    /// "The subscription details for the specified Outpost": one subscription per
    /// order placed on it and per renewal contract bought for it, in the order those
    /// contracts run.
    pub subscriptions: Vec<Subscription>,
    /// The date the term of the contract IN FORCE on this Outpost ends, rendered as
    /// the modelled `ContractEndDate` ([`CONTRACT_END_DATE_FORMAT`]), or `None` for
    /// an Outpost with no contract in force now or one whose contract has no recorded
    /// term to end.
    pub contract_end_date: Option<String>,
    /// The `PaymentTerm` that contract covers, absent when it records none.
    pub payment_term: Option<String>,
    /// The `PaymentOption` that contract was bought under, absent when it records
    /// none.
    pub payment_option: Option<String>,
}

/// One subscription while it is being derived: how it is reported, the instant its
/// term ends — which is where the contract that renews it begins — and what it was
/// bought under, which is what the report of the Outpost's CURRENT contract states.
///
/// The modelled [`Subscription`] carries neither the payment option and term of the
/// contract behind it nor its end as an instant, so those travel beside it here
/// rather than being read back out of the projection.
#[derive(Debug, Clone)]
struct DerivedSubscription {
    /// The subscription as the report answers it.
    subscription: Subscription,
    /// When its term ends, or `None` for one with no recorded term and for one whose
    /// billing has not started.
    end: Option<SystemTime>,
    /// The `PaymentOption` the contract was bought under, if it records one.
    payment_option: Option<String>,
    /// The `PaymentTerm` it covers, if it records one.
    payment_term: Option<String>,
}

impl DerivedSubscription {
    /// Whether this subscription's term is the CONTRACT TERM in force now: one being
    /// billed at this instant — the modelled [`SUBSCRIPTION_STATUS_ACTIVE`], which is
    /// the status a term that has begun and not ended holds — and one that has a term
    /// to end at all, because a subscription with no recorded term states no contract
    /// term a report of the current one could be read off.
    fn is_current_contract(&self) -> bool {
        self.end.is_some()
            && self.subscription.subscription_status.as_deref() == Some(SUBSCRIPTION_STATUS_ACTIVE)
    }
}

impl ComAmazonawsOutpostsOutpostsolafserviceState {
    /// The billing information of the Outpost `outpost_id` names in `region`: the
    /// subscriptions its own records create and the contract in force on it now.
    ///
    /// "Current and historical billing information about the specified Outpost", and
    /// what this service holds of it is the hardware bought for the Outpost and the
    /// contracts bought to go on paying for it:
    ///
    /// * one subscription per ORDER placed on the Outpost, in the order they were
    ///   placed — the first of them the modelled
    ///   [`SUBSCRIPTION_TYPE_ORIGINAL`] and every later one the
    ///   [`SUBSCRIPTION_TYPE_CAPACITY_INCREASE`] a scaling order buys — billed from
    ///   the instant the hardware it delivers is installed ([`order_billing_start`]),
    ///   for the term that order was bought under;
    /// * one subscription per RENEWAL contract bought for it
    ///   ([`SUBSCRIPTION_TYPE_RENEWAL`]), each beginning where the term it renews
    ///   ends, because a renewal extends the subscription already on that hardware
    ///   rather than starting a second one beside it;
    /// * the terms of the one of those contracts in force at the instant it is asked
    ///   for — the term being billed now that runs longest — which is what the
    ///   `ContractEndDate`, `PaymentTerm` and `PaymentOption` of the report state.
    ///
    /// Every figure follows from a record this store holds: the price of an order's
    /// subscription is the one subscription-pricing rule ([`subscription_price`])
    /// applied to the capacity that order's own line items subscribe
    /// ([`Self::order_capacity_vcpus`]) under the terms it was bought under, and the
    /// price of a renewal's is the one that renewal was actually bought at
    /// ([`RenewalRecord::pricing`]) — so nothing here is a figure the emulator
    /// invented, and a combination this service holds no rule for is reported without
    /// a price rather than at one.
    ///
    /// A READ: it derives the report and writes nothing. The clock-owed order
    /// progress a subscription's begin date is read off is materialised by the caller
    /// ([`Self::settle_orders`]) before the report is derived, exactly as every other
    /// report of an order does it.
    pub fn outpost_billing(&self, region: &str, outpost_id: &str) -> OutpostBilling {
        let now = SystemTime::now();
        // The orders placed on this Outpost, in the order they were placed: which of
        // them is the Outpost's FIRST order is what separates the original
        // subscription from the capacity-increase ones, so the walk is ordered by the
        // store's own creation sequence rather than by how the order store happens to
        // be keyed. A cancelled order is NOT skipped here, as it is where the question
        // is what hardware an Outpost holds ([`Self::subscribed_orders`]): its
        // subscription is part of the Outpost's billing history, reported in the
        // modelled status of a subscription that was cancelled.
        let mut orders: Vec<&OrderRecord> = all_in_region(&self.orders, region)
            .filter(|record| record.order.outpost_id.as_deref() == Some(outpost_id))
            .collect();
        orders.sort_by_key(|record| record.sequence);
        let mut derived: Vec<DerivedSubscription> = orders
            .iter()
            .enumerate()
            .map(|(placed, record)| self.order_subscription(outpost_id, record, placed == 0, now))
            .collect();
        // A renewal renews the subscription on the Outpost's hardware, so its own term
        // starts where the term it renews runs out — the latest end the contracts
        // bought before it reach — and the next renewal starts where that one ends.
        // An Outpost whose hardware is not installed yet has no term that has started,
        // so a renewal of it has no begin date either, and is reported as a
        // subscription billing has not commenced for.
        let mut extends: Option<SystemTime> = derived.iter().filter_map(|held| held.end).max();
        for (bought, renewal) in self
            .outpost(region, outpost_id)
            .map(|record| record.renewals.as_slice())
            .unwrap_or_default()
            .iter()
            .enumerate()
        {
            let contract = renewal_subscription(outpost_id, bought, renewal, extends, now);
            extends = contract.end.or(extends);
            derived.push(contract);
        }
        // The contract IN FORCE now: of the terms being billed at this instant, the
        // one that runs longest. An Outpost with none — nothing ordered, nothing
        // installed yet, every term run out or cancelled, or nothing bought under a
        // term at all — has no current contract term, so the three members that state
        // one are absent rather than reported off a contract that is not in force.
        let current = derived
            .iter()
            .filter(|held| held.is_current_contract())
            .max_by_key(|held| held.end);
        OutpostBilling {
            contract_end_date: current.and_then(|held| held.end).map(contract_end_date),
            payment_term: current.and_then(|held| held.payment_term.clone()),
            payment_option: current.and_then(|held| held.payment_option.clone()),
            subscriptions: derived.into_iter().map(|held| held.subscription).collect(),
        }
    }

    /// The subscription the order `record` creates on the Outpost it was placed on:
    /// the hardware that order bought, billed over the term it was bought under.
    ///
    /// `first` says whether it is the Outpost's first order, which is what the
    /// modelled `SubscriptionType` of its subscription turns on. See
    /// [`Self::outpost_billing`].
    fn order_subscription(
        &self,
        outpost_id: &str,
        record: &OrderRecord,
        first: bool,
        now: SystemTime,
    ) -> DerivedSubscription {
        let order = &record.order;
        let payment_option = order.payment_option.clone();
        let payment_term = order.payment_term.clone();
        // Billing starts when the hardware the order delivers is installed, so the
        // term runs from there for the months the order's payment term names.
        let begin = order_billing_start(record);
        let end = begin.and_then(|begin| term_end(begin, payment_term.as_deref()?));
        // The subscription of a CANCELLED order was never billed — nothing was
        // delivered for it ([`subscribes_hardware`]) — which is the one modelled
        // status that says so.
        let cancelled = !subscribes_hardware(order);
        // What the subscription costs: the one pricing rule this service prices a
        // subscription by, applied to the capacity THIS order's line items subscribe
        // under the terms it was bought under. An order that records no term, or whose
        // line items name hardware the catalog publishes no capacity for, is a
        // subscription this service holds no figures for.
        let pricing = payment_option
            .as_deref()
            .zip(payment_term.as_deref())
            .and_then(|(option, term)| {
                subscription_price(self.order_capacity_vcpus(order) as f64, option, term)
            });
        DerivedSubscription {
            subscription: Subscription {
                subscription_id: Some(subscription_id(
                    outpost_id,
                    "order",
                    order.order_id.as_deref().unwrap_or_default(),
                )),
                subscription_type: Some(
                    match first {
                        true => SUBSCRIPTION_TYPE_ORIGINAL,
                        false => SUBSCRIPTION_TYPE_CAPACITY_INCREASE,
                    }
                    .to_string(),
                ),
                subscription_status: Some(subscription_status(cancelled, begin, end, now).to_string()),
                // "The order ID for your subscription": the order this subscription
                // is the billing record of, and no other.
                order_ids: order.order_id.clone().map(|order_id| vec![order_id]),
                begin_date: begin.map(timestamp),
                end_date: end.map(timestamp),
                currency: pricing.as_ref().and_then(|price| price.currency.clone()),
                monthly_recurring_price: pricing
                    .as_ref()
                    .and_then(|price| price.monthly_recurring_price)
                    .map(f64::from),
                upfront_price: pricing
                    .as_ref()
                    .and_then(|price| price.upfront_price)
                    .map(f64::from),
            },
            end,
            payment_option,
            payment_term,
        }
    }

    /// The EC2 capacity, in vCPUs, the order `order` subscribes: for each of its line
    /// items, the capacity the catalog publishes for the configuration it names times
    /// the quantity of that configuration it carries.
    ///
    /// The per-order share of what [`Self::subscribed_capacity_vcpus`] totals for a
    /// whole Outpost, read off the same per-line-item fold
    /// ([`subscribed_capacities`]) — so what one order's subscription is priced on and
    /// what a renewal of every order's hardware is priced on cannot disagree about
    /// which items count. A line item naming a catalog item this service publishes no
    /// configuration for contributes nothing, for the reason stated there.
    fn order_capacity_vcpus(&self, order: &Order) -> i64 {
        subscribed_capacities(order, |catalog_item_id| {
            self.catalog_item_capacity_vcpus(catalog_item_id)
                .map(i64::from)
        })
        .fold(0, i64::saturating_add)
    }
}

/// The subscription the renewal `renewal` — the `bought`th contract recorded against
/// the Outpost `outpost_id` — creates, extending the term that ends at `extends`.
///
/// A renewal is bought at a price ([`RenewalRecord::pricing`]), so the figures of its
/// subscription are the ones that contract was actually bought at rather than a
/// re-pricing of today's hardware: a renewal of hardware that has since changed is
/// still billed at what it was bought for. See
/// [`ComAmazonawsOutpostsOutpostsolafserviceState::outpost_billing`].
fn renewal_subscription(
    outpost_id: &str,
    bought: usize,
    renewal: &RenewalRecord,
    extends: Option<SystemTime>,
    now: SystemTime,
) -> DerivedSubscription {
    let pricing = &renewal.pricing;
    let end = extends.and_then(|begin| term_end(begin, pricing.payment_term.as_deref()?));
    DerivedSubscription {
        subscription: Subscription {
            subscription_id: Some(subscription_id(outpost_id, "renewal", &bought.to_string())),
            subscription_type: Some(SUBSCRIPTION_TYPE_RENEWAL.to_string()),
            subscription_status: Some(subscription_status(false, extends, end, now).to_string()),
            // A renewal is not an order: nothing in this API places one for it, so
            // this store holds no order id it could be the billing record of.
            order_ids: None,
            begin_date: extends.map(timestamp),
            end_date: end.map(timestamp),
            currency: pricing.currency.clone(),
            monthly_recurring_price: pricing.monthly_recurring_price.map(f64::from),
            upfront_price: pricing.upfront_price.map(f64::from),
        },
        end,
        payment_option: pricing.payment_option.clone(),
        payment_term: pricing.payment_term.clone(),
    }
}

/// When billing on the hardware the order `record` was placed for starts, or `None`
/// for an order that will never deliver any.
///
/// A subscription on hardware is billed from the moment that hardware is INSTALLED,
/// which for an order is its fulfilment ("the fulfillment date for the order") — so a
/// fulfilled order is billed from the date it records, and one still on its way is
/// billed from the instant its own fulfilment falls due
/// ([`OrderTransition::at`]), which is a date its subscription has not reached yet. A
/// cancelled order delivers nothing and is owed nothing, so it has no billing start at
/// all.
fn order_billing_start(record: &OrderRecord) -> Option<SystemTime> {
    if let Some(fulfilled) = record.order.order_fulfilled_date.as_ref() {
        return SystemTime::try_from(*fulfilled).ok();
    }
    record
        .pending
        .iter()
        .find(|transition| transition.status == ORDER_STATUS_COMPLETED)
        .map(|transition| transition.at)
}

/// When a term of `payment_term` that begins at `begin` ends, or `None` for a term
/// this service holds no length for and for one no instant can carry.
///
/// A term is a number of MONTHS ([`PAYMENT_TERM_MONTHS`], the same table the
/// subscription price is computed over) and months differ in length, so the end is
/// calendar arithmetic rather than a span of seconds: a term beginning on the 31st
/// ends on the last day of the month it ends in.
fn term_end(begin: SystemTime, payment_term: &str) -> Option<SystemTime> {
    let (_, months) = PAYMENT_TERM_MONTHS
        .iter()
        .find(|(term, _)| *term == payment_term)?;
    let months = ::chrono::Months::new(u32::try_from(*months).ok()?);
    let begin = ::chrono::DateTime::<::chrono::Utc>::from(begin);
    Some(SystemTime::from(begin.checked_add_months(months)?))
}

/// The modelled `SubscriptionStatus` of a term that begins at `begin` and ends at
/// `end`, as it stands at `now`.
///
/// The four modelled values are the four things that can be true of a subscription,
/// and the model documents each: one that was `cancelled` is
/// [`SUBSCRIPTION_STATUS_CANCELLED`]; one whose begin date has not been reached is
/// [`SUBSCRIPTION_STATUS_PENDING`], "billing has not yet commenced"; one that has
/// begun and whose end is still in the future is [`SUBSCRIPTION_STATUS_ACTIVE`]; and
/// one whose end has passed is [`SUBSCRIPTION_STATUS_INACTIVE`]. A term that has begun
/// and records no end has not run out, so it is being billed.
fn subscription_status(
    cancelled: bool,
    begin: Option<SystemTime>,
    end: Option<SystemTime>,
    now: SystemTime,
) -> &'static str {
    if cancelled {
        return SUBSCRIPTION_STATUS_CANCELLED;
    }
    match begin {
        Some(begin) if begin <= now => match end {
            Some(end) if end <= now => SUBSCRIPTION_STATUS_INACTIVE,
            _ => SUBSCRIPTION_STATUS_ACTIVE,
        },
        _ => SUBSCRIPTION_STATUS_PENDING,
    }
}

/// `at` as the modelled `ContractEndDate` states a date
/// ([`CONTRACT_END_DATE_FORMAT`]).
fn contract_end_date(at: SystemTime) -> String {
    ::chrono::DateTime::<::chrono::Utc>::from(at)
        .format(CONTRACT_END_DATE_FORMAT)
        .to_string()
}

/// The `SubscriptionId` of the subscription the record `source` of `kind` creates for
/// the Outpost `outpost_id`.
///
/// A subscription is the billing record of an order or of a renewal contract rather
/// than a resource a call of this API creates, so its id is DERIVED from the record it
/// reports: the same subscription is reported under the same id by every read of one
/// store, and no two records of it share an id. It is rendered like the ids this
/// service mints ([`ComAmazonawsOutpostsOutpostsolafserviceState::mint_id`]) — a short
/// prefix and 17 lowercase hex characters — which is well inside what the modelled
/// member accepts, since AWS publishes no form for it
/// ([`SUBSCRIPTION_ID_PREFIX`]).
fn subscription_id(outpost_id: &str, kind: &str, source: &str) -> String {
    let digest = Sha256::digest(format!("subscription|{outpost_id}|{kind}|{source}").as_bytes());
    let mut hex = hex::encode(&digest[..9]);
    // 9 bytes encode to 18 characters; the ids of this service are 17 long.
    hex.truncate(17);
    format!("{SUBSCRIPTION_ID_PREFIX}-{hex}")
}
