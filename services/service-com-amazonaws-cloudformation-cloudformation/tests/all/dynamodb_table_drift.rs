// SPDX-License-Identifier: CC0-1.0

//! What a drift check reports about a stack-managed `AWS::DynamoDB::Table` after its
//! configuration was changed — or the table removed — outside the stack, end to end over each
//! service's own wire.
//!
//! **The rule under test.** A drift check classifies each managed resource on what the service
//! that OWNS it answers for it: a resource that service no longer holds is `DELETED` and
//! carries neither properties document nor differences, while a resource it still reports is
//! compared property by property against the declaration — `MODIFIED` with one
//! `PropertyDifference` per declared leaf the two sides disagree about (and both properties
//! documents) where any differ, and `IN_SYNC` where none do. So "the actual properties differ"
//! and "the resource is gone" are different answers, and putting the changed property back the
//! way the template declares it returns the resource to `IN_SYNC`.
//!
//! The rows differ along what the rule generalises over rather than repeating one recorded
//! case: WHICH property is changed outside the stack (one DynamoDB applies through a call of
//! its own and reports through a read of its own, in two of those settings, and one that is a
//! member of the table itself), in WHICH direction (a declared `false` turned on), a table
//! nothing touched whose declared setting is ON, a table nothing touched in the other
//! read/write capacity mode with the throughput that mode declares, and a table deleted
//! outright — which is the one row `DELETED` belongs to.
//!
//! Everything goes through the one router `harness.rs` builds: CloudFormation over awsQuery,
//! Amazon DynamoDB as a client's SDK calls it (the caller `dynamodb_table_resources.rs` owns).

use std::time::Duration;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::dynamodb_table_resources::{describe_table, dynamodb, failure_reasons};
use crate::ec2_network_stack_drift::{drift_report, record_of, settled_detection};
use crate::stack_lifecycle::{call, create_stack, list_members, member, router, xml_text};
use crate::stack_owned_secret_teardown::poll_to_terminal;
use service_com_amazonaws_cloudformation_cloudformation::state::{
    drift::difference_type, drift_detection_status, resource_drift_status, DRIFT_STATUS_DRIFTED,
};

/// How often a wait looks again at a condition DynamoDB's own clock owes, and how many times.
///
/// The budget covers the spans that service holds a change of a table open for several times
/// over, so a loaded machine is slower than those spans without the rule being any different;
/// what is waited for is the CONDITION, never a fixed stretch of clock.
const POLL_INTERVAL: Duration = Duration::from_millis(25);
const POLLS: usize = 1_200;

/// One difference a record must carry: the path, the expected value, the actual value and the
/// kind of difference.
type Difference = (&'static str, &'static str, &'static str, &'static str);

/// What a row does to its table outside the stack, before the check runs.
#[derive(Debug, Clone, Copy)]
enum Tamper {
    /// Nothing: the table is as the deployment left it.
    Nothing,
    /// Turn continuous backups on, through `UpdateContinuousBackups`.
    EnableContinuousBackups,
    /// Turn Time to Live on over the attribute the declaration names, through
    /// `UpdateTimeToLive`.
    EnableTimeToLive,
    /// Turn deletion protection on, through `UpdateTable` — a member of the table itself
    /// rather than a setting of its own.
    ProtectFromDeletion,
    /// Delete the table, through `DeleteTable`.
    DeleteTheTable,
}

/// One table of the stack the rows deploy, and what the check over it must report.
struct Row {
    case: &'static str,
    logical_id: &'static str,
    /// The table's name, which is this resource's physical id ("Ref returns the table name").
    table: &'static str,
    /// The properties the declaration carries beside the key schema every row shares.
    declared: Value,
    tamper: Tamper,
    status: &'static str,
    differences: Vec<Difference>,
}

/// The key schema every row's table declares, with the attribute definition behind it and the
/// capacity mode — the members DynamoDB needs in order to create a table at all.
fn base_properties(table: &str) -> Value {
    json!({
        "TableName": table,
        "BillingMode": "PAY_PER_REQUEST",
        "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
        "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
    })
}

/// One row's resource declaration, as the template carries it.
fn declaration(row: &Row) -> Value {
    let mut properties = base_properties(row.table);
    let members = properties
        .as_object_mut()
        .expect("the declaration is an object");
    for (property, value) in row
        .declared
        .as_object()
        .expect("the row's declared properties are an object")
    {
        members.insert(property.clone(), value.clone());
    }
    json!({"Type": "AWS::DynamoDB::Table", "Properties": properties})
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "continuous backups turned on outside the stack, declared off",
            logical_id: "BackedUp",
            table: "drift-pitr-table",
            declared: json!({
                "PointInTimeRecoverySpecification": {"PointInTimeRecoveryEnabled": false},
            }),
            tamper: Tamper::EnableContinuousBackups,
            status: resource_drift_status::MODIFIED,
            differences: vec![(
                "/PointInTimeRecoverySpecification/PointInTimeRecoveryEnabled",
                "false",
                "true",
                difference_type::NOT_EQUAL,
            )],
        },
        Row {
            case: "another setting of its own: Time to Live turned on, declared off",
            logical_id: "Expiring",
            table: "drift-ttl-table",
            declared: json!({
                "TimeToLiveSpecification": {"AttributeName": "expiresAt", "Enabled": false},
            }),
            tamper: Tamper::EnableTimeToLive,
            status: resource_drift_status::MODIFIED,
            differences: vec![(
                "/TimeToLiveSpecification/Enabled",
                "false",
                "true",
                difference_type::NOT_EQUAL,
            )],
        },
        Row {
            case: "a member of the table itself: deletion protection turned on, declared off",
            logical_id: "Protected",
            table: "drift-protection-table",
            declared: json!({"DeletionProtectionEnabled": false}),
            tamper: Tamper::ProtectFromDeletion,
            status: resource_drift_status::MODIFIED,
            differences: vec![(
                "/DeletionProtectionEnabled",
                "false",
                "true",
                difference_type::NOT_EQUAL,
            )],
        },
        Row {
            case: "a table nothing touched, whose declared setting is on",
            logical_id: "Untouched",
            table: "drift-untouched-table",
            declared: json!({
                "PointInTimeRecoverySpecification": {"PointInTimeRecoveryEnabled": true},
            }),
            tamper: Tamper::Nothing,
            status: resource_drift_status::IN_SYNC,
            differences: vec![],
        },
        Row {
            // The other read/write capacity mode, with the throughput that mode declares:
            // both are members `DescribeTable` reports, so a table nothing touched is in
            // sync on them rather than differing because the read spells them its own way.
            case: "a provisioned table nothing touched, in the other capacity mode",
            logical_id: "Provisioned",
            table: "drift-provisioned-table",
            declared: json!({
                "BillingMode": "PROVISIONED",
                "ProvisionedThroughput": {"ReadCapacityUnits": 3, "WriteCapacityUnits": 2},
                "DeletionProtectionEnabled": false,
            }),
            tamper: Tamper::Nothing,
            status: resource_drift_status::IN_SYNC,
            differences: vec![],
        },
        Row {
            case: "a table deleted outside the stack",
            logical_id: "Removed",
            table: "drift-removed-table",
            declared: json!({"DeletionProtectionEnabled": false}),
            tamper: Tamper::DeleteTheTable,
            status: resource_drift_status::DELETED,
            differences: vec![],
        },
    ]
}

/// Call one DynamoDB operation on a row's table and assert that service accepted it.
async fn accepted(router: &MultiServiceRouter, case: &str, operation: &str, request: &Value) {
    let (status, answered) = dynamodb(router, operation, request).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected {operation} to be accepted, got {status} with {answered}"
    );
}

/// Carry out one row's out-of-band change, through the owning service's own wire.
async fn tamper_with(router: &MultiServiceRouter, row: &Row) {
    let case = row.case;
    let table = row.table;
    match row.tamper {
        Tamper::Nothing => {}
        Tamper::EnableContinuousBackups => {
            set_continuous_backups(router, case, table, true).await;
        }
        Tamper::EnableTimeToLive => {
            accepted(
                router,
                case,
                "UpdateTimeToLive",
                &json!({
                    "TableName": table,
                    "TimeToLiveSpecification": {"AttributeName": "expiresAt", "Enabled": true},
                }),
            )
            .await;
        }
        Tamper::ProtectFromDeletion => {
            accepted(
                router,
                case,
                "UpdateTable",
                &json!({"TableName": table, "DeletionProtectionEnabled": true}),
            )
            .await;
            // The member is in force the moment that service answers; the read is what says
            // so rather than an assumption about it.
            let reported = wait_until(POLLS, || async {
                describe_table(router, table)
                    .await
                    .and_then(|described| {
                        described.get("DeletionProtectionEnabled").cloned()
                    })
                    .filter(|protection| *protection == Value::Bool(true))
            })
            .await;
            assert_eq!(
                reported,
                Some(Value::Bool(true)),
                "case {case:?}: expected DynamoDB to report deletion protection on {table:?}, \
                 got {reported:?}"
            );
        }
        Tamper::DeleteTheTable => {
            accepted(router, case, "DeleteTable", &json!({"TableName": table})).await;
            // "DynamoDB immediately returns a response with a TableStatus of DELETING", and
            // the table is gone once that service's own span falls due: the check below is
            // about a table that is really no longer there, so the wait is on that service
            // reporting none.
            let held = wait_until(POLLS, || async {
                describe_table(router, table).await.map_or(Some(()), |_| None)
            })
            .await;
            assert!(
                held.is_some(),
                "case {case:?}: expected DynamoDB to hold no table {table:?} once the delete \
                 fell due, got one"
            );
        }
    }
}

/// Turn continuous backups on or off on `table`, the way an operator does.
async fn set_continuous_backups(
    router: &MultiServiceRouter,
    case: &str,
    table: &str,
    enabled: bool,
) {
    accepted(
        router,
        case,
        "UpdateContinuousBackups",
        &json!({
            "TableName": table,
            "PointInTimeRecoverySpecification": {"PointInTimeRecoveryEnabled": enabled},
        }),
    )
    .await;
    let wanted = if enabled { "ENABLED" } else { "DISABLED" };
    let reported = wait_until(POLLS, || async {
        let (_, answered) = dynamodb(
            router,
            "DescribeContinuousBackups",
            &json!({"TableName": table}),
        )
        .await;
        answered
            .pointer(
                "/ContinuousBackupsDescription/PointInTimeRecoveryDescription/\
                 PointInTimeRecoveryStatus",
            )
            .and_then(Value::as_str)
            .filter(|status| *status == wanted)
            .map(str::to_string)
    })
    .await;
    assert_eq!(
        reported.as_deref(),
        Some(wanted),
        "case {case:?}: expected DynamoDB to report point-in-time recovery {wanted} on \
         {table:?}, got {reported:?}"
    );
}

/// Look again until `condition` answers `Some`, up to `attempts` times — a wait on the
/// CONDITION, which is what every span DynamoDB owes here falls due against.
async fn wait_until<T, F, Fut>(attempts: usize, mut condition: F) -> Option<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    for _ in 0..attempts {
        if let Some(answer) = condition().await {
            return Some(answer);
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    condition().await
}

/// The two properties documents one logical resource's drift record carries, as the response
/// spells them: `(ExpectedProperties, ActualProperties)`.
async fn properties_documents(
    router: &MultiServiceRouter,
    stack: &str,
    logical_id: &str,
) -> (Option<String>, Option<String>) {
    let (status, body) = call(
        router,
        "DescribeStackResourceDrifts",
        &[member("StackName", stack)],
    )
    .await;
    assert_eq!(
        status, 200,
        "DescribeStackResourceDrifts({stack}) answered {status} with {body}"
    );
    list_members(&body, "StackResourceDrifts")
        .into_iter()
        .find(|block| xml_text(block, "LogicalResourceId").as_deref() == Some(logical_id))
        .map(|block| {
            (
                xml_text(&block, "ExpectedProperties"),
                xml_text(&block, "ActualProperties"),
            )
        })
        .unwrap_or_default()
}

/// One properties document as the document it is.
fn parsed(case: &str, what: &str, text: Option<&String>) -> Value {
    let text = text.unwrap_or_else(|| {
        panic!("case {case:?}: expected the record to report its {what}, got none")
    });
    serde_json::from_str(text)
        .unwrap_or_else(|error| panic!("case {case:?}: {text:?} is not a document: {error}"))
}

/// Assert one row's record: the status it adds up to, the differences it reports, and the two
/// properties documents the comparison was carried out over.
async fn assert_record(
    router: &MultiServiceRouter,
    stack: &str,
    row: &Row,
    report: &[(String, crate::ec2_network_stack_drift::ReportedDrift)],
) {
    let case = row.case;
    let record = record_of(case, report, row.logical_id);
    assert_eq!(
        record.physical_resource_id, row.table,
        "case {case:?}: expected the record to name the table the stack published, got {:?}",
        record.physical_resource_id
    );
    assert_eq!(
        record.status, row.status,
        "case {case:?}: expected {}, got {} with {:?}",
        row.status, record.status, record.differences
    );
    let wanted: Vec<(String, String, String, String)> = row
        .differences
        .iter()
        .map(|(path, expected, actual, kind)| {
            (
                (*path).to_string(),
                (*expected).to_string(),
                (*actual).to_string(),
                (*kind).to_string(),
            )
        })
        .collect();
    assert_eq!(
        record.differences, wanted,
        "case {case:?}: expected the differences {wanted:?}, got {:?}",
        record.differences
    );

    let (expected_properties, actual_properties) =
        properties_documents(router, stack, row.logical_id).await;
    if row.status == resource_drift_status::DELETED {
        // "For resources whose StackResourceDriftStatus is DELETED", neither structure "will
        // be present": there is no configuration left to report, and none was compared.
        assert_eq!(
            (expected_properties.as_deref(), actual_properties.as_deref()),
            (None, None),
            "case {case:?}: expected a deleted resource to report neither properties document, \
             got {expected_properties:?} and {actual_properties:?}"
        );
        return;
    }
    // The DECLARATION is reported in full, whatever the comparison established about it.
    let declared = declaration(row);
    let declared = declared
        .pointer("/Properties")
        .expect("the row's declared properties");
    let reported = parsed(case, "expected properties", expected_properties.as_ref());
    assert_eq!(
        &reported, declared,
        "case {case:?}: expected the record to report the declaration {declared}, got {reported}"
    );
    // ...and the ACTUAL side is what the owning service answered: every leaf the record
    // reports a difference for carries that service's value there.
    let actual = parsed(case, "actual properties", actual_properties.as_ref());
    for (path, _, value, _) in &row.differences {
        let at = actual.pointer(path).cloned();
        assert_eq!(
            at.as_ref().map(ToString::to_string).as_deref(),
            Some(*value),
            "case {case:?}: expected the actual properties to carry {value:?} at {path}, got \
             {at:?} in {actual}"
        );
    }
}

/// RULE: a drift check tells a managed table whose properties were changed outside the stack
/// (`MODIFIED`, with the differences and both properties documents) from one the owning
/// service no longer holds (`DELETED`, with neither) — and from one that still matches its
/// declaration (`IN_SYNC`).
#[tokio::test]
async fn an_out_of_band_change_to_a_managed_table_is_reported_modified() {
    let router = router();
    let stack = "ddb-drift";
    let rows = rows();
    let resources: serde_json::Map<String, Value> = rows
        .iter()
        .map(|row| (row.logical_id.to_string(), declaration(row)))
        .collect();
    let template = json!({"Resources": resources}).to_string();

    let (status, body) = create_stack(&router, stack, &template, &[]).await;
    assert_eq!(
        status, 200,
        "CreateStack answered {status} with {body}"
    );
    let described = poll_to_terminal(&router, "the deployment", stack).await;
    let settled = xml_text(&described, "StackStatus").unwrap_or_default();
    assert_eq!(
        settled, "CREATE_COMPLETE",
        "expected the create to complete, got {settled:?} because {:?} in {described}",
        failure_reasons(&router, stack).await
    );

    for row in &rows {
        tamper_with(&router, row).await;
    }

    let (detection_status, stack_drift, drifted_count) =
        settled_detection("the out-of-band changes", &router, stack).await;
    assert_eq!(
        detection_status,
        drift_detection_status::DETECTION_COMPLETE,
        "expected the detection to complete, got {detection_status:?}"
    );
    assert_eq!(
        stack_drift, DRIFT_STATUS_DRIFTED,
        "expected the stack to be reported {DRIFT_STATUS_DRIFTED}, got {stack_drift:?}"
    );
    let drifted: usize = rows
        .iter()
        .filter(|row| row.status != resource_drift_status::IN_SYNC)
        .count();
    assert_eq!(
        drifted_count,
        drifted.to_string(),
        "expected {drifted} drifted resources, got {drifted_count:?}"
    );

    let report = drift_report("the out-of-band changes", &router, stack).await;
    for row in &rows {
        assert_record(&router, stack, row, &report).await;
    }
}

/// RULE: the comparison is carried out against the owning service on every check rather than
/// latched, so putting the changed property back the way the template declares it returns the
/// resource to `IN_SYNC`.
#[tokio::test]
async fn reverting_the_out_of_band_change_returns_the_table_to_in_sync() {
    let router = router();
    let stack = "ddb-drift-reverted";
    let case = "continuous backups turned on and then off again";
    let table = "drift-revert-table";
    let mut properties = base_properties(table);
    properties
        .as_object_mut()
        .expect("the declaration is an object")
        .insert(
            "PointInTimeRecoverySpecification".to_string(),
            json!({"PointInTimeRecoveryEnabled": false}),
        );
    let template = json!({
        "Resources": {"Table": {"Type": "AWS::DynamoDB::Table", "Properties": properties}},
    })
    .to_string();

    let (status, body) = create_stack(&router, stack, &template, &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateStack answered {status} with {body}"
    );
    let described = poll_to_terminal(&router, case, stack).await;
    let settled = xml_text(&described, "StackStatus").unwrap_or_default();
    assert_eq!(
        settled, "CREATE_COMPLETE",
        "case {case:?}: expected the create to complete, got {settled:?} in {described}"
    );

    // Changed outside the stack: the resource is reported MODIFIED...
    set_continuous_backups(&router, case, table, true).await;
    let (_, drifted, _) = settled_detection(case, &router, stack).await;
    assert_eq!(
        drifted, DRIFT_STATUS_DRIFTED,
        "case {case:?}: expected the stack to be reported {DRIFT_STATUS_DRIFTED}, got \
         {drifted:?}"
    );
    let report = drift_report(case, &router, stack).await;
    let record = record_of(case, &report, "Table");
    assert_eq!(
        record.status,
        resource_drift_status::MODIFIED,
        "case {case:?}: expected {} after the change, got {} with {:?}",
        resource_drift_status::MODIFIED,
        record.status,
        record.differences
    );

    // ...and put back the way the template declares it, the next check reports it IN_SYNC.
    set_continuous_backups(&router, case, table, false).await;
    let (_, reverted, count) = settled_detection(case, &router, stack).await;
    assert_eq!(
        (reverted.as_str(), count.as_str()),
        (
            service_com_amazonaws_cloudformation_cloudformation::state::DRIFT_STATUS_IN_SYNC,
            "0"
        ),
        "case {case:?}: expected the reverted stack to be in sync with no drifted resource, \
         got {reverted:?} with {count:?}"
    );
    let report = drift_report(case, &router, stack).await;
    let record = record_of(case, &report, "Table");
    assert_eq!(
        record.status,
        resource_drift_status::IN_SYNC,
        "case {case:?}: expected {} once the change was reverted, got {} with {:?}",
        resource_drift_status::IN_SYNC,
        record.status,
        record.differences
    );
}
