// SPDX-License-Identifier: CC0-1.0

//! An `AWS::DynamoDB::Table` whose name a template COMPUTES from the stack's parameters is a
//! real table in Amazon DynamoDB, under the very name the completed stack hands its clients.
//!
//! The rule under test: **a resource CloudFormation reports created exists in the service that
//! OWNS it, under the physical resource id the stack publishes — whatever expression the
//! template computed that resource's declared name from.** For an `AWS::DynamoDB::Table` whose
//! `TableName` is resolved from a stack parameter (by `Ref`, by `Fn::Sub`, by `Fn::Join`, in
//! either of the two spellings a template is written in, and whether the value came from the
//! create call or from the declaration's own `Default`), the resolved name is the name the
//! table was CREATED under, so DynamoDB's own `DescribeTable` answers 200 for the name the
//! stack's own outputs echo, reporting the declared `KeySchema` and `TableStatus` `ACTIVE`,
//! its `ListTables` names it, and its data plane serves it. That service's not-found refusal
//! (`ResourceNotFoundException`, HTTP 400, "Requested resource not found: Table: `<name>` not
//! found") is reserved for a name no table was ever created under: a resource
//! CloudFormation says it created and a resource DynamoDB says it never had are not both true
//! of one name.
//!
//! Two crates have to agree for that to hold — CloudFormation resolves the name and makes the
//! create, DynamoDB holds the table and decides its status — so the suite lives here rather
//! than in either crate: a CloudFormation-only test can see the output and the stack status
//! but not whether a table exists behind them.
//!
//! Why it matters: taking a resource's name from a parameter and echoing it in an output is
//! how a template hands a deployment's tables to the code that uses them, and that output is
//! the only handle those clients have. If the name it carries resolves nowhere, a stack
//! reporting success has delivered nothing usable, and the success status cannot be told from
//! a table that was never created.
//!
//! The rows of [`rows`] differ along what the rule generalises over — how the declared name is
//! computed from the parameters (`Ref` of a passed value, `Ref` of a defaulted value,
//! `Fn::Sub`, `Fn::Join`), the template spelling (JSON, YAML with its short-form tags), the
//! key schema (one partition key, a partition key with a sort key), the read/write mode
//! (on-demand, provisioned) and how many tables one parameter names. The negative half is
//! checked on every row, over a name derived from the same parameter that the template never
//! declared.
//!
//! The awsQuery caller, the poll that waits for a stack to first report a status, the awsJson
//! caller for DynamoDB and the XML readers are each written ONCE in this test crate and
//! shared, so this suite adds no wire caller of its own.

use std::sync::Arc;

use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_apigatewayv2_resources::{cloudformation, REGION};
use crate::cloudformation_dynamodb_table_in_place_update::{described_table, dynamodb};
use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::cloudformation_provisions_glue_jobs::error_code;
use crate::query_wire::{elements, outputs_of, physical_ids_of};

/// The `StackStatus` a completed create reports.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The `TableStatus` a table ready for use reports: "`ACTIVE` - The table is ready for use"
/// (`TableDescription$TableStatus`).
const ACTIVE: &str = "ACTIVE";

/// The resource type every row declares.
const TABLE_TYPE: &str = "AWS::DynamoDB::Table";

/// DynamoDB's refusal of a read addressing a table it does not hold, and the status it
/// answers it on.
///
/// Source: `ResourceNotFoundException` in DynamoDB's model
/// (`smithy_query(query="operation",operation="DescribeTable")`), a client error at
/// `@httpError(400)`.
const NOT_FOUND: &str = "ResourceNotFoundException";
const NOT_FOUND_STATUS: u16 = 400;

/// The name a client passes for its table, and the prefix the rows that COMPOSE a name build
/// theirs from.
const PASSED_NAME: &str = "cfn-parameterised-items";
const DEFAULTED_NAME: &str = "cfn-parameterised-defaulted";
const PREFIX: &str = "cfn-parameterised-estate";

/// AWS's sentence for a table the endpoint holds none of, over the name the request asked
/// for: the fixed text with the requested name in it, and nothing else.
fn not_found_message(name: &str) -> String {
    format!("Requested resource not found: Table: {name} not found")
}

/// The emulator with the two services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them: CloudFormation resolves the name and makes the
/// create, DynamoDB holds the table and answers for it.
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
        service_com_amazonaws_dynamodb_dynamodb_20120810::EmulatedService::new(engine),
    );
    router
}

/// The `message` an awsJson refusal carries, under either of the casings a client reads it by.
fn refusal_message(body: &Value) -> String {
    body.get("message")
        .or_else(|| body.get("Message"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The `KeySchema` a declaration of `keys` — `(attribute, key type)` in the order the table
/// declares them — carries.
fn key_schema(keys: &[(&str, &str)]) -> Value {
    Value::Array(
        keys.iter()
            .map(|(name, key_type)| json!({"AttributeName": name, "KeyType": key_type}))
            .collect(),
    )
}

/// The `AttributeDefinitions` those keys need: a key attribute has to be defined.
fn attribute_definitions(keys: &[(&str, &str)]) -> Value {
    Value::Array(
        keys.iter()
            .map(|(name, _)| json!({"AttributeName": name, "AttributeType": "S"}))
            .collect(),
    )
}

/// One item key over those attributes, which is what a client writes with the name the stack
/// handed it.
fn item_key(keys: &[(&str, &str)]) -> Value {
    Value::Object(
        keys.iter()
            .map(|(name, _)| ((*name).to_string(), json!({"S": format!("{name}-value")})))
            .collect(),
    )
}

/// A table resource's `Properties`: the expression its `TableName` is declared as, the key
/// schema and attribute definitions for `keys`, and whatever `extra` states beside them.
fn properties(table_name: Value, keys: &[(&str, &str)], extra: Value) -> Value {
    let mut declared = Map::new();
    declared.insert("TableName".to_string(), table_name);
    declared.insert("KeySchema".to_string(), key_schema(keys));
    declared.insert(
        "AttributeDefinitions".to_string(),
        attribute_definitions(keys),
    );
    for (member, value) in extra.as_object().cloned().unwrap_or_default() {
        declared.insert(member, value);
    }
    Value::Object(declared)
}

/// The on-demand read/write mode, so no throughput has to be declared beside it.
fn on_demand() -> Value {
    json!({"BillingMode": "PAY_PER_REQUEST"})
}

/// A JSON template: the `Parameters` section as given, one `AWS::DynamoDB::Table` per entry of
/// `tables` (logical id -> properties) with an output publishing its `Ref` under
/// `<logical id>Name`, and one output per entry of `echoes` (output key -> the expression it
/// publishes).
fn json_template(
    parameters: Value,
    tables: Vec<(&str, Value)>,
    echoes: Vec<(&str, Value)>,
) -> String {
    let mut resources = Map::new();
    let mut outputs = Map::new();
    for (logical_id, declared) in tables {
        resources.insert(
            logical_id.to_string(),
            json!({"Type": TABLE_TYPE, "Properties": declared}),
        );
        outputs.insert(
            name_output_key(logical_id),
            json!({"Value": {"Ref": logical_id}}),
        );
    }
    for (key, value) in echoes {
        outputs.insert(key.to_string(), json!({"Value": value}));
    }
    json!({
        "Parameters": parameters,
        "Resources": Value::Object(resources),
        "Outputs": Value::Object(outputs),
    })
    .to_string()
}

/// The output every row publishes a table's own `Ref` under.
fn name_output_key(logical_id: &str) -> String {
    format!("{logical_id}Name")
}

/// One table a row declares, and what the row expects of it.
struct ExpectedTable {
    /// The logical id the template declares it under.
    logical_id: &'static str,
    /// The name the declaration's `TableName` expression resolves to.
    name: String,
    /// The keys it is declared with, which is both what `DescribeTable` has to report and
    /// what an item written to it is keyed by.
    keys: &'static [(&'static str, &'static str)],
}

/// One row: a template whose tables are named off its parameters, the parameters the create
/// passes, and what the completed stack has to have created.
struct Row {
    case: &'static str,
    template: String,
    /// `(ParameterKey, ParameterValue)` pairs the create carries, which may be empty where
    /// the declaration carries its own `Default`.
    parameters: Vec<(&'static str, String)>,
    tables: Vec<ExpectedTable>,
    /// Outputs that echo a PARAMETER rather than a resource, with the value expected of each:
    /// the handle a client reads the table's name off.
    echoes: Vec<(&'static str, String)>,
}

/// The rows, differing along how the declared name is computed, the template spelling, the key
/// schema, the read/write mode, and how many tables one parameter names.
fn rows() -> Vec<Row> {
    const PARTITION_ONLY: &[(&str, &str)] = &[("pk", "HASH")];
    const COMPOSITE: &[(&str, &str)] = &[("pk", "HASH"), ("sk", "RANGE")];
    const NAME_PARAMETER: &str = "TableNameParameter";
    const PREFIX_PARAMETER: &str = "Prefix";
    /// The output name a template echoes its table-name parameter under.
    const ECHO_OUTPUT: &str = "OldTableName";

    vec![
        Row {
            // The recorded shape: the name is a parameter the create passes, and an output
            // echoes that parameter straight back.
            case: "a TableName that is Ref of a parameter the create passes",
            template: json_template(
                json!({NAME_PARAMETER: {"Type": "String"}}),
                vec![(
                    "Items",
                    properties(json!({"Ref": NAME_PARAMETER}), PARTITION_ONLY, on_demand()),
                )],
                vec![(ECHO_OUTPUT, json!({"Ref": NAME_PARAMETER}))],
            ),
            parameters: vec![(NAME_PARAMETER, PASSED_NAME.to_string())],
            tables: vec![ExpectedTable {
                logical_id: "Items",
                name: PASSED_NAME.to_string(),
                keys: PARTITION_ONLY,
            }],
            echoes: vec![(ECHO_OUTPUT, PASSED_NAME.to_string())],
        },
        Row {
            // The value comes from the declaration itself rather than from the call.
            case: "a TableName that is Ref of a parameter left at its Default",
            template: json_template(
                json!({NAME_PARAMETER: {"Type": "String", "Default": DEFAULTED_NAME}}),
                vec![(
                    "Items",
                    properties(json!({"Ref": NAME_PARAMETER}), PARTITION_ONLY, on_demand()),
                )],
                vec![(ECHO_OUTPUT, json!({"Ref": NAME_PARAMETER}))],
            ),
            parameters: Vec::new(),
            tables: vec![ExpectedTable {
                logical_id: "Items",
                name: DEFAULTED_NAME.to_string(),
                keys: PARTITION_ONLY,
            }],
            echoes: vec![(ECHO_OUTPUT, DEFAULTED_NAME.to_string())],
        },
        Row {
            // The name is COMPOSED from the parameter rather than being it.
            case: "a TableName composed with Fn::Sub from a parameter",
            template: json_template(
                json!({PREFIX_PARAMETER: {"Type": "String"}}),
                vec![(
                    "Items",
                    properties(
                        json!({"Fn::Sub": format!("${{{PREFIX_PARAMETER}}}-items")}),
                        PARTITION_ONLY,
                        on_demand(),
                    ),
                )],
                vec![(ECHO_OUTPUT, json!({"Ref": PREFIX_PARAMETER}))],
            ),
            parameters: vec![(PREFIX_PARAMETER, PREFIX.to_string())],
            tables: vec![ExpectedTable {
                logical_id: "Items",
                name: format!("{PREFIX}-items"),
                keys: PARTITION_ONLY,
            }],
            echoes: vec![(ECHO_OUTPUT, PREFIX.to_string())],
        },
        Row {
            // A composed name over a composite key, provisioned rather than on-demand.
            case: "a TableName composed with Fn::Join over a composite key",
            template: json_template(
                json!({PREFIX_PARAMETER: {"Type": "String"}}),
                vec![(
                    "Events",
                    properties(
                        json!({"Fn::Join": ["-", [{"Ref": PREFIX_PARAMETER}, "events"]]}),
                        COMPOSITE,
                        json!({"ProvisionedThroughput": {
                            "ReadCapacityUnits": 2,
                            "WriteCapacityUnits": 3,
                        }}),
                    ),
                )],
                vec![(ECHO_OUTPUT, json!({"Ref": PREFIX_PARAMETER}))],
            ),
            parameters: vec![(PREFIX_PARAMETER, PREFIX.to_string())],
            tables: vec![ExpectedTable {
                logical_id: "Events",
                name: format!("{PREFIX}-events"),
                keys: COMPOSITE,
            }],
            echoes: vec![(ECHO_OUTPUT, PREFIX.to_string())],
        },
        Row {
            // One parameter names TWO tables, each of which has to exist under its own name.
            case: "two tables named off one parameter",
            template: json_template(
                json!({PREFIX_PARAMETER: {"Type": "String"}}),
                vec![
                    (
                        "Items",
                        properties(
                            json!({"Fn::Sub": format!("${{{PREFIX_PARAMETER}}}-pair-items")}),
                            PARTITION_ONLY,
                            on_demand(),
                        ),
                    ),
                    (
                        "Events",
                        properties(
                            json!({"Fn::Sub": format!("${{{PREFIX_PARAMETER}}}-pair-events")}),
                            COMPOSITE,
                            on_demand(),
                        ),
                    ),
                ],
                vec![(ECHO_OUTPUT, json!({"Ref": PREFIX_PARAMETER}))],
            ),
            parameters: vec![(PREFIX_PARAMETER, PREFIX.to_string())],
            tables: vec![
                ExpectedTable {
                    logical_id: "Items",
                    name: format!("{PREFIX}-pair-items"),
                    keys: PARTITION_ONLY,
                },
                ExpectedTable {
                    logical_id: "Events",
                    name: format!("{PREFIX}-pair-events"),
                    keys: COMPOSITE,
                },
            ],
            echoes: vec![(ECHO_OUTPUT, PREFIX.to_string())],
        },
        Row {
            // The other spelling a template is written in, where the reference is a
            // short-form tag rather than an object member.
            case: "a YAML template whose TableName is a short-form !Ref",
            template: format!(
                "Parameters:\n  {NAME_PARAMETER}:\n    Type: String\n\
                 Resources:\n  Items:\n    Type: {TABLE_TYPE}\n    Properties:\n      \
                 TableName: !Ref {NAME_PARAMETER}\n      BillingMode: PAY_PER_REQUEST\n      \
                 KeySchema:\n        - AttributeName: pk\n          KeyType: HASH\n      \
                 AttributeDefinitions:\n        - AttributeName: pk\n          \
                 AttributeType: S\n\
                 Outputs:\n  ItemsName:\n    Value: !Ref Items\n  {ECHO_OUTPUT}:\n    \
                 Value: !Ref {NAME_PARAMETER}\n"
            ),
            parameters: vec![(NAME_PARAMETER, PASSED_NAME.to_string())],
            tables: vec![ExpectedTable {
                logical_id: "Items",
                name: PASSED_NAME.to_string(),
                keys: PARTITION_ONLY,
            }],
            echoes: vec![(ECHO_OUTPUT, PASSED_NAME.to_string())],
        },
    ]
}

/// Deploy one row's template, passing its parameters the way a client does.
async fn create_stack(router: &MultiServiceRouter, row: &Row, stack: &str) {
    let mut owned: Vec<(String, String)> = vec![
        ("StackName".to_string(), stack.to_string()),
        ("TemplateBody".to_string(), row.template.clone()),
    ];
    for (position, (key, value)) in row.parameters.iter().enumerate() {
        let index = position + 1;
        owned.push((
            format!("Parameters.member.{index}.ParameterKey"),
            (*key).to_string(),
        ));
        owned.push((
            format!("Parameters.member.{index}.ParameterValue"),
            value.clone(),
        ));
    }
    let members: Vec<(&str, &str)> = owned
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    let (status, answered) = cloudformation(router, "CreateStack", &members).await;
    assert_eq!(
        status, 200,
        "case {:?}: expected HTTP 200 from CreateStack for {stack:?}, got {status} with \
         {answered}",
        row.case
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

/// Write one item to `table` and read it back, which is what a client does with the name the
/// stack handed it.
async fn assert_round_trips(
    router: &MultiServiceRouter,
    case: &str,
    table: &str,
    keys: &[(&str, &str)],
) {
    let key = item_key(keys);
    let mut item = key.as_object().cloned().unwrap_or_default();
    item.insert("marker".to_string(), json!({"S": table}));
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
        &json!({"TableName": table, "Key": key, "ConsistentRead": true}),
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

/// A name derived from the same parameter that the template never declared: the only thing
/// DynamoDB's not-found refusal is for.
fn never_declared(name: &str) -> String {
    format!("{name}-never-declared")
}

/// Every row: the stack completes, the name its outputs echo is the id it published for the
/// table, and that name is a table DynamoDB holds — `ACTIVE`, reporting the declared key
/// schema, named by `ListTables` and serving an item round trip — while a name the template
/// never declared is refused `ResourceNotFoundException` with that service's own wording.
#[tokio::test]
async fn a_table_named_from_a_stack_parameter_resolves_in_dynamodb() {
    for (index, row) in rows().into_iter().enumerate() {
        let router = build_router();
        let stack = format!("cfn-ddb-parameterised-{index}");

        create_stack(&router, &row, &stack).await;
        let (stack_status, described) =
            wait_for_stack_status(&router, &stack, CREATE_COMPLETE).await;
        if stack_status != CREATE_COMPLETE {
            let (_, events) = cloudformation(
                &router,
                "DescribeStackEvents",
                &[("StackName", stack.as_str())],
            )
            .await;
            panic!(
                "case {:?}: expected {stack:?} to reach {CREATE_COMPLETE}, got {stack_status} \
                 with {described} and resource failures {:?}",
                row.case,
                elements(&events, "ResourceStatusReason")
            );
        }
        let outputs = outputs_of(&described);
        let (status, resources) = cloudformation(
            &router,
            "DescribeStackResources",
            &[("StackName", stack.as_str())],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: expected HTTP 200 from DescribeStackResources, got {status} with \
             {resources}",
            row.case
        );
        let reported = physical_ids_of(&resources);
        let held = listed_tables(&router, row.case).await;

        // The handle the issue's clients read: the output echoing the parameter they passed.
        for (key, expected) in &row.echoes {
            assert_eq!(
                outputs.get(*key),
                Some(expected),
                "case {:?}: expected output {key:?} to echo {expected:?}, got {outputs:?}",
                row.case
            );
        }

        for table in &row.tables {
            let case = format!("{} / {}", row.case, table.logical_id);
            let published = outputs
                .get(&name_output_key(table.logical_id))
                .cloned()
                .unwrap_or_default();
            assert_eq!(
                published, table.name,
                "case {case:?}: expected the stack's output to publish the resolved name \
                 {:?}, got {published:?}",
                table.name
            );
            let physical_id = reported.get(table.logical_id).cloned().unwrap_or_default();
            assert_eq!(
                physical_id, table.name,
                "case {case:?}: expected the physical resource id to be the resolved name \
                 {:?}, got {physical_id:?}",
                table.name
            );

            // The OWNING service's own reads, on the name the stack published.
            let (status, answered) = described_table(&router, &published).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected DynamoDB to resolve the name the completed stack \
                 published ({published:?}), got {status} with {answered}"
            );
            assert_eq!(
                answered.pointer("/Table/TableName"),
                Some(&json!(published)),
                "case {case:?}: expected the table to report the published name as its own, \
                 got {answered}"
            );
            assert_eq!(
                answered.pointer("/Table/TableStatus"),
                Some(&json!(ACTIVE)),
                "case {case:?}: expected the published table to be {ACTIVE} the instant the \
                 stack reported {CREATE_COMPLETE}, got {answered}"
            );
            assert_eq!(
                answered.pointer("/Table/KeySchema"),
                Some(&key_schema(table.keys)),
                "case {case:?}: expected the table to report the declared key schema {}, got \
                 {answered}",
                key_schema(table.keys)
            );
            assert_eq!(
                answered.pointer("/Table/TableArn"),
                Some(&json!(format!(
                    "arn:aws:dynamodb:{REGION}:{}:table/{published}",
                    engine::emulator_account_id()
                ))),
                "case {case:?}: expected the table's ARN to carry the published name, got \
                 {answered}"
            );
            assert!(
                held.contains(&published),
                "case {case:?}: expected DynamoDB's own ListTables to name {published:?}, got \
                 {held:?}"
            );
            assert_round_trips(&router, &case, &published, table.keys).await;

            // The refusal is for a name nothing created, and names that name.
            let absent = never_declared(&table.name);
            let (status, refused) = described_table(&router, &absent).await;
            assert_eq!(
                (status, error_code(&refused).as_str()),
                (NOT_FOUND_STATUS, NOT_FOUND),
                "case {case:?}: expected {NOT_FOUND_STATUS} {NOT_FOUND} for {absent:?}, got \
                 {status} with {refused}"
            );
            assert_eq!(
                refusal_message(&refused),
                not_found_message(&absent),
                "case {case:?}: expected the refusal to name {absent:?}, got {refused}"
            );
            assert!(
                !held.contains(&absent),
                "case {case:?}: expected ListTables not to name {absent:?}, got {held:?}"
            );
        }
    }
}
