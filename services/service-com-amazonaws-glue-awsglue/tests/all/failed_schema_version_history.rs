// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for what a schema set holds once a registration FAILS its compatibility
//! check: the version is stored, not refused.
//!
//! The rule under test: `RegisterSchemaVersion` reports a definition that "does not meet the
//! compatibility requirements of the schema set" through the version's own
//! `SchemaVersionStatus` rather than by refusing the call ("You can call the GetSchemaVersion
//! API with the SchemaVersionId to check compatibility modes" — `smithy_query
//! operation=RegisterSchemaVersion`), so such a version is a FULL member of the set's
//! history: it answers 200, takes the set's next `VersionNumber`, moves the set's
//! `LatestSchemaVersion` and `NextSchemaVersion` onto itself while leaving its
//! `SchemaCheckpoint` where it was, is listed by `ListSchemaVersions`, reads back by its
//! `SchemaVersionId` and resolves from its exact definition through `GetSchemaByDefinition` —
//! each read reporting the stored `FAILURE` status, which is a property of the version and
//! not of the read (`GetSchemaVersion` and `GetSchemaByDefinition` both project `Status:
//! SchemaVersionStatus`). A later registration of the same definition registers nothing and
//! reports that stored version, and a later registration of another definition takes the
//! number AFTER the failed one.
//!
//! The suites are TABLES whose rows differ along what the rule generalises over: the
//! `Compatibility` mode the schema set was created with, which comparison the registration
//! fails (a field added without a default, a required field dropped, a field's type changed),
//! and where in the set's history the failure happens (the second version, or a third one
//! after a compatible registration). The definitions are the ones
//! [`crate::schema_version_compatibility`] pins the per-mode verdicts of, so the two suites
//! cannot disagree about which difference is incompatible under which mode.
//!
//! Every call goes over the wire through the shared [`crate::wire`] harness.

use serde_json::{json, Value};
use service_com_amazonaws_glue_awsglue::state::{
    DEFAULT_REGISTRY_NAME, FIRST_SCHEMA_VERSION, SCHEMA_VERSION_STATUS_AVAILABLE,
    SCHEMA_VERSION_STATUS_FAILURE,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::schema_version_compatibility::{
    ADD_OPTIONAL, ADD_REQUIRED, BASE, CHANGE_TYPE, DROP_FIELD,
};
use crate::wire::{emulator, ok};

/// A further evolution of [`BASE`] that adds a field carrying a default, and so is
/// compatible in both directions with every definition a row below registers before it: the
/// registration that follows a failed one, which must take the number after it.
const FOLLOWER: &str = concat!(
    r#"{"type":"record","name":"User","fields":[{"name":"id","type":"string"},"#,
    r#"{"name":"tag","type":["null","string"],"default":null}]}"#
);

/// One row: a schema set created with `mode` holding [`BASE`], the compatible registrations
/// made against it before the failing one, and the definition whose check fails under that
/// mode.
struct Row {
    case: &'static str,
    /// The schema set's name. `SchemaRegistryNameString` admits `^[a-zA-Z0-9-_$#.]+$` only.
    schema: &'static str,
    mode: &'static str,
    /// Registrations made before the failing one, each of which must settle `AVAILABLE`.
    leading: &'static [&'static str],
    /// The definition whose compatibility check fails under `mode`.
    incompatible: &'static str,
}

/// The rows every suite here runs: each `Compatibility` mode that asks for a check in one of
/// the two directions, the difference that breaks it, and a chain long enough to place the
/// failure past the set's second version.
///
/// Which difference is incompatible under which mode is pinned by
/// [`crate::schema_version_compatibility`]: a field added without a default breaks a
/// backward check, a required field dropped breaks a forward one, and a field's type changed
/// breaks either.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "BACKWARD, a field added without a default",
            schema: "users-backward-failed",
            mode: "BACKWARD",
            leading: &[],
            incompatible: ADD_REQUIRED,
        },
        Row {
            case: "BACKWARD, a type change after a compatible version",
            schema: "users-backward-chain-failed",
            mode: "BACKWARD",
            leading: &[ADD_OPTIONAL],
            incompatible: CHANGE_TYPE,
        },
        Row {
            case: "BACKWARD_ALL, a type change after a compatible version",
            schema: "users-backward-all-failed",
            mode: "BACKWARD_ALL",
            leading: &[ADD_OPTIONAL],
            incompatible: CHANGE_TYPE,
        },
        Row {
            case: "FORWARD, the required field dropped",
            schema: "users-forward-failed",
            mode: "FORWARD",
            leading: &[],
            incompatible: DROP_FIELD,
        },
        Row {
            case: "FULL, a field added without a default",
            schema: "users-full-failed",
            mode: "FULL",
            leading: &[],
            incompatible: ADD_REQUIRED,
        },
    ]
}

/// A string member of a response body.
fn text(case: &str, body: &Value, member: &str) -> String {
    body.get(member)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case}: {member} must be a string, got {body}"))
        .to_string()
}

/// A numeric member of a response body.
fn number(case: &str, body: &Value, member: &str) -> i64 {
    body.get(member)
        .and_then(Value::as_i64)
        .unwrap_or_else(|| panic!("case {case}: {member} must be a number, got {body}"))
}

/// Register `definition` against `schema` and return the registration's answer, which must
/// be served (the compatibility verdict travels in `Status`, never as a refusal).
async fn register(router: &MultiServiceRouter, schema: &str, definition: &str) -> Value {
    ok(
        router,
        "RegisterSchemaVersion",
        &json!({
            "SchemaId": { "SchemaName": schema },
            "SchemaDefinition": definition,
        }),
    )
    .await
}

/// Drive one row's chain: create the schema set, make its leading registrations, then
/// register the incompatible definition. Reports that registration's answer beside the
/// `VersionNumber` it must carry.
async fn given_failed_version(router: &MultiServiceRouter, row: &Row) -> (Value, i64) {
    let created = ok(
        router,
        "CreateSchema",
        &json!({
            "SchemaName": row.schema,
            "DataFormat": "AVRO",
            "Compatibility": row.mode,
            "SchemaDefinition": BASE,
        }),
    )
    .await;
    assert_eq!(
        text(row.case, &created, "SchemaVersionStatus"),
        SCHEMA_VERSION_STATUS_AVAILABLE,
        "case {}: the version a create registers is never compatibility-checked, got {created}",
        row.case
    );

    let mut expected_number = FIRST_SCHEMA_VERSION;
    for leading in row.leading {
        expected_number += 1;
        let registered = register(router, row.schema, leading).await;
        assert_eq!(
            text(row.case, &registered, "Status"),
            SCHEMA_VERSION_STATUS_AVAILABLE,
            "case {}: the chain's version {expected_number} is compatible under {}, got \
             {registered}",
            row.case,
            row.mode
        );
        assert_eq!(
            number(row.case, &registered, "VersionNumber"),
            expected_number,
            "case {}: expected the set's next version number, got {registered}",
            row.case
        );
    }

    expected_number += 1;
    let failed = register(router, row.schema, row.incompatible).await;
    assert_eq!(
        text(row.case, &failed, "Status"),
        SCHEMA_VERSION_STATUS_FAILURE,
        "case {}: the registration breaks {} compatibility, so it must settle {}, got {failed}",
        row.case,
        row.mode,
        SCHEMA_VERSION_STATUS_FAILURE
    );
    assert_eq!(
        number(row.case, &failed, "VersionNumber"),
        expected_number,
        "case {}: a failed version still takes the set's next version number, got {failed}",
        row.case
    );
    (failed, expected_number)
}

/// The `Schemas` entries of a `ListSchemaVersions` answer, in the order served.
async fn listed_versions(router: &MultiServiceRouter, case: &str, schema: &str) -> Vec<Value> {
    let listed = ok(
        router,
        "ListSchemaVersions",
        &json!({ "SchemaId": { "SchemaName": schema } }),
    )
    .await;
    listed["Schemas"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case}: Schemas must be a list, got {listed}"))
        .clone()
}

/// The schema set as `GetSchema` reports it. The read takes a COMPLETE reference — "at
/// least one of (registryName and schemaName) or schemaArn" — so the set is named beside
/// the registry a schema create defaults into.
async fn read_schema(router: &MultiServiceRouter, schema: &str) -> Value {
    ok(
        router,
        "GetSchema",
        &json!({
            "SchemaId": { "SchemaName": schema, "RegistryName": DEFAULT_REGISTRY_NAME },
        }),
    )
    .await
}

// ---------------------------------------------------------------------------
// What the schema set holds once a registration failed its check
// ---------------------------------------------------------------------------

/// A version whose compatibility check failed is stored and reported everywhere the set's
/// versions are reported, carrying the `FAILURE` status it was stored with: the read by id,
/// the listing, the read of the set's latest version, and the lookup by definition all agree
/// on it, and the set's counters have moved onto it.
#[tokio::test]
async fn a_failed_version_is_held_as_the_schema_sets_latest_version() {
    let router = emulator();

    for row in rows() {
        let case = row.case;
        let (failed, failed_number) = given_failed_version(&router, &row).await;
        let version_id = text(case, &failed, "SchemaVersionId");

        // The read by `SchemaVersionId` the operation's own documentation directs a caller
        // to: the definition registered, under the status it was stored with.
        let read = ok(
            &router,
            "GetSchemaVersion",
            &json!({ "SchemaVersionId": version_id }),
        )
        .await;
        assert_eq!(
            text(case, &read, "Status"),
            SCHEMA_VERSION_STATUS_FAILURE,
            "case {case}: the stored status is what a later read reports, got {read}"
        );
        assert_eq!(
            text(case, &read, "SchemaDefinition"),
            row.incompatible,
            "case {case}: the definition registered is the one read back, got {read}"
        );
        assert_eq!(
            number(case, &read, "VersionNumber"),
            failed_number,
            "case {case}: expected version {failed_number} on the read, got {read}"
        );

        // The set's own counters moved onto the failed version, and its checkpoint — "the
        // last time the compatibility mode was changed" — did not.
        let schema_read = read_schema(&router, row.schema).await;
        assert_eq!(
            number(case, &schema_read, "LatestSchemaVersion"),
            failed_number,
            "case {case}: expected the latest version to be the failed one, got {schema_read}"
        );
        assert_eq!(
            number(case, &schema_read, "NextSchemaVersion"),
            failed_number + 1,
            "case {case}: expected the next version to follow the failed one, got {schema_read}"
        );
        assert_eq!(
            number(case, &schema_read, "SchemaCheckpoint"),
            FIRST_SCHEMA_VERSION,
            "case {case}: a failed registration does not move the checkpoint, got {schema_read}"
        );

        // The listing holds every version of the set, in registration order, with each one's
        // own status: the failed one is the last of them.
        let listed = listed_versions(&router, case, row.schema).await;
        let numbers: Vec<i64> = listed
            .iter()
            .map(|entry| number(case, entry, "VersionNumber"))
            .collect();
        let expected_numbers: Vec<i64> = (FIRST_SCHEMA_VERSION..=failed_number).collect();
        assert_eq!(
            numbers, expected_numbers,
            "case {case}: expected the listing to hold {expected_numbers:?}, got {numbers:?}"
        );
        let statuses: Vec<String> = listed
            .iter()
            .map(|entry| text(case, entry, "Status"))
            .collect();
        let mut expected_statuses: Vec<String> = (FIRST_SCHEMA_VERSION..failed_number)
            .map(|_| SCHEMA_VERSION_STATUS_AVAILABLE.to_string())
            .collect();
        expected_statuses.push(SCHEMA_VERSION_STATUS_FAILURE.to_string());
        assert_eq!(
            statuses, expected_statuses,
            "case {case}: expected the listed statuses {expected_statuses:?}, got {statuses:?}"
        );
        let last = listed
            .last()
            .unwrap_or_else(|| panic!("case {case}: the listing holds the versions registered"));
        assert_eq!(
            text(case, last, "SchemaVersionId"),
            version_id,
            "case {case}: the failed version is the last the listing holds, got {listed:?}"
        );

        // "The latest version available for the schema" is that same version, so the set's
        // reported latest version and the one this read resolves cannot disagree.
        let latest = ok(
            &router,
            "GetSchemaVersion",
            &json!({
                "SchemaId": { "SchemaName": row.schema },
                "SchemaVersionNumber": { "LatestVersion": true },
            }),
        )
        .await;
        assert_eq!(
            text(case, &latest, "SchemaVersionId"),
            version_id,
            "case {case}: the set's latest version is the failed one, got {latest}"
        );
        assert_eq!(
            text(case, &latest, "Status"),
            SCHEMA_VERSION_STATUS_FAILURE,
            "case {case}: expected the stored status on the latest-version read, got {latest}"
        );

        // The failed version's exact bytes still resolve back to it.
        let by_definition = ok(
            &router,
            "GetSchemaByDefinition",
            &json!({
                "SchemaId": { "SchemaName": row.schema },
                "SchemaDefinition": row.incompatible,
            }),
        )
        .await;
        assert_eq!(
            text(case, &by_definition, "SchemaVersionId"),
            version_id,
            "case {case}: the definition registered resolves to the version holding it, got \
             {by_definition}"
        );
        assert_eq!(
            text(case, &by_definition, "Status"),
            SCHEMA_VERSION_STATUS_FAILURE,
            "case {case}: expected the stored status on the definition lookup, got \
             {by_definition}"
        );
    }
}

// ---------------------------------------------------------------------------
// What the registrations after it do
// ---------------------------------------------------------------------------

/// The failed version is stored, so registering the SAME definition again registers nothing
/// and reports that version — and registering another definition takes the number AFTER it,
/// leaving the failed version readable where it is.
#[tokio::test]
async fn the_registrations_after_a_failed_version_see_it_in_the_history() {
    let router = emulator();

    for row in rows() {
        let case = row.case;
        let (failed, failed_number) = given_failed_version(&router, &row).await;
        let version_id = text(case, &failed, "SchemaVersionId");

        // "If the same schema definition is already stored in Schema Registry as a version,
        // the schema ID of the existing schema is returned to the caller": the failed
        // version is one of those, so this registration mints nothing.
        let again = register(&router, row.schema, row.incompatible).await;
        assert_eq!(
            text(case, &again, "SchemaVersionId"),
            version_id,
            "case {case}: re-registering the stored definition reports its version, got {again}"
        );
        assert_eq!(
            number(case, &again, "VersionNumber"),
            failed_number,
            "case {case}: re-registering mints no version, got {again}"
        );
        assert_eq!(
            text(case, &again, "Status"),
            SCHEMA_VERSION_STATUS_FAILURE,
            "case {case}: the version reported carries its stored status, got {again}"
        );
        let listed = listed_versions(&router, case, row.schema).await;
        assert_eq!(
            listed.len(),
            usize::try_from(failed_number).expect("a version number counts up from one"),
            "case {case}: the set holds the versions it held before, got {listed:?}"
        );

        // A definition compatible with every version the set holds as available takes the
        // number after the failed one: a failed version occupies its own.
        let follower = register(&router, row.schema, FOLLOWER).await;
        assert_eq!(
            text(case, &follower, "Status"),
            SCHEMA_VERSION_STATUS_AVAILABLE,
            "case {case}: the follower is compatible with every available version, got \
             {follower}"
        );
        assert_eq!(
            number(case, &follower, "VersionNumber"),
            failed_number + 1,
            "case {case}: expected the number after the failed version, got {follower}"
        );
        let schema_read = read_schema(&router, row.schema).await;
        assert_eq!(
            number(case, &schema_read, "LatestSchemaVersion"),
            failed_number + 1,
            "case {case}: the set's latest version is the follower, got {schema_read}"
        );

        // The failed version is where it was, with the status it was stored with.
        let read = ok(
            &router,
            "GetSchemaVersion",
            &json!({ "SchemaVersionId": version_id }),
        )
        .await;
        assert_eq!(
            (
                text(case, &read, "Status"),
                number(case, &read, "VersionNumber")
            ),
            (SCHEMA_VERSION_STATUS_FAILURE.to_string(), failed_number),
            "case {case}: a later registration leaves the failed version as it was, got {read}"
        );
    }
}
