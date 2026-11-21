// SPDX-License-Identifier: CC0-1.0

//! What a deployment of an `AWS::Glue::Table` reports — through CloudFormation's event stream
//! when the catalog refuses the table, and through AWS Glue's own read when the catalog holds
//! it.
//!
//! Two rules, the two halves of that one path. Two crates have to agree for either to hold —
//! CloudFormation makes the `CreateTable` call and AWS Glue answers it — so the suite lives
//! here: a crate-local test only ever sees one side.
//!
//! RULE A, which the first suite pins: **a stack's OWN events report the stack's lifecycle
//! status, so a resource status appears on a stack-level event only where the stack RESTS in
//! it.** A create CloudFormation rolls back goes from `CREATE_IN_PROGRESS` straight into
//! `ROLLBACK_IN_PROGRESS` and on to `ROLLBACK_COMPLETE`, so the `LogicalResourceId`s such a
//! deployment reports under a `*_FAILED` resource status are exactly the TEMPLATE resources
//! whose action the owning service refused; a caller that asked for the failed state to be kept
//! (`OnFailure=DO_NOTHING`, `DisableRollback=true`) gets a stack that does rest in
//! `CREATE_FAILED`, and then the stack's own event reports it too. Why it matters: the standard
//! way to find what failed a deploy is to filter the events for `CREATE_FAILED` and map the
//! ids onto template resources — an extra stack-level entry puts in that set a name no
//! template resource carries, so tooling mis-attributes the failure.
//!
//! RULE B, which the second suite pins: **AWS Glue reports a catalog table with every modelled
//! property map of its definition present — `Table$Parameters` ("these key-value pairs define
//! properties associated with the table"), `StorageDescriptor$Parameters` ("the user-supplied
//! properties in key-value form") and `SerDeInfo$Parameters` (source: `smithy_query
//! shape=Table,StorageDescriptor,SerDeInfo` on that service's own model) — EMPTY where the
//! definition declares no property**, exactly as the same projection reports a descriptor's
//! `Columns` as `[]`. A definition that declares nothing has an empty set of properties rather
//! than an unknown one, so a client that deployed the table and reads it back iterates or
//! merges the map instead of telling "no properties" apart from "member not returned".
//!
//! The rows of each suite differ along what its rule generalises over: for A, what the request
//! asked for on failure (the documented default rollback, `OnFailure=DO_NOTHING`,
//! `DisableRollback=true`) and whether the failing table stands alone in the template or beside
//! a resource the same deployment really does create; for B, which of the three maps the
//! declaration states, whether it states them absent or explicitly empty, how many entries a
//! stated map carries, and whether the declaration carries a storage descriptor or a SerDe at
//! all.
//!
//! The router, the two wire helpers over it, the stack clock and the XML readers are the test
//! crate's own ([`crate::cloudformation_provisions_glue_jobs`],
//! [`crate::cloudformation_stack_clock`], [`crate::query_wire`]), not another copy.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_provisions_glue_jobs::{
    build_router_and_engine, cloudformation, error_code, glue,
};
use crate::cloudformation_stack_clock::settle_the_stacks;
use crate::query_wire::{block_of, members_of, opt_element};

/// The Region every stack below is deployed in and every catalog read is made through.
const REGION: &str = "us-east-2";

/// The logical ids the templates use, so every read is of the one resource it is about.
const TABLE_LOGICAL_ID: &str = "AthenaTable";
const DATABASE_LOGICAL_ID: &str = "CatalogDatabase";

/// The resource types the two logical ids above are declared as, and the type a stack's OWN
/// event is reported under.
const TABLE_TYPE: &str = "AWS::Glue::Table";
const DATABASE_TYPE: &str = "AWS::Glue::Database";
const STACK_TYPE: &str = "AWS::CloudFormation::Stack";

/// The resource status a create the owning service refused is recorded under.
const CREATE_FAILED: &str = "CREATE_FAILED";

/// How long a case waits for a stack to reach a terminal status while its clock is driven past
/// every due instant on each look ([`settle_the_stacks`]).
///
/// Generous: the condition is looked at again until it holds, so this bounds only a stack that
/// never settles at all, and no case depends on how fast the host runs.
const SETTLED_DEADLINE: Duration = Duration::from_secs(60);

/// How long one look at that condition waits before looking again.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// One event of a stack, as deployment tooling reads the stream.
#[derive(Clone, Debug)]
struct StackEvent {
    logical_id: String,
    resource_type: String,
    status: String,
    reason: String,
}

/// Every event `DescribeStackEvents` reports for `stack`.
async fn stack_events(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
) -> Vec<StackEvent> {
    let (status, body) =
        cloudformation(router, REGION, "DescribeStackEvents", &[("StackName", stack)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeStackEvents, got {status} with body {body}"
    );
    members_of(&body)
        .into_iter()
        .map(|event| StackEvent {
            logical_id: block_of(event, "LogicalResourceId")
                .unwrap_or_default()
                .to_string(),
            resource_type: block_of(event, "ResourceType")
                .unwrap_or_default()
                .to_string(),
            status: block_of(event, "ResourceStatus")
                .unwrap_or_default()
                .to_string(),
            reason: block_of(event, "ResourceStatusReason")
                .unwrap_or_default()
                .to_string(),
        })
        .collect()
}

/// Drive the stack clock and read `stack` back until it rests in a terminal status, and answer
/// the status it rested in.
///
/// Every transition a deployment owes is on the stack's own CLOCK, so each due instant is
/// pulled into the past and the condition looked at again, rather than waited for
/// ([`settle_the_stacks`]).
async fn settled_status(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    case: &str,
    stack: &str,
) -> String {
    let deadline = SystemTime::now() + SETTLED_DEADLINE;
    loop {
        settle_the_stacks(router, engine, case).await;
        let (status, described) =
            cloudformation(router, REGION, "DescribeStacks", &[("StackName", stack)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeStacks, got {status} with body \
             {described}"
        );
        let reached = opt_element(&described, "StackStatus").unwrap_or_default();
        if reached.ends_with("_COMPLETE") || reached.ends_with("_FAILED") {
            return reached;
        }
        assert!(
            SystemTime::now() < deadline,
            "case {case:?}: the stack never left {reached:?}; last read {described}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

// ---------------------------------------------------------------------------
// RULE A — which ids a failed deployment reports a failed resource status for
// ---------------------------------------------------------------------------

/// A template whose only resource is a catalog table, declared in the database `database`
/// names — which the rows below name a database the catalog does not hold, so the owning
/// service refuses the create and the deployment fails with it.
fn table_only_template(database: &str, table: &str) -> String {
    json!({
        "Description": "one AWS Glue catalog table",
        "Resources": {
            TABLE_LOGICAL_ID: {
                "Type": TABLE_TYPE,
                "Properties": {
                    "DatabaseName": database,
                    "TableInput": {
                        "Name": table,
                        "TableType": "EXTERNAL_TABLE",
                        "StorageDescriptor": {
                            "Location": "s3://athorigin-events/events/",
                            "Columns": [{ "Name": "id", "Type": "bigint" }]
                        }
                    }
                }
            }
        }
    })
    .to_string()
}

/// The same failing table BESIDE a database this deployment really does create: the green
/// resource is what shows the failed set is the resources that failed and not every resource
/// of the template, and what the rollback is then read against.
fn table_beside_a_database_template(absent: &str, created: &str, table: &str) -> String {
    json!({
        "Description": "a catalog database, and a table declared in another one",
        "Resources": {
            DATABASE_LOGICAL_ID: {
                "Type": DATABASE_TYPE,
                "Properties": { "DatabaseInput": { "Name": created } }
            },
            TABLE_LOGICAL_ID: {
                "Type": TABLE_TYPE,
                "Properties": {
                    "DatabaseName": absent,
                    "TableInput": { "Name": table, "TableType": "EXTERNAL_TABLE" }
                }
            }
        }
    })
    .to_string()
}

/// What a row's `CreateStack` asked for on failure, and so whether the stack rests in the
/// failed status.
struct FailureHandling {
    label: &'static str,
    /// The member the request carries beside the template, or none for the documented default
    /// ("ROLLBACK — Roll back the stack creation. This is the default").
    member: Option<(&'static str, &'static str)>,
    rests_in_the_failure: bool,
}

const FAILURE_HANDLINGS: &[FailureHandling] = &[
    FailureHandling {
        label: "the documented default rollback",
        member: None,
        rests_in_the_failure: false,
    },
    FailureHandling {
        label: "the request asked for the failed state to be kept",
        member: Some(("OnFailure", "DO_NOTHING")),
        rests_in_the_failure: true,
    },
    FailureHandling {
        label: "the request disabled rollback",
        member: Some(("DisableRollback", "true")),
        rests_in_the_failure: true,
    },
];

/// What the template declares beside the table whose create is refused.
struct TemplateShape {
    label: &'static str,
    /// The database the deployment itself creates, for the shape that declares one.
    green_database: Option<&'static str>,
}

const TEMPLATE_SHAPES: &[TemplateShape] = &[
    TemplateShape {
        label: "the failing table alone in the template",
        green_database: None,
    },
    TemplateShape {
        label: "the failing table beside a database the deployment does create",
        green_database: Some("athorigin_attributed_db"),
    },
];

/// The distinct `(LogicalResourceId, ResourceType)` pairs a stack reports under a `*_FAILED`
/// resource status, sorted so the assertion is about the SET and not the stream's order.
fn failed_pairs(events: &[StackEvent]) -> Vec<(String, String)> {
    let distinct: BTreeSet<(String, String)> = events
        .iter()
        .filter(|event| event.status.ends_with("_FAILED"))
        .map(|event| (event.logical_id.clone(), event.resource_type.clone()))
        .collect();
    distinct.into_iter().collect()
}

/// Every status the stack's OWN events carry, in the order the stream reports them.
fn stack_own_statuses(events: &[StackEvent], stack: &str) -> Vec<String> {
    events
        .iter()
        .filter(|event| event.logical_id == stack)
        .map(|event| event.status.clone())
        .collect()
}

/// A deployment whose table the catalog refuses reports that TABLE under `CREATE_FAILED`, and
/// nothing else — the stack's own events carry the stack's lifecycle statuses, and name the
/// stack under a failed status only where the request asked for the failed state to be kept.
#[tokio::test]
async fn a_failed_glue_table_deployment_reports_the_failed_status_for_its_resources() {
    let table = "athorigin_events";
    let mut cases_run = 0usize;

    for (handling_index, handling) in FAILURE_HANDLINGS.iter().enumerate() {
        for (shape_index, shape) in TEMPLATE_SHAPES.iter().enumerate() {
            let case = format!("{} / {}", handling.label, shape.label);
            // One emulator per row: what the rollback removed is read back out of the owning
            // service, so no row may see another row's catalog.
            let (router, engine) = build_router_and_engine();
            let stack = format!("athorigin-attribution-{handling_index}-{shape_index}");
            let absent = format!("athorigin_nodb_{handling_index}{shape_index}");
            let body = match shape.green_database {
                None => table_only_template(&absent, table),
                Some(created) => {
                    table_beside_a_database_template(&absent, created, table)
                }
            };
            let mut members = vec![
                ("StackName", stack.as_str()),
                ("TemplateBody", body.as_str()),
            ];
            if let Some((name, value)) = handling.member {
                members.push((name, value));
            }
            let (status, answered) = cloudformation(
                &router, REGION, "CreateStack", &members,
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: CreateStack must be accepted — the stack reports the outcome — \
                 got {status} with body {answered}"
            );

            let reached = settled_status(&router, &engine, &case, &stack).await;
            let expected_status = match handling.rests_in_the_failure {
                true => CREATE_FAILED,
                false => "ROLLBACK_COMPLETE",
            };
            assert_eq!(
                reached, expected_status,
                "case {case:?}: expected the stack to settle at {expected_status:?}, got \
                 {reached:?}"
            );

            let events = stack_events(&router, &case, &stack).await;

            // The set a client filters the stream for: the table that failed, and the stack
            // itself only where the stack rests in the failed status.
            let mut expected = vec![(TABLE_LOGICAL_ID.to_string(), TABLE_TYPE.to_string())];
            if handling.rests_in_the_failure {
                expected.push((stack.clone(), STACK_TYPE.to_string()));
            }
            expected.sort();
            let reported = failed_pairs(&events);
            assert_eq!(
                reported, expected,
                "case {case:?}: expected exactly {expected:?} to carry a failed status, got \
                 {reported:?}"
            );

            // A stack-level event is the STACK's: it is reported under the stack's own type,
            // and it carries one of the statuses this deployment's stack went through.
            let allowed: &[&str] = match handling.rests_in_the_failure {
                true => &["CREATE_IN_PROGRESS", CREATE_FAILED],
                false => &[
                    "CREATE_IN_PROGRESS",
                    "ROLLBACK_IN_PROGRESS",
                    "ROLLBACK_COMPLETE",
                ],
            };
            for event in events.iter().filter(|event| event.logical_id == stack) {
                assert_eq!(
                    event.resource_type, STACK_TYPE,
                    "case {case:?}: the stack's own event must be reported under {STACK_TYPE}, \
                     got {event:?}"
                );
                assert!(
                    allowed.contains(&event.status.as_str()),
                    "case {case:?}: the stack's own event must carry one of {allowed:?}, got \
                     {event:?}"
                );
            }
            let own = stack_own_statuses(&events, &stack);
            for owed in allowed {
                assert!(
                    own.iter().any(|status| status == owed),
                    "case {case:?}: expected the stack's own {owed} event, got {own:?}"
                );
            }

            // The failing resource's own event is what says WHAT to fix: it relays the owning
            // service's refusal, which names the database the declaration addressed (the
            // catalog keys a database under the folded name).
            let diagnosis = events
                .iter()
                .find(|event| event.logical_id == TABLE_LOGICAL_ID && event.status == CREATE_FAILED)
                .map(|event| event.reason.clone())
                .unwrap_or_else(|| panic!("case {case:?}: no {CREATE_FAILED} event for the table"));
            assert!(
                diagnosis.contains(&absent.to_ascii_lowercase()),
                "case {case:?}: the table's {CREATE_FAILED} reason must relay the owning \
                 service's refusal naming {absent:?}, got {diagnosis:?}"
            );

            // The green resource is not a report either: the rollback really removed the
            // database the deployment had made, and the kept failure really left it there.
            if let Some(created) = shape.green_database {
                let (status, read) =
                    glue(&router, REGION, "GetDatabase", &json!({ "Name": created })).await;
                match handling.rests_in_the_failure {
                    true => assert_eq!(
                        status, 200,
                        "case {case:?}: a kept failure must leave the database the deployment \
                         created, got {status} with body {read}"
                    ),
                    false => {
                        assert_eq!(
                            status, 400,
                            "case {case:?}: the rollback must remove the database the \
                             deployment created, got {status} with body {read}"
                        );
                        assert_eq!(
                            error_code(&read),
                            "EntityNotFoundException",
                            "case {case:?}: expected EntityNotFoundException for the released \
                             database, got {read}"
                        );
                    }
                }
            }
            cases_run += 1;
        }
    }

    assert_eq!(
        cases_run,
        FAILURE_HANDLINGS.len() * TEMPLATE_SHAPES.len(),
        "every failure handling and template shape must have been exercised, got {cases_run} \
         cases"
    );
}

// ---------------------------------------------------------------------------
// RULE B — the property maps a deployed table is reported with
// ---------------------------------------------------------------------------

/// The database every row of the second suite is deployed into, created by the same stack so
/// the tables depend on it.
const DEFAULTS_DATABASE: &str = "athorigin_defaults_db";

/// The serialization library the recorded shape's declaration names, which must come back as
/// it was stored beside the empty property map of the same SerDe.
const LAZY_SIMPLE_SERDE: &str = "org.apache.hadoop.hive.serde2.lazy.LazySimpleSerDe";

/// One row of the second suite: the table a declaration asks for, and the three property maps
/// the owning service's read must then report for it.
struct DefinitionRow {
    case: &'static str,
    logical_id: &'static str,
    name: &'static str,
    /// The `TableInput` the template declares, without its `Name` (added below).
    input: Value,
    /// `Table.Parameters`.
    parameters: Value,
    /// `Table.StorageDescriptor.Parameters`, or `None` for a declaration that carries no
    /// storage descriptor at all — the catalog holds the properties OF a descriptor it stores,
    /// not whether there is one.
    storage: Option<Value>,
    /// `Table.StorageDescriptor.SerdeInfo.Parameters`, or `None` when the descriptor names no
    /// SerDe.
    serde: Option<Value>,
}

/// A map of `entries` properties, for the rows that move the entry COUNT.
fn properties(entries: usize) -> Value {
    let map: serde_json::Map<String, Value> = (0..entries)
        .map(|index| (format!("property_{index}"), json!(format!("value_{index}"))))
        .collect();
    Value::Object(map)
}

/// The rows of the second suite.
fn definition_rows() -> Vec<DefinitionRow> {
    vec![
        DefinitionRow {
            // The recorded shape: a located, serde-described table with no property anywhere.
            case: "no property map declared anywhere",
            logical_id: "NoProperties",
            name: "athorigin_no_properties",
            input: json!({
                "TableType": "EXTERNAL_TABLE",
                "StorageDescriptor": {
                    "Columns": [{ "Name": "id", "Type": "bigint" }],
                    "Location": "s3://athorigin-events/no_properties/",
                    "InputFormat": "org.apache.hadoop.mapred.TextInputFormat",
                    "SerdeInfo": { "SerializationLibrary": LAZY_SIMPLE_SERDE }
                }
            }),
            parameters: json!({}),
            storage: Some(json!({})),
            serde: Some(json!({})),
        },
        DefinitionRow {
            case: "no storage descriptor declared at all",
            logical_id: "NoStorage",
            name: "athorigin_no_storage",
            input: json!({ "TableType": "EXTERNAL_TABLE" }),
            parameters: json!({}),
            storage: None,
            serde: None,
        },
        DefinitionRow {
            case: "a storage descriptor that names no serde",
            logical_id: "NoSerde",
            name: "athorigin_no_serde",
            input: json!({
                "TableType": "EXTERNAL_TABLE",
                "StorageDescriptor": { "Location": "s3://athorigin-events/no_serde/" }
            }),
            parameters: json!({}),
            storage: Some(json!({})),
            serde: None,
        },
        DefinitionRow {
            case: "all three maps declared explicitly empty",
            logical_id: "ExplicitlyEmpty",
            name: "athorigin_explicitly_empty",
            input: json!({
                "TableType": "EXTERNAL_TABLE",
                "Parameters": {},
                "StorageDescriptor": {
                    "Location": "s3://athorigin-events/explicitly_empty/",
                    "Parameters": {},
                    "SerdeInfo": { "Parameters": {} }
                }
            }),
            parameters: json!({}),
            storage: Some(json!({})),
            serde: Some(json!({})),
        },
        DefinitionRow {
            case: "nine properties on the table, one on its storage, none on its serde",
            logical_id: "ManyProperties",
            name: "athorigin_many_properties",
            input: json!({
                "TableType": "EXTERNAL_TABLE",
                "Parameters": properties(9),
                "StorageDescriptor": {
                    "Location": "s3://athorigin-events/many_properties/",
                    "Parameters": properties(1),
                    "SerdeInfo": { "SerializationLibrary": LAZY_SIMPLE_SERDE }
                }
            }),
            parameters: properties(9),
            storage: Some(properties(1)),
            serde: Some(json!({})),
        },
        DefinitionRow {
            case: "the storage and its serde declare properties, the table none",
            logical_id: "StorageProperties",
            name: "athorigin_storage_properties",
            input: json!({
                "TableType": "EXTERNAL_TABLE",
                "StorageDescriptor": {
                    "Location": "s3://athorigin-events/storage_properties/",
                    "Parameters": { "skip.header.line.count": "1" },
                    "SerdeInfo": { "Parameters": { "field.delim": "," } }
                }
            }),
            parameters: json!({}),
            storage: Some(json!({ "skip.header.line.count": "1" })),
            serde: Some(json!({ "field.delim": "," })),
        },
    ]
}

/// The template of the second suite: the database, and one table per row declared in it.
fn definition_template(rows: &[DefinitionRow]) -> String {
    let mut resources = serde_json::Map::new();
    resources.insert(
        DATABASE_LOGICAL_ID.to_string(),
        json!({
            "Type": DATABASE_TYPE,
            "Properties": { "DatabaseInput": { "Name": DEFAULTS_DATABASE } }
        }),
    );
    for row in rows {
        let mut input = row.input.as_object().cloned().unwrap_or_default();
        input.insert("Name".to_string(), json!(row.name));
        resources.insert(
            row.logical_id.to_string(),
            json!({
                "Type": TABLE_TYPE,
                "Properties": {
                    "DatabaseName": { "Ref": DATABASE_LOGICAL_ID },
                    "TableInput": Value::Object(input)
                }
            }),
        );
    }
    json!({
        "Description": "one AWS Glue catalog database and the tables declared in it",
        "Resources": Value::Object(resources)
    })
    .to_string()
}

/// Assert the three property maps of one projected table, naming which read answered it.
fn assert_property_maps(case: &str, reader: &str, table: &Value, row: &DefinitionRow) {
    assert_eq!(
        table["Parameters"], row.parameters,
        "case {case:?}: {reader} must report Table.Parameters {}, got {:?}",
        row.parameters, table["Parameters"]
    );
    match &row.storage {
        None => assert!(
            table["StorageDescriptor"].is_null(),
            "case {case:?}: {reader} must report no storage descriptor for a definition that \
             declares none, got {:?}",
            table["StorageDescriptor"]
        ),
        Some(storage) => assert_eq!(
            table["StorageDescriptor"]["Parameters"], *storage,
            "case {case:?}: {reader} must report StorageDescriptor.Parameters {storage}, got {:?}",
            table["StorageDescriptor"]["Parameters"]
        ),
    }
    match &row.serde {
        None => assert!(
            table["StorageDescriptor"]["SerdeInfo"].is_null(),
            "case {case:?}: {reader} must report no SerdeInfo for a descriptor that names none, \
             got {:?}",
            table["StorageDescriptor"]["SerdeInfo"]
        ),
        Some(serde) => assert_eq!(
            table["StorageDescriptor"]["SerdeInfo"]["Parameters"], *serde,
            "case {case:?}: {reader} must report SerdeInfo.Parameters {serde}, got {:?}",
            table["StorageDescriptor"]["SerdeInfo"]["Parameters"]
        ),
    }
}

/// A table a stack deployed is reported by AWS Glue with each of the three property maps of its
/// definition present: the properties the declaration stated, and an EMPTY map where it stated
/// none — beside the members of the definition that were stored as declared.
#[tokio::test]
async fn a_deployed_glue_table_reports_every_property_map_of_its_definition() {
    let (router, engine) = build_router_and_engine();
    let rows = definition_rows();
    let body = definition_template(&rows);
    let stack = "athorigin-definition-defaults";

    let (status, answered) = cloudformation(
        &router,
        REGION,
        "CreateStack",
        &[("StackName", stack), ("TemplateBody", &body)],
    )
    .await;
    assert_eq!(
        status, 200,
        "expected HTTP 200 from CreateStack, got {status} with body {answered}"
    );
    let reached = settled_status(&router, &engine, "the deployment", stack).await;
    assert_eq!(
        reached, "CREATE_COMPLETE",
        "the deployment must complete before its tables are read, got {reached:?}"
    );

    // The one listing every row is also read out of, so the single-resource read and the list
    // cannot answer differently.
    let (status, page) = glue(
        &router,
        REGION,
        "GetTables",
        &json!({ "DatabaseName": DEFAULTS_DATABASE }),
    )
    .await;
    assert_eq!(
        status, 200,
        "expected HTTP 200 from GetTables, got {status} with body {page}"
    );
    let listed = page["TableList"]
        .as_array()
        .unwrap_or_else(|| panic!("no TableList in {page}"))
        .clone();

    for row in &rows {
        let case = row.case;
        let (status, read) = glue(
            &router,
            REGION,
            "GetTable",
            &json!({ "DatabaseName": DEFAULTS_DATABASE, "Name": row.name }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the deployed table must resolve, got {status} with body {read}"
        );
        let table = read["Table"].clone();
        assert_eq!(
            table["Name"].as_str(),
            Some(row.name),
            "case {case:?}: expected the deployed table to be reported, got {table:?}"
        );
        assert_property_maps(case, "GetTable", &table, row);

        let from_list = listed
            .iter()
            .find(|entry| entry["Name"] == json!(row.name))
            .unwrap_or_else(|| panic!("case {case:?}: {} must appear in TableList", row.name))
            .clone();
        assert_property_maps(case, "GetTables", &from_list, row);
        assert_eq!(
            from_list, table,
            "case {case:?}: the TableList element must be the GetTable projection, got \
             {from_list:?} against {table:?}"
        );

        // The maps are reported BESIDE the definition as it was declared: the empty map is the
        // catalog materialising a property set, not a descriptor it lost.
        if row.serde.is_some() {
            let declared = row.input["StorageDescriptor"]["SerdeInfo"]["SerializationLibrary"]
                .as_str()
                .map(str::to_string);
            let reported = table["StorageDescriptor"]["SerdeInfo"]["SerializationLibrary"]
                .as_str()
                .map(str::to_string);
            assert_eq!(
                reported, declared,
                "case {case:?}: expected the declared SerializationLibrary {declared:?} beside \
                 the SerDe's property map, got {reported:?}"
            );
        }
        if let Some(location) = row.input["StorageDescriptor"]["Location"].as_str() {
            assert_eq!(
                table["StorageDescriptor"]["Location"].as_str(),
                Some(location),
                "case {case:?}: expected the declared Location {location:?}, got {:?}",
                table["StorageDescriptor"]["Location"]
            );
        }
    }
}
