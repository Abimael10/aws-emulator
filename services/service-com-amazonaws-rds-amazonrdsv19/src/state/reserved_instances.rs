// SPDX-License-Identifier: CC0-1.0

//! RESERVED DB INSTANCES: the offering catalogue this service publishes, the reservation
//! record a purchase persists, and the projections the three reserved-instance operations
//! report both through.
//!
//! Split out of [`super`] so that module keeps the store itself; every name here is
//! re-exported from it, so a handler reads them as `crate::state::<name>`.
//!
//! Two different kinds of thing live here, which is why the one concern is one file:
//!
//! * an OFFERING is published BY the service — "lists available reserved DB instance
//!   offerings" — so no request creates one and none is stored per account. It is REFERENCE
//!   data, derived from the dimensions a reservation is bought along (the DB instance class,
//!   the product description, the term, the payment option and whether it covers a Multi-AZ
//!   deployment) exactly as this crate's engine-version catalogue is derived from its own
//!   tables, and the prices follow from one published rate table ([`CLASS_HOURLY_RATE_PER_VCPU`]);
//! * a RESERVATION is what `PurchaseReservedDBInstancesOffering` persists for the account,
//!   so it is a record in the store, keyed per Region like every other record of this
//!   service, and `DescribeReservedDBInstances` reports what is there.
//!
//! The two are joined by the offering id a reservation carries: a purchase resolves that id
//! against the catalogue below and copies the offering's own terms onto the record, so a
//! reservation can never report a price or a term no offering publishes.

use std::sync::OnceLock;
use std::time::Instant;

use super::*;
use crate::ops::types::{RecurringCharge, ReservedDBInstance, ReservedDBInstancesOffering};

/// `ReservedDBInstancesOffering$Duration` for a one-year term, in seconds — the unit that
/// member is documented in ("the duration of the offering in seconds"), and one of the four
/// values the `Duration` filter publishes ("1 | 3 | 31536000 | 94608000").
pub const RESERVED_TERM_ONE_YEAR: i32 = 31_536_000;

/// The same for a THREE-year term, the other term the `Duration` filter's value list names.
pub const RESERVED_TERM_THREE_YEARS: i32 = 94_608_000;

/// The terms the offering catalogue publishes, shortest first: the two the `Duration` filter
/// names in years (`1`, `3`) and in seconds.
pub const RESERVED_TERMS: [i32; 2] = [RESERVED_TERM_ONE_YEAR, RESERVED_TERM_THREE_YEARS];

/// `OfferingType` — part of the term is paid as one upfront charge and the rest hourly.
pub const OFFERING_TYPE_PARTIAL_UPFRONT: &str = "Partial Upfront";

/// `OfferingType` — the whole term is paid as one upfront charge.
pub const OFFERING_TYPE_ALL_UPFRONT: &str = "All Upfront";

/// `OfferingType` — nothing is paid upfront and the whole term is billed hourly.
pub const OFFERING_TYPE_NO_UPFRONT: &str = "No Upfront";

/// Every offering type the catalogue publishes, in the order the member's own value list
/// writes them (`DescribeReservedDBInstancesOfferingsMessage$OfferingType`: "Partial
/// Upfront" | "All Upfront" | "No Upfront").
pub const RESERVED_OFFERING_TYPES: [&str; 3] = [
    OFFERING_TYPE_PARTIAL_UPFRONT,
    OFFERING_TYPE_ALL_UPFRONT,
    OFFERING_TYPE_NO_UPFRONT,
];

/// `ReservedDBInstancesOffering$CurrencyCode`: every price in the catalogue is quoted in US
/// dollars, the currency AWS publishes its price list in.
pub const RESERVED_CURRENCY_CODE: &str = "USD";

/// `RecurringCharge$RecurringChargeFrequency`: a reservation's recurring charge is billed per
/// hour of its term.
pub const RECURRING_CHARGE_HOURLY: &str = "Hourly";

/// The product descriptions the catalogue publishes — `ProductDescription` is "the database
/// engine used by the offering", and these are the values the reserved-instance price list
/// names an engine by: the engine's own name for the engines whose licence is the community
/// one, and the `(li)` suffix for the editions whose licence is included in the price.
///
/// Reference data, and an explicitly PARTIAL subset of AWS's own list, for the same reason
/// the DB instance classes below are: the real catalogue spans every engine and edition AWS
/// offers in every Region, which moves with each launch. What the catalogue guarantees is
/// that every description here is offered for every class, term and payment option, so a
/// client can read one and purchase it.
pub const RESERVED_PRODUCT_DESCRIPTIONS: [&str; 6] = [
    "aurora mysql",
    "aurora postgresql",
    "mariadb",
    "mysql",
    "postgresql",
    "sqlserver-se(li)",
];

/// The DB instance classes the catalogue publishes an offering for.
///
/// `DBInstanceClass` is "the DB instance class for the reserved DB instance", so the
/// catalogue spans the classes a reservation can be bought for. Reference data, and an
/// explicitly PARTIAL subset of the classes a DB instance can RUN
/// ([`crate::ops::describe_orderable_db_instance_options::instance_classes`]): the published
/// matrix is per Region and moves with every instance-type launch. One burstable, one
/// general-purpose and one memory-optimised generation are in it, in each of two sizes, so
/// the relation between the prices of two classes is observable.
pub const RESERVED_DB_INSTANCE_CLASSES: [&str; 8] = [
    "db.t3.micro",
    "db.t3.medium",
    "db.t4g.micro",
    "db.m5.large",
    "db.m5.xlarge",
    "db.m6g.large",
    "db.r5.large",
    "db.r6g.large",
];

/// The hourly on-demand rate, in US dollars, of ONE vCPU of a DB instance class, keyed by
/// the first letter of the class's instance family (`t3`, `m5`, `m6g`, `r5`, `r6g`).
///
/// This is the emulator's OWN price list: AWS's price list is not in the Smithy model and is
/// not published through this API, so there is nothing to read it from. What the catalogue
/// therefore guarantees is the RELATION between its prices, not an AWS price: a burstable
/// vCPU costs less than a general-purpose one and a general-purpose one less than a
/// memory-optimised one, the class's vCPU count scales each
/// ([`crate::ops::describe_orderable_db_instance_options::class_vcpus`], the one table this
/// crate reads a class's hardware from), and a Multi-AZ deployment runs two of them
/// ([`MULTI_AZ_FACTOR`]). Every price the catalogue reports is derived from this table, so
/// one offering costs the same on every call and in every run.
pub const CLASS_HOURLY_RATE_PER_VCPU: [(char, f64); 3] = [
    // Burstable (`t3`, `t4g`).
    ('t', 0.0085),
    // General purpose (`m5`, `m6g`).
    ('m', 0.0480),
    // Memory optimised (`r5`, `r6g`).
    ('r', 0.0600),
];

/// What a Multi-AZ deployment costs against a Single-AZ one: Amazon RDS runs a standby in a
/// second Availability Zone for it, so the deployment is two DB instances of the class and
/// its offering is priced as two.
pub const MULTI_AZ_FACTOR: f64 = 2.0;

/// The fraction of the on-demand rate a ONE-year reservation costs, before the payment
/// option's own discount. The emulator's own figure, like [`CLASS_HOURLY_RATE_PER_VCPU`];
/// what it fixes is that reserving is cheaper than paying on demand.
const ONE_YEAR_RATE: f64 = 0.70;

/// The same fraction for a THREE-year reservation: a longer commitment costs less per hour
/// than a shorter one, so this is below [`ONE_YEAR_RATE`].
const THREE_YEAR_RATE: f64 = 0.48;

/// The further discount for paying the whole term upfront — the largest of the three, so
/// `All Upfront` is the cheapest payment option of a term.
const ALL_UPFRONT_DISCOUNT: f64 = 0.92;

/// The further discount for paying part of the term upfront: between
/// [`ALL_UPFRONT_DISCOUNT`] and [`NO_UPFRONT_DISCOUNT`].
const PARTIAL_UPFRONT_DISCOUNT: f64 = 0.96;

/// Paying nothing upfront earns no further discount, so that payment option is the most
/// expensive of a term.
const NO_UPFRONT_DISCOUNT: f64 = 1.0;

/// How many seconds make an hour, for turning a term's `Duration` into the hours its charges
/// are billed over.
const SECONDS_PER_HOUR: f64 = 3600.0;

/// The hourly on-demand rate of one DB instance class in one deployment shape: its family's
/// rate per vCPU times the class's vCPU count, doubled for a Multi-AZ deployment.
///
/// `None` for a class whose hardware this service publishes nothing about, or whose family
/// is outside [`CLASS_HOURLY_RATE_PER_VCPU`] — such a class carries no offering rather than
/// one priced as a family it is not.
fn on_demand_hourly_price(db_instance_class: &str, multi_az: bool) -> Option<f64> {
    let vcpus = crate::ops::describe_orderable_db_instance_options::class_vcpus(db_instance_class)?;
    let family = db_instance_class
        .strip_prefix("db.")
        .and_then(|rest| rest.split('.').next())?;
    let generation = family.chars().next()?;
    let (_, rate) = CLASS_HOURLY_RATE_PER_VCPU
        .iter()
        .find(|(letter, _)| *letter == generation)?;
    let deployment = if multi_az { MULTI_AZ_FACTOR } else { 1.0 };
    Some(rate * f64::from(vcpus) * deployment)
}

/// The fraction of the on-demand rate a reservation of this term and offering type costs:
/// the term's rate times the payment option's discount.
///
/// `None` for an offering type outside [`RESERVED_OFFERING_TYPES`], so a value added to that
/// list is left out of the catalogue rather than silently priced as one of the three.
fn reserved_rate(duration: i32, offering_type: &str) -> Option<f64> {
    let term = if duration >= RESERVED_TERM_THREE_YEARS {
        THREE_YEAR_RATE
    } else {
        ONE_YEAR_RATE
    };
    let upfront = match offering_type {
        OFFERING_TYPE_ALL_UPFRONT => ALL_UPFRONT_DISCOUNT,
        OFFERING_TYPE_PARTIAL_UPFRONT => PARTIAL_UPFRONT_DISCOUNT,
        OFFERING_TYPE_NO_UPFRONT => NO_UPFRONT_DISCOUNT,
        _ => return None,
    };
    Some(term * upfront)
}

/// `value` rounded to whole cents, the precision an upfront charge is quoted at.
fn cents(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

/// `value` rounded to the precision an hourly rate is quoted at — thousandths of a cent, so
/// a small class's hourly charge does not round away to zero.
fn hourly(value: f64) -> f64 {
    (value * 100_000.0).round() / 100_000.0
}

/// The `ReservedDBInstancesOfferingId` of one catalogue entry.
///
/// DERIVED from what the entry IS — its class, product description, term, offering type and
/// deployment shape — through this crate's one name-based UUID minting
/// ([`super::uuid_of_seed`]), so the id is in the 8-4-4-4-12 form the member's own example
/// publishes (`438012d3-4052-4cc7-b2e3-8d3372e0e706`), two entries can never share one, and
/// the id a caller read in one call is the id the next call reports — which a freshly
/// generated id would not be.
pub fn reserved_offering_id(
    db_instance_class: &str,
    product_description: &str,
    duration: i32,
    offering_type: &str,
    multi_az: bool,
) -> String {
    super::uuid_of_seed(&format!(
        "reserved-db-instances-offering/{db_instance_class}/{product_description}/{duration}/\
         {offering_type}/{multi_az}"
    ))
}

/// One catalogue entry: what one DB instance class costs to reserve for one term under one
/// payment option, for one product description and one deployment shape.
///
/// The three price members hold together the way a reservation is billed: the upfront charge
/// is `FixedPrice`, whatever is left of the term is a recurring hourly charge, and
/// `UsagePrice` — "the hourly price charged for this offering", the rate charged for USING
/// the reserved capacity — is zero, because the reservation has already paid for those
/// hours. So `All Upfront` carries the whole term as a fixed price and nothing hourly, `No
/// Upfront` everything hourly and no fixed price, and `Partial Upfront` half the term each
/// way.
///
/// `RecurringCharges` carries ONE entry whatever the payment option: the charge is billed per
/// hour of the term for every offering, so an offering whose term is paid up front publishes
/// that charge with a ZERO amount — the charge exists and is nothing — rather than publishing
/// no charge at all, which would leave a caller summing an empty list to tell a prepaid
/// offering from one whose hourly rate the answer simply did not report.
fn reserved_offering(
    db_instance_class: &str,
    product_description: &str,
    duration: i32,
    offering_type: &str,
    multi_az: bool,
) -> Option<ReservedDBInstancesOffering> {
    let on_demand = on_demand_hourly_price(db_instance_class, multi_az)?;
    let effective_hourly = on_demand * reserved_rate(duration, offering_type)?;
    let term_hours = f64::from(duration) / SECONDS_PER_HOUR;
    let (fixed_price, recurring_hourly) = match offering_type {
        OFFERING_TYPE_ALL_UPFRONT => (effective_hourly * term_hours, 0.0),
        OFFERING_TYPE_PARTIAL_UPFRONT => {
            (effective_hourly * term_hours / 2.0, effective_hourly / 2.0)
        }
        OFFERING_TYPE_NO_UPFRONT => (0.0, effective_hourly),
        _ => return None,
    };
    Some(ReservedDBInstancesOffering {
        reserved_db_instances_offering_id: Some(reserved_offering_id(
            db_instance_class,
            product_description,
            duration,
            offering_type,
            multi_az,
        )),
        db_instance_class: Some(db_instance_class.to_string()),
        duration: Some(duration),
        fixed_price: Some(cents(fixed_price)),
        usage_price: Some(0.0),
        currency_code: Some(RESERVED_CURRENCY_CODE.to_string()),
        product_description: Some(product_description.to_string()),
        offering_type: Some(offering_type.to_string()),
        multi_az: Some(multi_az),
        recurring_charges: Some(vec![RecurringCharge {
            recurring_charge_amount: Some(hourly(recurring_hourly)),
            recurring_charge_frequency: Some(RECURRING_CHARGE_HOURLY.to_string()),
        }]),
    })
}

/// The whole offering catalogue this service publishes, in the order
/// `DescribeReservedDBInstancesOfferings` reports it.
///
/// Published BY the service rather than created by a caller, so it is reference data derived
/// ONCE from the tables above: every DB instance class in [`RESERVED_DB_INSTANCE_CLASSES`],
/// crossed with every product description in [`RESERVED_PRODUCT_DESCRIPTIONS`], every term in
/// [`RESERVED_TERMS`], every offering type in [`RESERVED_OFFERING_TYPES`] and both deployment
/// shapes — bounded and complete, so a class a client can RUN a reservation for is one it can
/// read every term and payment option of.
///
/// The ORDER is the order of those tables, outermost first, which is the order a price list
/// is read in: one class's descriptions, terms and payment options together, and the same
/// order on every call, so a page boundary and the `Marker` that resumes it hold still.
pub fn published_reserved_offerings() -> &'static [ReservedDBInstancesOffering] {
    static CATALOG: OnceLock<Vec<ReservedDBInstancesOffering>> = OnceLock::new();
    CATALOG.get_or_init(|| {
        let mut catalog = Vec::new();
        for class in RESERVED_DB_INSTANCE_CLASSES {
            for description in RESERVED_PRODUCT_DESCRIPTIONS {
                for duration in RESERVED_TERMS {
                    for offering_type in RESERVED_OFFERING_TYPES {
                        for multi_az in [false, true] {
                            if let Some(offering) = reserved_offering(
                                class,
                                description,
                                duration,
                                offering_type,
                                multi_az,
                            ) {
                                catalog.push(offering);
                            }
                        }
                    }
                }
            }
        }
        catalog
    })
}

/// The published offering `offering_id` names, or `None` for an id this service never issued.
///
/// The comparison is on the id the entry itself CARRIES, so a value only ever resolves to an
/// offering whose own reports carry that id — and a purchase and a report of the catalogue
/// cannot disagree about which offering an id names.
pub fn resolve_reserved_offering(offering_id: &str) -> Option<&'static ReservedDBInstancesOffering> {
    published_reserved_offerings().iter().find(|offering| {
        offering
            .reserved_db_instances_offering_id
            .as_deref()
            .is_some_and(|published| published == offering_id)
    })
}

/// Whether `requested` names the term `duration` seconds long: the `Duration` filter is
/// "specified in years or seconds" and publishes both spellings of each term
/// ("1 | 3 | 31536000 | 94608000"), so `1` and `31536000` select the same offerings.
pub fn duration_filter_matches(requested: &str, duration: i32) -> bool {
    let years = match duration {
        RESERVED_TERM_THREE_YEARS => "3",
        _ => "1",
    };
    requested == years || requested.parse::<i32>() == Ok(duration)
}

/// The message a `ReservedDBInstancesOfferingNotFoundFault` carries: the offering id the
/// request named, which "does not exist".
///
/// Amazon RDS words it with the resource kind in the singular — one OFFERING was asked for —
/// then the identifier the request carried and the `not found.` the other named-resource
/// faults of this front door end with. One sentence serves the report of the catalogue and
/// the purchase that resolves an id against it: the claim is the same about the same value in
/// both.
pub fn reserved_offering_not_found_message(offering_id: &str) -> String {
    format!("ReservedDBInstanceOffering {offering_id} not found.")
}

/// The message a `ReservedDBInstanceNotFoundFault` carries: the reservation id the request
/// named, which names no reservation this account purchased.
pub fn reserved_db_instance_not_found_message(reservation_id: &str) -> String {
    format!("ReservedDBInstance {reservation_id} not found.")
}

/// The message a `ReservedDBInstanceAlreadyExistsFault` carries, for the condition the
/// modelled fault publishes: "user already has a reservation with the given identifier".
///
/// `ReservedDBInstanceId` is the caller's own handle on the reservation
/// ("customer-specified identifier to track this reservation"), so the sentence names the
/// value the request carried, in the `<resource> <name> already exists.` wording this front
/// door answers a taken name with.
pub fn reserved_db_instance_already_exists_message(reservation_id: &str) -> String {
    format!("ReservedDBInstance {reservation_id} already exists.")
}

/// The message a `ReservedDBInstanceQuotaExceededFault` carries: the purchase "would exceed
/// the user's DB Instance quota", so the sentence names the allowance and what the request
/// would take the account to.
pub fn reserved_db_instance_quota_message(held: usize, asked: i32) -> String {
    format!(
        "The requested reservation of {asked} DB instances would exceed the Reserved DB \
         Instances quota of {RESERVED_DB_INSTANCE_QUOTA_PER_REGION}: {held} are already reserved."
    )
}

/// How many reserved DB instances one account holds in a Region: Amazon RDS's published
/// default quota ("Reserved DB instances": 40 per Region), which
/// `DescribeAccountAttributes` reports as the `ReservedDBInstances` maximum and
/// `PurchaseReservedDBInstancesOffering` refuses past — so the report and the refusal state
/// one and the same number. It counts the DB instances the account's reservations reserve,
/// which is what the quota is about, and bounds this store the way every other per-region
/// quota in `src/state.rs` does.
pub const RESERVED_DB_INSTANCE_QUOTA_PER_REGION: usize = 40;

/// How many reservation RECORDS one Region may hold at all, whatever state each is in.
///
/// The allowance above counts only the reservations that still HOLD capacity, so without
/// this cap a caller could let a reservation retire, win its allowance back and purchase
/// another without bound. Generous next to that allowance, since a reservation reserves at
/// least one DB instance.
pub const RESERVED_DB_INSTANCE_RECORD_QUOTA_PER_REGION: usize =
    RESERVED_DB_INSTANCE_QUOTA_PER_REGION * 2;

/// `ReservedDBInstance$State` — the state a purchase is accepted in, while Amazon RDS takes
/// the payment for the reservation.
pub const RESERVED_DB_INSTANCE_STATE_PAYMENT_PENDING: &str = "payment-pending";

/// `ReservedDBInstance$State` — the state a paid reservation holds its capacity in, and the
/// one its hours are billed against.
pub const RESERVED_DB_INSTANCE_STATE_ACTIVE: &str = "active";

/// `ReservedDBInstance$State` — the state a reservation reaches once its whole term has
/// passed: the reservation is over and holds no capacity any more.
pub const RESERVED_DB_INSTANCE_STATE_RETIRED: &str = "retired";

/// The ARN resource type of a reserved DB instance, as the published Amazon RDS ARN forms
/// write it ("Constructing an ARN for Amazon RDS": `ri`).
const ARN_RESOURCE_TYPE: &str = "ri";

/// A reservation this account purchased.
#[derive(Debug, Clone)]
pub struct ReservedDbInstanceRecord {
    /// `ReservedDBInstanceId`, which is half this record's key: the caller's own identifier
    /// for the reservation, or the one this service minted when the request named none.
    pub reservation_id: String,
    /// The region whose credential scope the purchase was signed with.
    pub region: String,
    /// `ReservedDBInstancesOfferingId`: the published offering this reservation was bought
    /// from, which every price and term below was copied from.
    pub offering_id: String,
    /// `DBInstanceClass`.
    pub db_instance_class: String,
    /// `StartTime`: "the time the reservation started", which is the instant the purchase was
    /// served.
    pub start_time: ::aws_smithy_types::DateTime,
    /// `Duration`, in seconds.
    pub duration: i32,
    /// `FixedPrice`.
    pub fixed_price: f64,
    /// `UsagePrice`.
    pub usage_price: f64,
    /// `CurrencyCode`.
    pub currency_code: String,
    /// `DBInstanceCount`: "the number of reserved DB instances".
    pub db_instance_count: i32,
    /// `ProductDescription`.
    pub product_description: String,
    /// `OfferingType`.
    pub offering_type: String,
    /// `MultiAZ`.
    pub multi_az: bool,
    /// `State`.
    pub state: String,
    /// `RecurringCharges`, as the offering published them.
    pub recurring_charges: Vec<RecurringCharge>,
    /// The state change the clock owes this record, if any.
    pub pending_status: Option<PendingStatus>,
    /// `Tags` — the tags the purchase carried. The reservation's own report has no tag
    /// member, so they are held for the tag operations that address a resource by ARN.
    pub tags: Vec<Tag>,
}

impl ReservedDbInstanceRecord {
    /// Apply a change whose due instant has passed, and retire a reservation whose term is
    /// over.
    ///
    /// The term's end is owed by the WALL clock rather than by this service: the reservation
    /// was over at `StartTime` plus `Duration` whether or not anybody called, so the state is
    /// materialised here exactly as the state the purchase owed it is. Nothing is removed:
    /// "the used value is the count of the active reserved DB instances", so a retired
    /// reservation is still reported, holding no capacity.
    pub(super) fn settle(&mut self, now: Instant, wall_clock: ::aws_smithy_types::DateTime) {
        if self.pending_status.as_ref().is_some_and(|p| now >= p.due_at) {
            let pending = self.pending_status.take().expect("checked just above");
            if let PendingChange::Status(state) = pending.change {
                self.state = state;
            }
        }
        if self.state == RESERVED_DB_INSTANCE_STATE_ACTIVE && self.term_ended(wall_clock) {
            self.state = RESERVED_DB_INSTANCE_STATE_RETIRED.to_string();
        }
    }

    /// Whether the reservation's term has run out by `wall_clock`.
    fn term_ended(&self, wall_clock: ::aws_smithy_types::DateTime) -> bool {
        wall_clock.as_secs_f64() >= self.start_time.as_secs_f64() + f64::from(self.duration)
    }

    /// Whether the reservation still HOLDS the capacity it reserved, which is what the
    /// account's `ReservedDBInstances` allowance counts.
    pub fn holds_capacity(&self) -> bool {
        self.state != RESERVED_DB_INSTANCE_STATE_RETIRED
    }

    /// How many DB instances this reservation reserves.
    pub fn reserved_instances(&self) -> usize {
        usize::try_from(self.db_instance_count).unwrap_or_default()
    }

    /// `ReservedDBInstanceArn`, from the stored region and the reservation's identifier.
    pub fn arn(&self) -> String {
        emulator_server::emulator_arn(
            ARN_NAMESPACE,
            &self.region,
            &format!("{ARN_RESOURCE_TYPE}:{}", self.reservation_id),
        )
    }

    /// `LeaseId`: "the unique identifier for the lease associated with the reserved DB
    /// instance".
    ///
    /// One lease goes with one reservation, so it is DERIVED from that reservation's own ARN
    /// — the handle that names it in one account and one Region — rather than stored beside
    /// it: the value a caller read in one call is the value the next call reports, and the
    /// `LeaseId` filter of the report resolves the same reservation it came from.
    pub fn lease_id(&self) -> String {
        super::uuid_of_seed(&format!("reserved-db-instance-lease/{}", self.arn()))
    }

    /// Project the stored record onto the modelled `ReservedDBInstance` shape.
    pub fn to_wire(&self) -> ReservedDBInstance {
        ReservedDBInstance {
            reserved_db_instance_id: Some(self.reservation_id.clone()),
            reserved_db_instances_offering_id: Some(self.offering_id.clone()),
            db_instance_class: Some(self.db_instance_class.clone()),
            start_time: Some(self.start_time),
            duration: Some(self.duration),
            fixed_price: Some(self.fixed_price),
            usage_price: Some(self.usage_price),
            currency_code: Some(self.currency_code.clone()),
            db_instance_count: Some(self.db_instance_count),
            product_description: Some(self.product_description.clone()),
            offering_type: Some(self.offering_type.clone()),
            multi_az: Some(self.multi_az),
            state: Some(self.state.clone()),
            recurring_charges: Some(self.recurring_charges.clone()),
            reserved_db_instance_arn: Some(self.arn()),
            lease_id: Some(self.lease_id()),
        }
    }
}

impl ComAmazonawsRdsAmazonrdsv19State {
    /// The reservations of `region`, in identifier order — the order
    /// `DescribeReservedDBInstances` reports them in.
    pub fn purchased_reserved_db_instances_in(
        &self,
        region: &str,
    ) -> Vec<&ReservedDbInstanceRecord> {
        range_in(&self.reserved_db_instances, region)
    }

    /// How many DB instances the reservations of `region` that still hold capacity reserve —
    /// the usage the account's `ReservedDBInstances` quota is counted against.
    ///
    /// Not to be confused with [`Self::reserved_db_instances_in`], which counts the DB
    /// instances the region's DB CAPACITY RESERVATIONS hold compute and memory for: that is
    /// capacity held in an Availability Zone, and this is a billing commitment over a DB
    /// instance class.
    pub fn reserved_db_instance_capacity_in(&self, region: &str) -> usize {
        self.purchased_reserved_db_instances_in(region)
            .into_iter()
            .filter(|record| record.holds_capacity())
            .map(ReservedDbInstanceRecord::reserved_instances)
            .fold(0, usize::saturating_add)
    }

    /// The states the reservations of `region` have reached, now that the clock has made them
    /// due: the one a purchase owes a reservation it has taken the payment for, and the
    /// retirement its own term makes due. Neither removes the record, so this walk collects
    /// no keys.
    pub(super) fn settle_reserved_db_instances(&mut self, region: &str, now: Instant) {
        let wall_clock = now_as_wire_timestamp();
        for (key, reservation) in self
            .reserved_db_instances
            .range_mut((region.to_string(), String::new())..)
        {
            if key.0 != region {
                break;
            }
            reservation.settle(now, wall_clock);
        }
    }
}
