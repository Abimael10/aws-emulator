// SPDX-License-Identifier: CC0-1.0

//! Two things the plugin's not-found report of a REFERENCED resource is held to beside its
//! wording, which `bedrock_data_source_missing_knowledge_base.rs` pins for an
//! `AWS::Bedrock::DataSource`: WHICH identifier the sentence names when the template does not
//! state it literally, and what becomes of the resources the failed stack HAD created.
//!
//! RULE under test: **the identifier the plugin's not-found sentence names is the one the
//! declaration RESOLVED to — the declaration is the resolved request, so a template that
//! computes the reference is reported with the computed value rather than with the expression it
//! wrote — and the create that refusal failed rolls the stack back, so every resource the
//! deployment had already created is removed from the service that owns it.**
//!
//! Why it matters: a client reading the failed deployment matches the reported identifier
//! against the value its own template resolved (a stack parameter, an id joined from parts, one
//! carried in from elsewhere), and tooling that gates on the stack status has to be able to
//! conclude that the rolled-back stack left nothing behind. A reason naming an expression, or a
//! bucket that outlives the rollback, breaks exactly those two readings.
//!
//! The rows differ along HOW the template states the reference — joined out of two literals, and
//! taken from a stack PARAMETER the deployment supplies — neither of which the sentence varies
//! with, and both resolving to an identifier of the modelled `^[0-9a-zA-Z]{10}$` form that no
//! knowledge base of this account uses. Each row's stack also declares the `AWS::S3::Bucket` the
//! connector reads, which is the resource the rollback has to remove.
//!
//! The POSITIVE CONTROL deploys the same two resources with the reference RESOLVING, so the
//! removal above is what a ROLLBACK does rather than what every deployment of these two does.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::bedrock_knowledge_base_identity::{
    knowledge_base_resource, opensearch_storage, TITAN_G1,
};
use crate::harness::{failed_resource_reason, quoted_and_class, resource_event_reason_in};
use crate::stack_lifecycle::{
    acknowledging_iam, create_stack, member, resource_event_statuses, router, settled_stack,
    xml_text, REGION,
};
use crate::stack_resource_provisioning::s3;
use service_com_amazonaws_cloudformation_cloudformation::state::{resource_status, status};

/// The logical ids every template here declares: the data source whose reference is under test,
/// and the bucket its connector reads — the resource the rollback has to remove.
const DATA_SOURCE: &str = "Ds";
const BUCKET: &str = "DsBucket";

/// The parameter a row's template takes the reference from.
const CORPUS_PARAMETER: &str = "Corpus";

/// The `HandlerErrorCode` class a refusal that a resource was NOT FOUND falls in, a value of the
/// model's own `HandlerErrorCode` enum (`smithy_query(query="shape",shape="HandlerErrorCode")`).
const NOT_FOUND: &str = "NotFound";

/// The CloudFormation type of the resource a data source's declaration REFERENCES, which is the
/// type the plugin's sentence names when that resource is the missing one.
const REFERENCED_TYPE: &str = "AWS::Bedrock::KnowledgeBase";

/// The statuses Amazon S3 answers a `HEAD` of a bucket it holds and of one it does not with.
const BUCKET_HELD: u16 = 200;
const NO_SUCH_BUCKET: u16 = 404;

/// The plugin's own not-found sentence, whose fixed text is `Resource of type '`, `' with
/// identifier '` and `' was not found.` and whose two variable parts are the CloudFormation type
/// of the missing resource and the identifier the declaration stated for it.
fn plugin_not_found(resource_type: &str, identifier: &str) -> String {
    format!("Resource of type '{resource_type}' with identifier '{identifier}' was not found.")
}

/// One row: the stack it deploys, the bucket it declares, how its template states the knowledge
/// base reference, the parameter value (if any) the deployment supplies, and the identifier that
/// declaration resolves to.
struct Row {
    case: &'static str,
    stack: &'static str,
    bucket: &'static str,
    /// The `KnowledgeBaseId` property as the template writes it — an expression rather than a
    /// literal, so the value the create carries is one CloudFormation computed.
    declared: Value,
    /// The value supplied for [`CORPUS_PARAMETER`], for the row whose expression reads it.
    parameter: Option<&'static str>,
    /// What the expression resolves to, which is the identifier the sentence must name.
    identifier: &'static str,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a reference joined out of two literals",
            stack: "dscomputed-kb-join",
            bucket: "dscomputed-kb-join-documents",
            declared: json!({ "Fn::Join": ["", ["aB3dE6", "gH9k"]] }),
            parameter: None,
            identifier: "aB3dE6gH9k",
        },
        Row {
            case: "a reference taken from a stack parameter",
            stack: "dscomputed-kb-parameter",
            bucket: "dscomputed-kb-parameter-documents",
            declared: json!({ "Ref": CORPUS_PARAMETER }),
            parameter: Some("0a1B2c3D4e"),
            identifier: "0a1B2c3D4e",
        },
    ]
}

/// The template of one row: the bucket the connector reads, and the data source declared against
/// the reference the row states — so nothing of the stack creates the knowledge base it names.
fn template_of(row: &Row) -> String {
    let mut template = json!({
        "Resources": {
            BUCKET: {
                "Type": "AWS::S3::Bucket",
                "Properties": { "BucketName": row.bucket }
            },
            DATA_SOURCE: {
                "Type": "AWS::Bedrock::DataSource",
                "Properties": {
                    "KnowledgeBaseId": row.declared,
                    "Name": "dscomputed-kb-source",
                    "DataSourceConfiguration": {
                        "Type": "S3",
                        "S3Configuration": { "BucketArn": { "Fn::GetAtt": [BUCKET, "Arn"] } }
                    }
                }
            }
        }
    });
    // A template whose expression reads a parameter declares it, as every template taking one
    // does.
    if row.parameter.is_some() {
        template["Parameters"] = json!({ CORPUS_PARAMETER: { "Type": "String" } });
    }
    template.to_string()
}

/// The `CreateStack` members a row deploys with: the capabilities every suite acknowledges, plus
/// the parameter its template takes.
fn request_of(row: &Row) -> Vec<(String, String)> {
    let mut request = acknowledging_iam(&[]);
    if let Some(value) = row.parameter {
        request.push(member("Parameters.member.1.ParameterKey", CORPUS_PARAMETER));
        request.push(member("Parameters.member.1.ParameterValue", value));
    }
    request
}

/// Whether Amazon S3 still holds `bucket`, read as a client reads it (`HEAD /<bucket>`).
async fn bucket_status(router: &MultiServiceRouter, bucket: &str) -> u16 {
    s3(router, "HEAD", &format!("/{bucket}"), b"").await.status
}

#[tokio::test]
async fn a_computed_reference_is_reported_as_resolved_and_its_rollback_removes_the_bucket() {
    for row in rows() {
        let router = router();
        let (code, created) =
            create_stack(&router, row.stack, &template_of(&row), &request_of(&row)).await;
        assert_eq!(
            code, 200,
            "case {:?}: CreateStack answered {code} with {created}",
            row.case
        );

        let described = settled_stack(&router, row.case, row.stack).await;
        assert_eq!(
            xml_text(&described, "StackStatus").as_deref(),
            Some(status::ROLLBACK_COMPLETE),
            "case {:?}: expected the refused create to roll back, got {described}",
            row.case
        );

        // The sentence names the identifier the declaration RESOLVED to, not the expression.
        let reason = failed_resource_reason(
            row.case,
            &router,
            REGION,
            row.stack,
            DATA_SOURCE,
            resource_status::CREATE_FAILED,
        )
        .await;
        let (quoted, class) = quoted_and_class(row.case, &reason);
        let expected = plugin_not_found(REFERENCED_TYPE, row.identifier);
        assert_eq!(
            quoted, expected,
            "case {:?}: expected the quoted message to be {expected:?}, got {quoted:?} in \
             {reason:?}",
            row.case
        );
        assert_eq!(
            class, NOT_FOUND,
            "case {:?}: expected the {NOT_FOUND} class, got {class:?} in {reason:?}",
            row.case
        );

        // The bucket the stack HAD created was created and then removed again by the rollback...
        let events = resource_event_statuses(&router, row.case, row.stack, BUCKET).await;
        for wanted in [
            resource_status::CREATE_COMPLETE,
            resource_status::DELETE_COMPLETE,
        ] {
            assert!(
                events.iter().any(|recorded| recorded == wanted),
                "case {:?}: expected the bucket's events to carry {wanted}, got {events:?}",
                row.case
            );
        }
        // ...so the service that owns it no longer holds it.
        let held = bucket_status(&router, row.bucket).await;
        assert_eq!(
            held, NO_SUCH_BUCKET,
            "case {:?}: expected Amazon S3 to hold no {} after the rollback, got {held}",
            row.case, row.bucket
        );
    }
}

/// The control: the same two resources with the reference RESOLVING — a data source of a
/// knowledge base the stack creates itself — complete, and the bucket is still there afterwards.
#[tokio::test]
async fn a_resolving_reference_leaves_the_stacks_bucket_in_place() {
    let case = "a data source of a knowledge base the stack creates, beside its bucket";
    let stack = "dscomputed-kb-control";
    let bucket = "dscomputed-kb-control-documents";
    let router = router();
    let template = json!({
        "Resources": {
            BUCKET: {
                "Type": "AWS::S3::Bucket",
                "Properties": { "BucketName": bucket }
            },
            CORPUS_PARAMETER: knowledge_base_resource(
                Some("dscomputed-kb-corpus"),
                None,
                "dscomputed-kb",
                TITAN_G1,
                opensearch_storage("dscomputed-kb", "corpus-index"),
            ),
            DATA_SOURCE: {
                "Type": "AWS::Bedrock::DataSource",
                "Properties": {
                    "KnowledgeBaseId": { "Ref": CORPUS_PARAMETER },
                    "Name": "dscomputed-kb-control-source",
                    "DataSourceConfiguration": {
                        "Type": "S3",
                        "S3Configuration": { "BucketArn": { "Fn::GetAtt": [BUCKET, "Arn"] } }
                    }
                }
            }
        }
    })
    .to_string();

    let (code, created) = create_stack(&router, stack, &template, &acknowledging_iam(&[])).await;
    assert_eq!(
        code, 200,
        "case {case:?}: CreateStack answered {code} with {created}"
    );
    let described = settled_stack(&router, case, stack).await;
    assert_eq!(
        xml_text(&described, "StackStatus").as_deref(),
        Some(status::CREATE_COMPLETE),
        "case {case:?}: expected the create to complete, got {described}"
    );
    let held = bucket_status(&router, bucket).await;
    assert_eq!(
        held, BUCKET_HELD,
        "case {case:?}: expected Amazon S3 to still hold {bucket}, got {held}"
    );
    let failed = resource_event_reason_in(
        &router,
        REGION,
        stack,
        DATA_SOURCE,
        resource_status::CREATE_FAILED,
    )
    .await;
    assert!(
        failed.is_empty(),
        "case {case:?}: expected no failed create, got {failed:?}"
    );
}
