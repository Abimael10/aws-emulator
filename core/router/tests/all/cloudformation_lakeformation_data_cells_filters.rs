// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation stack that reports `CREATE_COMPLETE` has really created the
//! `AWS::LakeFormation::DataCellsFilter` its template declares — in Lake Formation, on the
//! Data Catalog table the declaration names — and removing that resource deletes it there.
//!
//! The rule under test: **a resource type Amazon Web Services publishes is admitted by every
//! operation that submits a template, and a stack reports a resource of it as created only
//! where the service that owns it created it, through that service's own public API, under
//! the physical id the stack publishes.** For a data cells filter that id is the four values
//! Lake Formation keys one by — the catalog, the database, the table and the filter's own
//! name — so `{"Ref": "<logical id>"}` resolves to them joined, and a client that reads the
//! deployment back calls `lakeformation ListDataCellsFilter` on the parent table and finds
//! the filter the template described, rather than being told at submit time that the type is
//! not a resource type at all.
//!
//! Its other half: **a declaration the resource provider or the owning service REFUSES fails
//! that resource and rolls the stack back, quoting the refusal, rather than failing the
//! submitted template or reporting a filter nothing created.** A declaration missing one of
//! the four values that identify a filter is the provider's own model validation; a
//! declaration that states no row predicate, and one whose predicate names both of its
//! alternatives, are Lake Formation's own refusals — each leaving the stack
//! `ROLLBACK_COMPLETE` with nothing created in that service and the database the stack made
//! gone with it.
//!
//! Three crates have to agree for any of that to hold (CloudFormation makes the calls, Lake
//! Formation and AWS Glue serve them), so the suite lives here. Amazon S3 is mounted beside
//! them because the table a filter is declared on lives over a bucket the same template
//! declares.
//!
//! The rows differ along what the rule generalises over: the row-predicate form (the
//! all-rows wildcard, an ordinary expression), how the columns are named (a wildcard with no
//! exclusion, a wildcard with exclusions, an explicit list), how the catalog is named (the
//! account pseudo-parameter or a literal account id), the template spelling (JSON and YAML),
//! and the Region the stack is deployed in.
//!
//! The stack clock and the XML readers are the test crate's own
//! ([`crate::cloudformation_stack_clock`], [`crate::query_wire`]), not another copy.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_stack_clock::settle_the_stacks;
use crate::query_wire::{form_encode, opt_element, outputs_of, physical_ids_of};

/// The account id every ARN this emulator mints carries, which is also the Data Catalog id
/// a catalog call defaults to.
const ACCOUNT: &str = "000000000000";

/// The logical ids every row gives its resources, so each physical id is read from the
/// resource it belongs to.
const FILTER_LOGICAL_ID: &str = "Filter";
const DATABASE_LOGICAL_ID: &str = "Db";
const TABLE_LOGICAL_ID: &str = "Tbl";
const BUCKET_LOGICAL_ID: &str = "Bucket";

/// The output key every row's template publishes: the filter's `Ref`, which is how a client
/// reaches the deployed filter at all.
const FILTER_OUTPUT: &str = "FilterId";

/// The refusal a type CloudFormation does not know is answered with, which no row here may
/// ever see: the whole point of the rule is that a published type is not refused as a
/// template format error.
const UNRECOGNIZED_PREFIX: &str = "Template format error: Unrecognized resource types: [";

/// The separator CloudFormation joins the parts of a composite physical resource id with.
const COMPOSITE: &str = "|";

/// How many clock-driven passes a settling stack is given. Generous: what the suite waits
/// for is the CONDITION (a status that is not `…_IN_PROGRESS`), and each pass makes every
/// transition the store owes due NOW rather than waiting for it, so a slower host takes no
/// longer.
const SETTLE_PASSES: usize = 24;

/// One row: a deployment of one filter, and what the rule says Lake Formation must then
/// hold.
struct Row {
    case: &'static str,
    /// The Region the row deploys in, which is the Region the filter is created in.
    region: &'static str,
    /// The database, the table and the filter the row's template declares.
    database: &'static str,
    table: &'static str,
    filter: &'static str,
    /// The template body, built from the row's own names; `with_filter` false is the same
    /// template with ONLY the filter removed, which is what the update submits.
    template: fn(&Row, bool) -> String,
    /// The `RowFilter` Lake Formation must report for the filter, which is not always the
    /// one the template declared: a filter selecting every row is reported carrying both
    /// forms of that predicate.
    reported_row_filter: fn() -> Value,
    /// The column selection the declaration states, which is reported exactly as declared.
    reported_columns: fn() -> Vec<(&'static str, Value)>,
}

/// The rows: three deployments spanning the predicate forms, the column forms, the catalog
/// spellings, the template languages and the Regions.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "an all-rows wildcard over every column, naming the account \
                   pseudo-parameter, in JSON",
            region: "us-east-1",
            database: "lakehouse",
            table: "orders",
            filter: "all-rows",
            template: |row, with_filter| {
                let mut resources = json!({
                    BUCKET_LOGICAL_ID: { "Type": "AWS::S3::Bucket", "Properties": {} },
                    DATABASE_LOGICAL_ID: {
                        "Type": "AWS::Glue::Database",
                        "Properties": {
                            "CatalogId": { "Ref": "AWS::AccountId" },
                            "DatabaseInput": { "Name": row.database },
                        },
                    },
                    TABLE_LOGICAL_ID: {
                        "Type": "AWS::Glue::Table",
                        "Properties": {
                            "CatalogId": { "Ref": "AWS::AccountId" },
                            "DatabaseName": { "Ref": DATABASE_LOGICAL_ID },
                            "TableInput": {
                                "Name": row.table,
                                "TableType": "EXTERNAL_TABLE",
                                "StorageDescriptor": {
                                    "Location": {
                                        "Fn::Sub": format!(
                                            "s3://${{{BUCKET_LOGICAL_ID}}}/{}/",
                                            row.table
                                        ),
                                    },
                                    "Columns": [
                                        { "Name": "order_id", "Type": "bigint" },
                                        { "Name": "region", "Type": "string" },
                                    ],
                                },
                            },
                        },
                    },
                });
                let mut outputs = json!({});
                if with_filter {
                    resources[FILTER_LOGICAL_ID] = json!({
                        "Type": "AWS::LakeFormation::DataCellsFilter",
                        "DependsOn": [TABLE_LOGICAL_ID],
                        "Properties": {
                            "TableCatalogId": { "Ref": "AWS::AccountId" },
                            "DatabaseName": { "Ref": DATABASE_LOGICAL_ID },
                            "TableName": { "Ref": TABLE_LOGICAL_ID },
                            "Name": row.filter,
                            "RowFilter": { "AllRowsWildcard": {} },
                            "ColumnWildcard": { "ExcludedColumnNames": [] },
                        },
                    });
                    outputs[FILTER_OUTPUT] = json!({ "Value": { "Ref": FILTER_LOGICAL_ID } });
                }
                json!({
                    "AWSTemplateFormatVersion": "2010-09-09",
                    "Description": "a governed table and the cell-level filter on it",
                    "Resources": resources,
                    "Outputs": outputs,
                })
                .to_string()
            },
            // The all-rows wildcard and the `TRUE` expression say the same thing, and Lake
            // Formation reports both for a filter that selects every row.
            reported_row_filter: || json!({ "FilterExpression": "TRUE", "AllRowsWildcard": {} }),
            reported_columns: || vec![("ColumnWildcard", json!({ "ExcludedColumnNames": [] }))],
        },
        Row {
            case: "a PartiQL predicate over a wildcard with exclusions, in YAML",
            region: "us-east-2",
            database: "warehouse",
            table: "shipments",
            filter: "eu-only",
            template: |row, with_filter| {
                let filter = format!(
                    "  {FILTER_LOGICAL_ID}:
    Type: AWS::LakeFormation::DataCellsFilter
    DependsOn:
      - {TABLE_LOGICAL_ID}
    Properties:
      TableCatalogId: \"{ACCOUNT}\"
      DatabaseName: !Ref {DATABASE_LOGICAL_ID}
      TableName: !Ref {TABLE_LOGICAL_ID}
      Name: {filter}
      RowFilter:
        FilterExpression: country = 'DE'
      ColumnWildcard:
        ExcludedColumnNames:
          - ssn
Outputs:
  {FILTER_OUTPUT}:
    Value: !Ref {FILTER_LOGICAL_ID}
",
                    filter = row.filter,
                );
                format!(
                    "\
Resources:
  {BUCKET_LOGICAL_ID}:
    Type: AWS::S3::Bucket
  {DATABASE_LOGICAL_ID}:
    Type: AWS::Glue::Database
    Properties:
      CatalogId: \"{ACCOUNT}\"
      DatabaseInput:
        Name: {database}
  {TABLE_LOGICAL_ID}:
    Type: AWS::Glue::Table
    Properties:
      CatalogId: \"{ACCOUNT}\"
      DatabaseName: !Ref {DATABASE_LOGICAL_ID}
      TableInput:
        Name: {table}
        TableType: EXTERNAL_TABLE
        StorageDescriptor:
          Location: !Sub s3://${{{BUCKET_LOGICAL_ID}}}/{table}/
          Columns:
            - Name: shipment_id
              Type: bigint
            - Name: country
              Type: string
            - Name: ssn
              Type: string
{tail}",
                    database = row.database,
                    table = row.table,
                    tail = if with_filter { filter.as_str() } else { "" },
                )
            },
            reported_row_filter: || json!({ "FilterExpression": "country = 'DE'" }),
            reported_columns: || {
                vec![(
                    "ColumnWildcard",
                    json!({ "ExcludedColumnNames": ["ssn"] }),
                )]
            },
        },
        Row {
            case: "a PartiQL predicate over an explicit column list, in JSON",
            region: "eu-central-1",
            database: "analytics",
            table: "events",
            filter: "recent",
            template: |row, with_filter| {
                let mut resources = json!({
                    BUCKET_LOGICAL_ID: { "Type": "AWS::S3::Bucket", "Properties": {} },
                    DATABASE_LOGICAL_ID: {
                        "Type": "AWS::Glue::Database",
                        "Properties": {
                            "DatabaseInput": { "Name": row.database },
                        },
                    },
                    TABLE_LOGICAL_ID: {
                        "Type": "AWS::Glue::Table",
                        "Properties": {
                            "DatabaseName": { "Ref": DATABASE_LOGICAL_ID },
                            "TableInput": {
                                "Name": row.table,
                                "TableType": "EXTERNAL_TABLE",
                                "StorageDescriptor": {
                                    "Location": {
                                        "Fn::Sub": format!(
                                            "s3://${{{BUCKET_LOGICAL_ID}}}/{}/",
                                            row.table
                                        ),
                                    },
                                    "Columns": [
                                        { "Name": "event_id", "Type": "bigint" },
                                        { "Name": "at", "Type": "timestamp" },
                                    ],
                                },
                            },
                        },
                    },
                });
                let mut outputs = json!({});
                if with_filter {
                    resources[FILTER_LOGICAL_ID] = json!({
                        "Type": "AWS::LakeFormation::DataCellsFilter",
                        "DependsOn": [TABLE_LOGICAL_ID],
                        "Properties": {
                            "TableCatalogId": ACCOUNT,
                            "DatabaseName": { "Ref": DATABASE_LOGICAL_ID },
                            "TableName": { "Ref": TABLE_LOGICAL_ID },
                            "Name": row.filter,
                            "RowFilter": { "FilterExpression": "at > '2026-01-01'" },
                            "ColumnNames": ["event_id", "at"],
                        },
                    });
                    outputs[FILTER_OUTPUT] = json!({ "Value": { "Ref": FILTER_LOGICAL_ID } });
                }
                json!({ "Resources": resources, "Outputs": outputs }).to_string()
            },
            reported_row_filter: || json!({ "FilterExpression": "at > '2026-01-01'" }),
            reported_columns: || vec![("ColumnNames", json!(["event_id", "at"]))],
        },
    ]
}

/// The emulator with the four services this suite exercises, mounted on ONE engine exactly
/// as `core/router/src/lib.rs` mounts them. Request authentication is off, as in every other
/// suite here; the in-process callback CloudFormation makes signs its request either way
/// (the credential scope is what routes it).
fn build_router() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_lakeformation_awslakeformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_glue_awsglue::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
    (router, engine)
}

/// A SigV4 `Authorization` header whose credential scope names `service` in `region`, which
/// is what routes the request and what each handler reads its Region from. The signature is
/// never checked: this router runs with request authentication off.
fn authorization(service: &str, region: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{region}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// Call one CloudFormation operation over its awsQuery wire, signed for `region`.
async fn cloudformation(
    router: &MultiServiceRouter,
    region: &str,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-05-15");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("cloudformation.{region}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("cloudformation", region))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// Call one Lake Formation operation over its `restJson1` wire, on the URI that operation's
/// own `@http` binding declares (`POST /<operation>`), through `region`'s endpoint.
async fn lakeformation(
    router: &MultiServiceRouter,
    region: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("/{operation}"))
        .header("host", format!("lakeformation.{region}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("lakeformation", region))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, payload)
}

/// Call one AWS Glue operation over its awsJson1_1 wire, through `region`'s endpoint.
async fn glue(
    router: &MultiServiceRouter,
    region: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("glue.{region}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("AWSGlue.{operation}"))
        .header("authorization", authorization("glue", region))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, payload)
}

/// The stack as it reads once its operation has reached a TERMINAL status: each pass makes
/// every transition the store owes due NOW and then reads, so the case drives the stack's own
/// clock rather than waiting for it.
async fn settled_stack(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    case: &str,
    region: &str,
    stack: &str,
) -> String {
    for _ in 0..SETTLE_PASSES {
        settle_the_stacks(router, engine, case).await;
        let (status, described) =
            cloudformation(router, region, "DescribeStacks", &[("StackName", stack)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeStacks answered {status} with body {described}"
        );
        if !opt_element(&described, "StackStatus")
            .unwrap_or_default()
            .ends_with("_IN_PROGRESS")
        {
            return described;
        }
    }
    panic!("case {case:?}: the stack {stack:?} never reached a terminal status");
}

/// The filters Lake Formation reports on one table, as a client that deployed the stack
/// reads them.
async fn filters_on(
    router: &MultiServiceRouter,
    region: &str,
    catalog: &str,
    database: &str,
    table: &str,
) -> (u16, Vec<Value>) {
    let (status, payload) = lakeformation(
        router,
        region,
        "ListDataCellsFilter",
        &json!({
            "Table": { "CatalogId": catalog, "DatabaseName": database, "Name": table },
        }),
    )
    .await;
    let reported = payload
        .get("DataCellsFilters")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    (status, reported)
}

/// The error code of a JSON error envelope, read from the member the AWS JSON families name
/// the error shape in.
fn error_code(payload: &Value) -> String {
    payload
        .get("__type")
        .or_else(|| payload.get("Code"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .rsplit(['#', ':'])
        .next()
        .unwrap_or_default()
        .to_string()
}

/// Every row: the stack is admitted, completes, publishes the composite identity Lake
/// Formation keys the filter by, and the filter that service reports on the table is the one
/// the declaration asked for — then an update that drops only the filter deletes it there
/// while leaving the table, and deleting the stack takes the database with it.
#[tokio::test]
async fn a_stack_creates_the_data_cells_filter_it_declares_in_lake_formation() {
    for (index, row) in rows().into_iter().enumerate() {
        let (router, engine) = build_router();
        let case = row.case;
        let region = row.region;
        let stack_name = format!("lakeformation-filter-stack-{index}");
        let template = (row.template)(&row, true);

        let (status, body) = cloudformation(
            &router,
            region,
            "CreateStack",
            &[("StackName", &stack_name), ("TemplateBody", &template)],
        )
        .await;
        let message = opt_element(&body, "Message").unwrap_or_default();
        assert!(
            !message.starts_with(UNRECOGNIZED_PREFIX),
            "case {case:?}: expected a type Amazon Web Services publishes to be recognised, \
             got {message:?}"
        );
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {body}"
        );

        let described = settled_stack(&router, &engine, case, region, &stack_name).await;
        assert_eq!(
            opt_element(&described, "StackStatus").as_deref(),
            Some("CREATE_COMPLETE"),
            "case {case:?}: expected the stack to complete, got {described}"
        );

        // The identity the stack publishes is the one every Lake Formation call on the
        // filter takes: the catalog, the database, the table and the filter's own name.
        let identity = format!(
            "{ACCOUNT}{COMPOSITE}{}{COMPOSITE}{}{COMPOSITE}{}",
            row.database, row.table, row.filter
        );
        let outputs = outputs_of(&described);
        assert_eq!(
            outputs.get(FILTER_OUTPUT).map(String::as_str),
            Some(identity.as_str()),
            "case {case:?}: expected Ref to be {identity:?}, got {outputs:?}"
        );
        let (_, resources) = cloudformation(
            &router,
            region,
            "DescribeStackResources",
            &[("StackName", &stack_name)],
        )
        .await;
        let physical = physical_ids_of(&resources);
        assert_eq!(
            physical.get(FILTER_LOGICAL_ID).map(String::as_str),
            Some(identity.as_str()),
            "case {case:?}: expected the resource's physical id to be {identity:?}, got \
             {physical:?}"
        );

        // The filter really is in Lake Formation, on the table the same stack created, with
        // every property the declaration stated.
        let (status, reported) =
            filters_on(&router, region, ACCOUNT, row.database, row.table).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from ListDataCellsFilter, got {status} with \
             body {reported:?}"
        );
        assert_eq!(
            reported.len(),
            1,
            "case {case:?}: expected exactly the declared filter, got {reported:?}"
        );
        let filter = &reported[0];
        let mut expected = json!({
            "TableCatalogId": ACCOUNT,
            "DatabaseName": row.database,
            "TableName": row.table,
            "Name": row.filter,
            "RowFilter": (row.reported_row_filter)(),
        });
        for (member, value) in (row.reported_columns)() {
            expected[member] = value;
        }
        for (member, value) in expected.as_object().expect("an object") {
            assert_eq!(
                filter.get(member),
                Some(value),
                "case {case:?}: expected the filter's {member} to be {value}, got {filter}"
            );
        }

        // An update that drops ONLY the filter deletes it in Lake Formation and leaves the
        // table it was on: the stack created the filter, and the table outlives it.
        let without_the_filter = (row.template)(&row, false);
        let (status, body) = cloudformation(
            &router,
            region,
            "UpdateStack",
            &[
                ("StackName", &stack_name),
                ("TemplateBody", &without_the_filter),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from UpdateStack, got {status} with body {body}"
        );
        let described = settled_stack(&router, &engine, case, region, &stack_name).await;
        assert_eq!(
            opt_element(&described, "StackStatus").as_deref(),
            Some("UPDATE_COMPLETE"),
            "case {case:?}: expected the update to complete, got {described}"
        );
        let (status, reported) =
            filters_on(&router, region, ACCOUNT, row.database, row.table).await;
        assert_eq!(
            (status, reported.as_slice()),
            (200, [].as_slice()),
            "case {case:?}: expected the dropped filter to be deleted in Lake Formation, got \
             {status} {reported:?}"
        );
        let (status, table) = glue(
            &router,
            region,
            "GetTable",
            &json!({ "DatabaseName": row.database, "Name": row.table }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the table the filter was on to outlive it, got {status} \
             with body {table}"
        );

        // And deleting the stack takes the catalog entities it created with it.
        let (status, body) =
            cloudformation(&router, region, "DeleteStack", &[("StackName", &stack_name)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DeleteStack, got {status} with body {body}"
        );
        for _ in 0..SETTLE_PASSES {
            settle_the_stacks(&router, &engine, case).await;
        }
        let (status, gone) = glue(
            &router,
            region,
            "GetDatabase",
            &json!({ "Name": row.database }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected the deleted stack to take its database, got {status} \
             with body {gone}"
        );
        assert_eq!(
            error_code(&gone),
            "EntityNotFoundException",
            "case {case:?}: expected EntityNotFoundException for the deleted database, got \
             {gone}"
        );
    }
}

/// A declaration the provider or the owning service REFUSES fails that resource and rolls
/// the stack back, quoting the refusal, with nothing created in Lake Formation — rather than
/// refusing the submitted template, which is what a type CloudFormation does not recognise
/// earns.
///
/// The rows differ along WHO refuses and WHY: a declaration missing a value that identifies
/// the filter (the provider's own model validation, before anything is asked of Lake
/// Formation), a declaration stating no row predicate at all, and one whose predicate names
/// both of its alternatives (that service's own refusals, each with its own fixed sentence).
#[tokio::test]
async fn a_data_cells_filter_declaration_that_is_refused_fails_the_stack() {
    // (case, the filter's declared properties beside its database and table, the sentence
    // the resource's failure must carry).
    let rows: Vec<(&str, Value, &str)> = vec![
        (
            "a declaration that states no filter name",
            json!({
                "TableCatalogId": ACCOUNT,
                "RowFilter": { "AllRowsWildcard": {} },
                "ColumnWildcard": {},
            }),
            "Model validation failed (#: required key [Name] not found)",
        ),
        (
            "a declaration that states no row predicate",
            json!({
                "TableCatalogId": ACCOUNT,
                "Name": "no-rows",
                "ColumnWildcard": { "ExcludedColumnNames": [] },
            }),
            "Row filter cannot be null",
        ),
        (
            "a predicate naming both of its alternatives",
            json!({
                "TableCatalogId": ACCOUNT,
                "Name": "both-rows",
                "RowFilter": { "FilterExpression": "country = 'DE'", "AllRowsWildcard": {} },
                "ColumnWildcard": {},
            }),
            "Filter expression and AllRowsWildcard cannot both be set.",
        ),
    ];

    for (index, (case, declared, expected_reason)) in rows.into_iter().enumerate() {
        let (router, engine) = build_router();
        let region = "us-west-2";
        let database = "refusals";
        let table = "ledger";
        let stack_name = format!("lakeformation-filter-refusal-stack-{index}");

        let mut properties = declared.clone();
        properties["DatabaseName"] = json!({ "Ref": DATABASE_LOGICAL_ID });
        properties["TableName"] = json!({ "Ref": TABLE_LOGICAL_ID });
        let template = json!({
            "Resources": {
                DATABASE_LOGICAL_ID: {
                    "Type": "AWS::Glue::Database",
                    "Properties": { "DatabaseInput": { "Name": database } },
                },
                TABLE_LOGICAL_ID: {
                    "Type": "AWS::Glue::Table",
                    "Properties": {
                        "DatabaseName": { "Ref": DATABASE_LOGICAL_ID },
                        "TableInput": {
                            "Name": table,
                            "TableType": "EXTERNAL_TABLE",
                            "StorageDescriptor": {
                                "Columns": [{ "Name": "amount", "Type": "double" }],
                            },
                        },
                    },
                },
                FILTER_LOGICAL_ID: {
                    "Type": "AWS::LakeFormation::DataCellsFilter",
                    "DependsOn": [TABLE_LOGICAL_ID],
                    "Properties": properties,
                },
            },
        })
        .to_string();

        let (status, body) = cloudformation(
            &router,
            region,
            "CreateStack",
            &[("StackName", &stack_name), ("TemplateBody", &template)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the template to be ADMITTED and the refusal to be the \
             resource's, got {status} with body {body}"
        );

        let described = settled_stack(&router, &engine, case, region, &stack_name).await;
        assert_eq!(
            opt_element(&described, "StackStatus").as_deref(),
            Some("ROLLBACK_COMPLETE"),
            "case {case:?}: expected the failed create to be rolled back, got {described}"
        );
        let reason = opt_element(&described, "StackStatusReason").unwrap_or_default();
        assert!(
            reason.contains(FILTER_LOGICAL_ID),
            "case {case:?}: expected the failure to name the resource, got {reason:?}"
        );

        let (_, events) = cloudformation(
            &router,
            region,
            "DescribeStackEvents",
            &[("StackName", &stack_name)],
        )
        .await;
        assert!(
            events.contains(expected_reason),
            "case {case:?}: expected the resource's failure to carry {expected_reason:?}, got \
             {events}"
        );

        // Nothing was created in Lake Formation for the failed resource, and the catalog
        // entities the rolled-back stack had created are gone with it.
        let (status, reported) = filters_on(&router, region, ACCOUNT, database, table).await;
        assert_eq!(
            (status, reported.as_slice()),
            (200, [].as_slice()),
            "case {case:?}: expected no filter to be left behind, got {status} {reported:?}"
        );
        let (status, gone) = glue(&router, region, "GetDatabase", &json!({ "Name": database })).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected the rolled-back stack to take its database, got {status} \
             with body {gone}"
        );
    }
}

/// The refusal Lake Formation itself worded reaches the failed resource's event as the SDK
/// exception a resource provider reports: that service's own sentence, the diagnostic its
/// SDK appends to a refused call, and the `HandlerErrorCode` class the refusal falls in.
///
/// The per-call parts of that diagnostic — the request id and the attempt count — are not
/// asserted as values: what the rule fixes is the frame around them.
#[tokio::test]
async fn the_services_own_refusal_is_quoted_in_the_failed_resources_reason() {
    let (router, engine) = build_router();
    let case = "a filter declared without a row predicate";
    let region = "ap-northeast-1";
    let database = "quoted";
    let table = "ledger";
    let stack_name = "lakeformation-filter-quoted-refusal";
    let template = json!({
        "Resources": {
            DATABASE_LOGICAL_ID: {
                "Type": "AWS::Glue::Database",
                "Properties": { "DatabaseInput": { "Name": database } },
            },
            TABLE_LOGICAL_ID: {
                "Type": "AWS::Glue::Table",
                "Properties": {
                    "DatabaseName": { "Ref": DATABASE_LOGICAL_ID },
                    "TableInput": {
                        "Name": table,
                        "TableType": "EXTERNAL_TABLE",
                        "StorageDescriptor": {
                            "Columns": [{ "Name": "amount", "Type": "double" }],
                        },
                    },
                },
            },
            FILTER_LOGICAL_ID: {
                "Type": "AWS::LakeFormation::DataCellsFilter",
                "DependsOn": [TABLE_LOGICAL_ID],
                "Properties": {
                    "TableCatalogId": ACCOUNT,
                    "DatabaseName": { "Ref": DATABASE_LOGICAL_ID },
                    "TableName": { "Ref": TABLE_LOGICAL_ID },
                    "Name": "no-rows",
                    "ColumnWildcard": { "ExcludedColumnNames": [] },
                },
            },
        },
    })
    .to_string();

    let (status, body) = cloudformation(
        &router,
        region,
        "CreateStack",
        &[("StackName", stack_name), ("TemplateBody", &template)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {body}"
    );
    let described = settled_stack(&router, &engine, case, region, stack_name).await;
    assert_eq!(
        opt_element(&described, "StackStatus").as_deref(),
        Some("ROLLBACK_COMPLETE"),
        "case {case:?}: expected the stack to roll back, got {described}"
    );

    let (_, events) = cloudformation(
        &router,
        region,
        "DescribeStackEvents",
        &[("StackName", stack_name)],
    )
    .await;
    // The sentence, the SDK's frame around it naming the service and the status, the attempt
    // count that frame closes with, and the class a refused request falls in.
    for fragment in [
        "Row filter cannot be null (Service: LakeFormation, Status Code: 400, Request ID: ",
        ") (SDK Attempt Count: ",
        "HandlerErrorCode: InvalidRequest",
    ] {
        assert!(
            events.contains(fragment),
            "case {case:?}: expected the failed resource's reason to carry {fragment:?}, got \
             {events}"
        );
    }
}
