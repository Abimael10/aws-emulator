// SPDX-License-Identifier: CC0-1.0

//! The deployment a default-synthesizer CDK app of two stacks sharing an export makes, end to
//! end: the change-set pair per stack, with the analytics resource that synthesizer adds to
//! every stack it writes.
//!
//! The rule under test: **a resource type CloudFormation serves ITSELF is accepted by every
//! stage of the change-set deployment path and deployed inertly, so the stack carrying it
//! reaches its terminal create status with that resource among its own, and the resources the
//! same template declares are created in the services that OWN them — including, for a stack
//! whose template imports another's export, a value that resolves in the owning service to
//! the very resource the exporting stack created.** A template-format refusal of such a type
//! is a refusal of the whole deployment at its first call: the plan is never offered, so
//! neither stack is created and none of the resources the app declared is reached.
//!
//! Three crates have to agree for that to hold — CloudFormation validates the templates,
//! executes the plans and resolves the import, Amazon DynamoDB owns the table and is the only
//! side that can say whether it exists and serves data, and AWS Systems Manager holds the
//! parameter the importing stack writes the value into — so the suite lives here rather than
//! in any of them: a CloudFormation-only test can see the stack status and the published id
//! but not whether anything is behind them.
//!
//! The rows differ along what the rule generalises over — how the self-served resource is
//! declared (with its version report, with no properties at all, guarded by a condition that
//! holds and by one that does not), the logical id it is declared under, whether the
//! producer's table is named by the template or by CloudFormation, and where the importing
//! stack puts the value it imported (a parameter of its own, an output of its own). The
//! recorded deployment is one row of that.
//!
//! The awsQuery caller, the change-set wait, the stack-status wait, the XML readers and the
//! awsJson callers for DynamoDB and AWS Systems Manager are each written ONCE in this test
//! crate and shared, so this suite adds no wire caller and no waiter of its own.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_cloudformation_cloudformation::state::resource_types::CDK_METADATA;
use service_com_amazonaws_cloudformation_cloudformation::state::{
    change_set_status, change_set_type, execution_status, resource_status, status,
};

use crate::cfn_create_change_set_deploys_its_stack::{settled_change_set, CHANGE_SET};
use crate::cloudformation_apigatewayv2_resources::{cloudformation, REGION};
use crate::cloudformation_dynamodb_table_in_place_update::{described_table, dynamodb};
use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::cloudformation_lambda_ssm_sourced_update::ssm;
use crate::query_wire::{elements, opt_element, outputs_of, pairs_of, physical_ids_of};

/// The logical id the AWS CDK gives the analytics resource it adds to a stack it synthesizes.
const CDK_LOGICAL_ID: &str = "CDKMetadata";

/// The name of the condition the AWS CDK guards that resource with.
const CDK_CONDITION: &str = "CDKMetadataAvailable";

/// The property the version report is carried in, and a value of the form the CDK encodes
/// that report into. Source: the AWS CDK Developer Guide's "Version reporting" section, which
/// is where this resource's one property is published.
const ANALYTICS_PROPERTY: &str = "Analytics";
const ANALYTICS: &str = "v2:deflate64:H4sIAAAAAAAA";

/// The resource types the two stacks declare beside the analytics resource.
const TABLE_TYPE: &str = "AWS::DynamoDB::Table";
const PARAMETER_TYPE: &str = "AWS::SSM::Parameter";

/// The logical ids those two resources are declared under.
const TABLE_LOGICAL_ID: &str = "Table";
const PARAMETER_LOGICAL_ID: &str = "TableNameParameter";

/// The output an importing stack publishes the value it imported through, where the row puts
/// the import there.
const IMPORTED_OUTPUT: &str = "ImportedTableName";

/// The partition key the producer's table is keyed on, and so the only attribute the data
/// plane round trip below addresses.
const KEY: &str = "pk";

/// The `TableStatus` a table ready for use reports: "`ACTIVE` - The table is ready for use"
/// (`TableDescription$TableStatus`).
const ACTIVE: &str = "ACTIVE";

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

/// How a row's two templates declare the analytics resource.
#[derive(Clone, Copy, Debug)]
enum Analytics {
    /// With the version report, unguarded — what the default synthesizer writes.
    Reported,
    /// With no properties at all.
    Bare,
    /// Guarded by the condition the CDK emits, over a Region list that CARRIES the Region the
    /// request is signed for, so the guard holds.
    ConditionHolds,
    /// The same guard over a list that does not carry it: the stack completes, and reports no
    /// such resource.
    ConditionFails,
}

/// Where an importing stack puts the value it imported.
#[derive(Clone, Copy, Debug)]
enum Consumed {
    /// In an `AWS::SSM::Parameter` of its own.
    InAnSsmParameter,
    /// In an output of its own.
    InAnOutput,
}

/// One row: how both stacks declare the analytics resource, how the producer's table is named,
/// and where the consumer puts the import.
#[derive(Debug)]
struct Row {
    case: &'static str,
    analytics: Analytics,
    /// The logical id the analytics resource is declared under.
    logical_id: &'static str,
    /// The `TableName` the producer's template STATES, or `None` where naming the table is
    /// CloudFormation's — which is what a CDK `Table` with no `tableName` leaves it.
    declared_table_name: Option<&'static str>,
    consumed: Consumed,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            // The recorded shape: the version report in both stacks, the producer's table
            // unnamed, the import written into a parameter.
            case: "the version report in both stacks, imported into an SSM parameter",
            analytics: Analytics::Reported,
            logical_id: CDK_LOGICAL_ID,
            declared_table_name: None,
            consumed: Consumed::InAnSsmParameter,
        },
        Row {
            case: "the analytics resource guarded by a condition that holds, table named by \
                   the template",
            analytics: Analytics::ConditionHolds,
            logical_id: CDK_LOGICAL_ID,
            declared_table_name: Some("cdk-shared-export-guarded"),
            consumed: Consumed::InAnSsmParameter,
        },
        Row {
            case: "a guard that does not hold, so each stack carries no analytics resource",
            analytics: Analytics::ConditionFails,
            logical_id: CDK_LOGICAL_ID,
            declared_table_name: None,
            consumed: Consumed::InAnOutput,
        },
        Row {
            case: "the analytics resource declared with no properties, under another logical \
                   id",
            analytics: Analytics::Bare,
            logical_id: "CDKMetadata1",
            declared_table_name: Some("cdk-shared-export-bare"),
            consumed: Consumed::InAnOutput,
        },
    ]
}

/// The Region list a guarded row writes its condition over, or `None` where the row's
/// declaration is unguarded.
fn guarded_regions(analytics: Analytics) -> Option<&'static [&'static str]> {
    match analytics {
        Analytics::Reported | Analytics::Bare => None,
        Analytics::ConditionHolds => Some(&[REGION, "eu-west-1"]),
        Analytics::ConditionFails => Some(&["eu-west-1", "ap-south-1"]),
    }
}

/// A `Fn::Or` of `Fn::Equals` over `AWS::Region`, as the CDK emits its condition: true for a
/// list carrying the Region the request is signed for, false for one that does not.
fn region_condition(regions: &[&str]) -> Value {
    json!({
        "Fn::Or": regions
            .iter()
            .map(|region| json!({ "Fn::Equals": [{ "Ref": "AWS::Region" }, region] }))
            .collect::<Vec<Value>>()
    })
}

/// `document` with the row's analytics resource added to it, and the `Conditions` section a
/// guarded row needs.
fn with_analytics(row: &Row, mut document: Value) -> Value {
    let mut declaration = json!({ "Type": CDK_METADATA });
    if !matches!(row.analytics, Analytics::Bare) {
        declaration["Properties"] = json!({ ANALYTICS_PROPERTY: ANALYTICS });
    }
    if let Some(regions) = guarded_regions(row.analytics) {
        declaration["Condition"] = json!(CDK_CONDITION);
        document["Conditions"] = json!({ CDK_CONDITION: region_condition(regions) });
    }
    document["Resources"][row.logical_id] = declaration;
    document
}

/// The producing stack's template: the table the app declares, its `Ref` exported under
/// `export_name`, and the row's analytics resource.
fn producer_template(row: &Row, export_name: &str) -> String {
    let mut properties = json!({
        "KeySchema": [{"AttributeName": KEY, "KeyType": "HASH"}],
        "AttributeDefinitions": [{"AttributeName": KEY, "AttributeType": "S"}],
        "BillingMode": "PAY_PER_REQUEST",
    });
    if let Some(name) = row.declared_table_name {
        properties["TableName"] = json!(name);
    }
    with_analytics(
        row,
        json!({
            "AWSTemplateFormatVersion": "2010-09-09",
            "Resources": {
                TABLE_LOGICAL_ID: {"Type": TABLE_TYPE, "Properties": properties},
            },
            "Outputs": {
                "ExportsOutputRefTable": {
                    "Value": {"Ref": TABLE_LOGICAL_ID},
                    "Export": {"Name": export_name},
                },
            },
        }),
    )
    .to_string()
}

/// The consuming stack's template: a parameter holding the import, or one holding a literal
/// with the import published as the consumer's own output, plus the row's analytics resource.
fn consumer_template(row: &Row, export_name: &str, parameter_name: &str) -> String {
    let (value, outputs) = match row.consumed {
        Consumed::InAnSsmParameter => (json!({"Fn::ImportValue": export_name}), json!({})),
        Consumed::InAnOutput => (
            json!("not-the-import"),
            json!({IMPORTED_OUTPUT: {"Value": {"Fn::ImportValue": export_name}}}),
        ),
    };
    with_analytics(
        row,
        json!({
            "AWSTemplateFormatVersion": "2010-09-09",
            "Resources": {
                PARAMETER_LOGICAL_ID: {
                    "Type": PARAMETER_TYPE,
                    "Properties": {"Name": parameter_name, "Type": "String", "Value": value},
                },
            },
            "Outputs": outputs,
        }),
    )
    .to_string()
}

/// Deploy one stack the way a deploying tool does — `CreateChangeSet(ChangeSetType=CREATE)`,
/// a wait for the plan, `ExecuteChangeSet`, a wait for the stack's terminal create status —
/// and answer the settled `DescribeStacks` body with the `DescribeStackResources` one.
async fn deploy(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    template: &str,
) -> (String, String) {
    let (http_status, created) = cloudformation(
        router,
        "CreateChangeSet",
        &[
            ("StackName", stack),
            ("ChangeSetName", CHANGE_SET),
            ("ChangeSetType", change_set_type::CREATE),
            ("TemplateBody", template),
        ],
    )
    .await;
    assert_eq!(
        http_status, 200,
        "case {case:?}: expected HTTP 200 from CreateChangeSet for {stack:?}, got {http_status} \
         with {created}"
    );
    assert!(
        opt_element(&created, "Id").is_some_and(|id| !id.is_empty()),
        "case {case:?}: CreateChangeSet must answer the plan's Id for {stack:?}, got {created}"
    );
    let (plan_status, execution, planned) = settled_change_set(router, case, stack).await;
    assert_eq!(
        (plan_status.as_str(), execution.as_str()),
        (
            change_set_status::CREATE_COMPLETE,
            execution_status::AVAILABLE
        ),
        "case {case:?}: the plan for {stack:?} must be offered for execution, got {planned}"
    );

    // Before the execute the stack exists and holds nothing, so the deployment's resources
    // are the EXECUTE's work and not the plan's.
    let (http_status, in_review) =
        cloudformation(router, "DescribeStacks", &[("StackName", stack)]).await;
    assert_eq!(
        (http_status, opt_element(&in_review, "StackStatus")),
        (200, Some(status::REVIEW_IN_PROGRESS.to_string())),
        "case {case:?}: a CREATE plan must leave {stack:?} in review, got {in_review}"
    );

    let (http_status, executed) = cloudformation(
        router,
        "ExecuteChangeSet",
        &[("StackName", stack), ("ChangeSetName", CHANGE_SET)],
    )
    .await;
    assert_eq!(
        http_status, 200,
        "case {case:?}: expected HTTP 200 from ExecuteChangeSet for {stack:?}, got \
         {http_status} with {executed}"
    );
    let (stack_status, settled) =
        wait_for_stack_status(router, stack, status::CREATE_COMPLETE).await;
    let reasons = if stack_status == status::CREATE_COMPLETE {
        Vec::new()
    } else {
        let (_, events) =
            cloudformation(router, "DescribeStackEvents", &[("StackName", stack)]).await;
        elements(&events, "ResourceStatusReason")
    };
    assert_eq!(
        stack_status,
        status::CREATE_COMPLETE,
        "case {case:?}: the executed plan must settle {stack:?} {}, got {settled} with resource \
         failures {reasons:?}",
        status::CREATE_COMPLETE
    );

    let (http_status, resources) =
        cloudformation(router, "DescribeStackResources", &[("StackName", stack)]).await;
    assert_eq!(
        http_status, 200,
        "case {case:?}: expected HTTP 200 from DescribeStackResources for {stack:?}, got \
         {http_status} with {resources}"
    );
    (settled, resources)
}

/// What a deployed stack owes for the analytics resource its template declared: it is reported
/// under the type and logical id the template declared, in the create's terminal resource
/// status and with a physical id of its own — or reported by nothing at all, where the row's
/// guard does not hold.
fn assert_analytics_resource(case: &str, row: &Row, stack: &str, resources: &str) {
    let types = pairs_of(resources, "LogicalResourceId", "ResourceType");
    let logical_id = row.logical_id;
    if matches!(row.analytics, Analytics::ConditionFails) {
        assert!(
            !types.values().any(|declared| declared == CDK_METADATA),
            "case {case:?}: {stack:?} must report no {CDK_METADATA} resource for a guard that \
             does not hold, got {resources}"
        );
        return;
    }
    assert_eq!(
        types.get(logical_id).map(String::as_str),
        Some(CDK_METADATA),
        "case {case:?}: {stack:?} must report {logical_id:?} under {CDK_METADATA}, got \
         {resources}"
    );
    assert_eq!(
        pairs_of(resources, "LogicalResourceId", "ResourceStatus")
            .get(logical_id)
            .map(String::as_str),
        Some(resource_status::CREATE_COMPLETE),
        "case {case:?}: {stack:?} must report {logical_id:?} {}, got {resources}",
        resource_status::CREATE_COMPLETE
    );
    assert!(
        physical_ids_of(resources)
            .get(logical_id)
            .is_some_and(|id| !id.is_empty()),
        "case {case:?}: {stack:?} must publish a physical id for {logical_id:?}, got {resources}"
    );
}

/// RULE: a template declaring a resource type CloudFormation serves itself deploys through the
/// change-set path, that resource is reported among the stack's own, and the resources the
/// template declares are real in the services that own them — across the export one stack
/// publishes and another imports.
#[tokio::test]
async fn both_stacks_of_a_default_synthesizer_app_deploy_through_their_change_sets() {
    for (index, row) in rows().into_iter().enumerate() {
        // A router of its own per row: every row deploys into the one account and Region its
        // engine holds, and what each case asserts is about stacks nothing else has acted on.
        let router = build_router();
        let case = row.case;
        let producer = format!("cdk-shared-export-producer-{index}");
        let consumer = format!("cdk-shared-export-consumer-{index}");
        // The export name shape the CDK gives an output it generates for a cross-stack
        // reference.
        let export_name = format!("{producer}:ExportsOutputRefTable");
        let parameter_name = format!("/cdk-shared-export/{index}/table-name");

        // ---- The producing stack. ----
        let (_, producer_resources) = deploy(
            &router,
            case,
            &producer,
            &producer_template(&row, &export_name),
        )
        .await;
        assert_analytics_resource(case, &row, &producer, &producer_resources);

        let table = physical_ids_of(&producer_resources)
            .get(TABLE_LOGICAL_ID)
            .cloned()
            .unwrap_or_default();
        match row.declared_table_name {
            Some(name) => assert_eq!(
                table, name,
                "case {case:?}: the stack must publish the declared table name, got \
                 {producer_resources}"
            ),
            None => assert!(
                !table.is_empty() && table != TABLE_LOGICAL_ID,
                "case {case:?}: the stack must publish a generated table name, got {table:?} in \
                 {producer_resources}"
            ),
        }

        // The id it published is a table DynamoDB holds, ready for use, and it serves data —
        // which is what says a real table is behind the deployment rather than a record of one.
        let (http_status, answered) = described_table(&router, &table).await;
        assert_eq!(
            (http_status, answered.pointer("/Table/TableStatus")),
            (200, Some(&json!(ACTIVE))),
            "case {case:?}: DynamoDB must report {table:?} {ACTIVE}, got {http_status} with \
             {answered}"
        );
        let (http_status, written) = dynamodb(
            &router,
            "PutItem",
            &json!({"TableName": table, "Item": {KEY: {"S": case}}}),
        )
        .await;
        assert_eq!(
            http_status, 200,
            "case {case:?}: expected {table:?} to take an item write, got {http_status} with \
             {written}"
        );

        // The export the producing stack publishes carries that same id.
        let (http_status, exports) = cloudformation(&router, "ListExports", &[]).await;
        assert_eq!(
            http_status, 200,
            "case {case:?}: expected HTTP 200 from ListExports, got {http_status} with {exports}"
        );
        assert_eq!(
            pairs_of(&exports, "Name", "Value")
                .get(&export_name)
                .map(String::as_str),
            Some(table.as_str()),
            "case {case:?}: expected the export {export_name:?} to carry {table:?}, got {exports}"
        );

        // ---- The consuming stack, deployed the same way. ----
        let (consumer_described, consumer_resources) = deploy(
            &router,
            case,
            &consumer,
            &consumer_template(&row, &export_name, &parameter_name),
        )
        .await;
        assert_analytics_resource(case, &row, &consumer, &consumer_resources);

        let held = match row.consumed {
            Consumed::InAnSsmParameter => {
                let (http_status, read) =
                    ssm(&router, "GetParameter", &json!({"Name": parameter_name})).await;
                assert_eq!(
                    http_status, 200,
                    "case {case:?}: expected HTTP 200 from GetParameter for \
                     {parameter_name:?}, got {http_status} with {read}"
                );
                read.pointer("/Parameter/Value")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string()
            }
            Consumed::InAnOutput => outputs_of(&consumer_described)
                .get(IMPORTED_OUTPUT)
                .cloned()
                .unwrap_or_default(),
        };
        assert_eq!(
            held, table,
            "case {case:?}: expected the consuming stack to hold the exported table name \
             {table:?}, got {held:?}"
        );

        // The handle the consuming stack holds reaches the producer's table, item and all.
        let (http_status, read) = dynamodb(
            &router,
            "GetItem",
            &json!({
                "TableName": held,
                "Key": {KEY: {"S": case}},
                "ConsistentRead": true,
            }),
        )
        .await;
        assert_eq!(
            (http_status, read.pointer(&format!("/Item/{KEY}/S"))),
            (200, Some(&json!(case))),
            "case {case:?}: expected the imported name {held:?} to serve the producer's item \
             back, got {http_status} with {read}"
        );
    }
}
