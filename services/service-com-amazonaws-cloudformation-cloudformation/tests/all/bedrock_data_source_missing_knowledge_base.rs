// SPDX-License-Identifier: CC0-1.0

//! What a stack reports for an `AWS::Bedrock::DataSource` whose declared `KnowledgeBaseId`
//! names a knowledge base that is not there, end to end over the awsQuery wire.
//!
//! RULE under test: **where a resource provider's call is refused because a resource the
//! declaration merely REFERENCES is not there, the failed resource's `ResourceStatusReason`
//! quotes the resource-provider plugin's own not-found sentence — `Resource of type '<the
//! CloudFormation type of the MISSING resource>' with identifier '<the identifier the
//! declaration stated for it>' was not found.` — rather than the owning service's own message,
//! closed by the `RequestToken` of the resource request and the `NotFound` class the refusal
//! falls in; nothing is created for the resource and the stack rolls back.**
//!
//! Why it matters: the reason is the only place a client learns WHICH resource a failed
//! provisioning could not find. The plugin's sentence is uniform across every resource type and
//! names a CloudFormation type, so deployment tooling, CDK/SAM error surfaces and CI log
//! matchers tell "a referenced resource is missing" from "a property is invalid" generically. A
//! service-native sentence names no CloudFormation type at all, and an operator reading the
//! event cannot tell that the missing thing was the knowledge base rather than the data source
//! itself.
//!
//! The rows differ along what the rule generalises over: the FORM of the stated identifier
//! (digits, lower case, upper case and mixed — the whole of the modelled `^[0-9a-zA-Z]{10}$`
//! character set, `smithy_query(query="operation",operation="CreateDataSource")` on Amazon
//! Bedrock Agent's model), and the kind of connector the declaration configures (an Amazon S3
//! source, one with inclusion prefixes and a description, and a `CUSTOM` one) — none of which
//! the sentence varies with. The control row is the positive path the same provider takes: a
//! data source declared against a knowledge base the same stack really creates, which is
//! created, read back through `GetDataSource` and reports no failure at all.

use std::time::Duration;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::bedrock_knowledge_base_identity::{
    bedrock_agent, knowledge_base_resource, opensearch_storage, output_value, TITAN_G1,
};
use crate::harness::{envelope_parts, failed_resource_reason, is_a_request_id};
use crate::stack_lifecycle::{
    acknowledging_iam, create_stack, describe_stack, router, xml_text, REGION,
};
use service_com_amazonaws_cloudformation_cloudformation::state::{resource_status, status};

/// How long the suite goes on reading a stack for a status that is no longer in progress, and
/// how often: the wait is for the CONDITION, so the deadline is generous while a stack whose
/// transitions are already due is seen the moment they are.
const POLL_INTERVAL: Duration = Duration::from_millis(50);
const POLLS: usize = 1_200;

/// The logical id of the data source every row declares.
const DATA_SOURCE: &str = "Ds";

/// The `HandlerErrorCode` class a refusal that a resource was NOT FOUND falls in, a value of
/// the model's own `HandlerErrorCode` enum (`smithy_query(query="shape",
/// shape="HandlerErrorCode")`).
const NOT_FOUND: &str = "NotFound";

/// The CloudFormation type of the resource a data source's declaration REFERENCES, which is the
/// type the plugin's sentence names when that resource is the missing one.
const REFERENCED_TYPE: &str = "AWS::Bedrock::KnowledgeBase";

/// The S3 bucket a declared connector reads, as `S3DataSourceConfiguration$bucketArn`'s own
/// pattern spells one (`^arn:aws(|-cn|-us-gov):s3:::[a-z0-9][a-z0-9.-]{1,61}[a-z0-9]$`).
const DOCUMENT_BUCKET_ARN: &str = "arn:aws:s3:::dsmissing-kb-documents";

/// The plugin's own not-found sentence, whose fixed text is `Resource of type '`, `' with
/// identifier '` and `' was not found.` and whose two variable parts are the CloudFormation
/// type of the missing resource and the identifier the declaration stated for it.
fn plugin_not_found(resource_type: &str, identifier: &str) -> String {
    format!("Resource of type '{resource_type}' with identifier '{identifier}' was not found.")
}

/// One row: the stack it deploys, the identifier it states for the knowledge base, and the
/// `DataSourceConfiguration` it declares.
struct Row {
    case: &'static str,
    stack: &'static str,
    /// The `KnowledgeBaseId` the declaration states — an identifier of the modelled form that
    /// no knowledge base of this account uses.
    knowledge_base_id: &'static str,
    properties: Value,
}

/// The rows the rule is asserted over: four identifiers of the modelled form, each with a
/// connector of its own.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "an identifier of digits, over an Amazon S3 source",
            stack: "dsmissing-kb-digits",
            knowledge_base_id: "0123456789",
            properties: json!({
                "Name": "dsmissing-kb-digits-source",
                "DataSourceConfiguration": {
                    "Type": "S3",
                    "S3Configuration": { "BucketArn": DOCUMENT_BUCKET_ARN }
                }
            }),
        },
        Row {
            case: "an identifier of lower-case letters, over a CUSTOM source",
            stack: "dsmissing-kb-lower",
            knowledge_base_id: "abcdefghij",
            properties: json!({
                "Name": "dsmissing_kb_lower_source",
                "DataSourceConfiguration": { "Type": "CUSTOM" }
            }),
        },
        Row {
            case: "an identifier of upper-case letters, over an Amazon S3 source with prefixes",
            stack: "dsmissing-kb-upper",
            knowledge_base_id: "ABCDEFGHIJ",
            properties: json!({
                "Name": "dsmissing-kb-upper-source",
                "Description": "the source of a knowledge base that is not there",
                "DataSourceConfiguration": {
                    "Type": "S3",
                    "S3Configuration": {
                        "BucketArn": DOCUMENT_BUCKET_ARN,
                        "InclusionPrefixes": ["manuals/"]
                    }
                }
            }),
        },
        Row {
            case: "an identifier of mixed case and digits",
            stack: "dsmissing-kb-mixed",
            knowledge_base_id: "aB3dE6gH9j",
            properties: json!({
                "Name": "dsmissing-kb-mixed-source",
                "DataSourceConfiguration": {
                    "Type": "S3",
                    "S3Configuration": { "BucketArn": DOCUMENT_BUCKET_ARN }
                }
            }),
        },
    ]
}

/// The template of one row: the data source alone, declared against the identifier the row
/// states — so nothing of the stack creates the knowledge base it names.
fn template_of(row: &Row) -> String {
    let mut properties = row.properties.clone();
    properties["KnowledgeBaseId"] = json!(row.knowledge_base_id);
    json!({
        "Resources": {
            DATA_SOURCE: { "Type": "AWS::Bedrock::DataSource", "Properties": properties }
        }
    })
    .to_string()
}

/// Read a stack until it reports a status that is no longer in progress, and answer that read's
/// body together with the status it reported.
async fn settled(router: &MultiServiceRouter, case: &str, stack: &str) -> (String, String) {
    let mut last = (String::new(), String::new());
    for _ in 0..POLLS {
        let (code, body) = describe_stack(router, stack).await;
        assert_eq!(
            code, 200,
            "case {case:?}: DescribeStacks({stack}) must answer 200, got {code} {body}"
        );
        let reported = xml_text(&body, "StackStatus").unwrap_or_default();
        last = (reported.clone(), body);
        if !reported.is_empty() && !reported.ends_with("_IN_PROGRESS") {
            return last;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    last
}

/// `ListDataSources` for one knowledge base: the HTTP status and the answer, which is how a
/// client asks that service whether anything was created under the identifier the row stated.
async fn list_data_sources(router: &MultiServiceRouter, knowledge_base_id: &str) -> (u16, Value) {
    bedrock_agent(
        router,
        "POST",
        &format!("/knowledgebases/{knowledge_base_id}/datasources/"),
        Some(&json!({})),
    )
    .await
}

/// A data source declared against a knowledge base nothing issued fails with the plugin's own
/// not-found sentence, naming the REFERENCED type and the stated identifier, and nothing is
/// created for it.
#[tokio::test]
async fn a_referenced_knowledge_base_that_is_not_there_is_reported_as_the_plugins_not_found() {
    for row in rows() {
        let Row {
            case,
            stack,
            knowledge_base_id,
            ..
        } = &row;
        let router = router();
        let (code, body) =
            create_stack(&router, stack, &template_of(&row), &acknowledging_iam(&[])).await;
        assert_eq!(
            code, 200,
            "case {case:?}: CreateStack answered {code} with {body}"
        );

        let (reported, described) = settled(&router, case, stack).await;
        assert_eq!(
            reported,
            status::ROLLBACK_COMPLETE,
            "case {case:?}: expected the stack to roll back, got {described}"
        );

        let reason = failed_resource_reason(
            case,
            &router,
            REGION,
            stack,
            DATA_SOURCE,
            resource_status::CREATE_FAILED,
        )
        .await;
        let (quoted, token, class) = envelope_parts(case, &reason);
        let expected = plugin_not_found(REFERENCED_TYPE, knowledge_base_id);
        assert_eq!(
            quoted, expected,
            "case {case:?}: expected the quoted message to be {expected:?}, got {quoted:?} in \
             {reason:?}"
        );
        assert_eq!(
            class, NOT_FOUND,
            "case {case:?}: expected the {NOT_FOUND} class, got {class:?} in {reason:?}"
        );
        assert!(
            is_a_request_id(&token),
            "case {case:?}: expected the resource request's own token, got {token:?} in \
             {reason:?}"
        );

        // Nothing was created for the resource: that service holds no knowledge base under the
        // stated identifier, so it has no data sources to list either.
        let (code, listed) = list_data_sources(&router, knowledge_base_id).await;
        assert_eq!(
            code, 404,
            "case {case:?}: expected ListDataSources on {knowledge_base_id:?} to be refused as \
             not found, got {code} with {listed}"
        );
    }
}

/// The control: the same provider's positive path. A data source declared against a knowledge
/// base the stack really creates is created, resolves through `GetDataSource` under the identity
/// the stack published, and reports no failure.
#[tokio::test]
async fn a_data_source_of_a_knowledge_base_the_stack_creates_is_created() {
    let router = router();
    let stack = "dsmissing-kb-control";
    let template = json!({
        "Resources": {
            "Corpus": knowledge_base_resource(
                Some("dsmissing-kb-corpus"),
                None,
                "dsmissing-kb",
                TITAN_G1,
                opensearch_storage("dsmissing-kb", "corpus-index"),
            ),
            DATA_SOURCE: {
                "Type": "AWS::Bedrock::DataSource",
                "Properties": {
                    "KnowledgeBaseId": { "Ref": "Corpus" },
                    "Name": "dsmissing-kb-control-source",
                    "DataSourceConfiguration": {
                        "Type": "S3",
                        "S3Configuration": { "BucketArn": DOCUMENT_BUCKET_ARN }
                    }
                }
            }
        },
        "Outputs": {
            "CorpusRef": { "Value": { "Ref": "Corpus" } },
            "DsId": { "Value": { "Fn::GetAtt": [DATA_SOURCE, "DataSourceId"] } }
        }
    })
    .to_string();
    let case = "a data source of a knowledge base the stack creates";
    let (code, body) = create_stack(&router, stack, &template, &acknowledging_iam(&[])).await;
    assert_eq!(
        code, 200,
        "case {case:?}: CreateStack answered {code} with {body}"
    );
    let (reported, described) = settled(&router, case, stack).await;
    assert_eq!(
        reported,
        status::CREATE_COMPLETE,
        "case {case:?}: expected the create to complete, got {described}"
    );

    let knowledge_base_id = output_value(&router, case, stack, "CorpusRef")
        .await
        .unwrap_or_else(|| panic!("case {case:?}: no CorpusRef output for {stack}"));
    let data_source_id = output_value(&router, case, stack, "DsId")
        .await
        .unwrap_or_else(|| panic!("case {case:?}: no DsId output for {stack}"));

    // The round trip: the data source the stack reported created is the one that service holds.
    let (code, answered) = bedrock_agent(
        &router,
        "GET",
        &format!("/knowledgebases/{knowledge_base_id}/datasources/{data_source_id}"),
        None,
    )
    .await;
    assert_eq!(
        code, 200,
        "case {case:?}: expected GetDataSource to resolve {data_source_id:?}, got {code} with \
         {answered}"
    );
    let name = answered.pointer("/dataSource/name").and_then(Value::as_str);
    assert_eq!(
        name,
        Some("dsmissing-kb-control-source"),
        "case {case:?}: expected the declared name, got {name:?} in {answered}"
    );
    let bucket = answered
        .pointer("/dataSource/dataSourceConfiguration/s3Configuration/bucketArn")
        .and_then(Value::as_str);
    assert_eq!(
        bucket,
        Some(DOCUMENT_BUCKET_ARN),
        "case {case:?}: expected the declared bucket, got {bucket:?} in {answered}"
    );

    // ...and nothing failed: no resource of this stack recorded a create failure.
    let reason = crate::harness::resource_event_reason_in(
        &router,
        REGION,
        stack,
        DATA_SOURCE,
        resource_status::CREATE_FAILED,
    )
    .await;
    assert!(
        reason.is_empty(),
        "case {case:?}: expected no failed create, got {reason:?}"
    );
}
