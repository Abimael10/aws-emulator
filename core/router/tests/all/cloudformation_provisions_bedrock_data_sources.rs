// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation stack that declares an `AWS::Bedrock::DataSource` really creates it in
//! Amazon Bedrock Agent, on the knowledge base the same template declares, so the data source
//! the stack reports created is the one that service's own `ListDataSources` finds and
//! `GetDataSource` resolves.
//!
//! The rule under test: **a resource type this emulator provisions is created through the
//! owning service's own API, so every resource a `CREATE_COMPLETE` stack reports is one that
//! service itself reports back, under the identifier IT assigned** — materialising the resource
//! only inside the stack's own resource table publishes a handle that resolves to nothing and
//! leaves the owning service's listing empty. For a data source that means: `ListDataSources`
//! on the knowledge base the stack published carries exactly one summary per declared data
//! source, each under the `Name` the template stated and the `dataSourceId` that service
//! minted, `GetDataSource` on that id answers `200` with the configuration the template
//! declared, and the resource's physical id — what `Ref` returns — is
//! `<knowledgeBaseId>|<dataSourceId>`. Three crates have to agree for that to hold
//! (CloudFormation makes the call, Bedrock Agent serves it, and the knowledge base resolves its
//! vector store in S3 Vectors), so the suite lives here rather than in any one of them.
//!
//! The rows differ along what the rule generalises over: the KIND of connector the data source
//! declares — the `DataSourceConfiguration.Type` discriminator and the sibling member that
//! carries that kind's connection details — which is what the declared document the create is
//! built from varies in (a flat structure, no connection member at all, and a nested document
//! carrying a list). The catalogue those kinds come from is the modelled `DataSourceType` enum
//! and the `DataSourceConfiguration` structure
//! (`smithy_query(query="shape",shape="DataSourceType,DataSourceConfiguration")`), and the
//! rows cover kinds the issue this suite was written for did not cite.
//!
//! Every expected value is computed here from what the row declares, except the minted
//! identifier itself, which is the owning service's to assign: what is checked about it is its
//! FORM (the modelled `^[0-9a-zA-Z]{10}$`), that every reader of the stack reports the same
//! one, and that the owning service resolves it.

use std::collections::BTreeMap;
use std::time::Duration;
use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The awsQuery form encoder and the XML element readers this crate writes ONCE
// (`tests/all/query_wire.rs`): "Nothing new should write another one — call this module."
use super::query_wire::{form_encode, opt_element, outputs_of, physical_ids_of};

/// How long the suite goes on polling for an outcome that is already due, and how often: a
/// loaded machine can be slower than the emulator's own spans without the rule being any
/// different.
const POLL_INTERVAL: Duration = Duration::from_millis(100);
const POLLS: usize = 400;

/// The account id every ARN this emulator mints carries.
const ACCOUNT: &str = "000000000000";

/// The Region these stacks deploy in.
const REGION: &str = "us-east-1";

/// The `StackStatus` values the rules above are stated in.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const UPDATE_COMPLETE: &str = "UPDATE_COMPLETE";

/// How many characters a minted `dataSourceId` carries: the modelled `Id` shape every
/// `dataSourceId` member targets (`@pattern ^[0-9a-zA-Z]{10}$`).
const DATA_SOURCE_ID_CHARACTERS: usize = 10;

/// The separator CloudFormation joins the parts of a COMPOSITE physical resource id with, which
/// an `AWS::Bedrock::DataSource`'s `Ref` returns the knowledge base and the data source id
/// joined with.
const COMPOSITE_SEPARATOR: &str = "|";

/// The status a data source that is ready for ingestion is reported in: the modelled
/// `DataSource$status` documents `AVAILABLE` as the status of a data source that "has been
/// created and is ready for ingestion into the knowledge base".
const AVAILABLE: &str = "AVAILABLE";

/// The embedding model the knowledge base reads, and how many values its vectors carry: Titan
/// Text Embeddings G1 produces 1536, which is the length the declared index holds.
const TITAN_G1: &str = "amazon.titan-embed-text-v1";
const TITAN_G1_DIMENSION: u32 = 1536;

/// The emulator with the three services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them. Request authentication is off, as in every other suite
/// here; the in-process callback CloudFormation makes signs its request either way (the
/// credential scope is what routes it).
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_s3vectors_s3vectors::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_bedrockagent_amazonbedrockagentbuildtimelambda::EmulatedService::new(
            engine,
        ),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service` in [`REGION`], which is
/// what routes the request and what each handler reads its Region from.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/\
         aws4_request, SignedHeaders=host, Signature=0000000000000000000000000000000000000000\
         000000000000000000000000"
    )
}

/// Call one CloudFormation operation over its awsQuery wire.
async fn cloudformation(
    router: &MultiServiceRouter,
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
        .header("host", format!("cloudformation.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("cloudformation"))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = String::from_utf8_lossy(response.body()).to_string();
    (status, payload)
}

/// Call one Amazon Bedrock Agent operation over the `restJson1` wire its model declares, on the
/// method and URI that operation's own `@http` binding names, signed for `bedrock` — the signing
/// name every Bedrock-family model declares.
async fn bedrock_agent(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let payload = match &body {
        Some(document) => Bytes::from(document.to_string()),
        None => Bytes::new(),
    };
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("bedrock.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("bedrock"))
        .body(payload)
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let answered: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, answered)
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
            Some(request),
        )
        .await;
        assert_eq!(
            status, 200,
            "ListDataSources on {knowledge_base_id}: expected HTTP 200, got {status} with \
             {answered}"
        );
        let page = answered
            .get("dataSourceSummaries")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        summaries.extend(page);
        next_token = answered
            .get("nextToken")
            .and_then(Value::as_str)
            .map(ToString::to_string);
        if next_token.is_none() {
            return summaries;
        }
    }
}

/// `GetDataSource` for one data source of one knowledge base: the HTTP status and the
/// `dataSource` it answers (the whole answer where it carries no such member, so a refusal's
/// own document is what an assertion quotes).
async fn get_data_source(
    router: &MultiServiceRouter,
    knowledge_base_id: &str,
    data_source_id: &str,
) -> (u16, Value) {
    let (status, answered) = bedrock_agent(
        router,
        "GET",
        &format!("/knowledgebases/{knowledge_base_id}/datasources/{data_source_id}"),
        None,
    )
    .await;
    let data_source = answered
        .get("dataSource")
        .cloned()
        .unwrap_or_else(|| answered.clone());
    (status, data_source)
}

/// Poll the stack until it reports `wanted`, and answer the last status it reported with the
/// `DescribeStacks` body that reported it.
///
/// The wait is for the CONDITION rather than for a fixed time: the stack's own clock-owed spans
/// and the owning services' are due within a fraction of this deadline, which is generous so a
/// loaded machine reaches the same verdict.
async fn wait_for_stack_status(
    router: &MultiServiceRouter,
    stack: &str,
    wanted: &str,
) -> (String, String) {
    let mut last = (String::new(), String::new());
    for _ in 0..POLLS {
        let (status, body) =
            cloudformation(router, "DescribeStacks", &[("StackName", stack)]).await;
        assert_eq!(
            status, 200,
            "DescribeStacks {stack}: expected HTTP 200, got {status} with body {body}"
        );
        let reported = opt_element(&body, "StackStatus").unwrap_or_default();
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

/// One declared data source: its logical id in the template, the `Name` it states, and the
/// `DataSourceConfiguration` document its kind takes.
struct Row {
    case: &'static str,
    logical_id: &'static str,
    name: &'static str,
    /// The `DataSourceConfiguration.Type` discriminator, which is also what the owning
    /// service's own read reports the configuration's `type` as.
    kind: &'static str,
    /// The whole declared `DataSourceConfiguration`, built by [`Row::configuration`].
    configuration: fn() -> Value,
    /// The description the declaration states, where it states one.
    description: Option<&'static str>,
}

impl Row {
    fn configuration(&self) -> Value {
        (self.configuration)()
    }
}

/// The S3 bucket the S3-connector row reads, as `S3DataSourceConfiguration$bucketArn`'s own
/// pattern spells one (`^arn:aws(|-cn|-us-gov):s3:::[a-z0-9][a-z0-9.-]{1,61}[a-z0-9]$`).
const DOCUMENT_BUCKET_ARN: &str = "arn:aws:s3:::dscfn-router-documents";

/// The rows: one per KIND of connector, which is what the rule generalises over.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "an S3 connector over a bucket of the account",
            logical_id: "Manuals",
            name: "dscfn-router-manuals",
            kind: "S3",
            configuration: || {
                json!({
                    "Type": "S3",
                    "S3Configuration": {
                        "BucketArn": DOCUMENT_BUCKET_ARN,
                        "InclusionPrefixes": ["manuals/"],
                        "BucketOwnerAccountId": ACCOUNT
                    }
                })
            },
            description: Some("the product manuals"),
        },
        Row {
            // `CUSTOM` names no source at all: its documents are pushed in through the
            // service's own document API, so the configuration carries no sibling member.
            case: "a custom connector the documents are pushed into",
            logical_id: "Tickets",
            name: "dscfn_router_tickets",
            kind: "CUSTOM",
            configuration: || json!({ "Type": "CUSTOM" }),
            description: None,
        },
        Row {
            // A nested configuration carrying a LIST, so the declared document the create is
            // built from is re-cased all the way down rather than only at its top level.
            case: "a web crawler over seed URLs",
            logical_id: "Handbook",
            name: "dscfn-router-handbook",
            kind: "WEB",
            configuration: || {
                json!({
                    "Type": "WEB",
                    "WebConfiguration": {
                        "SourceConfiguration": {
                            "UrlConfiguration": {
                                "SeedUrls": [{ "Url": "https://docs.example.com/handbook" }]
                            }
                        }
                    }
                })
            },
            description: Some("the public handbook"),
        },
    ]
}

/// The template: the vector store, the knowledge base over it, and one data source per declared
/// row, with each data source's identity published both ways.
fn template(declared: &[&Row]) -> String {
    let mut resources = serde_json::Map::new();
    resources.insert(
        "Vectors".to_string(),
        json!({
            "Type": "AWS::S3Vectors::VectorBucket",
            "Properties": { "VectorBucketName": "dscfn-router-vectors" }
        }),
    );
    resources.insert(
        "Embeddings".to_string(),
        json!({
            "Type": "AWS::S3Vectors::Index",
            "Properties": {
                "VectorBucketArn": { "Ref": "Vectors" },
                "IndexName": "dscfn-router-embeddings",
                "DataType": "float32",
                "Dimension": TITAN_G1_DIMENSION,
                "DistanceMetric": "cosine"
            }
        }),
    );
    resources.insert(
        "Corpus".to_string(),
        json!({
            "Type": "AWS::Bedrock::KnowledgeBase",
            "Properties": {
                "Name": "dscfn-router-corpus",
                "RoleArn": format!("arn:aws:iam::{ACCOUNT}:role/dscfn-router"),
                "KnowledgeBaseConfiguration": {
                    "Type": "VECTOR",
                    "VectorKnowledgeBaseConfiguration": {
                        "EmbeddingModelArn":
                            format!("arn:aws:bedrock:{REGION}::foundation-model/{TITAN_G1}")
                    }
                },
                "StorageConfiguration": {
                    "Type": "S3_VECTORS",
                    "S3VectorsConfiguration": { "IndexArn": { "Ref": "Embeddings" } }
                }
            }
        }),
    );
    let mut outputs = serde_json::Map::new();
    outputs.insert(
        "CorpusRef".to_string(),
        json!({ "Value": { "Ref": "Corpus" } }),
    );
    for row in declared {
        let mut properties = serde_json::Map::new();
        properties.insert(
            "KnowledgeBaseId".to_string(),
            json!({ "Ref": "Corpus" }),
        );
        properties.insert("Name".to_string(), json!(row.name));
        properties.insert("DataSourceConfiguration".to_string(), row.configuration());
        if let Some(description) = row.description {
            properties.insert("Description".to_string(), json!(description));
        }
        resources.insert(
            row.logical_id.to_string(),
            json!({ "Type": "AWS::Bedrock::DataSource", "Properties": properties }),
        );
        outputs.insert(
            format!("{}Ref", row.logical_id),
            json!({ "Value": { "Ref": row.logical_id } }),
        );
        outputs.insert(
            format!("{}Id", row.logical_id),
            json!({ "Value": { "Fn::GetAtt": [row.logical_id, "DataSourceId"] } }),
        );
    }
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": resources,
        "Outputs": outputs
    })
    .to_string()
}

/// Every data source the stack declares is created in Amazon Bedrock Agent, so that service's
/// own listing of the knowledge base's data sources carries exactly one summary per declared
/// row, and `GetDataSource` on the id the stack published resolves it.
#[tokio::test]
async fn a_stack_publishes_the_data_sources_bedrock_agent_created() {
    let router = build_router();
    let stack = "dscfn-router";
    let declared = rows();
    let all: Vec<&Row> = declared.iter().collect();
    let (status, body) = cloudformation(
        &router,
        "CreateStack",
        &[("StackName", stack), ("TemplateBody", &template(&all))],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: expected HTTP 200 from CreateStack, got {status} with body {body}"
    );

    let (reached, described) = wait_for_stack_status(&router, stack, CREATE_COMPLETE).await;
    assert_eq!(
        reached, CREATE_COMPLETE,
        "setup: expected the stack to reach {CREATE_COMPLETE}, got {described}"
    );
    let outputs = outputs_of(&described);
    let knowledge_base_id = outputs
        .get("CorpusRef")
        .cloned()
        .unwrap_or_else(|| panic!("setup: no CorpusRef output in {described}"));

    let (status, resources) =
        cloudformation(&router, "DescribeStackResources", &[("StackName", stack)]).await;
    assert_eq!(
        status, 200,
        "setup: expected HTTP 200 from DescribeStackResources, got {status} with {resources}"
    );
    let physical_ids = physical_ids_of(&resources);
    let summaries = list_data_sources(&router, &knowledge_base_id).await;

    // One summary per declared data source and not one more: the listing reports the knowledge
    // base's data sources, which are exactly the ones the stack created.
    assert_eq!(
        summaries.len(),
        all.len(),
        "case \"the whole declared set\": expected {} summaries from ListDataSources, got {:?}",
        all.len(),
        summaries
    );

    for row in &all {
        let case = row.case;
        let reported_id = outputs
            .get(&format!("{}Id", row.logical_id))
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: no DataSourceId output in {described}"));
        assert!(
            is_data_source_id(&reported_id),
            "case {case:?}: expected GetAtt DataSourceId to be an identifier of the modelled \
             ^[0-9a-zA-Z]{{10}}$ form, got {reported_id:?}"
        );

        // `Ref` and the resource's physical id are the PAIR the type is identified by.
        let identity = format!("{knowledge_base_id}{COMPOSITE_SEPARATOR}{reported_id}");
        for (reader, answered) in [
            ("Ref", outputs.get(&format!("{}Ref", row.logical_id))),
            ("PhysicalResourceId", physical_ids.get(row.logical_id)),
        ] {
            assert_eq!(
                answered,
                Some(&identity),
                "case {case:?}: expected {reader} to be {identity:?}, got {answered:?}"
            );
        }

        // ...and the owning service's own listing carries exactly one summary under the Name
        // the template stated, whose id is the one the stack published.
        let listed: Vec<&Value> = summaries
            .iter()
            .filter(|summary| summary.get("name").and_then(Value::as_str) == Some(row.name))
            .collect();
        assert_eq!(
            listed.len(),
            1,
            "case {case:?}: expected exactly one summary named {:?} in ListDataSources, got \
             {listed:?}",
            row.name
        );
        let summary = listed[0];
        for (member, expected) in [
            ("knowledgeBaseId", knowledge_base_id.as_str()),
            ("dataSourceId", reported_id.as_str()),
            ("status", AVAILABLE),
        ] {
            let answered = summary.get(member).and_then(Value::as_str);
            assert_eq!(
                answered,
                Some(expected),
                "case {case:?}: expected the summary's {member:?} to be {expected:?}, got \
                 {answered:?} in {summary}"
            );
        }

        // ...and `GetDataSource` on that id answers the data source the template declared.
        let (status, data_source) =
            get_data_source(&router, &knowledge_base_id, &reported_id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from GetDataSource on the id the stack \
             published, got {status} with {data_source}"
        );
        for (member, expected) in [
            ("knowledgeBaseId", Some(knowledge_base_id.clone())),
            ("dataSourceId", Some(reported_id.clone())),
            ("name", Some(row.name.to_string())),
            ("status", Some(AVAILABLE.to_string())),
            ("description", row.description.map(ToString::to_string)),
        ] {
            let answered = data_source
                .get(member)
                .and_then(Value::as_str)
                .map(ToString::to_string);
            assert_eq!(
                answered, expected,
                "case {case:?}: expected the data source's {member:?} to be {expected:?}, got \
                 {answered:?} in {data_source}"
            );
        }
        let kind = data_source
            .pointer("/dataSourceConfiguration/type")
            .and_then(Value::as_str);
        assert_eq!(
            kind,
            Some(row.kind),
            "case {case:?}: expected the stored configuration's type to be {:?}, got {kind:?} \
             in {data_source}",
            row.kind
        );
    }

    // The nested document the web row declared reached the owning service re-cased all the way
    // down, list included, rather than only at its top level.
    let crawled = summaries
        .iter()
        .find(|summary| summary.get("name").and_then(Value::as_str) == Some("dscfn-router-handbook"))
        .and_then(|summary| summary.get("dataSourceId"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let (_, handbook) = get_data_source(&router, &knowledge_base_id, &crawled).await;
    let seed = handbook
        .pointer("/dataSourceConfiguration/webConfiguration/sourceConfiguration/urlConfiguration/seedUrls/0/url")
        .and_then(Value::as_str);
    assert_eq!(
        seed,
        Some("https://docs.example.com/handbook"),
        "case \"a web crawler over seed URLs\": expected the stored seed URL to be the one the \
         template declared, got {seed:?} in {handbook}"
    );

    // ...and the S3 row's bucket likewise.
    let s3_id = outputs.get("ManualsId").cloned().unwrap_or_default();
    let (_, manuals) = get_data_source(&router, &knowledge_base_id, &s3_id).await;
    let bucket = manuals
        .pointer("/dataSourceConfiguration/s3Configuration/bucketArn")
        .and_then(Value::as_str);
    assert_eq!(
        bucket,
        Some(DOCUMENT_BUCKET_ARN),
        "case \"an S3 connector over a bucket of the account\": expected the stored bucket to \
         be the one the template declared, got {bucket:?} in {manuals}"
    );
}

/// The other half of the same rule: an update that STOPS DECLARING one data source removes it
/// from the owning service too, while the ones the template still declares keep the identities
/// that service minted for them.
#[tokio::test]
async fn an_update_that_drops_a_data_source_removes_it_from_the_knowledge_base() {
    let case = "a template that no longer declares one of its data sources";
    let router = build_router();
    let stack = "dscfn-router-update";
    let declared = rows();
    let all: Vec<&Row> = declared.iter().collect();
    let (status, body) = cloudformation(
        &router,
        "CreateStack",
        &[("StackName", stack), ("TemplateBody", &template(&all))],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup {case:?}: expected HTTP 200 from CreateStack, got {status} with body {body}"
    );
    let (reached, described) = wait_for_stack_status(&router, stack, CREATE_COMPLETE).await;
    assert_eq!(
        reached, CREATE_COMPLETE,
        "setup {case:?}: expected the stack to reach {CREATE_COMPLETE}, got {described}"
    );
    let outputs = outputs_of(&described);
    let knowledge_base_id = outputs
        .get("CorpusRef")
        .cloned()
        .unwrap_or_else(|| panic!("setup {case:?}: no CorpusRef output in {described}"));
    let dropped_id = outputs
        .get("ManualsId")
        .cloned()
        .unwrap_or_else(|| panic!("setup {case:?}: no ManualsId output in {described}"));
    let kept: Vec<&Row> = all
        .iter()
        .copied()
        .filter(|row| row.logical_id != "Manuals")
        .collect();
    let kept_ids: BTreeMap<&str, String> = kept
        .iter()
        .map(|row| {
            (
                row.name,
                outputs
                    .get(&format!("{}Id", row.logical_id))
                    .cloned()
                    .unwrap_or_default(),
            )
        })
        .collect();

    let (status, body) = cloudformation(
        &router,
        "UpdateStack",
        &[("StackName", stack), ("TemplateBody", &template(&kept))],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from UpdateStack, got {status} with body {body}"
    );
    let (reached, described) = wait_for_stack_status(&router, stack, UPDATE_COMPLETE).await;
    assert_eq!(
        reached, UPDATE_COMPLETE,
        "case {case:?}: expected the update to reach {UPDATE_COMPLETE}, got {described}"
    );

    // The data source the template no longer declares is gone from the owning service: it is
    // in no listing, and its id resolves to nothing.
    let summaries = list_data_sources(&router, &knowledge_base_id).await;
    let still_listed: Vec<&Value> = summaries
        .iter()
        .filter(|summary| {
            summary.get("dataSourceId").and_then(Value::as_str) == Some(dropped_id.as_str())
        })
        .collect();
    assert!(
        still_listed.is_empty(),
        "case {case:?}: expected ListDataSources to report no summary for the dropped data \
         source {dropped_id:?}, got {still_listed:?}"
    );
    let (status, answered) = get_data_source(&router, &knowledge_base_id, &dropped_id).await;
    assert_eq!(
        status, 404,
        "case {case:?}: expected HTTP 404 from GetDataSource on the dropped data source, got \
         {status} with {answered}"
    );

    // ...and the ones the template still declares keep the identities that service minted.
    assert_eq!(
        summaries.len(),
        kept.len(),
        "case {case:?}: expected {} summaries from ListDataSources after the update, got \
         {summaries:?}",
        kept.len()
    );
    for (name, expected) in &kept_ids {
        let listed = summaries
            .iter()
            .find(|summary| summary.get("name").and_then(Value::as_str) == Some(name))
            .and_then(|summary| summary.get("dataSourceId"))
            .and_then(Value::as_str);
        assert_eq!(
            listed,
            Some(expected.as_str()),
            "case {case:?}: expected the kept data source {name:?} to keep the id \
             {expected:?}, got {listed:?} in {summaries:?}"
        );
    }
}
