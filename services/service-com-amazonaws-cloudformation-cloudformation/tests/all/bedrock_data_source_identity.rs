// SPDX-License-Identifier: CC0-1.0

//! `AWS::Bedrock::DataSource` end to end: the data source a stack declares exists in Amazon
//! Bedrock Agent itself, under the identity the stack publishes, and a changed declaration is
//! applied to THAT data source rather than to a record the stack keeps of its own.
//!
//! RULE: for a resource type whose owning service MINTS the resource's identifier, the stack
//! creates the resource through that service and publishes the identifier THAT service answered
//! as the resource's physical id — so `ListDataSources` on the knowledge base the stack
//! published carries one summary per declared data source and `GetDataSource` resolves the id
//! the stack handed out. A data source is such a resource: `CreateDataSource` takes its NAME and
//! answers the `dataSourceId` it minted, modelled `^[0-9a-zA-Z]{10}$`, which no name
//! CloudFormation decides could satisfy; the type's primary identifier is that id TOGETHER with
//! the knowledge base, which is what its `Ref` returns.
//!
//! What this suite covers, beside the create: a CHANGED declaration, which the same rule governs
//! — the change goes to the data source that service holds, through `UpdateDataSource`, and the
//! data source keeps the id it was created under, so every handle the stack published still
//! resolves. The rows differ along WHICH declared property the revision changes: the data
//! source's name, its description, and the connection configuration itself (the one property
//! whose shape differs per connector kind).
//!
//! The create over an Amazon S3 Vectors store, with the whole catalogue of connector kinds, is
//! `core/router/tests/all/cloudformation_provisions_bedrock_data_sources.rs`.
//!
//! Everything goes over the wire each service speaks through the one router `stack_lifecycle.rs`
//! builds — the CloudFormation calls over its awsQuery wire, the data-source reads over Amazon
//! Bedrock Agent's own `restJson1` wire — so what is asserted is what a client that deploys the
//! template and then calls that service observes.

use std::time::Duration;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::bedrock_knowledge_base_identity::{
    described_physical_id, knowledge_base_resource, opensearch_storage, output_value, TITAN_G1,
};
use crate::stack_lifecycle::{
    acknowledging_iam, call, create_stack, describe_stack, member, router, xml_text, REGION,
};

/// How long the suite goes on polling for a stack status that is already due, and how often: a
/// loaded machine can be slower than the emulator's own spans without the rule being any
/// different.
const POLL_INTERVAL: Duration = Duration::from_millis(50);
const POLLS: usize = 600;

/// How many characters a minted `dataSourceId` carries: the modelled `Id` shape every
/// `dataSourceId` member targets (`@pattern ^[0-9a-zA-Z]{10}$`).
const DATA_SOURCE_ID_CHARACTERS: usize = 10;

/// The separator CloudFormation joins the parts of a COMPOSITE physical resource id with.
const COMPOSITE_SEPARATOR: &str = "|";

/// The status a data source ready for ingestion reports (`DataSource$status`: "AVAILABLE – The
/// data source has been created and is ready for ingestion into the knowledge base").
const AVAILABLE: &str = "AVAILABLE";

/// One Amazon Bedrock Agent call over the `restJson1` wire its model declares, on the method and
/// URI that operation's own `@http` binding names, signed for `bedrock` — the signing name every
/// Bedrock-family model declares.
async fn bedrock_agent(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> (u16, Value) {
    let payload = body.map(ToString::to_string).unwrap_or_default();
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://bedrock.{REGION}.amazonaws.com{uri}"))
        .header("host", format!("bedrock.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/bedrock/\
                 aws4_request, SignedHeaders=host, Signature=0123456789abcdef"
            ),
        )
        .body(bytes::Bytes::from(payload))
        .expect("a well-formed Amazon Bedrock Agent request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let raw = response.into_body();
    let parsed = if raw.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&raw).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// `GetDataSource` for one data source of one knowledge base: the HTTP status and the
/// `dataSource` the answer carries (the whole answer where it carries no such member, so a
/// refusal's own document is what an assertion quotes).
async fn get_data_source(
    router: &MultiServiceRouter,
    knowledge_base_id: &str,
    data_source_id: &str,
) -> (u16, Value) {
    let (status, document) = bedrock_agent(
        router,
        "GET",
        &format!("/knowledgebases/{knowledge_base_id}/datasources/{data_source_id}"),
        None,
    )
    .await;
    let data_source = document
        .get("dataSource")
        .cloned()
        .unwrap_or(document.clone());
    (status, data_source)
}

/// Every `dataSourceSummaries` entry `ListDataSources` reports for one knowledge base, over all
/// its pages — the paginated read a client discovers a knowledge base's data sources through.
async fn list_data_sources(router: &MultiServiceRouter, knowledge_base_id: &str) -> Vec<Value> {
    let mut summaries = Vec::new();
    let mut next_token: Option<String> = None;
    loop {
        let mut request = json!({ "maxResults": 1 });
        if let Some(token) = &next_token {
            request["nextToken"] = Value::String(token.clone());
        }
        let (status, answered) = bedrock_agent(
            router,
            "POST",
            &format!("/knowledgebases/{knowledge_base_id}/datasources/"),
            Some(&request),
        )
        .await;
        assert_eq!(
            status, 200,
            "ListDataSources on {knowledge_base_id}: expected HTTP 200, got {status} with \
             {answered}"
        );
        summaries.extend(
            answered
                .get("dataSourceSummaries")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
        );
        next_token = answered
            .get("nextToken")
            .and_then(Value::as_str)
            .map(ToString::to_string);
        if next_token.is_none() {
            return summaries;
        }
    }
}

/// Poll the stack until it reports `wanted`, and answer the last status it reported with the
/// `DescribeStacks` body that reported it.
///
/// The wait is for the CONDITION rather than for a fixed time, so the deadline can be generous
/// without the suite taking that long when the emulator's own spans come due promptly.
async fn wait_for_stack_status(
    router: &MultiServiceRouter,
    stack: &str,
    wanted: &str,
) -> (String, String) {
    let mut last = (String::new(), String::new());
    for _ in 0..POLLS {
        let (status, body) = describe_stack(router, stack).await;
        assert_eq!(
            status, 200,
            "DescribeStacks {stack}: expected HTTP 200, got {status} with {body}"
        );
        let reported = xml_text(&body, "StackStatus").unwrap_or_default();
        last = (reported.clone(), body);
        if reported == wanted {
            return last;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    last
}

/// Whether `value` is an identifier of the form the owning service mints: exactly
/// [`DATA_SOURCE_ID_CHARACTERS`] alphanumeric characters, the whole of the modelled pattern.
fn is_data_source_id(value: &str) -> bool {
    value.len() == DATA_SOURCE_ID_CHARACTERS && value.chars().all(|c| c.is_ascii_alphanumeric())
}

/// The S3 bucket the declared connector reads, as `S3DataSourceConfiguration$bucketArn`'s own
/// pattern spells one (`^arn:aws(|-cn|-us-gov):s3:::[a-z0-9][a-z0-9.-]{1,61}[a-z0-9]$`).
const DOCUMENT_BUCKET_ARN: &str = "arn:aws:s3:::dscfn-crate-documents";

/// One revision of the template: the data source's `Name`, its `Description` and the inclusion
/// prefix inside its connection configuration, which is what each row changes one of.
struct Revision {
    name: &'static str,
    description: &'static str,
    prefix: &'static str,
}

/// The template: a knowledge base in an OpenSearch Serverless store, the data source under it as
/// `revision` states it, and a second data source of another connector kind that no revision
/// touches — so a row also shows the update reaching ONE of the knowledge base's data sources.
fn template(revision: &Revision) -> String {
    json!({
        "Resources": {
            "Corpus": knowledge_base_resource(
                Some("dscfn-crate-corpus"),
                Some("the crate-local corpus"),
                "dscfn-crate",
                TITAN_G1,
                opensearch_storage("dscfn-crate", "corpus-index"),
            ),
            "Manuals": {
                "Type": "AWS::Bedrock::DataSource",
                "Properties": {
                    "KnowledgeBaseId": { "Ref": "Corpus" },
                    "Name": revision.name,
                    "Description": revision.description,
                    "DataSourceConfiguration": {
                        "Type": "S3",
                        "S3Configuration": {
                            "BucketArn": DOCUMENT_BUCKET_ARN,
                            "InclusionPrefixes": [revision.prefix]
                        }
                    }
                }
            },
            "Tickets": {
                "Type": "AWS::Bedrock::DataSource",
                "Properties": {
                    "KnowledgeBaseId": { "Ref": "Corpus" },
                    "Name": "dscfn_crate_tickets",
                    "DataSourceConfiguration": { "Type": "CUSTOM" }
                }
            }
        },
        "Outputs": {
            "CorpusRef": { "Value": { "Ref": "Corpus" } },
            "ManualsRef": { "Value": { "Ref": "Manuals" } },
            "ManualsId": { "Value": { "Fn::GetAtt": ["Manuals", "DataSourceId"] } },
            "TicketsId": { "Value": { "Fn::GetAtt": ["Tickets", "DataSourceId"] } }
        }
    })
    .to_string()
}

/// The revision the stack is created with.
const CREATED: Revision = Revision {
    name: "dscfn-crate-manuals",
    description: "the product manuals",
    prefix: "manuals/",
};

/// The data sources the stack declares really exist in Amazon Bedrock Agent, and a changed
/// declaration is applied to the data source that service holds while the identity it minted
/// stays in force.
#[tokio::test]
async fn a_changed_declaration_updates_the_data_source_bedrock_agent_holds() {
    let router = router();
    let stack = "dscfn-crate";
    let (status, body) = create_stack(
        &router,
        stack,
        &template(&CREATED),
        &acknowledging_iam(&[]),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateStack answered {status} with {body}"
    );
    let (reached, described) = wait_for_stack_status(&router, stack, "CREATE_COMPLETE").await;
    assert_eq!(
        reached, "CREATE_COMPLETE",
        "setup: expected the create to complete, got {described}"
    );

    let knowledge_base_id = output_value(&router, "setup", stack, "CorpusRef")
        .await
        .unwrap_or_else(|| panic!("setup: no CorpusRef output for {stack}"));
    let data_source_id = output_value(&router, "setup", stack, "ManualsId")
        .await
        .unwrap_or_else(|| panic!("setup: no ManualsId output for {stack}"));
    let other_id = output_value(&router, "setup", stack, "TicketsId")
        .await
        .unwrap_or_else(|| panic!("setup: no TicketsId output for {stack}"));
    for (logical_id, reported) in [("Manuals", &data_source_id), ("Tickets", &other_id)] {
        assert!(
            is_data_source_id(reported),
            "setup: expected GetAtt DataSourceId for {logical_id} to be an identifier of the \
             modelled ^[0-9a-zA-Z]{{10}}$ form, got {reported:?}"
        );
    }

    // The resource's physical id — what `Ref` returns — is the PAIR the type is identified by.
    let identity = format!("{knowledge_base_id}{COMPOSITE_SEPARATOR}{data_source_id}");
    let described_id = described_physical_id(&router, "setup", stack, "Manuals").await;
    assert_eq!(
        described_id, identity,
        "setup: expected the physical id to be {identity:?}, got {described_id:?}"
    );
    let referenced = output_value(&router, "setup", stack, "ManualsRef").await;
    assert_eq!(
        referenced.as_deref(),
        Some(identity.as_str()),
        "setup: expected Ref to be {identity:?}, got {referenced:?}"
    );

    // Both data sources are the knowledge base's, and nothing else is.
    let summaries = list_data_sources(&router, &knowledge_base_id).await;
    let listed: Vec<&str> = summaries
        .iter()
        .filter_map(|summary| summary.get("dataSourceId").and_then(Value::as_str))
        .collect();
    assert_eq!(
        listed.len(),
        2,
        "setup: expected ListDataSources to report the stack's two data sources, got \
         {summaries:?}"
    );
    for expected in [&data_source_id, &other_id] {
        assert!(
            listed.contains(&expected.as_str()),
            "setup: expected ListDataSources to report {expected:?}, got {listed:?}"
        );
    }

    // One revision per declared property a change can land on, applied in turn to the stack the
    // create left behind: each asserts the data source that service holds carries the revised
    // value and keeps the identity it was created under.
    let rows: [(&str, Revision); 3] = [
        (
            "a revision that renames the data source",
            Revision {
                name: "dscfn-crate-manuals-v2",
                ..CREATED
            },
        ),
        (
            "a revision that restates the description",
            Revision {
                name: "dscfn-crate-manuals-v2",
                description: "the product manuals, second edition",
                ..CREATED
            },
        ),
        (
            "a revision that restates the connection configuration",
            Revision {
                name: "dscfn-crate-manuals-v2",
                description: "the product manuals, second edition",
                prefix: "manuals/v2/",
            },
        ),
    ];

    for (case, revision) in &rows {
        let (status, body) = call(
            &router,
            "UpdateStack",
            &acknowledging_iam(&[
                member("StackName", stack),
                member("TemplateBody", &template(revision)),
            ]),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: UpdateStack answered {status} with {body}"
        );
        let (reached, described) = wait_for_stack_status(&router, stack, "UPDATE_COMPLETE").await;
        assert_eq!(
            reached, "UPDATE_COMPLETE",
            "case {case:?}: expected the update to complete, got {described}"
        );

        // The identity the owning service minted is still the one in force: an in-place update
        // is not a replacement, so every handle the stack published still resolves.
        let after = output_value(&router, case, stack, "ManualsId").await;
        assert_eq!(
            after.as_deref(),
            Some(data_source_id.as_str()),
            "case {case:?}: expected the data source to keep the id {data_source_id:?}, got \
             {after:?}"
        );
        let described_id = described_physical_id(&router, case, stack, "Manuals").await;
        assert_eq!(
            described_id, identity,
            "case {case:?}: expected the physical id to stay {identity:?}, got {described_id:?}"
        );

        // ...and the data source that service holds carries what the revision declared.
        let (status, data_source) =
            get_data_source(&router, &knowledge_base_id, &data_source_id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from GetDataSource, got {status} with \
             {data_source}"
        );
        for (member_name, expected) in [
            ("name", revision.name),
            ("description", revision.description),
            ("status", AVAILABLE),
        ] {
            let answered = data_source.get(member_name).and_then(Value::as_str);
            assert_eq!(
                answered,
                Some(expected),
                "case {case:?}: expected the data source's {member_name:?} to be {expected:?}, \
                 got {answered:?} in {data_source}"
            );
        }
        let prefix = data_source
            .pointer("/dataSourceConfiguration/s3Configuration/inclusionPrefixes/0")
            .and_then(Value::as_str);
        assert_eq!(
            prefix,
            Some(revision.prefix),
            "case {case:?}: expected the stored inclusion prefix to be {:?}, got {prefix:?} in \
             {data_source}",
            revision.prefix
        );

        // The other data source of the same knowledge base is untouched by the update.
        let (status, untouched) = get_data_source(&router, &knowledge_base_id, &other_id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the other data source to still resolve, got {status} with \
             {untouched}"
        );
        let name = untouched.get("name").and_then(Value::as_str);
        assert_eq!(
            name,
            Some("dscfn_crate_tickets"),
            "case {case:?}: expected the untouched data source to keep its name, got {name:?} \
             in {untouched}"
        );
    }
}
