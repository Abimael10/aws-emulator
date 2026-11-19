// SPDX-License-Identifier: CC0-1.0

//! The AWS SDK a managed `python3.x` runtime SUPPLIES, driven end to end over the wire:
//! Lambda, Amazon DynamoDB, IAM and the token service on ONE engine, a function whose
//! module-scope code imports the SDK, and the item it wrote read back where a client reads it.
//!
//! RULE: a managed `python3.x` runtime puts the AWS SDK's own modules (`boto3`, `botocore`
//! and their subpackages) on the handler's import path, so an SDK import at MODULE scope
//! resolves during the initialization phase whatever the point release of the runtime and
//! whichever of the SDK's import forms the module uses — the invocation answers HTTP 200 with
//! no `X-Amz-Function-Error` (`Invoke` binds that member to the `X-Amz-Function-Error` header,
//! so a module that could not be imported is reported there), and a client built from what was
//! imported reaches the service it names, addressing the target the function's CURRENT
//! configuration points at and bounded by the execution role's own grants.
//!
//! That the SDK is importable at all is pinned beside this file by the suites over what a
//! RUNNING handler reaches with it (`tests/all/handler_aws_sdk.rs` and
//! `tests/all/handler_object_and_item_store_sdk.rs`), each over one runtime and one import
//! form; what is new here is the generalisation along the two axes below, and the repoint.
//!
//! The rows differ along what the rule generalises over: WHICH point release of the runtime
//! runs the module (a spread of the `python3.x` identifiers of the model's `Runtime` value set
//! that this crate's own deprecation catalog (`state::DEPRECATED_RUNTIMES`) does not name,
//! including releases the issue did not cite), and WHICH import form the module reaches the
//! SDK through (the package, a name out of it, a submodule, a class out of a submodule, a
//! configuration class beside the client, and a subpackage whose import binds the package).
//! Each row's client is built at module scope from exactly what that row imported, so a row
//! whose form did not resolve cannot run at all.
//!
//! The second suite drives the repoint: the same handler, one environment variable naming its
//! target, `UpdateFunctionConfiguration` pointing that variable at another table, and an
//! execution role still scoped to the FIRST table — so the SDK call the next invocation makes
//! addresses the new target and is refused by DynamoDB itself, as that service words it, which
//! is a refusal of the CALL and not a failure to import the SDK.
//!
//! The suites RUN handlers, so they take this target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]).

use std::sync::Arc;

use base64::Engine as _;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The DynamoDB wire call, the table fixture and the key attribute it keys a table on are the
// ones the stream-delivery suite publishes, reached rather than written a second time here.
use crate::dynamodb_stream_event_source_delivery::{dynamodb, given_table, KEY};
use crate::event_source_mapping_role_access::{allow, given_role, logs_only};
use crate::event_source_mappings::{await_function_active, call, router_on};
// The one .zip writer this crate's suites package a handler with, exported by the suite that
// first needed it rather than copied here.
use crate::node_invoke_run::package;

/// Where `CreateFunction` is bound, and the prefix every function request below is built on.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The module and attribute every package below carries its handler as.
const HANDLER: &str = "lambda_function.lambda_handler";

/// The environment variable each handler reads its target table out of.
const TARGET_VARIABLE: &str = "TARGET_TABLE";

/// The attribute each handler records the runtime it ran under on.
const RAN_UNDER: &str = "ran_under";

/// The table the rows of the first suite write to.
const TABLE: &str = "bundled-sdk-ledger";

/// The two tables of the repoint suite: the one the function is created pointing at, and the
/// one it is repointed to.
const FIRST_TABLE: &str = "bundled-sdk-first";
/// See [`FIRST_TABLE`].
const SECOND_TABLE: &str = "bundled-sdk-second";

/// The code and status DynamoDB refuses an unauthorized request with (its
/// `AccessDeniedException`, HTTP 400).
const ACCESS_DENIED: &str = "AccessDeniedException";
/// See [`ACCESS_DENIED`].
const ACCESS_DENIED_STATUS: u16 = 400;

/// The IAM action of the call every handler below makes.
const PUT_ITEM_ACTION: &str = "dynamodb:PutItem";

/// Lambda, Amazon DynamoDB, IAM and the token service over ONE shared engine, as
/// `core/router/src/lib.rs` mounts them: the handler's own SDK call is dispatched back into
/// this same router, so the table it reaches is the one this suite reads and the role its
/// credentials are minted of is the record IAM holds.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = router_on(&engine);
    router.register_service(
        service_com_amazonaws_dynamodb_dynamodb_20120810::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(engine),
    );
    router
}

/// One DynamoDB call that must be served, over the shared wire helper.
///
/// `pub(crate)` for the reason [`given_table_arn`] is: a suite reading back what a handler's own
/// SDK call left in the table (`tests/all/handler_item_store_and_bus_operations.rs`) makes the
/// same read, and shares this one wrapper rather than keeping a copy of it.
pub(crate) async fn dynamodb_ok(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let (status, answered) = dynamodb(router, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: DynamoDB {operation} must succeed, got {status} with {answered}"
    );
    answered
}

/// The table `name`, created and waited out to `ACTIVE` by the shared fixture, answered as the
/// ARN DynamoDB persisted — which is the resource a grant for it names.
///
/// `pub(crate)` because the suites that grant a role one table and then read the refusal naming
/// it (`tests/all/sqs_event_source_revoked_downstream_grant.rs`) need the very ARN DynamoDB
/// persisted, read this one way.
pub(crate) async fn given_table_arn(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    given_table(router, case, name, None).await;
    let described = dynamodb_ok(router, case, "DescribeTable", &json!({"TableName": name})).await;
    described["Table"]["TableArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no TableArn in {described}"))
        .to_string()
}

/// The handler every function below runs: it imports the SDK at MODULE scope through the form
/// the row states, builds its client there from what that import bound, and writes one item to
/// the table its environment names — reporting either what it wrote or the refusal the
/// addressed service answered, as the SDK's own error class carries it.
fn handler_source(imports: &str, client: &str) -> String {
    format!(
        r#"
{imports}
from botocore.exceptions import ClientError
import os

TARGET = os.environ["{TARGET_VARIABLE}"]
CLIENT = {client}


def lambda_handler(event, context):
    item = {{
        "{KEY}": {{"S": event["item"]}},
        "{RAN_UNDER}": {{"S": os.environ["AWS_EXECUTION_ENV"]}},
    }}
    try:
        CLIENT.put_item(TableName=TARGET, Item=item)
    except ClientError as error:
        return {{
            "refused": {{
                "code": error.response["Error"]["Code"],
                "message": error.response["Error"]["Message"],
                "status": error.response["ResponseMetadata"]["HTTPStatusCode"],
            }}
        }}
    return {{
        "wrote": event["item"],
        "table": TARGET,
        "{RAN_UNDER}": item["{RAN_UNDER}"]["S"],
    }}
"#
    )
}

/// Create the function `name`, running `source` under `runtime` as `role_arn`, with
/// [`TARGET_VARIABLE`] naming `table`.
async fn given_function(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    runtime: &str,
    source: &str,
    role_arn: &str,
    table: &str,
) {
    let created = call(
        router,
        "POST",
        FUNCTIONS_URI,
        &json!({
            "FunctionName": name,
            "Role": role_arn,
            "Runtime": runtime,
            "Handler": HANDLER,
            "Timeout": 30,
            "Environment": { "Variables": { TARGET_VARIABLE: table } },
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD
                    .encode(package(&[("lambda_function.py", source)])),
            },
        }),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "case {case:?}: expected CreateFunction {name:?} to answer 201, got {} {}",
        created.status,
        created.text()
    );
}

/// Invoke `name` with one item key, answering what the handler reported — asserting on the way
/// that the run reported no function error, which is what a runtime that could not import the
/// SDK reports instead of a result.
async fn invoked(router: &MultiServiceRouter, case: &str, name: &str, item: &str) -> Value {
    let answered = call(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{name}/invocations"),
        &json!({ "item": item }),
    )
    .await;
    assert_eq!(
        answered.status,
        200,
        "case {case:?}: expected HTTP 200 from Invoke, got {} {}",
        answered.status,
        answered.text()
    );
    let payload = answered.json();
    assert_eq!(
        answered.header("x-amz-function-error"),
        None,
        "case {case:?}: expected the handler's own result, got a function error with {payload}"
    );
    payload
}

/// The assumed-role session an SDK call made by an invocation of `function` speaks for: the
/// session is named for the FUNCTION, and it is the principal a refusal names.
fn session(role: &str, function: &str) -> String {
    format!(
        "arn:aws:sts::{}:assumed-role/{role}/{function}",
        engine::emulator_account_id()
    )
}

/// One row of the first suite: a point release of the runtime, and the import form the module
/// reaches the SDK through.
struct Row {
    case: &'static str,
    /// A `python3.x` identifier of the model's `Runtime` value set
    /// (`smithy_query(query="shape", shape="Runtime")`) that this crate's own catalog of the
    /// identifiers Lambda no longer supports (`state::DEPRECATED_RUNTIMES`) does not name, so
    /// a function may be created with it.
    runtime: &'static str,
    /// What the handler's module imports, at module scope.
    imports: &'static str,
    /// The expression the module builds its client with, which may only use what `imports`
    /// bound.
    client: &'static str,
}

impl Row {
    /// The function this row invokes.
    fn function(&self) -> String {
        format!("bundled-sdk-{}", self.runtime.replace('.', "-"))
    }

    /// The item key this row writes, so one row's item is its own.
    fn item(&self) -> String {
        format!("item-{}", self.runtime)
    }

    /// What `AWS_EXECUTION_ENV` states for this row's runtime, which is how the handler
    /// reports which runtime really ran it.
    fn execution_env(&self) -> String {
        format!("AWS_Lambda_{}", self.runtime)
    }
}

/// The rows: every supported `python3.x` release, each reaching the SDK its runtime supplies
/// through a different one of the SDK's import forms.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the package imported whole",
            runtime: "python3.9",
            imports: "import boto3",
            client: r#"boto3.client("dynamodb")"#,
        },
        Row {
            case: "a name imported out of the package",
            runtime: "python3.10",
            imports: "from boto3 import client",
            client: r#"client("dynamodb")"#,
        },
        Row {
            case: "a submodule of the package imported",
            runtime: "python3.11",
            imports: "import boto3.session",
            client: r#"boto3.session.Session().client("dynamodb")"#,
        },
        Row {
            case: "a class imported out of a submodule",
            runtime: "python3.12",
            imports: "from boto3.session import Session",
            client: r#"Session().client("dynamodb")"#,
        },
        Row {
            case: "the package beside botocore's configuration class",
            runtime: "python3.13",
            imports: "import boto3\nfrom botocore.config import Config",
            client: r#"boto3.client("dynamodb", config=Config(retries={"max_attempts": 1}))"#,
        },
        Row {
            case: "a subpackage imported, which binds the package",
            runtime: "python3.14",
            imports: "import boto3.dynamodb.conditions",
            client: r#"boto3.client("dynamodb")"#,
        },
    ]
}

/// Every supported `python3.x` runtime supplies the AWS SDK to a module-scope import, whatever
/// form that import takes — and the client built from it writes the item a client then reads.
#[tokio::test]
async fn every_python_runtime_supplies_the_sdk_to_a_module_scope_import() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    let case = "the SDK a managed python runtime supplies";
    let router = emulator();

    let table_arn = given_table_arn(&router, case, TABLE).await;
    let role = "bundled-sdk-writer";
    let role_arn = given_role(
        &router,
        case,
        role,
        Some(json!([
            allow("dynamodb", &["PutItem"], &table_arn),
            logs_only(),
        ])),
    )
    .await;

    let rows = rows();
    // Every function first, so the provisioning spans the clock owes run down together rather
    // than one after another.
    for row in &rows {
        given_function(
            &router,
            row.case,
            &row.function(),
            row.runtime,
            &handler_source(row.imports, row.client),
            &role_arn,
            TABLE,
        )
        .await;
    }
    for row in &rows {
        await_function_active(&router, row.case, &row.function()).await;
    }

    for row in &rows {
        let reported = invoked(&router, row.case, &row.function(), &row.item()).await;
        let expected = json!({
            "wrote": row.item(),
            "table": TABLE,
            RAN_UNDER: row.execution_env(),
        });
        assert_eq!(
            reported, expected,
            "case {:?} on {}: expected {expected}, got {reported}",
            row.case, row.runtime
        );
    }

    // What DynamoDB ITSELF holds, read where a client reads it: a run that reported a write
    // and did none fails here.
    for row in &rows {
        let held = dynamodb_ok(
            &router,
            row.case,
            "GetItem",
            &json!({ "TableName": TABLE, "Key": { KEY: { "S": row.item() } } }),
        )
        .await;
        let expected = json!({
            KEY: { "S": row.item() },
            RAN_UNDER: { "S": row.execution_env() },
        });
        assert_eq!(
            held["Item"], expected,
            "case {:?} on {}: expected DynamoDB to hold {expected}, got {held}",
            row.case, row.runtime
        );
    }
}

/// The handler of the repoint suite: the first row's import form, which is the one the SDK's
/// own documentation writes.
fn repoint_handler() -> String {
    handler_source("import boto3", r#"boto3.client("dynamodb")"#)
}

/// A variable repointed by `UpdateFunctionConfiguration` is what the NEXT invocation's SDK call
/// addresses, and the execution role's grants are what decide that call — so a role left scoped
/// to the former target earns DynamoDB's own refusal of the new one.
#[tokio::test]
async fn a_repointed_variable_is_what_the_next_invocation_addresses() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    let case = "a target repointed under a role scoped to the former one";
    let router = emulator();

    let first_arn = given_table_arn(&router, case, FIRST_TABLE).await;
    let second_arn = given_table_arn(&router, case, SECOND_TABLE).await;
    let role = "bundled-sdk-repoint";
    let role_arn = given_role(
        &router,
        case,
        role,
        Some(json!([
            allow("dynamodb", &["PutItem"], &first_arn),
            logs_only(),
        ])),
    )
    .await;

    let function = "bundled-sdk-repointed";
    let source = repoint_handler();
    given_function(
        &router,
        case,
        function,
        "python3.12",
        &source,
        &role_arn,
        FIRST_TABLE,
    )
    .await;
    await_function_active(&router, case, function).await;

    let granted = "item-before-the-repoint";
    let reported = invoked(&router, case, function, granted).await;
    let expected = json!({
        "wrote": granted,
        "table": FIRST_TABLE,
        RAN_UNDER: "AWS_Lambda_python3.12",
    });
    assert_eq!(
        reported, expected,
        "case {case:?} / \"the target the configuration names\": expected {expected}, got \
         {reported}"
    );

    // The repoint: the same code, the same role, one variable naming another table.
    let updated = call(
        &router,
        "PUT",
        &format!("{FUNCTIONS_URI}/{function}/configuration"),
        &json!({ "Environment": { "Variables": { TARGET_VARIABLE: SECOND_TABLE } } }),
    )
    .await;
    assert_eq!(
        updated.status,
        200,
        "case {case:?}: expected UpdateFunctionConfiguration to answer 200, got {} {}",
        updated.status,
        updated.text()
    );
    await_function_active(&router, case, function).await;

    let refused_key = "item-after-the-repoint";
    let reported = invoked(&router, case, function, refused_key).await;
    let expected = json!({
        "refused": {
            "code": ACCESS_DENIED,
            "status": ACCESS_DENIED_STATUS,
            "message": format!(
                "User: {} is not authorized to perform: {PUT_ITEM_ACTION} on resource: \
                 {second_arn} because no identity-based policy allows the {PUT_ITEM_ACTION} \
                 action",
                session(role, function)
            ),
        }
    });
    assert_eq!(
        reported, expected,
        "case {case:?} / \"the repointed target, unscoped by the role\": expected {expected}, \
         got {reported}"
    );

    // What the two tables THEMSELVES hold: the granted write landed in the first, and the
    // refused one stored nothing anywhere.
    let held = dynamodb_ok(
        &router,
        case,
        "GetItem",
        &json!({ "TableName": FIRST_TABLE, "Key": { KEY: { "S": granted } } }),
    )
    .await;
    let expected = json!({
        KEY: { "S": granted },
        RAN_UNDER: { "S": "AWS_Lambda_python3.12" },
    });
    assert_eq!(
        held["Item"], expected,
        "case {case:?}: expected the first table to hold {expected}, got {held}"
    );
    for table in [FIRST_TABLE, SECOND_TABLE] {
        let absent = dynamodb_ok(
            &router,
            case,
            "GetItem",
            &json!({ "TableName": table, "Key": { KEY: { "S": refused_key } } }),
        )
        .await;
        assert_eq!(
            absent.get("Item"),
            None,
            "case {case:?}: expected the refused write to have stored nothing in {table:?}, got \
             {absent}"
        );
    }
}
