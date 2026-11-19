// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for `UpdateIntegrationResourceProperty`, and the
//! `GetIntegrationResourceProperty` that reads back what it wrote.
//!
//! Every call goes over the wire through the shared [`crate::wire`] harness, so what is
//! asserted is HTTP behaviour and not a direct call into a handler. A record is addressed the
//! way the create suite addresses one ([`crate::integration_resource_properties`]).
//!
//! RULE under test: the write states ONE side of the integration — the source's processing
//! properties or the target's — of the ONE resource its `ResourceArn` names, so a request
//! stating both sides and a request stating neither are refused on the request body alone,
//! with the operation's modelled `ValidationException`; a `ResourceArn` the service can read
//! as neither leg of an integration is that same class naming the value it was handed; and a
//! leg this account does not hold is the modelled `EntityNotFoundException`. Past them, the
//! resource has exactly one record, so the write is that record's next one — or its first —
//! and what it reports is what a read of that record reports.
//!
//! The recorded calls — a request with neither processing property, one whose ARN names a
//! resource type this API does not accept, and one writing a target's properties against a
//! Glue database ARN — are one row each of the tables below; the rows differ along which side
//! is stated, how many members it carries, which resource kind the ARN names and the form of
//! an ARN that names no leg at all.

use serde_json::{json, Value};
use service_com_amazonaws_glue_awsglue::state::messages;
use smithy_server_runtime::MultiServiceRouter;

use crate::wire::{call, emulator, error_code, error_message, ok, ACCOUNT_ID, REGION};

/// The ARN of the Glue connection `name` — the SOURCE leg of an integration.
fn connection_arn(name: &str) -> String {
    format!("arn:aws:glue:{REGION}:{ACCOUNT_ID}:connection/{name}")
}

/// The ARN of the Glue database `name` — the TARGET leg.
fn database_arn(name: &str) -> String {
    format!("arn:aws:glue:{REGION}:{ACCOUNT_ID}:database/{name}")
}

/// The role either side's properties name: "the IAM role to access the Glue
/// connection"/"database".
fn role_arn(name: &str) -> String {
    format!("arn:aws:iam::{ACCOUNT_ID}:role/{name}")
}

/// Register the Glue connection a source's properties belong to.
async fn given_connection(router: &MultiServiceRouter, name: &str) {
    ok(
        router,
        "CreateConnection",
        &json!({
            "ConnectionInput": {
                "Name": name,
                "ConnectionType": "JDBC",
                "ConnectionProperties": {
                    "JDBC_CONNECTION_URL": "jdbc:postgresql://db.internal:5432/warehouse",
                    "USERNAME": "etl",
                },
            },
        }),
    )
    .await;
}

/// Register the Glue database a target's properties belong to.
async fn given_database(router: &MultiServiceRouter, name: &str) {
    ok(
        router,
        "CreateDatabase",
        &json!({ "DatabaseInput": { "Name": name } }),
    )
    .await;
}

/// Write one record through the update and answer the response body.
async fn update_property(router: &MultiServiceRouter, body: &Value) -> Value {
    ok(router, "UpdateIntegrationResourceProperty", body).await
}

/// What `GetIntegrationResourceProperty` reports for `resource_arn`.
async fn get_property(router: &MultiServiceRouter, resource_arn: &str) -> Value {
    ok(
        router,
        "GetIntegrationResourceProperty",
        &json!({ "ResourceArn": resource_arn }),
    )
    .await
}

// ---------------------------------------------------------------------------
// The write, and the read that reports it
// ---------------------------------------------------------------------------

/// What the update states is what the read reports: every member of the side it stated comes
/// back from the store, under the one `ResourcePropertyArn` that resource derives — and the
/// record the resource already had is WRITTEN rather than duplicated, with the side the
/// request says nothing about left alone.
///
/// The rows differ along which side is stated, how many members it carries, and which
/// resource kind the ARN names.
#[tokio::test]
async fn an_update_states_one_side_and_the_read_reports_it() {
    let router = emulator();
    given_connection(&router, "source-connection").await;
    given_database(&router, "target_database").await;

    let cases: Vec<(&str, String, Value)> = vec![
        (
            "a target's role alone, against a database ARN",
            database_arn("target_database"),
            json!({ "TargetProcessingProperties": { "RoleArn": role_arn("target-role") } }),
        ),
        (
            "a target carrying every member it has",
            database_arn("target_database"),
            json!({
                "TargetProcessingProperties": {
                    "RoleArn": role_arn("target-role-2"),
                    "KmsArn": format!("arn:aws:kms:{REGION}:{ACCOUNT_ID}:key/abcd-1234"),
                    "ConnectionName": "source-connection",
                    "EventBusArn": format!(
                        "arn:aws:events:{REGION}:{ACCOUNT_ID}:event-bus/integration"
                    ),
                },
            }),
        ),
        (
            "a source's role alone, against a connection ARN",
            connection_arn("source-connection"),
            json!({ "SourceProcessingProperties": { "RoleArn": role_arn("source-role") } }),
        ),
    ];

    for (case, resource_arn, stated) in &cases {
        let mut body = json!({ "ResourceArn": resource_arn });
        for (member, value) in stated.as_object().expect("the row states a side") {
            body[member] = value.clone();
        }
        let written = update_property(&router, &body).await;

        assert_eq!(
            written.get("ResourceArn").and_then(Value::as_str),
            Some(resource_arn.as_str()),
            "case {case}: the write names the resource it wrote, got {written}"
        );
        for (member, value) in stated.as_object().expect("the row states a side") {
            assert_eq!(
                written.get(member),
                Some(value),
                "case {case}: the write reports {member} as stated, got {written}"
            );
        }

        // The read reports the same record, member for member — including the
        // `ResourcePropertyArn` the resource derives, which both calls answer.
        let read = get_property(&router, resource_arn).await;
        assert_eq!(
            read, written,
            "case {case}: the read must report what the write stored, expected {written}, got \
             {read}"
        );
        assert!(
            read.get("ResourcePropertyArn")
                .and_then(Value::as_str)
                .is_some_and(|arn| arn.contains("integrationresourceproperty")),
            "case {case}: the record carries its own ARN, got {read}"
        );
    }

    // One record per resource: the two target writes above wrote ONE record, so the second
    // one's members are what the database's record now holds.
    let read = get_property(&router, &database_arn("target_database")).await;
    assert_eq!(
        read.pointer("/TargetProcessingProperties/RoleArn")
            .and_then(Value::as_str),
        Some(role_arn("target-role-2").as_str()),
        "the later write states the record, got {read}"
    );
    assert_eq!(
        read.get("SourceProcessingProperties"),
        None,
        "a target write says nothing about the source side, got {read}"
    );
}

/// A write for a resource whose record a CREATE made is that record's next write: the stated
/// side replaces the stored one and the other side is left alone, under the same
/// `ResourcePropertyArn`.
#[tokio::test]
async fn an_update_writes_the_record_a_create_made() {
    let router = emulator();
    given_connection(&router, "shared-connection").await;
    let resource_arn = connection_arn("shared-connection");

    let created = ok(
        &router,
        "CreateIntegrationResourceProperty",
        &json!({
            "ResourceArn": &resource_arn,
            "SourceProcessingProperties": { "RoleArn": role_arn("created-role") },
        }),
    )
    .await;
    let updated = update_property(
        &router,
        &json!({
            "ResourceArn": &resource_arn,
            "SourceProcessingProperties": { "RoleArn": role_arn("updated-role") },
        }),
    )
    .await;

    assert_eq!(
        updated.get("ResourcePropertyArn"),
        created.get("ResourcePropertyArn"),
        "the record keeps the ARN its resource derives, created {created}, updated {updated}"
    );
    let read = get_property(&router, &resource_arn).await;
    assert_eq!(
        read.pointer("/SourceProcessingProperties/RoleArn")
            .and_then(Value::as_str),
        Some(role_arn("updated-role").as_str()),
        "the update states the stored record, got {read}"
    );
}

// ---------------------------------------------------------------------------
// The validation boundaries
// ---------------------------------------------------------------------------

/// A request that states neither side, and one that states both, are refused on the request
/// body alone — before the `ResourceArn` is read at all, so even an ARN that names nothing
/// earns the body's refusal — and nothing is stored for either.
#[tokio::test]
async fn a_request_that_states_neither_or_both_sides_is_refused() {
    let router = emulator();
    given_database(&router, "boundary_database").await;
    let resource_arn = database_arn("boundary_database");

    for (case, body, expected_message) in [
        (
            "neither side, against a resource that exists",
            json!({ "ResourceArn": &resource_arn }),
            messages::INTEGRATION_PROCESSING_PROPERTIES_REQUIRED.to_string(),
        ),
        (
            "neither side, against an ARN that names nothing",
            json!({ "ResourceArn": "arn:aws:rds:us-east-1:000000000000:cluster:orders" }),
            messages::INTEGRATION_PROCESSING_PROPERTIES_REQUIRED.to_string(),
        ),
        (
            "neither side, against a value that is no ARN",
            json!({ "ResourceArn": "not-an-arn" }),
            messages::INTEGRATION_PROCESSING_PROPERTIES_REQUIRED.to_string(),
        ),
        (
            "both sides at once",
            json!({
                "ResourceArn": &resource_arn,
                "SourceProcessingProperties": { "RoleArn": role_arn("source-role") },
                "TargetProcessingProperties": { "RoleArn": role_arn("target-role") },
            }),
            service_com_amazonaws_glue_awsglue::service::integration_properties::PROCESSING_PROPERTIES_SIDES_EXCLUSIVE
                .to_string(),
        ),
    ] {
        let (status, answer) = call(&router, "UpdateIntegrationResourceProperty", &body).await;
        assert_eq!(
            status, 400,
            "case {case}: expected 400, got {status} with {answer}"
        );
        assert!(
            error_code(&answer).contains("ValidationException"),
            "case {case}: expected ValidationException, got {answer}"
        );
        assert_eq!(
            error_message(&answer),
            expected_message,
            "case {case}: expected the body refusal, got {answer}"
        );
    }

    // Nothing was stored: the record the resource would carry does not exist.
    let (status, answer) = call(
        &router,
        "GetIntegrationResourceProperty",
        &json!({ "ResourceArn": &resource_arn }),
    )
    .await;
    assert_eq!(
        status, 400,
        "a refused write must store nothing, got {status} with {answer}"
    );
}

/// A `ResourceArn` the service can read as neither leg of an integration is refused as
/// invalid, naming the value the request sent.
///
/// The rows span the forms such a value takes: another service's ARN (the recorded case's RDS
/// cluster), a Glue ARN of a resource type this API does not accept, an ARN of another
/// partition's Glue, and a value that is no ARN at all.
#[tokio::test]
async fn a_resource_arn_that_names_no_integration_leg_is_refused_as_invalid() {
    let router = emulator();

    for (case, resource_arn) in [
        (
            "an RDS cluster ARN",
            format!("arn:aws:rds:{REGION}:{ACCOUNT_ID}:cluster:orders"),
        ),
        (
            "a Redshift namespace ARN",
            format!("arn:aws:redshift:{REGION}:{ACCOUNT_ID}:namespace/orders"),
        ),
        (
            "a Glue crawler ARN",
            format!("arn:aws:glue:{REGION}:{ACCOUNT_ID}:crawler/nightly"),
        ),
        (
            "a Glue table ARN",
            format!("arn:aws:glue:{REGION}:{ACCOUNT_ID}:table/sales/orders"),
        ),
        ("a value that is no ARN", "orders".to_string()),
    ] {
        let (status, answer) = call(
            &router,
            "UpdateIntegrationResourceProperty",
            &json!({
                "ResourceArn": &resource_arn,
                "TargetProcessingProperties": { "RoleArn": role_arn("target-role") },
            }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case}: expected 400, got {status} with {answer}"
        );
        assert!(
            error_code(&answer).contains("ValidationException"),
            "case {case}: expected ValidationException, got {answer}"
        );
        assert_eq!(
            error_message(&answer),
            service_com_amazonaws_glue_awsglue::service::integration_properties::invalid_resource_arn(
                &resource_arn
            ),
            "case {case}: expected the invalid-ARN refusal naming the value, got {answer}"
        );
    }
}

/// An ARN that names a leg of an integration this account does not hold is the modelled miss,
/// and the write stores nothing for it: the rows span a database and a connection that were
/// never created, and a leg of another account and another Region.
#[tokio::test]
async fn a_write_against_a_leg_this_account_does_not_hold_is_the_modelled_miss() {
    let router = emulator();
    given_database(&router, "held_database").await;

    for (case, resource_arn, side) in [
        (
            "a database never created",
            database_arn("absent_database"),
            json!({ "TargetProcessingProperties": { "RoleArn": role_arn("target-role") } }),
        ),
        (
            "a connection never created",
            connection_arn("absent-connection"),
            json!({ "SourceProcessingProperties": { "RoleArn": role_arn("source-role") } }),
        ),
        (
            "a database of another account",
            format!("arn:aws:glue:{REGION}:123456789012:database/held_database"),
            json!({ "TargetProcessingProperties": { "RoleArn": role_arn("target-role") } }),
        ),
        (
            "a connection of another account",
            format!("arn:aws:glue:{REGION}:123456789012:connection/held-connection"),
            json!({ "SourceProcessingProperties": { "RoleArn": role_arn("source-role") } }),
        ),
    ] {
        let mut body = json!({ "ResourceArn": &resource_arn });
        for (member, value) in side.as_object().expect("the row states a side") {
            body[member] = value.clone();
        }
        let (status, answer) = call(&router, "UpdateIntegrationResourceProperty", &body).await;
        assert_eq!(
            status, 400,
            "case {case}: expected 400, got {status} with {answer}"
        );
        assert!(
            error_code(&answer).contains("EntityNotFoundException"),
            "case {case}: expected EntityNotFoundException, got {answer}"
        );

        let (status, answer) = call(
            &router,
            "GetIntegrationResourceProperty",
            &json!({ "ResourceArn": &resource_arn }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case}: a refused write must store nothing, got {status} with {answer}"
        );
    }
}
