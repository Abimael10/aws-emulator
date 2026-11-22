// SPDX-License-Identifier: CC0-1.0

//! What a CloudFormation-deployed `AWS::Glue::Table` carries in the Data Catalog that a table
//! written through AWS Glue's OWN API does not: the key-value property maps of the definition.
//!
//! The rule under test: **the Data Catalog serves a table definition as it was WRITTEN — a
//! `Parameters` map on the table, on its storage descriptor or on that descriptor's SerDe is
//! reported where the write supplied it and absent where it did not — and CloudFormation
//! resolves a declared `AWS::Glue::Table` into a WHOLE `TableInput`, stating those maps (empty
//! where the declaration states no property) when it submits the create.** So a client that
//! reads a stack-deployed table back through `glue GetTable` finds all three present, while the
//! same definition submitted through `glue CreateTable` carries only what the caller sent.
//! Sources: `Table$Parameters` "these key-value pairs define properties associated with the
//! table", `StorageDescriptor$Parameters` "the user-supplied properties in key-value form" and
//! `SerDeInfo$Parameters` (none of the three `@required` or `@default`, source: `smithy_query
//! shape=Table,StorageDescriptor,SerDeInfo` on the AWS Glue model).
//!
//! WHY the suite lives here: it is exactly the difference between two writers of one catalog —
//! CloudFormation provisioning the table and a client calling AWS Glue directly — so a
//! crate-local test can only ever see one side of it.
//!
//! The rows differ along what the declaration states: nothing at all beyond the storage it
//! locates, a serde named but no properties anywhere, and properties declared at every level
//! (the control, where the deployment adds nothing and drops nothing).
//!
//! The stack's own clock is driven THROUGH THE STORE
//! ([`crate::cloudformation_stack_clock::settle_the_stacks`]) rather than waited for, so
//! nothing here races the host.

use serde_json::{json, Value};

use crate::cloudformation_provisions_glue_jobs::{build_router_and_engine, cloudformation, glue};
use crate::cloudformation_stack_clock::settle_the_stacks;
use crate::query_wire::opt_element;

/// The Region every row deploys and reads in.
const REGION: &str = "us-east-1";

/// The logical ids every row's template uses.
const DATABASE_LOGICAL_ID: &str = "PropertyMapDatabase";
const TABLE_LOGICAL_ID: &str = "PropertyMapTable";

/// One row: what the declaration states, and what the deployed table must then report for the
/// three property maps.
struct Row {
    case: &'static str,
    /// The `TableInput` the template declares, under the table's own name.
    declared: Value,
    /// `Table.Parameters`, `StorageDescriptor.Parameters` and `SerdeInfo.Parameters` as the
    /// read must report them for the DEPLOYED table.
    deployed: [Value; 3],
}

/// The rows of the table.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a declaration stating no property anywhere",
            declared: json!({
                "TableType": "EXTERNAL_TABLE",
                "StorageDescriptor": {
                    "Location": "s3://sim2real-lake/deployed/",
                    "Columns": [ { "Name": "id", "Type": "bigint" } ],
                    "SerdeInfo": {
                        "SerializationLibrary":
                            "org.apache.hadoop.hive.serde2.lazy.LazySimpleSerDe"
                    }
                }
            }),
            deployed: [json!({}), json!({}), json!({})],
        },
        Row {
            case: "a declaration whose serde states only its name",
            declared: json!({
                "TableType": "EXTERNAL_TABLE",
                "StorageDescriptor": {
                    "Location": "s3://sim2real-lake/deployed-serde/",
                    "SerdeInfo": { "Name": "csv" }
                }
            }),
            deployed: [json!({}), json!({}), json!({})],
        },
        Row {
            // The control: a declaration that states properties everywhere, where the
            // deployment neither adds nor drops one.
            case: "a declaration stating properties at every level",
            declared: json!({
                "TableType": "EXTERNAL_TABLE",
                "Parameters": { "classification": "csv" },
                "StorageDescriptor": {
                    "Location": "s3://sim2real-lake/deployed-full/",
                    "Parameters": { "skip.header.line.count": "1" },
                    "SerdeInfo": { "Parameters": { "field.delim": "," } }
                }
            }),
            deployed: [
                json!({ "classification": "csv" }),
                json!({ "skip.header.line.count": "1" }),
                json!({ "field.delim": "," }),
            ],
        },
    ]
}

/// The template one row deploys: the database that holds the table, and the table itself.
fn template(database: &str, table: &str, declared: &Value) -> String {
    let mut input = declared.clone();
    input["Name"] = json!(table);
    json!({
        "Description": "a Glue table whose property maps the deployment states",
        "Resources": {
            DATABASE_LOGICAL_ID: {
                "Type": "AWS::Glue::Database",
                "Properties": { "DatabaseInput": { "Name": database } }
            },
            TABLE_LOGICAL_ID: {
                "Type": "AWS::Glue::Table",
                "Properties": {
                    "DatabaseName": { "Ref": DATABASE_LOGICAL_ID },
                    "TableInput": input
                }
            }
        }
    })
    .to_string()
}

/// The three property maps of one reported table, as `Option` so an ABSENT member is told
/// apart from an empty one.
fn property_maps(table: &Value) -> [Option<Value>; 3] {
    [
        table.get("Parameters").cloned(),
        table
            .pointer("/StorageDescriptor/Parameters")
            .cloned(),
        table
            .pointer("/StorageDescriptor/SerdeInfo/Parameters")
            .cloned(),
    ]
}

/// The member each slot of [`property_maps`] is, for an assertion that names it.
const MAP_NAMES: [&str; 3] = [
    "Table.Parameters",
    "StorageDescriptor.Parameters",
    "StorageDescriptor.SerdeInfo.Parameters",
];

/// A stack-deployed table carries the property maps its deployment stated, while the SAME
/// definition submitted through AWS Glue's own `CreateTable` carries only what the caller
/// sent.
#[tokio::test]
async fn a_deployed_glue_table_states_its_property_maps_and_a_client_written_one_does_not() {
    let (router, engine) = build_router_and_engine();

    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        let stack_name = format!("glue-table-property-maps-{index}");
        let database = format!("prop_map_db_{index}");
        let table = format!("prop_map_table_{index}");
        let body = template(&database, &table, &row.declared);

        let (status, answer) = cloudformation(
            &router,
            REGION,
            "CreateStack",
            &[("StackName", &stack_name), ("TemplateBody", &body)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 from CreateStack, got {status} with {answer}"
        );
        settle_the_stacks(&router, &engine, case).await;
        let (status, described) =
            cloudformation(&router, REGION, "DescribeStacks", &[("StackName", &stack_name)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 from DescribeStacks, got {status} with {described}"
        );
        assert_eq!(
            opt_element(&described, "StackStatus").as_deref(),
            Some("CREATE_COMPLETE"),
            "case {case:?}: the stack must reach CREATE_COMPLETE, got {described}"
        );

        let (status, read) = glue(
            &router,
            REGION,
            "GetTable",
            &json!({ "DatabaseName": database, "Name": table }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 from GetTable for the deployed table, got {status} \
             with {read}"
        );
        let deployed = property_maps(&read["Table"]);
        for (slot, member) in MAP_NAMES.iter().enumerate() {
            assert_eq!(
                deployed[slot].as_ref(),
                Some(&row.deployed[slot]),
                "case {case:?}: the deployed table must report {member} {}, got {:?}",
                row.deployed[slot],
                deployed[slot]
            );
        }

        // The same definition, written through AWS Glue's own API in the same catalog: the
        // maps the client did not send are the ones the read withholds.
        let client_table = format!("{table}_client");
        let mut input = row.declared.clone();
        input["Name"] = json!(client_table);
        let (status, answer) = glue(
            &router,
            REGION,
            "CreateTable",
            &json!({ "DatabaseName": database, "TableInput": input }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 from CreateTable, got {status} with {answer}"
        );
        let (status, read) = glue(
            &router,
            REGION,
            "GetTable",
            &json!({ "DatabaseName": database, "Name": client_table }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 from GetTable for the client-written table, got \
             {status} with {read}"
        );
        let written = property_maps(&read["Table"]);
        for (slot, member) in MAP_NAMES.iter().enumerate() {
            let declared_map = match slot {
                0 => row.declared.get("Parameters").cloned(),
                1 => row.declared.pointer("/StorageDescriptor/Parameters").cloned(),
                _ => row
                    .declared
                    .pointer("/StorageDescriptor/SerdeInfo/Parameters")
                    .cloned(),
            };
            assert_eq!(
                written[slot], declared_map,
                "case {case:?}: a client-written table must report {member} exactly as the \
                 write supplied it ({declared_map:?}), got {:?}",
                written[slot]
            );
        }
    }
}
