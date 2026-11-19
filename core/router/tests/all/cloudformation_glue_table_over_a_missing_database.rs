// SPDX-License-Identifier: CC0-1.0

//! What AWS Glue answers a client that reads a catalog table back after the stack which was
//! going to create it rolled back — the read whose `DatabaseName` names a database the
//! catalog never held.
//!
//! The rule under test: **a Data Catalog read that addresses a TABLE and cannot resolve it is
//! refused `EntityNotFoundException` carrying that read's ONE fixed sentence, the same bytes
//! whichever step of the address resolved to nothing (the database the request names, or the
//! table inside it) and whatever the request spelled — while the operations that address a
//! DATABASE itself keep their own templated sentence, which renders the name the catalog
//! keys the database under.** So the table read discloses neither name nor catalog and is
//! byte-identical across requests, and a client confirming that a rolled-back deployment
//! left no catalog entry matches one constant instead of a per-request string.
//!
//! Two crates have to agree for that to hold — CloudFormation makes the `CreateTable` call
//! that fails and AWS Glue serves the read afterwards — so the suite lives here: a
//! crate-local test sees only one side of it. AWS Glue's own suites pin the constant's
//! wording and its invariance over request shapes; what this one adds is the deployment path
//! the rule is reached through, where the failing resource's own `CREATE_FAILED` reason
//! carries the templated DATABASE sentence at the same time as the read carries the fixed
//! TABLE one.
//!
//! The rows differ along what the rule generalises over: how the absent `DatabaseName` is
//! spelled (a generated name, one character, mixed case, the documented 255 ceiling) and
//! which optional members the `GetTable` request carries (`CatalogId`, `AttributesToGet`).
//!
//! The router, the two wire helpers over it, the stack clock and the XML readers are the test
//! crate's own ([`crate::cloudformation_provisions_glue_jobs`],
//! [`crate::cloudformation_stack_clock`], [`crate::query_wire`]), not another copy.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_provisions_glue_jobs::{
    build_router_and_engine, cloudformation, error_code, glue, ACCOUNT,
};
use crate::cloudformation_stack_clock::settle_the_stacks;
use crate::query_wire::{block_of, members_of, opt_element};

/// The Region every stack below is deployed in and every catalog read is made through.
const REGION: &str = "us-east-2";

/// The logical id the template gives the table, which is the resource whose create fails when
/// the database it is declared in does not exist.
const TABLE_LOGICAL_ID: &str = "AthenaTable";

/// The template's two parameters, so one template serves both the control deploy and every
/// absent-database row.
const DATABASE_PARAMETER: &str = "DatabaseNameParameter";
const TABLE_PARAMETER: &str = "TableNameParameter";

/// The resource status a create the owning service refused is recorded under.
const CREATE_FAILED: &str = "CREATE_FAILED";

/// The database the CONTROL stack deploys into, and the table it declares there: the deploy
/// that must stay green, and the table every row reads back afterwards so a constant refusal
/// cannot be passing by refusing every read.
const CONTROL_DATABASE: &str = "athorigin_present_db";
const CONTROL_TABLE: &str = "athorigin_events";

/// A table name the control database never held: the TABLE step of the address missing, which
/// is the answer every database-step miss below must equal byte for byte.
const UNKNOWN_TABLE: &str = "athorigin_never_created";

/// The longest name AWS Glue accepts for a database or a table (`NameString`,
/// `@length(min: 1, max: 255)` on `GetTable`'s `DatabaseName` and `Name` in that service's own
/// model), which the last row sits at.
const MAX_NAME: usize = 255;

/// The glue-table-only template the steps to reproduce deploy, parameterised by the database
/// that holds the table and by the table's own name.
fn template() -> String {
    json!({
        "Description": "one AWS Glue catalog table, in the database the parameter names",
        "Parameters": {
            DATABASE_PARAMETER: { "Type": "String" },
            TABLE_PARAMETER: { "Type": "String" }
        },
        "Resources": {
            TABLE_LOGICAL_ID: {
                "Type": "AWS::Glue::Table",
                "Properties": {
                    "DatabaseName": { "Ref": DATABASE_PARAMETER },
                    "TableInput": {
                        "Name": { "Ref": TABLE_PARAMETER },
                        "TableType": "EXTERNAL_TABLE",
                        "StorageDescriptor": {
                            "Location": "s3://sim2real-athorigin/events/",
                            "Columns": [
                                { "Name": "id", "Type": "bigint" },
                                { "Name": "payload", "Type": "string" }
                            ]
                        }
                    }
                }
            }
        },
        "Outputs": {
            "AthenaTableName": { "Value": { "Ref": TABLE_LOGICAL_ID } }
        }
    })
    .to_string()
}

/// Deploy the template once for `(database, table)` and settle the stack, answering the
/// status it rests in.
///
/// Both transitions a failing create owes — its own conclusion and the rollback that
/// conclusion makes due — are on the stack's own CLOCK, so each is made due rather than waited
/// for ([`crate::cloudformation_stack_clock::settle_the_stacks`]).
async fn deploy(
    router: &MultiServiceRouter,
    engine: &std::sync::Arc<engine::InMemoryEngine>,
    case: &str,
    stack: &str,
    database: &str,
    table: &str,
) -> String {
    let body = template();
    let (status, answered) = cloudformation(
        router,
        REGION,
        "CreateStack",
        &[
            ("StackName", stack),
            ("TemplateBody", &body),
            ("Parameters.member.1.ParameterKey", DATABASE_PARAMETER),
            ("Parameters.member.1.ParameterValue", database),
            ("Parameters.member.2.ParameterKey", TABLE_PARAMETER),
            ("Parameters.member.2.ParameterValue", table),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {answered}"
    );
    settle_the_stacks(router, engine, case).await;
    settle_the_stacks(router, engine, case).await;

    let (status, described) =
        cloudformation(router, REGION, "DescribeStacks", &[("StackName", stack)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeStacks, got {status} with body {described}"
    );
    opt_element(&described, "StackStatus")
        .unwrap_or_else(|| panic!("case {case:?}: no StackStatus in {described}"))
}

/// The logical ids of the RESOURCES a stack recorded a refused create for, with the reason
/// each carries — the stack's own `CREATE_FAILED` event (whose logical id is the stack name)
/// left out, since it reports the deployment rather than a resource.
async fn refused_resources(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
) -> Vec<(String, String)> {
    let (status, events) =
        cloudformation(router, REGION, "DescribeStackEvents", &[("StackName", stack)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeStackEvents, got {status} with body {events}"
    );
    members_of(&events)
        .into_iter()
        .filter(|event| block_of(event, "ResourceStatus") == Some(CREATE_FAILED))
        .map(|event| {
            (
                block_of(event, "LogicalResourceId")
                    .unwrap_or_default()
                    .to_string(),
                block_of(event, "ResourceStatusReason")
                    .unwrap_or_default()
                    .to_string(),
            )
        })
        .filter(|(logical_id, _)| logical_id != stack)
        .collect()
}

/// The optional input members one `GetTable` miss is asked with: the members a templated
/// message could have reached for beside the two names.
struct RequestShape {
    label: &'static str,
    /// The account id the model documents as `CatalogId`'s default ("If none is provided, the
    /// Amazon Web Services account ID is used by default", `smithy_query` on `GetTable`).
    catalog_id: Option<&'static str>,
    /// `DEFAULT` is the `AttributesToGet` value that selects the Hive-style table definition
    /// this read serves (same source).
    attributes: Option<&'static [&'static str]>,
}

/// The request shapes every row is asked in.
fn request_shapes() -> Vec<RequestShape> {
    vec![
        RequestShape {
            label: "no optional members",
            catalog_id: None,
            attributes: None,
        },
        RequestShape {
            label: "explicit default CatalogId",
            catalog_id: Some(ACCOUNT),
            attributes: None,
        },
        RequestShape {
            label: "CatalogId and AttributesToGet DEFAULT",
            catalog_id: Some(ACCOUNT),
            attributes: Some(&["DEFAULT"]),
        },
    ]
}

/// One `GetTable` body for an address and a request shape.
fn get_table_request(database: &str, name: &str, shape: &RequestShape) -> Value {
    let mut request = json!({ "DatabaseName": database, "Name": name });
    if let Some(catalog_id) = shape.catalog_id {
        request["CatalogId"] = json!(catalog_id);
    }
    if let Some(attributes) = shape.attributes {
        request["AttributesToGet"] = json!(attributes);
    }
    request
}

/// The `Message` an awsJson refusal carries.
fn error_message(payload: &Value) -> String {
    payload
        .get("Message")
        .or_else(|| payload.get("message"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// Read one `GetTable` miss and answer the sentence it was refused with, requiring the status
/// and code the model gives the refusal (`EntityNotFoundException`, HTTP 400).
async fn miss_sentence(router: &MultiServiceRouter, case: &str, request: &Value) -> String {
    let (status, body) = glue(router, REGION, "GetTable", request).await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400 from the unresolvable GetTable, got {status} with body \
         {body}"
    );
    assert_eq!(
        error_code(&body),
        "EntityNotFoundException",
        "case {case:?}: expected EntityNotFoundException, got {body}"
    );
    error_message(&body)
}

/// Shortest identifier a substring match is evidence about: a one- or two-character name
/// occurs inside ordinary English by chance (`d` is in `Found`), so for a name that short the
/// equality against the table step's sentence is what shows nothing was interpolated.
const DISCLOSURE_FLOOR: usize = 4;

/// Require that `sentence` names none of the request's or the catalog's identifiers.
fn assert_withholds(case: &str, sentence: &str, secret: &str) {
    if secret.len() < DISCLOSURE_FLOOR {
        return;
    }
    assert!(
        !sentence
            .to_ascii_lowercase()
            .contains(&secret.to_ascii_lowercase()),
        "case {case:?}: the refusal must not disclose {secret:?}, got {sentence:?}"
    );
}

/// `(case, the absent database name)` rows spanning how an unresolvable `DatabaseName` is
/// spelled. The first is the shape of the recorded deployment's generated name; the last sits
/// at the documented ceiling of `NameString`.
fn absent_databases() -> Vec<(&'static str, String)> {
    vec![
        (
            "a generated name the catalog never held",
            "athorigin_nodb_7c41e0b2a9d4".to_string(),
        ),
        ("a single-character name", "d".to_string()),
        ("a mixed-case name", "Athorigin_NoDB".to_string()),
        ("a name at the 255 ceiling", "d".repeat(MAX_NAME)),
    ]
}

/// A stack declaring an `AWS::Glue::Table` in a database that does not exist rolls back naming
/// that resource, and the catalog read a client then makes to confirm nothing was left behind
/// is refused with the TABLE read's one fixed sentence — the same bytes as a miss of the table
/// step of the address, for every spelling of the absent database and every request shape —
/// while the operation addressing the DATABASE keeps its own templated sentence.
#[tokio::test]
async fn a_rolled_back_glue_table_leaves_one_fixed_table_lookup_refusal() {
    let (router, engine) = build_router_and_engine();

    // The control: the database exists, so the same template deploys green and the table it
    // declared is really in the catalog.
    let (status, created) = glue(
        &router,
        REGION,
        "CreateDatabase",
        &json!({ "DatabaseInput": { "Name": CONTROL_DATABASE } }),
    )
    .await;
    assert_eq!(
        status, 200,
        "expected HTTP 200 from CreateDatabase, got {status} with body {created}"
    );
    let reached = deploy(
        &router,
        &engine,
        "the control stack",
        "athorigin-control-stack",
        CONTROL_DATABASE,
        CONTROL_TABLE,
    )
    .await;
    assert_eq!(
        reached, "CREATE_COMPLETE",
        "the control stack must deploy green, got {reached:?}"
    );
    assert_control_table_resolves(&router, "the control stack").await;

    // The TABLE step of the address missing, in the database that exists: the sentence every
    // row below has to answer.
    let table_step = miss_sentence(
        &router,
        "the table step of the address",
        &json!({ "DatabaseName": CONTROL_DATABASE, "Name": UNKNOWN_TABLE }),
    )
    .await;
    assert!(
        !table_step.is_empty(),
        "the table step's refusal must carry a sentence, got {table_step:?}"
    );

    let mut cases_run = 0usize;
    for (index, (case, absent)) in absent_databases().into_iter().enumerate() {
        let stack = format!("athorigin-nodb-stack-{index}");
        let reached = deploy(&router, &engine, case, &stack, &absent, CONTROL_TABLE).await;
        assert_eq!(
            reached, "ROLLBACK_COMPLETE",
            "case {case:?}: a table declared in a database that does not exist must roll the \
             stack back, got {reached:?}"
        );
        let refused = refused_resources(&router, case, &stack).await;
        let failed: Vec<&str> = refused
            .iter()
            .map(|(logical_id, _)| logical_id.as_str())
            .collect();
        assert_eq!(
            failed,
            vec![TABLE_LOGICAL_ID],
            "case {case:?}: the table must be the one resource recorded {CREATE_FAILED}, got \
             {refused:?}"
        );
        // The deployment's own refusal came from the operation that ADDRESSES the database, so
        // it carries that operation's templated sentence — rendered in the name the catalog
        // keys a database under, which is the folded one.
        let reason = &refused[0].1;
        assert!(
            reason.contains(&absent.to_ascii_lowercase()),
            "case {case:?}: the resource's {CREATE_FAILED} reason must relay the owning \
             service's refusal naming the database, got {reason:?}"
        );

        for shape in &request_shapes() {
            let read_case = format!("{case} / {label}", label = shape.label);
            let request = get_table_request(&absent, CONTROL_TABLE, shape);
            let sentence = miss_sentence(&router, &read_case, &request).await;
            assert_eq!(
                sentence, table_step,
                "case {read_case:?}: the database step of the address must answer the table \
                 step's sentence {table_step:?}, got {sentence:?}"
            );
            assert_withholds(&read_case, &sentence, &absent);
            assert_withholds(&read_case, &sentence, CONTROL_TABLE);
            assert_withholds(&read_case, &sentence, ACCOUNT);
            cases_run += 1;
        }

        // The boundary the rule is bounded by: the read that addresses the DATABASE itself is
        // refused with its own templated sentence, so the two refusals stay tellable apart on
        // the wire.
        let (status, body) = glue(&router, REGION, "GetDatabase", &json!({ "Name": absent })).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400 from GetDatabase for an absent database, got \
             {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            "EntityNotFoundException",
            "case {case:?}: expected EntityNotFoundException from GetDatabase, got {body}"
        );
        let database_sentence = error_message(&body);
        assert_eq!(
            database_sentence,
            format!("Database {} not found.", absent.to_ascii_lowercase()),
            "case {case:?}: GetDatabase must keep its own templated sentence, got \
             {database_sentence:?}"
        );
        assert_ne!(
            database_sentence, table_step,
            "case {case:?}: the database read's sentence must stay distinct from the table \
             read's constant {table_step:?}"
        );

        // A refused read changes nothing a later caller sees.
        assert_control_table_resolves(&router, case).await;
    }

    assert_eq!(
        cases_run,
        absent_databases().len() * request_shapes().len(),
        "every absent-database row and request shape must have been exercised, got {cases_run} \
         cases"
    );
}

/// The control table the control stack deployed still resolves through AWS Glue's own read, so
/// a constant refusal above cannot be a handler that refuses everything.
async fn assert_control_table_resolves(router: &MultiServiceRouter, case: &str) {
    let (status, body) = glue(
        router,
        REGION,
        "GetTable",
        &json!({ "DatabaseName": CONTROL_DATABASE, "Name": CONTROL_TABLE }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the table the control stack created must still resolve, got {status} \
         with body {body}"
    );
    assert_eq!(
        body.pointer("/Table/Name").and_then(Value::as_str),
        Some(CONTROL_TABLE),
        "case {case:?}: expected the control table to be reported, got {body}"
    );
    assert_eq!(
        body.pointer("/Table/DatabaseName").and_then(Value::as_str),
        Some(CONTROL_DATABASE),
        "case {case:?}: expected the control table's own database to be reported, got {body}"
    );
}
