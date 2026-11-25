// SPDX-License-Identifier: CC0-1.0

//! The WHOLE life of a change-data-capture estate a stack declares: the `AWS::Kinesis::Stream`
//! resources, the `AWS::DynamoDB::Table`s whose `KinesisStreamSpecification` replicates to
//! them, the inventory of streaming tables a client then derives per Region, and the deletion
//! that takes the estate away again.
//!
//! RULE: a stack's declared resources live in the services that own them AND in the Region the
//! stack was deployed in — `CREATE_COMPLETE` means each one exists there carrying the
//! configuration its declaration asked for, a completed deletion means none of them is held
//! there any more, and neither reaches another Region's estate ("table names must be unique
//! within each Region ... you can have two tables with same name if you create the tables in
//! different Regions", the `CreateTable` doc). So a client that deploys this estate can list
//! the Region's tables, ask each one for its streaming destination, and get an inventory of
//! exactly the tables whose declaration named one, pointed at exactly the data streams the
//! same stack created in that Region; and after `DeleteStack` that Region's tables and streams
//! are gone while the other Region's are untouched.
//!
//! The rows differ along what the rule generalises over: which Region the estate is deployed
//! in (neither row's assertions hang on the emulator's default one), how many streaming tables
//! the estate carries (one, and two with a data stream each), whether it also carries a table
//! that declares NO destination, and the capacity mode of each streaming table
//! (`ProvisionedThroughput` and `PAY_PER_REQUEST`). Both rows declare their resources under
//! the SAME physical names in their own Region, so an inventory that reached across Regions —
//! or a deletion that did — shows up as the wrong estate rather than passing unnoticed.
//!
//! `dynamodb_kinesis_streaming_estate.rs` is the sibling suite about ONE deployment of such an
//! estate (the capacity the table declares, the precision the destination replicates under);
//! this one is about the lifecycle around it — the per-Region inventory, and the teardown.
//!
//! The CloudFormation calls go over the awsQuery wire this crate's harness owns, each signed
//! for its row's Region, and the DynamoDB and Kinesis reads as a client's SDK makes them,
//! through ONE router carrying all three services on one engine, exactly as the emulator
//! mounts them.

use std::time::Duration;

use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::dynamodb_table_resources::dynamodb_in;
use crate::harness::{
    call_in, member, router, settled_stack, stack_event_reasons, xml_text, REGION,
};
use crate::stack_lifecycle::{kinesis_in, output_value, stream_arn_in};
use crate::stack_owned_secret_teardown::error_code;

/// The `StackStatus` a deployment of an estate settles on.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The stack name both rows deploy under: one name per Region is one stack per Region
/// (`CreateStackInput$StackName`: "the name must be unique in the Region in which you are
/// creating the stack").
const STACK: &str = "ddbkds-estate-lifecycle";

/// The partition key every declared table carries.
const KEY: &str = "id";

/// The error code both owning services answer for a resource they hold none of
/// (`ResourceNotFoundException`), which is what says a deletion really took it away.
const RESOURCE_NOT_FOUND: &str = "ResourceNotFoundException";

/// The `DestinationStatus` a registered destination settles in: "ACTIVE - Enabling is
/// complete" (`DescribeKinesisStreamingDestination`'s own status enum).
const ACTIVE: &str = "ACTIVE";

/// How often a row asks whether something it is waiting for has settled, and how long it goes
/// on asking.
///
/// The budget covers the spans the stack, the data streams, the tables and the streaming
/// destinations each owe, one after the other, so a loaded machine is slower than those spans
/// without the rule being any different; what a row waits for is the CONDITION, never a time.
const POLL_INTERVAL: Duration = Duration::from_millis(50);
const POLLS: usize = 1_200;

/// One row: the Region the estate is deployed in, the streams and the tables replicating to
/// them, and the tables that declare no destination at all.
struct Row {
    case: &'static str,
    region: &'static str,
    /// `(stream name, table name)` pairs: one declared data stream, and the table whose
    /// `KinesisStreamSpecification` names it through `Fn::GetAtt`.
    streaming: &'static [(&'static str, &'static str)],
    /// The tables the estate declares with no streaming destination.
    plain: &'static [&'static str],
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "one streaming table and one plain table, in the default Region",
            region: REGION,
            streaming: &[("home-changes", "home-streaming")],
            plain: &["home-plain"],
        },
        Row {
            case: "the same names in a second Region, with two streaming tables and no plain one",
            region: "us-west-2",
            streaming: &[
                ("home-changes", "home-streaming"),
                ("edge-changes", "edge-streaming"),
            ],
            plain: &[],
        },
    ]
}

/// Every table name one row's estate declares, in the ascending order `ListTables` reports
/// them in ("The names of the tables associated with the current account at the current
/// endpoint", answered sorted).
fn declared_tables(row: &Row) -> Vec<String> {
    let mut names: Vec<String> = row
        .streaming
        .iter()
        .map(|(_, table)| (*table).to_string())
        .chain(row.plain.iter().map(|table| (*table).to_string()))
        .collect();
    names.sort();
    names
}

/// The ARN of one declared data stream in the Region its stack was deployed in.
fn stream_arn(row: &Row, stream: &str) -> String {
    stream_arn_in(row.region, stream)
}

/// The inventory one row's estate must answer: the streaming tables, each with the ARN of the
/// stream it replicates to, in table-name order.
fn expected_inventory(row: &Row) -> Vec<(String, String)> {
    let mut inventory: Vec<(String, String)> = row
        .streaming
        .iter()
        .map(|(stream, table)| ((*table).to_string(), stream_arn(row, stream)))
        .collect();
    inventory.sort();
    inventory
}

/// The `KeySchema`/`AttributeDefinitions` pair every declared table carries.
fn key_members() -> (Value, Value) {
    (
        json!([{"AttributeName": KEY, "AttributeType": "S"}]),
        json!([{"AttributeName": KEY, "KeyType": "HASH"}]),
    )
}

/// The capacity the streaming table at `index` declares: the two modes the type offers, so the
/// rule is not pinned to either (`BillingMode` left out is `PROVISIONED`, which is the mode
/// `ProvisionedThroughput` belongs to).
fn capacity(index: usize) -> Map<String, Value> {
    let mut members = Map::new();
    if index.is_multiple_of(2) {
        members.insert(
            "ProvisionedThroughput".to_string(),
            json!({"ReadCapacityUnits": 5, "WriteCapacityUnits": 5}),
        );
    } else {
        members.insert("BillingMode".to_string(), json!("PAY_PER_REQUEST"));
    }
    members
}

/// The template one row deploys: a data stream and a streaming table per entry of its
/// inventory, the tables that declare no destination, and the outputs a client reads the
/// estate through.
fn template(row: &Row) -> String {
    let (attributes, schema) = key_members();
    let mut resources = Map::new();
    let mut outputs = Map::new();
    for (index, (stream, table)) in row.streaming.iter().enumerate() {
        let stream_id = format!("Stream{index}");
        let table_id = format!("StreamingTable{index}");
        resources.insert(
            stream_id.clone(),
            json!({
                "Type": "AWS::Kinesis::Stream",
                "Properties": {"Name": stream, "ShardCount": 1},
            }),
        );
        let mut properties = capacity(index);
        properties.insert("TableName".to_string(), json!(table));
        properties.insert("AttributeDefinitions".to_string(), attributes.clone());
        properties.insert("KeySchema".to_string(), schema.clone());
        properties.insert(
            "KinesisStreamSpecification".to_string(),
            json!({"StreamArn": {"Fn::GetAtt": [stream_id.clone(), "Arn"]}}),
        );
        resources.insert(
            table_id.clone(),
            json!({"Type": "AWS::DynamoDB::Table", "Properties": properties}),
        );
        outputs.insert(
            format!("StreamArn{index}"),
            json!({"Value": {"Fn::GetAtt": [stream_id, "Arn"]}}),
        );
        outputs.insert(
            format!("StreamTableName{index}"),
            json!({"Value": {"Ref": table_id}}),
        );
    }
    for (index, table) in row.plain.iter().enumerate() {
        let table_id = format!("PlainTable{index}");
        resources.insert(
            table_id.clone(),
            json!({
                "Type": "AWS::DynamoDB::Table",
                "Properties": {
                    "TableName": table,
                    "BillingMode": "PAY_PER_REQUEST",
                    "AttributeDefinitions": attributes.clone(),
                    "KeySchema": schema.clone(),
                },
            }),
        );
        outputs.insert(
            format!("PlainTableName{index}"),
            json!({"Value": {"Ref": table_id}}),
        );
    }
    json!({"Resources": resources, "Outputs": outputs}).to_string()
}

/// Deploy one row's estate and answer the `DescribeStacks` body of the settled stack, having
/// asserted that it completed.
async fn deployed(router: &MultiServiceRouter, row: &Row) -> String {
    let case = row.case;
    let settled = settled_stack(case, router, row.region, STACK).await;
    let reported = xml_text(&settled, "StackStatus").unwrap_or_default();
    assert_eq!(
        reported, CREATE_COMPLETE,
        "case {case:?}: expected the estate to reach {CREATE_COMPLETE}, got {reported:?} \
         because {:?}",
        stack_event_reasons(router, row.region, STACK).await
    );
    settled
}

/// The names of the tables `region` holds, as that Region's own `ListTables` reports them.
async fn tables_in(router: &MultiServiceRouter, region: &str) -> Vec<String> {
    let (status, answered) = dynamodb_in(router, region, "ListTables", &json!({})).await;
    assert_eq!(
        status, 200,
        "ListTables in {region:?}: expected HTTP 200, got {status} with body {answered}"
    );
    answered
        .get("TableNames")
        .and_then(Value::as_array)
        .map(|names| {
            names
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// The inventory of streaming tables a client derives in `region`: for each table the Region
/// holds, the stream its destination replicates to where DynamoDB reports one ACTIVE.
///
/// Registering a destination is asynchronous in that service, so the inventory is derived
/// again until it holds `wanted` entries or the budget passes — the CONDITION a client's own
/// waiter watches for.
async fn active_streaming_inventory(
    router: &MultiServiceRouter,
    region: &str,
    wanted: usize,
) -> Vec<(String, String)> {
    let mut inventory = Vec::new();
    for _ in 0..POLLS {
        inventory = Vec::new();
        for table in tables_in(router, region).await {
            let (status, answered) = dynamodb_in(
                router,
                region,
                "DescribeKinesisStreamingDestination",
                &json!({"TableName": &table}),
            )
            .await;
            assert_eq!(
                status, 200,
                "DescribeKinesisStreamingDestination {table:?} in {region:?}: expected HTTP \
                 200, got {status} with body {answered}"
            );
            let destinations = answered
                .get("KinesisDataStreamDestinations")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            for destination in destinations {
                if destination.get("DestinationStatus") == Some(&json!(ACTIVE)) {
                    let arn = destination
                        .get("StreamArn")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    inventory.push((table.clone(), arn));
                }
            }
        }
        // Equal is the settled inventory; MORE than that is a destination the estate never
        // declared, which the assertion below reports rather than being waited out.
        if inventory.len() >= wanted {
            break;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    inventory.sort();
    inventory
}

/// Read `table` in `region` until that service stops holding it, and answer the status and
/// the error code of the refusal; the 200 it kept answering where the budget passes first.
async fn table_after_teardown(
    case: &str,
    router: &MultiServiceRouter,
    region: &str,
    table: &str,
) -> (u16, String) {
    for _ in 0..POLLS {
        let (status, answered) =
            dynamodb_in(router, region, "DescribeTable", &json!({"TableName": table})).await;
        if status != 200 {
            return (status, error_code(case, &answered));
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    (200, String::new())
}

/// The same read for a data stream, through Kinesis's own summary call.
async fn stream_after_teardown(
    case: &str,
    router: &MultiServiceRouter,
    region: &str,
    stream: &str,
) -> (u16, String) {
    for _ in 0..POLLS {
        let (status, answered) = kinesis_in(
            router,
            region,
            "DescribeStreamSummary",
            &json!({"StreamName": stream}),
        )
        .await;
        if status != 200 {
            return (status, error_code(case, &answered));
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    (200, String::new())
}

/// Every row: the estate is deployed in its own Region, the inventory a client derives there
/// is exactly that estate's streaming tables on that Region's streams, and the deletion of ONE
/// Region's stack takes its tables and its streams away while the other Region's estate goes
/// on answering.
#[tokio::test]
async fn a_declared_streaming_estate_is_regional_and_is_torn_down_with_its_stack() {
    let router = router();
    let rows = rows();

    // Both estates are asked for before either is waited on, so the Regions provision
    // alongside each other rather than one after the other.
    for row in &rows {
        let case = row.case;
        let (status, answer) = call_in(
            &router,
            row.region,
            "CreateStack",
            &[
                member("StackName", STACK),
                member("TemplateBody", &template(row)),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {status} {answer}"
        );
    }

    for row in &rows {
        let case = row.case;
        let settled = deployed(&router, row).await;

        // The outputs a client reads the estate through: every declared name and stream ARN
        // carries a value.
        for (index, (stream, table)) in row.streaming.iter().enumerate() {
            let published = output_value(&settled, &format!("StreamTableName{index}"));
            assert_eq!(
                published.as_deref(),
                Some(*table),
                "case {case:?}: expected Ref of the streaming table to answer its declared \
                 name, got {published:?}"
            );
            let arn = output_value(&settled, &format!("StreamArn{index}"));
            assert_eq!(
                arn.as_deref(),
                Some(stream_arn(row, stream).as_str()),
                "case {case:?}: expected the stack's stream ARN output to be the data stream's \
                 own ARN in its Region, got {arn:?}"
            );
        }
        for (index, table) in row.plain.iter().enumerate() {
            let published = output_value(&settled, &format!("PlainTableName{index}"));
            assert_eq!(
                published.as_deref(),
                Some(*table),
                "case {case:?}: expected Ref of the table that declared no destination to \
                 answer its declared name, got {published:?}"
            );
        }

        // Every declared table exists in that Region, and in that Region only the estate's own
        // tables are there.
        let held = tables_in(&router, row.region).await;
        assert_eq!(
            held,
            declared_tables(row),
            "case {case:?}: expected {} to hold exactly the estate's tables, got {held:?} \
             because {:?}",
            row.region,
            stack_event_reasons(&router, row.region, STACK).await
        );

        // The inventory: the tables whose declaration named a destination, on the streams the
        // same stack created in that Region.
        let inventory = active_streaming_inventory(&router, row.region, row.streaming.len()).await;
        assert_eq!(
            inventory,
            expected_inventory(row),
            "case {case:?}: expected the Region's streaming inventory to be the estate's own \
             tables on its own streams, got {inventory:?}"
        );
    }

    // The teardown: ONE Region's stack is deleted.
    let torn_down = rows.last().expect("a row to tear down");
    let case = torn_down.case;
    let (status, answer) = call_in(
        &router,
        torn_down.region,
        "DeleteStack",
        &[member("StackName", STACK)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DeleteStack, got {status} {answer}"
    );
    let left = settled_stack(case, &router, torn_down.region, STACK).await;
    assert!(
        left.is_empty(),
        "case {case:?}: expected the deleted stack to be reported no more, got {left}"
    );

    // Its tables and its streams are gone from the services that held them.
    for (stream, table) in torn_down.streaming {
        let gone = table_after_teardown(case, &router, torn_down.region, table).await;
        assert_eq!(
            (gone.0, gone.1.as_str()),
            (400, RESOURCE_NOT_FOUND),
            "case {case:?}: expected DynamoDB to hold the deleted table {table:?} no more, got \
             {gone:?}"
        );
        let gone = stream_after_teardown(case, &router, torn_down.region, stream).await;
        assert_eq!(
            (gone.0, gone.1.as_str()),
            (400, RESOURCE_NOT_FOUND),
            "case {case:?}: expected Kinesis to hold the deleted stream {stream:?} no more, got \
             {gone:?}"
        );
    }
    let left = tables_in(&router, torn_down.region).await;
    assert!(
        left.is_empty(),
        "case {case:?}: expected the torn-down Region to hold no table of the estate, got \
         {left:?}"
    );

    // And the OTHER Region's estate, declared under the same names, still answers: a deletion
    // reaches the Region its stack was deployed in and no other.
    let kept = rows.first().expect("a row that stays");
    let case = kept.case;
    let held = tables_in(&router, kept.region).await;
    assert_eq!(
        held,
        declared_tables(kept),
        "case {case:?}: expected the untouched Region to hold its own estate still, got {held:?}"
    );
    let inventory = active_streaming_inventory(&router, kept.region, kept.streaming.len()).await;
    assert_eq!(
        inventory,
        expected_inventory(kept),
        "case {case:?}: expected the untouched Region's streaming inventory to be unchanged, \
         got {inventory:?}"
    );
}
