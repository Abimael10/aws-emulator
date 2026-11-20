// SPDX-License-Identifier: CC0-1.0

//! The address an OpenSearch Serverless collection PUBLISHES is a property of the
//! collection, not of the call that read it — so it survives the control-plane writes a
//! client makes while it builds a retrieval-augmented stack on top of it.
//!
//! RULE: every endpoint a collection publishes — `CollectionDetail$collectionEndpoint`, the
//! "Collection-specific endpoint used to submit index, search, and data upload requests to an
//! OpenSearch Serverless collection", its Dashboards endpoint and the FIPS pair — names a
//! `host:port` THIS emulator answers that collection's OpenSearch REST data plane on, never
//! one of AWS's own `<id>.<region>.aoss.amazonaws.com` hosts, which resolve nowhere on the
//! machine the emulator runs on; and because the address belongs to the collection, every
//! later control-plane read reports the same four values, by whichever key that read names
//! the collection, after every control-plane write the collection takes.
//!
//! Three crates have to agree for the rule to be worth anything — the published address and
//! the REST surface behind it are
//! `service-com-amazonaws-opensearchserverless-opensearchserverless`'s, the knowledge base
//! and the ingestion over that index are the Bedrock Agent build-time plane's, and the
//! retrieval and its refusal are the Bedrock Agent runtime plane's — so the suite lives in
//! the cross-service target and shares its setup with
//! `bedrock_knowledge_base_over_an_aoss_collection.rs`.
//!
//! What the sibling suites pin, and this one does not repeat: the VALUE each published member
//! takes and the REST routes served at it
//! (`services/service-com-amazonaws-opensearchserverless-opensearchserverless/tests/all/\
//! announced_collection_endpoints.rs` and `collection_data_plane.rs`), the same address over
//! a real TLS socket (`aoss_published_collection_endpoint_dialled_by_a_stock_client.rs`), and
//! the retrieval and the not-found refusal over a freshly built stack
//! (`bedrock_retrieve_over_a_published_collection_endpoint_index.rs`). Here the stack is
//! built ONCE and then written to: the rows differ along the control-plane write that lands
//! between the publish and the retrieval — nothing at all, an update of the record's own
//! metadata, an update of `vectorOptions`, which reconfigures the collection and moves it
//! through a non-terminal status before the clock settles it back, a tag written, a tag
//! removed, and a duplicate create the service refuses — and each row re-reads the collection
//! BOTH ways `BatchGetCollection` admits, by `ids` and by `names`.
//!
//! Every wait is a poll for the CONDITION, bounded by how many times it LOOKS; nothing here
//! asserts how long a step took.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::bedrock_knowledge_base_over_an_aoss_collection::{
    active_collection, addressed, admit_network_access, aoss, at, await_active_knowledge_base,
    await_indexed_document, bedrock, build_router, encrypt_collections, grant_the_role_data_access,
    inline_byte_document, knn_index_body, opensearch_storage, rest, text, KB_ROLE, REGION,
};

/// The embedding model the knowledge base of this suite names, and how many values its
/// vectors carry — `amazon.titan-embed-text-v2:0` publishes 1024-value embeddings, so the
/// index is created with that dimension.
const MODEL: &str = "amazon.titan-embed-text-v2:0";
const DIMENSION: usize = 1024;

/// The settled status a collection reports (`CollectionStatus`): the one it reaches on its
/// own after a create, and the one it returns to after a reconfiguring update.
const ACTIVE: &str = "ACTIVE";

/// The error `Retrieve` declares for a knowledge base that is not there
/// (`smithy_query(query="operation",operation="Retrieve")` -> `ResourceNotFoundException`,
/// `httpError` 404).
const NOT_FOUND: &str = "ResourceNotFoundException";

/// The status this `awsJson1_0` service answers a modelled client error with. Which status
/// that class takes is the reading of
/// `services/service-com-amazonaws-opensearchserverless-opensearchserverless/tests/all/\
/// client_error_status.rs`, not of this suite, which needs only that the write was refused.
const CLIENT_ERROR: u16 = 400;

/// The error a create of a name the account already holds earns
/// (`smithy_query(query="operation",operation="CreateCollection")` -> `ConflictException`).
const CONFLICT: &str = "ConflictException";

/// A `KnowledgeBaseIdentifier` of the modelled bare form `^[0-9a-zA-Z]{10}$` that nothing
/// creates here, so every row can check that the refusal leg is still refused while the
/// served one is still served.
const UNHELD_KNOWLEDGE_BASE: &str = "0OpenSrch0";

/// How many times a read looks again for a transition the clock owes. Generous for the few
/// seconds such a transition is owed in — a host running every suite of this workspace at
/// once is far slower than one running this file alone — while one look is 25 ms, so a
/// transition that has fallen due is seen at once. Bounded rather than open-ended so a
/// genuine regression fails the suite promptly instead of sitting in a poll loop: the whole
/// run takes about three seconds.
const SETTLE_ATTEMPTS: usize = 2_400;

/// How long one look waits before the next.
const SETTLE_PAUSE: ::std::time::Duration = ::std::time::Duration::from_millis(25);

/// The collection every row writes to: the three ways the control plane names it.
struct Collection {
    id: String,
    name: String,
    arn: String,
}

/// The control-plane write that lands between the collection publishing its address and the
/// retrieval that depends on it.
#[derive(Debug, Clone, Copy)]
enum Mutation {
    /// Nothing at all: the baseline reading, taken the same way every later row is.
    Nothing,
    /// `UpdateCollection` of members of the stored record alone, which is complete when the
    /// call answers and leaves the collection in the settled status it was in.
    Metadata {
        description: &'static str,
        deletion_protection: &'static str,
    },
    /// `UpdateCollection` of `vectorOptions`, the member that "specifies whether serverless
    /// vector acceleration is enabled for the collection" — work the service has to do, so
    /// the collection reports a non-terminal status until the clock makes it due.
    Reconfigure { acceleration: &'static str },
    /// `TagResource` on the collection's ARN.
    Tag {
        key: &'static str,
        value: &'static str,
    },
    /// `UntagResource` of that same key.
    Untag { key: &'static str },
    /// A second `CreateCollection` of a name this account already holds, which the service
    /// refuses with `ConflictException` and which therefore changes nothing.
    RefusedDuplicateCreate,
}

/// One case: the write that lands between the publish and the reads below it.
struct Row {
    case: &'static str,
    mutation: Mutation,
}

/// Make `mutation` against `collection`, asserting the status the control plane answers it
/// with — a write the service refuses is a row too, and has to be seen to be refused.
async fn apply(
    router: &MultiServiceRouter,
    case: &str,
    mutation: Mutation,
    collection: &Collection,
) {
    let (operation, body, expected) = match mutation {
        Mutation::Nothing => return,
        Mutation::Metadata {
            description,
            deletion_protection,
        } => (
            "UpdateCollection",
            json!({
                "id": &collection.id,
                "description": description,
                "deletionProtection": deletion_protection,
            }),
            200,
        ),
        Mutation::Reconfigure { acceleration } => (
            "UpdateCollection",
            json!({
                "id": &collection.id,
                "vectorOptions": { "ServerlessVectorAcceleration": acceleration },
            }),
            200,
        ),
        Mutation::Tag { key, value } => (
            "TagResource",
            json!({
                "resourceArn": &collection.arn,
                "tags": [{ "key": key, "value": value }],
            }),
            200,
        ),
        Mutation::Untag { key } => (
            "UntagResource",
            json!({ "resourceArn": &collection.arn, "tagKeys": [key] }),
            200,
        ),
        Mutation::RefusedDuplicateCreate => (
            "CreateCollection",
            json!({ "name": &collection.name, "type": "VECTORSEARCH" }),
            CLIENT_ERROR,
        ),
    };
    let (status, answer) = aoss(router, operation, body).await;
    assert_eq!(
        status, expected,
        "case {case:?}: expected {operation} to answer {expected}, got {status} with {answer}"
    );
    // A refused write is a row too, and has to be seen to have been REFUSED rather than to
    // have quietly created a second collection: the body names the error's own shape.
    if matches!(mutation, Mutation::RefusedDuplicateCreate) {
        assert_eq!(
            error_code(&answer),
            CONFLICT,
            "case {case:?}: expected the duplicate create to be refused {CONFLICT}, got {answer}"
        );
    }
}

/// The four endpoints a collection publishes, in the order `CollectionDetail` carries them.
fn published(detail: &Value) -> Vec<(&'static str, String)> {
    vec![
        ("collectionEndpoint", text(detail, "collectionEndpoint")),
        ("dashboardEndpoint", text(detail, "dashboardEndpoint")),
        (
            "fipsEndpoints.collectionEndpoint",
            text(detail, "fipsEndpoints.collectionEndpoint"),
        ),
        (
            "fipsEndpoints.dashboardEndpoint",
            text(detail, "fipsEndpoints.dashboardEndpoint"),
        ),
    ]
}

/// Assert that every endpoint `detail` publishes is an address THIS emulator serves on — an
/// HTTPS address naming no AWS host and carrying the port it is served on — and answer the
/// four values, so a later read can be held against them.
///
/// How the four relate to one another is `services/service-com-amazonaws-opensearchserverless\
/// -opensearchserverless/tests/all/announced_collection_endpoints.rs`'s reading, not this
/// suite's: what is read here is that each one is an address a client reaches and that it
/// stays the collection's across every write below.
fn assert_served_addresses(case: &str, detail: &Value) -> Vec<(&'static str, String)> {
    let reported = published(detail);
    for (member, url) in &reported {
        assert!(
            url.starts_with("https://"),
            "case {case:?}: expected {member} to be an HTTPS address, got {url:?}"
        );
        // AWS's own `<id>.<region>.aoss[-fips].amazonaws.com` hosts resolve nowhere on the
        // machine this emulator runs on, so a published address may not name one: a client
        // that only has this value could send nothing at all.
        assert!(
            !url.contains("amazonaws.com"),
            "case {case:?}: expected {member} to name an address this emulator serves rather \
             than an AWS host, got {url:?}"
        );
        // The authority the endpoint addresses, read the way every REST call of this suite
        // addresses it (`addressed`), which is what makes the port part of what is published.
        let (authority, _path) = addressed(url);
        assert!(
            authority
                .rsplit(':')
                .next()
                .is_some_and(|port| port.parse::<u16>().is_ok_and(|port| port != 0)),
            "case {case:?}: expected {member} to carry the port it is served on, got {url:?}"
        );
    }
    reported
}

/// Read the collection `BatchGetCollection` reports for `selector` (`ids` or `names`) naming
/// `value`, looking again until it reports ACTIVE.
///
/// A reconfiguring update leaves the collection in a non-terminal status it settles out of on
/// its own, with nobody calling, so this polls for that CONDITION.
async fn await_active_collection(
    router: &MultiServiceRouter,
    case: &str,
    selector: &str,
    value: &str,
) -> Value {
    let body = json!({ selector: [value] });
    for _ in 0..SETTLE_ATTEMPTS {
        let (status, read) = aoss(router, "BatchGetCollection", body.clone()).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected BatchGetCollection by {selector} to answer, got {status} \
             with {read}"
        );
        let detail = at(&read, "collectionDetails")
            .as_array()
            .and_then(|details| details.first())
            .cloned()
            .unwrap_or(Value::Null);
        if text(&detail, "status") == ACTIVE {
            return detail;
        }
        ::tokio::time::sleep(SETTLE_PAUSE).await;
    }
    panic!("case {case:?}: the collection named by {selector} {value:?} never reported {ACTIVE}");
}

/// One `Retrieve` call, as the operation's `@http` binding places it:
/// `POST /knowledgebases/{knowledgeBaseId}/retrieve`.
async fn retrieve(router: &MultiServiceRouter, identifier: &str, query: &str) -> (u16, Value) {
    bedrock(
        router,
        "POST",
        &format!("/knowledgebases/{identifier}/retrieve"),
        Some(&json!({ "retrievalQuery": { "text": query } })),
    )
    .await
}

/// The wire error code a JSON protocol of this workspace puts in `__type`, without the
/// shape's namespace — the `restJson1` Bedrock refusals and the `awsJson1_0` OpenSearch
/// Serverless ones alike.
fn error_code(body: &Value) -> String {
    text(body, "__type")
        .rsplit('#')
        .next()
        .unwrap_or_default()
        .to_string()
}

/// RULE: a collection's published endpoints name an address this emulator serves the
/// collection's OpenSearch REST data plane on, and they belong to the COLLECTION — so every
/// control-plane read reports the same four values whichever key it names the collection by
/// and whatever control-plane write has landed since, the index created at that address keeps
/// backing the knowledge base built over it, and `Retrieve` keeps being served there while an
/// identifier no knowledge base was created for keeps being refused.
#[tokio::test]
async fn the_published_collection_endpoint_outlives_every_control_plane_write() {
    const COLLECTION: &str = "mutated-collection";
    const INDEX: &str = "bedrock-knowledge-base-mutated";
    const KNOWLEDGE_BASE: &str = "kb-mutated";
    const DOCUMENT: &str = "doc-mutated";
    const BYTES: &str = "escalation policy: page the on-call engineer within ten minutes";

    let router = build_router();
    encrypt_collections(&router).await;
    admit_network_access(&router).await;
    grant_the_role_data_access(&router, COLLECTION).await;

    let detail = active_collection(&router, COLLECTION).await;
    let collection = Collection {
        id: text(&detail, "id"),
        name: COLLECTION.to_string(),
        arn: text(&detail, "arn"),
    };
    let baseline = assert_served_addresses("the collection as its create published it", &detail);
    let endpoint = text(&detail, "collectionEndpoint");

    // The vector index a knowledge base's `storageConfiguration` requires, created at the
    // address the collection published, exactly as a client of that address sends it.
    let (status, created) = rest(
        &router,
        &endpoint,
        "PUT",
        &format!("/{INDEX}"),
        Some(&knn_index_body(DIMENSION)),
    )
    .await;
    assert_eq!(
        (status, at(&created, "acknowledged")),
        (200, json!(true)),
        "setup: expected the index create at {endpoint} to be served, got {status} with {created}"
    );

    // ...the knowledge base over it, a custom data source, and one document pushed through
    // that data source, so the retrieval below reports a chunk an ingestion really recorded.
    let (status, knowledge_base) = bedrock(
        &router,
        "PUT",
        "/knowledgebases/",
        Some(&json!({
            "name": KNOWLEDGE_BASE,
            "roleArn": KB_ROLE,
            "knowledgeBaseConfiguration": {
                "type": "VECTOR",
                "vectorKnowledgeBaseConfiguration": {
                    "embeddingModelArn":
                        format!("arn:aws:bedrock:{REGION}::foundation-model/{MODEL}"),
                },
            },
            "storageConfiguration": opensearch_storage(&collection.arn, INDEX),
        })),
    )
    .await;
    assert_eq!(
        status, 202,
        "setup: expected CreateKnowledgeBase over the published index to be accepted, got \
         {status} with {knowledge_base}"
    );
    let knowledge_base_id = text(&knowledge_base, "knowledgeBase.knowledgeBaseId");
    await_active_knowledge_base(&router, "setup", &knowledge_base_id).await;

    let (status, source) = bedrock(
        &router,
        "PUT",
        &format!("/knowledgebases/{knowledge_base_id}/datasources/"),
        Some(&json!({
            "name": format!("{KNOWLEDGE_BASE}-pushed"),
            "dataSourceConfiguration": { "type": "CUSTOM" },
        })),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: expected CreateDataSource(CUSTOM) accepted, got {status} with {source}"
    );
    let data_source_id = text(&source, "dataSource.dataSourceId");
    let (status, ingested) = bedrock(
        &router,
        "PUT",
        &format!("/knowledgebases/{knowledge_base_id}/datasources/{data_source_id}/documents"),
        Some(&json!({ "documents": [inline_byte_document(DOCUMENT, BYTES)] })),
    )
    .await;
    assert_eq!(
        status, 202,
        "setup: expected the ingestion accepted, got {status} with {ingested}"
    );
    await_indexed_document(
        &router,
        "setup",
        &knowledge_base_id,
        &data_source_id,
        DOCUMENT,
    )
    .await;

    let rows: &[Row] = &[
        Row {
            case: "no write at all",
            mutation: Mutation::Nothing,
        },
        Row {
            case: "an update of the record's own metadata",
            mutation: Mutation::Metadata {
                description: "the vector store of the escalation knowledge base",
                deletion_protection: "ENABLED",
            },
        },
        Row {
            case: "an update of vectorOptions, which reconfigures the collection",
            mutation: Mutation::Reconfigure {
                acceleration: "ENABLED",
            },
        },
        Row {
            case: "a tag written on the collection",
            mutation: Mutation::Tag {
                key: "owner",
                value: "search-platform",
            },
        },
        Row {
            case: "that tag removed again",
            mutation: Mutation::Untag { key: "owner" },
        },
        Row {
            case: "a duplicate create of the same name, which the service refuses",
            mutation: Mutation::RefusedDuplicateCreate,
        },
        Row {
            case: "a second reconfiguring update, which disables the acceleration again",
            mutation: Mutation::Reconfigure {
                acceleration: "DISABLED",
            },
        },
    ];

    for Row { case, mutation } in rows {
        apply(&router, case, *mutation, &collection).await;

        // The collection settles back to ACTIVE on its own where the write reconfigured it,
        // and every read of it — by either key `BatchGetCollection` admits — reports the very
        // addresses the publish wrote.
        for (selector, value) in [("ids", &collection.id), ("names", &collection.name)] {
            let case = format!("{case}, read by {selector}");
            let read = await_active_collection(&router, &case, selector, value).await;
            let reported = assert_served_addresses(&case, &read);
            assert_eq!(
                reported, baseline,
                "case {case:?}: expected the published endpoints {baseline:?}, got {reported:?}"
            );
        }

        // ...the address still serves THIS collection's index, so a client that cached the
        // published value reaches the same data plane it created the index at.
        let (status, read) = rest(&router, &endpoint, "GET", &format!("/{INDEX}"), None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected GET /{INDEX} at {endpoint} to be served, got {status} \
             with {read}"
        );
        assert!(
            read.get(INDEX).is_some(),
            "case {case:?}: expected {endpoint} to report the index of the collection it \
             names, got {read}"
        );

        // ...and the knowledge base built on that index is still retrievable, citing the
        // chunk the ingestion recorded.
        let (status, answer) = retrieve(&router, &knowledge_base_id, BYTES).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the retrieval from {knowledge_base_id} to be served, got \
             {status} with {answer}"
        );
        let results = at(&answer, "retrievalResults")
            .as_array()
            .cloned()
            .unwrap_or_default();
        let cited = results
            .iter()
            .find(|result| text(result, "location.customDocumentLocation.id") == DOCUMENT)
            .cloned()
            .unwrap_or_else(|| {
                panic!("case {case:?}: expected the retrieval to cite {DOCUMENT:?}, got {answer}")
            });
        assert_eq!(
            text(&cited, "content.text"),
            BYTES,
            "case {case:?}: expected the cited content to be the bytes ingested, got {cited}"
        );
        let score = at(&cited, "score").as_f64().unwrap_or(f64::NAN);
        assert!(
            (0.0..=1.0).contains(&score),
            "case {case:?}: expected a relevance score in [0, 1], got {score} in {cited}"
        );

        // ...while an identifier this plane holds no knowledge base for is refused, so the
        // served leg above is not a blanket answer.
        let (status, refused) = retrieve(&router, UNHELD_KNOWLEDGE_BASE, BYTES).await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected 404 for {UNHELD_KNOWLEDGE_BASE}, got {status} with {refused}"
        );
        assert_eq!(
            error_code(&refused),
            NOT_FOUND,
            "case {case:?}: expected the modelled not-found error for \
             {UNHELD_KNOWLEDGE_BASE}, got {refused}"
        );
        assert_eq!(
            text(&refused, "message"),
            format!("Knowledge Base with id {UNHELD_KNOWLEDGE_BASE} does not exist"),
            "case {case:?}: expected the refusal to name the identifier the request carried, \
             got {refused}"
        );
    }
}
