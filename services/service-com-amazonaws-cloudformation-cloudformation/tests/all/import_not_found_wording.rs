// SPDX-License-Identifier: CC0-1.0

//! What a change set reports for an IMPORT whose resource is not there, end to end over the
//! awsQuery wire.
//!
//! RULE: CloudFormation plans an import by calling the resource provider that owns the type
//! for the identifier the mapping names, so a change set that cannot resolve one fails with
//! THAT provider's own not-found diagnostic: the sentence a type whose handler words the
//! absence itself states (`AWS::DynamoDB::Table` says a table does not exist), and the
//! uniform "Resource of type '<type>' with identifier '<id>' was not found." for a type whose
//! handler reports through the generic resource-provider base class.
//!
//! The rows differ along what the rule generalises over: which type's provider is called,
//! whether that provider words the absence itself or reports through the base class, whether
//! the sentence names the resource at all, and whether the identifier comes from a name the
//! TEMPLATE states or only from the import mapping's own `ResourceIdentifier`.

use crate::change_set_lifecycle::{create_change_set, describe_change_set, settle_the_store};
use crate::harness::{member, router_with_engine, xml_text};
use service_com_amazonaws_cloudformation_cloudformation::state::{
    change_set_status, execution_status,
};

/// One row: an IMPORT of a resource nothing holds, and the reason the change set must fail
/// with.
struct Row {
    case: &'static str,
    /// The stack the change set is created for (one per row, since each one fails its own
    /// plan).
    stack: &'static str,
    /// The resource type the mapping names, which decides whose diagnostic is reported.
    resource_type: &'static str,
    /// The declaration's own properties, as the template spells them — carrying the resource's
    /// NAME where the row states it in the template.
    properties: &'static str,
    /// The `ResourceIdentifier` key the mapping names the resource by: the type's primary
    /// identifier property.
    identifier_key: &'static str,
    /// The physical identifier the mapping gives, which nothing holds.
    identifier: &'static str,
    /// The `StatusReason` the failed change set must carry.
    reason: &'static str,
}

/// RULE: the reason a change set fails an unresolvable IMPORT with is the owning resource
/// provider's own not-found diagnostic, and the uniform sentence for a type whose provider
/// states none.
#[tokio::test]
async fn an_unresolvable_import_fails_with_the_owning_providers_diagnostic() {
    // The sentences are the ones each type's own handler states for a resource it cannot
    // read; a type absent from that set is reported in the uniform sentence, which names the
    // type and the identifier and nothing else.
    let rows: &[Row] = &[
        Row {
            case: "a DynamoDB table the mapping file names",
            stack: "import-absent-table",
            resource_type: "AWS::DynamoDB::Table",
            // No `TableName`: the mapping file is what names the table, exactly as a CDK
            // template leaves it to.
            properties: r#""KeySchema":[{"AttributeName":"pk","KeyType":"HASH"}],
                "AttributeDefinitions":[{"AttributeName":"pk","AttributeType":"S"}],
                "BillingMode":"PAY_PER_REQUEST""#,
            identifier_key: "TableName",
            identifier: "absent-orders",
            reason: "Table: absent-orders does not exist.",
        },
        Row {
            case: "a DynamoDB table the TEMPLATE names",
            stack: "import-absent-named-table",
            resource_type: "AWS::DynamoDB::Table",
            properties: r#""TableName":"absent-ledger",
                "KeySchema":[{"AttributeName":"pk","KeyType":"HASH"}],
                "AttributeDefinitions":[{"AttributeName":"pk","AttributeType":"S"}],
                "BillingMode":"PAY_PER_REQUEST""#,
            identifier_key: "TableName",
            identifier: "absent-ledger",
            reason: "Table: absent-ledger does not exist.",
        },
        Row {
            case: "an S3 bucket, whose provider names the resource nowhere",
            stack: "import-absent-bucket",
            resource_type: "AWS::S3::Bucket",
            properties: r#""BucketName":"absent-assets-bucket""#,
            identifier_key: "BucketName",
            identifier: "absent-assets-bucket",
            reason: "Bucket not found",
        },
        Row {
            case: "a CloudWatch alarm, whose provider names the resource nowhere",
            stack: "import-absent-alarm",
            resource_type: "AWS::CloudWatch::Alarm",
            properties: r#""AlarmName":"absent-cpu-high",
                "ComparisonOperator":"GreaterThanThreshold","EvaluationPeriods":1,
                "MetricName":"CPUUtilization","Namespace":"AWS/EC2","Period":60,
                "Statistic":"Average","Threshold":80"#,
            identifier_key: "AlarmName",
            identifier: "absent-cpu-high",
            reason: "Alarm does not exist",
        },
        Row {
            case: "a Systems Manager parameter, reported through the base class",
            stack: "import-absent-parameter",
            resource_type: "AWS::SSM::Parameter",
            properties: r#""Name":"/import/absent-wording","Type":"String","Value":"held""#,
            identifier_key: "Name",
            identifier: "/import/absent-wording",
            reason: "Resource of type 'AWS::SSM::Parameter' with identifier \
                     '/import/absent-wording' was not found.",
        },
        Row {
            case: "a CloudWatch Logs group, reported through the base class",
            stack: "import-absent-log-group",
            resource_type: "AWS::Logs::LogGroup",
            properties: r#""LogGroupName":"/import/absent-group""#,
            identifier_key: "LogGroupName",
            identifier: "/import/absent-group",
            reason: "Resource of type 'AWS::Logs::LogGroup' with identifier \
                     '/import/absent-group' was not found.",
        },
    ];

    for row in rows {
        let (router, engine) = router_with_engine();
        let case = row.case;
        // Every imported resource must carry a `DeletionPolicy`, which CloudFormation
        // requires of an IMPORT's template.
        let template = format!(
            r#"{{"Resources":{{"Imported":{{"Type":"{}","DeletionPolicy":"Retain",
                "Properties":{{{}}}}}}}}}"#,
            row.resource_type, row.properties
        );
        let (code, body) = create_change_set(
            &router,
            row.stack,
            "cs",
            &[
                member("ChangeSetType", "IMPORT"),
                member("TemplateBody", &template),
                member("ResourcesToImport.member.1.ResourceType", row.resource_type),
                member("ResourcesToImport.member.1.LogicalResourceId", "Imported"),
                member(
                    "ResourcesToImport.member.1.ResourceIdentifier.entry.1.key",
                    row.identifier_key,
                ),
                member(
                    "ResourcesToImport.member.1.ResourceIdentifier.entry.1.value",
                    row.identifier,
                ),
            ],
        )
        .await;
        assert_eq!(
            code, 200,
            "case {case:?}: the change set is created, got {code} {body}"
        );
        settle_the_store(&router, &engine).await;
        let (_, body) = describe_change_set(&router, "cs", Some(row.stack)).await;
        let reported = (
            xml_text(&body, "Status").unwrap_or_default(),
            xml_text(&body, "ExecutionStatus").unwrap_or_default(),
            xml_text(&body, "StatusReason").unwrap_or_default(),
        );
        let expected = (
            change_set_status::FAILED.to_string(),
            execution_status::UNAVAILABLE.to_string(),
            row.reason.to_string(),
        );
        assert_eq!(
            reported, expected,
            "case {case:?}: expected {expected:?}, got {reported:?} in {body}"
        );
    }
}
