// SPDX-License-Identifier: CC0-1.0

//! `DeleteSnapshotSchedule`, end to end over the awsQuery wire.
//!
//! Every case drives a real HTTP request through the same `MultiServiceRouter`
//! the production router builds (the harness is shared with
//! [`crate::cluster_lifecycle`]), so each assertion covers the whole walk:
//! form-urlencoded deserialize -> handler -> XML serialize.
//!
//! The suite asserts ONE rule over a table of schedules rather than one
//! recorded request: a delete removes exactly the schedule the account holds
//! under the identifier the request spells, so every later read reports that
//! identifier absent, the schedules around it keep the order every read walks
//! them in and the identifier takes a create again — unless the account holds
//! no schedule under it as spelled (`SnapshotScheduleNotFound`) or a cluster
//! takes its snapshots on it (`InvalidClusterSnapshotScheduleState`), in which
//! case the record stays.
//!
//! The rows differ along what the rule generalises over: the FORM of the
//! identifier (hyphenated, mixed case, digits only, one character, the
//! documented maximum length), how many definitions the schedule holds and in
//! which expression type, whether it carries tags, and how many clusters are
//! associated with it (none, one, two).

use service_com_amazonaws_redshift_redshiftserviceversion20121201::state;

use crate::cluster_lifecycle::{all_text, Answer, Emulator};

/// The documented maximum length of a `ScheduleIdentifier`, as this service
/// bounds it (`state::SNAPSHOT_SCHEDULE_IDENTIFIER_MAX_LEN`): the model bounds
/// the member only at `len 0..2147483647`, so the row at the bound is built
/// from the constant rather than spelled out.
const IDENTIFIER_MAX_LEN: usize = state::SNAPSHOT_SCHEDULE_IDENTIFIER_MAX_LEN;

/// A schedule the account holds, named so the delete can be asked for it.
struct DeleteCase {
    case: &'static str,
    /// The identifier the create spells, and the delete names.
    identifier: &'static str,
    /// Pad the identifier with trailing alphanumerics up to
    /// [`IDENTIFIER_MAX_LEN`], for the row at the documented bound.
    pad_to_max: bool,
    /// The `ScheduleDefinitions` the create carries, in order.
    definitions: &'static [&'static str],
    tags: &'static [(&'static str, &'static str)],
    /// The clusters created naming this schedule, in creation order: the
    /// clusters whose association stands in the way of the delete.
    clusters: &'static [&'static str],
}

/// The delete table. The identifier forms are the ones the documented syntax
/// admits (`state::check_snapshot_schedule_identifier`: 1 to 255 alphanumeric
/// characters or hyphens, never case folded).
const DELETE_CASES: &[DeleteCase] = &[
    DeleteCase {
        case: "a hyphenated identifier, one rate definition, no cluster on it",
        identifier: "schedule-nightly",
        pad_to_max: false,
        definitions: &["rate(12 hours)"],
        tags: &[],
        clusters: &[],
    },
    DeleteCase {
        case: "a mixed-case identifier, a cron definition and two tags",
        identifier: "ScheduleMixedCase",
        pad_to_max: false,
        definitions: &["cron(0 12 * * ? *)"],
        tags: &[("owner", "admin"), ("environment", "production")],
        clusters: &[],
    },
    DeleteCase {
        case: "an identifier of digits only, two definitions",
        identifier: "2026backups",
        pad_to_max: false,
        definitions: &["rate(1 day)", "cron(0 0 * * ? *)"],
        tags: &[],
        clusters: &[],
    },
    DeleteCase {
        case: "an identifier of one character, no definition at all",
        identifier: "s",
        pad_to_max: false,
        definitions: &[],
        tags: &[],
        clusters: &[],
    },
    DeleteCase {
        case: "an identifier at the documented maximum length",
        identifier: "schedule-at-the-bound",
        pad_to_max: true,
        definitions: &["rate(90 minutes)"],
        tags: &[("owner", "admin")],
        clusters: &[],
    },
    DeleteCase {
        case: "a schedule one cluster takes its snapshots on",
        identifier: "schedule-one-cluster",
        pad_to_max: false,
        definitions: &["rate(12 hours)"],
        tags: &[],
        clusters: &["cluster-only"],
    },
    DeleteCase {
        case: "a schedule two clusters take their snapshots on",
        identifier: "schedule-two-clusters",
        pad_to_max: false,
        definitions: &["rate(6 hours)", "rate(1 day)"],
        tags: &[("owner", "admin")],
        clusters: &["cluster-alpha", "cluster-beta"],
    },
];

/// The identifier of one row, built out to [`IDENTIFIER_MAX_LEN`] for the row at
/// the bound.
fn identifier_of(row: &DeleteCase) -> String {
    let mut identifier = row.identifier.to_string();
    if row.pad_to_max {
        while identifier.chars().count() < IDENTIFIER_MAX_LEN {
            identifier.push('0');
        }
    }
    identifier
}

/// The same identifier with the case of every letter swapped, when that spells
/// something else: a schedule identifier is never case folded, so this names no
/// schedule of the account.
fn case_swapped(identifier: &str) -> Option<String> {
    let swapped: String = identifier
        .chars()
        .map(|c| {
            if c.is_ascii_uppercase() {
                c.to_ascii_lowercase()
            } else {
                c.to_ascii_uppercase()
            }
        })
        .collect();
    (swapped != identifier).then_some(swapped)
}

/// Drive one action with owned parameters, so a case can build its values.
async fn call_with(emulator: &Emulator, action: &str, params: &[(String, String)]) -> Answer {
    let borrowed: Vec<(&str, &str)> = params
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();
    emulator.call(action, &borrowed).await
}

/// `CreateSnapshotSchedule` for one identifier, definitions and tags.
async fn create_schedule(
    emulator: &Emulator,
    identifier: &str,
    definitions: &[&str],
    tags: &[(&str, &str)],
) -> Answer {
    let mut params: Vec<(String, String)> = vec![(
        "ScheduleIdentifier".to_string(),
        identifier.to_string(),
    )];
    for (position, definition) in definitions.iter().enumerate() {
        params.push((
            format!("ScheduleDefinitions.ScheduleDefinition.{}", position + 1),
            (*definition).to_string(),
        ));
    }
    for (position, (key, value)) in tags.iter().enumerate() {
        params.push((format!("Tags.Tag.{}.Key", position + 1), (*key).to_string()));
        params.push((
            format!("Tags.Tag.{}.Value", position + 1),
            (*value).to_string(),
        ));
    }
    call_with(emulator, "CreateSnapshotSchedule", &params).await
}

/// `DeleteSnapshotSchedule` for one identifier.
async fn delete_schedule(emulator: &Emulator, identifier: &str) -> Answer {
    call_with(
        emulator,
        "DeleteSnapshotSchedule",
        &[(
            "ScheduleIdentifier".to_string(),
            identifier.to_string(),
        )],
    )
    .await
}

/// The `<ScheduleIdentifier>` of every schedule a body carries, in the order the
/// page lists them.
fn listed_schedules(body: &str) -> Vec<String> {
    all_text(body, "ScheduleIdentifier")
}

/// The identifiers the STORE holds, in the order it holds them, read straight
/// from this service's state rather than from a response.
fn stored_identifiers(emulator: &Emulator) -> Vec<String> {
    let store = state::service_state(&*emulator.engine).expect("the Redshift store");
    let store = store.lock();
    store
        .snapshot_schedules
        .iter()
        .map(|schedule| schedule.schedule_identifier.clone())
        .collect()
}

/// The definitions the STORE holds for one schedule.
fn stored_definitions(emulator: &Emulator, identifier: &str) -> Option<Vec<String>> {
    let store = state::service_state(&*emulator.engine).expect("the Redshift store");
    let store = store.lock();
    store
        .snapshot_schedule(identifier)
        .map(|schedule| schedule.schedule_definitions.clone())
}

/// Assert one refusal: the answer's status, `<Code>` and `<Message>`.
fn assert_refused(case: &str, answer: &Answer, status: u16, code: &str, message: &str) {
    assert_eq!(
        answer.status, status,
        "case {case}: expected status {status}, got {} with body {}",
        answer.status, answer.body
    );
    assert_eq!(
        answer.error_code().as_deref(),
        Some(code),
        "case {case}: expected <Code>{code}</Code>, got {:?} with body {}",
        answer.error_code(),
        answer.body
    );
    assert_eq!(
        answer.text("Message").as_deref(),
        Some(message),
        "case {case}: expected <Message>{message}</Message>, got {:?} with body {}",
        answer.text("Message"),
        answer.body
    );
}

/// Assert a served success.
fn assert_served(case: &str, answer: &Answer) {
    assert_eq!(
        answer.status, 200,
        "case {case}: expected status 200, got {} with body {}",
        answer.status, answer.body
    );
}

/// The schedules the account holds beside the one each row deletes, so every row
/// also shows that the delete removed NOTHING else and left the creation order
/// of the rest as every read walks it.
const BYSTANDERS: [&str; 2] = ["bystander-first", "bystander-last"];

/// A delete removes exactly the schedule the request names: every later read
/// reports that identifier absent, the schedules around it keep their order, a
/// second delete of it is the modelled not-found refusal and the identifier
/// takes a create again. A cluster associated with the schedule takes its
/// snapshots on it, so the delete is refused with the modelled
/// `InvalidClusterSnapshotScheduleState` and the record stays until the cluster
/// is released from it.
#[tokio::test]
async fn a_delete_removes_exactly_the_schedule_the_request_names() {
    for row in DELETE_CASES {
        let case = row.case;
        let emulator = Emulator::new();
        let identifier = identifier_of(row);

        // The bystanders are created around the row's schedule, so the delete
        // has a schedule before it and one after it in the store's order.
        let first = create_schedule(&emulator, BYSTANDERS[0], &["rate(12 hours)"], &[]).await;
        assert_served(case, &first);
        let created =
            create_schedule(&emulator, &identifier, row.definitions, row.tags).await;
        assert_served(case, &created);
        let last = create_schedule(&emulator, BYSTANDERS[1], &["rate(1 day)"], &[]).await;
        assert_served(case, &last);

        for cluster in row.clusters {
            let answer = emulator
                .call(
                    "CreateCluster",
                    &[
                        ("ClusterIdentifier", cluster),
                        ("NodeType", "ra3.xlplus"),
                        ("MasterUsername", "admin"),
                        ("SnapshotScheduleIdentifier", &identifier),
                    ],
                )
                .await;
            assert_served(case, &answer);
        }

        let definitions: Vec<String> =
            row.definitions.iter().map(|d| (*d).to_string()).collect();
        assert_eq!(
            stored_definitions(&emulator, &identifier).as_deref(),
            Some(&definitions[..]),
            "case {case}: expected the schedule stored as created, got {:?}",
            stored_definitions(&emulator, &identifier)
        );

        // A delete naming the identifier in another case names no schedule of
        // the account: a schedule identifier is never case folded, so the
        // refusal reports the identifier the REQUEST spelled and the record
        // stays.
        if let Some(swapped) = case_swapped(&identifier) {
            let refused = delete_schedule(&emulator, &swapped).await;
            assert_refused(
                case,
                &refused,
                400,
                "SnapshotScheduleNotFound",
                &state::snapshot_schedule_not_found_message(
                    state::SnapshotScheduleNotFoundWording::ScheduleNotFoundForIdentifier,
                    &swapped,
                ),
            );
            assert_eq!(
                stored_definitions(&emulator, &identifier).as_deref(),
                Some(&definitions[..]),
                "case {case}: a refused delete removed nothing, got {:?}",
                stored_definitions(&emulator, &identifier)
            );
        }

        // A schedule a cluster takes its snapshots on cannot be dropped out
        // from under it, and the refusal names every cluster that holds it in
        // the order the store holds them.
        if !row.clusters.is_empty() {
            let held: Vec<String> = row.clusters.iter().map(|c| (*c).to_string()).collect();
            let refused = delete_schedule(&emulator, &identifier).await;
            assert_refused(
                case,
                &refused,
                400,
                "InvalidClusterSnapshotScheduleState",
                &state::snapshot_schedule_in_use_message(&identifier, &held),
            );
            assert_eq!(
                stored_definitions(&emulator, &identifier).as_deref(),
                Some(&definitions[..]),
                "case {case}: the refused delete removed nothing, got {:?}",
                stored_definitions(&emulator, &identifier)
            );

            // Releasing every cluster from the schedule takes away the only
            // thing that refused the delete.
            for cluster in row.clusters {
                let released = emulator
                    .call(
                        "ModifyClusterSnapshotSchedule",
                        &[
                            ("ClusterIdentifier", cluster),
                            ("DisassociateSchedule", "true"),
                        ],
                    )
                    .await;
                assert_served(case, &released);
            }
        }

        let deleted = delete_schedule(&emulator, &identifier).await;
        assert_served(case, &deleted);

        // The read of the deleted identifier is SERVED, with no schedule in it,
        // and the record left the store.
        let read_back = call_with(
            &emulator,
            "DescribeSnapshotSchedules",
            &[(
                "ScheduleIdentifier".to_string(),
                identifier.clone(),
            )],
        )
        .await;
        assert_served(case, &read_back);
        assert_eq!(
            listed_schedules(&read_back.body),
            Vec::<String>::new(),
            "case {case}: expected the deleted identifier absent, got body {}",
            read_back.body
        );

        // Exactly the one schedule left, and the rest keep the order every read
        // walks them in.
        let expected_rest = vec![BYSTANDERS[0].to_string(), BYSTANDERS[1].to_string()];
        assert_eq!(
            stored_identifiers(&emulator),
            expected_rest,
            "case {case}: expected only the other schedules left, in creation order"
        );
        let all = emulator.call("DescribeSnapshotSchedules", &[]).await;
        assert_eq!(
            listed_schedules(&all.body),
            expected_rest,
            "case {case}: expected the other schedules reported in creation order, got body {}",
            all.body
        );

        // Deleting it a second time names a schedule the account no longer
        // holds, worded with the identifier the request spelled.
        let again = delete_schedule(&emulator, &identifier).await;
        assert_refused(
            case,
            &again,
            400,
            "SnapshotScheduleNotFound",
            &state::snapshot_schedule_not_found_message(
                state::SnapshotScheduleNotFoundWording::ScheduleNotFoundForIdentifier,
                &identifier,
            ),
        );

        // The identifier the delete freed takes a create again, with
        // definitions of its own: the deletion freed the identity, not just the
        // record.
        let recreated = create_schedule(&emulator, &identifier, &["rate(4 hours)"], &[]).await;
        assert_served(case, &recreated);
        assert_eq!(
            stored_definitions(&emulator, &identifier).as_deref(),
            Some(["rate(4 hours)".to_string()].as_slice()),
            "case {case}: expected the re-created schedule's definitions stored, got {:?}",
            stored_definitions(&emulator, &identifier)
        );
    }
}

/// A delete naming an identifier nothing ever created is the same modelled
/// refusal, whatever the form of the identifier — and it removes nothing from an
/// account that holds schedules.
#[tokio::test]
async fn a_delete_naming_no_schedule_of_the_account_is_refused() {
    let emulator = Emulator::new();
    for identifier in BYSTANDERS {
        let answer = create_schedule(&emulator, identifier, &["rate(12 hours)"], &[]).await;
        assert_served(identifier, &answer);
    }

    // The forms differ along what the documented identifier syntax admits, plus
    // the one the row below builds at the bound.
    let mut unknown: Vec<String> = ["schedule-never-created", "NeverCreated", "7", "0-0"]
        .iter()
        .map(|identifier| (*identifier).to_string())
        .collect();
    unknown.push("z".repeat(IDENTIFIER_MAX_LEN));

    for identifier in &unknown {
        let refused = delete_schedule(&emulator, identifier).await;
        assert_refused(
            identifier,
            &refused,
            400,
            "SnapshotScheduleNotFound",
            &state::snapshot_schedule_not_found_message(
                state::SnapshotScheduleNotFoundWording::ScheduleNotFoundForIdentifier,
                identifier,
            ),
        );
    }

    assert_eq!(
        stored_identifiers(&emulator),
        vec![BYSTANDERS[0].to_string(), BYSTANDERS[1].to_string()],
        "case the refused deletes: the account's schedules are untouched"
    );
}
