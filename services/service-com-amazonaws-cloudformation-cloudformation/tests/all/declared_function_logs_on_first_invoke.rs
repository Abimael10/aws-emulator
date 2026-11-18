// SPDX-License-Identifier: CC0-1.0

//! The log group of an `AWS::Lambda::Function` a stack declares: absent while the stack is the
//! only thing that has happened, and opened by the function's FIRST invocation.
//!
//! RULE: a stack reaches a successful terminal status only once every resource it declares
//! really exists in the service that OWNS it, so a declared function is one AWS Lambda holds
//! and takes calls on (`State` `Active`) the moment the stack reports the deployment complete —
//! and from there the function behaves as any other function of that service does: its log
//! group (the one its `LoggingConfig$LogGroup` names, else the default
//! `/aws/lambda/<function name>`) is created by the first invocation that is allowed to create
//! it, acting as the execution role the stack created, never by the deployment.
//!
//! That is the chain a deployment pipeline walks: `CreateStack`, wait for the stack, wait for
//! the function (`GetFunctionConfiguration`, the `FunctionActiveV2` waiter built on it), then
//! invoke it and read its log. A stack that reports success for a function AWS Lambda does not
//! hold breaks the chain at its second step, far from its cause, and nothing downstream of it —
//! the implicit log group among it — can be observed at all.
//!
//! The rows differ along what the rule generalises over: whether the template NAMES the function
//! or leaves CloudFormation to name it (so the default group is named after an identifier the
//! stack decided), the runtime family the declaration states, which document the declared
//! execution role carries its CloudWatch Logs grant on (an AWS-managed policy attached by
//! `ManagedPolicyArns`, an inline `Policies` document, and a document granting nothing on that
//! service), and whether the function's `LoggingConfig` sends its log somewhere other than the
//! default group — with the template declaring that group itself, the shape a deployment that
//! manages its function's logs has.
//!
//! Every expected value is computed here from what the row declares plus the identifiers the
//! stack publishes through its outputs; nothing is read out of the answer being checked.

use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::dynamodb_table_resources::failure_reasons;
use crate::hotswapped_function_code::lambda;
// ONE spelling of a CloudWatch Logs call for this target, owned by the suite that first needed
// it, and the two group reads built on it.
use crate::logs_group_children_resources::{group_exists, logs, stream_names};
use crate::stack_lifecycle::{
    acknowledging_iam, create_stack, output_value, router, ACCOUNT_ID, REGION,
};
// The sibling suite over the identity of a declared function deploys the same shape, so what
// settling is, how long it is given, the trust policy the role is declared with, the managed
// policy it carries and the state AWS Lambda takes calls in are ITS definitions rather than a
// second copy of them here.
use crate::stack_declared_function_identity::{
    settled_stack, trust_policy, LOOKS, LOOK_AGAIN, MANAGED_POLICY_ARN, STATE_ACTIVE,
};
use service_com_amazonaws_cloudformation_cloudformation::state::status;

/// The logical ids every template below declares its resources under.
const ROLE_LOGICAL_ID: &str = "ExecutionRole";
const GROUP_LOGICAL_ID: &str = "LogGroup";
const FUNCTION_LOGICAL_ID: &str = "Fn";

/// The output keys every template publishes the function's identity under.
const NAME_OUTPUT: &str = "FunctionName";
const ARN_OUTPUT: &str = "FunctionArn";

/// The IAM action namespace CloudWatch Logs' calls are named in, and the three calls a
/// function's runtime makes to deliver one invocation's log. Source: the actions AWS's own
/// managed execution-role policy for a function (`AWSLambdaBasicExecutionRole`) grants.
const LOGS_NAMESPACE: &str = "logs";
const LOG_WRITES: [&str; 3] = ["CreateLogGroup", "CreateLogStream", "PutLogEvents"];

/// The fixed text of the framing a function's runtime writes around one run's own output.
/// Source: the lines Lambda writes for one invocation — `START RequestId: <id> Version:
/// <version>`, `END RequestId: <id>` and `REPORT RequestId: <id>` with the run's spend.
const START_PREFIX: &str = "START RequestId: ";
const END_PREFIX: &str = "END RequestId: ";
const REPORT_PREFIX: &str = "REPORT RequestId: ";

/// Which document the declared execution role carries its CloudWatch Logs grant on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Grant {
    /// `ManagedPolicyArns` naming [`MANAGED_POLICY_ARN`], the AWS-managed basic-execution
    /// policy, which grants exactly the three log writes a function's runtime makes.
    AwsManaged,
    /// An inline `Policies` document stating the three log writes.
    Inline,
    /// An inline `Policies` document about another service only: a policy set carrying no log
    /// write at all.
    NoLogs,
}

/// One row of the table.
struct Row {
    case: &'static str,
    stack: &'static str,
    /// The `FunctionName` the declaration states, or `None` to leave the naming to
    /// CloudFormation — the physical id the stack then publishes through `Ref`.
    declared_name: Option<&'static str>,
    runtime: &'static str,
    /// The group the function's `LoggingConfig` names, or `None` to take the default.
    logging_config_group: Option<&'static str>,
    /// Whether the template declares an `AWS::Logs::LogGroup` at the group the function logs
    /// to, so the group exists before any invocation.
    declares_the_group: bool,
    grant: Grant,
    /// The one line the handler writes, which has to be one event of its own.
    written: &'static str,
    /// Whether the run's log has to be in the function's group once it has been invoked.
    delivers: bool,
}

const ROWS: &[Row] = &[
    Row {
        // The issue's own shape: a named function on a role carrying the managed
        // basic-execution policy, logging to the default group.
        case: "a named function whose declared role carries the managed basic-execution policy",
        stack: "declared-logs-managed-stack",
        declared_name: Some("declared-logs-managed"),
        runtime: "python3.12",
        logging_config_group: None,
        declares_the_group: false,
        grant: Grant::AwsManaged,
        written: "declared-logs-managed-ran",
        delivers: true,
    },
    Row {
        // No `FunctionName`: the default group is named after the physical id CloudFormation
        // decided, and the grant arrives on an inline document instead.
        case: "a function CloudFormation names, its logs grant on an inline policy",
        stack: "declared-logs-inline-stack",
        declared_name: None,
        runtime: "nodejs20.x",
        logging_config_group: None,
        declares_the_group: false,
        grant: Grant::Inline,
        written: "declared-logs-inline-ran",
        delivers: true,
    },
    Row {
        // The function logs to a group of the deployment's own, which the template declares.
        case: "a function logging to the group the template declares for it",
        stack: "declared-logs-vended-stack",
        declared_name: Some("declared-logs-vended"),
        runtime: "nodejs22.x",
        logging_config_group: Some("/aws/vendedlogs/declared-logs-vended"),
        declares_the_group: true,
        grant: Grant::AwsManaged,
        written: "declared-logs-vended-ran",
        delivers: true,
    },
    Row {
        // The role the stack created is what Lambda acts as, so a declared role granting
        // nothing on CloudWatch Logs leaves the group uncreated — and the invocation is served
        // all the same.
        case: "a declared role granting nothing on CloudWatch Logs",
        stack: "declared-logs-ungranted-stack",
        declared_name: Some("declared-logs-ungranted"),
        runtime: "nodejs20.x",
        logging_config_group: None,
        declares_the_group: false,
        grant: Grant::NoLogs,
        written: "declared-logs-ungranted-ran",
        delivers: false,
    },
];

/// One `Allow` statement over `namespace`'s `actions`, on every resource.
fn allow(namespace: &str, actions: &[&str]) -> Value {
    let named: Vec<String> = actions
        .iter()
        .map(|action| format!("{namespace}:{action}"))
        .collect();
    json!({"Effect": "Allow", "Action": named, "Resource": "*"})
}

/// The inline source the row's function is declared with, which writes its one line through the
/// runtime's own console — so the line the delivery has to carry is the handler's, not this
/// suite's.
fn inline_source(row: &Row) -> String {
    if row.runtime.starts_with("python") {
        format!(
            "def handler(event, context):\n    print('{}')\n    return {{'ok': True}}\n",
            row.written
        )
    } else {
        format!(
            "exports.handler = async () => {{\n    console.log('{}');\n    return {{ ok: true \
             }};\n}};\n",
            row.written
        )
    }
}

/// The handler the declaration names, which decides the file the inline source is archived
/// under: `index` plus the extension the declared runtime loads by default.
const HANDLER: &str = "index.handler";

/// The group the row's function logs to: the one its `LoggingConfig` names, else the default
/// built from the name the stack published.
///
/// Source: `LoggingConfig$LogGroup` — "The name of the Amazon CloudWatch log group the function
/// sends logs to. By default, Lambda functions send logs to a default log group named
/// /aws/lambda/<function name>".
fn log_group_of(row: &Row, name: &str) -> String {
    row.logging_config_group
        .map_or_else(|| format!("/aws/lambda/{name}"), str::to_string)
}

/// The template a row deploys: the execution role with the row's grant on the row's document,
/// the log group the row declares, and the function — with the outputs that publish the
/// identifiers the stack decided.
fn template(row: &Row) -> String {
    let mut role_properties = json!({"AssumeRolePolicyDocument": trust_policy()});
    match row.grant {
        Grant::AwsManaged => {
            role_properties["ManagedPolicyArns"] = json!([MANAGED_POLICY_ARN]);
        }
        Grant::Inline => {
            role_properties["Policies"] = json!([{
                "PolicyName": "write-the-log",
                "PolicyDocument": {
                    "Version": "2012-10-17",
                    "Statement": [allow(LOGS_NAMESPACE, &LOG_WRITES)],
                },
            }]);
        }
        Grant::NoLogs => {
            role_properties["Policies"] = json!([{
                "PolicyName": "read-one-bucket",
                "PolicyDocument": {
                    "Version": "2012-10-17",
                    "Statement": [allow("s3", &["GetObject"])],
                },
            }]);
        }
    }

    let mut function_properties = json!({
        "Runtime": row.runtime,
        "Handler": HANDLER,
        "Role": {"Fn::GetAtt": [ROLE_LOGICAL_ID, "Arn"]},
        "Code": {"ZipFile": inline_source(row)},
    });
    if let Some(name) = row.declared_name {
        function_properties["FunctionName"] = json!(name);
    }
    if let Some(group) = row.logging_config_group {
        function_properties["LoggingConfig"] = json!({"LogGroup": group});
    }
    let mut function = json!({
        "Type": "AWS::Lambda::Function",
        "Properties": function_properties,
    });

    let mut resources = Map::new();
    resources.insert(
        ROLE_LOGICAL_ID.to_string(),
        json!({"Type": "AWS::IAM::Role", "Properties": role_properties}),
    );
    if row.declares_the_group {
        let group = row
            .logging_config_group
            .expect("a row that declares the group names it");
        resources.insert(
            GROUP_LOGICAL_ID.to_string(),
            json!({
                "Type": "AWS::Logs::LogGroup",
                "Properties": {"LogGroupName": group, "RetentionInDays": 3},
            }),
        );
        function["DependsOn"] = json!(GROUP_LOGICAL_ID);
    }
    resources.insert(FUNCTION_LOGICAL_ID.to_string(), function);

    json!({
        "Resources": Value::Object(resources),
        "Outputs": {
            NAME_OUTPUT: {"Value": {"Ref": FUNCTION_LOGICAL_ID}},
            ARN_OUTPUT: {"Value": {"Fn::GetAtt": [FUNCTION_LOGICAL_ID, "Arn"]}},
        },
    })
    .to_string()
}

/// What `GetFunctionConfiguration` reports for `name`, as AWS Lambda's model binds it: `GET
/// /2015-03-31/functions/{FunctionName}/configuration`.
async fn configuration(router: &MultiServiceRouter, case: &str, name: &str) -> Value {
    let answered = lambda(
        router,
        "GET",
        &format!("/2015-03-31/functions/{name}/configuration"),
        &[],
        &[],
    )
    .await;
    assert_eq!(
        answered.status, 200,
        "case {case:?}: GetFunctionConfiguration({name:?}) must answer 200 for the function the \
         stack reports as created, got {} with {}",
        answered.status, answered.body
    );
    answered.body
}

/// The `State` AWS Lambda reports for `name`, looked for again until it is no longer the
/// provisioning one — which is what the `FunctionActiveV2` waiter does for a client.
///
/// `pub(crate)` with [`invoke`]: the suite over the log group a stack declares at a function's
/// own log-group name (`declared_log_group_takes_the_functions_log.rs`) waits on the same
/// condition and invokes the same way, so neither of them can drift from the other.
pub(crate) async fn awaited_state(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let mut last = String::new();
    for _ in 0..LOOKS {
        last = configuration(router, case, name)
            .await
            .get("State")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if last == STATE_ACTIVE {
            return last;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
    last
}

/// The messages of the events one stream holds, in the order they were written, read through
/// `GetLogEvents`, each as the RECORD's own text: every record of a run is delivered closed by
/// the line break the write that produced it ended with, which belongs to the record rather
/// than to its text.
///
/// `pub(crate)` for the reason [`logs`] is `pub(crate)`: the suite over a stack refused for a
/// log-group name an earlier function's invocation left behind
/// (`implicit_log_group_collision.rs`) reads that group's events back through this one reader
/// rather than a second copy of it.
pub(crate) async fn event_messages(
    router: &MultiServiceRouter,
    group: &str,
    stream: &str,
) -> Vec<String> {
    let (status, document) = logs(
        router,
        "GetLogEvents",
        &json!({
            "logGroupName": group,
            "logStreamName": stream,
            "startFromHead": true,
        }),
    )
    .await;
    if status != 200 {
        return Vec::new();
    }
    document
        .get("events")
        .and_then(Value::as_array)
        .map(|events| {
            events
                .iter()
                .filter_map(|event| {
                    event
                        .get("message")
                        .and_then(Value::as_str)
                        .map(|message| message.strip_suffix('\n').unwrap_or(message).to_string())
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Invoke `name` synchronously, and answer what the run reported: a function that RAN carries
/// no `X-Amz-Function-Error`.
///
/// `pub(crate)` for the reason [`awaited_state`] is.
pub(crate) async fn invoke(router: &MultiServiceRouter, case: &str, name: &str) -> Value {
    let answered = lambda(
        router,
        "POST",
        &format!("/2015-03-31/functions/{name}/invocations"),
        json!({"probe": "declared-logs"}).to_string().as_bytes(),
        &[("x-amz-invocation-type", "RequestResponse")],
    )
    .await;
    assert_eq!(
        (answered.status, answered.function_error()),
        (200, None),
        "case {case:?}: the declared function {name:?} must run to completion, got {} with {}",
        answered.status,
        answered.body
    );
    answered.body
}

/// Look again until `group` holds a stream carrying the run's closing report, and answer that
/// stream's events; an empty answer means nothing was ever delivered.
///
/// `pub(crate)` for the reason [`awaited_state`] is: the suite over the group a stack declares
/// at a function's own log-group name (`declared_log_group_takes_the_functions_log.rs`) waits
/// for the same delivery through this one loop rather than a second copy of it.
pub(crate) async fn delivered_log(router: &MultiServiceRouter, group: &str) -> Vec<String> {
    let mut last = Vec::new();
    for _ in 0..LOOKS {
        for stream in stream_names(router, group).await {
            let held = event_messages(router, group, &stream).await;
            if held.iter().any(|line| line.starts_with(REPORT_PREFIX)) {
                return held;
            }
            if !held.is_empty() {
                last = held;
            }
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
    last
}

#[tokio::test]
async fn a_declared_functions_log_group_is_opened_by_its_first_invocation() {
    for row in ROWS {
        let case = row.case;
        let router = router();

        let (code, created) = create_stack(
            &router,
            row.stack,
            &template(row),
            &acknowledging_iam(&[]),
        )
        .await;
        assert_eq!(
            code, 200,
            "case {case:?}: CreateStack must answer 200, got {code}: {created}"
        );

        let (reported_status, described) = settled_stack(&router, case, row.stack).await;
        assert_eq!(
            reported_status,
            status::CREATE_COMPLETE,
            "case {case:?}: the stack must reach CREATE_COMPLETE, got {reported_status:?} \
             because {:?}",
            failure_reasons(&router, row.stack).await
        );

        let name = output_value(&described, NAME_OUTPUT).unwrap_or_else(|| {
            panic!("case {case:?}: the stack must publish the function's name, got {described}")
        });
        if let Some(declared) = row.declared_name {
            assert_eq!(
                name, declared,
                "case {case:?}: the stack must publish the declared name, got {name:?}"
            );
        }
        // The ARN a client builds for the published name, so the identity the stack reports is
        // checked against the form rather than against itself.
        let arn = output_value(&described, ARN_OUTPUT).unwrap_or_else(|| {
            panic!("case {case:?}: the stack must publish the function's ARN, got {described}")
        });
        assert_eq!(
            arn,
            format!("arn:aws:lambda:{REGION}:{ACCOUNT_ID}:function:{name}"),
            "case {case:?}: the ARN the stack publishes must be the function ARN of {name:?}, \
             got {arn:?}"
        );

        // The step a deployment pipeline takes next: the function AWS Lambda holds under the
        // identity the stack published, out of its provisioning window.
        let state = awaited_state(&router, case, &name).await;
        assert_eq!(
            state, STATE_ACTIVE,
            "case {case:?}: AWS Lambda must report the function the stack published ({arn:?}) \
             {STATE_ACTIVE} once the stack is complete, got {state:?}"
        );

        // The group is the first invocation's doing, not the deployment's — unless the template
        // declared the group itself, in which case the stack is what created it.
        let group = log_group_of(row, &name);
        let before = group_exists(&router, &group).await;
        assert_eq!(
            before, row.declares_the_group,
            "case {case:?}: before any invocation {group:?} must exist only where the template \
             declares it, got {before}"
        );
        let streams_before = stream_names(&router, &group).await;
        assert!(
            streams_before.is_empty(),
            "case {case:?}: before any invocation {group:?} must hold no stream, got \
             {streams_before:?}"
        );

        let answered = invoke(&router, case, &name).await;
        assert_eq!(
            answered.get("ok"),
            Some(&json!(true)),
            "case {case:?}: expected the declared handler's own answer, got {answered}"
        );

        if !row.delivers {
            // The write was withheld, not the invocation: the group stays as the deployment
            // left it and the call was served all the same.
            let after = group_exists(&router, &group).await;
            assert_eq!(
                after, row.declares_the_group,
                "case {case:?}: a declared role with no logs grant must leave {group:?} as the \
                 deployment left it, got {after}"
            );
            let streams_after = stream_names(&router, &group).await;
            assert!(
                streams_after.is_empty(),
                "case {case:?}: a declared role with no logs grant must leave no stream in \
                 {group:?}, got {streams_after:?}"
            );
            continue;
        }

        assert!(
            group_exists(&router, &group).await,
            "case {case:?}: the first invocation must leave the function's log group {group:?} \
             in CloudWatch Logs, got none"
        );
        let streams_after = stream_names(&router, &group).await;
        assert_eq!(
            streams_after.len(),
            1,
            "case {case:?}: the one invocation must open one stream in {group:?}, got \
             {streams_after:?}"
        );
        let delivered = delivered_log(&router, &group).await;
        assert_eq!(
            delivered.len(),
            4,
            "case {case:?}: expected the run's three framing lines around the handler's own, got \
             {delivered:?}"
        );
        assert!(
            delivered[0].starts_with(START_PREFIX),
            "case {case:?}: expected the delivered log to open on its START line, got \
             {delivered:?}"
        );
        // A node runtime delivers a console line FRAMED — the instant, the invocation's id,
        // the level, then the text — so what the handler wrote is the line's last field; a
        // python handler's `print` is delivered as the text it is.
        assert_eq!(
            delivered[1].rsplit('\t').next().unwrap_or_default(),
            row.written,
            "case {case:?}: expected the handler's own line, got {delivered:?}"
        );
        assert!(
            delivered[2].starts_with(END_PREFIX) && delivered[3].starts_with(REPORT_PREFIX),
            "case {case:?}: expected the delivered log to close on END and then REPORT, got \
             {delivered:?}"
        );

        // A function whose configuration names another group leaves the DEFAULT one uncreated:
        // the log went where the declaration said, not to both.
        if row.logging_config_group.is_some() {
            let default_group = format!("/aws/lambda/{name}");
            let held = group_exists(&router, &default_group).await;
            assert!(
                !held,
                "case {case:?}: a function logging to {group:?} must leave {default_group:?} \
                 uncreated, got it held"
            );
        }
    }
}
