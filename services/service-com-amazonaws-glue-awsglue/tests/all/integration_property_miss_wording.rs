// SPDX-License-Identifier: CC0-1.0

//! Which SENTENCE a not-found refusal of the zero-ETL integration PROPERTY family carries.
//!
//! Every call goes over the wire through the shared [`crate::wire`] harness, so what is
//! asserted is HTTP behaviour and not a direct call into a handler.
//!
//! The rule under test: the refusal's error class is the same for every one of these misses
//! (the modelled `EntityNotFoundException`, 400) and its WORDING is chosen by the resource
//! KIND the request's `ResourceArn` resolves to — a Glue DATABASE ARN addresses the
//! integration's TARGET ("the connection ARN of the source, or the database ARN of the
//! target", `smithy_query shape=CreateIntegrationResourcePropertyRequest member=ResourceArn`)
//! and is answered with the target resource not being found, while every other ARN kind is
//! answered with the miss of the RECORD the request addressed. So the two legs of ONE
//! lifecycle — the read after the delete and the repeat delete — carry the SAME sentence, and
//! the two legs of an integration carry different ones.
//!
//! The rows differ along what the rule generalises over: which surface is asked (the
//! processing properties of a resource, and the override properties of a replicated table),
//! which operation asks (the read, the delete, the repeat delete), and which resource kind the
//! ARN names (the connection of the source, the database of the target, a catalog table,
//! another Glue kind, and a resource this account does not hold).

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::integration_resource_properties::{
    addresses_the_integration_target, connection_arn, create_connection, create_database,
    database_arn, record_miss_message, target_miss_message,
};
use crate::integration_table_properties::create_table;
use crate::wire::{call, emulator, error_code, error_message, ok, ACCOUNT_ID, REGION};

/// The table whose properties the table-property rows are about.
const TABLE: &str = "sap_orders";

/// The sentence the miss of a stored integration TABLE PROPERTY record carries, around the two
/// members the request addressed it by. Written here independently of the handler, so this
/// suite pins the wording and not the code.
fn table_record_miss_message(resource_arn: &str, table: &str) -> String {
    format!(
        "Failed to find the sourceTableProperties with sourceArn {resource_arn} and tableName \
         {table}"
    )
}

/// The sentence a miss of one surface earns for `resource_arn`, by the rule above.
fn expected_message(resource_arn: &str, table_properties: bool) -> String {
    if addresses_the_integration_target(resource_arn) {
        return target_miss_message(resource_arn);
    }
    if table_properties {
        table_record_miss_message(resource_arn, TABLE)
    } else {
        record_miss_message(resource_arn)
    }
}

/// Assert that `operation` refuses `request` with the modelled `EntityNotFoundException` and
/// the sentence the rule gives `resource_arn`.
async fn assert_miss(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    request: &Value,
    resource_arn: &str,
    table_properties: bool,
) {
    let (status, body) = call(router, operation, request).await;
    assert_eq!(
        status, 400,
        "case {case} ({operation}): a miss must be refused with 400, got {status} with {body}"
    );
    assert!(
        error_code(&body).ends_with("EntityNotFoundException"),
        "case {case} ({operation}): expected EntityNotFoundException, got {} in {body}",
        error_code(&body)
    );
    let expected = expected_message(resource_arn, table_properties);
    assert_eq!(
        error_message(&body),
        expected,
        "case {case} ({operation}): expected {expected:?}, got {:?}",
        error_message(&body)
    );
}

/// The two legs of ONE lifecycle agree, and the sentence is the one the ARN's kind earns: for
/// each row, create a record, delete it, and then both the read of it and the repeat delete
/// carry the same sentence.
///
/// The rows differ along which resource kind the ARN names, which is what the rule turns on.
#[tokio::test]
async fn both_legs_of_a_property_lifecycle_carry_the_sentence_the_arn_kind_earns() {
    let router = emulator();
    create_connection(&router, "lifecycle-source").await;
    create_database(&router, "lifecycle_target").await;
    create_table(&router, "lifecycle_target", TABLE).await;

    // (case, the ARN of the leg the record belongs to)
    let rows: Vec<(&str, String)> = vec![
        (
            // The recorded case of the source leg.
            "the connection ARN of the source",
            connection_arn("lifecycle-source"),
        ),
        (
            // The recorded case of the target leg.
            "the database ARN of the target",
            database_arn("lifecycle_target"),
        ),
    ];

    for (case, resource_arn) in &rows {
        // --- the processing properties of the resource ---------------------------------
        ok(
            &router,
            "CreateIntegrationResourceProperty",
            &json!({
                "ResourceArn": resource_arn,
                "SourceProcessingProperties": {
                    "RoleArn": format!("arn:aws:iam::{ACCOUNT_ID}:role/glue-zero-etl"),
                },
            }),
        )
        .await;
        ok(
            &router,
            "DeleteIntegrationResourceProperty",
            &json!({ "ResourceArn": resource_arn }),
        )
        .await;
        let request = json!({ "ResourceArn": resource_arn });
        for operation in [
            "GetIntegrationResourceProperty",
            "DeleteIntegrationResourceProperty",
        ] {
            assert_miss(&router, case, operation, &request, resource_arn, false).await;
        }

        // --- the override properties of a replicated table ----------------------------
        ok(
            &router,
            "CreateIntegrationTableProperties",
            &json!({
                "ResourceArn": resource_arn,
                "TableName": TABLE,
                "TargetTableConfig": { "TargetTableName": "orders_replica" },
            }),
        )
        .await;
        ok(
            &router,
            "DeleteIntegrationTableProperties",
            &json!({ "ResourceArn": resource_arn, "TableName": TABLE }),
        )
        .await;
        let request = json!({ "ResourceArn": resource_arn, "TableName": TABLE });
        for operation in [
            "GetIntegrationTableProperties",
            "DeleteIntegrationTableProperties",
        ] {
            assert_miss(&router, case, operation, &request, resource_arn, true).await;
        }
    }
}

/// The rule holds for every ARN KIND these operations resolve, whether or not this account
/// holds the resource and whether or not it could ever carry such a record — so a client
/// matching the refusal of one target kind is never shown another kind's wording.
///
/// The rows are the kinds and forms the member can carry: the two legs of an integration, a
/// catalog table's own ARN, other Glue resource kinds, and resources this account does not
/// hold.
#[tokio::test]
async fn the_sentence_is_chosen_by_the_resolved_arn_kind_for_every_kind() {
    let router = emulator();
    create_connection(&router, "held-source").await;
    create_database(&router, "held_target").await;
    create_table(&router, "held_target", TABLE).await;

    // (case, the value sent in `ResourceArn`, whether the table-property surface can
    // interpret it at all — that surface resolves the two legs and a catalog table's own ARN,
    // and refuses every other value as a validation failure, which is its own rule and not
    // this one)
    let rows: Vec<(&str, String, bool)> = vec![
        ("a held connection", connection_arn("held-source"), true),
        ("a held database", database_arn("held_target"), true),
        (
            "a connection this account does not hold",
            connection_arn("absent-source"),
            true,
        ),
        (
            "a database this account does not hold",
            database_arn("absent_target"),
            true,
        ),
        (
            "a held catalog table's own ARN",
            format!("arn:aws:glue:{REGION}:{ACCOUNT_ID}:table/held_target/{TABLE}"),
            true,
        ),
        (
            "a Glue crawler ARN",
            format!("arn:aws:glue:{REGION}:{ACCOUNT_ID}:crawler/nightly"),
            false,
        ),
        (
            "another account's database",
            format!("arn:aws:glue:{REGION}:111122223333:database/elsewhere"),
            true,
        ),
        (
            "another Region's connection",
            format!("arn:aws:glue:eu-west-1:{ACCOUNT_ID}:connection/elsewhere"),
            true,
        ),
    ];

    for (case, resource_arn, a_table_property_target) in &rows {
        assert_miss(
            &router,
            case,
            "DeleteIntegrationResourceProperty",
            &json!({ "ResourceArn": resource_arn }),
            resource_arn,
            false,
        )
        .await;
        if !a_table_property_target {
            continue;
        }
        let request = json!({ "ResourceArn": resource_arn, "TableName": TABLE });
        for operation in [
            "GetIntegrationTableProperties",
            "DeleteIntegrationTableProperties",
        ] {
            assert_miss(&router, case, operation, &request, resource_arn, true).await;
        }
    }
}
