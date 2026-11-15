// SPDX-License-Identifier: CC0-1.0

//! `AWS::Glue::TableOptimizer` is a resource type a template may DECLARE: every operation that
//! submits a template admits a body declaring one, and the catalogue entries behind that say
//! how a resource of the type is identified.
//!
//! The rule under test: **a resource type Amazon Web Services publishes belongs to the
//! catalogue a submitted template is held to, so a body declaring it is admitted, and a type
//! whose owning API addresses one resource by SEVERAL values is identified by those values
//! together** — the half of the rule this crate owns on its own. It is the mirror of
//! `unrecognized_resource_types.rs`, which pins the other direction: a type CloudFormation does
//! NOT know is refused `Template format error: Unrecognized resource types: [<types>]`, and
//! that refusal must never be the answer for a type AWS publishes.
//!
//! What the stack then DOES with the declaration — the call to AWS Glue's own
//! `CreateTableOptimizer`, the optimizer that service holds afterwards, its removal with the
//! stack, and the resource-level failure an optimizer type outside the published set earns —
//! needs that service mounted beside this one, so it lives in the router's own cross-service
//! suite (`core/router/tests/all/cloudformation_provisions_glue_table_optimizers.rs`). This
//! suite asserts admission alone, which is what a client sees before any resource is
//! attempted.
//!
//! The rows differ along what the rule generalises over: which of the three optimizer types
//! the declaration states, what its configuration carries, how the catalog is named (a literal
//! account id or the account pseudo-parameter), the template spelling (JSON and YAML), how many
//! optimizers one body declares, and whether the declaration stands beside a resource of
//! another type.

use service_com_amazonaws_cloudformation_cloudformation::state::primary_identifier;
use service_com_amazonaws_cloudformation_cloudformation::state::resource_types::{
    is_published, is_recognized, AWS_RESOURCE_TYPES,
};
use service_com_amazonaws_cloudformation_cloudformation::state::template::{
    composite_identity, composite_identity_properties, published_attribute_names,
};

use crate::stack_lifecycle::{call, create_stack, member, router, xml_text, ACCOUNT_ID};

/// The fixed form of the refusal a type CloudFormation does not know is answered with, which
/// no row here may ever see.
const UNRECOGNIZED_PREFIX: &str = "Template format error: Unrecognized resource types: [";

/// The type under test, spelled as the "AWS resource and property types reference" spells it.
const OPTIMIZER: &str = "AWS::Glue::TableOptimizer";

/// The properties that identify ONE optimizer, which are the members every AWS Glue call on
/// one takes (`smithy_query(query="operation", operation="GetTableOptimizer")`).
const IDENTITY_PROPERTIES: &[&str] = &["CatalogId", "DatabaseName", "TableName", "Type"];

/// The three optimizer types AWS Glue enumerates (`smithy_query(query="shape",
/// shape="TableOptimizerType")`), which is the set the type's `Type` property publishes.
const OPTIMIZER_TYPES: &[&str] = &["compaction", "retention", "orphan_file_deletion"];

/// The role an optimizer runs as, which its configuration always carries.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/glue-table-optimizer";

/// One row: a template body declaring the type, and the name of the stack it is submitted
/// under.
struct Row {
    case: &'static str,
    stack_name: &'static str,
    template: String,
}

fn rows() -> Vec<Row> {
    vec![
        // The issue's own body: one compaction optimizer naming the catalog, the database and
        // the table it is on, with its identity read back through `Ref`.
        Row {
            case: "a compaction optimizer over a named table, with Ref read back",
            stack_name: "glue-optimizer-admitted",
            template: format!(
                r#"{{"AWSTemplateFormatVersion": "2010-09-09",
                     "Resources": {{"Optimizer": {{"Type": "{OPTIMIZER}",
                       "Properties": {{"CatalogId": "{ACCOUNT_ID}",
                         "DatabaseName": "lakehouse", "TableName": "orders",
                         "Type": "compaction",
                         "TableOptimizerConfiguration": {{"RoleArn": "{ROLE_ARN}",
                           "Enabled": true}}}}}}}},
                     "Outputs": {{"Identity": {{"Value": {{"Ref": "Optimizer"}}}}}}}}"#
            ),
        },
        // The catalog named through the account pseudo-parameter, and an optimizer declared
        // DISABLED with the configuration its own type takes.
        Row {
            case: "an orphan-file-deletion optimizer under the account pseudo-parameter",
            stack_name: "glue-optimizer-pseudo-catalog",
            template: format!(
                r#"{{"Resources": {{"Optimizer": {{"Type": "{OPTIMIZER}",
                     "Properties": {{"CatalogId": {{"Ref": "AWS::AccountId"}},
                       "DatabaseName": "analytics", "TableName": "events",
                       "Type": "orphan_file_deletion",
                       "TableOptimizerConfiguration": {{"RoleArn": "{ROLE_ARN}",
                         "Enabled": false,
                         "OrphanFileDeletionConfiguration": {{"IcebergConfiguration":
                           {{"OrphanFileRetentionPeriodInDays": 5}}}}}}}}}}}}}}"#
            ),
        },
        // The YAML spelling, with the retention optimizer's own nested configuration.
        Row {
            case: "the YAML spelling of a retention optimizer",
            stack_name: "glue-optimizer-yaml",
            template: format!(
                "\
Resources:
  Optimizer:
    Type: {OPTIMIZER}
    Properties:
      CatalogId: \"{ACCOUNT_ID}\"
      DatabaseName: warehouse
      TableName: shipments
      Type: retention
      TableOptimizerConfiguration:
        RoleArn: {ROLE_ARN}
        Enabled: true
        RetentionConfiguration:
          IcebergConfiguration:
            SnapshotRetentionPeriodInDays: 7
            NumberOfSnapshotsToRetain: 3
            CleanExpiredFiles: true
Outputs:
  Identity:
    Value: !Ref Optimizer
"
            ),
        },
        // Two optimizers of DIFFERENT types on the same table, beside a resource of another
        // type: the catalogue admits every type a body declares or none of them.
        Row {
            case: "two optimizers of one table beside a resource of another type",
            stack_name: "glue-optimizer-mixed",
            template: format!(
                r#"{{"Resources": {{
                     "Compaction": {{"Type": "{OPTIMIZER}",
                       "Properties": {{"CatalogId": "{ACCOUNT_ID}",
                         "DatabaseName": "lakehouse", "TableName": "orders",
                         "Type": "compaction",
                         "TableOptimizerConfiguration": {{"RoleArn": "{ROLE_ARN}"}}}}}},
                     "Retention": {{"Type": "{OPTIMIZER}",
                       "Properties": {{"CatalogId": "{ACCOUNT_ID}",
                         "DatabaseName": "lakehouse", "TableName": "orders",
                         "Type": "retention",
                         "TableOptimizerConfiguration": {{"RoleArn": "{ROLE_ARN}"}}}}}},
                     "Topic": {{"Type": "AWS::SNS::Topic",
                       "Properties": {{"TopicName": "glue-optimizer-topic"}}}}}}}}"#
            ),
        },
    ]
}

/// Assert one answer admitted the body rather than refusing its resource types.
fn assert_admitted(case: &str, status: u16, body: &str) {
    let message = xml_text(body, "Message").unwrap_or_default();
    assert!(
        !message.starts_with(UNRECOGNIZED_PREFIX),
        "case {case:?}: expected a type Amazon Web Services publishes to be recognised, got \
         {message:?}"
    );
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 for a body declaring a published type, got {status} \
         with body {body}"
    );
}

/// Every operation that submits a template for deployment admits a body declaring the type,
/// and so does the operation whose whole answer is a verdict on the body.
#[tokio::test]
async fn every_operation_that_takes_a_template_admits_a_table_optimizer() {
    for row in rows() {
        let router = router();
        let (status, body) = create_stack(&router, row.stack_name, &row.template, &[]).await;
        assert_admitted(row.case, status, &body);
        let stack_id = xml_text(&body, "StackId").unwrap_or_default();
        assert!(
            stack_id.contains(":stack/"),
            "case {:?}: expected CreateStack to answer a stack id, got {body}",
            row.case
        );

        let (status, body) = call(
            &router,
            "ValidateTemplate",
            &[member("TemplateBody", &row.template)],
        )
        .await;
        assert_admitted(row.case, status, &body);

        let (status, body) = call(
            &router,
            "CreateChangeSet",
            &[
                member("StackName", &format!("{}-change-set", row.stack_name)),
                member("ChangeSetName", "a-change-set"),
                member("ChangeSetType", "CREATE"),
                member("TemplateBody", &row.template),
            ],
        )
        .await;
        assert_admitted(row.case, status, &body);
    }
}

/// The catalogue entries the rule rests on: the type is published and recognised, a near-miss
/// spelling of it still is not, and the type is identified by the four values AWS Glue
/// addresses one optimizer by.
#[test]
fn the_catalogue_publishes_the_table_optimizer_type_and_how_it_is_identified() {
    assert!(
        AWS_RESOURCE_TYPES.contains(&OPTIMIZER),
        "expected {OPTIMIZER:?} in the published catalogue, it is absent"
    );
    assert!(
        is_published(OPTIMIZER) && is_recognized(OPTIMIZER),
        "expected {OPTIMIZER:?} to be a type a template may declare, it is not"
    );
    // The catalogue is a list of names, not a shape test: a name it does not carry stays
    // unrecognised however close it is to one it does. Each is a spelling the resource
    // reference does NOT publish: a plural of this type, the optimizer's own run entity, and
    // a type under the namespace of a service that publishes no such resource type.
    for misspelled in [
        "AWS::Glue::TableOptimizers",
        "AWS::Glue::TableOptimizerRun",
        "AWS::LakeFormation::TableOptimizer",
    ] {
        assert!(
            !is_recognized(misspelled),
            "expected {misspelled:?} to stay unrecognised, it is recognised"
        );
    }
    // Every call of the owning API on one optimizer takes those four values, so they are what
    // a client names an existing optimizer by.
    assert_eq!(
        primary_identifier::properties(OPTIMIZER),
        IDENTITY_PROPERTIES
            .iter()
            .map(|property| (*property).to_string())
            .collect::<Vec<String>>(),
        "expected the optimizer to be identified by the values AWS Glue keys one by, got {:?}",
        primary_identifier::properties(OPTIMIZER)
    );
    assert_eq!(
        composite_identity_properties(OPTIMIZER),
        IDENTITY_PROPERTIES,
        "expected the type's composite identifier to be {IDENTITY_PROPERTIES:?}, got {:?}",
        composite_identity_properties(OPTIMIZER)
    );
    // The type publishes no `Fn::GetAtt` attribute at all — its `Ref` is the whole of what a
    // stack reports for one.
    assert!(
        published_attribute_names(OPTIMIZER).is_empty(),
        "expected the type to publish no Fn::GetAtt attribute, got {:?}",
        published_attribute_names(OPTIMIZER)
    );
}

/// RULE: the physical id of a resource whose identifier is composite is its declared parts
/// joined, for every published optimizer type and whatever the parts are — and a declaration
/// that leaves one of them out names no optimizer at all, so it keeps the identity the naming
/// rules computed.
#[test]
fn the_physical_id_is_the_declared_parts_joined() {
    // (case, the declared properties, the identity they make).
    let mut cases: Vec<(String, serde_json::Value, Option<String>)> = Vec::new();
    for optimizer_type in OPTIMIZER_TYPES {
        cases.push((
            format!("the published type {optimizer_type:?}"),
            serde_json::json!({
                "CatalogId": ACCOUNT_ID,
                "DatabaseName": "lakehouse",
                "TableName": "orders",
                "Type": optimizer_type,
                "TableOptimizerConfiguration": {"RoleArn": ROLE_ARN},
            }),
            Some(format!("{ACCOUNT_ID}|lakehouse|orders|{optimizer_type}")),
        ));
    }
    cases.push((
        "another catalog, database and table".to_string(),
        serde_json::json!({
            "CatalogId": "210987654321",
            "DatabaseName": "warehouse",
            "TableName": "shipments",
            "Type": "retention",
        }),
        Some("210987654321|warehouse|shipments|retention".to_string()),
    ));
    cases.push((
        "a declaration naming no table".to_string(),
        serde_json::json!({
            "CatalogId": ACCOUNT_ID,
            "DatabaseName": "lakehouse",
            "Type": "compaction",
        }),
        None,
    ));
    cases.push((
        "a declaration with no properties at all".to_string(),
        serde_json::Value::Null,
        None,
    ));
    for (case, properties, expected) in cases {
        let identity = composite_identity(OPTIMIZER, &properties);
        assert_eq!(
            identity, expected,
            "case {case:?}: expected the identity {expected:?}, got {identity:?}"
        );
    }
}
