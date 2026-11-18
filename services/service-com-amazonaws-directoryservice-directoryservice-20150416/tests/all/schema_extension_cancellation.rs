// SPDX-License-Identifier: CC0-1.0

//! `CancelSchemaExtension`, driven end to end over HTTP.
//!
//! Every case goes through the same front door a client uses: `POST /` with
//! `X-Amz-Target: DirectoryService_20150416.<Operation>` and an `awsJson1_1` body, so
//! nothing here reaches into the state struct and a response that is not computed from
//! persisted state cannot pass.
//!
//! The rule under test is that a cancel winds back the stored extension the request names
//! whenever that extension's own status is one the operation documents as cancellable,
//! leaving it in the transient `CancelInProgress` whose rollback walk the clock settles on
//! `Cancelled`, and
//! refuses every other request from the same stored record: an id naming no stored
//! directory or no extension of it is the modelled "entity does not exist", and an
//! extension past the cancellable states — one replicating to the domain controllers, or
//! already finished — is the modelled client error. The table rows
//! differ along everything a start varies in (description form and length, LDIF size,
//! whether a snapshot was taken first) and, for the window rule, along the phase of its
//! application the cancel arrives in.

use std::cell::RefCell;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{call, ok, refused, service, string};
use crate::schema_extensions::{
    directory_in_service, extension_status, poll_until_extension, poll_until_extension_status,
    reported_extension, STATUS_COMPLETED, STATUS_CREATING_SNAPSHOT, STATUS_INITIALIZING,
    STATUS_REPLICATING, STATUS_UPDATING_SCHEMA,
};

/// The values of the modelled `SchemaExtensionStatus` enum a cancel puts an extension
/// through (`smithy_query(query="shape",shape="SchemaExtensionStatus")` lists
/// `Initializing`, `CreatingSnapshot`, `UpdatingSchema`, `Replicating`,
/// `CancelInProgress`, `RollbackInProgress`, `Cancelled`, `Failed`, `Completed`).
const STATUS_CANCEL_IN_PROGRESS: &str = "CancelInProgress";
const STATUS_ROLLBACK_IN_PROGRESS: &str = "RollbackInProgress";
const STATUS_CANCELLED: &str = "Cancelled";

/// The statuses `CancelSchemaExtension`'s own documentation admits a cancel in: "A schema
/// extension can be canceled during any of the following states; Initializing,
/// CreatingSnapshot, and UpdatingSchema."
const CANCELLABLE_STATUSES: [&str; 3] = [
    STATUS_INITIALIZING,
    STATUS_CREATING_SNAPSHOT,
    STATUS_UPDATING_SCHEMA,
];

/// The statuses the rollback a cancel starts walks, in order.
const ROLLBACK_WALK: [&str; 3] = [
    STATUS_CANCEL_IN_PROGRESS,
    STATUS_ROLLBACK_IN_PROGRESS,
    STATUS_CANCELLED,
];

/// The maximum length of the modelled `Description`, from its `@length` trait
/// (`min: 0, max: 128`).
const DESCRIPTION_MAX: usize = 128;

/// A well-formed `DirectoryId` (`^d-[0-9a-f]{10}$`) that is never created.
const ABSENT_DIRECTORY_ID: &str = "d-9f0c3a7b12";

/// A well-formed `SchemaExtensionId` (`^e-[0-9a-f]{10}$`) that is never minted.
const ABSENT_SCHEMA_EXTENSION_ID: &str = "e-1a2b3c4d5e";

/// Start a schema extension of `directory_id`, returning its minted id.
async fn start(
    router: &MultiServiceRouter,
    directory_id: &str,
    description: &str,
    ldif: &str,
    snapshot_first: bool,
    case: &str,
) -> String {
    let answer = ok(
        router,
        "StartSchemaExtension",
        json!({
            "DirectoryId": directory_id,
            "Description": description,
            "LdifContent": ldif,
            "CreateSnapshotBeforeSchemaExtension": snapshot_first,
        }),
        case,
    )
    .await;
    string(&answer, "SchemaExtensionId", case)
}

/// `CancelSchemaExtension`, asserting it was accepted and carried no members back: the
/// modelled `CancelSchemaExtensionResult` declares none.
async fn cancel(
    router: &MultiServiceRouter,
    directory_id: &str,
    schema_extension_id: &str,
    case: &str,
) {
    let answer = ok(
        router,
        "CancelSchemaExtension",
        json!({
            "DirectoryId": directory_id,
            "SchemaExtensionId": schema_extension_id,
        }),
        case,
    )
    .await;
    assert_eq!(
        answer,
        json!({}),
        "case {case:?}: expected an empty CancelSchemaExtension result, got {answer}"
    );
}

/// A numeric member of a reported extension, in epoch seconds — the unit an `awsJson1_1`
/// timestamp is rendered in.
fn epoch_member(entry: &Value, member: &str, case: &str) -> f64 {
    entry[member]
        .as_f64()
        .unwrap_or_else(|| panic!("case {case:?}: expected a numeric {member}, got {entry}"))
}

/// One row of the cancel table: how the extension it cancels was started.
struct CancelCase {
    case: &'static str,
    description: String,
    ldif: String,
    snapshot_first: bool,
}

#[tokio::test]
async fn a_cancel_winds_back_an_extension_still_in_a_cancellable_status() {
    let router = service();
    let directory = directory_in_service(&router, "corp.example.com", "the cancelled directory").await;

    // The rows differ along everything a start varies in: the form and length of the
    // `Description` (its `@length` trait admits up to 128 characters and its pattern
    // admits punctuation and whitespace after the first), the size and shape of the
    // `LdifContent`, and whether the directory was snapshotted first — none of which the
    // cancel rule depends on.
    let cases = vec![
        CancelCase {
            case: "a plain description, no snapshot",
            description: "Add_user_attributes".to_string(),
            ldif: "dn: CN=Shoe-Size,CN=Schema,CN=Configuration,DC=corp,DC=example,DC=com\n\
                   changetype: add\nobjectClass: attributeSchema\n"
                .to_string(),
            snapshot_first: false,
        },
        CancelCase {
            case: "a punctuated description with whitespace, snapshot first",
            description: "pre-flight schema change #2 @ 03:00".to_string(),
            ldif: "dn: CN=Badge-Id,CN=Schema,CN=Configuration,DC=corp,DC=example,DC=com\n"
                .to_string(),
            snapshot_first: true,
        },
        CancelCase {
            case: "a description at the published length bound, a large LDIF file",
            description: "d".repeat(DESCRIPTION_MAX),
            ldif: "dn: CN=Bulk,CN=Schema,CN=Configuration,DC=corp,DC=example,DC=com\n".repeat(40),
            snapshot_first: false,
        },
        CancelCase {
            case: "a description and an LDIF file of one character each",
            description: "a".to_string(),
            ldif: "#".to_string(),
            snapshot_first: false,
        },
    ];

    let mut cancelled: Vec<(&CancelCase, String)> = Vec::new();
    // One extension nothing cancels, so the cancels can be shown to touch only the
    // extension each request names.
    let bystander = start(
        &router,
        &directory,
        "left_alone",
        "dn: CN=Bystander,CN=Schema,CN=Configuration,DC=corp,DC=example,DC=com\n",
        false,
        "the bystander",
    )
    .await;

    for row in &cases {
        let case = row.case;
        let id = start(
            &router,
            &directory,
            &row.description,
            &row.ldif,
            row.snapshot_first,
            case,
        )
        .await;
        let before = extension_status(&router, &directory, &id, case).await;
        assert_eq!(
            before, STATUS_INITIALIZING,
            "case {case:?}: expected a fresh extension in {STATUS_INITIALIZING}, got {before:?}"
        );

        cancel(&router, &directory, &id, case).await;

        // The cancel answers before the winding back is done, so a caller listing
        // straight afterwards sees the transient status and no end instant yet.
        let entry = reported_extension(&router, &directory, &id, case).await;
        assert_eq!(
            entry["SchemaExtensionStatus"],
            json!(STATUS_CANCEL_IN_PROGRESS),
            "case {case:?}: expected {STATUS_CANCEL_IN_PROGRESS} straight after the cancel, got \
             {entry}"
        );
        assert!(
            entry["EndDateTime"].is_null(),
            "case {case:?}: expected no EndDateTime while the cancel is still under way, got \
             {entry}"
        );
        assert_eq!(
            entry["Description"],
            json!(row.description),
            "case {case:?}: expected Description {:?} to survive the cancel, got {entry}",
            row.description
        );
        cancelled.push((row, id));
    }

    // Once every phase of the rollback the cancel owes has fallen due, each cancelled
    // extension has settled on the terminal status, with an end instant at or after its
    // start. The wait is for the CONDITION, not for a span of time.
    for (row, id) in &cancelled {
        let case = row.case;
        let entry =
            poll_until_extension_status(&router, &directory, id, STATUS_CANCELLED, case).await;
        let started_at = epoch_member(&entry, "StartDateTime", case);
        let ended_at = epoch_member(&entry, "EndDateTime", case);
        assert!(
            ended_at >= started_at,
            "case {case:?}: expected EndDateTime {ended_at} at or after StartDateTime \
             {started_at}"
        );

        // A listing is a read: the settled status does not move on being read again.
        let again = extension_status(&router, &directory, id, case).await;
        assert_eq!(
            again, STATUS_CANCELLED,
            "case {case:?}: expected {STATUS_CANCELLED} to hold across reads, got {again:?}"
        );
    }

    // A cancel winds back only the extension its request named: the one nothing cancelled
    // walked the phases its own start owed through to the end.
    let case = "the bystander";
    let bystander_status =
        poll_until_extension_status(&router, &directory, &bystander, STATUS_COMPLETED, case)
            .await;
    assert_eq!(
        bystander_status["SchemaExtensionStatus"],
        json!(STATUS_COMPLETED),
        "case {case:?}: expected the extension nothing cancelled to reach {STATUS_COMPLETED}, \
         got {bystander_status}"
    );
}

#[tokio::test]
async fn a_cancel_is_refused_for_an_absent_or_uncancellable_extension() {
    let router = service();
    let directory = directory_in_service(&router, "corp.example.com", "refusals").await;
    let other = directory_in_service(&router, "sales.example.org", "refusals").await;
    let ldif = "dn: CN=Refused,CN=Schema,CN=Configuration,DC=corp,DC=example,DC=com\n";

    // An extension of ANOTHER directory is as absent as one nothing started.
    let other_directorys = start(
        &router,
        &other,
        "other_directory",
        ldif,
        false,
        "an extension of another directory",
    )
    .await;

    for (case, directory_id, schema_extension_id) in [
        (
            "an absent DirectoryId",
            ABSENT_DIRECTORY_ID.to_string(),
            ABSENT_SCHEMA_EXTENSION_ID.to_string(),
        ),
        (
            "an absent SchemaExtensionId",
            directory.clone(),
            ABSENT_SCHEMA_EXTENSION_ID.to_string(),
        ),
        (
            "an extension of another directory",
            directory.clone(),
            other_directorys.clone(),
        ),
    ] {
        refused(
            &router,
            "CancelSchemaExtension",
            json!({
                "DirectoryId": directory_id,
                "SchemaExtensionId": schema_extension_id,
            }),
            "EntityDoesNotExistException",
            case,
        )
        .await;
    }

    // The refused cancels wound nothing back: the other directory's extension is still on
    // the walk its own start set, which no cancel status is part of.
    let case = "an extension of another directory";
    let untouched = extension_status(&router, &other, &other_directorys, case).await;
    assert!(
        [
            STATUS_INITIALIZING,
            STATUS_CREATING_SNAPSHOT,
            STATUS_UPDATING_SCHEMA,
            STATUS_REPLICATING,
            STATUS_COMPLETED,
        ]
        .contains(&untouched.as_str()),
        "case {case:?}: expected a refused cancel to leave the extension on the path its own \
         start set, got {untouched:?}"
    );

    // An extension past the cancellable states is refused with the modelled client
    // error: one the clock already completed, one this emulator is still winding back,
    // and one already cancelled.
    let completed = start(&router, &directory, "already_done", ldif, false, "completed").await;
    let settled = poll_until_extension_status(
        &router,
        &directory,
        &completed,
        STATUS_COMPLETED,
        "completed",
    )
    .await;
    assert!(
        !settled["EndDateTime"].is_null(),
        "expected the completed extension to report an EndDateTime, got {settled}"
    );

    let winding_back = start(
        &router,
        &directory,
        "being_cancelled",
        ldif,
        false,
        "cancel in progress",
    )
    .await;
    cancel(&router, &directory, &winding_back, "cancel in progress").await;

    for (case, schema_extension_id, terminal_status) in [
        (
            "an extension already completed",
            &completed,
            Some(STATUS_COMPLETED),
        ),
        ("an extension still being cancelled", &winding_back, None),
    ] {
        let (status_code, parsed) = call(
            &router,
            "CancelSchemaExtension",
            json!({
                "DirectoryId": &directory,
                "SchemaExtensionId": schema_extension_id,
            }),
        )
        .await;
        assert_eq!(
            status_code, 400,
            "case {case:?}: expected 400, got {status_code} with {parsed}"
        );
        let wire_type = parsed["__type"].as_str().unwrap_or_default();
        assert!(
            wire_type.contains("ClientException"),
            "case {case:?}: expected ClientException, got {parsed}"
        );
        // The message names the status that refused the cancel and the states that admit
        // one. For a record that has settled that status is fixed; for one still being
        // wound back the clock may have carried it to the next phase of the rollback, so
        // any status of that walk is the status it was in.
        let named: Vec<&str> = match terminal_status {
            Some(status) => vec![status],
            None => ROLLBACK_WALK.to_vec(),
        };
        let message = parsed["Message"].as_str().unwrap_or_default();
        assert!(
            named.iter().any(|status| message.contains(status))
                && message.contains(STATUS_INITIALIZING),
            "case {case:?}: expected the message to name the status that refused the cancel \
             (one of {named:?}) and the states that admit one, got {message:?}"
        );

        // The refusal left the stored status where it was: a refused cancel starts no
        // rollback, and an extension already being wound back stays on the rollback it
        // was on.
        let after = extension_status(&router, &directory, schema_extension_id, case).await;
        match terminal_status {
            Some(status) => assert_eq!(
                after, status,
                "case {case:?}: expected the refused cancel to leave the settled status at \
                 {status}, got {after:?}"
            ),
            None => assert!(
                ROLLBACK_WALK.contains(&after.as_str()),
                "case {case:?}: expected the refused cancel to leave the extension on the \
                 rollback walk {ROLLBACK_WALK:?}, got {after:?}"
            ),
        }
    }

    // Once the rollback it is winding back settles, a further cancel of the terminal
    // record is refused the same way.
    poll_until_extension_status(
        &router,
        &directory,
        &winding_back,
        STATUS_CANCELLED,
        "already cancelled",
    )
    .await;
    refused(
        &router,
        "CancelSchemaExtension",
        json!({
            "DirectoryId": &directory,
            "SchemaExtensionId": &winding_back,
        }),
        "ClientException",
        "an extension already cancelled",
    )
    .await;
    let after = extension_status(&router, &directory, &winding_back, "already cancelled").await;
    assert_eq!(
        after, STATUS_CANCELLED,
        "expected the refused cancel to leave the status at {STATUS_CANCELLED}, got {after:?}"
    );
}

/// One row of the cancel-window table: the phase of the extension's application the cancel
/// arrives in, how the start that reached it was made, and whether the operation's own
/// documentation admits a cancel there.
struct WindowCase {
    case: &'static str,
    snapshot_first: bool,
    cancel_in: &'static str,
    accepted: bool,
}

/// Start the extension one window row describes, wait for the phase that row's cancel is
/// to arrive in, and assert the cancel is accepted — and then wound back phase by phase —
/// or refused, as that phase requires.
///
/// Its own function so the rows can be driven CONCURRENTLY: each row waits out the
/// service's own clock-owed chain, so driving them at once costs one walk rather than one
/// per row, and each row only ever reads the extension it started.
async fn drive_window_case(router: &MultiServiceRouter, directory: &str, row: &WindowCase) {
    let case = row.case;
    // Whether the row expects the cancel to be accepted is the operation's own documented
    // set, not a value chosen row by row.
    assert_eq!(
        CANCELLABLE_STATUSES.contains(&row.cancel_in),
        row.accepted,
        "case {case:?}: the documented cancellable states {CANCELLABLE_STATUSES:?} and this \
         row's expectation for {:?} disagree",
        row.cancel_in
    );
    let ldif = "dn: CN=Window,CN=Schema,CN=Configuration,DC=corp,DC=example,DC=com\n";
    let id = start(
        router,
        directory,
        "window_case",
        ldif,
        row.snapshot_first,
        case,
    )
    .await;
    // The phase has to last long enough for a client that polls for it to act in it:
    // this wait IS that client.
    poll_until_extension_status(router, directory, &id, row.cancel_in, case).await;

    if row.accepted {
        cancel(router, directory, &id, case).await;

        // The cancel starts a rollback that is reported phase by phase, exactly as the
        // application it replaced was.
        let seen = RefCell::new(Vec::<String>::new());
        poll_until_extension(
            router,
            directory,
            &id,
            "settle on the cancelled status",
            case,
            |observed| {
                let mut seen = seen.borrow_mut();
                if seen.last().map(String::as_str) != Some(observed) {
                    seen.push(observed.to_string());
                }
                observed == STATUS_CANCELLED
            },
        )
        .await;
        let walked = seen.into_inner();
        let walked: Vec<&str> = walked.iter().map(String::as_str).collect();
        assert_eq!(
            walked,
            ROLLBACK_WALK.to_vec(),
            "case {case:?}: expected the cancelled extension to be reported in \
             {ROLLBACK_WALK:?}, got {walked:?}"
        );
    } else {
        refused(
            router,
            "CancelSchemaExtension",
            json!({
                "DirectoryId": directory,
                "SchemaExtensionId": &id,
            }),
            "ClientException",
            case,
        )
        .await;
        let after = extension_status(router, directory, &id, case).await;
        assert!(
            !ROLLBACK_WALK.contains(&after.as_str()),
            "case {case:?}: expected the refused cancel to leave the extension on the \
             application walk, got {after:?}"
        );
    }
}

#[tokio::test]
async fn the_phase_a_cancel_arrives_in_decides_whether_it_is_accepted() {
    let router = service();
    let directory = directory_in_service(&router, "corp.example.com", "the cancel window").await;

    // "A schema extension can be canceled during any of the following states;
    // Initializing, CreatingSnapshot, and UpdatingSchema" — and "once a schema extension
    // has started replicating to all domain controllers, the task can no longer be
    // canceled". One row per phase of the application the extension can be reported in,
    // the cancellable ones and the two past them, each reached by POLLING for it the way a
    // client does.
    let cases = [
        WindowCase {
            case: "a cancel while the extension is initializing",
            snapshot_first: false,
            cancel_in: STATUS_INITIALIZING,
            accepted: true,
        },
        WindowCase {
            case: "a cancel while the snapshot asked for is being taken",
            snapshot_first: true,
            cancel_in: STATUS_CREATING_SNAPSHOT,
            accepted: true,
        },
        WindowCase {
            case: "a cancel while the schema is being updated",
            snapshot_first: false,
            cancel_in: STATUS_UPDATING_SCHEMA,
            accepted: true,
        },
        WindowCase {
            case: "a cancel once the change is replicating",
            snapshot_first: false,
            cancel_in: STATUS_REPLICATING,
            accepted: false,
        },
        WindowCase {
            case: "a cancel once the extension has completed",
            snapshot_first: true,
            cancel_in: STATUS_COMPLETED,
            accepted: false,
        },
    ];

    tokio::join!(
        drive_window_case(&router, &directory, &cases[0]),
        drive_window_case(&router, &directory, &cases[1]),
        drive_window_case(&router, &directory, &cases[2]),
        drive_window_case(&router, &directory, &cases[3]),
        drive_window_case(&router, &directory, &cases[4]),
    );
}
