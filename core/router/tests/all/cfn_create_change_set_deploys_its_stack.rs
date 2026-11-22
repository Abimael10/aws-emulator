// SPDX-License-Identifier: CC0-1.0

//! The change-set deployment path a CDK/CLI client takes for a stack that does not exist yet:
//! `CreateChangeSet(ChangeSetType=CREATE)` followed by `ExecuteChangeSet`.
//!
//! The rule under test: **a `CREATE` change set leaves the new stack in
//! `REVIEW_IN_PROGRESS` with nothing provisioned for it, and EXECUTING that change set opens
//! the stack's create, settles it in a terminal create status, installs the executed change
//! set's template as the stack's (so `GetTemplate` serves that body afterwards) and creates
//! every resource the template declares in the service that OWNS it.** An accepted execute
//! that leaves the stack in review is no execute: the deploying tool then has no terminal
//! status to wait on and the deployment aborts with nothing provisioned.
//!
//! What a template read answers WHILE the stack is in review is
//! `change_set_created_stack_template_reads.rs`'s rule, in the CloudFormation crate; this
//! suite is about what the EXECUTE does.
//!
//! Two crates have to agree for that to hold — CloudFormation executes the change set and
//! publishes the physical ids, Amazon DynamoDB owns the tables and is the only side that can
//! say whether one exists and is usable — so the suite lives here rather than in either
//! crate.
//!
//! The rows differ along what the rule generalises over: how many resources one template
//! declares, whether the declaration STATES the resource's name or leaves it to
//! CloudFormation, the read/write mode the table is declared with, and whether the template
//! publishes outputs over the resources. The recorded case (one named table, one resource) is
//! one row of it.
//!
//! The awsQuery caller, the poll that waits for a stack to first report a status, the XML
//! readers and the awsJson caller for DynamoDB are each written ONCE in this test crate and
//! shared, so this suite adds no wire caller of its own. The one poll it does add is over
//! `DescribeChangeSet`, bounded by a generous WALL-CLOCK deadline rather than by a count of
//! reads: what a case asserts is the CONDITION (a change set that settled), never how long
//! the host took to get there.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_apigatewayv2_resources::{cloudformation, REGION};
use crate::cloudformation_dynamodb_table_in_place_update::{described_table, dynamodb};
use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::{elements, opt_element, outputs_of, physical_ids_of};

/// The `StackStatus` a stack a `CREATE` change set brought into existence reports before that
/// change set is executed: "CloudFormation creates a stack with a unique stack ID, but no
/// template or resources".
const REVIEW_IN_PROGRESS: &str = "REVIEW_IN_PROGRESS";

/// The terminal status the create the execute opens settles in.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The `TableStatus` a table ready for use reports: "`ACTIVE` - The table is ready for use"
/// (`TableDescription$TableStatus`).
const ACTIVE: &str = "ACTIVE";

/// The resource type every row declares.
const TABLE_TYPE: &str = "AWS::DynamoDB::Table";

/// The partition key every declared table carries, and so the only attribute the data-plane
/// round trip below addresses.
const KEY: &str = "pk";

/// The change-set name every row creates its plan under — one name per stack, the way a
/// deploying tool reuses its own.
///
/// `pub(crate)` with [`settled_change_set`]: a sibling suite that deploys through the same
/// change-set pair names its plans the same way and waits on them with the same poller,
/// rather than carrying a second copy that can drift from this one.
pub(crate) const CHANGE_SET: &str = "deploy";

/// How long the change-set poll below waits for a creation to settle. Generous: the condition
/// it waits for is the change set's own settled status, which the service owes from its own
/// clock, so a loaded host cannot decide a case.
const SETTLE_DEADLINE: Duration = Duration::from_secs(60);
/// See [`SETTLE_DEADLINE`].
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The emulator with the two services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them.
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

/// One table a row's template declares.
struct Declared {
    /// The logical id it is declared under.
    logical_id: &'static str,
    /// The `TableName` the template STATES, or `None` where naming the table is
    /// CloudFormation's.
    declared_name: Option<&'static str>,
    /// The read/write mode the declaration asks for, as the properties stating it.
    mode: fn() -> Value,
}

/// A `PROVISIONED` table's mode properties.
fn provisioned() -> Value {
    json!({
        "BillingMode": "PROVISIONED",
        "ProvisionedThroughput": {"ReadCapacityUnits": 1, "WriteCapacityUnits": 1},
    })
}

/// An on-demand table's mode properties, which state no capacity at all.
fn on_demand() -> Value {
    json!({"BillingMode": "PAY_PER_REQUEST"})
}

/// One row: the stack a change set creates, what its template declares, and whether that
/// template publishes an output per declared resource.
struct Row {
    case: &'static str,
    stack: &'static str,
    tables: Vec<Declared>,
    outputs: bool,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            // The recorded case: one table, named by the template.
            case: "one table the template names",
            stack: "ccs-deploy-named",
            tables: vec![Declared {
                logical_id: "Table",
                declared_name: Some("ccs-deploy-named-table"),
                mode: provisioned,
            }],
            outputs: false,
        },
        Row {
            case: "one table whose name CloudFormation generates, published as an output",
            stack: "ccs-deploy-generated",
            tables: vec![Declared {
                logical_id: "Table",
                declared_name: None,
                mode: provisioned,
            }],
            outputs: true,
        },
        Row {
            case: "one on-demand table",
            stack: "ccs-deploy-on-demand",
            tables: vec![Declared {
                logical_id: "Table",
                declared_name: Some("ccs-deploy-on-demand-table"),
                mode: on_demand,
            }],
            outputs: false,
        },
        Row {
            case: "two tables, one named and one generated, both published",
            stack: "ccs-deploy-two",
            tables: vec![
                Declared {
                    logical_id: "First",
                    declared_name: Some("ccs-deploy-two-first"),
                    mode: provisioned,
                },
                Declared {
                    logical_id: "Second",
                    declared_name: None,
                    mode: on_demand,
                },
            ],
            outputs: true,
        },
    ]
}

/// The template a row submits: one `AWS::DynamoDB::Table` per declaration, each keyed on
/// [`KEY`], and — where the row asks for them — one output per table carrying its `Ref`.
fn template(row: &Row) -> String {
    let mut resources = Map::new();
    for table in &row.tables {
        let mut properties = json!({
            "KeySchema": [{"AttributeName": KEY, "KeyType": "HASH"}],
            "AttributeDefinitions": [{"AttributeName": KEY, "AttributeType": "S"}],
        });
        for (member, value) in (table.mode)()
            .as_object()
            .expect("a mode is stated as properties")
        {
            properties[member] = value.clone();
        }
        if let Some(name) = table.declared_name {
            properties["TableName"] = json!(name);
        }
        resources.insert(
            table.logical_id.to_string(),
            json!({"Type": TABLE_TYPE, "Properties": properties}),
        );
    }
    let mut document = json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": Value::Object(resources),
    });
    if row.outputs {
        let mut outputs = Map::new();
        for table in &row.tables {
            outputs.insert(
                output_key(table.logical_id),
                json!({"Value": {"Ref": table.logical_id}}),
            );
        }
        document["Outputs"] = Value::Object(outputs);
    }
    document.to_string()
}

/// The output name a row publishes a table's `Ref` under.
fn output_key(logical_id: &str) -> String {
    format!("{logical_id}Ref")
}

/// The ARN Amazon DynamoDB holds the table called `name` under.
fn table_arn(name: &str) -> String {
    format!(
        "arn:aws:dynamodb:{REGION}:{}:table/{name}",
        engine::emulator_account_id()
    )
}

/// Poll `DescribeChangeSet` the way a deploying tool's waiter does, and answer the status the
/// creation SETTLED on with the answer it was read from.
///
/// `pub(crate)` for the reason [`CHANGE_SET`] is: a suite deploying a stack through
/// `CreateChangeSet` + `ExecuteChangeSet` needs exactly this wait, on the CONDITION that the
/// creation settled rather than on how long the host took to settle it.
pub(crate) async fn settled_change_set(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
) -> (String, String, String) {
    let giving_up_at = Instant::now() + SETTLE_DEADLINE;
    loop {
        let (http_status, body) = cloudformation(
            router,
            "DescribeChangeSet",
            &[("StackName", stack), ("ChangeSetName", CHANGE_SET)],
        )
        .await;
        assert_eq!(
            http_status, 200,
            "case {case:?}: expected HTTP 200 from DescribeChangeSet, got {http_status} with \
             {body}"
        );
        let status = opt_element(&body, "Status").unwrap_or_default();
        let execution = opt_element(&body, "ExecutionStatus").unwrap_or_default();
        if status == "CREATE_COMPLETE" || status == "FAILED" {
            return (status, execution, body);
        }
        assert!(
            Instant::now() < giving_up_at,
            "case {case:?}: expected the change set of {stack} to settle, last saw \
             {status:?}/{execution:?} in {body}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// RULE: a `CREATE` change set leaves its new stack in review with nothing provisioned, and
/// executing it settles the stack in a terminal create status, installs the executed template
/// as the stack's and creates the declared resources in the service that owns them.
#[tokio::test]
async fn an_executed_create_change_set_deploys_the_stack_it_left_in_review() {
    for row in rows() {
        let case = row.case;
        // A router of its own per row: every row deploys into the one account and Region its
        // engine holds, and what each case asserts is about a stack nothing else has acted on.
        let router = build_router();
        let body = template(&row);

        // ---- The plan: a CREATE change set over a stack that does not exist yet. ----
        let (http_status, created) = cloudformation(
            &router,
            "CreateChangeSet",
            &[
                ("StackName", row.stack),
                ("ChangeSetName", CHANGE_SET),
                ("ChangeSetType", "CREATE"),
                ("TemplateBody", &body),
            ],
        )
        .await;
        assert_eq!(
            http_status, 200,
            "case {case:?}: expected HTTP 200 from CreateChangeSet, got {http_status} with \
             {created}"
        );
        assert!(
            opt_element(&created, "Id").is_some_and(|id| !id.is_empty()),
            "case {case:?}: CreateChangeSet must answer the change set's Id, got {created}"
        );
        let (status, execution, described) = settled_change_set(&router, case, row.stack).await;
        assert_eq!(
            (status.as_str(), execution.as_str()),
            ("CREATE_COMPLETE", "AVAILABLE"),
            "case {case:?}: the plan must be offered for execution, got {described}"
        );

        // ---- Before the execute: a stack in review, with nothing provisioned for it. ----
        let (http_status, stacks) =
            cloudformation(&router, "DescribeStacks", &[("StackName", row.stack)]).await;
        assert_eq!(
            http_status, 200,
            "case {case:?}: expected HTTP 200 from DescribeStacks, got {http_status} with {stacks}"
        );
        assert_eq!(
            opt_element(&stacks, "StackStatus").as_deref(),
            Some(REVIEW_IN_PROGRESS),
            "case {case:?}: a CREATE change set leaves the stack in review, got {stacks}"
        );
        // A name the plan would create under is still one DynamoDB holds no table for: a
        // change set that was not executed has deployed nothing.
        for table in row.tables.iter().filter_map(|table| table.declared_name) {
            let (http_status, answered) = described_table(&router, table).await;
            assert_ne!(
                http_status, 200,
                "case {case:?}: nothing may exist for {table} before the change set is \
                 executed, got {answered}"
            );
        }

        // ---- The execute, and the terminal status it settles the stack in. ----
        let (http_status, executed) = cloudformation(
            &router,
            "ExecuteChangeSet",
            &[("StackName", row.stack), ("ChangeSetName", CHANGE_SET)],
        )
        .await;
        assert_eq!(
            http_status, 200,
            "case {case:?}: expected HTTP 200 from ExecuteChangeSet, got {http_status} with \
             {executed}"
        );
        let (stack_status, settled) =
            wait_for_stack_status(&router, row.stack, CREATE_COMPLETE).await;
        let reasons = if stack_status == CREATE_COMPLETE {
            Vec::new()
        } else {
            let (_, events) =
                cloudformation(&router, "DescribeStackEvents", &[("StackName", row.stack)]).await;
            elements(&events, "ResourceStatusReason")
        };
        assert_eq!(
            stack_status, CREATE_COMPLETE,
            "case {case:?}: the executed CREATE change set must settle the stack \
             {CREATE_COMPLETE}, got {settled} with resource failures {reasons:?}"
        );

        // ---- The executed template is the stack's, at every stage a client reads. ----
        for stage in ["", "Original", "Processed"] {
            let mut members = vec![("StackName", row.stack)];
            if !stage.is_empty() {
                members.push(("TemplateStage", stage));
            }
            let (http_status, served) = cloudformation(&router, "GetTemplate", &members).await;
            assert_eq!(
                (
                    http_status,
                    elements(&served, "TemplateBody")
                        .first()
                        .cloned()
                        .unwrap_or_default()
                ),
                (200, body.clone()),
                "case {case:?}: GetTemplate(TemplateStage={stage:?}) must serve the executed \
                 template, got {http_status} with {served}"
            );
        }

        // ---- The resources it declared exist in the service that owns them. ----
        let (http_status, resources) =
            cloudformation(&router, "DescribeStackResources", &[("StackName", row.stack)]).await;
        assert_eq!(
            http_status, 200,
            "case {case:?}: expected HTTP 200 from DescribeStackResources, got {http_status} \
             with {resources}"
        );
        let published = physical_ids_of(&resources);
        let outputs = outputs_of(&settled);
        for table in &row.tables {
            let logical_id = table.logical_id;
            let id = published.get(logical_id).cloned().unwrap_or_default();
            match table.declared_name {
                Some(name) => assert_eq!(
                    id, name,
                    "case {case:?}: the stack must publish the declared name for {logical_id}, \
                     got {resources}"
                ),
                None => assert!(
                    !id.is_empty() && id != logical_id,
                    "case {case:?}: the stack must publish a generated name for {logical_id}, \
                     got {id:?} in {resources}"
                ),
            }
            // The id the stack hands its clients resolves in DynamoDB, under a table that is
            // ready for use.
            let (http_status, answered) = described_table(&router, &id).await;
            assert_eq!(
                http_status, 200,
                "case {case:?}: DynamoDB must resolve the id the completed stack published for \
                 {logical_id} ({id:?}), got {http_status} with {answered}"
            );
            assert_eq!(
                (
                    answered.pointer("/Table/TableName"),
                    answered.pointer("/Table/TableStatus"),
                    answered.pointer("/Table/TableArn"),
                ),
                (
                    Some(&json!(id)),
                    Some(&json!(ACTIVE)),
                    Some(&json!(table_arn(&id))),
                ),
                "case {case:?}: expected {id:?} to be an {ACTIVE} table of this account and \
                 Region, got {answered}"
            );
            // ...and it serves the data plane, which is what says a real table is behind the
            // id rather than a record of one.
            let (http_status, written) = dynamodb(
                &router,
                "PutItem",
                &json!({"TableName": id, "Item": {KEY: {"S": logical_id}}}),
            )
            .await;
            assert_eq!(
                http_status, 200,
                "case {case:?}: expected {id:?} to take an item write, got {http_status} with \
                 {written}"
            );
            let (http_status, read) = dynamodb(
                &router,
                "GetItem",
                &json!({
                    "TableName": id,
                    "Key": {KEY: {"S": logical_id}},
                    "ConsistentRead": true,
                }),
            )
            .await;
            assert_eq!(
                (http_status, read.pointer(&format!("/Item/{KEY}/S"))),
                (200, Some(&json!(logical_id))),
                "case {case:?}: expected {id:?} to serve the item back, got {http_status} with \
                 {read}"
            );
            // What the stack PUBLISHES for the resource is that same id.
            if row.outputs {
                assert_eq!(
                    outputs.get(&output_key(logical_id)),
                    Some(&id),
                    "case {case:?}: the completed stack must publish {id:?} for {logical_id}, \
                     got {outputs:?}"
                );
            }
        }
    }
}
