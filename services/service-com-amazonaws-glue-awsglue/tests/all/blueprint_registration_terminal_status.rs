// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the status a blueprint registration COMES TO REST IN, and for the
//! wording of the `Blueprint$ErrorMessage` a registration that could not read its published
//! archive carries.
//!
//! The rule under test: `CreateBlueprint` answers only `Name` and Glue then fetches the
//! archive published at `BlueprintLocation`, so the registration leaves `CREATING` for the
//! TERMINAL status that fetch decides — `ACTIVE` ("the blueprint has been successfully
//! registered") when the object store serves the object the location names, and `FAILED`
//! ("the blueprint registration failed") carrying the fixed sentence `Access denied on object
//! at location: ` plus the location the REQUEST named when it does not. `ACTIVE` is
//! unreachable for a registration whose archive could not be read: the status is terminal, so
//! publishing the archive afterwards does not move it, and only another registration of the
//! blueprint could.
//!
//! Where [`crate::blueprint_archive_registration`] tables the archive forms a registration
//! settles on, this suite pins what that one does not: the SENTENCE a failed registration
//! reports, spelled out here rather than taken from the service's own helper and read over
//! location shapes that suite does not carry (a key beside an object the same prefix holds, a
//! key whose own prefix IS an object, and a key carrying the metacharacters the location's
//! pattern admits), the TERMINALITY of the status it reports it with, and what a client that
//! only polls observes.
//!
//! Amazon S3 is mounted beside Glue on one engine, so the archive a case stages with
//! `PutObject` is the object the registration reads and the locations nothing stages name
//! objects the store really does not hold — what is asserted is HTTP behaviour over state the
//! object store really keeps.
//!
//! One test leaves the due instant of the registration where the service put it and POLLS
//! `GetBlueprint` for the condition it waits on — that the registration has left `CREATING` —
//! against a generous deadline, which is what the recorded client does; the others move that
//! instant through the store instead of waiting for it. Neither asserts anything about how
//! fast the host runs.

use serde_json::{json, Value};
use service_com_amazonaws_glue_awsglue::state::{
    BLUEPRINT_STATUS_ACTIVE, BLUEPRINT_STATUS_CREATING, BLUEPRINT_STATUS_FAILED,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::blueprint_archive_registration::{
    make_settlement_due, read, stage_archive, PUBLISHED_ARCHIVE,
};
use crate::job_runs::{RUN_AT_REST_DEADLINE, RUN_POLL_INTERVAL};
use crate::job_script_execution::{create_bucket, emulator_with_s3, BUCKET};
use crate::wire::ok;

/// The sentence a registration that could not fetch its published archive reports, as AWS
/// words it: fixed text, then the `BlueprintLocation` the request named, then a period. The
/// location is the only variable part, and it is taken from the REQUEST, so every case below
/// builds its own expectation from the location it sent.
fn expected_failure_message(location: &str) -> String {
    format!("Access denied on object at location: {location}.")
}

/// Register `name` at `location` and answer nothing: the create reports only the name it
/// stored, and what the registration came to is read back through `GetBlueprint`.
async fn register(router: &MultiServiceRouter, name: &str, location: &str) {
    let answered = ok(
        router,
        "CreateBlueprint",
        &json!({ "Name": name, "BlueprintLocation": location }),
    )
    .await;
    assert_eq!(
        answered.get("Name").and_then(Value::as_str),
        Some(name),
        "case {name}: the create reports the name it registered, got {answered}"
    );
}

/// The `Status` of the blueprint `read` reports.
fn status_of(case: &str, blueprint: &Value) -> String {
    blueprint
        .get("Status")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!("case {case}: every registration reports a Status, got {blueprint}")
        })
        .to_string()
}

/// The `ErrorMessage` of the blueprint `read` reports, and `None` when it carries none.
fn error_message_of(blueprint: &Value) -> Option<String> {
    blueprint
        .get("ErrorMessage")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// One location whose archive the object store does not serve, and what makes it unservable.
struct UnreadableLocation {
    /// What this row varies, named in every assertion it drives.
    case: &'static str,
    /// An object to stage before the create, so the row proves the registration reads the
    /// object the location NAMES and not merely the bucket or the prefix around it.
    staged: Option<&'static str>,
    /// The location the create sends, built by [`UnreadableLocation::location`].
    requested: &'static str,
    /// Whether `requested` names the suite's own bucket, or a bucket nothing created.
    in_the_suites_bucket: bool,
}

impl UnreadableLocation {
    /// The `s3://` location this row registers the blueprint at.
    fn location(&self) -> String {
        if self.in_the_suites_bucket {
            format!("s3://{BUCKET}/{}", self.requested)
        } else {
            format!("s3://{}", self.requested)
        }
    }
}

/// The rows: locations that name no object this store serves, differing along what is
/// missing and along the characters the location carries — a nested key under a bucket
/// nothing created, a key beside an object the same prefix does hold, a key whose own prefix
/// is an object, and a key carrying the metacharacters the location's own pattern admits.
///
/// The two shapes [`crate::blueprint_archive_registration`] already tables — a key the
/// suite's bucket does not hold, and a bucket nothing created with a single-segment key —
/// are not repeated here.
fn unreadable_locations() -> Vec<UnreadableLocation> {
    vec![
        UnreadableLocation {
            case: "a nested key under a bucket the object store does not hold",
            staged: None,
            requested: "no-such-bucket-for-blueprints/published/nested/blueprint.zip",
            in_the_suites_bucket: false,
        },
        UnreadableLocation {
            case: "a key beside an object the same prefix does hold",
            staged: Some("published/another-blueprint.zip"),
            requested: "published/blueprint.zip",
            in_the_suites_bucket: true,
        },
        UnreadableLocation {
            case: "a key whose own prefix is an object the store holds",
            staged: Some("published/blueprint.zip"),
            requested: "published/blueprint.zip/inner.zip",
            in_the_suites_bucket: true,
        },
        UnreadableLocation {
            // `OrchestrationS3Location`'s pattern — ^s3://([^/]+)/([^/]+/)*([^/]+)$, from
            // `smithy_query shape=OrchestrationS3Location` — admits every character but `/`
            // in a key, so a location a caller sends can carry the metacharacters of the
            // protocol the answer is serialised in. The message has to come back carrying
            // them unchanged, which is what this row reads.
            case: "a key carrying the metacharacters the location pattern admits",
            staged: None,
            requested: "quote\"backslash\\angle<&>brace{}.zip",
            in_the_suites_bucket: true,
        },
    ]
}

/// A registration whose published archive the object store does not serve comes to rest
/// `FAILED`, reporting the sentence AWS words for an archive it could not fetch with the
/// location the request named in it.
#[tokio::test]
async fn an_unreadable_archive_settles_failed_reporting_the_location_it_could_not_read() {
    let (router, engine) = emulator_with_s3();
    create_bucket(&router).await;

    for (index, row) in unreadable_locations().iter().enumerate() {
        let case = row.case;
        if let Some(key) = row.staged {
            stage_archive(&router, key, PUBLISHED_ARCHIVE.to_vec()).await;
        }
        let location = row.location();
        let name = format!("terminal-failure-{index}");
        register(&router, &name, &location).await;

        make_settlement_due(&engine, &name);
        let settled = read(&router, &name).await;
        assert_eq!(
            status_of(case, &settled),
            BLUEPRINT_STATUS_FAILED,
            "case {case}: a registration whose archive cannot be read must come to rest \
             {BLUEPRINT_STATUS_FAILED}, got {settled}"
        );
        assert_eq!(
            error_message_of(&settled),
            Some(expected_failure_message(&location)),
            "case {case}: expected the failed registration to report {:?}, got {settled}",
            expected_failure_message(&location)
        );
    }
}

/// `FAILED` is terminal: a registration that could not read its archive stays `FAILED`,
/// reporting the same sentence, however often it is read and even once the very object it was
/// pointed at IS published — only another registration of the blueprint could give it a
/// definition.
#[tokio::test]
async fn a_failed_registration_never_reaches_active() {
    let (router, engine) = emulator_with_s3();
    create_bucket(&router).await;

    let case = "a registration published at a key staged only afterwards";
    let key = "published-too-late/blueprint.zip";
    let name = "terminal-failure-is-terminal";
    let location = format!("s3://{BUCKET}/{key}");
    register(&router, name, &location).await;
    make_settlement_due(&engine, name);
    let failed = read(&router, name).await;
    assert_eq!(
        status_of(case, &failed),
        BLUEPRINT_STATUS_FAILED,
        "case {case}: the registration must come to rest {BLUEPRINT_STATUS_FAILED}, got {failed}"
    );

    // The archive lands at the very location the registration named — after that
    // registration already came to rest.
    let staged = stage_archive(&router, key, PUBLISHED_ARCHIVE.to_vec()).await;
    assert_eq!(
        staged, location,
        "case {case}: the archive must be staged at the location the create named, got {staged:?}"
    );

    // Read again, with the clock asked once more for anything it owes this record: a status
    // that is terminal does not move, and the registration reports the same record it did
    // before the archive landed.
    for turn in ["read again", "read again with the clock asked once more"] {
        if turn == "read again with the clock asked once more" {
            make_settlement_due(&engine, name);
        }
        let again = read(&router, name).await;
        assert_eq!(
            again, failed,
            "case {case} ({turn}): a {BLUEPRINT_STATUS_FAILED} registration must report the \
             same record; first {failed}, then {again}"
        );
        assert_ne!(
            status_of(case, &again),
            BLUEPRINT_STATUS_ACTIVE,
            "case {case} ({turn}): {BLUEPRINT_STATUS_ACTIVE} is unreachable for a registration \
             whose archive could not be read, got {again}"
        );
    }
}

/// What a client that ONLY polls `GetBlueprint` observes, with the due instant left where the
/// service put it: each registration leaves `CREATING` for the terminal status its own archive
/// decides — the served one `ACTIVE` with no error, the unserved one `FAILED` carrying the
/// location it could not read.
#[tokio::test]
async fn a_polling_client_observes_the_terminal_status_each_archive_decides() {
    let (router, _engine) = emulator_with_s3();
    create_bucket(&router).await;

    let served = stage_archive(&router, "polled/served.zip", PUBLISHED_ARCHIVE.to_vec()).await;
    let unserved = format!("s3://{BUCKET}/polled/never-served.zip");
    let cases = [
        (
            "an archive the object store serves",
            "polled-served",
            &served,
            BLUEPRINT_STATUS_ACTIVE,
        ),
        (
            "an archive the object store does not hold",
            "polled-unserved",
            &unserved,
            BLUEPRINT_STATUS_FAILED,
        ),
    ];
    for (_, name, location, _) in cases {
        register(&router, name, location).await;
    }

    for (case, name, location, settles_to) in cases {
        // The poll waits for the CONDITION — that the registration has left the status the
        // create put it in — against a deadline far longer than the span the service owes it,
        // so nothing here depends on how fast the host runs, and every status the poller saw
        // is kept so the case can say what a client observes.
        let deadline = std::time::SystemTime::now() + RUN_AT_REST_DEADLINE;
        let mut observed: Vec<String> = Vec::new();
        let settled = loop {
            let reported = read(&router, name).await;
            let status = status_of(case, &reported);
            if observed.last() != Some(&status) {
                observed.push(status.clone());
            }
            if status != BLUEPRINT_STATUS_CREATING {
                break reported;
            }
            assert!(
                std::time::SystemTime::now() < deadline,
                "case {case}: the registration of {name} must leave {BLUEPRINT_STATUS_CREATING}, \
                 and it reports {reported}"
            );
            tokio::time::sleep(RUN_POLL_INTERVAL).await;
        };

        // A poller observes the registration being made and then its terminal status, and
        // never a status outside those two — how many turns it spent in each depends on when
        // it polled, so nothing is asserted about that.
        let expected_statuses = [BLUEPRINT_STATUS_CREATING, settles_to];
        assert!(
            observed
                .iter()
                .all(|status| expected_statuses.contains(&status.as_str())),
            "case {case}: a poller must observe only {expected_statuses:?}, got {observed:?}"
        );
        assert_eq!(
            observed.last().map(String::as_str),
            Some(settles_to),
            "case {case}: the last status a poller observes is the one the registration came \
             to rest in, got {observed:?}"
        );

        assert_eq!(
            status_of(case, &settled),
            settles_to,
            "case {case}: expected the polled registration to come to rest {settles_to}, got \
             {settled}"
        );
        let expected = (settles_to == BLUEPRINT_STATUS_FAILED)
            .then(|| expected_failure_message(location.as_str()));
        assert_eq!(
            error_message_of(&settled),
            expected,
            "case {case}: expected the error message of a {settles_to} registration, got {settled}"
        );
    }
}
