// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for an OpenSearch application's saved-object migrations:
//! `StartMigration` and the `GetMigration` read it round-trips through.
//!
//! Every case drives the real wire path — a `restJson1` HTTP request through the
//! multi-service router into the mounted service — so what is asserted is what a client
//! sees, not what a handler returns. The harness comes from [`super::domain_lifecycle`]
//! and the application a migration migrates into from
//! [`super::data_source_attachments`]; this suite adds no second copy of either.
//!
//! The tests assert RULES over sets of inputs, not one recorded call:
//!
//! * a start mints ONE job from the request and the read reports that same stored
//!   record, over a table of nine starts differing in the export scope they name (none,
//!   types only, one object, several, duplicates of one object, a deep-reference
//!   expansion), in the target workspace (an existing one, a new one), in the
//!   conflict-resolution strategy (none, and each published value) and in the shape of
//!   the source ARN;
//! * the job is started in a transient status and the clock owes it the statuses running
//!   it leaves it in: `PENDING` the instant the call returns, then `IN_PROGRESS` once the
//!   export has run and `SUCCEEDED` once the import has, in that order and without any
//!   call having asked for a move, with the counts the job reports never going backwards;
//! * what the job exports is what its export options selected, counted once per distinct
//!   saved object, and a job that succeeded imported every object it exported;
//! * `clientToken` makes a start idempotent: the same token twice answers with the one
//!   job the first call started, while the same token for a different application is the
//!   modelled `ConflictException`;
//! * a request naming no migration target, two at once, an unsupported conflict
//!   resolution or a source that is no collection ARN is refused with the modelled
//!   `ValidationException` and starts nothing;
//! * an `applicationId` naming no application of this account, and a `migrationId`
//!   naming no job, are refused with the modelled `ResourceNotFoundException`;
//! * the SOURCE is resolved against the application's own data sources — linked by the
//!   create, named by an update or attached afterwards — so a `datasourceArn` it holds none
//!   under is refused with the modelled `ResourceNotFoundException` naming the data source and
//!   the application, and mints nothing, while every way one reaches the application is
//!   served;
//! * an `applicationId` the account holds no application under is refused by the start and by
//!   the listing of its jobs, each in the words its own surface states that miss in.

use serde_json::{json, Value};
use service_com_amazonaws_opensearch_amazonopensearchservice::state::{
    APPLICATION_STATUS_ACTIVE, CONFLICT_RESOLUTION_VALUES, MIGRATION_STATUS_IN_PROGRESS,
    MIGRATION_STATUS_PENDING, MIGRATION_STATUS_SUCCEEDED, MIGRATION_STATUS_VALUES, PROVISIONING,
};

use super::data_source_attachments::application;
use super::domain_lifecycle::{
    error_code, poll_until, string_member, Answer, Emulator, PREFIX, RECORDED_NOT_FOUND_STATUS,
};

/// The ARN of the data source a row migrates saved objects out of, unless it names
/// another.
///
/// Saved objects are exported from a search COLLECTION, which is the shape
/// `StartMigration` validates its source ARN against, so every row that expects a start
/// to be served names one — held by the account this emulator serves
/// (`engine::emulator_account_id`), because a source in another account names a collection
/// no request of this account can export from.
const SOURCE: &str = "arn:aws:aoss:us-east-1:000000000000:collection/savedobjects";

/// `StartMigration`: `POST /2021-01-01/opensearch/app-migrations`.
pub(crate) async fn start_migration(emulator: &Emulator, body: Value) -> Answer {
    emulator
        .call(
            "POST",
            &format!("{PREFIX}/opensearch/app-migrations"),
            Some(body),
        )
        .await
}

/// An application of this account that is ACTIVE — the condition a start is taken on: a
/// migration is started INTO an application's workspace, so one still being provisioned is
/// not yet an application to migrate into.
///
/// Polls the STATUS the clock owes the application rather than sleeping for the provisioning
/// span, so a case that needs an application to migrate into cannot depend on how fast the
/// host runs. Shared with the sibling suites that start a migration.
pub(crate) async fn active_application(emulator: &Emulator, name: &str) -> String {
    let id = application(emulator, name).await;
    let case = format!("the application {name:?} becoming active");
    poll_until(&case, || async {
        let read = emulator
            .call(
                "GET",
                &format!("{PREFIX}/opensearch/application/{id}"),
                None,
            )
            .await;
        read.body["status"].as_str() == Some(APPLICATION_STATUS_ACTIVE)
    })
    .await;
    id
}

/// The same ACTIVE application, holding `source` as one of its data sources — the state a
/// start is taken on, since a job exports from a data source OF the application it migrates
/// into.
///
/// The data source is LINKED by the create (`CreateApplicationRequest$dataSources`, "the data
/// sources to link to the OpenSearch application"), which is one call rather than a create
/// and an attach, and it is the same set an attach adds to — so a row that needs a source the
/// application holds names it here. Shared with the sibling suites that start a migration.
pub(crate) async fn application_with_source(
    emulator: &Emulator,
    name: &str,
    source: &str,
) -> String {
    let answer = emulator
        .call(
            "POST",
            &format!("{PREFIX}/opensearch/application"),
            Some(json!({
                "name": name,
                "dataSources": [{ "dataSourceArn": source }],
            })),
        )
        .await;
    assert_eq!(
        answer.status, 200,
        "case CreateApplication({name:?}, {source}): expected 200, got {} with {:?}",
        answer.status, answer.body
    );
    let id = string_member(&answer.body, "id", name);
    poll_until(&format!("the application {name:?} becoming active"), || async {
        let read = emulator
            .call(
                "GET",
                &format!("{PREFIX}/opensearch/application/{id}"),
                None,
            )
            .await;
        read.body["status"].as_str() == Some(APPLICATION_STATUS_ACTIVE)
    })
    .await;
    id
}

/// The source ARN a start's `migrationOptions` names, read out of the options themselves —
/// what the application that start is taken on has to hold as a data source.
pub(crate) fn named_source(options: &Value) -> &str {
    options["source"]["datasourceArn"].as_str().unwrap_or(SOURCE)
}

/// `GetMigration`: `GET /2021-01-01/opensearch/app-migrations/{migrationId}`.
pub(crate) async fn get_migration(emulator: &Emulator, migration_id: &str) -> Answer {
    emulator
        .call(
            "GET",
            &format!("{PREFIX}/opensearch/app-migrations/{migration_id}"),
            None,
        )
        .await
}

/// A timestamp member of a response, in seconds — the `restJson1` form of a modelled
/// timestamp.
fn instant(body: &Value, member: &str, case: &str) -> f64 {
    body[member].as_f64().unwrap_or_else(|| {
        panic!("case {case}: expected a numeric {member}, got {body:?}")
    })
}

/// One start this suite makes: the migration options it carries and how many distinct
/// saved objects those options select.
struct Row {
    case: &'static str,
    /// The application the row migrates into, by the name it is created under.
    name: &'static str,
    /// The request's `migrationOptions`.
    options: Value,
    /// The number of saved objects the row's `exportOptions` name, counted once per
    /// distinct (type, id) pair — what the export reports having exported.
    selected: i64,
}

/// The table every start case runs: the rows differ in the export scope, the target
/// workspace, the conflict-resolution strategy and the shape of the source ARN.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "no export filter at all",
            name: "migration-whole-source",
            options: json!({
                "source": { "datasourceArn": SOURCE },
                "workspace": { "workspaceId": "ws-reporting" },
            }),
            selected: 0,
        },
        Row {
            case: "a filter naming object types only",
            name: "migration-types-only",
            options: json!({
                "source": { "datasourceArn": SOURCE },
                "workspace": { "workspaceId": "ws-reporting" },
                "exportOptions": { "types": ["dashboard", "visualization"] },
            }),
            selected: 0,
        },
        Row {
            case: "one named object",
            name: "migration-one-object",
            options: json!({
                "source": { "datasourceArn": SOURCE },
                "workspace": { "workspaceId": "ws-reporting" },
                "exportOptions": { "objects": [{ "type": "dashboard", "id": "sales" }] },
            }),
            selected: 1,
        },
        Row {
            case: "several named objects of several types",
            name: "migration-many-objects",
            options: json!({
                "source": { "datasourceArn": SOURCE },
                "workspace": { "workspaceId": "ws-reporting" },
                "exportOptions": {
                    "objects": [
                        { "type": "dashboard", "id": "sales" },
                        { "type": "visualization", "id": "revenue" },
                        { "type": "index-pattern", "id": "orders-*" },
                        { "type": "search", "id": "late-orders" },
                    ],
                },
            }),
            selected: 4,
        },
        Row {
            case: "the same object named twice",
            name: "migration-repeated-object",
            options: json!({
                "source": { "datasourceArn": SOURCE },
                "workspace": { "workspaceId": "ws-reporting" },
                "exportOptions": {
                    "objects": [
                        { "type": "dashboard", "id": "sales" },
                        { "type": "dashboard", "id": "sales" },
                        { "type": "query", "id": "top-sellers" },
                    ],
                },
            }),
            selected: 2,
        },
        Row {
            case: "objects, types and a deep reference expansion together",
            name: "migration-deep-references",
            options: json!({
                "source": { "datasourceArn": SOURCE },
                "workspace": { "workspaceId": "ws-reporting" },
                "exportOptions": {
                    "types": ["dashboard"],
                    "objects": [
                        { "type": "dashboard", "id": "sales" },
                        { "type": "dashboard", "id": "ops" },
                    ],
                    "includeReferencesDeep": true,
                },
            }),
            selected: 2,
        },
        Row {
            case: "a workspace the migration is asked to create",
            name: "migration-new-workspace",
            options: json!({
                "source": { "datasourceArn": SOURCE },
                "workspace": {
                    "createWorkspace": true,
                    "name": "Imported dashboards",
                    "type": "OBSERVABILITY",
                },
                "exportOptions": { "objects": [{ "type": "search", "id": "late-orders" }] },
                "conflictResolution": CONFLICT_RESOLUTION_VALUES[0],
            }),
            selected: 1,
        },
        Row {
            case: "the second published conflict resolution",
            name: "migration-overwrite",
            options: json!({
                "source": { "datasourceArn": SOURCE },
                "workspace": { "workspaceId": "ws-reporting" },
                "exportOptions": {
                    "objects": [
                        { "type": "dashboard", "id": "sales" },
                        { "type": "search", "id": "late-orders" },
                    ],
                },
                "conflictResolution": CONFLICT_RESOLUTION_VALUES[1],
            }),
            selected: 2,
        },
        Row {
            case: "a published conflict resolution spelled in the other case",
            name: "migration-overwrite-upper",
            options: json!({
                "source": { "datasourceArn": SOURCE },
                "workspace": { "workspaceId": "ws-reporting" },
                "exportOptions": {
                    "objects": [
                        { "type": "dashboard", "id": "sales" },
                        { "type": "search", "id": "late-orders" },
                    ],
                },
                // The published set spells one value in upper case and the other in lower
                // ([`CONFLICT_RESOLUTION_VALUES`]), so the strategy is named by its WORD:
                // the same strategy spelled the other way names the same strategy.
                "conflictResolution": CONFLICT_RESOLUTION_VALUES[1].to_uppercase(),
            }),
            selected: 2,
        },
        Row {
            case: "a source ARN in another partition",
            name: "migration-other-partition",
            options: json!({
                "source": {
                    "datasourceArn":
                        "arn:aws-us-gov:aoss:us-gov-west-1:000000000000:collection/savedobjects",
                },
                "workspace": { "workspaceId": "ws-reporting" },
                "exportOptions": { "objects": [{ "type": "index-pattern", "id": "orders-*" }] },
            }),
            selected: 1,
        },
    ]
}

/// RULE: a start mints ONE job from the request, in the status a job is first created
/// in, and the read reports that same stored record — so the start's answer and a later
/// read cannot disagree about one job.
#[tokio::test]
async fn a_start_mints_one_pending_job_the_read_reports() {
    let emulator = Emulator::new();
    let mut started: Vec<(Row, String, String)> = Vec::new();

    for row in rows() {
        let case = row.case;
        let id = application_with_source(&emulator, row.name, named_source(&row.options)).await;
        let answer = start_migration(
            &emulator,
            json!({ "applicationId": id, "migrationOptions": row.options }),
        )
        .await;
        assert_eq!(
            answer.status, 200,
            "case {case}: expected 200 from StartMigration, got {} with {:?}",
            answer.status, answer.body
        );
        let migration_id = string_member(&answer.body, "migrationId", case);
        assert_eq!(
            answer.body["status"],
            json!(MIGRATION_STATUS_PENDING),
            "case {case}: a job is {MIGRATION_STATUS_PENDING} when it is first created, got {:?}",
            answer.body
        );

        // Read straight back: the job is still in the status the start reported, and
        // nothing has been exported or imported yet.
        let read = get_migration(&emulator, &migration_id).await;
        assert_eq!(
            read.status, 200,
            "case {case}: expected 200 from GetMigration({migration_id}), got {} with {:?}",
            read.status, read.body
        );
        assert_eq!(
            read.body["migrationId"],
            json!(migration_id),
            "case {case}: the read reports the job the start minted, got {:?}",
            read.body
        );
        assert_eq!(
            read.body["applicationId"],
            json!(id),
            "case {case}: the read reports the application the start named, got {:?}",
            read.body
        );
        assert_eq!(
            read.body["source"], row.options["source"],
            "case {case}: the read reports the source the start named, got {:?}",
            read.body
        );
        assert_eq!(
            read.body["status"],
            json!(MIGRATION_STATUS_PENDING),
            "case {case}: a job read straight after the start is still \
             {MIGRATION_STATUS_PENDING}, got {:?}",
            read.body
        );
        assert_eq!(
            (read.body["exportedCount"].as_i64(), read.body["importedCount"].as_i64()),
            (Some(0), Some(0)),
            "case {case}: a job whose export has not run has moved no object, got {:?}",
            read.body
        );
        assert_eq!(
            read.body["error"],
            Value::Null,
            "case {case}: a job that has not failed reports no error, got {:?}",
            read.body
        );
        assert_eq!(
            instant(&read.body, "updatedAt", case),
            instant(&read.body, "createdAt", case),
            "case {case}: a job nothing has changed was last changed when it was created, \
             got {:?}",
            read.body
        );
        started.push((row, id, migration_id));
    }

    // The ids are minted per job, so no two of the starts above share one.
    for (one, other) in started.iter().zip(started.iter().skip(1)) {
        assert_ne!(
            one.2, other.2,
            "case {:?} vs {:?}: expected distinct migration ids, got {:?}",
            one.0.case, other.0.case, one.2
        );
    }

    // Nothing asked for a move: the clock owes every job its export and its import, and
    // whichever call reads the record next materialises them — the CONDITION this polls for
    // rather than a stretch of time it waits out.
    for (row, _, migration_id) in &started {
        let case = row.case;
        poll_until(&format!("the job of {case} completing"), || async {
            get_migration(&emulator, migration_id).await.body["status"].as_str()
                == Some(MIGRATION_STATUS_SUCCEEDED)
        })
        .await;
    }

    for (row, id, migration_id) in &started {
        let case = row.case;
        let read = get_migration(&emulator, migration_id).await;
        assert_eq!(
            read.status, 200,
            "case {case}: expected 200 from GetMigration({migration_id}), got {} with {:?}",
            read.status, read.body
        );
        assert_eq!(
            read.body["status"],
            json!(MIGRATION_STATUS_SUCCEEDED),
            "case {case}: a job both of whose steps have fallen due has completed, got {:?}",
            read.body
        );
        assert_eq!(
            read.body["exportedCount"].as_i64(),
            Some(row.selected),
            "case {case}: expected the {} selected object(s) exported, got {:?}",
            row.selected,
            read.body
        );
        assert_eq!(
            read.body["importedCount"].as_i64(),
            Some(row.selected),
            "case {case}: a job that succeeded imported every object it exported, got {:?}",
            read.body
        );
        assert_eq!(
            read.body["applicationId"],
            json!(id),
            "case {case}: the application the job migrates into does not change, got {:?}",
            read.body
        );
        assert!(
            instant(&read.body, "updatedAt", case) > instant(&read.body, "createdAt", case),
            "case {case}: a job the clock has moved was changed after it was created, got {:?}",
            read.body
        );
    }
}

/// RULE: a job the clock owes a move settles without any call asking for it, through the
/// statuses of its lifecycle in order, and the counts it reports never go backwards.
#[tokio::test]
async fn the_clock_moves_a_job_through_its_lifecycle_in_order() {
    let emulator = Emulator::new();
    let id = application_with_source(&emulator, "migration-progression", SOURCE).await;
    let answer = start_migration(
        &emulator,
        json!({
            "applicationId": id,
            "migrationOptions": {
                "source": { "datasourceArn": SOURCE },
                "workspace": { "workspaceId": "ws-reporting" },
                "exportOptions": {
                    "objects": [
                        { "type": "dashboard", "id": "sales" },
                        { "type": "visualization", "id": "revenue" },
                        { "type": "search", "id": "late-orders" },
                    ],
                },
            },
        }),
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "case progression: expected 200 from StartMigration, got {} with {:?}",
        answer.status, answer.body
    );
    let migration_id = string_member(&answer.body, "migrationId", "progression");
    let selected = 3;

    // Polled at a fraction of the span each step takes, so every status the job passes
    // through is observed rather than stepped over.
    let poll = PROVISIONING / 10;
    let mut seen: Vec<String> = Vec::new();
    let mut counts: Vec<(i64, i64)> = Vec::new();
    for attempt in 0..40 {
        let case = format!("progression poll {attempt}");
        let read = get_migration(&emulator, &migration_id).await;
        assert_eq!(
            read.status, 200,
            "case {case}: expected 200 from GetMigration, got {} with {:?}",
            read.status, read.body
        );
        let status = string_member(&read.body, "status", &case);
        assert!(
            MIGRATION_STATUS_VALUES.contains(&status.as_str()),
            "case {case}: expected one of the published statuses \
             {MIGRATION_STATUS_VALUES:?}, got {status:?}"
        );
        let exported = read.body["exportedCount"].as_i64().unwrap_or_default();
        let imported = read.body["importedCount"].as_i64().unwrap_or_default();
        assert!(
            imported <= exported,
            "case {case}: a job cannot import more than it exported, got {imported} of \
             {exported}"
        );
        let expected_counts = match status.as_str() {
            MIGRATION_STATUS_PENDING => (0, 0),
            MIGRATION_STATUS_IN_PROGRESS => (selected, 0),
            _ => (selected, selected),
        };
        assert_eq!(
            (exported, imported),
            expected_counts,
            "case {case}: expected {expected_counts:?} for a {status} job, got {:?}",
            read.body
        );
        if seen.last() != Some(&status) {
            seen.push(status.clone());
        }
        counts.push((exported, imported));
        if status == MIGRATION_STATUS_SUCCEEDED {
            break;
        }
        tokio::time::sleep(poll).await;
    }

    assert_eq!(
        seen,
        vec![
            MIGRATION_STATUS_PENDING.to_string(),
            MIGRATION_STATUS_IN_PROGRESS.to_string(),
            MIGRATION_STATUS_SUCCEEDED.to_string(),
        ],
        "case progression: expected the lifecycle's statuses in order, got {seen:?}"
    );
    for (one, next) in counts.iter().zip(counts.iter().skip(1)) {
        assert!(
            next.0 >= one.0 && next.1 >= one.1,
            "case progression: expected the counts never to go backwards, got {one:?} then \
             {next:?}"
        );
    }

    // A job in a terminal status is owed nothing more, so the NEXT read reports the same
    // record: there is no move to wait for, which is why this reads again straight away
    // rather than waiting out a span.
    let again = get_migration(&emulator, &migration_id).await;
    assert_eq!(
        (
            again.body["status"].clone(),
            again.body["exportedCount"].as_i64(),
            again.body["importedCount"].as_i64()
        ),
        (
            json!(MIGRATION_STATUS_SUCCEEDED),
            Some(selected),
            Some(selected)
        ),
        "case progression: a completed job does not change again, got {:?}",
        again.body
    );
}

/// RULE: a `clientToken` identifies the start of ONE job — a repeat is answered with
/// that job, and the same token for another application is the modelled conflict.
#[tokio::test]
async fn a_client_token_identifies_the_start_of_one_job() {
    let emulator = Emulator::new();
    let one = application_with_source(&emulator, "migration-token-one", SOURCE).await;
    let other = application_with_source(&emulator, "migration-token-other", SOURCE).await;
    let options = json!({
        "source": { "datasourceArn": SOURCE },
        "workspace": { "workspaceId": "ws-reporting" },
    });

    let first = start_migration(
        &emulator,
        json!({
            "applicationId": one,
            "migrationOptions": options,
            "clientToken": "one-start-only",
        }),
    )
    .await;
    assert_eq!(
        first.status, 200,
        "case first start: expected 200, got {} with {:?}",
        first.status, first.body
    );
    let migration_id = string_member(&first.body, "migrationId", "first start");

    let repeat = start_migration(
        &emulator,
        json!({
            "applicationId": one,
            "migrationOptions": options,
            "clientToken": "one-start-only",
        }),
    )
    .await;
    assert_eq!(
        repeat.body["migrationId"],
        json!(migration_id),
        "case repeated token: expected the job the first start made, got {:?}",
        repeat.body
    );

    let fresh = start_migration(
        &emulator,
        json!({
            "applicationId": one,
            "migrationOptions": options,
            "clientToken": "another-start",
        }),
    )
    .await;
    assert_ne!(
        fresh.body["migrationId"],
        json!(migration_id),
        "case another token: expected a second job, got {:?}",
        fresh.body
    );

    let reused = start_migration(
        &emulator,
        json!({
            "applicationId": other,
            "migrationOptions": options,
            "clientToken": "one-start-only",
        }),
    )
    .await;
    assert_eq!(
        (reused.status, error_code(&reused).as_str()),
        (409, "ConflictException"),
        "case token reused for another application: expected the modelled conflict, got {} \
         with {:?}",
        reused.status,
        reused.body
    );

    assert_eq!(
        emulator.store().lock().migrations.len(),
        2,
        "case token reused for another application: expected the two jobs the two distinct \
         tokens started, got {:?}",
        emulator.store().lock().migrations.keys().collect::<Vec<_>>()
    );
}

/// RULE: a request that names no one migration target, an unsupported conflict
/// resolution, a source that is no collection ARN or an application this account does
/// not hold is refused with the error the operation models for it, and starts nothing.
#[tokio::test]
async fn a_request_the_service_cannot_run_is_refused_and_starts_nothing() {
    let emulator = Emulator::new();
    let id = application_with_source(&emulator, "migration-refusals", SOURCE).await;

    // (case, the applicationId, the migrationOptions, the status and code expected)
    let rows: Vec<(&str, String, Value, u16, &str)> = vec![
        (
            "both an existing workspace and a new one",
            id.clone(),
            json!({
                "source": { "datasourceArn": SOURCE },
                "workspace": { "workspaceId": "ws-reporting", "createWorkspace": true },
            }),
            400,
            "ValidationException",
        ),
        (
            "no workspace target at all",
            id.clone(),
            json!({
                "source": { "datasourceArn": SOURCE },
                "workspace": {},
            }),
            400,
            "ValidationException",
        ),
        (
            "a new workspace with no name",
            id.clone(),
            json!({
                "source": { "datasourceArn": SOURCE },
                "workspace": { "createWorkspace": true },
            }),
            400,
            "ValidationException",
        ),
        (
            "a new workspace named with an empty string",
            id.clone(),
            json!({
                "source": { "datasourceArn": SOURCE },
                "workspace": { "createWorkspace": true, "name": "" },
            }),
            400,
            "ValidationException",
        ),
        (
            "a conflict resolution outside the published set",
            id.clone(),
            json!({
                "source": { "datasourceArn": SOURCE },
                "workspace": { "workspaceId": "ws-reporting" },
                "conflictResolution": "REPLACE_EVERYTHING",
            }),
            400,
            "ValidationException",
        ),
        (
            "a source that is no ARN at all",
            id.clone(),
            json!({
                "source": { "datasourceArn": "saved-objects-source-domain" },
                "workspace": { "workspaceId": "ws-reporting" },
            }),
            400,
            "ValidationException",
        ),
        (
            "a source ARN naming no resource",
            id.clone(),
            json!({
                "source": { "datasourceArn": "arn:aws:es:us-east-1:123456789012:" },
                "workspace": { "workspaceId": "ws-reporting" },
            }),
            400,
            "ValidationException",
        ),
        (
            "an application this account does not hold",
            "nosuchapplication".to_string(),
            json!({
                "source": { "datasourceArn": SOURCE },
                "workspace": { "workspaceId": "ws-reporting" },
            }),
            404,
            "ResourceNotFoundException",
        ),
    ];

    for (case, application_id, options, status, code) in rows {
        let answer = start_migration(
            &emulator,
            json!({ "applicationId": application_id, "migrationOptions": options }),
        )
        .await;
        assert_eq!(
            (answer.status, error_code(&answer).as_str()),
            (status, code),
            "case {case:?}: expected {status} {code}, got {} with {:?}",
            answer.status,
            answer.body
        );
    }

    assert!(
        emulator.store().lock().migrations.is_empty(),
        "case refusals: expected a refused start to persist nothing, got {:?}",
        emulator.store().lock().migrations.keys().collect::<Vec<_>>()
    );
}

/// RULE: a job is addressed by its id alone, and that id names the application the job
/// belongs to — so a `migrationId` whose application this account does not hold is
/// refused naming THAT application, one whose application holds no such job is refused
/// naming the job, and both are the modelled `ResourceNotFoundException`.
#[tokio::test]
async fn an_id_naming_no_job_is_refused() {
    let emulator = Emulator::new();
    let id = application_with_source(&emulator, "migration-unknown-ids", SOURCE).await;
    let started = start_migration(
        &emulator,
        json!({
            "applicationId": id,
            "migrationOptions": {
                "source": { "datasourceArn": SOURCE },
                "workspace": { "workspaceId": "ws-reporting" },
            },
        }),
    )
    .await;
    let migration_id = string_member(&started.body, "migrationId", "unknown ids");
    assert!(
        migration_id.starts_with(&format!("{id}-")),
        "case the job that was started: expected the id to name the application it \
         belongs to ({id}), got {migration_id:?}"
    );

    // (the id, the application its leading segment names, whether the refusal's message
    // is pinned). A row whose segment is an application this account holds is refused for
    // the JOB; a row whose segment is a well-formed `ApplicationId` this account does not
    // hold is refused for the APPLICATION, because there is nothing to look a job up in;
    // and a row whose segment is no `ApplicationId` at all names neither, so it is refused
    // for its SHAPE before anything is resolved — the status and the code are pinned for
    // those rows and the sentence is pinned by `control_plane_refusal_wording.rs`.
    let rows: Vec<(String, Option<&str>, bool)> = vec![
        ("mig-000000000000".to_string(), None, true),
        ("migration-1".to_string(), None, true),
        (format!("{migration_id}x"), Some(id.as_str()), true),
        (format!("{id}-000000000000"), Some(id.as_str()), true),
    ];

    for (case, held_by, pinned) in rows {
        let answer = get_migration(&emulator, &case).await;
        // Both refusals are a resource this account does not hold — the APPLICATION the
        // id's leading segment names, or the JOB an application it holds carries none of
        // — so both are answered on the one status this service carries that shape on.
        assert_eq!(
            (answer.status, error_code(&answer).as_str()),
            (RECORDED_NOT_FOUND_STATUS, "ResourceNotFoundException"),
            "case {case:?}: expected {RECORDED_NOT_FOUND_STATUS} ResourceNotFoundException, got \
             {} with {:?}",
            answer.status,
            answer.body
        );
        if !pinned {
            continue;
        }
        let expected = match held_by {
            Some(_) => format!("Migration not found: {case}"),
            None => format!(
                "Application {} not found",
                case.split_once('-').map_or(case.as_str(), |(head, _)| head)
            ),
        };
        assert_eq!(
            answer.body["message"].as_str(),
            Some(expected.as_str()),
            "case {case:?}: expected the refusal to name what could not be resolved, got {:?}",
            answer.body
        );
    }

    // An id whose leading segment is no `ApplicationId` names no application in ANY
    // account, so it is the malformed request it is rather than a resource that happens
    // not to exist.
    for case in ["0".to_string(), migration_id.to_uppercase()] {
        let answer = get_migration(&emulator, &case).await;
        assert_eq!(
            (answer.status, error_code(&answer).as_str()),
            (400, "ValidationException"),
            "case {case:?}: expected 400 ValidationException, got {} with {:?}",
            answer.status,
            answer.body
        );
    }

    let held = get_migration(&emulator, &migration_id).await;
    assert_eq!(
        held.status, 200,
        "case the job that was started: expected 200, got {} with {:?}",
        held.status, held.body
    );
}

// ---------------------------------------------------------------------------
// The SOURCE a start exports from is a data source OF the application
// ---------------------------------------------------------------------------

/// A collection ARN of this account with the id `id`: the shape `StartMigration` validates a
/// source against, so a row below is about the RESOLUTION rather than about the value's shape
/// or its owner.
fn collection(id: &str) -> String {
    format!(
        "arn:aws:aoss:us-east-1:{}:collection/{id}",
        ::engine::emulator_account_id()
    )
}

/// A start of `application_id` from `source`.
async fn start_from(emulator: &Emulator, application_id: &str, source: &str) -> Answer {
    start_migration(
        emulator,
        json!({
            "applicationId": application_id,
            "migrationOptions": {
                "source": { "datasourceArn": source },
                "workspace": { "workspaceId": "ws-source-resolution" },
            },
        }),
    )
    .await
}

/// The sentence this service states about a source the application holds no data source
/// under: the data source as the request spelled it, and the application it was resolved
/// against.
fn not_found_for_application(source: &str, application_id: &str) -> String {
    format!("Data source {source} not found for application {application_id}")
}

/// RULE: saved objects are exported from a data source OF the application a job migrates
/// into, so a `datasourceArn` the application holds no data source under names nothing to
/// export from: the start is refused with the modelled `ResourceNotFoundException` naming both
/// values the lookup was made from, and mints no job.
#[tokio::test]
async fn a_start_from_a_source_the_application_does_not_hold_is_refused_and_starts_nothing() {
    let emulator = Emulator::new();

    // The application under test holds ONE data source, so every refusal below is about the
    // source the request named rather than about an application holding none at all.
    let held_source = collection("heldsource");
    let id = application_with_source(&emulator, "sourceresolution", &held_source).await;

    // Another application holds a second data source, so a row can name a data source that
    // exists — for someone else.
    let other_source = collection("othersource");
    let other = application_with_source(&emulator, "sourceresolutionother", &other_source).await;
    assert_ne!(id, other, "the two applications must be distinct");

    // Rows differ along WHY this application holds no data source under the value: never
    // named, named by another application, and one character away from the one it holds.
    let rows: Vec<(&str, String)> = vec![
        (
            "a collection no call ever named",
            collection("nosuchcollection"),
        ),
        (
            "a collection another application holds",
            other_source.clone(),
        ),
        (
            "a collection whose id differs from the held one by one character",
            collection("heldsources"),
        ),
    ];

    for (case, source) in &rows {
        assert_ne!(
            source, &held_source,
            "case {case}: the row must name another data source than the one held"
        );
        let answer = start_from(&emulator, &id, source).await;
        assert_eq!(
            (answer.status, error_code(&answer).as_str()),
            (RECORDED_NOT_FOUND_STATUS, "ResourceNotFoundException"),
            "case {case}: expected {RECORDED_NOT_FOUND_STATUS} ResourceNotFoundException, got {} \
             with {:?}",
            answer.status,
            answer.body
        );
        let expected = not_found_for_application(source, &id);
        let stated = string_member(&answer.body, "message", case);
        assert_eq!(
            stated, expected,
            "case {case}: expected {expected:?}, got {stated:?}"
        );
    }

    // Nothing was minted: the store holds no job at all.
    assert!(
        emulator.store().lock().migrations.is_empty(),
        "case the refused starts: expected them to mint nothing, got {:?}",
        emulator.store().lock().migrations.keys().collect::<Vec<_>>()
    );

    // The CONTROL: a start from the data source the application DOES hold is served, so the
    // refusals above are about the source and nothing else.
    let served = start_from(&emulator, &id, &held_source).await;
    assert_eq!(
        served.status, 200,
        "case a start from the held data source: expected 200, got {} with {:?}",
        served.status, served.body
    );
}

/// RULE: the lookup is of the application's DATA SOURCES, however one reached it — linked by
/// the create, attached afterwards, or named by an update (which replaces the linked set, so
/// a source it dropped is no longer one the application holds).
#[tokio::test]
async fn a_start_resolves_its_source_however_that_data_source_reached_the_application() {
    let emulator = Emulator::new();

    // Row 1: LINKED by the create.
    let linked = collection("linkedsource");
    let one = application_with_source(&emulator, "sourcelinked", &linked).await;
    let served = start_from(&emulator, &one, &linked).await;
    assert_eq!(
        served.status, 200,
        "case a source linked by the create: expected 200, got {} with {:?}",
        served.status, served.body
    );

    // Row 2: ATTACHED after the application was created, the other way one reaches an
    // application.
    let two = active_application(&emulator, "sourceattached").await;
    let attached_source = collection("attachedsource");
    let attached = emulator
        .call(
            "POST",
            &format!("{PREFIX}/opensearch/application/{two}/attachDataSource"),
            Some(json!({ "dataSourceArn": attached_source })),
        )
        .await;
    assert_eq!(
        attached.status, 200,
        "case AttachDataSource(the attached row): expected 200, got {} with {:?}",
        attached.status, attached.body
    );
    let served = start_from(&emulator, &two, &attached_source).await;
    assert_eq!(
        served.status, 200,
        "case a source attached after the create: expected 200, got {} with {:?}",
        served.status, served.body
    );

    // Row 3: named by an UPDATE of the application, which replaces the set the create linked.
    let three = application_with_source(&emulator, "sourceupdated", &collection("oldsource")).await;
    let updated_source = collection("updatedsource");
    let updated = emulator
        .call(
            "PUT",
            &format!("{PREFIX}/opensearch/application/{three}"),
            Some(json!({ "dataSources": [{ "dataSourceArn": updated_source }] })),
        )
        .await;
    assert_eq!(
        updated.status, 200,
        "case UpdateApplication(the updated row): expected 200, got {} with {:?}",
        updated.status, updated.body
    );
    let served = start_from(&emulator, &three, &updated_source).await;
    assert_eq!(
        served.status, 200,
        "case a source named by an update: expected 200, got {} with {:?}",
        served.status, served.body
    );

    // ...and the source that update REPLACED is no longer one the application holds.
    let answer = start_from(&emulator, &three, &collection("oldsource")).await;
    assert_eq!(
        (answer.status, error_code(&answer).as_str()),
        (RECORDED_NOT_FOUND_STATUS, "ResourceNotFoundException"),
        "case a source the update replaced: expected {RECORDED_NOT_FOUND_STATUS} \
         ResourceNotFoundException, got {} with {:?}",
        answer.status,
        answer.body
    );
}

/// RULE: an `applicationId` the account holds no application under is refused by the start and
/// by the LISTING of its jobs, each in the words its own surface states that miss in, rather
/// than answered with a job id or with an empty listing.
#[tokio::test]
async fn an_application_this_account_does_not_hold_is_refused_by_the_start_and_by_the_listing() {
    let emulator = Emulator::new();
    let source = collection("absentapplication");
    let held = application_with_source(&emulator, "sourceabsentapp", &source).await;

    // Rows differ along the length and the alphabet of the id the request carries, inside the
    // expression the model declares for an application id.
    let absent = [
        "abc".to_string(),
        "a".repeat(30),
        "0123456789".to_string(),
        "nosuchapplicationid".to_string(),
    ];

    for id in &absent {
        assert_ne!(
            id, &held,
            "case {id:?}: the row must name another application"
        );

        let started = start_from(&emulator, id, &source).await;
        assert_eq!(
            (started.status, error_code(&started).as_str()),
            (RECORDED_NOT_FOUND_STATUS, "ResourceNotFoundException"),
            "case StartMigration({id:?}): expected {RECORDED_NOT_FOUND_STATUS} \
             ResourceNotFoundException, got {} with {:?}",
            started.status,
            started.body
        );
        let expected = format!("Application {id} does not exist");
        let stated = string_member(&started.body, "message", id);
        assert_eq!(
            stated, expected,
            "case StartMigration({id:?}): expected {expected:?}, got {stated:?}"
        );

        let listed = emulator
            .call(
                "GET",
                &format!("{PREFIX}/opensearch/app-migrations?applicationId={id}"),
                None,
            )
            .await;
        assert_eq!(
            (listed.status, error_code(&listed).as_str()),
            (RECORDED_NOT_FOUND_STATUS, "ResourceNotFoundException"),
            "case ListMigrations({id:?}): expected {RECORDED_NOT_FOUND_STATUS} \
             ResourceNotFoundException rather than a listing, got {} with {:?}",
            listed.status,
            listed.body
        );
        let expected = format!("Application {id} not found");
        let stated = string_member(&listed.body, "message", id);
        assert_eq!(
            stated, expected,
            "case ListMigrations({id:?}): expected {expected:?}, got {stated:?}"
        );
    }

    // The CONTROL: the application the account DOES hold is served by both.
    let served = start_from(&emulator, &held, &source).await;
    assert_eq!(
        served.status, 200,
        "case the held application: expected 200 from StartMigration, got {} with {:?}",
        served.status, served.body
    );
    let listed = emulator
        .call(
            "GET",
            &format!("{PREFIX}/opensearch/app-migrations?applicationId={held}"),
            None,
        )
        .await;
    assert_eq!(
        listed.status, 200,
        "case the held application: expected 200 from ListMigrations, got {} with {:?}",
        listed.status, listed.body
    );
}
