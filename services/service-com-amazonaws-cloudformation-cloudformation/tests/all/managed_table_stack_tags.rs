// SPDX-License-Identifier: CC0-1.0

//! The tags an `AWS::DynamoDB::Table` a stack MANAGES carries in Amazon DynamoDB itself,
//! across a stack-level tag update.
//!
//! RULE: the stack-level `Tags` a stack operation states are propagated to the resources the
//! stack manages, through each type's own tagging calls — so a table the stack manages ends
//! up carrying exactly the stated set, whether the stack CREATED it or ADOPTED one that
//! already existed, and a key a later operation no longer states is taken off it again. The
//! tags are read back through DYNAMODB's own wire, not out of CloudFormation's record of
//! them: `CreateTable`'s own `Tags` member applies to the table that call creates and to no
//! table that already exists, so an adopted table — created by no call of that service's at
//! all — is only ever tagged by that reconciliation.
//!
//! The rows differ along what the rule generalises over: how the stack came to manage the
//! table (created from the declaration, or adopted through an IMPORT change set), how many
//! stack tags the update states, and whether a key the table already carries is kept, changed
//! or dropped. The keys CloudFormation records about a resource it manages
//! (`aws:cloudformation:*`) are in the namespace DynamoDB reserves for itself, so they are
//! held in CloudFormation's own records and reach no tagging call — which is why only the
//! caller's own keys are expected here.

use std::collections::BTreeMap;

use serde_json::json;

use crate::change_set_lifecycle::{
    create_change_set, describe_change_set, execute_change_set, settle_the_store,
};
use crate::dynamodb_table_resources::{dynamodb, table_tags};
use crate::harness::{call, create_stack, member, router_with_engine, xml_text};
use crate::stack_lifecycle::describe_stack;

/// The key schema every row declares for its table.
const KEY_SCHEMA: &str = r#""KeySchema":[{"AttributeName":"pk","KeyType":"HASH"}],
    "AttributeDefinitions":[{"AttributeName":"pk","AttributeType":"S"}],
    "BillingMode":"PAY_PER_REQUEST""#;

/// How the stack came to manage the table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Managed {
    /// The stack CREATED the table from its declaration.
    Created,
    /// The stack ADOPTED a table a client had already created, through an IMPORT change set.
    Adopted,
}

/// One row: how the stack manages the table, and the stack-level tags each operation states.
struct Row {
    case: &'static str,
    stack: &'static str,
    table: &'static str,
    managed: Managed,
    /// The stack-level tags the FIRST operation states.
    first: &'static [(&'static str, &'static str)],
    /// The stack-level tags a second operation states, which the table must end up with.
    second: &'static [(&'static str, &'static str)],
}

/// The `Tags.member.N.Key`/`Value` request members a stack-level tag list is.
fn tag_members(tags: &[(&str, &str)]) -> Vec<(String, String)> {
    let mut members = Vec::new();
    for (index, (key, value)) in tags.iter().enumerate() {
        let position = index + 1;
        members.push(member(&format!("Tags.member.{position}.Key"), key));
        members.push(member(&format!("Tags.member.{position}.Value"), value));
    }
    members
}

/// The tags the table must end up carrying, as a map, so the comparison is about the SET and
/// not about its order.
fn expected_tags(tags: &[(&str, &str)]) -> BTreeMap<String, String> {
    tags.iter()
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect()
}

/// The tags DynamoDB holds for the table, as a map: the shared read
/// ([`crate::dynamodb_table_resources::table_tags`]) waits for the set the row expects, and
/// what it reports is compared as a set.
async fn held_tags(
    router: &smithy_server_runtime::MultiServiceRouter,
    name: &str,
    expected: &[(&str, &str)],
) -> BTreeMap<String, String> {
    table_tags(router, name, expected).await.into_iter().collect()
}

/// The template one row deploys: the table under the name the row states, with the deletion
/// policy an IMPORT requires of every resource it adopts — beside the handle the stack an
/// import is for was brought into existence with, since an import's template declares
/// everything the stack keeps.
fn template(table: &str, with_handle: bool) -> String {
    let handle = if with_handle {
        r#""Handle":{"Type":"AWS::CloudFormation::WaitConditionHandle"},"#
    } else {
        ""
    };
    format!(
        r#"{{"Resources":{{{handle}"Table":{{"Type":"AWS::DynamoDB::Table",
            "DeletionPolicy":"Retain",
            "Properties":{{"TableName":"{table}",{KEY_SCHEMA}}}}}}}}}"#
    )
}

/// RULE: a table the stack manages carries the stack-level tags the last operation stated —
/// whether the stack created it or adopted it — and no key that operation did not state.
#[tokio::test]
async fn a_managed_table_carries_the_stacks_tags() {
    let rows: &[Row] = &[
        Row {
            case: "a table the stack created, tagged by a later update",
            stack: "tagged-created-table",
            table: "tagged-created-table-store",
            managed: Managed::Created,
            first: &[],
            second: &[("sim2real:prefix", "sim2real-scn")],
        },
        Row {
            case: "a table the stack ADOPTED through an import, tagged by a later update",
            stack: "tagged-adopted-table",
            table: "tagged-adopted-table-store",
            managed: Managed::Adopted,
            first: &[],
            second: &[("sim2real:prefix", "sim2real-scn"), ("team", "platform")],
        },
        Row {
            case: "an adopted table whose tag is changed and whose other tag is dropped",
            stack: "retagged-adopted-table",
            table: "retagged-adopted-table-store",
            managed: Managed::Adopted,
            first: &[("owner", "the-stack"), ("dropped", "gone")],
            second: &[("owner", "the-other-stack")],
        },
    ];

    for row in rows {
        let (router, engine) = router_with_engine();
        let case = row.case;
        let body = template(row.table, row.managed == Managed::Adopted);
        if row.managed == Managed::Adopted {
            // The table a CLIENT created, through DynamoDB's own wire: nothing created it for
            // CloudFormation, so it carries no tags of any stack.
            let (code, answered) = dynamodb(
                &router,
                "CreateTable",
                &json!({
                    "TableName": row.table,
                    "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
                    "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
                    "BillingMode": "PAY_PER_REQUEST",
                }),
            )
            .await;
            assert_eq!(
                code, 200,
                "case {case:?}: the client's own CreateTable must be accepted, got {code} \
                 {answered}"
            );
            // The stack the import is for, with the tags the first operation states and no
            // resources of its own yet.
            let empty = r#"{"Resources":{"Handle":
                {"Type":"AWS::CloudFormation::WaitConditionHandle"}}}"#;
            let (code, answered) =
                create_stack(&router, row.stack, empty, &tag_members(row.first)).await;
            assert_eq!(
                code, 200,
                "case {case:?}: CreateStack must be accepted, got {code} {answered}"
            );
            settle_the_store(&router, &engine).await;
            let mut members = vec![
                member("ChangeSetType", "IMPORT"),
                member("TemplateBody", &body),
                member("ResourcesToImport.member.1.ResourceType", "AWS::DynamoDB::Table"),
                member("ResourcesToImport.member.1.LogicalResourceId", "Table"),
                member(
                    "ResourcesToImport.member.1.ResourceIdentifier.entry.1.key",
                    "TableName",
                ),
                member(
                    "ResourcesToImport.member.1.ResourceIdentifier.entry.1.value",
                    row.table,
                ),
            ];
            members.extend(tag_members(row.first));
            let (code, answered) = create_change_set(&router, row.stack, "import", &members).await;
            assert_eq!(
                code, 200,
                "case {case:?}: the import change set must be created, got {code} {answered}"
            );
            settle_the_store(&router, &engine).await;
            let (_, plan) = describe_change_set(&router, "import", Some(row.stack)).await;
            assert_eq!(
                xml_text(&plan, "Status").as_deref(),
                Some("CREATE_COMPLETE"),
                "case {case:?}: the import plan must be offered for execution, got {plan}"
            );
            let (code, answered) = execute_change_set(&router, "import", Some(row.stack)).await;
            assert_eq!(
                code, 200,
                "case {case:?}: the import must be executable, got {code} {answered}"
            );
            settle_the_store(&router, &engine).await;
            let (_, described) = describe_stack(&router, row.stack).await;
            assert_eq!(
                xml_text(&described, "StackStatus").as_deref(),
                Some("IMPORT_COMPLETE"),
                "case {case:?}: the executed import must complete, got {described}"
            );
        } else {
            let (code, answered) =
                create_stack(&router, row.stack, &body, &tag_members(row.first)).await;
            assert_eq!(
                code, 200,
                "case {case:?}: CreateStack must be accepted, got {code} {answered}"
            );
            settle_the_store(&router, &engine).await;
            let (_, described) = describe_stack(&router, row.stack).await;
            assert_eq!(
                xml_text(&described, "StackStatus").as_deref(),
                Some("CREATE_COMPLETE"),
                "case {case:?}: the create must complete, got {described}"
            );
        }
        // The tags the FIRST operation stated are what the table carries before the update:
        // a created table was created with them, an adopted one was tagged when the stack
        // took it over.
        let first = expected_tags(row.first);
        let held = held_tags(&router, row.table, row.first).await;
        assert_eq!(
            held, first,
            "case {case:?}: before the update the table must carry {first:?}, got {held:?}"
        );

        // ...and the stack-level tag update states a different set.
        let mut members = vec![
            member("StackName", row.stack),
            member("TemplateBody", &body),
        ];
        members.extend(tag_members(row.second));
        let (code, answered) = call(&router, "UpdateStack", &members).await;
        assert_eq!(
            code, 200,
            "case {case:?}: UpdateStack must be accepted, got {code} {answered}"
        );
        settle_the_store(&router, &engine).await;
        let (_, described) = describe_stack(&router, row.stack).await;
        assert_eq!(
            xml_text(&described, "StackStatus").as_deref(),
            Some("UPDATE_COMPLETE"),
            "case {case:?}: the tag update must complete, got {described}"
        );
        let expected = expected_tags(row.second);
        let held = held_tags(&router, row.table, row.second).await;
        assert_eq!(
            held, expected,
            "case {case:?}: after the update the table must carry {expected:?}, got {held:?}"
        );
    }
}
