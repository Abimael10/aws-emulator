// SPDX-License-Identifier: CC0-1.0

//! The stream-processing estate a template declares — an Amazon DynamoDB table with a stream,
//! the execution role that is allowed to read it, the function that consumes it and the AWS
//! Lambda event source mapping that wires the two together — deployed as ONE CloudFormation
//! stack.
//!
//! The rule under test: **a create whose declared resources the services that own them ACCEPT
//! reaches `CREATE_COMPLETE` with every declared resource `CREATE_COMPLETE`, and publishes
//! every declared output computed from what those services minted — `Ref`, each `Fn::GetAtt`
//! attribute the type publishes, and an `Fn::Sub` over one of them — so each identifier the
//! stack publishes is the identifier the owning service answers to; a stack reaches
//! `ROLLBACK_COMPLETE` only after a resource create really failed, and the reason is then
//! carried by `DescribeStackEvents`' `ResourceStatusReason`.**
//!
//! Six crates have to agree for that to hold — CloudFormation drives the deployment, DynamoDB
//! mints the table and its stream, DynamoDB Streams serves that stream to the poller, IAM
//! holds the execution role the poller's reads are judged against, Lambda holds the function
//! and the mapping and RUNS the handler, and CloudWatch Logs takes what the run printed — so
//! no single-service suite can ask it. What the neighbours pin and this one does not repeat:
//! `all/ddb_stream_lambda_esm_provisioning` pins that a stream-enabled table's stream is
//! mappable at all through each service's OWN API, and
//! `all/cloudformation_provisions_lambda_event_source_mappings` pins the mapping a stack
//! declares against the sources that are not a table's stream. What is pinned HERE is the
//! whole declared estate: the stack completing, the stream ARN it exports being the live one,
//! the `Fn::Sub` over the table's ARN reaching IAM, and a write to the deployed table really
//! reaching the deployed function.
//!
//! The rows differ along what the rule generalises over: every `StreamViewType` the model
//! declares (read off the list the DynamoDB Streams crate derives from its model, so the rows
//! cannot drift from the enum), crossed with both billing modes, with tables with and without
//! a sort key, and with the batch size and the `Enabled` flag the mapping is declared with.
//! None of those may change whether the estate stands up or whether its trigger fires.
//!
//! Every expected value is computed here from what the row declares or from the stack's own
//! outputs, except the three identifiers the owning services mint — the table's stream ARN,
//! the role's generated name and the mapping's UUID — which are checked by being resolved in
//! the service that minted them.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_dynamodbstreams_dynamodbstreams_20120810::ops::types::STREAM_VIEW_TYPE_VALUES;
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_apigatewayv2_resources::cloudformation;
use crate::cloudformation_dynamodb_table_in_place_update::{described_table, dynamodb};
use crate::cloudformation_firehose_stream_stabilisation::stack_status;
use crate::cloudformation_provisions_lambda_event_source_mappings::{
    is_mapping_uuid, mapping_until_settled, MAPPING_STATE_ENABLED,
};
use crate::cloudwatch_alarm_action_reaches_its_subscribed_queue::router as emulator;
use crate::logs_subscription_filter_to_lambda::{iam, logs, LAMBDA_LOG_GROUP_PREFIX};
use crate::query_wire::{elements, outputs_of, pairs_of, physical_ids_of};

/// The `StackStatus` a create reports while it runs, and the one a completed create reports.
const CREATE_IN_PROGRESS: &str = "CREATE_IN_PROGRESS";
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The `ResourceStatus` every resource of a completed create reports.
const RESOURCE_CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The logical ids every row's template declares its four resources under.
const TABLE: &str = "Table";
const ROLE: &str = "ExecRole";
const FUNCTION: &str = "Consumer";
const MAPPING: &str = "StreamToConsumer";

/// The keys every row's template publishes its identifiers under.
const TABLE_NAME_OUTPUT: &str = "TableName";
const TABLE_ARN_OUTPUT: &str = "TableArn";
const STREAM_ARN_OUTPUT: &str = "StreamArn";
const FUNCTION_NAME_OUTPUT: &str = "FunctionName";
const FUNCTION_ARN_OUTPUT: &str = "FunctionArn";
const MAPPING_ID_OUTPUT: &str = "EventSourceMappingId";

/// The attributes every row's table is keyed by: the partition key every row declares, and the
/// sort key the rows that have one declare beside it.
const PARTITION_KEY: &str = "pk";
const SORT_KEY: &str = "sk";

/// The two values the model's `BillingMode` enum declares
/// (`CreateTable$BillingMode`: "one of: PAY_PER_REQUEST | PROVISIONED").
const BILLING_MODES: [&str; 2] = ["PAY_PER_REQUEST", "PROVISIONED"];

/// The AWS-managed policy every Lambda execution role is written with, which is what grants
/// the run the three log calls it writes its output through.
const BASIC_EXECUTION_POLICY: &str =
    "arn:aws:iam::aws:policy/service-role/AWSLambdaBasicExecutionRole";

/// The name the stream-read grant is embedded under, which is what `GetRolePolicy` reads it
/// back by.
const STREAM_POLICY_NAME: &str = "read-the-stream";

/// Where every mapping starts reading: the oldest record in the shard, so the write a row
/// makes is delivered whenever the poller first looks.
const STARTING_POSITION: &str = "TRIM_HORIZON";

/// How long one invocation is given, as the function's own `Timeout`: well past what a run
/// needs on any host, because what is under test is that the estate stands up and fires and
/// not how fast an interpreter gets through a handler.
const FUNCTION_TIMEOUT: u64 = 120;

/// The marker the handler prints for each record it is handed, which is what says the write
/// reached the function the stack deployed.
const RECORD_MARKER: &str = "STREAM_RECORD_ID";

/// How long this suite waits for work the clock owes — a stack's resources coming up, a
/// mapping's poller settling, the asynchronous delivery of a batch and the log line the run
/// leaves behind — and how long it hands the runtime back between two looks.
///
/// Generous slack over the spans the services here choose: a row fails only when the work did
/// not happen at all, so a loaded host cannot decide the outcome. The wait is on the CONDITION
/// every time, and all the deadline is for is that the wait TERMINATES.
const WAIT: Duration = Duration::from_secs(120);
/// See [`WAIT`].
const LOOK_AGAIN: Duration = Duration::from_millis(100);

/// The handler every row deploys: for each record it is handed it prints one marker carrying
/// the changed item's own key, taken out of the record itself — so a marker can only appear for
/// a record that really reached the function. The key is read off the record's `Keys`, which
/// every `StreamViewType` carries, so one handler serves every row.
const MARKING_HANDLER: &str = r#"
def handler(event, context):
    seen = 0
    for record in event["Records"]:
        print("STREAM_RECORD_ID " + record["dynamodb"]["Keys"]["pk"]["S"])
        seen = seen + 1
    return {"seen": seen}
"#;

/// One row of the table: the shape of the table the estate reads and of the mapping that reads
/// it.
struct Row {
    case: String,
    /// What the stack, the table and the function of this row are named off, so no row can read
    /// or write another's resources.
    slug: String,
    /// The `StreamViewType` the table's stream captures.
    view_type: &'static str,
    /// The `BillingMode` the table is declared in.
    billing_mode: &'static str,
    /// Whether the table is declared with a sort key beside its partition key.
    sort_key: bool,
    /// The `BatchSize` the mapping is declared with.
    batch_size: u64,
    /// The `Enabled` flag the mapping is declared with, or `None` for a declaration that states
    /// none at all (the recorded shape).
    enabled: Option<bool>,
}

impl Row {
    fn stack(&self) -> String {
        format!("xsvc-stream-estate-{}", self.slug)
    }

    fn table(&self) -> String {
        format!("xsvc-stream-source-{}", self.slug)
    }

    fn function(&self) -> String {
        format!("xsvc-stream-consumer-{}", self.slug)
    }

    /// The key this row writes, which the handler prints the marker for.
    fn written_key(&self) -> String {
        format!("record-{}", self.slug)
    }
}

/// Every row: one per `StreamViewType` the model declares, crossing the billing modes and the
/// sort-key, batch-size and `Enabled` variations over them, so each secondary dimension is
/// covered on a different stride rather than correlated with the view type.
fn rows() -> Vec<Row> {
    STREAM_VIEW_TYPE_VALUES
        .iter()
        .enumerate()
        .map(|(position, view_type)| {
            let view_type: &'static str = view_type;
            let billing_mode = BILLING_MODES[position % BILLING_MODES.len()];
            let sort_key = (position / BILLING_MODES.len()) % 2 == 1;
            // A declaration that states the flag and one that states none at all: both ask for
            // a mapping that polls, which is what the rows differ along here.
            let enabled = if position % 2 == 1 { Some(true) } else { None };
            Row {
                case: format!(
                    "{view_type} in {billing_mode} (sort key: {sort_key}, enabled: {enabled:?})"
                ),
                slug: view_type.to_ascii_lowercase().replace('_', "-"),
                view_type,
                billing_mode,
                sort_key,
                batch_size: u64::try_from(position + 1).unwrap_or(1),
                enabled,
            }
        })
        .collect()
}

/// The trust policy a Lambda execution role needs: Lambda has to be able to assume it for the
/// function to be handed it at all.
fn trust_policy() -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"Service": "lambda.amazonaws.com"},
            "Action": "sts:AssumeRole",
        }],
    })
}

/// The grant the role carries over the table's stream, stated the way a client writes it: the
/// three calls a shard source is consumed by, on the stream of the table the same template
/// declares — named through an `Fn::Sub` over that table's own `Arn` attribute, because no
/// naming rule computes the stream's ARN.
fn stream_grant() -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": [
                "dynamodb:DescribeStream",
                "dynamodb:GetShardIterator",
                "dynamodb:GetRecords",
                "dynamodb:ListStreams",
            ],
            "Resource": {"Fn::Sub": format!("${{{TABLE}.Arn}}/stream/*")},
        }],
    })
}

/// The template one row deploys: the four resources, and the outputs a client reads the
/// estate's identifiers out of.
fn template(row: &Row) -> String {
    let mut key_schema = vec![json!({"AttributeName": PARTITION_KEY, "KeyType": "HASH"})];
    let mut definitions = vec![json!({"AttributeName": PARTITION_KEY, "AttributeType": "S"})];
    if row.sort_key {
        key_schema.push(json!({"AttributeName": SORT_KEY, "KeyType": "RANGE"}));
        definitions.push(json!({"AttributeName": SORT_KEY, "AttributeType": "N"}));
    }
    let mut table = json!({
        "TableName": row.table(),
        "KeySchema": key_schema,
        "AttributeDefinitions": definitions,
        "BillingMode": row.billing_mode,
        // The type's own `StreamSpecification` states what the stream CAPTURES and nothing
        // else, so a declared view type is what asks for a stream.
        "StreamSpecification": {"StreamViewType": row.view_type},
    });
    if row.billing_mode == "PROVISIONED" {
        table["ProvisionedThroughput"] = json!({"ReadCapacityUnits": 1, "WriteCapacityUnits": 1});
    }
    let mut mapping = json!({
        "FunctionName": {"Ref": FUNCTION},
        "EventSourceArn": {"Fn::GetAtt": [TABLE, "StreamArn"]},
        "StartingPosition": STARTING_POSITION,
        "BatchSize": row.batch_size,
    });
    if let Some(enabled) = row.enabled {
        mapping["Enabled"] = json!(enabled);
    }
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            TABLE: {"Type": "AWS::DynamoDB::Table", "Properties": table},
            ROLE: {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "AssumeRolePolicyDocument": trust_policy(),
                    "ManagedPolicyArns": [BASIC_EXECUTION_POLICY],
                    "Policies": [{
                        "PolicyName": STREAM_POLICY_NAME,
                        "PolicyDocument": stream_grant(),
                    }],
                },
            },
            FUNCTION: {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": row.function(),
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "Role": {"Fn::GetAtt": [ROLE, "Arn"]},
                    "Timeout": FUNCTION_TIMEOUT,
                    "Code": {"ZipFile": MARKING_HANDLER},
                },
            },
            MAPPING: {"Type": "AWS::Lambda::EventSourceMapping", "Properties": mapping},
        },
        "Outputs": {
            TABLE_NAME_OUTPUT: {"Value": {"Ref": TABLE}},
            TABLE_ARN_OUTPUT: {"Value": {"Fn::GetAtt": [TABLE, "Arn"]}},
            STREAM_ARN_OUTPUT: {"Value": {"Fn::GetAtt": [TABLE, "StreamArn"]}},
            FUNCTION_NAME_OUTPUT: {"Value": {"Ref": FUNCTION}},
            FUNCTION_ARN_OUTPUT: {"Value": {"Fn::GetAtt": [FUNCTION, "Arn"]}},
            MAPPING_ID_OUTPUT: {"Value": {"Ref": MAPPING}},
        },
    })
    .to_string()
}

/// Poll `stack` until the create has finished, in whatever status it finished in: the
/// transitions on the way are owed on the stack's own clock, so this waits for the CONDITION
/// rather than for a span, and answers the last status with the answer it was read from.
async fn wait_past_the_create(router: &MultiServiceRouter, stack: &str) -> (String, String) {
    let giving_up_at = Instant::now() + WAIT;
    let mut last = stack_status(router, stack).await;
    while last.0 == CREATE_IN_PROGRESS && Instant::now() < giving_up_at {
        tokio::time::sleep(LOOK_AGAIN).await;
        last = stack_status(router, stack).await;
    }
    last
}

/// Every `ResourceStatusReason` the stack has recorded, so a deployment that did not reach the
/// status the rule states reports WHY rather than only that it did not.
async fn failure_reasons(router: &MultiServiceRouter, stack: &str) -> Vec<String> {
    let (_, events) =
        cloudformation(router, "DescribeStackEvents", &[("StackName", stack)]).await;
    elements(&events, "ResourceStatusReason")
}

/// One output of the completed stack.
fn output(case: &str, described: &str, key: &str) -> String {
    outputs_of(described)
        .remove(key)
        .unwrap_or_else(|| panic!("case {case:?}: no {key:?} output in {described}"))
}

/// The document IAM holds for the role's embedded stream grant, read through `GetRolePolicy` —
/// the call a client reads an embedded policy back with.
///
/// IAM reports a policy document URL-encoded compliant with RFC 3986, so the member is
/// percent-decoded back to plain JSON text here, which is what the documented contract tells a
/// caller to do before it reads the document.
async fn embedded_stream_grant(router: &MultiServiceRouter, role: &str) -> String {
    let body = iam(
        router,
        "GetRolePolicy",
        &[("RoleName", role), ("PolicyName", STREAM_POLICY_NAME)],
    )
    .await;
    let document = elements(&body, "PolicyDocument")
        .into_iter()
        .next()
        .unwrap_or_default();
    ::smithy_server_runtime::percent_decode(&document)
}

/// Every message the function's default log group holds right now — "Lambda functions send
/// logs to a default log group named `/aws/lambda/<function name>`". A group no run has created
/// yet is no messages rather than a failure: that is exactly what a client polling for a marker
/// that has not arrived sees.
///
/// `pub(crate)` because every suite that polls a deployed function's own output needs exactly
/// this read, and a second copy of it drifts from this one.
pub(crate) async fn log_messages(router: &MultiServiceRouter, function: &str) -> Vec<String> {
    let group = format!("{LAMBDA_LOG_GROUP_PREFIX}{function}");
    let answered = logs(router, "FilterLogEvents", json!({"logGroupName": group})).await;
    if answered.status != 200 {
        return Vec::new();
    }
    answered
        .json()
        .get("events")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|event| event["message"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// Look at the function's log group again until it carries `marker`, answering every message it
/// last held — the delivery of a batch and the run behind it are owed on the mapping's own
/// clock, so this waits for that CONDITION.
async fn awaited_log_messages(
    router: &MultiServiceRouter,
    function: &str,
    marker: &str,
) -> Vec<String> {
    let giving_up_at = Instant::now() + WAIT;
    loop {
        let seen = log_messages(router, function).await;
        if seen.iter().any(|line| line.contains(marker)) || Instant::now() >= giving_up_at {
            return seen;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// RULE: a CloudFormation create whose declared resources the owning services accept reaches
/// `CREATE_COMPLETE` with every declared resource created, and publishes every declared output
/// computed from what those services minted, so each identifier it publishes resolves in the
/// service that owns the resource — here the whole DynamoDB-stream-driven Lambda estate.
#[tokio::test]
async fn a_declared_dynamodb_stream_lambda_estate_deploys_in_one_stack() {
    let router = emulator();

    for row in rows() {
        let case = row.case.as_str();
        let stack = row.stack();
        let body = template(&row);

        let (status, answered) = cloudformation(
            &router,
            "CreateStack",
            &[
                ("StackName", stack.as_str()),
                ("TemplateBody", body.as_str()),
                // The template declares an IAM role, which a create acknowledges before
                // CloudFormation provisions it.
                ("Capabilities.member.1", "CAPABILITY_IAM"),
                ("Capabilities.member.2", "CAPABILITY_NAMED_IAM"),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {answered}"
        );

        // 1. The create reaches CREATE_COMPLETE, and nothing was rolled back.
        let (reported, described) = wait_past_the_create(&router, &stack).await;
        assert_eq!(
            reported, CREATE_COMPLETE,
            "case {case:?}: expected the stack to reach {CREATE_COMPLETE}, got {described} with \
             resource failures {:?}",
            failure_reasons(&router, &stack).await
        );

        // 2. Every declared resource is one the stack reports as created.
        let (status, resources) =
            cloudformation(
                &router,
                "DescribeStackResources",
                &[("StackName", stack.as_str())],
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeStackResources, got {status} with body \
             {resources}"
        );
        let statuses = pairs_of(&resources, "LogicalResourceId", "ResourceStatus");
        let expected: std::collections::BTreeMap<String, String> =
            [TABLE, ROLE, FUNCTION, MAPPING]
                .into_iter()
                .map(|logical_id| {
                    (
                        logical_id.to_string(),
                        RESOURCE_CREATE_COMPLETE.to_string(),
                    )
                })
                .collect();
        assert_eq!(
            statuses, expected,
            "case {case:?}: expected every declared resource {RESOURCE_CREATE_COMPLETE}, got \
             {statuses:?} in {resources}"
        );
        let published = physical_ids_of(&resources);

        // 3. The table: the name and the ARN the stack published are the table's own, and the
        //    stream ARN it exported is the LIVE one DynamoDB reports for it.
        let table_name = output(case, &described, TABLE_NAME_OUTPUT);
        assert_eq!(
            table_name,
            row.table(),
            "case {case:?}: expected the stack to publish the declared table name, got {described}"
        );
        assert_eq!(
            published.get(TABLE).map(String::as_str),
            Some(table_name.as_str()),
            "case {case:?}: expected the table's physical id to be the name the output reports, \
             got {published:?}"
        );
        let (status, table) = described_table(&router, &table_name).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected DynamoDB to resolve the table name {table_name:?} the stack \
             published, got {status} with {table}"
        );
        let table_arn = output(case, &described, TABLE_ARN_OUTPUT);
        assert_eq!(
            table["Table"]["TableArn"],
            json!(table_arn),
            "case {case:?}: expected the exported ARN to be the one DynamoDB reports for the \
             table, got {table}"
        );
        assert!(
            table_arn.ends_with(&format!(":table/{table_name}")),
            "case {case:?}: expected the table's ARN to name the table, got {table_arn:?}"
        );
        let stream_arn = output(case, &described, STREAM_ARN_OUTPUT);
        assert_eq!(
            table["Table"]["LatestStreamArn"],
            json!(stream_arn),
            "case {case:?}: expected the exported StreamArn to be the live LatestStreamArn, got \
             {table}"
        );
        assert!(
            stream_arn.starts_with(&format!("{table_arn}/stream/")),
            "case {case:?}: expected the stream ARN to be the table's own, got {stream_arn:?}"
        );
        assert_eq!(
            table["Table"]["StreamSpecification"]["StreamViewType"],
            json!(row.view_type),
            "case {case:?}: expected the stream to capture what the declaration asked for, got \
             {table}"
        );

        // 4. The role: the Fn::Sub over the table's own ARN reached IAM as the resolved stream
        //    prefix, under the name the declaration embedded it with.
        let role_name = published
            .get(ROLE)
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: no physical id for the role in {resources}"));
        let grant = embedded_stream_grant(&router, &role_name).await;
        assert!(
            grant.contains(&format!("{table_arn}/stream/*")),
            "case {case:?}: expected the embedded grant to name the resolved stream prefix \
             {table_arn}/stream/*, got {grant}"
        );

        // 5. The mapping: the id the stack published is the UUID Lambda minted, and the mapping
        //    it addresses polls the exported stream on behalf of the deployed function.
        let function_name = output(case, &described, FUNCTION_NAME_OUTPUT);
        assert_eq!(
            function_name,
            row.function(),
            "case {case:?}: expected the stack to publish the declared function name, got \
             {described}"
        );
        let function_arn = output(case, &described, FUNCTION_ARN_OUTPUT);
        assert!(
            function_arn.ends_with(&format!(":function:{function_name}")),
            "case {case:?}: expected the function's ARN to name the function, got {function_arn:?}"
        );
        let uuid = output(case, &described, MAPPING_ID_OUTPUT);
        assert!(
            is_mapping_uuid(&uuid),
            "case {case:?}: expected the published mapping id to be the UUID Lambda assigns a \
             mapping, got {uuid:?}"
        );
        assert_eq!(
            published.get(MAPPING).map(String::as_str),
            Some(uuid.as_str()),
            "case {case:?}: expected the mapping's physical id to be the {uuid:?} the output \
             reports, got {published:?}"
        );
        let settled = mapping_until_settled(&router, case, &uuid).await;
        assert_eq!(
            settled.get("State").and_then(Value::as_str),
            Some(MAPPING_STATE_ENABLED),
            "case {case:?}: expected the mapping to settle into {MAPPING_STATE_ENABLED}, got \
             {settled}"
        );
        assert_eq!(
            settled.get("EventSourceArn").and_then(Value::as_str),
            Some(stream_arn.as_str()),
            "case {case:?}: expected the mapping to poll the exported stream, got {settled}"
        );
        assert_eq!(
            settled.get("FunctionArn").and_then(Value::as_str),
            Some(function_arn.as_str()),
            "case {case:?}: expected the mapping to invoke the function the stack created, got \
             {settled}"
        );
        assert_eq!(
            settled.get("BatchSize").and_then(Value::as_u64),
            Some(row.batch_size),
            "case {case:?}: expected the batch size the declaration asked for, got {settled}"
        );

        // 6. And the estate WORKS: a write to the deployed table reaches the deployed function,
        //    whose printed line lands in its own log group.
        let key = row.written_key();
        let mut item = json!({PARTITION_KEY: {"S": key}, "payload": {"S": "written"}});
        if row.sort_key {
            item[SORT_KEY] = json!({"N": "1"});
        }
        let (status, written) =
            dynamodb(&router, "PutItem", &json!({"TableName": table_name, "Item": item})).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from PutItem on the deployed table, got {status} \
             with {written}"
        );
        let marker = format!("{RECORD_MARKER} {key}");
        let seen = awaited_log_messages(&router, &function_name, &marker).await;
        assert!(
            seen.iter().any(|line| line.contains(&marker)),
            "case {case:?}: expected the write to reach the function, whose line {marker:?} lands \
             in its log group, got {seen:?}"
        );
    }
}
