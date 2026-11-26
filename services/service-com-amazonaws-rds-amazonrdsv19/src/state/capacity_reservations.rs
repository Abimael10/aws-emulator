// SPDX-License-Identifier: CC0-1.0

//! DB CAPACITY RESERVATIONS: the record `CreateDBCapacityReservation` persists, the
//! projection `DescribeDBCapacityReservations` reports it through, and the published
//! values both read.
//!
//! Split out of [`super`] so that module keeps the store itself; every name here is
//! re-exported from it, so a handler reads them as `crate::state::<name>`.
//!
//! A reservation "reserves compute and memory capacity for DB instances of one DB instance
//! class and one database engine" in each Availability Zone a create names, so what is
//! stored is that configuration plus the state the reservation has reached; the ARN, the
//! available capacity per zone and the commitment a future-dated reservation carries are
//! FUNCTIONS of it, computed by [`DbCapacityReservationRecord::to_wire`] rather than
//! stored, for the same reason [`super::DbShardGroupRecord`] computes its own.

use super::*;

/// The state of a reservation Amazon RDS holds the reserved capacity for, and the only one
/// "a DB instance can use the reservation in" (`DBCapacityReservation$State`).
pub const DB_CAPACITY_RESERVATION_STATE_ACTIVE: &str = "active";

/// The state a reservation reports while "Amazon RDS is creating the reserved capacity, or
/// it is applying a requested modification" — what an accepted create of a reservation that
/// starts immediately reports until the transition the clock owes it comes due.
pub const DB_CAPACITY_RESERVATION_STATE_PENDING: &str = "pending";

/// The state a FUTURE-DATED reservation reports while "Amazon RDS is evaluating the request"
/// for it: the state an accepted create that named a `StartDate` reports first.
pub const DB_CAPACITY_RESERVATION_STATE_ASSESSING: &str = "assessing";

/// The state a future-dated reservation reaches once "Amazon RDS accepted" it and "the
/// reservation is waiting for its start date".
pub const DB_CAPACITY_RESERVATION_STATE_SCHEDULED: &str = "scheduled";

/// The state of a reservation that "passed its end date", after which "Amazon RDS releases
/// the reserved capacity".
pub const DB_CAPACITY_RESERVATION_STATE_EXPIRED: &str = "expired";

/// The state a reservation reaches once "you cancelled the reservation, and Amazon RDS
/// released the reserved capacity" — what `CancelDBCapacityReservation` leaves behind.
pub const DB_CAPACITY_RESERVATION_STATE_CANCELLED: &str = "cancelled";

/// The state a future-dated reservation reports when "Amazon RDS couldn't provide the
/// reserved capacity of a future-dated reservation by its start date".
pub const DB_CAPACITY_RESERVATION_STATE_DELAYED: &str = "delayed";

/// Every state `DBCapacityReservationState` publishes
/// (`smithy_query(query="shape",shape="DBCapacityReservationState")`), in the order the
/// enum lists them. This is the domain of the `state` filter of
/// `DescribeDBCapacityReservations`, bar the one value that filter excludes
/// ([`DB_CAPACITY_RESERVATION_FILTER_STATES`]).
pub const DB_CAPACITY_RESERVATION_STATES: [&str; 9] = [
    DB_CAPACITY_RESERVATION_STATE_ACTIVE,
    DB_CAPACITY_RESERVATION_STATE_EXPIRED,
    DB_CAPACITY_RESERVATION_STATE_CANCELLED,
    DB_CAPACITY_RESERVATION_STATE_PENDING,
    "failed",
    DB_CAPACITY_RESERVATION_STATE_ASSESSING,
    DB_CAPACITY_RESERVATION_STATE_SCHEDULED,
    DB_CAPACITY_RESERVATION_STATE_DELAYED,
    "unsupported",
];

/// The states a reservation can no longer leave: Amazon RDS has released the reserved
/// capacity, or it never held it. A reservation in one of these is past every transition
/// the clock could owe it, which is what keeps [`DbCapacityReservationRecord::settle`] from
/// moving it again.
const TERMINAL_DB_CAPACITY_RESERVATION_STATES: [&str; 4] = [
    DB_CAPACITY_RESERVATION_STATE_EXPIRED,
    DB_CAPACITY_RESERVATION_STATE_CANCELLED,
    "failed",
    "unsupported",
];

/// The states `ModifyDBCapacityReservation` can act in: "the reservation must be in the
/// `active` state. A future-dated reservation that hasn't started isn't modifiable, and
/// neither is a reservation that Amazon RDS is already modifying" — both of those report
/// another state ([`DB_CAPACITY_RESERVATION_STATE_SCHEDULED`],
/// [`DB_CAPACITY_RESERVATION_STATE_PENDING`]), so the one admitted state is the whole rule.
pub const DB_CAPACITY_RESERVATION_MODIFIABLE_STATES: [&str; 1] =
    [DB_CAPACITY_RESERVATION_STATE_ACTIVE];

/// The states `CancelDBCapacityReservation` can act in: "the reservation must be in the
/// `active`, `assessing`, or `delayed` state."
///
/// "The reservation must not be past its end date" is the other constraint of that call, and
/// needs no state of its own here: a reservation past its end date has already been moved to
/// [`DB_CAPACITY_RESERVATION_STATE_EXPIRED`] by the clock
/// ([`DbCapacityReservationRecord::settle`]), which is not one of these.
pub const DB_CAPACITY_RESERVATION_CANCELLABLE_STATES: [&str; 3] = [
    DB_CAPACITY_RESERVATION_STATE_ACTIVE,
    DB_CAPACITY_RESERVATION_STATE_ASSESSING,
    DB_CAPACITY_RESERVATION_STATE_DELAYED,
];

/// The values the `state` filter of `DescribeDBCapacityReservations` accepts: every
/// published state except `expired`, which that filter "doesn't accept" — "to find an
/// expired reservation, compare `EndDate` against the current date and time".
pub fn db_capacity_reservation_filter_states() -> Vec<&'static str> {
    DB_CAPACITY_RESERVATION_STATES
        .into_iter()
        .filter(|state| *state != DB_CAPACITY_RESERVATION_STATE_EXPIRED)
        .collect()
}

/// The prefix every DB capacity reservation IDENTIFIER carries:
/// `DBCapacityReservation$DBCapacityReservationIdentifier` is "the unique identifier of the
/// DB capacity reservation. The identifier has the prefix `dbcr-`."
///
/// It is also what a `DBCapacityReservationName` "can't begin with", so the two namespaces
/// a reservation is looked up by cannot collide.
pub const DB_CAPACITY_RESERVATION_ID_PREFIX: &str = "dbcr-";

/// The identifier Amazon RDS assigns a DB capacity reservation:
/// [`DB_CAPACITY_RESERVATION_ID_PREFIX`] followed by an opaque, server-minted token.
///
/// The token is written in LOWERCASE hexadecimal ([`minted_hex_token`]) because that is the
/// alphabet the identifier's own shape admits — `DBCapacityReservationIdentifier`'s pattern
/// is `^[a-z](?:-?[a-z0-9]+){0,15}$`, so the uppercase form a regional resource id of this
/// service is written in ([`minted_token`]) would name no identifier at all. The width is
/// the one this service mints every other server-assigned token at
/// ([`RESOURCE_ID_TOKEN_LEN`]); the model publishes none for this member.
pub fn mint_db_capacity_reservation_identifier(mint: u64) -> String {
    format!(
        "{DB_CAPACITY_RESERVATION_ID_PREFIX}{}",
        minted_hex_token("db-capacity-reservation", mint, RESOURCE_ID_TOKEN_LEN)
    )
}

/// `arn:aws:rds:<region>:<account>:db-capacity-reservation:<identifier>` — the ARN form
/// this front door gives a DB capacity reservation, with the resource type spelling the
/// resource kind out, as it does for the other resources whose kind has no published
/// abbreviation (`shard-group`, `db-proxy`, `cluster-endpoint`).
pub fn db_capacity_reservation_arn(region: &str, identifier: &str) -> String {
    emulator_server::emulator_arn(
        ARN_NAMESPACE,
        region,
        &format!("db-capacity-reservation:{identifier}"),
    )
}

/// The message a `DBCapacityReservationNotFoundFault` carries: the
/// `DBCapacityReservationIdentifier` the request sent "doesn't refer to an existing DB
/// capacity reservation", in the sentence Amazon RDS words that condition with.
///
/// The FIXED text is `The specified DBCapacityReservation '` … `' does not exist.` — the
/// single quotes around the value and the trailing period included — and the only variable
/// part is the value the REQUEST carried, the identifier or the reservation name, whichever
/// it sent. It is quoted as AWS quotes it, so a caller that lifts the value back out of the
/// sentence finds exactly what it sent.
///
/// ONE wording serves every call that resolves a reservation and finds none — the report,
/// the modification, the cancel and the call that names a reservation for a DB instance to
/// use: the fault is the same claim about the same value in all of them. This resource is
/// therefore NOT worded like the resources whose own refusal this door words
/// `<resource type> <name> not found.` ([`db_shard_group_not_found_message`]).
pub fn db_capacity_reservation_not_found_message(named: &str) -> String {
    format!("The specified DBCapacityReservation '{named}' does not exist.")
}

/// What a call on a DB capacity reservation whose `State` is not one it can act in is
/// refused with: the modelled `InvalidDBCapacityReservationStateFault` says "the DB capacity
/// reservation isn't in a valid state", and the sentence states the reservation, the state it
/// is in and the states the call needs, all three from the record and the call itself.
///
/// `named` is the value the REQUEST carried, and `expected` the states that call admits
/// ([`DB_CAPACITY_RESERVATION_MODIFIABLE_STATES`],
/// [`DB_CAPACITY_RESERVATION_CANCELLABLE_STATES`]) — so a caller can see whether waiting
/// will clear the refusal. One wording for every operation in that position, in the form this
/// front door words a resource's state refusal in ([`cluster_state_message`]), so the two
/// calls that raise this fault cannot drift from each other.
pub fn db_capacity_reservation_state_message(
    named: &str,
    state: &str,
    expected: &[&str],
) -> String {
    format!(
        "DBCapacityReservation {named} is in {state} state but expected it to be one of {}.",
        expected.join(", ")
    )
}

/// What a cancel of a reservation that is holding capacity for a CAPACITY COMMITMENT is
/// refused with: "you can't cancel a reservation in the `active` state during a capacity
/// commitment", and "the response of `DescribeDBCapacityReservations` reports the end of the
/// commitment in `CommitmentInfo`" — which is where the sentence sends the caller, rather
/// than restating a date the report already carries.
///
/// The same modelled fault as [`db_capacity_reservation_state_message`] with a different
/// cause, so the sentence is what tells the two apart: this one is not a state the
/// reservation will leave on its own before the commitment ends.
pub fn db_capacity_reservation_commitment_message(named: &str) -> String {
    format!(
        "DBCapacityReservation {named} can't be cancelled during its capacity commitment. The \
         commitment ends at the CommitmentEndDate of its CommitmentInfo."
    )
}

/// `EndDateType` `limited`: "the reservation has a specific end date. You must also specify
/// `EndDate`." One of the two values that enum publishes
/// (`smithy_query(query="shape",shape="EndDateType")`).
pub const END_DATE_TYPE_LIMITED: &str = "limited";

/// `EndDateType` `unlimited`: "the reservation doesn't expire. Don't specify `EndDate`."
pub const END_DATE_TYPE_UNLIMITED: &str = "unlimited";

/// Refuse a new `CapacityPerAvailabilityZone` below the capacity a running commitment holds:
/// "during a capacity commitment, can't be less than `CommittedCapacityPerAvailabilityZone`."
pub fn check_committed_capacity_floor(capacity: i32, committed: i32) -> Result<(), String> {
    if capacity < committed {
        return Err(format!(
            "Invalid value {capacity} for CapacityPerAvailabilityZone. Must be at least the \
             committed capacity of {committed} during a capacity commitment"
        ));
    }
    Ok(())
}

/// How many DB capacity reservation RECORDS one region may hold at all, whatever state each
/// is in.
///
/// AWS publishes no DB capacity reservation quota, so this is THIS emulator's store guard
/// rather than a modelled limit; the modelled fault a create past it answers is
/// `InstanceQuotaExceededFault`, the quota fault that operation declares. It counts EVERY
/// stored record, the way the export-task family caps its own store
/// ([`EXPORT_TASK_QUOTA_PER_REGION`]), because the reserved-instance allowance
/// ([`ComAmazonawsRdsAmazonrdsv19State::reserved_db_instances_in`]) counts only the
/// reservations that still HOLD capacity: without this cap a caller could expire a
/// reservation, win its allowance back and create another, without bound. Generous next to
/// that allowance, since a reservation holds capacity for at least one DB instance of it.
pub const DB_CAPACITY_RESERVATION_QUOTA_PER_REGION: usize = DB_INSTANCE_QUOTA_PER_REGION;

/// The engines capacity can be reserved for, as
/// `CreateDBCapacityReservationRequest$Engine` publishes them ("Valid Values: aurora-mysql
/// | aurora-postgresql | mysql | postgres | mariadb | oracle-ee | oracle-ee-cdb |
/// oracle-se2 | oracle-se2-cdb | sqlserver-ee | sqlserver-se | sqlserver-web | sqlserver-ex
/// | sqlserver-dev-ee | sqlserver-dev-se | db2-ae | db2-ce | db2-se").
///
/// A subset of the engines a DB INSTANCE can run ([`DB_INSTANCE_ENGINES`]) — the custom
/// engines are not in it, and neither is an engine of a sibling control plane this one front
/// door serves — so the two lists are separate rather than one standing in for the other.
pub const DB_CAPACITY_RESERVATION_ENGINES: [&str; 18] = [
    "aurora-mysql",
    "aurora-postgresql",
    "db2-ae",
    "db2-ce",
    "db2-se",
    "mariadb",
    "mysql",
    "oracle-ee",
    "oracle-ee-cdb",
    "oracle-se2",
    "oracle-se2-cdb",
    "postgres",
    "sqlserver-dev-ee",
    "sqlserver-dev-se",
    "sqlserver-ee",
    "sqlserver-ex",
    "sqlserver-se",
    "sqlserver-web",
];

/// Whether capacity can be reserved for `engine` at all
/// ([`DB_CAPACITY_RESERVATION_ENGINES`]).
pub fn reserves_capacity_for_engine(engine: &str) -> bool {
    DB_CAPACITY_RESERVATION_ENGINES.contains(&engine)
}

/// The engines a create may name a `LicenseModel` for: "specify this parameter only for RDS
/// for Microsoft SQL Server Enterprise Edition, Standard Edition, and Developer Edition, for
/// RDS for Oracle, or for RDS for Db2 Standard Edition and Advanced Edition. Amazon RDS
/// rejects the request for any other engine. This includes Amazon Aurora, the Web and
/// Express editions of RDS for Microsoft SQL Server, and RDS for Db2 Community Edition."
///
/// A subset of [`DB_CAPACITY_RESERVATION_ENGINES`]: the editions whose licence the CALLER
/// chooses. WHICH values each of them accepts is the one published answer
/// `DescribeOrderableDBInstanceOptions` gives
/// ([`crate::ops::describe_orderable_db_instance_options::license_models`]), so this list
/// states only which engines take the member at all.
pub const DB_CAPACITY_RESERVATION_LICENSED_ENGINES: [&str; 10] = [
    "db2-ae",
    "db2-se",
    "oracle-ee",
    "oracle-ee-cdb",
    "oracle-se2",
    "oracle-se2-cdb",
    "sqlserver-dev-ee",
    "sqlserver-dev-se",
    "sqlserver-ee",
    "sqlserver-se",
];

/// Whether a reservation for `engine` may name a `LicenseModel`
/// ([`DB_CAPACITY_RESERVATION_LICENSED_ENGINES`]).
pub fn reservation_takes_a_license_model(engine: &str) -> bool {
    DB_CAPACITY_RESERVATION_LICENSED_ENGINES.contains(&engine)
}

/// What a request naming a configuration DB capacity reservations do not cover is refused
/// with: the modelled `DBCapacityReservationUnsupportedFault` says "the specified
/// configuration isn't supported for DB capacity reservations", and this names the member
/// and the value that is not covered, so a caller can see which part to change.
pub fn db_capacity_reservation_unsupported_message(member: &str, value: &str) -> String {
    format!(
        "The specified configuration isn't supported for DB capacity reservations: \
         {member}={value}"
    )
}

/// Refuse a `DBCapacityReservationName` that begins with the identifier prefix.
///
/// The member's remaining constraints — "must contain from 1 to 63 lowercase letters,
/// digits, or hyphens", the leading lowercase letter, the trailing and doubled hyphen and
/// the sixteen hyphen-separated groups — are the `@length` and `@pattern` its own shape
/// publishes, which the boundary validator has already refused before a handler runs. This
/// is the one clause the pattern cannot express: a name that "can't begin with `dbcr-`",
/// because that prefix belongs to the identifiers Amazon RDS mints
/// ([`DB_CAPACITY_RESERVATION_ID_PREFIX`]) and a lookup takes either.
pub fn check_db_capacity_reservation_name(name: &str) -> Result<(), String> {
    if name.starts_with(DB_CAPACITY_RESERVATION_ID_PREFIX) {
        return Err(format!(
            "Invalid value {name} for DBCapacityReservationName. The name can't begin with \
             {DB_CAPACITY_RESERVATION_ID_PREFIX}"
        ));
    }
    Ok(())
}

/// Refuse a `CapacityPerAvailabilityZone` that reserves no DB instance.
///
/// The member is "the number of DB instances to reserve in each Availability Zone", and
/// Amazon RDS "reserves this number of DB instances in every listed Availability Zone": a
/// count below one reserves nothing, so it names no capacity at all. The model publishes no
/// `@range` for it, which is why the floor is checked here.
pub fn check_capacity_per_availability_zone(capacity: i32) -> Result<(), String> {
    if capacity < 1 {
        return Err(format!(
            "Invalid value {capacity} for CapacityPerAvailabilityZone. Must be at least 1"
        ));
    }
    Ok(())
}

/// Refuse an `AvailabilityZones` list that is not one a reservation can be held in.
///
/// Its own constraints: "must specify one or more Availability Zones", "each Availability
/// Zone can appear only once" and "each Availability Zone must be in the Amazon Web
/// Services Region of the request" — resolved against the zones that Region publishes
/// ([`region_availability_zones`]), the same list every other report of this service names
/// a Region's zones from.
pub fn check_reservation_availability_zones(region: &str, zones: &[String]) -> Result<(), String> {
    if zones.is_empty() {
        return Err(
            "Invalid value for AvailabilityZones. Must specify one or more Availability Zones"
                .to_string(),
        );
    }
    let published = region_availability_zones(region);
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    for zone in zones {
        if !seen.insert(zone.as_str()) {
            return Err(format!(
                "Invalid value {zone} for AvailabilityZones. Each Availability Zone can \
                 appear only once"
            ));
        }
        if !published.iter().any(|name| name == zone) {
            return Err(format!(
                "Invalid value {zone} for AvailabilityZones. Each Availability Zone must be \
                 in {region}, whose Availability Zones are {}",
                published.join(", ")
            ));
        }
    }
    Ok(())
}

/// The seconds in a day, the unit the `StartDate` window is published in. Durations only:
/// nothing here does calendar arithmetic, which is `chrono`'s job wherever this service
/// needs it.
const SECONDS_PER_DAY: i64 = 24 * 60 * 60;

/// How far in the future a `StartDate` must fall: "must be from 5 to 120 days in the
/// future".
const START_DATE_WINDOW_DAYS: std::ops::RangeInclusive<i64> = 5..=120;

/// The shortest capacity commitment a future-dated reservation may ask for, in seconds:
/// "minimum 1209600, which is 14 days" (`CreateDBCapacityReservation$CommitmentDuration`).
pub const MIN_COMMITMENT_DURATION_SECONDS: i64 = 1_209_600;

/// The shortest commitment a reservation of a `db.x` DB instance class may ask for: "for a
/// db.x DB instance class, such as db.x2g.xlarge, the minimum is 7257600, which is 84
/// days".
pub const MIN_X_CLASS_COMMITMENT_DURATION_SECONDS: i64 = 7_257_600;

/// The longest commitment a reservation may ask for: "must be less than 315576000, which
/// is 10 years".
pub const MAX_COMMITMENT_DURATION_SECONDS: i64 = 315_576_000;

/// Refuse a `StartDate` outside the window the member publishes, measured from `now`.
pub fn check_reservation_start_date(
    start_date: ::aws_smithy_types::DateTime,
    now: ::aws_smithy_types::DateTime,
) -> Result<(), String> {
    let ahead = start_date.as_secs_f64() - now.as_secs_f64();
    let window = (*START_DATE_WINDOW_DAYS.start() * SECONDS_PER_DAY) as f64
        ..=(*START_DATE_WINDOW_DAYS.end() * SECONDS_PER_DAY) as f64;
    if window.contains(&ahead) {
        return Ok(());
    }
    Err(format!(
        "Invalid value for StartDate. Must be from {} to {} days in the future",
        START_DATE_WINDOW_DAYS.start(),
        START_DATE_WINDOW_DAYS.end()
    ))
}

/// Refuse a `CommitmentDuration` outside the window the member publishes for
/// `db_instance_class`: the `db.x` classes carry a longer minimum than every other class.
pub fn check_commitment_duration(
    commitment_duration: i64,
    db_instance_class: &str,
) -> Result<(), String> {
    let minimum = if is_x_instance_class(db_instance_class) {
        MIN_X_CLASS_COMMITMENT_DURATION_SECONDS
    } else {
        MIN_COMMITMENT_DURATION_SECONDS
    };
    if commitment_duration >= minimum && commitment_duration < MAX_COMMITMENT_DURATION_SECONDS {
        return Ok(());
    }
    Err(format!(
        "Invalid value {commitment_duration} for CommitmentDuration. Must be a minimum of \
         {minimum} and less than {MAX_COMMITMENT_DURATION_SECONDS}"
    ))
}

/// Refuse an `EndDate` the reservation could not expire at: one that is not "a date and
/// time in the future", or one before `holds_until` — the instant a future-dated
/// reservation's commitment ends, which its expiry "must be at or after".
pub fn check_reservation_end_date(
    end_date: ::aws_smithy_types::DateTime,
    now: ::aws_smithy_types::DateTime,
    holds_until: Option<::aws_smithy_types::DateTime>,
) -> Result<(), String> {
    if end_date.as_secs_f64() <= now.as_secs_f64() {
        return Err(
            "Invalid value for EndDate. Must be a date and time in the future".to_string(),
        );
    }
    if holds_until.is_some_and(|until| end_date.as_secs_f64() < until.as_secs_f64()) {
        return Err(
            "Invalid value for EndDate. Must be at or after StartDate plus \
             CommitmentDuration"
                .to_string(),
        );
    }
    Ok(())
}

/// When the capacity commitment of a reservation that starts at `start_date` and asks for
/// `commitment_duration` seconds ends.
///
/// Amazon RDS "can grant a shorter capacity commitment than you request", so what a
/// reservation reports is the commitment GRANTED rather than the one asked for; this
/// service grants the whole request, which is the longest commitment the constraints admit
/// and the only one it has a rule for.
pub fn granted_commitment_end_date(
    start_date: ::aws_smithy_types::DateTime,
    commitment_duration: i64,
) -> ::aws_smithy_types::DateTime {
    ::aws_smithy_types::DateTime::from_secs_f64(
        start_date.as_secs_f64() + commitment_duration as f64,
    )
}

/// The DB instance class FAMILY letters a future-dated reservation may be held for:
/// "`DBInstanceClass` must be a db.c, db.m, db.r, db.t, or db.x DB instance class. The
/// letter after db. gives the DB instance class family."
const FUTURE_DATED_CLASS_FAMILIES: [char; 5] = ['c', 'm', 'r', 't', 'x'];
/// The first generation of a `db.m` or `db.r` class a future-dated reservation may be held
/// for: "if DBInstanceClass is a db.m or db.r DB instance class, it must be from generation
/// 7 or later. The digit after the family letter gives the generation."
const FUTURE_DATED_MIN_GENERATION: u32 = 7;

/// The family letter and generation digit of `db_instance_class`: the two characters after
/// `db.` that the future-dated constraints are stated over, or `None` for a value whose
/// family is not written that way at all.
fn class_family_and_generation(db_instance_class: &str) -> Option<(char, Option<u32>)> {
    let family = db_instance_class
        .to_ascii_lowercase()
        .strip_prefix("db.")?
        .split('.')
        .next()?
        .to_string();
    let mut characters = family.chars();
    let letter = characters.next()?;
    Some((letter, characters.next().and_then(|digit| digit.to_digit(10))))
}

/// Whether `db_instance_class` is one of the `db.x` classes, whose capacity commitment
/// carries its own minimum ([`MIN_X_CLASS_COMMITMENT_DURATION_SECONDS`]).
fn is_x_instance_class(db_instance_class: &str) -> bool {
    class_family_and_generation(db_instance_class).is_some_and(|(letter, _)| letter == 'x')
}

/// Whether a FUTURE-DATED reservation can be held for `db_instance_class`: its family is
/// one of [`FUTURE_DATED_CLASS_FAMILIES`], and a `db.m` or `db.r` class is of
/// [`FUTURE_DATED_MIN_GENERATION`] or later.
pub fn future_dated_class_is_eligible(db_instance_class: &str) -> bool {
    let Some((letter, generation)) = class_family_and_generation(db_instance_class) else {
        return false;
    };
    if !FUTURE_DATED_CLASS_FAMILIES.contains(&letter) {
        return false;
    }
    if matches!(letter, 'm' | 'r') {
        return generation.is_some_and(|generation| generation >= FUTURE_DATED_MIN_GENERATION);
    }
    true
}

/// The reserved capacity a future-dated reservation must total in each Availability Zone,
/// in vCPUs: "the reserved capacity must total at least 32 vCPUs in each Availability Zone.
/// Multiply `CapacityPerAvailabilityZone` by the number of vCPUs of the DB instance class.
/// The product must be 32 or more."
pub const MIN_FUTURE_DATED_VCPUS_PER_ZONE: i64 = 32;

/// Refuse a future-dated reservation whose reserved capacity does not reach
/// [`MIN_FUTURE_DATED_VCPUS_PER_ZONE`] in each Availability Zone.
///
/// The vCPU count of the class comes from the one published hardware table this service
/// holds ([`crate::ops::describe_orderable_db_instance_options::class_vcpus`]); a class that
/// table says nothing about has no vCPU count to multiply, so the product cannot be shown to
/// reach the minimum and the request is refused rather than admitted on a figure nothing
/// published.
pub fn check_future_dated_reserved_capacity(
    db_instance_class: &str,
    capacity_per_availability_zone: i32,
) -> Result<(), String> {
    let vcpus = crate::ops::describe_orderable_db_instance_options::class_vcpus(db_instance_class)
        .map(i64::from);
    let reserved = vcpus.map(|vcpus| vcpus.saturating_mul(i64::from(capacity_per_availability_zone)));
    if reserved.is_some_and(|reserved| reserved >= MIN_FUTURE_DATED_VCPUS_PER_ZONE) {
        return Ok(());
    }
    Err(format!(
        "CapacityPerAvailabilityZone={capacity_per_availability_zone} and \
         DBInstanceClass={db_instance_class} reserve fewer than \
         {MIN_FUTURE_DATED_VCPUS_PER_ZONE} vCPUs in each Availability Zone, which a \
         reservation with a StartDate must total"
    ))
}

/// The capacity commitment of a future-dated reservation: what
/// `DBCapacityReservation$CommitmentInfo` reports, and the reason a reservation in the
/// `active` state cannot be cancelled while it lasts.
#[derive(Debug, Clone)]
pub struct DbCapacityReservationCommitment {
    /// `CommittedCapacityPerAvailabilityZone`: "the number of DB instances that Amazon RDS
    /// holds in each Availability Zone of the reservation for the length of the
    /// commitment" — the capacity the create asked for.
    pub committed_capacity_per_availability_zone: i32,
    /// `CommitmentEndDate`: "the date and time when the commitment ends", which Amazon RDS
    /// grants from `StartDate` and the `CommitmentDuration` of the request.
    pub commitment_end_date: ::aws_smithy_types::DateTime,
}

/// A stored DB capacity reservation: everything this service persists about one.
#[derive(Debug, Clone)]
pub struct DbCapacityReservationRecord {
    /// `DBCapacityReservationIdentifier`, minted once by
    /// [`mint_db_capacity_reservation_identifier`]; the key every other DB capacity
    /// reservation operation takes.
    pub identifier: String,
    /// `DBCapacityReservationName`, the name the create chose.
    pub name: String,
    /// The region the reservation was created in; the scope of its ARN and of its zones.
    pub region: String,
    /// `DBInstanceClass`: "the compute and memory capacity to reserve". Immutable — "you
    /// can't change it afterward".
    pub db_instance_class: String,
    /// `Engine`: "the database engine to reserve capacity for". Immutable too.
    pub engine: String,
    /// `LicenseModel`, for a reservation created for an engine that has one. Amazon RDS
    /// "returns this value only for a reservation that reserves capacity for a specific
    /// license model".
    pub license_model: Option<String>,
    /// `ReservedCapacityPerAvailabilityZone`: "the number of DB instances that Amazon RDS
    /// reserves in each Availability Zone of the reservation".
    pub reserved_capacity_per_availability_zone: i32,
    /// The Availability Zones of the reservation, in the order the create listed them: the
    /// zones its capacity is reserved in, which "you can't change ... after you create it".
    pub availability_zones: Vec<String>,
    /// `CreateDate`: "the date and time when the reservation was created".
    pub create_date: ::aws_smithy_types::DateTime,
    /// `StartDate`, for a FUTURE-DATED reservation only: Amazon RDS "doesn't return this
    /// value for a reservation that started immediately".
    pub start_date: Option<::aws_smithy_types::DateTime>,
    /// `EndDate`, for a reservation that expires: Amazon RDS "doesn't return this value if
    /// the reservation doesn't expire".
    pub end_date: Option<::aws_smithy_types::DateTime>,
    /// `CommitmentInfo`, which Amazon RDS "returns ... only for a future-dated
    /// reservation".
    pub commitment: Option<DbCapacityReservationCommitment>,
    /// `State`.
    pub state: String,
    /// The transition the clock owes this record, if any.
    pub pending_state: Option<PendingStatus>,
    /// `TagList`.
    pub tags: Vec<Tag>,
}

impl DbCapacityReservationRecord {
    /// Materialise the transitions this record's clocks have already made due: the state
    /// the create owed it once `now` passes the due instant, and the expiry its own
    /// `EndDate` owes it once `wall_clock` reaches that date.
    ///
    /// Both were owed at this instant whether or not anybody called, which is what makes
    /// applying them in a REPORTING operation a materialisation rather than a write — the
    /// same contract [`DbShardGroupRecord::settle`] holds. Nothing here removes the record:
    /// an expired reservation is still reported, with no capacity left available in any of
    /// its zones.
    pub(super) fn settle(&mut self, now: Instant, wall_clock: ::aws_smithy_types::DateTime) {
        let due = self
            .pending_state
            .as_ref()
            .is_some_and(|pending| now >= pending.due_at);
        if due {
            match self.pending_state.take().map(|pending| pending.change) {
                Some(PendingChange::Status(state)) => self.state = state,
                // A DB capacity reservation is never removed by the clock: a cancel and an
                // expiry both leave the record behind in a terminal state. Answered as
                // "nothing was due" rather than with a panic in a request path.
                Some(PendingChange::Removal) | None => {}
            }
        }
        // "`expired` - The reservation passed its end date. Amazon RDS releases the
        // reserved capacity." A reservation that has already reached a state it cannot
        // leave keeps it: there is no capacity left to release.
        if self.is_past(wall_clock) && !self.is_terminal() {
            self.state = DB_CAPACITY_RESERVATION_STATE_EXPIRED.to_string();
            self.pending_state = None;
        }
    }

    /// Whether the reservation is past its own `EndDate` at `wall_clock`. A reservation
    /// created without one "doesn't expire", so it is never past.
    fn is_past(&self, wall_clock: ::aws_smithy_types::DateTime) -> bool {
        self.end_date
            .is_some_and(|end| wall_clock.as_secs_f64() >= end.as_secs_f64())
    }

    /// Whether the reservation has reached a state it cannot leave
    /// ([`TERMINAL_DB_CAPACITY_RESERVATION_STATES`]).
    fn is_terminal(&self) -> bool {
        TERMINAL_DB_CAPACITY_RESERVATION_STATES.contains(&self.state.as_str())
    }

    /// Whether this is a FUTURE-DATED reservation: one created with a `StartDate`, which
    /// "reserves capacity for a period that starts later" and therefore "always carries a
    /// capacity commitment".
    pub fn is_future_dated(&self) -> bool {
        self.start_date.is_some()
    }

    /// Whether a capacity commitment of this reservation is still RUNNING at `wall_clock`:
    /// Amazon RDS "holds that capacity for the length of the commitment", and the commitment
    /// it granted ends at [`DbCapacityReservationCommitment::commitment_end_date`].
    ///
    /// The one reading of that question: it is what keeps a cancel from releasing committed
    /// capacity ("you can't cancel a reservation in the `active` state during a capacity
    /// commitment") and what floors a modification's new capacity
    /// ([`check_committed_capacity_floor`]). A reservation that carries no commitment is in
    /// none, and one whose commitment has ended is past it.
    pub fn commitment_is_running(&self, wall_clock: ::aws_smithy_types::DateTime) -> bool {
        self.commitment.as_ref().is_some_and(|commitment| {
            wall_clock.as_secs_f64() < commitment.commitment_end_date.as_secs_f64()
        })
    }

    /// The capacity a running commitment of this reservation holds in each Availability Zone
    /// at `wall_clock`, or `None` where no commitment is running: the floor a modification's
    /// new `CapacityPerAvailabilityZone` cannot go below.
    pub fn committed_capacity_per_availability_zone(
        &self,
        wall_clock: ::aws_smithy_types::DateTime,
    ) -> Option<i32> {
        self.commitment_is_running(wall_clock)
            .then(|| {
                self.commitment
                    .as_ref()
                    .map(|commitment| commitment.committed_capacity_per_availability_zone)
            })
            .flatten()
    }

    /// `arn:aws:rds:<region>:<account>:db-capacity-reservation:<identifier>`, from the
    /// stored region and identifier.
    pub fn arn(&self) -> String {
        db_capacity_reservation_arn(&self.region, &self.identifier)
    }

    /// The unused capacity of the reservation in each of its Availability Zones.
    ///
    /// "Amazon RDS reports available capacity only for a reservation that is in the
    /// `active` state and not past its end date. Otherwise, each entry reports an available
    /// capacity of `0`." For an active one every reserved unit is unused until a DB instance
    /// takes one, and no DB instance of this emulator holds reserved capacity yet, so an
    /// active reservation reports its reserved capacity in each zone — never a figure
    /// invented for a unit nothing took.
    fn available_capacity(
        &self,
        wall_clock: ::aws_smithy_types::DateTime,
    ) -> Vec<AvailableCapacityPerAvailabilityZone> {
        let available = if self.state == DB_CAPACITY_RESERVATION_STATE_ACTIVE
            && !self.is_past(wall_clock)
        {
            self.reserved_capacity_per_availability_zone
        } else {
            0
        };
        self.availability_zones
            .iter()
            .map(|zone| AvailableCapacityPerAvailabilityZone {
                availability_zone: Some(zone.clone()),
                available_capacity: Some(available),
            })
            .collect()
    }

    /// Project the stored record onto the modelled `DBCapacityReservation` wire shape — the
    /// output of `CreateDBCapacityReservation` and one entry of the
    /// `DescribeDBCapacityReservations` report, so the two cannot disagree about a
    /// reservation.
    pub fn to_wire(&self) -> DBCapacityReservation {
        DBCapacityReservation {
            db_capacity_reservation_name: Some(self.name.clone()),
            db_capacity_reservation_identifier: Some(self.identifier.clone()),
            db_capacity_reservation_arn: Some(self.arn()),
            db_instance_class: Some(self.db_instance_class.clone()),
            engine: Some(self.engine.clone()),
            license_model: self.license_model.clone(),
            reserved_capacity_per_availability_zone: Some(
                self.reserved_capacity_per_availability_zone,
            ),
            available_capacity_per_availability_zone_list: Some(
                self.available_capacity(now_as_wire_timestamp()),
            ),
            create_date: Some(self.create_date),
            start_date: self.start_date,
            end_date: self.end_date,
            commitment_info: self.commitment.as_ref().map(|commitment| {
                DBCapacityReservationCommitmentInfo {
                    committed_capacity_per_availability_zone: Some(
                        commitment.committed_capacity_per_availability_zone,
                    ),
                    commitment_end_date: Some(commitment.commitment_end_date),
                }
            }),
            state: Some(self.state.clone()),
            tag_list: Some(self.tags.clone()),
        }
    }

    /// How many DB instances this reservation holds capacity for in the Region: the
    /// capacity reserved in each Availability Zone, in every zone of the reservation.
    ///
    /// "The total reserved capacity is `CapacityPerAvailabilityZone` multiplied by the
    /// number of Availability Zones" — which is what a further create is checked against,
    /// since reserving capacity for a DB instance uses up the Region's DB instance
    /// allowance.
    pub fn reserved_instances(&self) -> usize {
        let per_zone = usize::try_from(self.reserved_capacity_per_availability_zone).unwrap_or(0);
        per_zone.saturating_mul(self.availability_zones.len())
    }
}

impl ComAmazonawsRdsAmazonrdsv19State {
    /// The DB capacity reservations of `region`, in identifier order.
    ///
    /// One region's ordered range, so the work a report does is bounded by that region's
    /// records rather than by what other callers stored elsewhere.
    pub fn capacity_reservations_in(&self, region: &str) -> Vec<&DbCapacityReservationRecord> {
        range_in(&self.db_capacity_reservations, region)
    }

    /// The reservation `named` addresses in `region`, or `None` where nothing there answers
    /// to it.
    ///
    /// `DescribeDBCapacityReservationsRequest$DBCapacityReservationIdentifier` is "the
    /// identifier or the name of the DB capacity reservation ... you can specify the
    /// identifier, which has the prefix `dbcr-`, or the reservation name", so both are
    /// resolved here: the identifier is the store's own key, and a name is placed by the
    /// region's range, in which one name identifies at most one reservation ("this name must
    /// be unique for your Amazon Web Services account in the ... Region").
    pub fn resolve_db_capacity_reservation(
        &self,
        region: &str,
        named: &str,
    ) -> Option<&DbCapacityReservationRecord> {
        if let Some(record) = self
            .db_capacity_reservations
            .get(&(region.to_string(), named.to_string()))
        {
            return Some(record);
        }
        self.capacity_reservations_in(region)
            .into_iter()
            .find(|record| record.name == named)
    }

    /// Whether `region` already holds a reservation under the name `name`.
    pub fn db_capacity_reservation_name_taken(&self, region: &str, name: &str) -> bool {
        self.capacity_reservations_in(region)
            .into_iter()
            .any(|record| record.name == name)
    }

    /// The same reservation [`Self::resolve_db_capacity_reservation`] resolves, held MUTABLY:
    /// what the calls that CHANGE one write through — the modification and the cancel.
    ///
    /// Resolved by the one reading of the two namespaces a reservation answers to, so a
    /// mutation and a report cannot disagree about which reservation a value names; the
    /// record is then taken by its own stored key, which is the identifier that reading
    /// placed.
    pub fn resolve_db_capacity_reservation_mut(
        &mut self,
        region: &str,
        named: &str,
    ) -> Option<&mut DbCapacityReservationRecord> {
        let identifier = self
            .resolve_db_capacity_reservation(region, named)?
            .identifier
            .clone();
        self.db_capacity_reservations
            .get_mut(&(region.to_string(), identifier))
    }

    /// How many DB instances the reservations of `region` already hold capacity for
    /// ([`DbCapacityReservationRecord::reserved_instances`]), over every reservation that
    /// still holds any — a reservation Amazon RDS has released the capacity of holds none.
    pub fn reserved_db_instances_in(&self, region: &str) -> usize {
        self.capacity_reservations_in(region)
            .into_iter()
            .filter(|record| !record.is_terminal())
            .map(DbCapacityReservationRecord::reserved_instances)
            .fold(0, usize::saturating_add)
    }
}
