// SPDX-License-Identifier: CC0-1.0

//! An `AWS::DynamoDB::Table` a template does not name is a REAL table in Amazon DynamoDB,
//! under the very physical id the completed stack hands its clients.
//!
//! The rule under test: **a physical resource id CloudFormation mints for a resource it
//! reports `CREATE_COMPLETE` resolves in the service that OWNS the resource, under every
//! handle the stack publishes that id through.** For an `AWS::DynamoDB::Table` whose
//! declaration carries no `TableName`, the generated id is the name the table was CREATED
//! under, so DynamoDB's own reads answer for it — `DescribeTable` reports `TableStatus`
//! `ACTIVE` for it, `ListTables` names it — and its data plane serves it. A read of such an
//! id can therefore never be the refusal DynamoDB's model bounds that read with
//! (`ResourceNotFoundException`, "The operation tried to access a nonexistent table or
//! index. The resource might not be specified correctly, or its status might not be
//! ACTIVE.", client, HTTP 400), because a resource CloudFormation says it created and a
//! resource DynamoDB says it never had are not both true of one name.
//!
//! Two crates have to agree for that to hold — CloudFormation decides the name and makes the
//! create, DynamoDB holds the table and decides its status — so the suite lives here rather
//! than in either crate: a CloudFormation-only test can see the id and the stack status but
//! not whether a table exists behind them.
//!
//! Why it matters: a generated name is the ordinary case for infrastructure as code (a CDK
//! `Table` with no `tableName`, a template that leaves the name out to keep the stack
//! re-deployable), and the id a stack publishes through `Ref`, an output, an export another
//! stack imports or an SSM parameter is the ONLY handle its clients have. If that id resolves
//! nowhere, a deployment that reports success has delivered nothing usable.
//!
//! The rows of [`rows`] differ along what the rule generalises over — the declaration whose
//! table the generated id has to name: the read/write mode (on-demand, provisioned), the key
//! schema (one partition key, a partition key with a sort key), a declared stream, a declared
//! global secondary index, and how many unnamed tables one stack declares. The template that
//! DOES name its table is the control row: the published id is then the declared name, and it
//! resolves just the same. [`rows_with_an_importing_stack`] carries the same rule over the
//! handles the id travels on between stacks: an export another stack imports into an SSM
//! parameter of its own, and one it imports into an output of its own.
//!
//! The awsQuery caller, the poll that waits for a stack to first report a status, the XML
//! readers and the awsJson callers for DynamoDB and AWS Systems Manager are each written ONCE
//! in this test crate and shared, so this suite adds no wire caller of its own.

use std::collections::BTreeSet;
use std::sync::Arc;

use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_apigatewayv2_resources::{cloudformation, REGION};
use crate::cloudformation_dynamodb_table_in_place_update::{described_table, dynamodb};
use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::cloudformation_lambda_ssm_sourced_update::ssm;
use crate::query_wire::{elements, outputs_of, pairs_of, physical_ids_of};

/// The `StackStatus` a completed create reports.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The `TableStatus` a table ready for use reports: "`ACTIVE` - The table is ready for use"
/// (`TableDescription$TableStatus`).
const ACTIVE: &str = "ACTIVE";

/// The resource type every row declares.
const TABLE_TYPE: &str = "AWS::DynamoDB::Table";

/// How many characters CloudFormation's generated-name unique segment carries, and the
/// shape it completes: `<stack name>-<logical id>-<unique segment>`.
const GENERATED_UNIQUE_LEN: usize = 12;

/// DynamoDB's own bounds on the member every call this suite makes addresses a table by.
///
/// Source: `TableName` in DynamoDB's model
/// (`smithy_query(query="shape",shape="TableName")`): `@length(min: 3, max: 255)` with
/// pattern `^[a-zA-Z0-9_.-]+$`. A generated id outside them is one that service could not
/// have created the table under at all.
const TABLE_NAME_MIN: usize = 3;
const TABLE_NAME_MAX: usize = 255;

/// The emulator with the three services this suite exercises, mounted on ONE engine exactly
/// as `core/router/src/lib.rs` mounts them.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_dynamodb_dynamodb_20120810::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_ssm_amazonssm::EmulatedService::new(
        engine,
    ));
    router
}

/// One table a row declares.
struct DeclaredTable {
    /// The logical id the template declares it under, which the generated name carries.
    logical_id: &'static str,
    /// The name the TEMPLATE states, or `None` — the case the rule is about — where naming
    /// the table is CloudFormation's.
    declared_name: Option<&'static str>,
    /// The partition key, and the sort key where the row declares a composite one.
    partition_key: &'static str,
    sort_key: Option<&'static str>,
    /// Attribute definitions beyond the key ones, as `(name, type)`: an index key has to be
    /// defined as an attribute.
    extra_attributes: Vec<(&'static str, &'static str)>,
    /// The properties declared beside the key schema and the attribute definitions.
    properties: Value,
    /// JSON pointers into the `Table` a settled `DescribeTable` answers, each with the value
    /// this declaration puts there — computed from the row, never read out of the answer
    /// being checked.
    in_force: Vec<(&'static str, Value)>,
}

impl DeclaredTable {
    /// A declaration with one partition key of type `S` and nothing else stated.
    fn keyed(logical_id: &'static str, partition_key: &'static str) -> Self {
        Self {
            logical_id,
            declared_name: None,
            partition_key,
            sort_key: None,
            extra_attributes: Vec::new(),
            properties: json!({}),
            in_force: Vec::new(),
        }
    }

    /// The same declaration with `properties` stated beside the key schema.
    fn with(mut self, properties: Value) -> Self {
        self.properties = properties;
        self
    }

    /// The same declaration, with the members a settled read has to report for it.
    fn reporting(mut self, in_force: Vec<(&'static str, Value)>) -> Self {
        self.in_force = in_force;
        self
    }

    /// The `Properties` of this table's resource declaration.
    fn declaration(&self) -> Value {
        let mut key_schema = vec![json!({"AttributeName": self.partition_key, "KeyType": "HASH"})];
        let mut attributes =
            vec![json!({"AttributeName": self.partition_key, "AttributeType": "S"})];
        if let Some(sort) = self.sort_key {
            key_schema.push(json!({"AttributeName": sort, "KeyType": "RANGE"}));
            attributes.push(json!({"AttributeName": sort, "AttributeType": "S"}));
        }
        for (name, attribute_type) in &self.extra_attributes {
            attributes.push(json!({"AttributeName": name, "AttributeType": attribute_type}));
        }
        let mut declared = Map::new();
        if let Some(name) = self.declared_name {
            declared.insert("TableName".to_string(), json!(name));
        }
        declared.insert("KeySchema".to_string(), Value::Array(key_schema));
        declared.insert("AttributeDefinitions".to_string(), Value::Array(attributes));
        for (member, value) in self
            .properties
            .as_object()
            .expect("a table's properties are an object")
        {
            declared.insert(member.clone(), value.clone());
        }
        Value::Object(declared)
    }

    /// The primary key of the item the data-plane round trip writes to this table.
    fn item_key(&self) -> Map<String, Value> {
        let mut key = Map::new();
        key.insert(self.partition_key.to_string(), json!({"S": "round-trip"}));
        if let Some(sort) = self.sort_key {
            key.insert(sort.to_string(), json!({"S": "sort-value"}));
        }
        key
    }
}

/// The output key a row's template publishes a table's `Ref` under.
fn ref_output_key(logical_id: &str) -> String {
    format!("{logical_id}Ref")
}

/// The template a row deploys: its tables, each publishing its `Ref` as an output — the
/// handle a client reads the name off.
fn template(tables: &[DeclaredTable]) -> String {
    let mut resources = Map::new();
    let mut outputs = Map::new();
    for table in tables {
        resources.insert(
            table.logical_id.to_string(),
            json!({"Type": TABLE_TYPE, "Properties": table.declaration()}),
        );
        outputs.insert(
            ref_output_key(table.logical_id),
            json!({"Value": {"Ref": table.logical_id}}),
        );
    }
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": Value::Object(resources),
        "Outputs": Value::Object(outputs),
    })
    .to_string()
}

/// The properties an on-demand table is declared with.
fn on_demand() -> Value {
    json!({"BillingMode": "PAY_PER_REQUEST"})
}

/// The properties a provisioned table is declared with at `read`/`write` capacity.
fn provisioned(read: i64, write: i64) -> Value {
    json!({
        "BillingMode": "PROVISIONED",
        "ProvisionedThroughput": {"ReadCapacityUnits": read, "WriteCapacityUnits": write},
    })
}

/// One row of the table: what one stack declares.
struct Row {
    case: &'static str,
    tables: Vec<DeclaredTable>,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            // The recorded shape: an unnamed on-demand table with a single partition key.
            case: "an unnamed on-demand table with one partition key",
            tables: vec![DeclaredTable::keyed("Table", "pk")
                .with(on_demand())
                .reporting(vec![
                    ("/BillingModeSummary/BillingMode", json!("PAY_PER_REQUEST")),
                    (
                        "/KeySchema",
                        json!([{"AttributeName": "pk", "KeyType": "HASH"}]),
                    ),
                ])],
        },
        Row {
            case: "an unnamed provisioned table with a composite key",
            tables: vec![DeclaredTable {
                sort_key: Some("sk"),
                ..DeclaredTable::keyed("Events", "pk")
            }
            .with(provisioned(3, 4))
            .reporting(vec![
                ("/ProvisionedThroughput/ReadCapacityUnits", json!(3)),
                ("/ProvisionedThroughput/WriteCapacityUnits", json!(4)),
                (
                    "/KeySchema",
                    json!([
                        {"AttributeName": "pk", "KeyType": "HASH"},
                        {"AttributeName": "sk", "KeyType": "RANGE"},
                    ]),
                ),
            ])],
        },
        Row {
            case: "an unnamed table whose declaration enables a stream",
            tables: vec![DeclaredTable::keyed("Streamed", "pk")
                .with(json!({
                    "BillingMode": "PAY_PER_REQUEST",
                    "StreamSpecification": {"StreamViewType": "NEW_AND_OLD_IMAGES"},
                }))
                .reporting(vec![
                    ("/StreamSpecification/StreamEnabled", json!(true)),
                    (
                        "/StreamSpecification/StreamViewType",
                        json!("NEW_AND_OLD_IMAGES"),
                    ),
                ])],
        },
        Row {
            case: "an unnamed table carrying a global secondary index",
            tables: vec![DeclaredTable {
                extra_attributes: vec![("gsi1pk", "S")],
                ..DeclaredTable::keyed("Indexed", "pk")
            }
            .with(json!({
                "BillingMode": "PAY_PER_REQUEST",
                "GlobalSecondaryIndexes": [{
                    "IndexName": "ByGsi1",
                    "KeySchema": [{"AttributeName": "gsi1pk", "KeyType": "HASH"}],
                    "Projection": {"ProjectionType": "ALL"},
                }],
            }))
            .reporting(vec![
                ("/GlobalSecondaryIndexes/0/IndexName", json!("ByGsi1")),
                (
                    "/GlobalSecondaryIndexes/0/KeySchema",
                    json!([{"AttributeName": "gsi1pk", "KeyType": "HASH"}]),
                ),
            ])],
        },
        Row {
            // Two unnamed tables of one stack are two tables, each under its own id.
            case: "two unnamed tables declared by one stack",
            tables: vec![
                DeclaredTable::keyed("First", "pk")
                    .with(on_demand())
                    .reporting(vec![(
                        "/BillingModeSummary/BillingMode",
                        json!("PAY_PER_REQUEST"),
                    )]),
                DeclaredTable::keyed("Second", "id")
                    .with(provisioned(1, 1))
                    .reporting(vec![(
                        "/ProvisionedThroughput/ReadCapacityUnits",
                        json!(1),
                    )]),
            ],
        },
        Row {
            // The control row: a template that NAMES its table publishes that name, and the
            // table resolves under it just the same.
            case: "a table the template names itself",
            tables: vec![DeclaredTable {
                declared_name: Some("cfn-declared-table-name"),
                ..DeclaredTable::keyed("Named", "pk")
            }
            .with(on_demand())
            .reporting(vec![(
                "/BillingModeSummary/BillingMode",
                json!("PAY_PER_REQUEST"),
            )])],
        },
    ]
}

/// A generated physical id is `<stack name>-<logical id>-<unique segment>`, the segment
/// being [`GENERATED_UNIQUE_LEN`] characters of upper-case letters and digits.
fn assert_generated_shape(case: &str, stack: &str, logical_id: &str, physical_id: &str) {
    let prefix = format!("{stack}-{logical_id}-");
    let Some(unique) = physical_id.strip_prefix(&prefix) else {
        panic!(
            "case {case:?}: expected the generated id to start with {prefix:?}, got \
             {physical_id:?}"
        );
    };
    assert_eq!(
        unique.chars().count(),
        GENERATED_UNIQUE_LEN,
        "case {case:?}: expected a {GENERATED_UNIQUE_LEN}-character unique segment, got \
         {unique:?} in {physical_id:?}"
    );
    assert!(
        unique
            .chars()
            .all(|character| character.is_ascii_uppercase() || character.is_ascii_digit()),
        "case {case:?}: expected the unique segment to be upper-case alphanumerics, got \
         {unique:?} in {physical_id:?}"
    );
}

/// A name DynamoDB could have created the table under: inside `TableName`'s own bounds.
fn assert_within_dynamodb_name_bounds(case: &str, physical_id: &str) {
    let length = physical_id.chars().count();
    assert!(
        (TABLE_NAME_MIN..=TABLE_NAME_MAX).contains(&length),
        "case {case:?}: expected the published id to be {TABLE_NAME_MIN}..={TABLE_NAME_MAX} \
         characters, got {length} in {physical_id:?}"
    );
    assert!(
        physical_id.chars().all(|character| character.is_ascii_alphanumeric()
            || character == '_'
            || character == '.'
            || character == '-'),
        "case {case:?}: expected the published id to match DynamoDB's TableName pattern, got \
         {physical_id:?}"
    );
}

/// The `TableNames` DynamoDB's own `ListTables` reports right now.
async fn listed_tables(router: &MultiServiceRouter, case: &str) -> Vec<String> {
    let (status, listed) = dynamodb(router, "ListTables", &json!({})).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from ListTables, got {status} with {listed}"
    );
    listed["TableNames"]
        .as_array()
        .map(|names| {
            names
                .iter()
                .filter_map(|name| name.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Write one item to `table` and read it back, which is what a client does with the id a
/// stack handed it.
async fn assert_round_trips(
    router: &MultiServiceRouter,
    case: &str,
    table: &str,
    key: Map<String, Value>,
    marker: &str,
) {
    let mut item = key.clone();
    item.insert("marker".to_string(), json!({"S": marker}));
    let (status, written) = dynamodb(
        router,
        "PutItem",
        &json!({"TableName": table, "Item": Value::Object(item.clone())}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the table the stack published ({table:?}) to take an item \
         write, got {status} with {written}"
    );
    let (status, read) = dynamodb(
        router,
        "GetItem",
        &json!({
            "TableName": table,
            "Key": Value::Object(key),
            "ConsistentRead": true,
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 reading the item back from {table:?}, got {status} \
         with {read}"
    );
    assert_eq!(
        read.get("Item"),
        Some(&Value::Object(item)),
        "case {case:?}: expected {table:?} to round-trip the item written to it, got {read}"
    );
}

/// The stack's own answers about a completed create: the `DescribeStacks` body and the
/// logical-id -> physical-id map `DescribeStackResources` reports.
async fn completed_stack(router: &MultiServiceRouter, case: &str, stack: &str) -> (String, String) {
    let (stack_status, described) = wait_for_stack_status(router, stack, CREATE_COMPLETE).await;
    let reasons = if stack_status == CREATE_COMPLETE {
        Vec::new()
    } else {
        let (_, events) =
            cloudformation(router, "DescribeStackEvents", &[("StackName", stack)]).await;
        elements(&events, "ResourceStatusReason")
    };
    assert_eq!(
        stack_status, CREATE_COMPLETE,
        "case {case:?}: expected {stack:?} to reach {CREATE_COMPLETE}, got {described} with \
         resource failures {reasons:?}"
    );
    let (status, resources) =
        cloudformation(router, "DescribeStackResources", &[("StackName", stack)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeStackResources, got {status} with \
         {resources}"
    );
    (described, resources)
}

/// Deploy `template` as `stack` and answer once CloudFormation reports it created.
async fn create_stack(router: &MultiServiceRouter, case: &str, stack: &str, body: &str) {
    let (status, answered) = cloudformation(
        router,
        "CreateStack",
        &[("StackName", stack), ("TemplateBody", body)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack for {stack:?}, got {status} with \
         body {answered}"
    );
}

/// Every row: the stack completes, and the id it publishes for each of its tables is a table
/// DynamoDB holds — reported `ACTIVE` with the declaration in force, named by that service's
/// own `ListTables`, and serving an item round trip.
#[tokio::test]
async fn an_unnamed_table_is_created_in_dynamodb_under_the_id_the_stack_publishes() {
    for (index, row) in rows().into_iter().enumerate() {
        let router = build_router();
        let stack = format!("cfn-ddb-generated-{index}");

        create_stack(&router, row.case, &stack, &template(&row.tables)).await;
        let (described, resources) = completed_stack(&router, row.case, &stack).await;
        let reported = physical_ids_of(&resources);
        let outputs = outputs_of(&described);
        let held = listed_tables(&router, row.case).await;

        let mut published = Vec::new();
        for table in &row.tables {
            let case = format!("{} / {}", row.case, table.logical_id);
            let physical_id = reported
                .get(table.logical_id)
                .cloned()
                .unwrap_or_default();
            assert!(
                !physical_id.is_empty(),
                "case {case:?}: expected a physical id for the table, got {resources}"
            );
            match table.declared_name {
                Some(name) => assert_eq!(
                    physical_id, name,
                    "case {case:?}: a declared name is the caller's, expected {name:?}, got \
                     {physical_id:?}"
                ),
                None => assert_generated_shape(&case, &stack, table.logical_id, &physical_id),
            }
            assert_within_dynamodb_name_bounds(&case, &physical_id);

            // Every handle the stack publishes the id through names the same table.
            let referenced = outputs
                .get(&ref_output_key(table.logical_id))
                .cloned()
                .unwrap_or_default();
            assert_eq!(
                referenced, physical_id,
                "case {case:?}: expected Ref to resolve to the reported physical id \
                 {physical_id:?}, got {referenced:?}"
            );

            // The OWNING service's own reads.
            let (status, answered) = described_table(&router, &physical_id).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected DynamoDB to resolve the id the completed stack \
                 published ({physical_id:?}), got {status} with {answered}"
            );
            assert_eq!(
                answered.pointer("/Table/TableStatus"),
                Some(&json!(ACTIVE)),
                "case {case:?}: expected the published table to be {ACTIVE} the instant the \
                 stack reported {CREATE_COMPLETE}, got {answered}"
            );
            assert_eq!(
                answered.pointer("/Table/TableName"),
                Some(&json!(physical_id)),
                "case {case:?}: expected the table to report the published id as its name, got \
                 {answered}"
            );
            assert_eq!(
                answered.pointer("/Table/TableArn"),
                Some(&json!(format!(
                    "arn:aws:dynamodb:{REGION}:{}:table/{physical_id}",
                    engine::emulator_account_id()
                ))),
                "case {case:?}: expected the table's ARN to carry the published id, got \
                 {answered}"
            );
            for (pointer, expected) in &table.in_force {
                assert_eq!(
                    answered.pointer(&format!("/Table{pointer}")),
                    Some(expected),
                    "case {case:?}: expected the settled table to report {pointer} as \
                     {expected}, got {answered}"
                );
            }
            if table.properties.get("StreamSpecification").is_some() {
                let stream_arn = answered
                    .pointer("/Table/LatestStreamArn")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                assert!(
                    stream_arn.contains(&physical_id) && stream_arn.contains("/stream/"),
                    "case {case:?}: expected the stream ARN DynamoDB minted to name the \
                     published table, got {stream_arn:?}"
                );
            }
            assert!(
                held.contains(&physical_id),
                "case {case:?}: expected DynamoDB's own ListTables to name {physical_id:?}, \
                 got {held:?}"
            );

            assert_round_trips(
                &router,
                &case,
                &physical_id,
                table.item_key(),
                table.logical_id,
            )
            .await;
            published.push(physical_id);
        }

        let distinct: BTreeSet<&String> = published.iter().collect();
        assert_eq!(
            distinct.len(),
            published.len(),
            "case {:?}: expected one id per declared table, got {published:?}",
            row.case
        );
    }
}

/// Where an importing stack puts the value it imported.
enum Consumed {
    /// In an `AWS::SSM::Parameter` of its own — the recorded topology.
    InAnSsmParameter,
    /// In an output of its own.
    InAnOutput,
}

/// One row of the cross-stack table: the producer's unnamed table, the name it exports the
/// table's `Ref` under, and where the consumer puts the import.
struct ExportRow {
    case: &'static str,
    table: fn() -> DeclaredTable,
    /// Built from the producer's stack name, as a CDK export is.
    export_name: fn(&str) -> String,
    consumed: Consumed,
}

fn rows_with_an_importing_stack() -> Vec<ExportRow> {
    vec![
        ExportRow {
            // The recorded topology: the CDK-shaped export name, imported into a parameter.
            case: "an export imported into an SSM parameter",
            table: || DeclaredTable::keyed("Table", "pk").with(on_demand()),
            export_name: |producer| format!("{producer}:ExportsOutputRefTable"),
            consumed: Consumed::InAnSsmParameter,
        },
        ExportRow {
            case: "a plainly named export imported into the consumer's own output",
            table: || {
                DeclaredTable {
                    sort_key: Some("sk"),
                    ..DeclaredTable::keyed("SharedTable", "pk")
                }
                .with(provisioned(2, 2))
            },
            export_name: |producer| format!("{producer}-table-name"),
            consumed: Consumed::InAnOutput,
        },
    ]
}

/// The producer's template: the unnamed table, with its `Ref` exported under `export_name`.
fn producer_template(table: &DeclaredTable, export_name: &str) -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            table.logical_id: {"Type": TABLE_TYPE, "Properties": table.declaration()},
        },
        "Outputs": {
            "ExportedTableName": {
                "Value": {"Ref": table.logical_id},
                "Export": {"Name": export_name},
            },
        },
    })
    .to_string()
}

/// The consumer's template: a parameter holding the import, or one holding a literal with
/// the import echoed as the consumer's own output.
fn consumer_template(consumed: &Consumed, export_name: &str, parameter_name: &str) -> String {
    let (value, outputs) = match consumed {
        Consumed::InAnSsmParameter => (json!({"Fn::ImportValue": export_name}), json!({})),
        Consumed::InAnOutput => (
            json!("not-the-import"),
            json!({"Echoed": {"Value": {"Fn::ImportValue": export_name}}}),
        ),
    };
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "TableNameParameter": {
                "Type": "AWS::SSM::Parameter",
                "Properties": {"Name": parameter_name, "Type": "String", "Value": value},
            },
        },
        "Outputs": outputs,
    })
    .to_string()
}

/// Every row: the id the producer exported is the id another stack imports, and the table
/// that id names is the one table both stacks are talking about — an item written through
/// the producer's handle is read back through the consumer's.
#[tokio::test]
async fn an_imported_export_of_a_generated_table_name_names_the_same_table() {
    for (index, row) in rows_with_an_importing_stack().into_iter().enumerate() {
        let router = build_router();
        let case = row.case;
        let producer = format!("cfn-ddb-producer-{index}");
        let consumer = format!("cfn-ddb-consumer-{index}");
        let export_name = (row.export_name)(&producer);
        let parameter_name = format!("/cfn-ddb/{index}/table-name");
        let table = (row.table)();

        create_stack(
            &router,
            case,
            &producer,
            &producer_template(&table, &export_name),
        )
        .await;
        let (_, resources) = completed_stack(&router, case, &producer).await;
        let physical_id = physical_ids_of(&resources)
            .get(table.logical_id)
            .cloned()
            .unwrap_or_default();
        assert!(
            !physical_id.is_empty(),
            "case {case:?}: expected a physical id for the producer's table, got {resources}"
        );
        assert_generated_shape(case, &producer, table.logical_id, &physical_id);

        // The export carries that id, and nothing else.
        let (status, exports) = cloudformation(&router, "ListExports", &[]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from ListExports, got {status} with {exports}"
        );
        let exported = pairs_of(&exports, "Name", "Value")
            .get(&export_name)
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            exported, physical_id,
            "case {case:?}: expected the export {export_name:?} to carry the table's physical \
             id {physical_id:?}, got {exported:?}"
        );

        // The consuming stack.
        create_stack(
            &router,
            case,
            &consumer,
            &consumer_template(&row.consumed, &export_name, &parameter_name),
        )
        .await;
        let (consumer_described, _) = completed_stack(&router, case, &consumer).await;

        let (status, listed) =
            cloudformation(&router, "ListImports", &[("ExportName", &export_name)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from ListImports, got {status} with {listed}"
        );
        assert!(
            elements(&listed, "member").contains(&consumer),
            "case {case:?}: expected ListImports on {export_name:?} to name {consumer:?}, got \
             {listed}"
        );

        // The value the consumer holds, read where that row put it.
        let held = match row.consumed {
            Consumed::InAnSsmParameter => {
                let (status, read) = ssm(&router, "GetParameter", &json!({"Name": parameter_name}))
                    .await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: expected HTTP 200 from GetParameter for \
                     {parameter_name:?}, got {status} with {read}"
                );
                read.pointer("/Parameter/Value")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string()
            }
            Consumed::InAnOutput => outputs_of(&consumer_described)
                .get("Echoed")
                .cloned()
                .unwrap_or_default(),
        };
        assert_eq!(
            held, physical_id,
            "case {case:?}: expected the consumer to hold the exported table name \
             {physical_id:?}, got {held:?}"
        );

        // The id the consumer holds resolves in DynamoDB, and names the producer's table.
        let (status, answered) = described_table(&router, &held).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected DynamoDB to resolve the imported table name {held:?}, got \
             {status} with {answered}"
        );
        assert_eq!(
            answered.pointer("/Table/TableStatus"),
            Some(&json!(ACTIVE)),
            "case {case:?}: expected the imported table name to report {ACTIVE}, got {answered}"
        );
        assert_round_trips(&router, case, &physical_id, table.item_key(), case).await;
        let (status, read) = dynamodb(
            &router,
            "GetItem",
            &json!({
                "TableName": held,
                "Key": Value::Object(table.item_key()),
                "ConsistentRead": true,
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 reading through the imported name {held:?}, got \
             {status} with {read}"
        );
        assert_eq!(
            read.pointer("/Item/marker"),
            Some(&json!({"S": case})),
            "case {case:?}: expected the imported name to reach the table the producer wrote \
             to, got {read}"
        );
    }
}
