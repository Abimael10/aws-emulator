// SPDX-License-Identifier: CC0-1.0

//! WHERE a deployment of an OpenSearch-Serverless-backed knowledge base fails when the
//! collection's own `data` access policies do or do not grant the knowledge base's role the
//! index: the OpenSearch Serverless control and data planes, the Bedrock Agent build-time
//! plane and the Bedrock Agent runtime plane, over one engine.
//!
//! RULE: for a knowledge base whose `storageConfiguration` names a vector index of an
//! OpenSearch Serverless collection, the collection's own data access policies decide the
//! binding and therefore WHICH call a deployment fails at — a role those policies grant the
//! index permissions a knowledge base performs has its ingestion write that index and its
//! `Retrieve` answered 200 with the chunk the ingestion took in, while a role they do not
//! grant them is refused at `CreateKnowledgeBase` with the modelled `ValidationException`
//! (400) carrying the dependency's own authorization detail, so no knowledge base is written
//! and no retrieval of one is ever reached. The STORE TYPE is never itself a failed
//! dependency: `OPENSEARCH_SERVERLESS` is a store a vector knowledge base is built on, so a
//! retrieval over one is served — with the chunks it holds, or with none when it holds none —
//! and a retrieval that cannot resolve a knowledge base at all is the modelled
//! `ResourceNotFoundException` (404), never a dependency failure standing in for either.
//!
//! Four crates have to agree for that to hold — the collection, its policies and its index
//! are served by `service-com-amazonaws-opensearchserverless-opensearchserverless`, the
//! knowledge base and the ingestion by the Bedrock Agent build-time plane, the retrieval by
//! the Bedrock Agent runtime plane — so the suite lives here rather than in any one of them.
//! The crate-local halves pin each side on its own: what a collection's policies decide about
//! a knowledge base's role in `services/service-com-amazonaws-bedrockagent-\
//! amazonbedrockagentbuildtimelambda/tests/all/knowledge_base_role_and_vector_store_access.rs`,
//! and where a served retrieval reads its chunks from in
//! `bedrock_retrieval_reads_the_bound_collection_index.rs`. What is pinned HERE is that one
//! deployment told both ways is distinguishable: the two outcomes are different calls with
//! different answers, so a client can tell a policy mistake from a healthy stack.
//!
//! The rows span what the rule generalises over: how the grant is written (the exact index
//! resource, a wildcarded one, every `aoss` permission against the ones a knowledge base
//! performs, the role named against every principal) and how it falls short (no policy at
//! all, another principal, the collection rather than its indexes, another index of the same
//! collection, the index reads without the write, the write alone), beside the bytes the
//! ingested document carries. The knowledge base's ROLE is the only thing the rows vary: the
//! caller that creates the index beforehand presents the account's own published credential,
//! which the data access policies do not bound, so that step is admitted by the network
//! policy alone whatever the row grants — the reading
//! [`crate::bedrock_knowledge_base_over_an_aoss_collection::grant_the_role_data_access`]
//! states and the one the collection endpoint's own authorization implements.
//!
//! The wire harness is the one the sibling AOSS suites share
//! ([`crate::bedrock_knowledge_base_over_an_aoss_collection`]), so the request forms and the
//! poll deadlines are stated once. Nothing here waits for a fixed time or asserts how long a
//! step took: every wait is for the CONDITION, under a generous deadline.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::bedrock_knowledge_base_over_an_aoss_collection::{
    active_collection, admit_network_access, aoss, at, await_active_knowledge_base,
    await_indexed_document, bedrock, build_router, encrypt_collections, inline_byte_document,
    opensearch_storage, rest, text, KB_ROLE, METADATA_FIELD, REGION, TEXT_FIELD, VECTOR_FIELD,
};

/// Another role, for the rows whose policy grants the index to someone other than the
/// knowledge base's own role. Matches the modelled `KnowledgeBaseRoleArn` pattern, as
/// [`KB_ROLE`] does.
const OTHER_ROLE: &str = "arn:aws:iam::000000000000:role/service-role/AmazonBedrockOtherRole";

/// The embedding model every knowledge base here is created with, and the number of values
/// its vectors carry — which is the dimension the vector index is created with.
/// `amazon.titan-embed-text-v2:0` publishes 1024-value vectors.
const EMBEDDING_MODEL: &str = "amazon.titan-embed-text-v2:0";
const DIMENSION: usize = 1024;

/// The permissions a knowledge base performs on the vector index it is bound to, which are
/// the ones its role has to be granted for the binding to be served AT CREATE: Amazon Bedrock
/// reads the index's own definition to check the field mapping, writes a document per
/// ingested chunk, and reads them back to serve a retrieval, and it resolves all three when
/// the knowledge base is created.
///
/// Source: the index-level permissions AWS's "Create a vector index in Amazon OpenSearch
/// Service" states for the knowledge-base role's data access policy, which is the set the
/// build-time plane resolves the binding against
/// (`service-com-amazonaws-bedrockagent-amazonbedrockagentbuildtimelambda`'s
/// `state::opensearch_serverless_store::REQUIRED_INDEX_PERMISSIONS`), spelled with the
/// permission names the owning service publishes
/// (`service-com-amazonaws-opensearchserverless-opensearchserverless`'s
/// `state::data_access::permission`).
const INDEX_PERMISSIONS: [&str; 3] = [
    "aoss:DescribeIndex",
    "aoss:ReadDocument",
    "aoss:WriteDocument",
];

/// The `ValidationException` message Amazon Bedrock answers when the vector store refuses the
/// knowledge base's own role: this service's opening for an invalid storage configuration and
/// the dependency's own refusal — the status and the message the vector store answered the
/// authorization failure with. Fixed text for this error class.
const UNREACHABLE_STORAGE: &str = "The knowledge base storage configuration provided is \
                                   invalid... Dependency error document status code: 403, \
                                   error message: Bad Authorization";

/// One `Rules` entry of a `data` access policy: the permissions it grants on one resource
/// pattern of one resource type, as AWS's "Data access control for Amazon OpenSearch
/// Serverless" writes them.
fn rule(resource_type: &str, resource: String, permissions: &[&str]) -> Value {
    json!({
        "ResourceType": resource_type,
        "Resource": [resource],
        "Permission": permissions,
    })
}

/// A whole `data` access policy document: the rules one statement carries, about one
/// principal.
fn data_policy(rules: Vec<Value>, principal: &str) -> Value {
    json!([{ "Rules": rules, "Principal": [principal] }])
}

/// A deployed collection: the stack a client puts up before a knowledge base can name it —
/// the encryption and network policies it cannot be created or reached without, the data
/// access policy the row under test declares (if any), the collection itself, and the vector
/// index created at the endpoint the collection publishes.
struct Deployment {
    router: MultiServiceRouter,
    collection_arn: String,
    endpoint: String,
    index: String,
}

impl Deployment {
    /// Deploy one stack. `data_access` is the row's own policy document, left out entirely for
    /// a stack that deploys none.
    async fn put_up(collection: &str, index: &str, data_access: Option<Value>) -> Self {
        let router = build_router();
        encrypt_collections(&router).await;
        admit_network_access(&router).await;
        if let Some(policy) = data_access {
            let (status, answer) = aoss(
                &router,
                "CreateAccessPolicy",
                json!({
                    "type": "data",
                    "name": format!("{collection}-data"),
                    "policy": policy.to_string(),
                }),
            )
            .await;
            assert_eq!(
                status, 200,
                "setup: expected the data access policy accepted, got {status} with {answer}"
            );
        }
        let detail = active_collection(&router, collection).await;
        let collection_arn = text(&detail, "arn");
        let endpoint = text(&detail, "collectionEndpoint");
        assert!(
            !collection_arn.is_empty() && !endpoint.is_empty(),
            "setup: expected an ACTIVE collection to publish an ARN and an endpoint, got {detail}"
        );
        vector_index(&router, &endpoint, index).await;
        Self {
            router,
            collection_arn,
            endpoint,
            index: index.to_string(),
        }
    }

    /// `CreateKnowledgeBase` over this deployment's index, as the knowledge-base role,
    /// answering the raw `(status, body)`.
    async fn create_knowledge_base(&self, name: &str) -> (u16, Value) {
        bedrock(
            &self.router,
            "PUT",
            "/knowledgebases/",
            Some(&json!({
                "name": name,
                "roleArn": KB_ROLE,
                "knowledgeBaseConfiguration": {
                    "type": "VECTOR",
                    "vectorKnowledgeBaseConfiguration": {
                        "embeddingModelArn":
                            format!("arn:aws:bedrock:{REGION}::foundation-model/{EMBEDDING_MODEL}"),
                    },
                },
                "storageConfiguration": opensearch_storage(&self.collection_arn, &self.index),
            })),
        )
        .await
    }

    /// The names `ListKnowledgeBases` reports.
    async fn knowledge_base_names(&self) -> Vec<String> {
        let (status, listed) =
            bedrock(&self.router, "POST", "/knowledgebases/", Some(&json!({}))).await;
        assert_eq!(
            status, 200,
            "setup: expected ListKnowledgeBases to answer, got {status} with {listed}"
        );
        at(&listed, "knowledgeBaseSummaries")
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|summary| text(summary, "name"))
            .collect()
    }
}

/// Create the vector index at the collection's published endpoint, with the `knn_vector`
/// mapping a knowledge base's index declares.
///
/// This call is the CALLER's, not the knowledge base's: it presents the account's own
/// published credential, which the collection's data access policies do not bound, so it is
/// admitted by the network policy alone on every row — including the rows that deploy no data
/// access policy at all. That is what keeps the knowledge base's own role the one thing the
/// rows vary.
async fn vector_index(router: &MultiServiceRouter, endpoint: &str, index: &str) {
    let body = json!({
        "settings": { "index": { "knn": true } },
        "mappings": { "properties": {
            VECTOR_FIELD: { "type": "knn_vector", "dimension": DIMENSION },
            TEXT_FIELD: { "type": "text" },
            METADATA_FIELD: { "type": "text", "index": false },
        } },
    });
    let (status, created) = rest(router, endpoint, "PUT", &format!("/{index}"), Some(&body)).await;
    assert_eq!(
        status, 200,
        "setup: expected the index create at the published endpoint to be served, got {status} \
         with {created}"
    );
}

/// Attach a CUSTOM data source — the kind documents are pushed into rather than crawled from
/// a source of its own — and answer its minted id.
async fn custom_data_source(router: &MultiServiceRouter, knowledge_base_id: &str) -> String {
    let (status, created) = bedrock(
        router,
        "PUT",
        &format!("/knowledgebases/{knowledge_base_id}/datasources/"),
        Some(&json!({
            "name": "pushed",
            "dataSourceConfiguration": { "type": "CUSTOM" },
        })),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: expected CreateDataSource(CUSTOM) accepted, got {status} with {created}"
    );
    let id = text(&created, "dataSource.dataSourceId");
    assert!(
        !id.is_empty(),
        "setup: expected a minted dataSourceId, got {created}"
    );
    id
}

/// Push one inline document into `data_source_id` and look again until it reports `INDEXED` —
/// the CONDITION the retrievals below are about, rather than a fixed wait.
async fn ingest(
    router: &MultiServiceRouter,
    case: &str,
    knowledge_base_id: &str,
    data_source_id: &str,
    document_id: &str,
    bytes: &str,
) {
    let (status, accepted) = bedrock(
        router,
        "PUT",
        &format!("/knowledgebases/{knowledge_base_id}/datasources/{data_source_id}/documents"),
        Some(&json!({ "documents": [inline_byte_document(document_id, bytes)] })),
    )
    .await;
    assert_eq!(
        status, 202,
        "setup: expected the ingestion accepted, got {status} with {accepted}"
    );
    await_indexed_document(
        router,
        case,
        knowledge_base_id,
        data_source_id,
        document_id,
    )
    .await;
}

/// `Retrieve` over one knowledge base, answering the raw `(status, body)`.
async fn retrieve(
    router: &MultiServiceRouter,
    knowledge_base_id: &str,
    query: &str,
) -> (u16, Value) {
    bedrock(
        router,
        "POST",
        &format!("/knowledgebases/{knowledge_base_id}/retrieve"),
        Some(&json!({
            "retrievalQuery": { "text": query },
            "retrievalConfiguration": { "vectorSearchConfiguration": { "numberOfResults": 10 } },
        })),
    )
    .await
}

/// The raw text the index of the collection at `endpoint` holds, as a CLIENT's own `_search`
/// reports it — the store that owns the chunks answering for them itself.
async fn indexed_text(
    router: &MultiServiceRouter,
    case: &str,
    endpoint: &str,
    index: &str,
) -> Vec<String> {
    let body = json!({ "query": { "match_all": {} }, "size": 50 });
    let (status, answered) = rest(
        router,
        endpoint,
        "POST",
        &format!("/{index}/_search"),
        Some(&body),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the collection's own search to be served, got {status} with \
         {answered}"
    );
    at(&answered, "hits.hits")
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|hit| text(hit, &format!("_source.{TEXT_FIELD}")))
        .collect()
}

/// RULE, the granted side: where the collection's data access policies grant the knowledge
/// base's role the index permissions a knowledge base performs, the whole deployment works —
/// `CreateKnowledgeBase` is accepted, the ingestion writes the collection's own index, and
/// `Retrieve` answers 200 with the bytes that were ingested. The store type is never a failed
/// dependency.
///
/// The rows span how the grant is written and the bytes the document carries.
#[tokio::test]
async fn a_granted_role_makes_the_whole_retrieval_work() {
    /// One case: how the collection's data access policy spells the grant, and the document
    /// the deployment ingests.
    struct Row {
        case: &'static str,
        /// The policy document, built from the collection and index names of the row's own
        /// deployment.
        policy: fn(&str, &str) -> Value,
        document: &'static str,
    }
    let rows: &[Row] = &[
        Row {
            case: "the role granted the index permissions a knowledge base performs, on the \
                   exact index",
            policy: |collection, index| {
                data_policy(
                    vec![rule(
                        "index",
                        format!("index/{collection}/{index}"),
                        &INDEX_PERMISSIONS,
                    )],
                    KB_ROLE,
                )
            },
            document: "Manifest: the klystron bay holds forty spare units.",
        },
        Row {
            case: "the role granted every aoss permission on every index of the collection",
            policy: |collection, _| {
                data_policy(
                    vec![
                        rule("index", format!("index/{collection}/*"), &["aoss:*"]),
                        rule("collection", format!("collection/{collection}"), &["aoss:*"]),
                    ],
                    KB_ROLE,
                )
            },
            document: "Dossier: Ωμέγα — 四十の導波管 — ударная секция — 🛰 orbiting relay.",
        },
        Row {
            case: "every principal granted the index permissions",
            policy: |collection, _| {
                data_policy(
                    vec![rule(
                        "index",
                        format!("index/{collection}/*"),
                        &INDEX_PERMISSIONS,
                    )],
                    "*",
                )
            },
            document: "Ledger: the propulsion team drew nine waveguides.",
        },
        Row {
            case: "the role granted the index creates and deletes as well",
            policy: |collection, index| {
                data_policy(
                    vec![rule(
                        "index",
                        format!("index/{collection}/{index}*"),
                        &[
                            "aoss:CreateIndex",
                            "aoss:DeleteIndex",
                            "aoss:DescribeIndex",
                            "aoss:ReadDocument",
                            "aoss:WriteDocument",
                        ],
                    )],
                    KB_ROLE,
                )
            },
            document: "Addendum: spare klystrons ship from the 導波管 depot.",
        },
    ];
    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let collection = format!("kb-grant-{index}");
        let vector_index_name = format!("kb-grant-index-{index}");
        let deployment = Deployment::put_up(
            &collection,
            &vector_index_name,
            Some((row.policy)(&collection, &vector_index_name)),
        )
        .await;

        let name = format!("kb-granted-{index}");
        let (status, created) = deployment.create_knowledge_base(&name).await;
        assert_eq!(
            status, 202,
            "case {case:?}: expected CreateKnowledgeBase accepted, got {status} with {created}"
        );
        let knowledge_base_id = text(&created, "knowledgeBase.knowledgeBaseId");
        assert!(
            !knowledge_base_id.is_empty(),
            "case {case:?}: expected a minted knowledgeBaseId, got {created}"
        );
        await_active_knowledge_base(&deployment.router, case, &knowledge_base_id).await;
        let data_source_id = custom_data_source(&deployment.router, &knowledge_base_id).await;
        ingest(
            &deployment.router,
            case,
            &knowledge_base_id,
            &data_source_id,
            "manifest",
            row.document,
        )
        .await;

        // The chunk is in the COLLECTION's own index, as that store's own search reports it:
        // what the retrieval below answers is backed by the store that owns it.
        let held = indexed_text(
            &deployment.router,
            case,
            &deployment.endpoint,
            &deployment.index,
        )
        .await;
        assert_eq!(
            held,
            vec![row.document.to_string()],
            "case {case:?}: expected the ingestion to have written the chunk into the granted \
             index, got {held:?}"
        );

        let (status, answered) = retrieve(
            &deployment.router,
            &knowledge_base_id,
            "which bay holds the spare waveguides",
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the retrieval over the granted index to be served, got \
             {status} with {answered}"
        );
        let results = at(&answered, "retrievalResults")
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            results.len(),
            1,
            "case {case:?}: expected the one ingested chunk reported, got {answered}"
        );
        assert_eq!(
            text(&results[0], "content.text"),
            row.document,
            "case {case:?}: expected the bytes the ingestion took in, got {answered}"
        );
        assert_eq!(
            text(&results[0], "location.customDocumentLocation.id"),
            "manifest",
            "case {case:?}: expected the retrieval to cite the document it was ingested as, got \
             {answered}"
        );
    }
}

/// RULE, the ungranted side: where those policies do NOT grant the role the index permissions
/// a knowledge base performs — every one of [`INDEX_PERMISSIONS`], which Amazon Bedrock
/// resolves when the knowledge base is CREATED — the deployment fails at
/// `CreateKnowledgeBase` with the modelled `ValidationException` (400) carrying the
/// dependency's own 403 authorization detail, and nothing is written, so there is no knowledge
/// base for a retrieval to fail at later.
///
/// The rows span the ways a grant falls short: no policy at all, one about another principal,
/// one about the collection rather than its indexes, one about another index of the same
/// collection, and ones granting fewer permissions than a knowledge base performs.
#[tokio::test]
async fn an_ungranted_role_fails_the_deployment_at_create_knowledge_base() {
    /// One case: the policy the deployment declares, `None` for a stack that declares none.
    struct Row {
        case: &'static str,
        policy: Option<fn(&str, &str) -> Value>,
    }
    let rows: &[Row] = &[
        Row {
            case: "no data access policy at all",
            policy: None,
        },
        Row {
            case: "a policy granting another principal the index",
            policy: Some(|collection, _| {
                data_policy(
                    vec![rule("index", format!("index/{collection}/*"), &["aoss:*"])],
                    OTHER_ROLE,
                )
            }),
        },
        Row {
            case: "a policy granting the role the collection but not its indexes",
            policy: Some(|collection, _| {
                data_policy(
                    vec![rule(
                        "collection",
                        format!("collection/{collection}"),
                        &["aoss:*"],
                    )],
                    KB_ROLE,
                )
            }),
        },
        Row {
            case: "a policy granting the role another index of the same collection",
            policy: Some(|collection, index| {
                data_policy(
                    vec![rule(
                        "index",
                        format!("index/{collection}/other-{index}"),
                        &["aoss:*"],
                    )],
                    KB_ROLE,
                )
            }),
        },
        Row {
            case: "a policy granting the role the index reads but not the write",
            policy: Some(|collection, _| {
                data_policy(
                    vec![rule(
                        "index",
                        format!("index/{collection}/*"),
                        &["aoss:DescribeIndex", "aoss:ReadDocument"],
                    )],
                    KB_ROLE,
                )
            }),
        },
        Row {
            case: "a policy granting the role the document write alone",
            policy: Some(|collection, _| {
                data_policy(
                    vec![rule(
                        "index",
                        format!("index/{collection}/*"),
                        &["aoss:WriteDocument"],
                    )],
                    KB_ROLE,
                )
            }),
        },
    ];
    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let collection = format!("kb-ungrant-{index}");
        let vector_index_name = format!("kb-ungrant-index-{index}");
        let deployment = Deployment::put_up(
            &collection,
            &vector_index_name,
            row.policy
                .map(|policy| policy(&collection, &vector_index_name)),
        )
        .await;

        let name = format!("kb-ungranted-{index}");
        let (status, refused) = deployment.create_knowledge_base(&name).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected CreateKnowledgeBase refused, got {status} with {refused}"
        );
        assert!(
            text(&refused, "__type").contains("ValidationException"),
            "case {case:?}: expected the modelled ValidationException, got {refused}"
        );
        assert_eq!(
            text(&refused, "message"),
            UNREACHABLE_STORAGE,
            "case {case:?}: expected the refusal Amazon Bedrock words for a storage \
             configuration its role cannot reach, got {refused}"
        );
        // Nothing was written, so the failure cannot resurface as a retrieval of a knowledge
        // base nobody can reach.
        let held = deployment.knowledge_base_names().await;
        assert!(
            !held.contains(&name),
            "case {case:?}: expected the refused knowledge base absent, got {held:?}"
        );
        assert_eq!(
            at(&refused, "knowledgeBase"),
            Value::Null,
            "case {case:?}: expected a refusal to mint no knowledge base, got {refused}"
        );
    }
}

/// RULE, what a retrieval of an unresolvable knowledge base is: the modelled
/// `ResourceNotFoundException` (404) — the identifier names nothing — and never the dependency
/// failure a store that could not be queried answers. The two sides above are therefore told
/// apart by the answers themselves and not only by which call failed.
#[tokio::test]
async fn a_retrieval_naming_no_knowledge_base_is_not_a_dependency_failure() {
    let case = "a knowledgeBaseId nothing minted";
    let router = build_router();
    // A bare identifier of the modelled `KnowledgeBaseId` form that no create of this engine
    // ever minted.
    let absent = "ABCDEFGHIJ";
    let (status, answered) =
        retrieve(&router, absent, "which bay holds the spare waveguides").await;
    assert_eq!(
        status, 404,
        "case {case:?}: expected the modelled 404, got {status} with {answered}"
    );
    assert!(
        text(&answered, "__type").contains("ResourceNotFoundException"),
        "case {case:?}: expected ResourceNotFoundException, got {answered}"
    );
}

/// RULE, the store type on its own: an `OPENSEARCH_SERVERLESS` knowledge base whose granted
/// index holds nothing yet retrieves NOTHING with 200 — "nothing has been ingested" is an
/// empty success, and the supported store type is never reported as a failed dependency.
#[tokio::test]
async fn a_granted_knowledge_base_that_has_ingested_nothing_retrieves_an_empty_page() {
    let case = "the granted index holds no chunk yet";
    let collection = "kb-grant-empty";
    let vector_index_name = "kb-grant-empty-index";
    let deployment = Deployment::put_up(
        collection,
        vector_index_name,
        Some(data_policy(
            vec![rule(
                "index",
                format!("index/{collection}/*"),
                &INDEX_PERMISSIONS,
            )],
            KB_ROLE,
        )),
    )
    .await;
    let (status, created) = deployment.create_knowledge_base("kb-granted-empty").await;
    assert_eq!(
        status, 202,
        "case {case:?}: expected CreateKnowledgeBase accepted, got {status} with {created}"
    );
    let knowledge_base_id = text(&created, "knowledgeBase.knowledgeBaseId");
    await_active_knowledge_base(&deployment.router, case, &knowledge_base_id).await;

    let (status, answered) = retrieve(
        &deployment.router,
        &knowledge_base_id,
        "which bay holds the spare waveguides",
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the retrieval served, got {status} with {answered}"
    );
    let results = at(&answered, "retrievalResults")
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        results.is_empty(),
        "case {case:?}: expected no result from an index holding no chunk, got {answered}"
    );
}
