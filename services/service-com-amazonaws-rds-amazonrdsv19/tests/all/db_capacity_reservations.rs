// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for this service's DB capacity reservation slice:
//! `CreateDBCapacityReservation` and the `DescribeDBCapacityReservations` report that reads
//! what it persisted.
//!
//! Every call goes over the wire: the service is mounted on a `MultiServiceRouter` exactly
//! as the production router mounts it (through `db_instances::emulator`, the one mounting
//! helper this test target has), and each request is a real awsQuery form POST carrying the
//! SigV4 credential scope the router keys on. The assertions read the XML the emulator
//! answers with, so a handler that computed nothing could not pass them.
//!
//! The rules under test:
//!
//! * a DB capacity reservation a create persisted is reported by every later read of the
//!   region it was created in — by the identifier that create minted and by the name it was
//!   given — with the configuration that create resolved, and a lookup answers the modelled
//!   not-found fault only for a value nothing there answers to;
//! * the report applies each filter its member documents, matching ANY value of one filter
//!   and EVERY filter of a request, and refuses a filter it cannot apply rather than
//!   ignoring it;
//! * a reservation reports available capacity only while it is `active` and not past its
//!   end date, and reaches the states its own clocks owe it.
//!
//! The tables below span what those rules generalise over: one zone, two zones and every
//! zone a Region publishes; capacities of one and above one; the engine families capacity
//! can be reserved for, with and without a license model; reservations with no tags, one
//! and two; an immediate reservation and a future-dated one at each published bound of its
//! commitment; each documented filter name with one and with several values and two filters
//! together; every name the filter list does not carry; both published bounds of
//! `MaxRecords` plus one value past each; a page boundary and the marker that resumes it;
//! the region scope; and the Region's reservable-instance allowance plus one create past
//! it.

use std::time::{SystemTime, UNIX_EPOCH};

use service_com_amazonaws_rds_amazonrdsv19::state::{
    DB_CAPACITY_RESERVATION_ID_PREFIX, DB_CAPACITY_RESERVATION_QUOTA_PER_REGION,
    DB_INSTANCE_QUOTA_PER_REGION, MAX_COMMITMENT_DURATION_SECONDS,
    MIN_COMMITMENT_DURATION_SECONDS, MIN_X_CLASS_COMMITMENT_DURATION_SECONDS,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::db_instances::{
    call, call_in, emulator, expect_error, expect_ok, optional_text, text, texts, REGION,
};
use crate::lifecycle_poll_loops::{poll_until, read_instance};

/// The canonical account id every generated emulator presents, which the ARNs below are
/// read against.
const ACCOUNT: &str = "000000000000";

/// A second region, so the regional scope of a DB capacity reservation is observable.
const OTHER_REGION: &str = "eu-west-1";

/// The state an accepted create of a reservation that starts immediately reports.
const PENDING: &str = "pending";

/// The state that reservation reaches once the transition this service owes it settles.
const ACTIVE: &str = "active";

/// The state an accepted create of a FUTURE-DATED reservation reports.
const ASSESSING: &str = "assessing";

/// The state a future-dated reservation reaches once this service has accepted it.
const SCHEDULED: &str = "scheduled";

/// The state a reservation reaches once it is past its own end date.
const EXPIRED: &str = "expired";

/// The seconds since the epoch now, the instant the date members below are placed against.
fn now_in_seconds() -> i64 {
    let since_epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("a clock at or after the epoch");
    i64::try_from(since_epoch.as_secs()).expect("an epoch second inside i64")
}

/// `seconds` since the epoch in the UTC form the `TStamp` members take
/// (`2026-12-31T23:59:59Z`).
fn wire_instant(seconds: i64) -> String {
    ::aws_smithy_types::DateTime::from_secs(seconds)
        .fmt(::aws_smithy_types::date_time::Format::DateTime)
        .expect("an instant renders in the UTC form a TStamp member takes")
}

/// The seconds in a day, the unit the `StartDate` window is published in.
const SECONDS_PER_DAY: i64 = 24 * 60 * 60;

/// One row of the create/describe round trip.
struct ReservationCase {
    case: &'static str,
    /// The `DBCapacityReservationName` the create names.
    name: &'static str,
    engine: &'static str,
    class: &'static str,
    /// `CapacityPerAvailabilityZone`.
    capacity: i32,
    /// `AvailabilityZones`, in the order the create lists them.
    zones: &'static [&'static str],
    /// The `LicenseModel` the create names, if any.
    license_model: Option<&'static str>,
    /// The tags the create names.
    tags: &'static [(&'static str, &'static str)],
    /// How many days ahead the `EndDate` falls, for a create that names one.
    end_date_in_days: Option<i64>,
    /// `(seconds ahead of now the StartDate falls, CommitmentDuration in seconds)` for a
    /// future-dated row.
    ///
    /// The start is held in SECONDS rather than days because the window it must fall in is
    /// measured from the instant the request is SERVED, which is a moment after the instant
    /// a row is placed against: a row at exactly the bound would be a moment outside it, so
    /// each bound is approached to the minute instead.
    future_dated: Option<(i64, i64)>,
}

impl ReservationCase {
    /// The state this create is accepted in: `assessing` while Amazon RDS evaluates a
    /// future-dated request, `pending` while it brings an immediate reservation's capacity
    /// up.
    fn accepted_state(&self) -> &'static str {
        if self.future_dated.is_some() {
            ASSESSING
        } else {
            PENDING
        }
    }

    /// The state the clock owes it from there.
    fn settled_state(&self) -> &'static str {
        if self.future_dated.is_some() {
            SCHEDULED
        } else {
            ACTIVE
        }
    }
}

/// The DB capacity reservations the round trip creates and reads back.
///
/// The rows differ along what the rule generalises over: one zone, two zones and every zone
/// `us-west-2` publishes; a capacity of one and capacities above it; a reservation with no
/// tags, one tag and two; an engine that takes no license model and three editions that do,
/// each under a license model its own engine publishes; a reservation that does not expire
/// and one that does; and both published bounds of a future-dated reservation's commitment
/// — the general minimum on a `db.m` class of the earliest generation it admits, and the
/// `db.x` minimum at the far end of the `StartDate` window.
const RESERVATION_CASES: &[ReservationCase] = &[
    ReservationCase {
        case: "nothing but the required members",
        name: "emu-reservation-minimal",
        engine: "mysql",
        class: "db.m5.large",
        capacity: 1,
        zones: &["us-west-2a"],
        license_model: None,
        tags: &[],
        end_date_in_days: None,
        future_dated: None,
    },
    ReservationCase {
        case: "two zones, a capacity above one and one tag",
        name: "emu-reservation-two-zones",
        engine: "postgres",
        class: "db.r7g.xlarge",
        capacity: 3,
        zones: &["us-west-2a", "us-west-2b"],
        license_model: None,
        tags: &[("owner", "platform")],
        end_date_in_days: None,
        future_dated: None,
    },
    ReservationCase {
        case: "every zone of the region, an end date and two tags",
        name: "emu-reservation-every-zone",
        engine: "mariadb",
        class: "db.t3.medium",
        capacity: 2,
        zones: &["us-west-2a", "us-west-2b", "us-west-2c", "us-west-2d"],
        license_model: None,
        tags: &[("owner", "platform"), ("cost-centre", "db")],
        end_date_in_days: Some(30),
        future_dated: None,
    },
    ReservationCase {
        case: "an Oracle edition under its own license model",
        name: "emu-reservation-oracle",
        engine: "oracle-se2",
        class: "db.m5.large",
        capacity: 1,
        zones: &["us-west-2c"],
        license_model: Some("bring-your-own-license"),
        tags: &[],
        end_date_in_days: None,
        future_dated: None,
    },
    ReservationCase {
        case: "a SQL Server edition under license-included",
        name: "emu-reservation-sqlserver",
        engine: "sqlserver-se",
        class: "db.m5.large",
        capacity: 1,
        zones: &["us-west-2d"],
        license_model: Some("license-included"),
        tags: &[],
        end_date_in_days: None,
        future_dated: None,
    },
    ReservationCase {
        case: "a future-dated reservation at the earliest start and the shortest commitment",
        name: "emu-reservation-future",
        engine: "aurora-mysql",
        class: "db.m7g.4xlarge",
        capacity: 2,
        zones: &["us-west-2a"],
        license_model: None,
        tags: &[("owner", "platform")],
        end_date_in_days: None,
        future_dated: Some((5 * SECONDS_PER_DAY + 60, MIN_COMMITMENT_DURATION_SECONDS)),
    },
    ReservationCase {
        case: "a future-dated db.x reservation at the latest start and its own commitment",
        name: "emu-reservation-future-x",
        engine: "mysql",
        class: "db.x2g.xlarge",
        capacity: 8,
        zones: &["us-west-2b"],
        license_model: None,
        tags: &[],
        end_date_in_days: None,
        future_dated: Some((
            120 * SECONDS_PER_DAY - 60,
            MIN_X_CLASS_COMMITMENT_DURATION_SECONDS,
        )),
    },
];

/// The request members one row of [`RESERVATION_CASES`] is created with, placed against
/// `now`.
fn create_members(row: &ReservationCase, now: i64) -> Vec<(String, String)> {
    let mut members = vec![
        (
            "DBCapacityReservationName".to_string(),
            row.name.to_string(),
        ),
        ("Engine".to_string(), row.engine.to_string()),
        ("DBInstanceClass".to_string(), row.class.to_string()),
        (
            "CapacityPerAvailabilityZone".to_string(),
            row.capacity.to_string(),
        ),
    ];
    for (index, zone) in row.zones.iter().enumerate() {
        members.push((
            format!("AvailabilityZones.AvailabilityZone.{}", index + 1),
            (*zone).to_string(),
        ));
    }
    if let Some(license_model) = row.license_model {
        members.push(("LicenseModel".to_string(), license_model.to_string()));
    }
    for (index, (key, value)) in row.tags.iter().enumerate() {
        members.push((format!("Tags.Tag.{}.Key", index + 1), (*key).to_string()));
        members.push((format!("Tags.Tag.{}.Value", index + 1), (*value).to_string()));
    }
    if let Some((ahead, duration)) = row.future_dated {
        members.push(("StartDate".to_string(), wire_instant(now + ahead)));
        members.push(("CommitmentDuration".to_string(), duration.to_string()));
    }
    if let Some(days) = row.end_date_in_days {
        members.push((
            "EndDate".to_string(),
            wire_instant(now + days * SECONDS_PER_DAY),
        ));
    }
    members
}

/// `CreateDBCapacityReservation` with the members `extra` names, against `region`.
async fn create_in(
    router: &MultiServiceRouter,
    region: &str,
    extra: &[(String, String)],
) -> (u16, String) {
    call_in(router, region, "CreateDBCapacityReservation", extra).await
}

/// A create of one reservation of `capacity` DB instances in one zone, named `name`: the
/// smallest reservation there is, which is what the paging and allowance suites below need
/// many of.
async fn create_one_zone(
    router: &MultiServiceRouter,
    region: &str,
    name: &str,
    capacity: i32,
) -> (u16, String) {
    create_in(
        router,
        region,
        &[
            ("DBCapacityReservationName".to_string(), name.to_string()),
            ("Engine".to_string(), "mysql".to_string()),
            ("DBInstanceClass".to_string(), "db.m5.large".to_string()),
            (
                "CapacityPerAvailabilityZone".to_string(),
                capacity.to_string(),
            ),
            (
                "AvailabilityZones.AvailabilityZone.1".to_string(),
                format!("{region}a"),
            ),
        ],
    )
    .await
}

/// `DescribeDBCapacityReservations` for the one reservation `named` addresses, as the HTTP
/// status and the body.
///
/// A function rather than a closure body so the future it answers OWNS the parameters it was
/// built from and a poll loop can call it again.
async fn read_reservation(
    router: &MultiServiceRouter,
    region: &str,
    named: &str,
) -> (u16, String) {
    call_in(
        router,
        region,
        "DescribeDBCapacityReservations",
        &[("DBCapacityReservationIdentifier", named)],
    )
    .await
}

/// The `State` the one reservation a body reports carries.
fn reported_state(body: &str) -> String {
    text(body, "State")
}

#[tokio::test]
async fn create_persists_every_member_and_describe_reads_it_back() {
    let router = emulator();
    let now = now_in_seconds();
    let mut minted: Vec<(&str, String)> = Vec::new();

    for row in RESERVATION_CASES {
        let members = create_members(row, now);
        let created = expect_ok(row.case, create_in(&router, REGION, &members).await);

        // The identifier is MINTED by the service, under the prefix the member documents,
        // and is not the name the create chose.
        let identifier = text(&created, "DBCapacityReservationIdentifier");
        assert!(
            identifier.starts_with(DB_CAPACITY_RESERVATION_ID_PREFIX),
            "case {:?}: expected an identifier under the {DB_CAPACITY_RESERVATION_ID_PREFIX} \
             prefix, got {identifier:?} in {created}",
            row.case
        );
        assert_ne!(
            identifier, row.name,
            "case {:?}: the identifier must be minted, not the name the create chose, got \
             {created}",
            row.case
        );
        assert_eq!(
            text(&created, "DBCapacityReservationName"),
            row.name,
            "case {:?}: expected the name the create chose, got {created}",
            row.case
        );
        assert_eq!(
            text(&created, "DBCapacityReservationArn"),
            format!("arn:aws:rds:{REGION}:{ACCOUNT}:db-capacity-reservation:{identifier}"),
            "case {:?}: expected the ARN of the minted identifier in the region the create \
             was signed for, got {created}",
            row.case
        );
        assert_eq!(
            text(&created, "Engine"),
            row.engine,
            "case {:?}: expected the engine the create named, got {created}",
            row.case
        );
        assert_eq!(
            text(&created, "DBInstanceClass"),
            row.class,
            "case {:?}: expected the DB instance class the create named, got {created}",
            row.case
        );
        assert_eq!(
            optional_text(&created, "LicenseModel").as_deref(),
            row.license_model,
            "case {:?}: expected the license model the create named and none otherwise, got \
             {created}",
            row.case
        );
        assert_eq!(
            text(&created, "ReservedCapacityPerAvailabilityZone"),
            row.capacity.to_string(),
            "case {:?}: expected the capacity the create reserved in each zone, got {created}",
            row.case
        );
        assert_eq!(
            texts(&created, "AvailabilityZone"),
            row.zones
                .iter()
                .map(|zone| (*zone).to_string())
                .collect::<Vec<String>>(),
            "case {:?}: expected one entry per Availability Zone of the reservation, got \
             {created}",
            row.case
        );
        // "Amazon RDS reports available capacity only for a reservation that is in the
        // active state and not past its end date. Otherwise, each entry reports an available
        // capacity of 0" — and an accepted create is not active yet.
        assert_eq!(
            texts(&created, "AvailableCapacity"),
            vec!["0".to_string(); row.zones.len()],
            "case {:?}: expected no available capacity before the reservation is active, got \
             {created}",
            row.case
        );
        assert_eq!(
            reported_state(&created),
            row.accepted_state(),
            "case {:?}: expected the state an accepted create reports, got {created}",
            row.case
        );
        assert!(
            !text(&created, "CreateDate").is_empty(),
            "case {:?}: expected the instant the reservation was created, got {created}",
            row.case
        );
        assert_eq!(
            optional_text(&created, "EndDate"),
            row.end_date_in_days
                .map(|days| wire_instant(now + days * SECONDS_PER_DAY)),
            "case {:?}: expected the end date the create named and none otherwise, got \
             {created}",
            row.case
        );
        // "Amazon RDS doesn't return this value for a reservation that started
        // immediately", and it "returns this object only for a future-dated reservation".
        let start_date = row.future_dated.map(|(ahead, _)| wire_instant(now + ahead));
        assert_eq!(
            optional_text(&created, "StartDate"),
            start_date,
            "case {:?}: expected a start date only for a future-dated reservation, got \
             {created}",
            row.case
        );
        assert_eq!(
            optional_text(&created, "CommitmentEndDate"),
            row.future_dated
                .map(|(ahead, duration)| wire_instant(now + ahead + duration)),
            "case {:?}: expected the commitment to end at StartDate plus CommitmentDuration, \
             got {created}",
            row.case
        );
        assert_eq!(
            optional_text(&created, "CommittedCapacityPerAvailabilityZone"),
            row.future_dated.map(|_| row.capacity.to_string()),
            "case {:?}: expected the committed capacity of a future-dated reservation, got \
             {created}",
            row.case
        );
        let reported_tags: Vec<(String, String)> = texts(&created, "Key")
            .into_iter()
            .zip(texts(&created, "Value"))
            .collect();
        let expected_tags: Vec<(String, String)> = row
            .tags
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect();
        assert_eq!(
            reported_tags, expected_tags,
            "case {:?}: expected the tags the create named, got {created}",
            row.case
        );

        minted.push((row.name, identifier));
    }

    // Every reservation is read back by the identifier the create minted, in the state the
    // clock owed it, with the available capacity that state reports.
    for (row, (_, identifier)) in RESERVATION_CASES.iter().zip(&minted) {
        let (settled, _) = poll_until(
            row.case,
            "the reservation",
            || read_reservation(&router, REGION, identifier),
            |status, body| status == 200 && reported_state(body) == row.settled_state(),
        )
        .await;
        assert_eq!(
            text(&settled, "DBCapacityReservationName"),
            row.name,
            "case {:?}: expected the lookup by identifier to report the reservation the \
             create stored, got {settled}",
            row.case
        );
        let available = if row.settled_state() == ACTIVE {
            row.capacity.to_string()
        } else {
            "0".to_string()
        };
        assert_eq!(
            texts(&settled, "AvailableCapacity"),
            vec![available; row.zones.len()],
            "case {:?}: expected every reserved unit of an active reservation to be \
             available and none otherwise, got {settled}",
            row.case
        );

        // "You can specify the identifier, which has the prefix dbcr-, or the reservation
        // name": both address the same reservation.
        let by_name = expect_ok(row.case, read_reservation(&router, REGION, row.name).await);
        assert_eq!(
            text(&by_name, "DBCapacityReservationIdentifier"),
            *identifier,
            "case {:?}: expected the lookup by name to report the same reservation as the \
             lookup by identifier, got {by_name}",
            row.case
        );
    }

    // "If you don't specify a DBCapacityReservationIdentifier, this operation returns
    // information about all DB capacity reservations for your Amazon Web Services account."
    let all = expect_ok(
        "the whole report",
        call(
            &router,
            "DescribeDBCapacityReservations",
            &[("MaxRecords", "100")],
        )
        .await,
    );
    let mut reported = texts(&all, "DBCapacityReservationName");
    reported.sort();
    let mut expected: Vec<String> = RESERVATION_CASES
        .iter()
        .map(|row| row.name.to_string())
        .collect();
    expected.sort();
    assert_eq!(
        reported, expected,
        "expected every created reservation in the unnamed report, once each, got {all}"
    );
}

#[tokio::test]
async fn a_value_nothing_created_is_the_modelled_not_found_fault() {
    let router = emulator();

    // An empty store answers the unnamed report with an empty list and a named lookup with
    // the modelled 404 — never the other way round.
    let empty = expect_ok(
        "the report of a region holding no reservation",
        call(&router, "DescribeDBCapacityReservations", &[("MaxRecords", "20")]).await,
    );
    assert!(
        texts(&empty, "DBCapacityReservationName").is_empty(),
        "expected an empty report where nothing was created, got {empty}"
    );

    let row = &RESERVATION_CASES[0];
    let created = expect_ok(
        row.case,
        create_in(&router, REGION, &create_members(row, now_in_seconds())).await,
    );
    let identifier = text(&created, "DBCapacityReservationIdentifier");
    expect_ok(
        "the reservation of this region",
        read_reservation(&router, REGION, &identifier).await,
    );

    // The rows differ along what the rule generalises over: an identifier of the minted
    // form, a name, the shortest value the member admits, a name that differs from a stored
    // one by one character, and a reservation that exists in ANOTHER region.
    let absent: &[(&str, &str)] = &[
        ("an identifier nothing minted", "dbcr-00000000000000000000000000"),
        ("a name nothing created", "emu-no-such-reservation"),
        ("the shortest value the member admits", "a"),
        ("a name one character off a stored one", "emu-reservation-minima"),
    ];
    for (case, named) in absent {
        let refused = expect_error(
            case,
            read_reservation(&router, REGION, named).await,
            404,
            "DBCapacityReservationNotFound",
        );
        assert_eq!(
            text(&refused, "Message"),
            format!("DBCapacityReservation {named} not found."),
            "case {case:?}: expected the fault to name the value the request carried, got \
             {refused}"
        );
    }

    // RDS is regional: the reservation of one region is not in another region's report, and
    // a lookup of it there is the same 404.
    let elsewhere = expect_ok(
        "the report of another region",
        call_in(
            &router,
            OTHER_REGION,
            "DescribeDBCapacityReservations",
            &[("MaxRecords", "20")],
        )
        .await,
    );
    assert!(
        texts(&elsewhere, "DBCapacityReservationName").is_empty(),
        "expected the reservation of {REGION} to be absent from {OTHER_REGION}, got {elsewhere}"
    );
    expect_error(
        "the identifier of another region's reservation",
        read_reservation(&router, OTHER_REGION, &identifier).await,
        404,
        "DBCapacityReservationNotFound",
    );
}

/// One row of the filter table: the filter members of one request and the reservation names
/// the report must carry for it.
struct FilterCase {
    case: &'static str,
    /// `[(Name, [Values..]), ..]` — one entry per `Filters.Filter.<n>`.
    filters: &'static [(&'static str, &'static [&'static str])],
    /// The names the report carries, in any order.
    expected: &'static [&'static str],
}

/// The `Filters.Filter.<n>` request members of one row.
fn filter_members(filters: &[(&str, &[&str])]) -> Vec<(String, String)> {
    let mut members = Vec::new();
    for (index, (name, values)) in filters.iter().enumerate() {
        members.push((
            format!("Filters.Filter.{}.Name", index + 1),
            (*name).to_string(),
        ));
        for (position, value) in values.iter().enumerate() {
            members.push((
                format!("Filters.Filter.{}.Values.Value.{}", index + 1, position + 1),
                (*value).to_string(),
            ));
        }
    }
    members
}

/// The rows the filter report is driven over: each documented filter name, one value and
/// several, two filters together, and a value nothing matches.
const FILTER_CASES: &[FilterCase] = &[
    FilterCase {
        case: "one name",
        filters: &[("db-capacity-reservation-name", &["emu-filter-mysql"])],
        expected: &["emu-filter-mysql"],
    },
    FilterCase {
        case: "two names, which the report matches either of",
        filters: &[(
            "db-capacity-reservation-name",
            &["emu-filter-mysql", "emu-filter-oracle"],
        )],
        expected: &["emu-filter-mysql", "emu-filter-oracle"],
    },
    FilterCase {
        case: "a name nothing answers to",
        filters: &[("db-capacity-reservation-name", &["emu-filter-absent"])],
        expected: &[],
    },
    FilterCase {
        case: "the state every reservation has settled in",
        filters: &[("state", &["active"])],
        expected: &["emu-filter-mysql", "emu-filter-postgres", "emu-filter-oracle"],
    },
    FilterCase {
        case: "a state none of them is in",
        filters: &[("state", &["pending"])],
        expected: &[],
    },
    FilterCase {
        case: "one engine",
        filters: &[("engine", &["postgres"])],
        expected: &["emu-filter-postgres"],
    },
    FilterCase {
        case: "two engines",
        filters: &[("engine", &["mysql", "oracle-se2"])],
        expected: &["emu-filter-mysql", "emu-filter-oracle"],
    },
    FilterCase {
        case: "one DB instance class, which two of them reserve",
        filters: &[("db-instance-class", &["db.m5.large"])],
        expected: &["emu-filter-mysql", "emu-filter-oracle"],
    },
    FilterCase {
        case: "an Availability Zone two reservations cover",
        filters: &[("availability-zone", &["us-west-2a"])],
        expected: &["emu-filter-mysql", "emu-filter-postgres"],
    },
    FilterCase {
        case: "an Availability Zone one reservation covers as its second zone",
        filters: &[("availability-zone", &["us-west-2b"])],
        expected: &["emu-filter-postgres"],
    },
    FilterCase {
        case: "two filters, which the report matches both of",
        filters: &[
            ("engine", &["mysql", "postgres"]),
            ("availability-zone", &["us-west-2b"]),
        ],
        expected: &["emu-filter-postgres"],
    },
    FilterCase {
        case: "two filters nothing matches both of",
        filters: &[
            ("engine", &["mysql"]),
            ("availability-zone", &["us-west-2b"]),
        ],
        expected: &[],
    },
];

#[tokio::test]
async fn the_report_applies_every_documented_filter() {
    let router = emulator();
    // Three reservations whose engines, classes and zones differ, so each filter name has
    // something to include and something to leave out.
    let fixtures: &[(&str, &str, &str, &[&str])] = &[
        ("emu-filter-mysql", "mysql", "db.m5.large", &["us-west-2a"]),
        (
            "emu-filter-postgres",
            "postgres",
            "db.r7g.xlarge",
            &["us-west-2a", "us-west-2b"],
        ),
        (
            "emu-filter-oracle",
            "oracle-se2",
            "db.m5.large",
            &["us-west-2c"],
        ),
    ];
    for (name, engine, class, zones) in fixtures {
        let mut members = vec![
            ("DBCapacityReservationName".to_string(), (*name).to_string()),
            ("Engine".to_string(), (*engine).to_string()),
            ("DBInstanceClass".to_string(), (*class).to_string()),
            ("CapacityPerAvailabilityZone".to_string(), "1".to_string()),
        ];
        for (index, zone) in zones.iter().enumerate() {
            members.push((
                format!("AvailabilityZones.AvailabilityZone.{}", index + 1),
                (*zone).to_string(),
            ));
        }
        expect_ok(name, create_in(&router, REGION, &members).await);
        // Every row of the table below reads a settled reservation, so each one is polled to
        // the state its create owed it before the report is driven.
        poll_until(
            name,
            "the reservation",
            || read_reservation(&router, REGION, name),
            |status, body| status == 200 && reported_state(body) == ACTIVE,
        )
        .await;
    }

    for row in FILTER_CASES {
        let mut members = filter_members(row.filters);
        members.push(("MaxRecords".to_string(), "100".to_string()));
        let reported_body = expect_ok(
            row.case,
            call(&router, "DescribeDBCapacityReservations", &members).await,
        );
        let mut reported = texts(&reported_body, "DBCapacityReservationName");
        reported.sort();
        let mut expected: Vec<String> =
            row.expected.iter().map(|name| (*name).to_string()).collect();
        expected.sort();
        assert_eq!(
            reported, expected,
            "case {:?}: expected the reservations the filters admit, got {reported_body}",
            row.case
        );
    }

    // "Amazon RDS rejects the request when a filter name isn't in this list, or when a
    // filter specifies no values", and "the state filter doesn't accept expired".
    /// `(case, the `Filters` entries of one request, the sentence the refusal carries or
    /// `None` where the row pins only its error code)`.
    type FilterRefusal = (
        &'static str,
        &'static [(&'static str, &'static [&'static str])],
        Option<&'static str>,
    );
    let refused: &[FilterRefusal] = &[
        (
            "a filter name the list does not carry",
            &[("db-capacity-reservation-id", &["anything"])],
            Some("Unrecognized filter name: db-capacity-reservation-id"),
        ),
        (
            "a supported name in another case",
            &[("State", &["active"])],
            Some("Unrecognized filter name: State"),
        ),
        (
            "a filter that specifies no values",
            &[("engine", &[])],
            None,
        ),
        ("the state the filter excludes", &[("state", &["expired"])], None),
        ("a state no reservation can be in", &[("state", &["retired"])], None),
    ];
    for (case, filters, message) in refused {
        let body = expect_error(
            case,
            call(
                &router,
                "DescribeDBCapacityReservations",
                &filter_members(filters),
            )
            .await,
            400,
            "InvalidParameterValue",
        );
        if let Some(message) = message {
            assert_eq!(
                text(&body, "Message"),
                *message,
                "case {case:?}: expected the refusal to name the filter the request \
                 carried, got {body}"
            );
        }
    }
}

#[tokio::test]
async fn the_report_pages_at_max_records_and_resumes_from_its_marker() {
    let router = emulator();
    // One reservation more than the smallest page the member admits, so there IS a page
    // boundary to observe.
    let smallest_page = 20usize;
    let count = smallest_page + 1;
    let mut names: Vec<String> = Vec::new();
    for index in 0..count {
        let name = format!("emu-paged-{index:02}");
        expect_ok(&name, create_one_zone(&router, REGION, &name, 1).await);
        names.push(name);
    }
    names.sort();

    let first = expect_ok(
        "the first page",
        call(
            &router,
            "DescribeDBCapacityReservations",
            &[("MaxRecords", smallest_page.to_string())],
        )
        .await,
    );
    let mut paged = texts(&first, "DBCapacityReservationName");
    assert_eq!(
        paged.len(),
        smallest_page,
        "expected a page of {smallest_page} reservations, got {first}"
    );
    let marker = text(&first, "Marker");
    assert!(
        !marker.is_empty(),
        "expected a marker while records remain, got {first}"
    );

    let second = expect_ok(
        "the page the marker resumes",
        call(
            &router,
            "DescribeDBCapacityReservations",
            &[
                ("MaxRecords".to_string(), smallest_page.to_string()),
                ("Marker".to_string(), marker),
            ],
        )
        .await,
    );
    paged.extend(texts(&second, "DBCapacityReservationName"));
    assert_eq!(
        optional_text(&second, "Marker"),
        None,
        "expected no marker once the report is exhausted, got {second}"
    );
    paged.sort();
    assert_eq!(
        paged, names,
        "expected the two pages to be every reservation, once each, got {paged:?}"
    );

    // The largest page the member admits carries the whole report and hands out no marker.
    let whole = expect_ok(
        "the largest page the member admits",
        call(
            &router,
            "DescribeDBCapacityReservations",
            &[("MaxRecords", "100")],
        )
        .await,
    );
    assert_eq!(
        texts(&whole, "DBCapacityReservationName").len(),
        count,
        "expected the whole report in one page of 100, got {whole}"
    );
    assert_eq!(
        optional_text(&whole, "Marker"),
        None,
        "expected no marker where the page holds every record, got {whole}"
    );

    // One value past each published bound of `MaxRecords`, and a token this report's codec
    // could not have produced.
    for (case, max_records) in [("one below the minimum", "19"), ("one above the maximum", "101")] {
        expect_error(
            case,
            call(
                &router,
                "DescribeDBCapacityReservations",
                &[("MaxRecords", max_records)],
            )
            .await,
            400,
            "InvalidParameterValue",
        );
    }
    let refused = expect_error(
        "a marker this report could not have handed out",
        call(
            &router,
            "DescribeDBCapacityReservations",
            &[("Marker", "not-a-marker-this-report-minted")],
        )
        .await,
        400,
        "InvalidParameterValue",
    );
    assert_eq!(
        text(&refused, "Message"),
        "Invalid marker. Please use the marker returned from a prior request or omit the \
         marker parameter.",
        "expected the invalid-marker sentence, got {refused}"
    );
}

#[tokio::test]
async fn the_reservable_instance_allowance_is_the_modelled_limit() {
    let router = emulator();
    let allowance = i32::try_from(DB_INSTANCE_QUOTA_PER_REGION).expect("an allowance in i32");

    // One reservation that uses the whole allowance of the region is accepted; the next DB
    // instance reserved there is not.
    expect_ok(
        "a reservation of the whole allowance",
        create_one_zone(&router, REGION, "emu-quota-whole", allowance).await,
    );
    expect_error(
        "one DB instance past the allowance",
        create_one_zone(&router, REGION, "emu-quota-past", 1).await,
        400,
        "InstanceQuotaExceeded",
    );
    // The allowance is over the TOTAL reserved capacity — "CapacityPerAvailabilityZone
    // multiplied by the number of Availability Zones" — so a request past it in one call is
    // refused the same way.
    expect_error(
        "one request past the allowance over two zones",
        create_in(
            &router,
            OTHER_REGION,
            &[
                (
                    "DBCapacityReservationName".to_string(),
                    "emu-quota-two-zones".to_string(),
                ),
                ("Engine".to_string(), "mysql".to_string()),
                ("DBInstanceClass".to_string(), "db.m5.large".to_string()),
                (
                    "CapacityPerAvailabilityZone".to_string(),
                    (allowance / 2 + 1).to_string(),
                ),
                (
                    "AvailabilityZones.AvailabilityZone.1".to_string(),
                    format!("{OTHER_REGION}a"),
                ),
                (
                    "AvailabilityZones.AvailabilityZone.2".to_string(),
                    format!("{OTHER_REGION}b"),
                ),
            ],
        )
        .await,
        400,
        "InstanceQuotaExceeded",
    );
    // The allowance is per region, so another region still takes its own reservations.
    expect_ok(
        "the first reservation of another region",
        create_one_zone(&router, OTHER_REGION, "emu-quota-elsewhere", allowance).await,
    );
}

#[tokio::test]
async fn a_region_holds_at_most_its_record_allowance_whatever_state_each_is_in() {
    let router = emulator();
    let now = now_in_seconds();
    // A reservation that expires releases its reserved capacity, so the reserved-instance
    // allowance is free again — but its RECORD is still reported, and the region's record
    // allowance counts it. Filling the region with records that have all expired is what
    // tells the two caps apart.
    let end_date = wire_instant(now + 2);
    for index in 0..DB_CAPACITY_RESERVATION_QUOTA_PER_REGION {
        let name = format!("emu-record-{index:02}");
        expect_ok(
            &name,
            create_in(
                &router,
                REGION,
                &[
                    ("DBCapacityReservationName".to_string(), name.clone()),
                    ("Engine".to_string(), "mysql".to_string()),
                    ("DBInstanceClass".to_string(), "db.m5.large".to_string()),
                    ("CapacityPerAvailabilityZone".to_string(), "1".to_string()),
                    (
                        "AvailabilityZones.AvailabilityZone.1".to_string(),
                        "us-west-2a".to_string(),
                    ),
                    ("EndDate".to_string(), end_date.clone()),
                ],
            )
            .await,
        );
    }
    // Every record has expired once the last one's end date passes, which the report is
    // polled for rather than waited out.
    let (expired, _) = poll_until(
        "a region full of expired reservations",
        "every end date",
        || {
            call_in(
                &router,
                REGION,
                "DescribeDBCapacityReservations",
                &[("MaxRecords", "100")],
            )
        },
        |status, body| {
            status == 200
                && texts(body, "State")
                    .iter()
                    .all(|state| state == EXPIRED)
        },
    )
    .await;
    assert_eq!(
        texts(&expired, "DBCapacityReservationName").len(),
        DB_CAPACITY_RESERVATION_QUOTA_PER_REGION,
        "expected every created reservation to still be reported after it expired, got \
         {expired}"
    );
    // The capacity they reserved is released, so only the record allowance can refuse the
    // next create — and it does.
    expect_error(
        "one record past the region's allowance",
        create_one_zone(&router, REGION, "emu-record-past", 1).await,
        400,
        "InstanceQuotaExceeded",
    );
    // And it is per region, like every other allowance of this store.
    expect_ok(
        "the first record of another region",
        create_one_zone(&router, OTHER_REGION, "emu-record-elsewhere", 1).await,
    );
}

/// One row of the create-refusal table.
struct RefusalCase {
    case: &'static str,
    /// The members this request carries beside the four every create needs.
    members: &'static [(&'static str, &'static str)],
    /// The members it REPLACES among those four, so a row can send a bad one.
    replaced: &'static [(&'static str, &'static str)],
    status: u16,
    code: &'static str,
}

/// The rows a create is refused over: one for each constraint the operation's own members
/// document, so the refusal is the documented one rather than a stored record no read could
/// render.
const REFUSAL_CASES: &[RefusalCase] = &[
    RefusalCase {
        case: "a name under the identifier prefix",
        members: &[],
        replaced: &[("DBCapacityReservationName", "dbcr-chosen-by-the-caller")],
        status: 400,
        code: "InvalidParameterValue",
    },
    RefusalCase {
        case: "a value that is no database engine",
        members: &[],
        replaced: &[("Engine", "not-an-engine")],
        status: 400,
        code: "InvalidParameterValue",
    },
    RefusalCase {
        case: "an engine this front door serves but capacity cannot be reserved for",
        members: &[],
        replaced: &[("Engine", "docdb")],
        status: 400,
        code: "DBCapacityReservationUnsupported",
    },
    RefusalCase {
        case: "a custom engine, which no reservation covers",
        members: &[],
        replaced: &[("Engine", "custom-oracle-ee")],
        status: 400,
        code: "DBCapacityReservationUnsupported",
    },
    RefusalCase {
        case: "a DB instance class of a generation this service publishes no option in",
        members: &[],
        replaced: &[("DBInstanceClass", "db.zz9.large")],
        status: 400,
        code: "DBCapacityReservationUnsupported",
    },
    RefusalCase {
        case: "a capacity that reserves no DB instance",
        members: &[],
        replaced: &[("CapacityPerAvailabilityZone", "0")],
        status: 400,
        code: "InvalidParameterValue",
    },
    RefusalCase {
        case: "a negative capacity",
        members: &[],
        replaced: &[("CapacityPerAvailabilityZone", "-1")],
        status: 400,
        code: "InvalidParameterValue",
    },
    RefusalCase {
        case: "an Availability Zone with no name",
        members: &[],
        replaced: &[("AvailabilityZones.AvailabilityZone.1", "")],
        status: 400,
        code: "InvalidParameterValue",
    },
    RefusalCase {
        case: "a zone of another region",
        members: &[],
        replaced: &[("AvailabilityZones.AvailabilityZone.1", "eu-west-1a")],
        status: 400,
        code: "InvalidParameterValue",
    },
    RefusalCase {
        case: "a zone letter the region does not publish",
        members: &[],
        replaced: &[("AvailabilityZones.AvailabilityZone.1", "us-west-2f")],
        status: 400,
        code: "InvalidParameterValue",
    },
    RefusalCase {
        case: "one zone named twice",
        members: &[("AvailabilityZones.AvailabilityZone.2", "us-west-2a")],
        replaced: &[],
        status: 400,
        code: "InvalidParameterValue",
    },
    RefusalCase {
        case: "a license model for an engine that takes none",
        members: &[("LicenseModel", "general-public-license")],
        replaced: &[],
        status: 400,
        code: "InvalidParameterCombination",
    },
    RefusalCase {
        case: "a license model the engine is not offered under",
        members: &[("LicenseModel", "license-included")],
        replaced: &[("Engine", "oracle-ee")],
        status: 400,
        code: "InvalidParameterCombination",
    },
    RefusalCase {
        case: "a start date with no commitment",
        members: &[("StartDate", "")],
        replaced: &[],
        status: 400,
        code: "InvalidParameterCombination",
    },
    RefusalCase {
        case: "a commitment with no start date",
        members: &[("CommitmentDuration", "1209600")],
        replaced: &[],
        status: 400,
        code: "InvalidParameterCombination",
    },
    RefusalCase {
        case: "a start date before the window opens",
        members: &[("StartDate", ""), ("CommitmentDuration", "1209600")],
        replaced: &[],
        status: 400,
        code: "InvalidParameterValue",
    },
];

#[tokio::test]
async fn a_create_outside_what_the_members_document_is_refused() {
    let router = emulator();
    let now = now_in_seconds();

    for row in REFUSAL_CASES {
        // The four members every create needs, with whatever this row replaces.
        let mut members: Vec<(String, String)> = vec![
            (
                "DBCapacityReservationName".to_string(),
                "emu-refused".to_string(),
            ),
            ("Engine".to_string(), "mysql".to_string()),
            ("DBInstanceClass".to_string(), "db.m5.large".to_string()),
            ("CapacityPerAvailabilityZone".to_string(), "1".to_string()),
            (
                "AvailabilityZones.AvailabilityZone.1".to_string(),
                "us-west-2a".to_string(),
            ),
        ];
        for (member, value) in row.replaced {
            let value = (*value).to_string();
            for held in &mut members {
                if held.0 == *member {
                    held.1 = value.clone();
                }
            }
        }
        // An empty `StartDate` in the table stands for "a date inside the window the member
        // publishes", which only a clock can supply; the rows that test the window itself
        // say which end they fall outside of in their own case name.
        for (member, value) in row.members {
            let value = if *member == "StartDate" && value.is_empty() {
                if row.case.contains("before the window") {
                    wire_instant(now + SECONDS_PER_DAY)
                } else {
                    wire_instant(now + 10 * SECONDS_PER_DAY)
                }
            } else {
                (*value).to_string()
            };
            members.push(((*member).to_string(), value));
        }
        expect_error(
            row.case,
            create_in(&router, REGION, &members).await,
            row.status,
            row.code,
        );
        // Nothing was stored for a refused create, so the name it asked for still answers
        // to nothing.
        expect_error(
            row.case,
            read_reservation(&router, REGION, "emu-refused").await,
            404,
            "DBCapacityReservationNotFound",
        );
    }
}

#[tokio::test]
async fn the_commitment_and_start_windows_are_the_published_ones() {
    let router = emulator();
    let now = now_in_seconds();

    /// `(case, seconds ahead of now the start date falls, commitment seconds, DB instance
    /// class, capacity, the error code the request is refused with or `None` for an accepted
    /// one)`.
    ///
    /// The start is held in SECONDS for the reason [`ReservationCase::future_dated`] gives:
    /// the window is measured from the instant the request is served, so a bound is
    /// approached to the minute rather than sat exactly on.
    type FutureDatedCase = (
        &'static str,
        i64,
        i64,
        &'static str,
        i32,
        Option<&'static str>,
    );
    /// A minute, the margin a row inside a bound of the start window keeps from it.
    const MARGIN: i64 = 60;
    // The rows span each published bound of the two windows and one value past it, the
    // instance-class families a future-dated reservation admits and one it does not, and
    // the reserved-capacity floor either side.
    let cases: &[FutureDatedCase] = &[
        ("just inside the earliest start, at the shortest commitment", 5 * SECONDS_PER_DAY + MARGIN, MIN_COMMITMENT_DURATION_SECONDS, "db.m7g.4xlarge", 2, None),
        ("just inside the latest start", 120 * SECONDS_PER_DAY - MARGIN, MIN_COMMITMENT_DURATION_SECONDS, "db.m7g.4xlarge", 2, None),
        ("a day before the window opens", 4 * SECONDS_PER_DAY, MIN_COMMITMENT_DURATION_SECONDS, "db.m7g.4xlarge", 2, Some("InvalidParameterValue")),
        ("a day after the window closes", 121 * SECONDS_PER_DAY, MIN_COMMITMENT_DURATION_SECONDS, "db.m7g.4xlarge", 2, Some("InvalidParameterValue")),
        ("one second under the shortest commitment", 10 * SECONDS_PER_DAY, MIN_COMMITMENT_DURATION_SECONDS - 1, "db.m7g.4xlarge", 2, Some("InvalidParameterValue")),
        ("the longest commitment", 10 * SECONDS_PER_DAY, MAX_COMMITMENT_DURATION_SECONDS - 1, "db.m7g.4xlarge", 2, None),
        ("the commitment the member excludes", 10 * SECONDS_PER_DAY, MAX_COMMITMENT_DURATION_SECONDS, "db.m7g.4xlarge", 2, Some("InvalidParameterValue")),
        ("a db.x class under its own longer minimum", 10 * SECONDS_PER_DAY, MIN_COMMITMENT_DURATION_SECONDS, "db.x2g.xlarge", 8, Some("InvalidParameterValue")),
        ("a db.x class at its own minimum", 10 * SECONDS_PER_DAY, MIN_X_CLASS_COMMITMENT_DURATION_SECONDS, "db.x2g.xlarge", 8, None),
        ("a db.m class of a generation before the seventh", 10 * SECONDS_PER_DAY, MIN_COMMITMENT_DURATION_SECONDS, "db.m5.8xlarge", 1, Some("DBCapacityReservationUnsupported")),
        ("a db.r class of the seventh generation", 10 * SECONDS_PER_DAY, MIN_COMMITMENT_DURATION_SECONDS, "db.r7g.8xlarge", 1, None),
        ("a family a future-dated reservation does not admit", 10 * SECONDS_PER_DAY, MIN_COMMITMENT_DURATION_SECONDS, "db.z1d.12xlarge", 1, Some("DBCapacityReservationUnsupported")),
        ("one vCPU short of the reserved-capacity floor", 10 * SECONDS_PER_DAY, MIN_COMMITMENT_DURATION_SECONDS, "db.m7g.4xlarge", 1, Some("InvalidParameterCombination")),
        ("a burstable class whose shared vCPUs cannot reach the floor", 10 * SECONDS_PER_DAY, MIN_COMMITMENT_DURATION_SECONDS, "db.t4g.micro", 15, Some("InvalidParameterCombination")),
        ("a burstable class that reaches the floor", 10 * SECONDS_PER_DAY, MIN_COMMITMENT_DURATION_SECONDS, "db.t4g.micro", 16, None),
    ];

    for (index, (case, ahead, duration, class, capacity, refused)) in cases.iter().enumerate() {
        let name = format!("emu-future-{index:02}");
        let members: Vec<(String, String)> = vec![
            ("DBCapacityReservationName".to_string(), name.clone()),
            ("Engine".to_string(), "mysql".to_string()),
            ("DBInstanceClass".to_string(), (*class).to_string()),
            (
                "CapacityPerAvailabilityZone".to_string(),
                capacity.to_string(),
            ),
            (
                "AvailabilityZones.AvailabilityZone.1".to_string(),
                "us-west-2a".to_string(),
            ),
            ("StartDate".to_string(), wire_instant(now + ahead)),
            ("CommitmentDuration".to_string(), duration.to_string()),
        ];
        let answered = create_in(&router, REGION, &members).await;
        match refused {
            Some(code) => {
                expect_error(case, answered, 400, code);
                expect_error(
                    case,
                    read_reservation(&router, REGION, &name).await,
                    404,
                    "DBCapacityReservationNotFound",
                );
            }
            None => {
                let created = expect_ok(case, answered);
                assert_eq!(
                    reported_state(&created),
                    ASSESSING,
                    "case {case:?}: expected a future-dated create to be accepted while this \
                     service evaluates it, got {created}"
                );
                assert_eq!(
                    text(&created, "CommitmentEndDate"),
                    wire_instant(now + ahead + duration),
                    "case {case:?}: expected the commitment to end at StartDate plus the \
                     duration granted, got {created}"
                );
            }
        }
    }
}

#[tokio::test]
async fn a_second_reservation_of_one_name_is_the_modelled_already_exists_fault() {
    let router = emulator();
    expect_ok(
        "the first reservation of this name",
        create_one_zone(&router, REGION, "emu-taken-name", 1).await,
    );
    expect_error(
        "a second reservation of the same name",
        create_one_zone(&router, REGION, "emu-taken-name", 2).await,
        400,
        "DBCapacityReservationAlreadyExists",
    );
    // The first reservation is untouched: the second create stored nothing over it.
    let held = expect_ok(
        "the reservation the first create stored",
        read_reservation(&router, REGION, "emu-taken-name").await,
    );
    assert_eq!(
        text(&held, "ReservedCapacityPerAvailabilityZone"),
        "1",
        "expected the capacity the FIRST create reserved, got {held}"
    );
    // One name identifies one reservation per region, so another region takes it.
    expect_ok(
        "the same name in another region",
        create_one_zone(&router, OTHER_REGION, "emu-taken-name", 1).await,
    );
}

#[tokio::test]
async fn a_reservation_expires_once_its_end_date_passes() {
    let router = emulator();
    let now = now_in_seconds();
    // An end date a poll loop can watch pass: "must be a date and time in the future", and
    // the loop below waits for the STATE rather than for the clock.
    let end_date = wire_instant(now + 2);
    let created = expect_ok(
        "a reservation that expires",
        create_in(
            &router,
            REGION,
            &[
                (
                    "DBCapacityReservationName".to_string(),
                    "emu-expiring".to_string(),
                ),
                ("Engine".to_string(), "mysql".to_string()),
                ("DBInstanceClass".to_string(), "db.m5.large".to_string()),
                ("CapacityPerAvailabilityZone".to_string(), "2".to_string()),
                (
                    "AvailabilityZones.AvailabilityZone.1".to_string(),
                    "us-west-2a".to_string(),
                ),
                (
                    "AvailabilityZones.AvailabilityZone.2".to_string(),
                    "us-west-2b".to_string(),
                ),
                ("EndDate".to_string(), end_date.clone()),
            ],
        )
        .await,
    );
    let identifier = text(&created, "DBCapacityReservationIdentifier");

    let (expired, _) = poll_until(
        "a reservation that expires",
        "the end date",
        || read_reservation(&router, REGION, &identifier),
        |status, body| status == 200 && reported_state(body) == EXPIRED,
    )
    .await;
    // "expired - The reservation passed its end date. Amazon RDS releases the reserved
    // capacity": the reservation is still reported, with its end date, and no capacity of
    // it is available in any zone.
    assert_eq!(
        text(&expired, "EndDate"),
        end_date,
        "expected the end date the create named, got {expired}"
    );
    assert_eq!(
        text(&expired, "ReservedCapacityPerAvailabilityZone"),
        "2",
        "expected the reserved capacity to be reported after the reservation expired, got \
         {expired}"
    );
    assert_eq!(
        texts(&expired, "AvailableCapacity"),
        vec!["0".to_string(), "0".to_string()],
        "expected no available capacity once the reserved capacity is released, got {expired}"
    );

    // An expired reservation is in no state the `state` filter accepts, so it is reported
    // by the unnamed report and by no filtered one.
    let unfiltered = expect_ok(
        "the whole report",
        call(
            &router,
            "DescribeDBCapacityReservations",
            &[("MaxRecords", "20")],
        )
        .await,
    );
    assert_eq!(
        texts(&unfiltered, "DBCapacityReservationName"),
        vec!["emu-expiring".to_string()],
        "expected the expired reservation in the unnamed report, got {unfiltered}"
    );
    let mut members = filter_members(&[("state", &["active"])]);
    members.push(("MaxRecords".to_string(), "20".to_string()));
    let filtered = expect_ok(
        "the active reservations",
        call(&router, "DescribeDBCapacityReservations", &members).await,
    );
    assert!(
        texts(&filtered, "DBCapacityReservationName").is_empty(),
        "expected an expired reservation to be outside the active ones, got {filtered}"
    );
}

#[tokio::test]
async fn a_start_refuses_a_capacity_reservation_nothing_created_and_takes_one_that_exists() {
    let router = emulator();
    // A stopped DB instance to start, and one reservation for it to use after it starts.
    expect_ok(
        "the instance",
        call(
            &router,
            "CreateDBInstance",
            &[
                ("DBInstanceIdentifier", "emu-reserved-start"),
                ("Engine", "mysql"),
                ("DBInstanceClass", "db.m5.large"),
            ],
        )
        .await,
    );
    poll_until(
        "the instance",
        "the create",
        || read_instance(&router, "emu-reserved-start"),
        |status, body| status == 200 && text(body, "DBInstanceStatus") == "available",
    )
    .await;
    expect_ok(
        "the stop",
        call(
            &router,
            "StopDBInstance",
            &[("DBInstanceIdentifier", "emu-reserved-start")],
        )
        .await,
    );
    poll_until(
        "the instance",
        "the stop",
        || read_instance(&router, "emu-reserved-start"),
        |status, body| status == 200 && text(body, "DBInstanceStatus") == "stopped",
    )
    .await;
    let created = expect_ok(
        "the reservation the start names",
        create_one_zone(&router, REGION, "emu-start-reservation", 1).await,
    );
    let identifier = text(&created, "DBCapacityReservationIdentifier");

    // A value nothing answers to is the modelled not-found fault, and the instance is left
    // stopped.
    expect_error(
        "a reservation nothing created",
        call(
            &router,
            "StartDBInstance",
            &[
                ("DBInstanceIdentifier", "emu-reserved-start"),
                ("DBCapacityReservationIdentifier", "emu-no-such-reservation"),
            ],
        )
        .await,
        404,
        "DBCapacityReservationNotFound",
    );
    let stopped = expect_ok(
        "the instance a refused start left alone",
        read_instance(&router, "emu-reserved-start").await,
    );
    assert_eq!(
        text(&stopped, "DBInstanceStatus"),
        "stopped",
        "expected a refused start to leave the instance stopped, got {stopped}"
    );

    // The reservation this suite created resolves by its minted identifier, so the start is
    // accepted — and the started instance reports the reservation it uses, so the
    // association is observable through a later read rather than accepted and dropped.
    let started = expect_ok(
        "a reservation this emulator holds",
        call(
            &router,
            "StartDBInstance",
            &[
                ("DBInstanceIdentifier", "emu-reserved-start"),
                ("DBCapacityReservationIdentifier", identifier.as_str()),
            ],
        )
        .await,
    );
    assert_eq!(
        text(&started, "DBCapacityReservationIdentifier"),
        identifier,
        "expected the started instance to report the reservation it uses, got {started}"
    );
    let running = expect_ok(
        "the started instance read back",
        read_instance(&router, "emu-reserved-start").await,
    );
    assert_eq!(
        text(&running, "DBCapacityReservationIdentifier"),
        identifier,
        "expected a later read of the instance to report the same reservation, got {running}"
    );

    // "You can specify the identifier ... or the reservation name": a start that names the
    // reservation by NAME records the same minted identifier.
    poll_until(
        "the instance",
        "the start",
        || read_instance(&router, "emu-reserved-start"),
        |status, body| status == 200 && text(body, "DBInstanceStatus") == "available",
    )
    .await;
    expect_ok(
        "the stop that detaches the reservation",
        call(
            &router,
            "StopDBInstance",
            &[
                ("DBInstanceIdentifier", "emu-reserved-start"),
                ("DetachFromDBCapacityReservation", "true"),
            ],
        )
        .await,
    );
    // "DetachFromDBCapacityReservation ... controls the reservation that the DB instance
    // belongs to": a stop that detaches releases the association, so the instance reports
    // none.
    let detached = expect_ok(
        "the detached instance",
        read_instance(&router, "emu-reserved-start").await,
    );
    assert_eq!(
        optional_text(&detached, "DBCapacityReservationIdentifier"),
        None,
        "expected a stop that detaches to leave the instance using no reservation, got \
         {detached}"
    );
    poll_until(
        "the instance",
        "the stop",
        || read_instance(&router, "emu-reserved-start"),
        |status, body| status == 200 && text(body, "DBInstanceStatus") == "stopped",
    )
    .await;
    let by_name = expect_ok(
        "a start naming the reservation by name",
        call(
            &router,
            "StartDBInstance",
            &[
                ("DBInstanceIdentifier", "emu-reserved-start"),
                ("DBCapacityReservationIdentifier", "emu-start-reservation"),
            ],
        )
        .await,
    );
    assert_eq!(
        text(&by_name, "DBCapacityReservationIdentifier"),
        identifier,
        "expected a start that named the reservation by name to record its minted \
         identifier, got {by_name}"
    );
}
