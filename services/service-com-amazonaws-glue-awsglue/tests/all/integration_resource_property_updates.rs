// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for `UpdateIntegrationResourceProperty`, the second write of the Glue
//! integration resource property slice, and the read and the delete that answer what it
//! wrote.
//!
//! Every call goes over the wire through the shared [`crate::wire`] harness, so what is
//! asserted is the HTTP behaviour and not a direct call into a handler. A record is addressed
//! the way the create suite addresses one ([`crate::integration_resource_properties`]).
//!
//! The rule under test: a write of an integration resource's processing properties COMMITS
//! them to the single record keyed by the `ResourceArn` it names — opening that record when
//! the resource has none yet — so the read and the delete of that same ARN answer from what
//! the write stored, under the one `ResourcePropertyArn` the resource derives. It holds for
//! each of the two writes of that record, whichever of them opened it: an update after a
//! create and a create after an update land in one record, and the side a write does not
//! state is left as stored. One call states ONE side, so a request stating BOTH sides and a
//! request stating NEITHER are refused on the request body alone and persist nothing. The
//! `ResourceArn` is then read as an ARN of one of the integration's two legs, and the two
//! classes of value that are not are answered differently: a value that is no ARN at all is
//! the modelled `InvalidInputException`, while an ARN that names neither leg is the modelled
//! `ValidationException` naming the value. Whether this account HOLDS the leg is a different
//! namespace from the record's and is not asked.
//!
//! The rows differ along what the rule generalises over: which side of the integration the
//! properties belong to, which resource kind the ARN names, whether this account holds that
//! resource, which account and Region the ARN carries, whether the record already existed and
//! which call opened it, how many members the side carries, their length, and the form of a
//! value that names no leg at all.
//!
//! The sibling suites cover the rest of the slice: [`crate::integration_resource_properties`]
//! (the create, the read and the delete), [`crate::integration_property_validation`] (the
//! order a body rule is read in) and [`crate::integration_property_miss_wording`] (which
//! sentence a miss carries).

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_glue_awsglue::ops::types::CreateIntegrationResourcePropertyInput;
use service_com_amazonaws_glue_awsglue::state::{
    ComAmazonawsGlueAwsglueState, IntegrationResourcePropertyEntry,
    MAX_INTEGRATION_RESOURCE_PROPERTIES,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::integration_resource_properties::{
    connection_arn, create_connection, create_database, database_arn, delete_miss_message,
    record_miss_message,
};
use crate::wire::{assert_echoed, call, emulator, emulator_on, error_code, ok, ACCOUNT_ID, REGION};

/// The longest `RoleArn` either side accepts: "len 1..128" on `String128`
/// (`smithy_query shape=SourceProcessingProperties,TargetProcessingProperties`).
const MAX_ROLE_ARN_LENGTH: usize = 128;

/// The sentence a write stating both processing sides is refused with.
const BOTH_SIDES_MESSAGE: &str = "Both source processing properties and target processing \
     properties cannot be provided in the same request.";

/// The sentence a write stating neither processing side is refused with.
const NEITHER_SIDE_MESSAGE: &str = "Either source processing properties or target processing \
     properties must be provided in the request.";

/// The sentence a refusal carries for an ARN that names neither leg of an integration, around
/// its one variable part — the value the request sent. Written here independently of the
/// handler, so the suite pins the wording and not the code.
fn invalid_resource_arn_message(resource_arn: &str) -> String {
    format!("Encountered invalid resource arn {resource_arn}")
}

/// Write one record through `UpdateIntegrationResourceProperty` and return the response body.
async fn update_property(router: &MultiServiceRouter, body: &Value) -> Value {
    ok(router, "UpdateIntegrationResourceProperty", body).await
}

/// The `GetIntegrationResourceProperty` response for `resource_arn`.
async fn get_property(router: &MultiServiceRouter, resource_arn: &str) -> Value {
    ok(
        router,
        "GetIntegrationResourceProperty",
        &json!({ "ResourceArn": resource_arn }),
    )
    .await
}

/// The `ResourcePropertyArn` of a response, which every call that reports a record carries.
fn property_arn(case: &str, body: &Value) -> String {
    body.get("ResourcePropertyArn")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!("case {case}: the response must carry a ResourcePropertyArn, got {body}")
        })
        .to_string()
}

/// The record ARN the resource `resource_arn` derives: the documented resource type ("The
/// format is something like arn:aws:glue:::integrationresourceproperty/*" —
/// `smithy_query shape=UpdateIntegrationResourcePropertyResponse member=ResourcePropertyArn`)
/// under this endpoint's Region and account, carrying the resource type and name the
/// resource's own ARN carries. Written here independently of the handler, so the suite pins
/// the form and not the code.
fn expected_record_arn(case: &str, resource_arn: &str) -> String {
    let described = resource_arn
        .rsplit_once(':')
        .unwrap_or_else(|| panic!("case {case}: {resource_arn:?} carries no resource portion"))
        .1;
    format!("arn:aws:glue:{REGION}:{ACCOUNT_ID}:integrationresourceproperty/{described}")
}

/// Assert that the read of `resource_arn` misses: the record is gone (or was never written),
/// which is what makes a delete and a refused write observable.
async fn assert_read_misses(router: &MultiServiceRouter, case: &str, resource_arn: &str) {
    let (status, body) = call(
        router,
        "GetIntegrationResourceProperty",
        &json!({ "ResourceArn": resource_arn }),
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case}: the read of a resource with no record must be refused, got {status} with \
         {body}"
    );
    assert!(
        error_code(&body).ends_with("EntityNotFoundException"),
        "case {case}: expected EntityNotFoundException from the read, got {} in {body}",
        error_code(&body)
    );
}

// ---------------------------------------------------------------------------
// The write, and the read and the delete that answer it
// ---------------------------------------------------------------------------

/// What `UpdateIntegrationResourceProperty` stores is what `GetIntegrationResourceProperty`
/// reports and what `DeleteIntegrationResourceProperty` removes: the write opens the record
/// its `ResourceArn` keys when the resource has none, reports it member for member under the
/// ARN that resource derives, and the read of the same ARN answers the same record.
#[tokio::test]
async fn updated_integration_resource_properties_are_read_back_from_the_store() {
    let router = emulator();

    let role_prefix = format!("arn:aws:iam::{ACCOUNT_ID}:role/");
    let long_role = role_prefix.clone() + &"r".repeat(MAX_ROLE_ARN_LENGTH - role_prefix.len());

    // (case, whether the ARN names a connection, whether this account holds the resource,
    // the properties the update states)
    let rows: Vec<(&str, bool, bool, Value)> = vec![
        (
            // The shape a client setting up a source sends: the connection's role alone.
            "a connection's source role",
            true,
            true,
            json!({
                "SourceProcessingProperties": {
                    "RoleArn": format!("{role_prefix}glue-source"),
                },
            }),
        ),
        (
            // The recorded case: a target's role, against the Glue database ARN of the
            // integration's target.
            "a database's target role",
            false,
            true,
            json!({
                "TargetProcessingProperties": {
                    "RoleArn": format!("{role_prefix}glue-target"),
                },
            }),
        ),
        (
            "a target carrying every modelled member",
            false,
            true,
            json!({
                "TargetProcessingProperties": {
                    "RoleArn": format!("{role_prefix}glue-target-full"),
                    "KmsArn": format!("arn:aws:kms:{REGION}:{ACCOUNT_ID}:key/11111111-2222-3333-4444-555555555555"),
                    "ConnectionName": "vpc-network-connection",
                    "EventBusArn": format!("arn:aws:events:{REGION}:{ACCOUNT_ID}:event-bus/integration-status"),
                },
            }),
        ),
        (
            "a target stating its network connection alone",
            false,
            true,
            json!({
                "TargetProcessingProperties": { "ConnectionName": "vpc-network-connection" },
            }),
        ),
        (
            "a role at the modelled length bound",
            true,
            true,
            json!({ "SourceProcessingProperties": { "RoleArn": long_role } }),
        ),
        (
            "a role of the shortest length the model admits",
            false,
            true,
            json!({ "TargetProcessingProperties": { "RoleArn": "r" } }),
        ),
        (
            // The recorded lifecycle writes against a Glue database ARN this account holds no
            // database for, and the read that follows reports the record: the write addresses
            // the record namespace and resolves the leg no further.
            "a database this account does not hold",
            false,
            false,
            json!({
                "TargetProcessingProperties": {
                    "RoleArn": format!("{role_prefix}glue-unheld-target"),
                },
            }),
        ),
        (
            "a connection this account does not hold",
            true,
            false,
            json!({
                "SourceProcessingProperties": {
                    "RoleArn": format!("{role_prefix}glue-unheld-source"),
                },
            }),
        ),
    ];

    let mut stored: Vec<(&str, String, Value)> = Vec::new();
    for (index, (case, is_connection, is_held, properties)) in rows.iter().enumerate() {
        let name = format!("updated-resource-{index}");
        let resource_arn = if *is_connection {
            if *is_held {
                create_connection(&router, &name).await;
            }
            connection_arn(&name)
        } else {
            if *is_held {
                create_database(&router, &name).await;
            }
            database_arn(&name)
        };

        // Nothing is stored for the resource yet, so this write is the one that opens the
        // record.
        assert_read_misses(&router, case, &resource_arn).await;

        let mut request = properties.clone();
        request["ResourceArn"] = json!(resource_arn);
        let updated = update_property(&router, &request).await;
        assert_eq!(
            updated.get("ResourceArn").and_then(Value::as_str),
            Some(resource_arn.as_str()),
            "case {case}: the update must report the ResourceArn it wrote against, got {updated}"
        );
        let record_arn = property_arn(case, &updated);
        assert_eq!(
            record_arn,
            expected_record_arn(case, &resource_arn),
            "case {case}: the record ARN must be derived from the resource it describes, got \
             {updated}"
        );
        assert_echoed(case, properties, &updated);

        let read = get_property(&router, &resource_arn).await;
        assert_eq!(
            read, updated,
            "case {case}: the read must report what the write stored, expected {updated}, got \
             {read}"
        );

        stored.push((case, resource_arn, properties.clone()));
    }

    // Each resource has its OWN record: after every write has landed, each one still reads
    // back its own properties under its own ARN, so no write reached another record.
    let mut record_arns: Vec<String> = Vec::new();
    for (case, resource_arn, properties) in &stored {
        let read = get_property(&router, resource_arn).await;
        assert_echoed(case, properties, &read);
        record_arns.push(property_arn(case, &read));
    }
    let distinct: std::collections::BTreeSet<&String> = record_arns.iter().collect();
    assert_eq!(
        distinct.len(),
        record_arns.len(),
        "two resources must not share one record ARN, got {record_arns:?}"
    );

    // The record an update wrote is the record the delete removes, and the read then misses:
    // one record, reached by three operations through the one ARN.
    for (case, resource_arn, _) in &stored {
        let removed = ok(
            &router,
            "DeleteIntegrationResourceProperty",
            &json!({ "ResourceArn": resource_arn }),
        )
        .await;
        assert_eq!(
            removed,
            json!({}),
            "case {case}: the delete answers an empty document, got {removed}"
        );
        assert_read_misses(&router, case, resource_arn).await;

        // A second delete is the same miss, worded as this delete words it.
        let (status, body) = call(
            &router,
            "DeleteIntegrationResourceProperty",
            &json!({ "ResourceArn": resource_arn }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case}: a second delete must be refused, got {status} with {body}"
        );
        assert_eq!(
            body.get("Message").and_then(Value::as_str),
            Some(delete_miss_message(resource_arn).as_str()),
            "case {case}: expected the miss this ARN's kind earns, got {body}"
        );
    }
}

/// The record is the resource's, not the catalog's: a write against a leg of an integration
/// this account holds no catalog record for is served and reads back, so the ARN's own
/// identity fields are all that address the record.
///
/// The rows span the forms of a leg this account does not hold: a database and a connection
/// never created, and legs of another account and another Region.
#[tokio::test]
async fn a_write_against_a_leg_this_account_does_not_hold_is_served() {
    let router = emulator();
    create_database(&router, "held_database").await;

    for (case, resource_arn, side) in [
        (
            // The recorded case: the lifecycle's own database ARN has no database behind it.
            "a database never created",
            database_arn("absent_database"),
            json!({ "TargetProcessingProperties": { "RoleArn": format!("arn:aws:iam::{ACCOUNT_ID}:role/target-role") } }),
        ),
        (
            "a connection never created",
            connection_arn("absent-connection"),
            json!({ "SourceProcessingProperties": { "RoleArn": format!("arn:aws:iam::{ACCOUNT_ID}:role/source-role") } }),
        ),
        (
            "a database of another account",
            format!("arn:aws:glue:{REGION}:123456789012:database/held_database"),
            json!({ "TargetProcessingProperties": { "RoleArn": format!("arn:aws:iam::{ACCOUNT_ID}:role/other-account") } }),
        ),
        (
            "a connection of another Region",
            format!("arn:aws:glue:eu-west-1:{ACCOUNT_ID}:connection/elsewhere"),
            json!({ "SourceProcessingProperties": { "RoleArn": format!("arn:aws:iam::{ACCOUNT_ID}:role/other-region") } }),
        ),
    ] {
        let mut request = side.clone();
        request["ResourceArn"] = json!(&resource_arn);
        let written = update_property(&router, &request).await;
        assert_echoed(case, &side, &written);

        // The record is the one that ARN keys: the read answers it, and the delete removes
        // it.
        let read = get_property(&router, &resource_arn).await;
        assert_eq!(
            read, written,
            "case {case}: the read must report what the write stored, expected {written}, got \
             {read}"
        );
        ok(
            &router,
            "DeleteIntegrationResourceProperty",
            &json!({ "ResourceArn": &resource_arn }),
        )
        .await;
        assert_read_misses(&router, case, &resource_arn).await;
    }

    // The resource this account DOES hold is untouched by any of it: its own reader still
    // reports it.
    let read = ok(
        &router,
        "GetDatabase",
        &json!({ "Name": "held_database" }),
    )
    .await;
    assert_eq!(
        read["Database"]["Name"].as_str(),
        Some("held_database"),
        "case the held database: the writes above must leave it as it was, got {read}"
    );
}

/// One record per resource, written by either call: an update after a create and a create
/// after an update land in the SAME record, under the one `ResourcePropertyArn` the resource
/// derives, and the side a write does not state is left exactly as stored.
///
/// The rows differ along which call opened the record, which side each write states, and
/// whether the second write replaces the stored side or adds the other one.
#[tokio::test]
async fn an_update_writes_the_record_the_create_opened_and_leaves_the_unstated_side() {
    let router = emulator();

    let source = |marker: &str| {
        json!({ "SourceProcessingProperties": { "RoleArn": format!("arn:aws:iam::{ACCOUNT_ID}:role/{marker}") } })
    };
    let target = |marker: &str| {
        json!({ "TargetProcessingProperties": { "RoleArn": format!("arn:aws:iam::{ACCOUNT_ID}:role/{marker}") } })
    };

    // (case, the operation that opens the record, the properties it states, the operation
    // that writes it next, the properties that write states, whether the first side must
    // survive the second write)
    let rows: Vec<(&str, &str, Value, &str, Value, bool)> = vec![
        (
            "an update replacing the side a create stored",
            "CreateIntegrationResourceProperty",
            source("created-source"),
            "UpdateIntegrationResourceProperty",
            source("updated-source"),
            false,
        ),
        (
            "an update stating the side a create left unstated",
            "CreateIntegrationResourceProperty",
            source("kept-source"),
            "UpdateIntegrationResourceProperty",
            target("added-target"),
            true,
        ),
        (
            "a create stating the side an update left unstated",
            "UpdateIntegrationResourceProperty",
            target("kept-target"),
            "CreateIntegrationResourceProperty",
            source("added-source"),
            true,
        ),
        (
            "an update replacing the side an earlier update stored",
            "UpdateIntegrationResourceProperty",
            target("first-target"),
            "UpdateIntegrationResourceProperty",
            target("second-target"),
            false,
        ),
    ];

    for (index, (case, opener, first, writer, second, first_side_survives)) in
        rows.iter().enumerate()
    {
        let name = format!("two-writes-{index}");
        create_connection(&router, &name).await;
        let resource_arn = connection_arn(&name);

        let mut request = first.clone();
        request["ResourceArn"] = json!(&resource_arn);
        let opened = ok(&router, opener, &request).await;
        let record_arn = property_arn(case, &opened);

        let mut request = second.clone();
        request["ResourceArn"] = json!(&resource_arn);
        let written = ok(&router, writer, &request).await;
        assert_eq!(
            property_arn(case, &written),
            record_arn,
            "case {case}: the second write must report the record the first opened, got {written}"
        );

        // The read answers one record: the side the second write stated — which replaced the
        // stored value when both writes state the same side — and the side the first wrote
        // when the second said nothing about it.
        let read = get_property(&router, &resource_arn).await;
        assert_echoed(case, second, &read);
        if *first_side_survives {
            assert_echoed(case, first, &read);
        }
        assert_eq!(
            property_arn(case, &read),
            record_arn,
            "case {case}: the record keeps the ARN its resource derives, got {read}"
        );

        // One record and not two: the delete of that ARN leaves nothing behind.
        ok(
            &router,
            "DeleteIntegrationResourceProperty",
            &json!({ "ResourceArn": &resource_arn }),
        )
        .await;
        assert_read_misses(&router, case, &resource_arn).await;
    }
}

// ---------------------------------------------------------------------------
// The validation boundaries
// ---------------------------------------------------------------------------

/// One call states ONE side of the integration, so an update stating BOTH sides and an update
/// stating NEITHER are each refused with the modelled `ValidationException` and persist
/// nothing — whichever resource kind they address, whether or not that resource already has a
/// record, and even when the `ResourceArn` is a value that would itself be refused, because
/// the body is read first.
///
/// The rows differ along which sides the request states, how many members those sides carry,
/// and what the `ResourceArn` carries.
#[tokio::test]
async fn an_update_stating_neither_or_both_processing_sides_is_refused() {
    let router = emulator();

    // (case, whether the ARN names a connection, the members beside `ResourceArn`, the
    // sentence the refusal must carry)
    let rows: Vec<(&str, bool, Value, &str)> = vec![
        (
            // The recorded case: a request with neither processing property.
            "neither side, on a database",
            false,
            json!({}),
            NEITHER_SIDE_MESSAGE,
        ),
        (
            "neither side, on a connection",
            true,
            json!({}),
            NEITHER_SIDE_MESSAGE,
        ),
        (
            "both sides at once",
            true,
            json!({
                "SourceProcessingProperties": {
                    "RoleArn": format!("arn:aws:iam::{ACCOUNT_ID}:role/both-source"),
                },
                "TargetProcessingProperties": {
                    "RoleArn": format!("arn:aws:iam::{ACCOUNT_ID}:role/both-target"),
                    "ConnectionName": "both-sides-connection",
                },
            }),
            BOTH_SIDES_MESSAGE,
        ),
        (
            "both sides at once, each carrying nothing",
            false,
            json!({ "SourceProcessingProperties": {}, "TargetProcessingProperties": {} }),
            BOTH_SIDES_MESSAGE,
        ),
    ];

    for (index, (case, is_connection, members, expected)) in rows.iter().enumerate() {
        let name = format!("side-required-{index}");
        let resource_arn = if *is_connection {
            create_connection(&router, &name).await;
            connection_arn(&name)
        } else {
            create_database(&router, &name).await;
            database_arn(&name)
        };

        let mut request = members.clone();
        request["ResourceArn"] = json!(&resource_arn);
        let (status, body) = call(&router, "UpdateIntegrationResourceProperty", &request).await;
        assert_eq!(
            status, 400,
            "case {case}: an update stating neither side or both sides is refused, got {status} \
             with {body}"
        );
        assert!(
            error_code(&body).ends_with("ValidationException"),
            "case {case}: expected ValidationException, got {} in {body}",
            error_code(&body)
        );
        assert_eq!(
            body.get("Message").and_then(Value::as_str),
            Some(*expected),
            "case {case}: expected the fixed refusal sentence, got {body}"
        );

        // The refusal persisted nothing, so the resource still has no record.
        assert_read_misses(&router, case, &resource_arn).await;

        // The requirement is on the REQUEST and not on what the store holds: the same
        // refusal answers a resource that already HAS a record, and that record is left
        // exactly as it was.
        let stored_role = format!("arn:aws:iam::{ACCOUNT_ID}:role/already-stored");
        let opened = update_property(
            &router,
            &json!({
                "ResourceArn": &resource_arn,
                "SourceProcessingProperties": { "RoleArn": &stored_role },
            }),
        )
        .await;
        let record_arn = property_arn(case, &opened);

        let (status, body) = call(&router, "UpdateIntegrationResourceProperty", &request).await;
        assert_eq!(
            status, 400,
            "case {case}: the refusal must not depend on the record existing, got {status} with \
             {body}"
        );
        assert_eq!(
            body.get("Message").and_then(Value::as_str),
            Some(*expected),
            "case {case}: expected the fixed refusal sentence, got {body}"
        );
        let read = get_property(&router, &resource_arn).await;
        assert_echoed(
            case,
            &json!({ "SourceProcessingProperties": { "RoleArn": &stored_role } }),
            &read,
        );
        assert_eq!(
            property_arn(case, &read),
            record_arn,
            "case {case}: the refused update must leave the record as it was, got {read}"
        );
    }

    // The body is read BEFORE the `ResourceArn`, so a request that breaks a body rule is told
    // about the body even when the ARN it carries would be refused on its own.
    for (case, resource_arn) in [
        (
            "neither side, against an ARN that names no leg",
            format!("arn:aws:rds:{REGION}:{ACCOUNT_ID}:cluster:orders"),
        ),
        (
            "neither side, against a value that is no ARN",
            "not-an-arn".to_string(),
        ),
    ] {
        let (status, body) = call(
            &router,
            "UpdateIntegrationResourceProperty",
            &json!({ "ResourceArn": &resource_arn }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case}: expected 400, got {status} with {body}"
        );
        assert!(
            error_code(&body).ends_with("ValidationException"),
            "case {case}: expected ValidationException, got {} in {body}",
            error_code(&body)
        );
        assert_eq!(
            body.get("Message").and_then(Value::as_str),
            Some(NEITHER_SIDE_MESSAGE),
            "case {case}: expected the body rule's sentence, got {body}"
        );
    }
}

/// A `ResourceArn` that names neither leg of an integration is refused by this write too,
/// classified by WHAT the value is: a value that is no ARN at all is the modelled
/// `InvalidInputException`, which states no sentence, while an ARN that names neither leg is
/// the modelled `ValidationException` naming the value the request sent. Neither stores
/// anything.
///
/// The rows span the forms such a value takes: another service's ARN (the recorded RDS
/// cluster), a Redshift namespace, three Glue ARNs of resource types this API does not
/// accept, a Glue ARN carrying no resource name, and two values that are no ARN at all.
#[tokio::test]
async fn an_update_whose_resource_arn_names_no_integration_leg_is_refused() {
    let router = emulator();

    // (case, the value sent in `ResourceArn`, whether the value is spelled as an ARN at all)
    for (case, resource_arn, an_arn) in [
        (
            // The recorded case: the zero-ETL target can live in another service.
            "an RDS cluster ARN",
            format!("arn:aws:rds:{REGION}:{ACCOUNT_ID}:cluster:orders"),
            true,
        ),
        (
            "a Redshift Serverless namespace ARN",
            format!("arn:aws:redshift-serverless:{REGION}:{ACCOUNT_ID}:namespace/warehouse"),
            true,
        ),
        ("an S3 object ARN", "arn:aws:s3:::bucket/key".to_string(), true),
        (
            "a Glue crawler ARN",
            format!("arn:aws:glue:{REGION}:{ACCOUNT_ID}:crawler/nightly"),
            true,
        ),
        (
            "a Glue job ARN",
            format!("arn:aws:glue:{REGION}:{ACCOUNT_ID}:job/etl"),
            true,
        ),
        (
            "a Glue table ARN",
            format!("arn:aws:glue:{REGION}:{ACCOUNT_ID}:table/sales/orders"),
            true,
        ),
        (
            "a connection ARN carrying no name",
            format!("arn:aws:glue:{REGION}:{ACCOUNT_ID}:connection/"),
            true,
        ),
        // The recorded case of the other class: a value with no ARN form at all.
        ("a value that is no ARN", "orders".to_string(), false),
        (
            "an ARN missing its fields",
            "arn:aws:glue:connection".to_string(),
            false,
        ),
    ] {
        let (status, body) = call(
            &router,
            "UpdateIntegrationResourceProperty",
            &json!({
                "ResourceArn": &resource_arn,
                "SourceProcessingProperties": {
                    "RoleArn": format!("arn:aws:iam::{ACCOUNT_ID}:role/r"),
                },
            }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case}: expected 400 for {resource_arn:?}, got {status} with {body}"
        );
        if an_arn {
            assert!(
                error_code(&body).ends_with("ValidationException"),
                "case {case}: expected ValidationException for {resource_arn:?}, got {} in {body}",
                error_code(&body)
            );
            assert_eq!(
                body.get("Message").and_then(Value::as_str),
                Some(invalid_resource_arn_message(&resource_arn).as_str()),
                "case {case}: expected the refusal naming the value, got {body}"
            );
        } else {
            assert!(
                error_code(&body).ends_with("InvalidInputException"),
                "case {case}: expected InvalidInputException for {resource_arn:?}, got {} in \
                 {body}",
                error_code(&body)
            );
            // RECORDED: this refusal is the code alone — it states no sentence.
            assert!(
                body.get("Message").is_none(),
                "case {case}: the refusal carries no Message, got {body}"
            );
        }

        // The refusal stored nothing, so the read of that value misses as well.
        let (read_status, read_body) = call(
            &router,
            "GetIntegrationResourceProperty",
            &json!({ "ResourceArn": &resource_arn }),
        )
        .await;
        assert_eq!(
            read_status, 400,
            "case {case}: the refused update must have stored nothing, got {read_status} with \
             {read_body}"
        );
    }

    // The legs of an integration still take a record, so the refusals above are refusals of
    // the value and not a reading that refuses everything.
    create_connection(&router, "readable-source").await;
    let resource_arn = connection_arn("readable-source");
    update_property(
        &router,
        &json!({
            "ResourceArn": &resource_arn,
            "SourceProcessingProperties": { "RoleArn": "arn:aws:iam::000000000000:role/after" },
        }),
    )
    .await;
    let read = get_property(&router, &resource_arn).await;
    assert_echoed(
        "the connection of the source",
        &json!({
            "SourceProcessingProperties": { "RoleArn": "arn:aws:iam::000000000000:role/after" },
        }),
        &read,
    );
}

/// The store holds a bounded number of records whichever write opens one: the update that
/// takes the last slot succeeds, the one that would open a record past the ceiling is refused
/// with the closest client error this operation models, an update of a record the store
/// ALREADY holds still succeeds at the ceiling (the bound is on records, not on writes), and
/// freeing a slot lets a new resource take one again.
#[tokio::test]
async fn an_update_opening_a_record_past_the_ceiling_is_refused() {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let router = emulator_on(engine.clone());

    // Reaching the ceiling over the wire would take a thousand requests and would exercise
    // the transport rather than the bound.
    {
        let state = engine.state::<ComAmazonawsGlueAwsglueState>();
        let mut store = state.lock();
        for index in 0..MAX_INTEGRATION_RESOURCE_PROPERTIES - 1 {
            let resource_arn = connection_arn(&format!("seeded-{index}"));
            let input = CreateIntegrationResourcePropertyInput {
                resource_arn: resource_arn.clone(),
                ..CreateIntegrationResourcePropertyInput::default()
            };
            store.integration_resource_properties.insert(
                resource_arn,
                IntegrationResourcePropertyEntry::from_create_input(&input),
            );
        }
    }

    create_connection(&router, "last-resource").await;
    create_connection(&router, "one-too-many").await;
    let last = json!({
        "ResourceArn": connection_arn("last-resource"),
        "SourceProcessingProperties": { "RoleArn": "arn:aws:iam::000000000000:role/last" },
    });
    let past = json!({
        "ResourceArn": connection_arn("one-too-many"),
        "SourceProcessingProperties": { "RoleArn": "arn:aws:iam::000000000000:role/past" },
    });

    update_property(&router, &last).await;
    let (status, body) = call(&router, "UpdateIntegrationResourceProperty", &past).await;
    assert_eq!(
        status, 400,
        "case record {} of {MAX_INTEGRATION_RESOURCE_PROPERTIES}: expected 400, got {status} \
         with {body}",
        MAX_INTEGRATION_RESOURCE_PROPERTIES + 1
    );
    assert!(
        error_code(&body).ends_with("InvalidInputException"),
        "case one record too many: expected InvalidInputException, got {} in {body}",
        error_code(&body)
    );
    assert_read_misses(
        &router,
        "one record too many",
        &connection_arn("one-too-many"),
    )
    .await;

    // A write to a record the store already holds opens none, so it is served at the
    // ceiling.
    let again = update_property(
        &router,
        &json!({
            "ResourceArn": connection_arn("last-resource"),
            "SourceProcessingProperties": { "RoleArn": "arn:aws:iam::000000000000:role/again" },
        }),
    )
    .await;
    assert_echoed(
        "a write at the ceiling",
        &json!({
            "SourceProcessingProperties": { "RoleArn": "arn:aws:iam::000000000000:role/again" },
        }),
        &again,
    );

    // Freeing a slot lets a new resource take one: the record store is a bound and not a
    // one-way door. The delete frees the slot the record the suite opened holds.
    ok(
        &router,
        "DeleteIntegrationResourceProperty",
        &json!({ "ResourceArn": connection_arn("last-resource") }),
    )
    .await;
    update_property(&router, &past).await;
    let read = get_property(&router, &connection_arn("one-too-many")).await;
    assert_echoed(
        "the resource that took the freed slot",
        &json!({
            "SourceProcessingProperties": { "RoleArn": "arn:aws:iam::000000000000:role/past" },
        }),
        &read,
    );
    // The record the delete removed is gone, and its miss is the record miss the source leg
    // of an integration earns.
    let (status, body) = call(
        &router,
        "GetIntegrationResourceProperty",
        &json!({ "ResourceArn": connection_arn("last-resource") }),
    )
    .await;
    assert_eq!(
        status, 400,
        "case the freed resource: expected 400, got {status} with {body}"
    );
    assert_eq!(
        body.get("Message").and_then(Value::as_str),
        Some(record_miss_message(&connection_arn("last-resource")).as_str()),
        "case the freed resource: expected the record miss of the source leg, got {body}"
    );
}
