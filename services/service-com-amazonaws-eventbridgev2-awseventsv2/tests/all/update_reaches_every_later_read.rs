// SPDX-License-Identifier: CC0-1.0

//! What every LATER read of an event bus reports once an update of it has been
//! accepted, read where a client reads it: off the `rpcv2Cbor` wire.
//!
//! RULE: an accepted update writes onto the stored resource exactly the members the
//! request carries and leaves every member it omits as it stands, so the resource
//! the service holds from then on is the one the update left: every later read of
//! it — repeated, and through every projection that carries the member — reports
//! the updated value, and no other resource of the service is touched.
//!
//! The rows span what the rule generalises over:
//!
//! * which member the request carries — one, two together, a structure member, and
//!   none at all — so applying a change is not keyed to one request shape;
//! * the kind of value — free text, the empty text the member admits, text at the
//!   modelled `@length` maximum, a structure naming a key identifier, and an
//!   integer at each published bound of `RetentionPeriodInDays`' `@range` as well
//!   as inside it;
//! * how many times and through which projection the resource is read afterwards —
//!   the update's own answer, three repeated `DescribeEventBus` reads, and the
//!   `ListEventBuses` summary — so persistence is not keyed to the first read;
//! * which resource the update names, so a change reaches that bus and only it.
//!
//! Each row is applied to the bus the previous row left, so an expectation names
//! the WHOLE configuration rather than only the members its own row carried.
//!
//! Nothing here waits a fixed time: both waits are for a CONDITION, each polled
//! under a generous deadline and each stopping as soon as it holds — a bus
//! reporting `ACTIVE`, and the clock's whole second having turned, which is what
//! makes the whole-second `LastModifiedTime` able to move at all.
//!
//! Nothing here is restated either: the wire harness, its ACTIVE poll and the
//! bounds of both polls come from `modelled_input_refusal.rs`, and the fact shape
//! a bus's updatable members are read into — together with the expectation builder
//! over it — from `event_bus_lifecycle.rs`, the suite that already drives this rule
//! through the typed handlers. This suite adds the rows and the assertions that
//! are its own: the wire path, the repeated reads, the list projection, the
//! last-modified rule and the cross-bus isolation.

use std::time::{Duration, Instant, SystemTime};

use aws_smithy_types::DateTime;
use engine::InMemoryEngine;

use service_com_amazonaws_eventbridgev2_awseventsv2 as eventsv2;

use eventsv2::ops::create_event_bus::CreateEventBus;
use eventsv2::ops::describe_event_bus::DescribeEventBus;
use eventsv2::ops::list_event_buses::ListEventBuses;
use eventsv2::ops::types::{
    CreateEventBusInput, DescribeEventBusInput, DescribeEventBusOutput, EncryptionConfiguration,
    ListEventBusesInput, StorageConfiguration, UpdateEventBusInput,
};
use eventsv2::ops::update_event_bus::UpdateEventBus;

// The ONE fact shape this crate reads a bus's updatable members into, and the ONE
// pair of bounds its condition polls use — both reused from the suites that already
// hold them rather than restated here.
use crate::event_bus_lifecycle::{expected_bus, facts_of_described, facts_of_updated, BusFacts};
use crate::modelled_input_refusal::{Wire, POLL_INTERVAL};

/// `RetentionPeriodInDays`' published `@range`, which a storage configuration a
/// request names must sit inside.
/// Source: `smithy_query(query="shape", shape="RetentionPeriodInDays")`.
const RETENTION_MIN: i32 = 1;
const RETENTION_MAX: i32 = 365;

/// `Description`'s published `@length` maximum.
/// Source: `smithy_query(query="shape", shape="Description")`.
const DESCRIPTION_MAX_LEN: usize = 512;

/// How many times each row reads the bus back: the recorded flow this suite pins
/// reads the updated bus again at two later steps, so one read is not enough to
/// say the change is held rather than merely answered.
const READS_PER_ROW: usize = 3;

/// How long the wait for the clock's whole second to turn keeps looking: the
/// condition holds within a second of real time, so this is room for a loaded host
/// rather than an expected wait.
const SECOND_TURN_DEADLINE: Duration = Duration::from_secs(30);

// Every expectation below names the bus as operational (`ACTIVE`). That is
// evidence rather than a restatement of what the handler happens to do: the
// recorded trace this suite was written from answers an accepted change to a bus's
// description, encryption or storage configuration with the bus already
// operational, and its later reads of that bus diverged only on the members the
// change carried — `State` was not among them.

/// Create a bus carrying `description`, `retention_period_in_days` and
/// `kms_key_identifier`, and hand back its ARN. The bus is still provisioning when
/// this returns.
async fn create_bus(
    wire: &Wire,
    case: &str,
    name: &str,
    description: &str,
    retention_period_in_days: i32,
    kms_key_identifier: &str,
) -> String {
    wire.ok::<CreateEventBus<InMemoryEngine>>(
        case,
        &CreateEventBusInput {
            name: name.to_string(),
            description: Some(description.to_string()),
            storage_configuration: Some(StorageConfiguration {
                retention_period_in_days: Some(retention_period_in_days),
            }),
            encryption_configuration: Some(EncryptionConfiguration {
                kms_key_identifier: Some(kms_key_identifier.to_string()),
            }),
            client_token: Some(format!("token-for-{name}")),
            ..Default::default()
        },
    )
    .await
    .event_bus_arn
    .unwrap_or_else(|| panic!("case {case:?}: CreateEventBus must report an EventBusArn"))
}

/// Read the bus at `arn`.
async fn describe(wire: &Wire, case: &str, arn: &str) -> DescribeEventBusOutput {
    wire.ok::<DescribeEventBus<InMemoryEngine>>(
        case,
        &DescribeEventBusInput {
            event_bus_arn: arn.to_string(),
        },
    )
    .await
}

/// The description the `ListEventBuses` summary of the bus named `name` carries.
/// The summary models `Description` but no storage configuration
/// (`smithy_query(query="shape", shape="EventBusSummary")`), so that is the member
/// this projection can be asked for.
async fn listed_description(wire: &Wire, case: &str, name: &str) -> Option<String> {
    let listed = wire
        .ok::<ListEventBuses<InMemoryEngine>>(
            case,
            &ListEventBusesInput {
                name_prefix: Some(name.to_string()),
                // `MaxResults` is a plain `i32` here, so a page size inside its
                // modelled range is stated rather than left at the type's zero.
                max_results: 10,
                ..Default::default()
            },
        )
        .await;
    listed
        .event_buses
        .unwrap_or_default()
        .into_iter()
        .find(|summary| summary.name.as_deref() == Some(name))
        .unwrap_or_else(|| panic!("case {case:?}: ListEventBuses must report the bus {name:?}"))
        .description
}

/// RULE: every read after an accepted update reports the bus the update left — the
/// members the request carried, the members it omitted as they stood — however
/// many times and through whichever projection the bus is read.
#[tokio::test]
async fn every_later_read_reports_the_bus_the_update_left() {
    let wire = Wire::new();
    let name = "read-back-bus";
    let case = "the bus under test";
    let arn = create_bus(&wire, case, name, "initial description", 3, "alias/initial-key").await;
    wire.await_active(case, &arn).await;

    let created = describe(&wire, "the created bus", &arn).await;
    assert_eq!(
        facts_of_described(&created),
        expected_bus("initial description", 3, "alias/initial-key"),
        "the created bus must report what the create asked for, got {created:?}"
    );
    let creation_time = created.creation_time;

    let longest_description = "d".repeat(DESCRIPTION_MAX_LEN);
    // The last member of each row says whether the request carries anything to
    // apply, which is what decides whether the resource was modified at all.
    let rows: Vec<(&str, UpdateEventBusInput, BusFacts, bool)> = vec![
        (
            "a description only",
            UpdateEventBusInput {
                event_bus_arn: arn.clone(),
                description: Some("updated description".to_string()),
                ..Default::default()
            },
            expected_bus("updated description", 3, "alias/initial-key"),
            true,
        ),
        (
            "a retention period only, at the published minimum",
            UpdateEventBusInput {
                event_bus_arn: arn.clone(),
                storage_configuration: Some(StorageConfiguration {
                    retention_period_in_days: Some(RETENTION_MIN),
                }),
                ..Default::default()
            },
            expected_bus("updated description", RETENTION_MIN, "alias/initial-key"),
            true,
        ),
        (
            "a description and a retention period together",
            UpdateEventBusInput {
                event_bus_arn: arn.clone(),
                description: Some("both members at once".to_string()),
                storage_configuration: Some(StorageConfiguration {
                    retention_period_in_days: Some(7),
                }),
                ..Default::default()
            },
            expected_bus("both members at once", 7, "alias/initial-key"),
            true,
        ),
        (
            "a retention period at the published maximum",
            UpdateEventBusInput {
                event_bus_arn: arn.clone(),
                storage_configuration: Some(StorageConfiguration {
                    retention_period_in_days: Some(RETENTION_MAX),
                }),
                ..Default::default()
            },
            expected_bus("both members at once", RETENTION_MAX, "alias/initial-key"),
            true,
        ),
        (
            "the empty description the member admits",
            UpdateEventBusInput {
                event_bus_arn: arn.clone(),
                description: Some(String::new()),
                ..Default::default()
            },
            expected_bus("", RETENTION_MAX, "alias/initial-key"),
            true,
        ),
        (
            "a description at the published maximum length",
            UpdateEventBusInput {
                event_bus_arn: arn.clone(),
                description: Some(longest_description.clone()),
                ..Default::default()
            },
            expected_bus(&longest_description, RETENTION_MAX, "alias/initial-key"),
            true,
        ),
        (
            "an encryption configuration only",
            UpdateEventBusInput {
                event_bus_arn: arn.clone(),
                encryption_configuration: Some(EncryptionConfiguration {
                    kms_key_identifier: Some("alias/rotated-key".to_string()),
                }),
                ..Default::default()
            },
            expected_bus(&longest_description, RETENTION_MAX, "alias/rotated-key"),
            true,
        ),
        (
            "no member at all, which changes nothing",
            UpdateEventBusInput {
                event_bus_arn: arn.clone(),
                ..Default::default()
            },
            expected_bus(&longest_description, RETENTION_MAX, "alias/rotated-key"),
            false,
        ),
    ];

    // The instant the bus reports as last modified, carried from row to row: a
    // change that applied something modified the resource, and one that carried
    // nothing to apply did not.
    let mut last_modified = created
        .last_modified_time
        .expect("the created bus must report a LastModifiedTime");

    for (case, input, want, carries_a_member) in rows {
        let answered = wire
            .ok::<UpdateEventBus<InMemoryEngine>>(case, &input)
            .await;
        let reported = facts_of_updated(&answered);
        assert_eq!(
            reported, want,
            "case {case:?}: UpdateEventBus must answer the bus the change left, got {reported:?}"
        );

        // The same bus, read back as many times as the recorded flow reads it:
        // a change that is held reports the same configuration on every read.
        for read_index in 0..READS_PER_ROW {
            let read = describe(&wire, case, &arn).await;
            let read_back = facts_of_described(&read);
            assert_eq!(
                read_back, want,
                "case {case:?}: read {read_index} after the update must report the bus the \
                 update left, got {read_back:?}"
            );
            // An update changes a bus's configuration, never its identity.
            assert_eq!(
                read.event_bus_arn.as_deref(),
                Some(arn.as_str()),
                "case {case:?}: the EventBusArn must not change, got {:?}",
                read.event_bus_arn
            );
            assert_eq!(
                read.name.as_deref(),
                Some(name),
                "case {case:?}: the Name must not change, got {:?}",
                read.name
            );
            assert_eq!(
                read.creation_time, creation_time,
                "case {case:?}: the CreationTime must not change, got {:?}",
                read.creation_time
            );
        }

        // `LastModifiedTime` never goes backwards, and a request that carried
        // nothing to apply leaves it exactly where it stood — the half of the rule
        // that can be checked without waiting on the clock, since the member is
        // reported in whole seconds and a whole table of rows can run inside one.
        // That it MOVES for a request that applies something is pinned on its own,
        // in `a_modification_moves_the_reported_last_modified_time`.
        let read = describe(&wire, case, &arn).await;
        let reported_modification = read
            .last_modified_time
            .unwrap_or_else(|| panic!("case {case:?}: a LastModifiedTime must be reported"));
        if carries_a_member {
            assert!(
                reported_modification.secs() >= last_modified.secs(),
                "case {case:?}: LastModifiedTime must not go backwards from \
                 {last_modified:?}, got {reported_modification:?}"
            );
        } else {
            assert_eq!(
                reported_modification.secs(),
                last_modified.secs(),
                "case {case:?}: a request carrying nothing to apply must leave \
                 LastModifiedTime at {last_modified:?}, got {reported_modification:?}"
            );
        }
        last_modified = reported_modification;

        // And through the other projection that carries the member.
        let summarised = listed_description(&wire, case, name).await;
        assert_eq!(
            summarised, want.description,
            "case {case:?}: the ListEventBuses summary must report the updated description, \
             got {summarised:?}"
        );
    }
}

/// The key identifier the bus called `name` is created with: one of its own, so a
/// change that leaked onto a neighbour is visible on this member too.
fn key_of(name: &str) -> String {
    format!("alias/{name}-key")
}

/// Look again until the clock's whole second has passed `at`, so a modification
/// made after this point is reported at a LATER whole-second instant than `at`.
///
/// The wait is for a CONDITION — the second has turned — not a fixed sleep. It is
/// the one condition here that does not depend on the emulator, and it holds within
/// a second of real time, so it carries a bound of its own: still many times the
/// second it waits for, so a loaded host has room, but short enough that a clock
/// that never turns is reported quickly rather than at the deadline a provisioning
/// poll needs.
async fn await_the_next_second(case: &str, at: DateTime) {
    let deadline = Instant::now() + SECOND_TURN_DEADLINE;
    loop {
        let now = DateTime::from(SystemTime::now());
        if now.secs() > at.secs() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the clock never passed the second of {at:?}, last read {now:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// RULE: `LastModifiedTime` reports a modification of the resource — it moves for
/// an update that applies something and stands still for one that carries nothing
/// to apply.
///
/// The instant is reported in whole seconds, so the second of the previous value
/// is waited OUT before the update: without that, a handler that moves the instant
/// and one that never touches it would answer the same value, and the assertion
/// could not fail.
#[tokio::test]
async fn a_modification_moves_the_reported_last_modified_time() {
    let wire = Wire::new();
    let case = "a bus modified after the clock's second turned";
    let arn = create_bus(
        &wire,
        case,
        "modification-time-bus",
        "initial description",
        3,
        "alias/initial-key",
    )
    .await;
    wire.await_active(case, &arn).await;

    let before = describe(&wire, case, &arn)
        .await
        .last_modified_time
        .unwrap_or_else(|| panic!("case {case:?}: a LastModifiedTime must be reported"));
    await_the_next_second(case, before).await;

    wire.ok::<UpdateEventBus<InMemoryEngine>>(
        case,
        &UpdateEventBusInput {
            event_bus_arn: arn.clone(),
            description: Some("a description applied after the second turned".to_string()),
            ..Default::default()
        },
    )
    .await;
    let after_a_change = describe(&wire, case, &arn)
        .await
        .last_modified_time
        .unwrap_or_else(|| panic!("case {case:?}: a LastModifiedTime must be reported"));
    assert!(
        after_a_change.secs() > before.secs(),
        "case {case:?}: an update that applied something must move LastModifiedTime past \
         {before:?}, got {after_a_change:?}"
    );

    // And the other way round: once the second has turned again, an update
    // carrying nothing to apply leaves the instant where the last change left it.
    await_the_next_second(case, after_a_change).await;
    wire.ok::<UpdateEventBus<InMemoryEngine>>(
        case,
        &UpdateEventBusInput {
            event_bus_arn: arn.clone(),
            ..Default::default()
        },
    )
    .await;
    let after_nothing = describe(&wire, case, &arn)
        .await
        .last_modified_time
        .unwrap_or_else(|| panic!("case {case:?}: a LastModifiedTime must be reported"));
    assert_eq!(
        after_nothing.secs(),
        after_a_change.secs(),
        "case {case:?}: an update carrying nothing to apply must leave LastModifiedTime at \
         {after_a_change:?}, got {after_nothing:?}"
    );
}

/// RULE, on the resource an update names: a change reaches the bus the request
/// addresses and leaves every other stored bus exactly as it stood.
#[tokio::test]
async fn an_update_reaches_only_the_bus_it_names() {
    let wire = Wire::new();

    // Three buses with configurations of their own, so a change that leaked onto a
    // neighbour is visible whichever neighbour it leaked onto.
    let buses: Vec<(&str, &str, i32)> = vec![
        ("isolated-bus-first", "first description", RETENTION_MIN),
        ("isolated-bus-second", "second description", 30),
        ("isolated-bus-third", "third description", RETENTION_MAX),
    ];
    let mut arns = Vec::new();
    for (name, description, days) in &buses {
        arns.push(create_bus(&wire, name, name, description, *days, &key_of(name)).await);
    }
    for (index, (name, _, _)) in buses.iter().enumerate() {
        wire.await_active(name, &arns[index]).await;
    }

    // Each pass updates ONE bus and then reads every bus: the updated one reports
    // the change, the others report what they were created with.
    for (updated_index, (updated_name, _, _)) in buses.iter().enumerate() {
        let case = *updated_name;
        let new_description = format!("{updated_name} after its own update");
        let new_days = 11 + i32::try_from(updated_index).expect("a row index fits in an i32");
        wire.ok::<UpdateEventBus<InMemoryEngine>>(
            case,
            &UpdateEventBusInput {
                event_bus_arn: arns[updated_index].clone(),
                description: Some(new_description.clone()),
                storage_configuration: Some(StorageConfiguration {
                    retention_period_in_days: Some(new_days),
                }),
                ..Default::default()
            },
        )
        .await;

        for (index, (name, created_description, created_days)) in buses.iter().enumerate() {
            let read = describe(&wire, case, &arns[index]).await;
            let read_back = facts_of_described(&read);
            // Every bus updated so far holds its own update; the rest hold what
            // their create asked for.
            let want = if index < updated_index {
                expected_bus(
                    &format!("{name} after its own update"),
                    11 + i32::try_from(index).expect("a row index fits in an i32"),
                    &key_of(name),
                )
            } else if index == updated_index {
                expected_bus(&new_description, new_days, &key_of(name))
            } else {
                expected_bus(created_description, *created_days, &key_of(name))
            };
            assert_eq!(
                read_back, want,
                "case {case:?}: after updating {updated_name:?}, the bus {name:?} must report \
                 {want:?}, got {read_back:?}"
            );
        }
    }
}
