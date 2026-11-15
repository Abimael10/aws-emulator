// SPDX-License-Identifier: CC0-1.0

//! WHICH policy grants a running handler's own AWS SDK call, end to end over the wire:
//! Lambda, Amazon DynamoDB, IAM and the token service on ONE engine, a `python3.x` function
//! whose module-scope code does `import boto3`, and every item its calls left behind read back
//! where a client reads it.
//!
//! RULE: a managed runtime supplies the AWS SDK as an importable module, and a client the
//! handler builds from it calls the addressed service with temporary credentials of the
//! FUNCTION's execution role — "the execution role grants the function permission to use
//! Amazon Web Services services". Each such call is therefore decided by the ADDRESSED
//! service's own authorization: it is permitted by an `Allow` about the request in the
//! identity-based documents in force on that role OR by one in the addressed resource's own
//! resource-based policy that names it ("the resource-based policy and the identity-based
//! policy are evaluated together, and access is granted if either allows it"), with each
//! statement's `Condition` decided against the condition keys the request itself answers for
//! — for a DynamoDB data-plane call, among them the partition it addresses
//! (`dynamodb:LeadingKeys`) — and refused by IAM's implicit deny when neither grants it.
//! Whatever that service answered, its own result or its own `AccessDeniedException`, reaches
//! the handler as its own value or as its caught `botocore.exceptions.ClientError`: none of it
//! is a failure of the runtime, so a run whose module imported the SDK reports no
//! `FunctionError` at all.
//!
//! The groups differ along what the rule generalises over: WHERE the grant comes from (the
//! role's own identity policy, the table's resource policy alone, and neither), WHETHER a
//! condition scopes that grant to some of the table's partitions and under which set
//! qualifier, WHICH partition the call addresses (one the condition selects and one it does
//! not), and WHICH operation addresses it (a write carrying the item, a read keyed on it).
//!
//! Each group's steps run in ONE invocation, because an invocation is the dearest thing this
//! target does, and the last assertions read every partition back through DynamoDB's OWN API:
//! a step that reported a write without doing the work fails there, and so does a refusal that
//! wrote something anyway.
//!
//! The suite RUNS handlers, so it takes this target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]).

use std::sync::Arc;
use std::time::Instant;

use base64::Engine as _;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The DynamoDB wire call, the table fixture and the key attribute it keys a table on are the
// ones the stream-delivery suite publishes, reached rather than written a second time here.
use crate::dynamodb_stream_event_source_delivery::{
    dynamodb, given_table, BUDGET, KEY, LOOK_AGAIN,
};
use crate::event_source_mapping_role_access::{allow, given_role, logs_only};
use crate::event_source_mappings::{await_function_active, call, router_on};
// The one .zip writer this crate's suites package a handler with, exported by the suite that
// first needed it rather than copied here.
use crate::node_invoke_run::package;

/// Where `CreateFunction` is bound, and the prefix every function request below is built on.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The Region every request below is signed for.
const REGION: &str = "us-east-1";

/// The runtime every function below runs, and the module and attribute its package carries its
/// handler as.
const RUNTIME: &str = "python3.12";
/// See [`RUNTIME`].
const HANDLER: &str = "lambda_function.lambda_handler";

/// The table whose own policy says nothing, so every call on it is decided by the calling
/// role's identity-based documents alone.
const TABLE_OPEN: &str = "grant-source-ledger";

/// The table carrying a resource-based policy of its own, which is the only thing granting one
/// group's role anything.
const TABLE_SHARED: &str = "grant-source-shared";

/// The attribute every item below carries beside its key, and the value the suite seeds the
/// shared table's item with. Neither is read by this emulator: they are the handler's own, and
/// what is asserted is that they round-trip.
const VALUE_ATTRIBUTE: &str = "v";
/// See [`VALUE_ATTRIBUTE`].
const SEEDED: &str = "seeded-by-the-owner";

/// The code and status DynamoDB refuses an unauthorized request with (its
/// `AccessDeniedException`, HTTP 400).
const ACCESS_DENIED: &str = "AccessDeniedException";
/// See [`ACCESS_DENIED`].
const ACCESS_DENIED_STATUS: u16 = 400;

/// What the group whose only grant is the TABLE's own policy is named after: its role is the
/// one that policy names, so the table has to be created with that role's ARN.
const SHARED_SLUG: &str = "shared";

/// The IAM actions of the two operations every group's handler calls.
const PUT_ITEM: &str = "dynamodb:PutItem";
/// See [`PUT_ITEM`].
const GET_ITEM: &str = "dynamodb:GetItem";

/// The condition key whose value is the partition a DynamoDB data-plane call addresses, which
/// is what a fine-grained policy scopes a caller to some of a table's partitions with (the
/// service's own condition key for fine-grained access, as this tree's DynamoDB emulator
/// publishes it: `state::authorization`).
const LEADING_KEYS: &str = "dynamodb:LeadingKeys";

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

/// The ARN of one of this emulator's tables, in the form the service itself mints — the
/// resource a grant for that table names, needed before the table exists because one of them
/// is created with a policy about itself.
fn table_arn(name: &str) -> String {
    service_com_amazonaws_dynamodb_dynamodb_20120810::state::table_arn(REGION, name)
}

/// One DynamoDB call that must be served, over the shared wire helper. The credential this
/// harness signs with is the account's own, so these are the OWNER's reads and writes rather
/// than a role's.
async fn dynamodb_ok(
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

/// Create the table `name` carrying `policy` as its own resource-based policy — "when you
/// attach a resource-based policy while creating a table, the policy application is strongly
/// consistent" (`CreateTable$ResourcePolicy`), so it is in force for the table's first
/// data-plane request — waited out to `ACTIVE` and answered as the ARN DynamoDB persisted.
async fn given_table_with_policy(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    policy: &str,
) -> String {
    let created = dynamodb_ok(
        router,
        case,
        "CreateTable",
        &json!({
            "TableName": name,
            "AttributeDefinitions": [{ "AttributeName": KEY, "AttributeType": "S" }],
            "KeySchema": [{ "AttributeName": KEY, "KeyType": "HASH" }],
            "BillingMode": "PAY_PER_REQUEST",
            "ResourcePolicy": policy,
        }),
    )
    .await;
    assert_eq!(
        created["TableDescription"]["TableName"],
        json!(name),
        "case {case:?}: the create must answer the table it made, got {created}"
    );
    let deadline = Instant::now() + BUDGET;
    loop {
        let read = dynamodb_ok(router, case, "DescribeTable", &json!({ "TableName": name })).await;
        if read["Table"]["TableStatus"] == json!("ACTIVE") {
            return read["Table"]["TableArn"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: no TableArn in {read}"))
                .to_string();
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the table never became ACTIVE: {read}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// The table `name`, created by the shared fixture and waited out to `ACTIVE`, answered as the
/// ARN DynamoDB persisted.
async fn given_plain_table(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    given_table(router, case, name, None).await;
    let described = dynamodb_ok(router, case, "DescribeTable", &json!({"TableName": name})).await;
    described["Table"]["TableArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no TableArn in {described}"))
        .to_string()
}

/// A resource-based policy granting `role_arn` the one READ of `table_arn` and nothing else,
/// which is the whole of what one group's role is permitted by.
fn read_granting_policy(role_arn: &str, table_arn: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": "ReadByTheFunctionsRole",
            "Effect": "Allow",
            "Principal": { "AWS": role_arn },
            "Action": GET_ITEM,
            "Resource": table_arn,
        }],
    })
    .to_string()
}

/// A statement allowing `actions` of DynamoDB on `resource` only for the partitions
/// `partitions` lists, scoped by [`LEADING_KEYS`] under the set qualifier `qualifier`
/// (`ForAllValues:` holds when every value the request carries is listed, `ForAnyValue:` when
/// any of them is).
fn allow_in_partitions(
    actions: &[&str],
    resource: &str,
    qualifier: &str,
    partitions: &[String],
) -> Value {
    let mut clause = serde_json::Map::new();
    clause.insert(LEADING_KEYS.to_string(), json!(partitions));
    let mut condition = serde_json::Map::new();
    condition.insert(format!("{qualifier}:StringEquals"), Value::Object(clause));
    let mut statement = allow("dynamodb", actions, resource);
    statement["Condition"] = Value::Object(condition);
    statement
}

/// The handler every group runs: it imports the SDK and builds its client at MODULE scope, so
/// a runtime shipping no SDK fails the initialization phase and no step runs at all; then for
/// each step the event names it calls that operation and reports what came back — the
/// service's own answer, or the refusal it raised as the SDK's own error class carries it.
const HANDLER_SOURCE: &str = r#"
import boto3
from botocore.exceptions import ClientError

CLIENT = boto3.client("dynamodb")


def lambda_handler(event, context):
    results = []
    for step in event["steps"]:
        try:
            answered = getattr(CLIENT, step["operation"])(**step["arguments"])
        except ClientError as error:
            results.append({
                "ok": False,
                "code": error.response["Error"]["Code"],
                "message": error.response["Error"]["Message"],
                "status": error.response["ResponseMetadata"]["HTTPStatusCode"],
            })
            continue
        read = {"ok": True, "status": answered["ResponseMetadata"]["HTTPStatusCode"]}
        if "Item" in answered:
            read["item"] = answered["Item"]
        results.append(read)
    return {"results": results}
"#;

/// The role one group's function runs as.
fn role_of(slug: &str) -> String {
    format!("grant-source-{slug}")
}

/// The function one group invokes.
fn function_of(slug: &str) -> String {
    format!("grant-source-{slug}-caller")
}

/// The assumed-role session an SDK call made by an invocation of that group's function speaks
/// for: the session is named for the FUNCTION, and it is the principal a refusal names.
fn session_of(slug: &str) -> String {
    format!(
        "arn:aws:sts::{}:assumed-role/{}/{}",
        engine::emulator_account_id(),
        role_of(slug),
        function_of(slug)
    )
}

/// The partition one group's grant selects, and one it does not — one per group, so one
/// group's items are its own.
fn inside(slug: &str) -> String {
    format!("{slug}-inside")
}
/// See [`inside`].
fn outside(slug: &str) -> String {
    format!("{slug}-outside")
}
/// The second partition a group whose grant LISTS more than one of them names.
fn second(slug: &str) -> String {
    format!("{slug}-second")
}

/// The item a handler writes into the partition `partition`, as the wire carries attribute
/// values.
fn item(partition: &str, value: &str) -> Value {
    json!({ KEY: { "S": partition }, VALUE_ATTRIBUTE: { "S": value } })
}

/// The input of a write of that item.
fn put_item_input(table: &str, partition: &str, value: &str) -> Value {
    json!({ "TableName": table, "Item": item(partition, value) })
}

/// The input of a read keyed on one partition.
fn get_item_input(table: &str, partition: &str) -> Value {
    json!({ "TableName": table, "Key": { KEY: { "S": partition } } })
}

/// What a served write must come to, and what a served read of an item the table holds and of
/// a key it holds nothing under must come to.
fn wrote() -> Value {
    json!({ "ok": true, "status": 200 })
}
/// See [`wrote`].
fn read_item(partition: &str, value: &str) -> Value {
    json!({ "ok": true, "status": 200, "item": item(partition, value) })
}

/// The refusal DynamoDB words when nothing in force permits the calling session the action:
/// its own error code and status, over the sentence that names who was refused, which action
/// on which resource, and the reason the refusal rests on.
fn refused(slug: &str, action: &str, table_arn: &str) -> Value {
    json!({
        "ok": false,
        "code": ACCESS_DENIED,
        "status": ACCESS_DENIED_STATUS,
        "message": format!(
            "User: {} is not authorized to perform: {action} on resource: {table_arn} \
             because no identity-based policy allows the {action} action",
            session_of(slug)
        ),
    })
}

/// One step of a group's invocation: the call the handler makes, and what the SDK must have
/// given the guest for it.
struct Step {
    case: &'static str,
    /// The client method, which names the operation.
    operation: &'static str,
    /// The keyword arguments it is called with: the operation's own input members.
    arguments: Value,
    /// What the handler must have read back.
    expected: Value,
}

/// One group: the policies in force on a function's execution role and on the table it
/// addresses, and the steps one invocation of that function makes.
struct Group {
    case: &'static str,
    /// What this group's role, function and partitions are named after.
    slug: &'static str,
    /// The statements of the role's one inline identity policy.
    statements: Value,
    steps: Vec<Step>,
}

/// The groups. Every action named in a grant and in a refusal is the IAM action of the
/// operation the step calls, and every resource is the table that call addressed.
fn groups(open_arn: &str, shared_arn: &str) -> Vec<Group> {
    let unscoped = {
        let slug = "unscoped";
        Group {
            case: "a role whose identity policy grants the table's write and read outright",
            slug,
            statements: json!([
                allow("dynamodb", &["PutItem", "GetItem"], open_arn),
                logs_only(),
            ]),
            steps: vec![
                Step {
                    case: "a write of one partition",
                    operation: "put_item",
                    arguments: put_item_input(TABLE_OPEN, &inside(slug), slug),
                    expected: wrote(),
                },
                Step {
                    case: "a read of what that write stored",
                    operation: "get_item",
                    arguments: get_item_input(TABLE_OPEN, &inside(slug)),
                    expected: read_item(&inside(slug), slug),
                },
                Step {
                    case: "a write of ANOTHER partition, which an unscoped grant covers too",
                    operation: "put_item",
                    arguments: put_item_input(TABLE_OPEN, &outside(slug), slug),
                    expected: wrote(),
                },
                Step {
                    case: "a read of that second partition",
                    operation: "get_item",
                    arguments: get_item_input(TABLE_OPEN, &outside(slug)),
                    expected: read_item(&outside(slug), slug),
                },
            ],
        }
    };
    let scoped = {
        let slug = "scoped";
        Group {
            case: "a role whose grant is scoped to one partition by a leading-key condition",
            slug,
            statements: json!([
                allow_in_partitions(
                    &["PutItem", "GetItem"],
                    open_arn,
                    "ForAllValues",
                    &[inside(slug)],
                ),
                logs_only(),
            ]),
            steps: vec![
                Step {
                    case: "a write inside the partition the condition selects",
                    operation: "put_item",
                    arguments: put_item_input(TABLE_OPEN, &inside(slug), slug),
                    expected: wrote(),
                },
                Step {
                    case: "a read inside that partition",
                    operation: "get_item",
                    arguments: get_item_input(TABLE_OPEN, &inside(slug)),
                    expected: read_item(&inside(slug), slug),
                },
                Step {
                    case: "a write of a partition the condition does not select",
                    operation: "put_item",
                    arguments: put_item_input(TABLE_OPEN, &outside(slug), slug),
                    expected: refused(slug, PUT_ITEM, open_arn),
                },
                Step {
                    case: "a read of that partition",
                    operation: "get_item",
                    arguments: get_item_input(TABLE_OPEN, &outside(slug)),
                    expected: refused(slug, GET_ITEM, open_arn),
                },
            ],
        }
    };
    let listed = {
        let slug = "listed";
        // The same key under the other set qualifier, over a list of two partitions: the
        // second of them is addressed too, so the grant is pinned on more than its first
        // entry.
        let second = second(slug);
        Group {
            case: "a role whose grant lists two partitions under the any-value qualifier",
            slug,
            statements: json!([
                allow_in_partitions(
                    &["PutItem", "GetItem"],
                    open_arn,
                    "ForAnyValue",
                    &[inside(slug), second.clone()],
                ),
                logs_only(),
            ]),
            steps: vec![
                Step {
                    case: "a write of the first listed partition",
                    operation: "put_item",
                    arguments: put_item_input(TABLE_OPEN, &inside(slug), slug),
                    expected: wrote(),
                },
                Step {
                    case: "a write of the second listed partition",
                    operation: "put_item",
                    arguments: put_item_input(TABLE_OPEN, &second, slug),
                    expected: wrote(),
                },
                Step {
                    case: "a read of the second listed partition",
                    operation: "get_item",
                    arguments: get_item_input(TABLE_OPEN, &second),
                    expected: read_item(&second, slug),
                },
                Step {
                    case: "a write of a partition neither entry lists",
                    operation: "put_item",
                    arguments: put_item_input(TABLE_OPEN, &outside(slug), slug),
                    expected: refused(slug, PUT_ITEM, open_arn),
                },
            ],
        }
    };
    let shared = {
        let slug = SHARED_SLUG;
        Group {
            case: "a role granted nothing of its own, against a table whose policy grants it \
                   the read",
            slug,
            statements: json!([logs_only()]),
            steps: vec![
                Step {
                    case: "the read the table's own policy allows",
                    operation: "get_item",
                    arguments: get_item_input(TABLE_SHARED, &inside(slug)),
                    expected: read_item(&inside(slug), SEEDED),
                },
                Step {
                    case: "a write that policy does not allow",
                    operation: "put_item",
                    arguments: put_item_input(TABLE_SHARED, &inside(slug), slug),
                    expected: refused(slug, PUT_ITEM, shared_arn),
                },
            ],
        }
    };
    let ungranted = {
        let slug = "ungranted";
        Group {
            case: "a role and a table that grant the call nothing",
            slug,
            statements: json!([logs_only()]),
            steps: vec![
                Step {
                    case: "a write nothing in force allows",
                    operation: "put_item",
                    arguments: put_item_input(TABLE_OPEN, &inside(slug), slug),
                    expected: refused(slug, PUT_ITEM, open_arn),
                },
                Step {
                    case: "a read nothing in force allows",
                    operation: "get_item",
                    arguments: get_item_input(TABLE_OPEN, &inside(slug)),
                    expected: refused(slug, GET_ITEM, open_arn),
                },
            ],
        }
    };
    vec![unscoped, scoped, listed, shared, ungranted]
}

/// Create the function of one group, running the one handler as `role_arn`.
async fn given_function(router: &MultiServiceRouter, case: &str, name: &str, role_arn: &str) {
    let created = call(
        router,
        "POST",
        FUNCTIONS_URI,
        &json!({
            "FunctionName": name,
            "Role": role_arn,
            "Runtime": RUNTIME,
            "Handler": HANDLER,
            "Timeout": 30,
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD
                    .encode(package(&[("lambda_function.py", HANDLER_SOURCE)])),
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

/// What a handler's SDK call may do is decided by the policies in force on its execution role
/// and on the table it addresses — and what the service answered, served or refused, is what
/// the handler read back.
#[tokio::test]
async fn the_policies_in_force_decide_a_handlers_own_sdk_call() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    let case = "a python handler's own SDK call, under the policies in force";
    let router = emulator();

    // The ARNs the grants name, known before the resources exist because one table is created
    // carrying a policy about itself; each is asserted against what the service persisted when
    // that table is made.
    let open_arn = table_arn(TABLE_OPEN);
    let shared_arn = table_arn(TABLE_SHARED);
    let groups = groups(&open_arn, &shared_arn);

    // The roles first: a resource-based policy holds its principals by the identity behind
    // them, so the role the shared table's policy names has to exist before that table is
    // created with it.
    let mut role_arns: Vec<String> = Vec::with_capacity(groups.len());
    for group in &groups {
        role_arns.push(
            given_role(
                &router,
                group.case,
                &role_of(group.slug),
                Some(group.statements.clone()),
            )
            .await,
        );
    }
    let shared_role = groups
        .iter()
        .zip(&role_arns)
        .find(|(group, _)| group.slug == SHARED_SLUG)
        .map(|(_, role_arn)| role_arn.clone())
        .unwrap_or_else(|| panic!("case {case:?}: the group addressing the shared table"));

    let open_made = given_plain_table(&router, case, TABLE_OPEN).await;
    assert_eq!(
        open_made, open_arn,
        "case {case:?}: the ARN the grants name must be the one DynamoDB persisted"
    );
    let shared_made = given_table_with_policy(
        &router,
        case,
        TABLE_SHARED,
        &read_granting_policy(&shared_role, &shared_arn),
    )
    .await;
    assert_eq!(
        shared_made, shared_arn,
        "case {case:?}: the ARN the table's own policy names must be the one DynamoDB \
         persisted"
    );
    // The item the resource-policy group reads, written by the table's OWNER so what that
    // group's read answers was not put there by the group itself.
    dynamodb_ok(
        &router,
        case,
        "PutItem",
        &put_item_input(TABLE_SHARED, &inside(SHARED_SLUG), SEEDED),
    )
    .await;

    // Every function first, so the provisioning spans the clock owes run down together rather
    // than one after another.
    for (group, role_arn) in groups.iter().zip(&role_arns) {
        given_function(&router, group.case, &function_of(group.slug), role_arn).await;
    }
    for group in &groups {
        await_function_active(&router, group.case, &function_of(group.slug)).await;
    }

    for group in &groups {
        let steps: Vec<Value> = group
            .steps
            .iter()
            .map(|step| {
                json!({ "operation": step.operation, "arguments": step.arguments })
            })
            .collect();
        let answered = call(
            &router,
            "POST",
            &format!("{FUNCTIONS_URI}/{}/invocations", function_of(group.slug)),
            &json!({ "steps": steps }),
        )
        .await;
        assert_eq!(
            answered.status,
            200,
            "case {:?}: expected HTTP 200 from Invoke, got {} {}",
            group.case,
            answered.status,
            answered.text()
        );
        let payload = answered.json();
        // The handler RAN: a runtime that could not import the SDK fails the initialization
        // phase, which Invoke reports as a function error instead of a result.
        assert_eq!(
            answered.header("x-amz-function-error"),
            None,
            "case {:?}: expected the handler's own result, got a function error with {payload}",
            group.case
        );
        assert_eq!(
            payload["results"].as_array().map(Vec::len),
            Some(group.steps.len()),
            "case {:?}: expected one result per step, got {payload}",
            group.case
        );
        for (index, step) in group.steps.iter().enumerate() {
            let actual = payload["results"][index].clone();
            assert_eq!(
                actual, step.expected,
                "case {:?} / {:?}: expected {}, got {actual} (the whole run: {payload})",
                group.case, step.case, step.expected
            );
        }
    }

    // What DynamoDB ITSELF holds, read where a client reads it: every partition a step wrote,
    // and every partition a refused step must have left alone.
    let held: &[(&str, &str, String, Option<String>)] = &[
        (
            "the unscoped group's first partition",
            TABLE_OPEN,
            inside("unscoped"),
            Some("unscoped".to_string()),
        ),
        (
            "the unscoped group's second partition",
            TABLE_OPEN,
            outside("unscoped"),
            Some("unscoped".to_string()),
        ),
        (
            "the scoped group's selected partition",
            TABLE_OPEN,
            inside("scoped"),
            Some("scoped".to_string()),
        ),
        (
            "the partition the scoped group's condition does not select",
            TABLE_OPEN,
            outside("scoped"),
            None,
        ),
        (
            "the listed group's first listed partition",
            TABLE_OPEN,
            inside("listed"),
            Some("listed".to_string()),
        ),
        (
            "the listed group's second listed partition",
            TABLE_OPEN,
            second("listed"),
            Some("listed".to_string()),
        ),
        (
            "the partition the listed group's condition lists nowhere",
            TABLE_OPEN,
            outside("listed"),
            None,
        ),
        (
            "the ungranted group's partition",
            TABLE_OPEN,
            inside("ungranted"),
            None,
        ),
        (
            "the shared table's item, whose owner wrote it",
            TABLE_SHARED,
            inside(SHARED_SLUG),
            Some(SEEDED.to_string()),
        ),
    ];
    for (row, table, partition, value) in held {
        let read = dynamodb_ok(&router, row, "GetItem", &get_item_input(table, partition)).await;
        match value {
            Some(value) => assert_eq!(
                read["Item"],
                item(partition, value),
                "case {case:?} / {row:?}: expected DynamoDB to hold the item the handler \
                 wrote, got {read}"
            ),
            None => assert_eq!(
                read.get("Item"),
                None,
                "case {case:?} / {row:?}: expected the refused write to have stored nothing, \
                 got {read}"
            ),
        }
    }
}
