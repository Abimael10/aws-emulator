// SPDX-License-Identifier: CC0-1.0

//! The `cdk import` flow over the awsQuery wire, end to end: a stack deployed through a
//! CREATE change set, an existing DynamoDB table ADOPTED into it through an IMPORT change
//! set whose mapping names the table, and the deploy that follows with the app's stack-level
//! tags.
//!
//! RULE: a resource an IMPORT change set adopts becomes a resource the stack MANAGES — it
//! keeps the identity and the data it already had, nothing is created for it, and from then
//! on a stack operation acts on it as on any other resource the stack manages, so the
//! stack-level `Tags` a later operation states reach it in the service that owns it. A
//! mapping that names a resource the owning service does NOT hold is no plan at all: that
//! change set fails, with the diagnostic the type's own resource provider states.
//!
//! The template every phase carries is the shape the CDK CLI synthesizes around the construct
//! being imported — the bootstrap-version parameter and the rule over it, the metadata
//! resource and its condition — so the flow is not read as working only for a template that
//! holds nothing but the imported resource.

use serde_json::json;
use smithy_server_runtime::MultiServiceRouter;

use crate::change_set_lifecycle::{
    create_change_set, describe_change_set, execute_change_set, settle_the_store,
};
use crate::dynamic_references::put_parameter;
use crate::dynamodb_table_resources::{describe_table, dynamodb, table_tags};
use crate::harness::{member, router_with_engine, xml_text, xml_texts, REGION};
use crate::stack_lifecycle::describe_stack;

/// The logical ids the CDK gives the constructs of the app this flow is written for: the
/// `ddb.Table` being imported, and the metadata resource the CDK adds to every synthesized
/// stack.
const TABLE_LOGICAL_ID: &str = "TableCD117FA1";
const METADATA_LOGICAL_ID: &str = "CDKMetadata";

/// The name of the condition the CDK guards its metadata resource with.
const METADATA_CONDITION: &str = "CDKMetadataAvailable";

/// The Parameter Store parameter a `DefaultStackSynthesizer` stack reads the bootstrap
/// version from, and a version its own rule admits.
const BOOTSTRAP_VERSION_PARAMETER: &str = "/cdk-bootstrap/hnb659fds/version";
const BOOTSTRAP_VERSION: &str = "21";

/// The stack the app deploys, the table a client created for it to adopt, and the item that
/// client wrote to the table.
const STACK: &str = "cdk-imported-table";
const TABLE: &str = "cdk-imported-table-store";
const ITEM_KEY: &str = "order-1";
const ITEM_FIELD: &str = "written-by-the-client";

/// The stack-level tag the deploy that follows the import states.
const TAG_KEY: &str = "sim2real:prefix";
const TAG_VALUE: &str = "sim2real-scn";

/// A table the mapping could name that nothing ever created.
const ABSENT_TABLE: &str = "cdk-imported-table-absent";

/// The template the app synthesizes, with the table declared only once the construct has been
/// added to it — which is what `cdk import` submits.
fn cdk_template(with_table: bool) -> String {
    let mut resources = json!({
        METADATA_LOGICAL_ID: {
            "Type": "AWS::CDK::Metadata",
            "Properties": {"Analytics": "v2:deflate64:H4sIAAAAAAAA"},
            "Condition": METADATA_CONDITION,
        },
    });
    if with_table {
        resources
            .as_object_mut()
            .expect("the resources section is an object")
            .insert(
                TABLE_LOGICAL_ID.to_string(),
                json!({
                    "Type": "AWS::DynamoDB::Table",
                    "Properties": {
                        "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
                        "AttributeDefinitions": [{"AttributeName": "pk",
                                                  "AttributeType": "S"}],
                        "BillingMode": "PAY_PER_REQUEST",
                    },
                    // What the CDK gives a resource it imports: an import's template must
                    // state what happens to a resource the stack did not create.
                    "UpdateReplacePolicy": "Retain",
                    "DeletionPolicy": "Retain",
                    "Metadata": {"aws:cdk:path": format!("{STACK}/Table/Resource")},
                }),
            );
    }
    json!({
        "Parameters": {
            "BootstrapVersion": {
                "Type": "AWS::SSM::Parameter::Value<String>",
                "Default": BOOTSTRAP_VERSION_PARAMETER,
                "Description": "Version of the CDK Bootstrap resources in this environment",
            },
        },
        "Rules": {
            "CheckBootstrapVersion": {
                "Assertions": [{
                    "Assert": {"Fn::Not": [{"Fn::Contains": [
                        ["1", "2", "3", "4", "5"], {"Ref": "BootstrapVersion"},
                    ]}]},
                    "AssertDescription":
                        "CDK bootstrap stack version 6 required. Please run 'cdk bootstrap' \
                         with a recent version of the CDK CLI.",
                }],
            },
        },
        "Conditions": {
            METADATA_CONDITION: {"Fn::Or": [
                {"Fn::Equals": [{"Ref": "AWS::Region"}, REGION]},
                {"Fn::Equals": [{"Ref": "AWS::Region"}, "eu-west-1"]},
            ]},
        },
        "Resources": resources,
    })
    .to_string()
}

/// The `ResourcesToImport` members a mapping naming one table is sent as.
fn mapping(table: &str) -> Vec<(String, String)> {
    vec![
        member("ResourcesToImport.member.1.ResourceType", "AWS::DynamoDB::Table"),
        member("ResourcesToImport.member.1.LogicalResourceId", TABLE_LOGICAL_ID),
        member(
            "ResourcesToImport.member.1.ResourceIdentifier.entry.1.key",
            "TableName",
        ),
        member(
            "ResourcesToImport.member.1.ResourceIdentifier.entry.1.value",
            table,
        ),
    ]
}

/// The item DynamoDB holds under the client's key, or `None` where it holds none.
async fn item(router: &MultiServiceRouter) -> Option<String> {
    let (status, answered) = dynamodb(
        router,
        "GetItem",
        &json!({"TableName": TABLE, "Key": {"pk": {"S": ITEM_KEY}}}),
    )
    .await;
    if status != 200 {
        return None;
    }
    answered
        .pointer("/Item/field/S")
        .and_then(|value| value.as_str())
        .map(ToString::to_string)
}

/// RULE: the deploy, the import and the deploy that follows leave the stack managing the table
/// the client already had — under the identifier and with the data it already carried — and
/// the stack-level tags the last deploy stated on it; a mapping naming a table nothing holds
/// fails its change set with the provider's own diagnostic instead.
#[tokio::test]
async fn an_imported_table_is_managed_tagged_and_left_with_its_data() {
    let (router, engine) = router_with_engine();
    put_parameter(
        &router,
        "bootstrap",
        BOOTSTRAP_VERSION_PARAMETER,
        "String",
        BOOTSTRAP_VERSION,
    )
    .await;

    // The table a client created out of band, which nothing of CloudFormation's manages.
    let (code, answered) = dynamodb(
        &router,
        "CreateTable",
        &json!({
            "TableName": TABLE,
            "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
            "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
            "BillingMode": "PAY_PER_REQUEST",
        }),
    )
    .await;
    assert_eq!(
        code, 200,
        "the client's own CreateTable must be accepted, got {code} {answered}"
    );
    // "You can perform read and write operations only on an ACTIVE table": the client's write
    // is made once that service reports the table out of its own creation window, looked at
    // again until it does rather than after a fixed wait.
    for _ in 0..8 {
        let reported = describe_table(&router, TABLE)
            .await
            .and_then(|table| table.get("TableStatus").cloned());
        if reported.as_ref().and_then(|status| status.as_str()) == Some("ACTIVE") {
            break;
        }
        crate::harness::wait_for_the_transition().await;
    }
    let (code, answered) = dynamodb(
        &router,
        "PutItem",
        &json!({"TableName": TABLE,
                "Item": {"pk": {"S": ITEM_KEY}, "field": {"S": ITEM_FIELD}}}),
    )
    .await;
    assert_eq!(
        code, 200,
        "the client's own PutItem must be accepted, got {code} {answered}"
    );

    // `cdk deploy`: a CREATE change set over the app without the table, then its execution.
    let base = cdk_template(false);
    let (code, answered) = create_change_set(
        &router,
        STACK,
        "cdk-deploy-change-set",
        &[
            member("ChangeSetType", "CREATE"),
            member("TemplateBody", &base),
        ],
    )
    .await;
    assert_eq!(
        code, 200,
        "the first deploy's change set must be created, got {code} {answered}"
    );
    settle_the_store(&router, &engine).await;
    let (code, answered) = execute_change_set(&router, "cdk-deploy-change-set", Some(STACK)).await;
    assert_eq!(
        code, 200,
        "the first deploy must be executable, got {code} {answered}"
    );
    settle_the_store(&router, &engine).await;
    let (_, described) = describe_stack(&router, STACK).await;
    assert_eq!(
        xml_text(&described, "StackStatus").as_deref(),
        Some("CREATE_COMPLETE"),
        "the first deploy must complete, got {described}"
    );

    // A mapping naming a table nothing holds: no plan at all, failed with the diagnostic the
    // DynamoDB table's own resource provider states for a table it cannot read.
    let imported = cdk_template(true);
    let mut members = vec![
        member("ChangeSetType", "IMPORT"),
        member("TemplateBody", &imported),
    ];
    members.extend(mapping(ABSENT_TABLE));
    let (code, answered) =
        create_change_set(&router, STACK, "cdk-import-absent", &members).await;
    assert_eq!(
        code, 200,
        "the change set over a missing table is created, got {code} {answered}"
    );
    settle_the_store(&router, &engine).await;
    let (_, plan) = describe_change_set(&router, "cdk-import-absent", Some(STACK)).await;
    assert_eq!(
        (
            xml_text(&plan, "Status").unwrap_or_default(),
            xml_text(&plan, "ExecutionStatus").unwrap_or_default(),
            xml_text(&plan, "StatusReason").unwrap_or_default(),
        ),
        (
            "FAILED".to_string(),
            "UNAVAILABLE".to_string(),
            format!("Table: {ABSENT_TABLE} does not exist."),
        ),
        "a mapping naming a table nothing holds must fail with the table provider's own \
         diagnostic, got {plan}"
    );
    let (_, described) = describe_stack(&router, STACK).await;
    assert_eq!(
        xml_text(&described, "StackStatus").as_deref(),
        Some("CREATE_COMPLETE"),
        "the stack must keep the status it had, got {described}"
    );

    // `cdk import` of the table that IS there: an IMPORT change set naming it, then its
    // execution.
    let mut members = vec![
        member("ChangeSetType", "IMPORT"),
        member("TemplateBody", &imported),
    ];
    members.extend(mapping(TABLE));
    let (code, answered) =
        create_change_set(&router, STACK, "cdk-import-change-set", &members).await;
    assert_eq!(
        code, 200,
        "the import change set must be created, got {code} {answered}"
    );
    settle_the_store(&router, &engine).await;
    let (_, plan) = describe_change_set(&router, "cdk-import-change-set", Some(STACK)).await;
    assert_eq!(
        (
            xml_text(&plan, "Status").unwrap_or_default(),
            xml_text(&plan, "StatusReason").unwrap_or_default(),
            xml_texts(&plan, "Action"),
        ),
        (
            "CREATE_COMPLETE".to_string(),
            String::new(),
            vec!["Import".to_string()],
        ),
        "the plan over a table that IS there must adopt it, got {plan}"
    );
    let (code, answered) = execute_change_set(&router, "cdk-import-change-set", Some(STACK)).await;
    assert_eq!(
        code, 200,
        "the import must be executable, got {code} {answered}"
    );
    settle_the_store(&router, &engine).await;
    let (_, described) = describe_stack(&router, STACK).await;
    assert_eq!(
        xml_text(&described, "StackStatus").as_deref(),
        Some("IMPORT_COMPLETE"),
        "the executed import must complete as an import, got {described}"
    );
    let held = item(&router).await;
    assert_eq!(
        held.as_deref(),
        Some(ITEM_FIELD),
        "the item the client wrote must survive the adoption, got {held:?}"
    );

    // ...and the deploy that follows, which states the app's stack-level tags: the plan is one
    // `Modify` of the adopted resource's tags, and executing it puts them in force on the
    // table itself while leaving its data alone.
    let (code, answered) = create_change_set(
        &router,
        STACK,
        "cdk-tag-change-set",
        &[
            member("ChangeSetType", "UPDATE"),
            member("TemplateBody", &imported),
            member("Tags.member.1.Key", TAG_KEY),
            member("Tags.member.1.Value", TAG_VALUE),
        ],
    )
    .await;
    assert_eq!(
        code, 200,
        "the tagging deploy's change set must be created, got {code} {answered}"
    );
    settle_the_store(&router, &engine).await;
    let (_, plan) = describe_change_set(&router, "cdk-tag-change-set", Some(STACK)).await;
    assert_eq!(
        (
            xml_text(&plan, "Status").unwrap_or_default(),
            xml_texts(&plan, "Action"),
            xml_texts(&plan, "Attribute"),
        ),
        (
            "CREATE_COMPLETE".to_string(),
            vec!["Modify".to_string()],
            vec!["Tags".to_string()],
        ),
        "a revision stating the stack's tags must plan the tag change on the resource it \
         manages, got {plan}"
    );
    let (code, answered) = execute_change_set(&router, "cdk-tag-change-set", Some(STACK)).await;
    assert_eq!(
        code, 200,
        "the tagging deploy must be executable, got {code} {answered}"
    );
    settle_the_store(&router, &engine).await;
    let (_, described) = describe_stack(&router, STACK).await;
    assert_eq!(
        xml_text(&described, "StackStatus").as_deref(),
        Some("UPDATE_COMPLETE"),
        "the tagging deploy must complete, got {described}"
    );
    let expected = [(TAG_KEY, TAG_VALUE)];
    let held = table_tags(&router, TABLE, &expected).await;
    assert_eq!(
        held,
        vec![(TAG_KEY.to_string(), TAG_VALUE.to_string())],
        "the adopted table must carry the stack's own tags, got {held:?}"
    );
    let held = item(&router).await;
    assert_eq!(
        held.as_deref(),
        Some(ITEM_FIELD),
        "the item must survive the tagging deploy too, got {held:?}"
    );
}
