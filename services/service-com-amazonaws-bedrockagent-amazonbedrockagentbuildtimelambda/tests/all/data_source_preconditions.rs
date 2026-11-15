// SPDX-License-Identifier: CC0-1.0

//! The pre-conditions a data source has to satisfy, end to end over HTTP:
//! which kinds of data source a knowledge base takes is decided by the
//! knowledge base's OWN kind, and ingestion is served only by a data source in
//! the status the model documents as ready for it.
//!
//! Every assertion here goes through the router, so it exercises the real wire
//! path (route matching, model constraint validation, serialization) rather
//! than calling a handler directly.

use std::sync::Arc;
use std::time::{Duration, Instant};

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

/// The S3 Vectors store a self-managed vector knowledge base here binds to.
const VECTOR_BUCKET: &str = "arn:aws:s3vectors:::bucket/preconditions-vectors";
const VECTOR_INDEX: &str = "preconditions-index";

/// A secret ARN matching the modelled `SecretArn` pattern, for the connectors
/// whose source configuration requires credentials.
const CREDENTIALS_SECRET: &str =
    "arn:aws:secretsmanager:eu-west-1:000000000000:secret:preconditions-credentials";

/// A Kendra index ARN matching the modelled `KendraIndexArn` pattern.
const KENDRA_INDEX: &str =
    "arn:aws:kendra:eu-west-1:000000000000:index/aaaaaaaa-1111-2222-3333-444444444444";

/// Every value of the modelled `KnowledgeBaseType`, which is what decides which
/// kinds of data source the knowledge base takes.
///
/// Source: this service's Smithy model
/// (`smithy_query(query="shape",shape="KnowledgeBaseType")`).
const KNOWLEDGE_BASE_TYPES: [&str; 4] = ["VECTOR", "KENDRA", "SQL", "MANAGED"];

/// Every value of the modelled `DataSourceType`.
///
/// Source: this service's Smithy model
/// (`smithy_query(query="shape",shape="DataSourceType")`).
const DATA_SOURCE_TYPES: [&str; 8] = [
    "S3",
    "WEB",
    "CONFLUENCE",
    "SALESFORCE",
    "SHAREPOINT",
    "CUSTOM",
    "REDSHIFT_METADATA",
    "MANAGED_KNOWLEDGE_BASE_CONNECTOR",
];

/// Every value the modelled `DataSourceStatus` enum declares, so a status this
/// emulator names in a refusal is asserted to be one of them.
///
/// Source: this service's Smithy model
/// (`smithy_query(query="shape",shape="DataSourceStatus")`).
const DATA_SOURCE_STATUSES: [&str; 6] = [
    "AVAILABLE",
    "DELETING",
    "DELETE_UNSUCCESSFUL",
    "CREATING",
    "UPDATING",
    "FAILED",
];

/// The one `DataSourceStatus` the model documents as ready for ingestion.
const READY_FOR_INGESTION: &str = "AVAILABLE";

/// How long a poll waits for a status the clock owes: generous, because what is
/// waited for is the CONDITION and the deadline only bounds a hung emulator.
const POLL_DEADLINE: Duration = Duration::from_secs(30);

/// How often that poll looks again.
const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// One HTTP answer from the emulator, parsed.
#[derive(Debug)]
struct Answer {
    status: u16,
    /// The restJson1 error shape name, from `x-amzn-errortype` or the body's
    /// `__type`; `None` on a success.
    error_type: Option<String>,
    /// The `x-amzn-errortype` header on its own, which a restJson1 client reads
    /// before the body.
    error_type_header: Option<String>,
    body: Value,
}

impl Answer {
    /// The refusal's `message` member.
    fn message(&self) -> String {
        self.body["message"].as_str().unwrap_or_default().to_string()
    }

    /// A member of the response's `dataSource`.
    fn data_source(&self, member: &str) -> &Value {
        &self.body["dataSource"][member]
    }

    /// The `dataSourceConfiguration.type` of the response's `dataSource`.
    fn data_source_type(&self) -> String {
        self.data_source("dataSourceConfiguration")["type"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    }

    fn data_source_status(&self) -> String {
        self.data_source("status")
            .as_str()
            .unwrap_or_default()
            .to_string()
    }

    /// A list member of the response, as a slice.
    fn list(&self, member: &str) -> Vec<Value> {
        self.body[member].as_array().cloned().unwrap_or_default()
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
        // The self-managed vector knowledge bases here bind to an S3 Vectors
        // index, which `CreateKnowledgeBase` resolves in the service that owns
        // it — so the index is created first, as a caller creates it first
        // against AWS.
        crate::vector_store_fixture::seed_index(
            &engine,
            VECTOR_BUCKET,
            VECTOR_INDEX,
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
        let error_type_header = response
            .headers()
            .get("x-amzn-errortype")
            .and_then(|value| value.to_str().ok())
            .map(ToString::to_string);
        let raw = response.into_body();
        let body: Value = if raw.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&raw).unwrap_or_else(|e| {
                panic!(
                    "{method} {uri} -> {status} body is not JSON ({e}): {}",
                    String::from_utf8_lossy(&raw)
                )
            })
        };
        let error_type = error_type_header.clone().or_else(|| {
            body.get("__type")
                .and_then(Value::as_str)
                .map(ToString::to_string)
        });
        Answer {
            status,
            error_type,
            error_type_header,
            body,
        }
    }

    /// Create a knowledge base and answer its minted `knowledgeBaseId`.
    async fn knowledge_base(&self, request: Value) -> String {
        let created = self.call("PUT", "/knowledgebases/", Some(request)).await;
        assert_eq!(
            created.status, 202,
            "case setup: expected CreateKnowledgeBase -> 202, got {created:?}"
        );
        created.body["knowledgeBase"]["knowledgeBaseId"]
            .as_str()
            .expect("a minted knowledgeBaseId")
            .to_string()
    }

    async fn create_data_source(&self, knowledge_base_id: &str, request: Value) -> Answer {
        self.call(
            "PUT",
            &format!("/knowledgebases/{knowledge_base_id}/datasources/"),
            Some(request),
        )
        .await
    }

    /// Create a data source that is expected to be accepted, and answer its
    /// minted `dataSourceId`.
    async fn data_source_id(&self, knowledge_base_id: &str, request: Value) -> String {
        let created = self.create_data_source(knowledge_base_id, request).await;
        assert_eq!(
            created.status, 200,
            "case setup: expected CreateDataSource -> 200, got {created:?}"
        );
        created.data_source("dataSourceId")
            .as_str()
            .expect("a minted dataSourceId")
            .to_string()
    }

    async fn update_data_source(
        &self,
        knowledge_base_id: &str,
        data_source_id: &str,
        request: Value,
    ) -> Answer {
        self.call(
            "PUT",
            &format!("/knowledgebases/{knowledge_base_id}/datasources/{data_source_id}"),
            Some(request),
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

    /// How many data sources the knowledge base holds, read back through
    /// `ListDataSources`.
    async fn data_source_count(&self, knowledge_base_id: &str) -> usize {
        let listed = self
            .call(
                "POST",
                &format!("/knowledgebases/{knowledge_base_id}/datasources/"),
                Some(json!({})),
            )
            .await;
        assert_eq!(
            listed.status, 200,
            "case setup: expected ListDataSources -> 200, got {listed:?}"
        );
        listed.list("dataSourceSummaries").len()
    }

    async fn start_ingestion_job(&self, knowledge_base_id: &str, data_source_id: &str) -> Answer {
        self.call(
            "PUT",
            &format!(
                "/knowledgebases/{knowledge_base_id}/datasources/{data_source_id}/ingestionjobs/"
            ),
            Some(json!({})),
        )
        .await
    }

    /// How many ingestion jobs the data source holds, read back through
    /// `ListIngestionJobs`.
    async fn ingestion_job_count(&self, knowledge_base_id: &str, data_source_id: &str) -> usize {
        let listed = self
            .call(
                "POST",
                &format!(
                    "/knowledgebases/{knowledge_base_id}/datasources/{data_source_id}\
                     /ingestionjobs/"
                ),
                Some(json!({})),
            )
            .await;
        assert_eq!(
            listed.status, 200,
            "case setup: expected ListIngestionJobs -> 200, got {listed:?}"
        );
        listed.list("ingestionJobSummaries").len()
    }

    /// Read the data source until it reports `wanted`, or until the deadline
    /// passes — the condition is what is waited for, not a span of time.
    async fn data_source_in_status(
        &self,
        case: &str,
        knowledge_base_id: &str,
        data_source_id: &str,
        wanted: &str,
    ) {
        let deadline = Instant::now() + POLL_DEADLINE;
        loop {
            let fetched = self.get_data_source(knowledge_base_id, data_source_id).await;
            let status = fetched.data_source_status();
            if status == wanted {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "case {case}: expected the data source to reach {wanted}, last read \
                 {status:?} in {fetched:?}"
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }
}

/// A knowledge base request of the kind `knowledge_base_type` names, carrying
/// that kind's own configuration member — and, for the self-managed vector
/// kind, the vector store it also requires.
///
/// Source: the `KnowledgeBaseType` enum and the `KnowledgeBaseConfiguration`,
/// `KendraKnowledgeBaseConfiguration` and `SqlKnowledgeBaseConfiguration`
/// structures of this service's Smithy model
/// (`smithy_query(query="shape",shape="KnowledgeBaseType,KnowledgeBaseConfiguration")`).
fn knowledge_base_of_type(name: &str, knowledge_base_type: &str) -> Value {
    let mut request = json!({ "name": name, "roleArn": KB_ROLE });
    match knowledge_base_type {
        "VECTOR" => {
            request["knowledgeBaseConfiguration"] = json!({
                "type": "VECTOR",
                "vectorKnowledgeBaseConfiguration": { "embeddingModelArn": EMBEDDING_MODEL },
            });
            request["storageConfiguration"] = json!({
                "type": "S3_VECTORS",
                "s3VectorsConfiguration": {
                    "vectorBucketArn": VECTOR_BUCKET,
                    "indexName": VECTOR_INDEX,
                },
            });
        }
        "KENDRA" => {
            request["knowledgeBaseConfiguration"] = json!({
                "type": "KENDRA",
                "kendraKnowledgeBaseConfiguration": { "kendraIndexArn": KENDRA_INDEX },
            });
        }
        "SQL" => {
            request["knowledgeBaseConfiguration"] = json!({
                "type": "SQL",
                "sqlKnowledgeBaseConfiguration": { "type": "REDSHIFT" },
            });
        }
        "MANAGED" => {
            request["knowledgeBaseConfiguration"] = json!({
                "type": "MANAGED",
                "managedKnowledgeBaseConfiguration": { "embeddingModelType": "MANAGED" },
            });
        }
        other => panic!("{other} is not a modelled KnowledgeBaseType"),
    }
    request
}

/// A data source request of the kind `data_source_type` names, carrying that
/// kind's own connection member where the model pairs one with it.
///
/// Source: the `DataSourceType` enum and the `DataSourceConfiguration`,
/// `S3DataSourceConfiguration`, `WebDataSourceConfiguration`,
/// `ConfluenceSourceConfiguration`, `SalesforceSourceConfiguration` and
/// `SharePointSourceConfiguration` structures of this service's Smithy model
/// (`smithy_query(query="shape",shape="DataSourceType,DataSourceConfiguration")`),
/// which gives `CUSTOM` and `REDSHIFT_METADATA` no member of their own.
fn data_source_of_type(name: &str, data_source_type: &str) -> Value {
    let configuration = match data_source_type {
        "S3" => json!({
            "type": "S3",
            "s3Configuration": { "bucketArn": "arn:aws:s3:::preconditions-docs" },
        }),
        "WEB" => json!({
            "type": "WEB",
            "webConfiguration": {
                "sourceConfiguration": {
                    "urlConfiguration": { "seedUrls": [{ "url": "https://example.com/docs" }] },
                },
            },
        }),
        "CONFLUENCE" => json!({
            "type": "CONFLUENCE",
            "confluenceConfiguration": {
                "sourceConfiguration": {
                    "hostUrl": "https://example.atlassian.net",
                    "hostType": "SAAS",
                    "authType": "BASIC",
                    "credentialsSecretArn": CREDENTIALS_SECRET,
                },
            },
        }),
        "SALESFORCE" => json!({
            "type": "SALESFORCE",
            "salesforceConfiguration": {
                "sourceConfiguration": {
                    "hostUrl": "https://example.my.salesforce.com",
                    "authType": "OAUTH2_CLIENT_CREDENTIALS",
                    "credentialsSecretArn": CREDENTIALS_SECRET,
                },
            },
        }),
        "SHAREPOINT" => json!({
            "type": "SHAREPOINT",
            "sharePointConfiguration": {
                "sourceConfiguration": {
                    "domain": "example",
                    "siteUrls": ["https://example.sharepoint.com/sites/docs"],
                    "hostType": "ONLINE",
                    "authType": "OAUTH2_CLIENT_CREDENTIALS",
                    "credentialsSecretArn": CREDENTIALS_SECRET,
                },
            },
        }),
        "MANAGED_KNOWLEDGE_BASE_CONNECTOR" => json!({
            "type": "MANAGED_KNOWLEDGE_BASE_CONNECTOR",
            "managedKnowledgeBaseConnectorConfiguration": {
                "connectorParameters": { "type": "CUSTOM", "version": "1" },
            },
        }),
        // CUSTOM and REDSHIFT_METADATA are the kinds the model pairs with no
        // sibling connection member, so they need none.
        other => json!({ "type": other }),
    };
    json!({ "name": name, "dataSourceConfiguration": configuration })
}

/// A `MANAGED_KNOWLEDGE_BASE_CONNECTOR` data source whose connector names a
/// source OUTSIDE the service: the connection has to be brought up before the
/// data source can serve ingestion, so it is not ready when it is created.
fn connector_over_an_external_source(name: &str) -> Value {
    json!({
        "name": name,
        "dataSourceConfiguration": {
            "type": "MANAGED_KNOWLEDGE_BASE_CONNECTOR",
            "managedKnowledgeBaseConnectorConfiguration": {
                "connectorParameters": {
                    "type": "WEB",
                    "version": "1",
                    "connectionConfiguration": { "url": "https://example.com/docs" },
                },
            },
        },
    })
}

/// Whether a knowledge base of `knowledge_base_type` supports a data source of
/// `data_source_type`: the managed-connector kind belongs to a service-managed
/// knowledge base, and every kind that carries its own connection details
/// belongs to a self-managed one.
fn supported(knowledge_base_type: &str, data_source_type: &str) -> bool {
    (knowledge_base_type == "MANAGED") == (data_source_type == "MANAGED_KNOWLEDGE_BASE_CONNECTOR")
}

/// The sentence a call that stores a data source words when the knowledge
/// base's kind does not support the kind of data source the request carries:
/// the knowledge base's own `KnowledgeBaseType` is the one variable part.
fn unsupported_type_message(knowledge_base_type: &str) -> String {
    format!("Unsupported data source type for {knowledge_base_type} knowledge base type.")
}

/// Assert `answer` is the modelled client refusal `code` carries, with
/// `message` as its sentence — in the body AND in the header a restJson1 client
/// reads first.
fn assert_refusal(case: &str, code: &str, status: u16, message: &str, answer: &Answer) {
    assert_eq!(
        answer.status, status,
        "case {case}: expected HTTP {status}, got {answer:?}"
    );
    assert_eq!(
        answer.error_type.as_deref(),
        Some(code),
        "case {case}: expected the {code} error shape, got {answer:?}"
    );
    assert_eq!(
        answer.error_type_header.as_deref(),
        Some(code),
        "case {case}: expected x-amzn-errortype: {code}, got {answer:?}"
    );
    assert_eq!(
        answer.message(),
        message,
        "case {case}: expected the message {message:?}, got {:?}",
        answer.message()
    );
}

/// Which kinds of data source a knowledge base takes is decided by the
/// knowledge base's own kind: a request naming a kind that knowledge base's
/// kind does not support is refused with the modelled `ValidationException`
/// naming the knowledge base's kind, and nothing is written.
///
/// The rows are every modelled `KnowledgeBaseType` crossed with every modelled
/// `DataSourceType`, so they differ along the kind of the knowledge base, the
/// kind of the data source, whether that kind carries its own connection
/// details and whether the two agree — the recorded case (a self-managed kind
/// against a MANAGED knowledge base) is one row of the 32.
#[tokio::test]
async fn a_data_source_kind_the_knowledge_bases_kind_does_not_support_is_refused() {
    let emulator = Emulator::new();

    for knowledge_base_type in KNOWLEDGE_BASE_TYPES {
        let knowledge_base_id = emulator
            .knowledge_base(knowledge_base_of_type(
                &format!("preconditions-{}", knowledge_base_type.to_lowercase()),
                knowledge_base_type,
            ))
            .await;
        // What the knowledge base holds, so a refused create is asserted to
        // have written nothing rather than merely to have answered an error.
        let mut stored = 0usize;

        for data_source_type in DATA_SOURCE_TYPES {
            let case = &format!("a {data_source_type} data source of a {knowledge_base_type} \
                 knowledge base");
            let request = data_source_of_type(
                &format!(
                    "ds-{}-{}",
                    knowledge_base_type.to_lowercase(),
                    data_source_type.to_lowercase()
                ),
                data_source_type,
            );
            let created = emulator
                .create_data_source(&knowledge_base_id, request)
                .await;

            if supported(knowledge_base_type, data_source_type) {
                assert_eq!(
                    created.status, 200,
                    "case {case}: the kinds agree, so the create is accepted, got {created:?}"
                );
                stored += 1;
                // The round trip: the stored data source, read back through
                // GetDataSource, is of the kind the accepted request named.
                let data_source_id = created
                    .data_source("dataSourceId")
                    .as_str()
                    .unwrap_or_else(|| panic!("case {case}: no minted dataSourceId in {created:?}"))
                    .to_string();
                let fetched = emulator
                    .get_data_source(&knowledge_base_id, &data_source_id)
                    .await;
                assert_eq!(
                    fetched.status, 200,
                    "case {case}: expected GetDataSource -> 200, got {fetched:?}"
                );
                assert_eq!(
                    fetched.data_source_type(),
                    data_source_type,
                    "case {case}: expected the stored data source to be of the kind the \
                     request named, got {fetched:?}"
                );
            } else {
                assert_refusal(
                    case,
                    "ValidationException",
                    400,
                    &unsupported_type_message(knowledge_base_type),
                    &created,
                );
            }

            let held = emulator.data_source_count(&knowledge_base_id).await;
            assert_eq!(
                held, stored,
                "case {case}: expected the knowledge base to hold {stored} data source(s), \
                 got {held}"
            );
        }
    }
}

/// The same cross-check governs an UPDATE: a data source cannot be moved to a
/// kind the knowledge base that holds it does not support, and the refused
/// update leaves the stored configuration exactly as it was.
#[tokio::test]
async fn an_update_cannot_store_a_data_source_kind_the_knowledge_base_does_not_support() {
    let emulator = Emulator::new();

    for knowledge_base_type in KNOWLEDGE_BASE_TYPES {
        let knowledge_base_id = emulator
            .knowledge_base(knowledge_base_of_type(
                &format!("update-{}", knowledge_base_type.to_lowercase()),
                knowledge_base_type,
            ))
            .await;
        // The kind this knowledge base DOES support, which is what the data
        // source is created as and must still be after every refused update.
        let held_type = if knowledge_base_type == "MANAGED" {
            "MANAGED_KNOWLEDGE_BASE_CONNECTOR"
        } else {
            "CUSTOM"
        };
        let name = format!("update-ds-{}", knowledge_base_type.to_lowercase());
        let data_source_id = emulator
            .data_source_id(&knowledge_base_id, data_source_of_type(&name, held_type))
            .await;

        for data_source_type in DATA_SOURCE_TYPES {
            if supported(knowledge_base_type, data_source_type) {
                continue;
            }
            let case = &format!(
                "updating a {held_type} data source of a {knowledge_base_type} knowledge base \
                 to {data_source_type}"
            );
            let answered = emulator
                .update_data_source(
                    &knowledge_base_id,
                    &data_source_id,
                    data_source_of_type(&name, data_source_type),
                )
                .await;
            assert_refusal(
                case,
                "ValidationException",
                400,
                &unsupported_type_message(knowledge_base_type),
                &answered,
            );

            let fetched = emulator
                .get_data_source(&knowledge_base_id, &data_source_id)
                .await;
            assert_eq!(
                fetched.data_source_type(),
                held_type,
                "case {case}: a refused update writes nothing, so the stored kind is \
                 unchanged, got {fetched:?}"
            );
        }
    }
}

/// `StartIngestionJob` serves a data source only in the one status the model
/// documents as ready for ingestion: in any other status the job is refused
/// with the modelled `ConflictException` naming the status the data source is
/// actually in, and no job is started.
///
/// The rows differ along the status the data source is in and how it got there:
/// ready as soon as it was stored, still bringing its connector's connection up
/// (the transient status a create leaves it in), settled on the outcome of that
/// connection, and being changed by an update.
#[tokio::test]
async fn an_ingestion_job_is_refused_on_a_data_source_whose_status_does_not_serve_ingestion() {
    let emulator = Emulator::new();
    let vector_kb = emulator
        .knowledge_base(knowledge_base_of_type("ingestion-gate-vector", "VECTOR"))
        .await;
    let managed_kb = emulator
        .knowledge_base(knowledge_base_of_type("ingestion-gate-managed", "MANAGED"))
        .await;

    // A data source the service has nothing to connect out for is ready as soon
    // as it is stored, so its ingestion job is accepted.
    let ready = emulator
        .data_source_id(&vector_kb, data_source_of_type("gate-ready", "CUSTOM"))
        .await;
    let started = emulator.start_ingestion_job(&vector_kb, &ready).await;
    assert_eq!(
        started.status, 202,
        "case a data source ready for ingestion: expected StartIngestionJob -> 202, \
         got {started:?}"
    );
    assert_eq!(
        emulator.ingestion_job_count(&vector_kb, &ready).await,
        1,
        "case a data source ready for ingestion: the accepted start leaves one job behind"
    );

    // A connector with a source outside the service to reach is not ready when
    // it is created: the clock owes it the outcome of bringing that connection
    // up, so at this instant it is in the transient status a create leaves it
    // in or already in the one that outcome settles at — and in neither does it
    // serve ingestion.
    let creating = emulator
        .data_source_id(&managed_kb, connector_over_an_external_source("gate-creating"))
        .await;
    let case = "a data source whose connector's connection is still being brought up";
    let refused = emulator.start_ingestion_job(&managed_kb, &creating).await;
    let named = refused_status(case, &refused);
    assert!(
        ["CREATING", "FAILED"].contains(&named.as_str()),
        "case {case}: expected the refusal to name the transient status a create leaves a \
         connector in or the one it settles at, got {named:?} in {refused:?}"
    );
    assert_eq!(
        emulator.ingestion_job_count(&managed_kb, &creating).await,
        0,
        "case {case}: a refused start begins no job"
    );

    // Once the clock has settled that connection's outcome the data source is
    // FAILED, and the refusal names that status.
    let case = "a data source whose connector's connection did not come up";
    emulator
        .data_source_in_status(case, &managed_kb, &creating, "FAILED")
        .await;
    let refused = emulator.start_ingestion_job(&managed_kb, &creating).await;
    assert_refusal(
        case,
        "ConflictException",
        409,
        "You cannot start an ingestion job on a data source with status FAILED.",
        &refused,
    );
    assert_eq!(
        emulator.ingestion_job_count(&managed_kb, &creating).await,
        0,
        "case {case}: a refused start begins no job"
    );

    // An accepted update leaves the data source in the transient status the
    // service applies the change in: either the start is refused naming that
    // status, or the clock has already settled the data source back to ready
    // and the start is served — never a job on a data source that is not ready.
    let updating = emulator
        .data_source_id(&vector_kb, data_source_of_type("gate-updating", "CUSTOM"))
        .await;
    let updated = emulator
        .update_data_source(
            &vector_kb,
            &updating,
            data_source_of_type("gate-updating", "CUSTOM"),
        )
        .await;
    assert_eq!(
        updated.status, 200,
        "case setup: expected UpdateDataSource -> 200, got {updated:?}"
    );
    let case = "a data source an update is being applied to";
    let answered = emulator.start_ingestion_job(&vector_kb, &updating).await;
    if answered.status == 202 {
        assert_eq!(
            emulator.ingestion_job_count(&vector_kb, &updating).await,
            1,
            "case {case}: the data source was ready again, so the accepted start leaves one \
             job behind"
        );
    } else {
        let named = refused_status(case, &answered);
        assert_eq!(
            named, "UPDATING",
            "case {case}: expected the refusal to name the status an update leaves the data \
             source in, got {named:?} in {answered:?}"
        );
        assert_eq!(
            emulator.ingestion_job_count(&vector_kb, &updating).await,
            0,
            "case {case}: a refused start begins no job"
        );
    }
}

/// The status a refusal of `StartIngestionJob` names, read out of the sentence
/// it words: the refusal is asserted to be the modelled conflict, its sentence
/// to be that one sentence, and the status it names to be a modelled
/// `DataSourceStatus` other than the one that serves ingestion.
fn refused_status(case: &str, answer: &Answer) -> String {
    assert_eq!(
        answer.status, 409,
        "case {case}: expected HTTP 409, got {answer:?}"
    );
    assert_eq!(
        answer.error_type.as_deref(),
        Some("ConflictException"),
        "case {case}: expected the ConflictException error shape, got {answer:?}"
    );
    assert_eq!(
        answer.error_type_header.as_deref(),
        Some("ConflictException"),
        "case {case}: expected x-amzn-errortype: ConflictException, got {answer:?}"
    );
    let message = answer.message();
    let prefix = "You cannot start an ingestion job on a data source with status ";
    let named = message
        .strip_prefix(prefix)
        .and_then(|rest| rest.strip_suffix('.'))
        .unwrap_or_else(|| {
            panic!(
                "case {case}: expected the sentence {prefix:?} closed by the data source's \
                 status and a period, got {message:?}"
            )
        })
        .to_string();
    assert!(
        DATA_SOURCE_STATUSES.contains(&named.as_str()) && named != READY_FOR_INGESTION,
        "case {case}: expected a modelled DataSourceStatus other than \
         {READY_FOR_INGESTION} ({DATA_SOURCE_STATUSES:?}), got {named:?}"
    );
    named
}
