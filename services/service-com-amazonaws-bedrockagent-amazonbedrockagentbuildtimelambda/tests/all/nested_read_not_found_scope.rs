// SPDX-License-Identifier: CC0-1.0

//! Which resource a nested READ names in its `ResourceNotFoundException`, end to
//! end over HTTP.
//!
//! RULE: a read whose request path nests the addressed resource under a data
//! source resolves that data source by the (`knowledgeBaseId`, `dataSourceId`)
//! pair BEFORE the resource the path addresses, and the refusal names the
//! resource whose own lookup failed — the data source when the pair resolves to
//! none, and the addressed resource only once the data source resolved. The
//! model nests the reads of this plane that way
//! (`smithy_query(query="operation",operation="GetIngestionJob,GetDataSource")`:
//! `/knowledgebases/{knowledgeBaseId}/datasources/{dataSourceId}/ingestionjobs/{ingestionJobId}`
//! and `/knowledgebases/{knowledgeBaseId}/datasources/{dataSourceId}`), and the
//! sentence itself is the one the sibling wording suite pins: "<ResourceType>
//! with id <id> is not found."
//!
//! Every assertion here goes through the router, so it exercises the real wire
//! path (route matching, model constraint validation, serialization) rather than
//! calling a handler directly.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_bedrockagent_amazonbedrockagentbuildtimelambda::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// A SigV4 `Authorization` header scoped to the `bedrock` signing name: the
/// routing key this service claims on.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260101/eu-west-1/bedrock/aws4_request, \
     SignedHeaders=host;x-amz-date, Signature=0123456789abcdef";

/// A role ARN matching the modelled `KnowledgeBaseRoleArn` pattern.
const KB_ROLE: &str =
    "arn:aws:iam::000000000000:role/service-role/AmazonBedrockExecutionRoleForKnowledgeBase";

/// An embedding model ARN matching the modelled `BedrockEmbeddingModelArn`.
const EMBEDDING_MODEL: &str =
    "arn:aws:bedrock:eu-west-1::foundation-model/amazon.titan-embed-text-v2:0";

/// The vector store the knowledge bases of this suite bind to.
const VECTOR_BUCKET: &str = "arn:aws:s3vectors:::bucket/nested-scope-vectors";
const INDEX: &str = "nested-scope-index";

/// The missing-resource error of this service and the status its model binds to
/// it (`smithy_query(query="operation",operation="GetIngestionJob")`).
const NOT_FOUND: (&str, u16) = ("ResourceNotFoundException", 404);

/// Identifiers of the modelled `Id` shape (`^[0-9a-zA-Z]{10}$`, as
/// `GetIngestionJob`'s three path members all target) that no record of this
/// suite carries: digits only, the mixed form, and a word-shaped one.
const ABSENT_IDS: [&str; 3] = ["0123456789", "aZ09aZ09aZ", "NoSuchIdAA"];

/// The sentence this service words for a resource it cannot resolve.
fn not_found_sentence(resource_type: &str, resource_id: &str) -> String {
    format!("{resource_type} with id {resource_id} is not found.")
}

/// One HTTP answer from the emulator, parsed.
#[derive(Debug)]
struct Answer {
    status: u16,
    /// The restJson1 error shape name, from `x-amzn-errortype` or the body's
    /// `__type`; `None` on a success.
    error_type: Option<String>,
    body: Value,
}

impl Answer {
    fn string(&self, path: &[&str]) -> String {
        let mut at = &self.body;
        for key in path {
            at = &at[*key];
        }
        at.as_str()
            .unwrap_or_else(|| panic!("{path:?} is not a string in {:?}", self.body))
            .to_string()
    }

    /// The refusal's `message` member, or the empty string on an answer carrying
    /// none.
    fn message(&self) -> String {
        self.body["message"].as_str().unwrap_or_default().to_string()
    }
}

/// The emulator under test: one router over one shared engine, exactly as the
/// production router mounts this service.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        // The knowledge bases here bind to an S3 Vectors index, which
        // `CreateKnowledgeBase` resolves in the service that owns it — so the
        // index is created first, as a caller creates it first against AWS.
        crate::vector_store_fixture::seed_index(
            &engine,
            VECTOR_BUCKET,
            INDEX,
            crate::vector_store_fixture::EMBEDDING_DIMENSION,
        );
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(engine));
        Self { router }
    }

    async fn call(&self, method: &str, uri: &str, body: Option<Value>) -> Answer {
        let bytes = body
            .map(|value| Bytes::from(serde_json::to_vec(&value).expect("serialize request body")))
            .unwrap_or_default();
        let request = http::Request::builder()
            .method(method)
            .uri(uri)
            .header("host", "bedrock-agent.eu-west-1.amazonaws.com")
            .header("authorization", AUTHORIZATION)
            .header("content-type", "application/json")
            .body(bytes)
            .expect("build the request");

        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let header_error = response
            .headers()
            .get("x-amzn-errortype")
            .and_then(|value| value.to_str().ok())
            .map(ToString::to_string);
        let raw = response.into_body();
        let body: Value = if raw.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&raw).unwrap_or_else(|error| {
                panic!(
                    "{method} {uri} -> {status} body is not JSON ({error}): {}",
                    String::from_utf8_lossy(&raw)
                )
            })
        };
        let error_type = header_error.or_else(|| {
            body.get("__type")
                .and_then(Value::as_str)
                .map(ToString::to_string)
        });
        Answer {
            status,
            error_type,
            body,
        }
    }

    /// Create a knowledge base bound to [`INDEX`] and answer its minted id.
    async fn knowledge_base(&self, name: &str) -> String {
        let created = self
            .call(
                "PUT",
                "/knowledgebases/",
                Some(json!({
                    "name": name,
                    "roleArn": KB_ROLE,
                    "knowledgeBaseConfiguration": {
                        "type": "VECTOR",
                        "vectorKnowledgeBaseConfiguration": {
                            "embeddingModelArn": EMBEDDING_MODEL,
                        },
                    },
                    "storageConfiguration": {
                        "type": "S3_VECTORS",
                        "s3VectorsConfiguration": {
                            "vectorBucketArn": VECTOR_BUCKET,
                            "indexName": INDEX,
                        },
                    },
                })),
            )
            .await;
        assert_eq!(
            created.status, 202,
            "case setup: expected CreateKnowledgeBase -> 202, got {created:?}"
        );
        created.string(&["knowledgeBase", "knowledgeBaseId"])
    }

    /// Attach a CUSTOM data source to `knowledge_base_id` and answer its minted
    /// id: the kind whose documents are pushed in, so nothing else has to exist
    /// for a job over it.
    async fn data_source(&self, knowledge_base_id: &str, name: &str) -> String {
        let created = self
            .call(
                "PUT",
                &format!("/knowledgebases/{knowledge_base_id}/datasources/"),
                Some(json!({
                    "name": name,
                    "dataSourceConfiguration": { "type": "CUSTOM" },
                })),
            )
            .await;
        assert_eq!(
            created.status, 200,
            "case setup: expected CreateDataSource -> 200, got {created:?}"
        );
        created.string(&["dataSource", "dataSourceId"])
    }

    /// Start an ingestion job over the pair and answer its minted id.
    async fn start_ingestion_job(&self, knowledge_base_id: &str, data_source_id: &str) -> String {
        let started = self
            .call(
                "PUT",
                &format!(
                    "/knowledgebases/{knowledge_base_id}/datasources/{data_source_id}\
                     /ingestionjobs/"
                ),
                Some(json!({})),
            )
            .await;
        assert_eq!(
            started.status, 202,
            "case setup: expected StartIngestionJob -> 202, got {started:?}"
        );
        started.string(&["ingestionJob", "ingestionJobId"])
    }

    async fn get_ingestion_job(
        &self,
        knowledge_base_id: &str,
        data_source_id: &str,
        ingestion_job_id: &str,
    ) -> Answer {
        self.call(
            "GET",
            &format!(
                "/knowledgebases/{knowledge_base_id}/datasources/{data_source_id}\
                 /ingestionjobs/{ingestion_job_id}"
            ),
            None,
        )
        .await
    }

    async fn get_data_source(&self, knowledge_base_id: &str, data_source_id: &str) -> Answer {
        self.call(
            "GET",
            &format!("/knowledgebases/{knowledge_base_id}/datasources/{data_source_id}"),
            None,
        )
        .await
    }
}

/// Assert `answer` is this service's missing-resource refusal worded for
/// `resource_type` and `resource_id`.
fn assert_not_found(case: &str, resource_type: &str, resource_id: &str, answer: &Answer) {
    let (code, status) = NOT_FOUND;
    assert_eq!(
        answer.status, status,
        "case {case:?}: expected HTTP {status}, got {answer:?}"
    );
    assert_eq!(
        answer.error_type.as_deref(),
        Some(code),
        "case {case:?}: expected {code}, got {answer:?}"
    );
    let expected = not_found_sentence(resource_type, resource_id);
    assert_eq!(
        answer.message(),
        expected,
        "case {case:?}: expected the refusal worded {expected:?}, got {:?}",
        answer.message()
    );
}

/// One row of the table: the triple a `GetIngestionJob` request carries, and the
/// resource its refusal has to name.
struct Row {
    case: String,
    knowledge_base_id: String,
    data_source_id: String,
    ingestion_job_id: String,
    resource_type: &'static str,
    /// Which member of the request the named id is taken from.
    named: NamedId,
}

/// Which of the request's ids the refusal names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NamedId {
    DataSource,
    IngestionJob,
}

/// RULE: `GetIngestionJob` resolves the data source the job is nested under
/// before the job, so the refusal names the DATA SOURCE whenever the
/// (`knowledgeBaseId`, `dataSourceId`) pair resolves to none, and the ingestion
/// job only once that pair resolved.
#[tokio::test]
async fn a_nested_job_read_names_the_data_source_whose_pair_resolves_to_none() {
    let emulator = Emulator::new();
    let knowledge_base_id = emulator.knowledge_base("nested-scope-kb").await;
    let other_knowledge_base_id = emulator.knowledge_base("nested-scope-other-kb").await;
    let data_source_id = emulator
        .data_source(&knowledge_base_id, "nested-scope-source")
        .await;
    let other_data_source_id = emulator
        .data_source(&other_knowledge_base_id, "nested-scope-other-source")
        .await;
    let stored_job = emulator
        .start_ingestion_job(&knowledge_base_id, &data_source_id)
        .await;
    let other_job = emulator
        .start_ingestion_job(&other_knowledge_base_id, &other_data_source_id)
        .await;

    // The rows differ along WHICH part of the nested address resolves to
    // nothing: the job alone, the data source (in each of the ways a pair fails
    // to resolve one — an id nothing holds, a knowledge base nothing holds, and
    // a data source of another knowledge base), and both at once; and along the
    // form of the absent id, which is the sentence's one variable part.
    let mut rows: Vec<Row> = Vec::new();
    for absent in ABSENT_IDS {
        // Only the job is unresolvable: its scope resolves, so the job is named.
        rows.push(Row {
            case: format!("a job id nothing holds ({absent}) under a stored data source"),
            knowledge_base_id: knowledge_base_id.clone(),
            data_source_id: data_source_id.clone(),
            ingestion_job_id: absent.to_string(),
            resource_type: "IngestionJob",
            named: NamedId::IngestionJob,
        });
        // The data source is unresolvable, so it is named however the job id
        // would have resolved on its own.
        rows.push(Row {
            case: format!("a data source id nothing holds ({absent}) with a job id nothing holds"),
            knowledge_base_id: knowledge_base_id.clone(),
            data_source_id: absent.to_string(),
            ingestion_job_id: ABSENT_IDS[0].to_string(),
            resource_type: "DataSource",
            named: NamedId::DataSource,
        });
        rows.push(Row {
            case: format!("a data source id nothing holds ({absent}) with a STORED job id"),
            knowledge_base_id: knowledge_base_id.clone(),
            data_source_id: absent.to_string(),
            ingestion_job_id: stored_job.clone(),
            resource_type: "DataSource",
            named: NamedId::DataSource,
        });
        // Neither the knowledge base nor the data source resolves: the pair is
        // what selects the data source, so the data source is still what the
        // refusal names.
        rows.push(Row {
            case: format!("a knowledge base and a data source nothing holds ({absent})"),
            knowledge_base_id: absent.to_string(),
            data_source_id: absent.to_string(),
            ingestion_job_id: ABSENT_IDS[1].to_string(),
            resource_type: "DataSource",
            named: NamedId::DataSource,
        });
        // The stored triple read under a knowledge base nothing holds: the data
        // source is stored, but not under THAT knowledge base.
        rows.push(Row {
            case: format!("a knowledge base nothing holds ({absent}) over the stored triple"),
            knowledge_base_id: absent.to_string(),
            data_source_id: data_source_id.clone(),
            ingestion_job_id: stored_job.clone(),
            resource_type: "DataSource",
            named: NamedId::DataSource,
        });
    }
    // A job stored under another knowledge base's data source, addressed through
    // this knowledge base: the pair resolves no data source, so the data source
    // is named rather than the job that does exist elsewhere.
    rows.push(Row {
        case: "another knowledge base's data source and job".to_string(),
        knowledge_base_id: knowledge_base_id.clone(),
        data_source_id: other_data_source_id.clone(),
        ingestion_job_id: other_job.clone(),
        resource_type: "DataSource",
        named: NamedId::DataSource,
    });
    // The other knowledge base's own job, addressed through its own data source
    // but with the FIRST knowledge base's job id: the scope resolves, so the job
    // is named.
    rows.push(Row {
        case: "a stored data source addressed with another data source's job".to_string(),
        knowledge_base_id: other_knowledge_base_id.clone(),
        data_source_id: other_data_source_id.clone(),
        ingestion_job_id: stored_job.clone(),
        resource_type: "IngestionJob",
        named: NamedId::IngestionJob,
    });

    for row in &rows {
        let answer = emulator
            .get_ingestion_job(
                &row.knowledge_base_id,
                &row.data_source_id,
                &row.ingestion_job_id,
            )
            .await;
        let named = match row.named {
            NamedId::DataSource => &row.data_source_id,
            NamedId::IngestionJob => &row.ingestion_job_id,
        };
        assert_not_found(&row.case, row.resource_type, named, &answer);
    }

    // The stored job is still readable through its own triple, so the refusals
    // above are about resolution and not about having broken the reader.
    let stored = emulator
        .get_ingestion_job(&knowledge_base_id, &data_source_id, &stored_job)
        .await;
    assert_eq!(
        stored.status, 200,
        "the stored job must still be readable, got {stored:?}"
    );
    assert_eq!(
        stored.string(&["ingestionJob", "ingestionJobId"]),
        stored_job,
        "the stored job must report its own id, got {:?}",
        stored.body
    );
}

/// RULE applied to the read that addresses the data source itself: it resolves
/// the same pair, so a request whose knowledge base id resolves to nothing is
/// refused with the DATA SOURCE sentence too — the pair, not the knowledge base
/// alone, is what a data-source-scoped read resolves.
#[tokio::test]
async fn a_data_source_read_names_the_data_source_its_pair_resolves_to_none() {
    let emulator = Emulator::new();
    let knowledge_base_id = emulator.knowledge_base("nested-scope-ds-kb").await;
    let other_knowledge_base_id = emulator.knowledge_base("nested-scope-ds-other-kb").await;
    let data_source_id = emulator
        .data_source(&knowledge_base_id, "nested-scope-ds-source")
        .await;

    // The rows differ along which half of the pair resolves to nothing: the data
    // source, the knowledge base, both, and a stored data source read through
    // another stored knowledge base.
    let mut rows: Vec<(String, String, String)> = Vec::new();
    for absent in ABSENT_IDS {
        rows.push((
            format!("a data source id nothing holds ({absent})"),
            knowledge_base_id.clone(),
            absent.to_string(),
        ));
        rows.push((
            format!("a knowledge base id nothing holds ({absent}) over a stored data source"),
            absent.to_string(),
            data_source_id.clone(),
        ));
        rows.push((
            format!("neither half of the pair held ({absent})"),
            absent.to_string(),
            absent.to_string(),
        ));
    }
    rows.push((
        "a stored data source read through another stored knowledge base".to_string(),
        other_knowledge_base_id.clone(),
        data_source_id.clone(),
    ));

    for (case, knowledge_base, data_source) in &rows {
        let answer = emulator.get_data_source(knowledge_base, data_source).await;
        assert_not_found(case, "DataSource", data_source, &answer);
        if knowledge_base != data_source {
            assert!(
                !answer.message().contains(knowledge_base.as_str()),
                "case {case:?}: the refusal must not name the enclosing knowledge base \
                 {knowledge_base:?}, got {:?}",
                answer.message()
            );
        }
    }

    // The stored data source is still readable through its own pair.
    let stored = emulator
        .get_data_source(&knowledge_base_id, &data_source_id)
        .await;
    assert_eq!(
        stored.status, 200,
        "the stored data source must still be readable, got {stored:?}"
    );
}
