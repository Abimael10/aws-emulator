// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation stack that reports `CREATE_COMPLETE` has really registered the
//! `AWS::Glue::TableOptimizer` its template declares — in AWS Glue, on the catalog table the
//! declaration names, under the optimizer type it states — and deleting the stack releases it
//! there.
//!
//! The rule under test: **a resource type Amazon Web Services publishes is admitted by every
//! operation that submits a template, and a stack reports a resource of it as created only
//! where the service that owns it created it, through that service's own public API, under the
//! physical id the stack publishes.** For an optimizer that id is the four values AWS Glue
//! keys one by — the catalog, the database, the table and the optimizer's own type — so
//! `{"Ref": "<logical id>"}` resolves to them joined and a client that reads the deployment
//! back calls `glue GetTableOptimizer` with values that service answers for, rather than being
//! told at submit time that the type is not a resource type at all.
//!
//! Its other half: **a declaration the resource provider or the owning service REFUSES fails
//! that resource and rolls the stack back, rather than failing the submitted template or
//! reporting an optimizer nothing registered.** A `Type` outside the set that service
//! enumerates is the provider's own model validation
//! (`Model validation failed (#/Type: failed validation constraint for keyword [enum])`), and a
//! table the Data Catalog does not hold is AWS Glue's own `EntityNotFoundException`: both leave
//! the stack `ROLLBACK_COMPLETE` naming the resource, with nothing registered in that service.
//!
//! Two crates have to agree for any of that to hold (CloudFormation makes the calls, AWS Glue
//! serves them), so the suite lives here. IAM is mounted beside them because the role an
//! optimizer runs as is the other half of the template an ordinary deploy writes.
//!
//! The rows differ along what the rule generalises over: which of the three optimizer types
//! the declaration states, what its configuration carries (the role alone, a retention
//! configuration, an orphan-file-deletion configuration, enabled and disabled), how the
//! catalog is named (a literal account id or the account pseudo-parameter), the template
//! spelling (JSON and YAML), and the Region the stack is deployed in.
//!
//! The router, the two wire helpers over it, the stack clock and the XML readers are the test
//! crate's own ([`crate::cloudformation_provisions_glue_jobs`],
//! [`crate::cloudformation_stack_clock`], [`crate::query_wire`]), not another copy.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_provisions_glue_jobs::{
    build_router_and_engine, cloudformation, error_code, glue, ACCOUNT, TRUST_POLICY,
};
use crate::cloudformation_stack_clock::settle_the_stacks;
use crate::query_wire::{opt_element, outputs_of, physical_ids_of};

/// The logical ids every row gives its two resources, so each physical id is read from the
/// resource it belongs to.
const OPTIMIZER_LOGICAL_ID: &str = "TableOptimizer";
const ROLE_LOGICAL_ID: &str = "OptimizerRole";

/// The output keys every row's template publishes: the optimizer's `Ref` — which is how a
/// client reaches the deployed optimizer at all — and the ARN of the role it runs as, which
/// only IAM can mint.
const OPTIMIZER_OUTPUT: &str = "OptimizerIdentity";
const ROLE_OUTPUT: &str = "OptimizerRoleArn";

/// The refusal a type CloudFormation does not know is answered with, which no row here may
/// ever see: the whole point of the rule is that a published type is not refused as a template
/// format error.
const UNRECOGNIZED_PREFIX: &str = "Template format error: Unrecognized resource types: [";

/// What the resource provider's own model validation answers for a `Type` outside the set AWS
/// Glue enumerates: the schema validator's sentence, at the location of that property.
const ENUM_VALIDATION_FAILED: &str =
    "Model validation failed (#/Type: failed validation constraint for keyword [enum])";

/// The separator CloudFormation joins the parts of a composite physical resource id with.
const COMPOSITE: &str = "|";

/// One member `GetTableOptimizer` must report for a row's optimizer: a JSON pointer into the
/// reported `TableOptimizer`, and the value the row's declaration asks for.
type ReportedMember = (&'static str, fn() -> Value);

/// One row: a deployment of one optimizer, and what the rule says AWS Glue must then hold.
struct Row {
    case: &'static str,
    /// The Region the row deploys in, which is the Region the optimizer is registered in.
    region: &'static str,
    /// The database and table the optimizer is declared on, created in AWS Glue before the
    /// stack is submitted — the state a client's catalog is in when it writes the template.
    database: &'static str,
    table: &'static str,
    /// How the declaration names the catalog, and what the catalog resolves to.
    declared_catalog: &'static str,
    catalog_id: &'static str,
    /// The optimizer type the declaration states, which is part of the resource's identity.
    optimizer_type: &'static str,
    /// The template body, built from the row's own names and the ARN expression for the role.
    template: fn(&Row) -> String,
    /// The members `GetTableOptimizer` must report for the optimizer, as `(JSON pointer into
    /// TableOptimizer, value)` pairs — beside the role, which is compared against the ARN IAM
    /// minted.
    expected_members: &'static [ReportedMember],
}

/// The trust policy every row's role carries, as the inline object a JSON template declares it
/// with: AWS Glue has to be able to assume the role an optimizer runs as.
fn trust_policy() -> Value {
    serde_json::from_str(TRUST_POLICY).expect("the trust policy is a document")
}

/// The rows: three deployments spanning the optimizer types AWS Glue enumerates, the
/// configurations each of them takes, how the catalog is named, the template spelling and the
/// Region.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a compaction optimizer naming the account pseudo-parameter, in JSON",
            region: "us-east-1",
            database: "lakehouse",
            table: "orders",
            declared_catalog: "{\"Ref\": \"AWS::AccountId\"}",
            catalog_id: ACCOUNT,
            optimizer_type: "compaction",
            template: |row| {
                json!({
                    "AWSTemplateFormatVersion": "2010-09-09",
                    "Description": "an Iceberg table optimizer and the role it runs as",
                    "Resources": {
                        ROLE_LOGICAL_ID: {
                            "Type": "AWS::IAM::Role",
                            "Properties": { "AssumeRolePolicyDocument": trust_policy() },
                        },
                        OPTIMIZER_LOGICAL_ID: {
                            "Type": "AWS::Glue::TableOptimizer",
                            "Properties": {
                                "CatalogId": { "Ref": "AWS::AccountId" },
                                "DatabaseName": row.database,
                                "TableName": row.table,
                                "Type": row.optimizer_type,
                                "TableOptimizerConfiguration": {
                                    "RoleArn": { "Fn::GetAtt": [ROLE_LOGICAL_ID, "Arn"] },
                                    "Enabled": true,
                                },
                            },
                        },
                    },
                    "Outputs": {
                        OPTIMIZER_OUTPUT: { "Value": { "Ref": OPTIMIZER_LOGICAL_ID } },
                        ROLE_OUTPUT: {
                            "Value": { "Fn::GetAtt": [ROLE_LOGICAL_ID, "Arn"] },
                        },
                    },
                })
                .to_string()
            },
            expected_members: &[
                ("/type", || json!("compaction")),
                ("/configuration/enabled", || json!(true)),
            ],
        },
        Row {
            case: "a retention optimizer with an Iceberg retention configuration, in YAML",
            region: "us-east-2",
            database: "warehouse",
            table: "shipments",
            declared_catalog: ACCOUNT,
            catalog_id: ACCOUNT,
            optimizer_type: "retention",
            template: |row| {
                format!(
                    "\
Resources:
  {ROLE_LOGICAL_ID}:
    Type: AWS::IAM::Role
    Properties:
      AssumeRolePolicyDocument: {TRUST_POLICY}
  {OPTIMIZER_LOGICAL_ID}:
    Type: AWS::Glue::TableOptimizer
    Properties:
      CatalogId: \"{catalog}\"
      DatabaseName: {database}
      TableName: {table}
      Type: {optimizer_type}
      TableOptimizerConfiguration:
        RoleArn: !GetAtt {ROLE_LOGICAL_ID}.Arn
        Enabled: true
        RetentionConfiguration:
          IcebergConfiguration:
            SnapshotRetentionPeriodInDays: 7
            NumberOfSnapshotsToRetain: 3
            CleanExpiredFiles: true
Outputs:
  {OPTIMIZER_OUTPUT}:
    Value: !Ref {OPTIMIZER_LOGICAL_ID}
  {ROLE_OUTPUT}:
    Value: !GetAtt {ROLE_LOGICAL_ID}.Arn
",
                    catalog = row.declared_catalog,
                    database = row.database,
                    table = row.table,
                    optimizer_type = row.optimizer_type,
                )
            },
            expected_members: &[
                ("/type", || json!("retention")),
                ("/configuration/enabled", || json!(true)),
                (
                    "/configuration/retentionConfiguration/icebergConfiguration/\
                     snapshotRetentionPeriodInDays",
                    || json!(7),
                ),
                (
                    "/configuration/retentionConfiguration/icebergConfiguration/\
                     numberOfSnapshotsToRetain",
                    || json!(3),
                ),
                (
                    "/configuration/retentionConfiguration/icebergConfiguration/\
                     cleanExpiredFiles",
                    || json!(true),
                ),
            ],
        },
        Row {
            case: "an orphan-file-deletion optimizer declared disabled, in JSON",
            region: "eu-central-1",
            database: "analytics",
            table: "events",
            declared_catalog: ACCOUNT,
            catalog_id: ACCOUNT,
            optimizer_type: "orphan_file_deletion",
            template: |row| {
                json!({
                    "Resources": {
                        ROLE_LOGICAL_ID: {
                            "Type": "AWS::IAM::Role",
                            "Properties": { "AssumeRolePolicyDocument": trust_policy() },
                        },
                        OPTIMIZER_LOGICAL_ID: {
                            "Type": "AWS::Glue::TableOptimizer",
                            "Properties": {
                                "CatalogId": row.declared_catalog,
                                "DatabaseName": row.database,
                                "TableName": row.table,
                                "Type": row.optimizer_type,
                                "TableOptimizerConfiguration": {
                                    "RoleArn": { "Fn::GetAtt": [ROLE_LOGICAL_ID, "Arn"] },
                                    "Enabled": false,
                                    "OrphanFileDeletionConfiguration": {
                                        "IcebergConfiguration": {
                                            "OrphanFileRetentionPeriodInDays": 5,
                                            "Location": "s3://sim2real-lake/events/data",
                                        },
                                    },
                                },
                            },
                        },
                    },
                    "Outputs": {
                        OPTIMIZER_OUTPUT: { "Value": { "Ref": OPTIMIZER_LOGICAL_ID } },
                        ROLE_OUTPUT: {
                            "Value": { "Fn::GetAtt": [ROLE_LOGICAL_ID, "Arn"] },
                        },
                    },
                })
                .to_string()
            },
            expected_members: &[
                ("/type", || json!("orphan_file_deletion")),
                ("/configuration/enabled", || json!(false)),
                (
                    "/configuration/orphanFileDeletionConfiguration/icebergConfiguration/\
                     orphanFileRetentionPeriodInDays",
                    || json!(5),
                ),
                (
                    "/configuration/orphanFileDeletionConfiguration/icebergConfiguration/\
                     location",
                    || json!("s3://sim2real-lake/events/data"),
                ),
            ],
        },
    ]
}

/// The emulator this suite drives, with the services the rows reach mounted on ONE engine:
/// CloudFormation, AWS Glue and IAM come from the suite beside this one, and Amazon S3 is
/// mounted here because the table an optimizer runs on lives over a bucket a template of an
/// Iceberg estate declares in the same stack.
fn build_router_with_storage() -> (MultiServiceRouter, std::sync::Arc<engine::InMemoryEngine>) {
    let (mut router, engine) = build_router_and_engine();
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        std::sync::Arc::clone(&engine),
    ));
    (router, engine)
}

/// Create, through AWS Glue's own API, the database and the open-table-format table a row's
/// optimizer is declared on: an optimizer is a resource OF a table, so the table is what the
/// client's catalog holds before it submits the stack.
async fn given_an_iceberg_table(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    database: &str,
    table: &str,
) {
    let (status, body) = glue(
        router,
        region,
        "CreateDatabase",
        &json!({ "DatabaseInput": { "Name": database } }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateDatabase, got {status} with body {body}"
    );
    let (status, body) = glue(
        router,
        region,
        "CreateTable",
        &json!({
            "DatabaseName": database,
            "TableInput": {
                "Name": table,
                "StorageDescriptor": {
                    "Location": format!("s3://sim2real-lake/{database}/{table}"),
                    "Columns": [{ "Name": "id", "Type": "bigint" }],
                },
            },
            "OpenTableFormatInput": {
                "IcebergInput": { "MetadataOperation": "CREATE", "Version": "2" },
            },
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateTable, got {status} with body {body}"
    );
}

/// Read one optimizer back out of AWS Glue, as a client that deployed the stack does.
async fn read_optimizer(
    router: &MultiServiceRouter,
    region: &str,
    catalog_id: &str,
    database: &str,
    table: &str,
    optimizer_type: &str,
) -> (u16, Value) {
    glue(
        router,
        region,
        "GetTableOptimizer",
        &json!({
            "CatalogId": catalog_id,
            "DatabaseName": database,
            "TableName": table,
            "Type": optimizer_type,
        }),
    )
    .await
}

/// Every row: the stack is admitted, completes, publishes the composite identity AWS Glue keys
/// the optimizer by, and the optimizer that service holds is the one the declaration asked
/// for — then deleting the stack releases it while leaving the table it was registered on.
#[tokio::test]
async fn a_stack_registers_the_table_optimizer_it_declares_in_glue() {
    for (index, row) in rows().into_iter().enumerate() {
        let (router, engine) = build_router_and_engine();
        let case = row.case;
        let region = row.region;
        given_an_iceberg_table(&router, case, region, row.database, row.table).await;

        let stack_name = format!("glue-optimizer-stack-{index}");
        let template = (row.template)(&row);
        let (status, body) = cloudformation(
            &router,
            region,
            "CreateStack",
            &[
                ("StackName", &stack_name),
                ("TemplateBody", &template),
                ("Capabilities.member.1", "CAPABILITY_IAM"),
            ],
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
        settle_the_stacks(&router, &engine, case).await;

        let (_, described) =
            cloudformation(&router, region, "DescribeStacks", &[("StackName", &stack_name)]).await;
        assert_eq!(
            opt_element(&described, "StackStatus").as_deref(),
            Some("CREATE_COMPLETE"),
            "case {case:?}: expected the stack to complete, got {described}"
        );

        // The identity the stack publishes is the one every AWS Glue call on the optimizer
        // takes: the catalog, the database, the table and the optimizer's own type.
        let identity = format!(
            "{}{COMPOSITE}{}{COMPOSITE}{}{COMPOSITE}{}",
            row.catalog_id, row.database, row.table, row.optimizer_type
        );
        let outputs = outputs_of(&described);
        assert_eq!(
            outputs.get(OPTIMIZER_OUTPUT).map(String::as_str),
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
            physical.get(OPTIMIZER_LOGICAL_ID).map(String::as_str),
            Some(identity.as_str()),
            "case {case:?}: expected the resource's physical id to be {identity:?}, got \
             {physical:?}"
        );

        // The optimizer really is in AWS Glue, with the configuration the template declared
        // and the role ARN only IAM could have minted.
        let role_arn = outputs
            .get(ROLE_OUTPUT)
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: no {ROLE_OUTPUT} output in {described}"));
        let (status, read) = read_optimizer(
            &router,
            region,
            row.catalog_id,
            row.database,
            row.table,
            row.optimizer_type,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from GetTableOptimizer, got {status} with body \
             {read}"
        );
        let optimizer = read
            .get("TableOptimizer")
            .cloned()
            .unwrap_or(Value::Null);
        assert_eq!(
            optimizer
                .pointer("/configuration/roleArn")
                .and_then(Value::as_str),
            Some(role_arn.as_str()),
            "case {case:?}: expected the optimizer to run as the role the template passed \
             ({role_arn:?}), got {optimizer}"
        );
        for (pointer, expected) in row.expected_members {
            let expected = expected();
            let actual = optimizer.pointer(pointer).cloned().unwrap_or(Value::Null);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected TableOptimizer{pointer} to be {expected}, got \
                 {actual} in {optimizer}"
            );
        }

        // Deleting the stack releases the optimizer AWS Glue holds for it, and leaves the
        // TABLE it was registered on: the stack created the optimizer, not the table.
        let (status, body) =
            cloudformation(&router, region, "DeleteStack", &[("StackName", &stack_name)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DeleteStack, got {status} with body {body}"
        );
        settle_the_stacks(&router, &engine, case).await;
        let (status, gone) = read_optimizer(
            &router,
            region,
            row.catalog_id,
            row.database,
            row.table,
            row.optimizer_type,
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400 from GetTableOptimizer once the stack deleted \
             the optimizer, got {status} with body {gone}"
        );
        assert_eq!(
            error_code(&gone),
            "EntityNotFoundException",
            "case {case:?}: expected EntityNotFoundException once the optimizer was deleted, \
             got {gone}"
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
            "case {case:?}: expected the table the optimizer was on to outlive the stack, got \
             {status} with body {table}"
        );
    }
}

/// The whole estate in ONE stack, which is the shape a client writing IaC for an Iceberg
/// lakehouse writes: the S3 bucket the table's data lives in, the Data Catalog database, the
/// open-table-format table over that bucket's location, the role the optimizer runs as, and the
/// optimizer itself — each resolving the one before it, so CloudFormation creates them in
/// dependency order and the optimizer is registered on the table the same stack just created.
#[tokio::test]
async fn one_stack_creates_the_table_and_the_optimizer_on_it() {
    let (router, engine) = build_router_with_storage();
    let case = "a bucket, a database, an Iceberg table and the optimizer on it in one stack";
    let region = "ap-northeast-1";
    let stack_name = "glue-optimizer-whole-estate";
    let database = "estate";
    let table = "trips";
    let template = json!({
        "Resources": {
            "Bucket": { "Type": "AWS::S3::Bucket", "Properties": {} },
            ROLE_LOGICAL_ID: {
                "Type": "AWS::IAM::Role",
                "Properties": { "AssumeRolePolicyDocument": trust_policy() },
            },
            "Db": {
                "Type": "AWS::Glue::Database",
                "Properties": {
                    "CatalogId": { "Ref": "AWS::AccountId" },
                    "DatabaseInput": { "Name": database },
                },
            },
            "Tbl": {
                "Type": "AWS::Glue::Table",
                "Properties": {
                    "CatalogId": { "Ref": "AWS::AccountId" },
                    "DatabaseName": { "Ref": "Db" },
                    "TableInput": {
                        "Name": table,
                        "TableType": "EXTERNAL_TABLE",
                        "StorageDescriptor": {
                            "Location": {
                                "Fn::Sub": format!("s3://${{Bucket}}/{table}/"),
                            },
                            "Columns": [{ "Name": "id", "Type": "bigint" }],
                        },
                    },
                    "OpenTableFormatInput": {
                        "IcebergInput": { "MetadataOperation": "CREATE", "Version": "2" },
                    },
                },
            },
            OPTIMIZER_LOGICAL_ID: {
                "Type": "AWS::Glue::TableOptimizer",
                "DependsOn": ["Tbl"],
                "Properties": {
                    "CatalogId": { "Ref": "AWS::AccountId" },
                    "DatabaseName": { "Ref": "Db" },
                    "TableName": { "Ref": "Tbl" },
                    "Type": "compaction",
                    "TableOptimizerConfiguration": {
                        "RoleArn": { "Fn::GetAtt": [ROLE_LOGICAL_ID, "Arn"] },
                        "Enabled": true,
                    },
                },
            },
        },
        "Outputs": {
            OPTIMIZER_OUTPUT: { "Value": { "Ref": OPTIMIZER_LOGICAL_ID } },
            ROLE_OUTPUT: { "Value": { "Fn::GetAtt": [ROLE_LOGICAL_ID, "Arn"] } },
        },
    })
    .to_string();

    let (status, body) = cloudformation(
        &router,
        region,
        "CreateStack",
        &[
            ("StackName", stack_name),
            ("TemplateBody", &template),
            ("Capabilities.member.1", "CAPABILITY_IAM"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {body}"
    );
    // The resources depend on one another, so the stack owes one clock-driven span per level.
    for _ in 0..4 {
        settle_the_stacks(&router, &engine, case).await;
    }

    let (_, described) =
        cloudformation(&router, region, "DescribeStacks", &[("StackName", stack_name)]).await;
    assert_eq!(
        opt_element(&described, "StackStatus").as_deref(),
        Some("CREATE_COMPLETE"),
        "case {case:?}: expected the whole estate to deploy, got {described}"
    );

    // The optimizer is registered on the table the same stack created.
    let (status, read) =
        read_optimizer(&router, region, ACCOUNT, database, table, "compaction").await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from GetTableOptimizer, got {status} with body {read}"
    );
    let outputs = outputs_of(&described);
    let role_arn = outputs
        .get(ROLE_OUTPUT)
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: no {ROLE_OUTPUT} output in {described}"));
    assert_eq!(
        read.pointer("/TableOptimizer/configuration/roleArn")
            .and_then(Value::as_str),
        Some(role_arn.as_str()),
        "case {case:?}: expected the optimizer to run as the stack's own role, got {read}"
    );
    assert_eq!(
        read.pointer("/TableOptimizer/type").and_then(Value::as_str),
        Some("compaction"),
        "case {case:?}: expected the declared optimizer type, got {read}"
    );
}

/// A declaration the provider or the owning service REFUSES fails that resource and rolls the
/// stack back, with nothing registered in AWS Glue — rather than refusing the submitted
/// template, which is what a type CloudFormation does not recognise earns.
///
/// The rows differ along WHO refuses and WHY: a `Type` outside the set that service enumerates
/// (the provider's own model validation, before anything is asked of AWS Glue), a type stated
/// in the wrong case (the same set, which is case-sensitive), and a table the Data Catalog does
/// not hold (that service's own refusal).
#[tokio::test]
async fn a_table_optimizer_declaration_that_is_refused_fails_the_stack() {
    let (router, engine) = build_router_and_engine();
    let region = "us-west-2";
    let database = "refusals";
    let table = "ledger";
    given_an_iceberg_table(&router, "the refusal rows", region, database, table).await;

    // (case, the table the declaration names, the type it states, the sentence the resource's
    // failure must carry).
    let rows: Vec<(&str, &str, &str, &str)> = vec![
        (
            "a type outside the set AWS Glue enumerates",
            table,
            "vacuum",
            ENUM_VALIDATION_FAILED,
        ),
        (
            "a published type spelled in the wrong case",
            table,
            "COMPACTION",
            ENUM_VALIDATION_FAILED,
        ),
        (
            "a table the Data Catalog does not hold",
            "no-such-table",
            "compaction",
            "EntityNotFoundException",
        ),
    ];

    for (index, (case, declared_table, optimizer_type, expected_reason)) in
        rows.into_iter().enumerate()
    {
        let stack_name = format!("glue-optimizer-refusal-stack-{index}");
        let template = json!({
            "Resources": {
                ROLE_LOGICAL_ID: {
                    "Type": "AWS::IAM::Role",
                    "Properties": { "AssumeRolePolicyDocument": trust_policy() },
                },
                OPTIMIZER_LOGICAL_ID: {
                    "Type": "AWS::Glue::TableOptimizer",
                    "Properties": {
                        "CatalogId": ACCOUNT,
                        "DatabaseName": database,
                        "TableName": declared_table,
                        "Type": optimizer_type,
                        "TableOptimizerConfiguration": {
                            "RoleArn": { "Fn::GetAtt": [ROLE_LOGICAL_ID, "Arn"] },
                            "Enabled": true,
                        },
                    },
                },
            },
        })
        .to_string();

        let (status, body) = cloudformation(
            &router,
            region,
            "CreateStack",
            &[
                ("StackName", &stack_name),
                ("TemplateBody", &template),
                ("Capabilities.member.1", "CAPABILITY_IAM"),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the template to be ADMITTED and the refusal to be the \
             resource's, got {status} with body {body}"
        );
        // The create failed, so the stack is being rolled back. Both transitions it owes are
        // on the stack's own clock, so the case drives that clock rather than waiting for it.
        settle_the_stacks(&router, &engine, case).await;
        settle_the_stacks(&router, &engine, case).await;

        let (_, described) =
            cloudformation(&router, region, "DescribeStacks", &[("StackName", &stack_name)]).await;
        assert_eq!(
            opt_element(&described, "StackStatus").as_deref(),
            Some("ROLLBACK_COMPLETE"),
            "case {case:?}: expected the failed create to be rolled back, got {described}"
        );
        let reason = opt_element(&described, "StackStatusReason").unwrap_or_default();
        assert!(
            reason.contains(OPTIMIZER_LOGICAL_ID),
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

        // Nothing was registered in AWS Glue for the failed resource: the type the row stated
        // is held by no optimizer of the table.
        let (status, read) = read_optimizer(
            &router,
            region,
            ACCOUNT,
            database,
            declared_table,
            optimizer_type,
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected no optimizer to be left behind by the failed stack, got \
             {status} with body {read}"
        );
    }
}
