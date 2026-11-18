// SPDX-License-Identifier: CC0-1.0

//! The OpenSearch Serverless vector store a knowledge base is bound to, resolved in the
//! service that OWNS that collection.
//!
//! `CreateKnowledgeBase` documents the vector store as something the caller sets up first
//! ("To create a knowledge base, you must first set up your data sources and configure a
//! supported vector store"), and for an `OPENSEARCH_SERVERLESS` storage configuration what
//! it sets up is a collection, a vector index inside it, and the data access policy that
//! lets the knowledge base's own role reach that index. Amazon Bedrock resolves all three
//! when the knowledge base is created — it reads and writes the index as the role the
//! request names — so a binding its role cannot reach is refused there and then, before any
//! knowledge base is written.
//!
//! The decision is NOT taken here: who may act on a collection's data is the owning
//! service's question and its own state answers it
//! (`::service_com_amazonaws_opensearchserverless_opensearchserverless::state::data_access`),
//! over the very policies `aoss:CreateAccessPolicy` wrote. What lives here is which
//! resource and which permissions a knowledge base takes on its index, the sentence this
//! service refuses an unreachable binding with, and what an ingestion WRITES into that
//! index — and reads back out of it — because a knowledge base bound to a collection keeps
//! its chunks there, so that collection's own search, count and document read are what answer
//! for them, and a chunk that collection no longer holds is a chunk the knowledge base no
//! longer holds.
//!
//! An ARN naming a collection that service holds no record of is not judged: nothing stored
//! says what its policies grant, so such a binding is not refused here — the same reading
//! [`crate::state::service_role`] gives a role the IAM store does not hold.
//!
//! LOCK ORDER: as [`crate::state::vector_store`] states — every function here takes the
//! owning service's lock and holds no other while it does, so each must be called with NO
//! guard of this service's store held.

use super::*;

/// The `KnowledgeBaseStorageType` of a knowledge base whose vectors live in an Amazon
/// OpenSearch Serverless collection
/// (`smithy_query(query="shape",shape="KnowledgeBaseStorageType")`).
pub const OPENSEARCH_SERVERLESS_STORAGE_TYPE: &str = "OPENSEARCH_SERVERLESS";

/// The permissions a knowledge base performs on the vector index it is bound to, which are
/// the ones its role must be granted for the binding to work: it reads the index's own
/// definition to check the field mapping, writes a document per ingested chunk, and reads
/// them back to serve a retrieval.
///
/// Source: the index-level permissions AWS's "Create a vector index in Amazon OpenSearch
/// Service" states for the knowledge-base role's data access policy, narrowed to the ones
/// this service's own operations perform.
const REQUIRED_INDEX_PERMISSIONS: [&str; 3] = [
    ::service_com_amazonaws_opensearchserverless_opensearchserverless::state::data_access::permission::DESCRIBE_INDEX,
    ::service_com_amazonaws_opensearchserverless_opensearchserverless::state::data_access::permission::READ_DOCUMENT,
    ::service_com_amazonaws_opensearchserverless_opensearchserverless::state::data_access::permission::WRITE_DOCUMENT,
];

/// The collection, the index and the FIELDS an `OPENSEARCH_SERVERLESS` storage
/// configuration binds a knowledge base to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenSearchServerlessIndex {
    /// The collection's ARN, as the request spells it.
    pub collection_arn: String,
    /// The vector index's name within that collection.
    pub index: String,
    /// "The name of the field in which Amazon Bedrock stores the vector embeddings for your
    /// data sources"
    /// (`smithy_query(query="shape",shape="OpenSearchServerlessFieldMapping")`).
    pub vector_field: String,
    /// "The name of the field in which Amazon Bedrock stores the raw text from your data",
    /// as that same mapping names it.
    pub text_field: String,
    /// "The name of the field in which Amazon Bedrock stores metadata about the vector
    /// store", as that same mapping names it.
    pub metadata_field: String,
}

/// The binding `storage_configuration` names, or `None` when it names no OpenSearch
/// Serverless index at all.
pub fn opensearch_serverless_index(
    storage_configuration: Option<&StorageConfiguration>,
) -> Option<OpenSearchServerlessIndex> {
    let configuration = storage_configuration
        .filter(|storage| storage.r#type == OPENSEARCH_SERVERLESS_STORAGE_TYPE)?
        .opensearch_serverless_configuration
        .as_ref()?;
    Some(OpenSearchServerlessIndex {
        collection_arn: configuration.collection_arn.clone(),
        index: configuration.vector_index_name.clone(),
        vector_field: configuration.field_mapping.vector_field.clone(),
        text_field: configuration.field_mapping.text_field.clone(),
        metadata_field: configuration.field_mapping.metadata_field.clone(),
    })
}

/// `None` when nothing stored says the role `role_arn` cannot reach the index
/// `storage_configuration` names; otherwise the `ValidationException` message that refuses
/// the call.
///
/// The collection is resolved by the ARN the request carries, in the owning service's own
/// store, and the data access policies of that service decide whether the role is granted
/// the permissions a knowledge base performs on the index ([`REQUIRED_INDEX_PERMISSIONS`]).
/// A role granted none of them cannot read or write the vectors the knowledge base is for,
/// which is what makes the storage configuration invalid.
///
/// LOCK ORDER: as the module doc says — call with NO guard of this service's store held.
pub fn opensearch_serverless_access_problem<Eng: Send + Sync + 'static>(
    engine: &Eng,
    role_arn: &str,
    storage_configuration: Option<&StorageConfiguration>,
) -> Option<String> {
    use ::service_com_amazonaws_opensearchserverless_opensearchserverless::state as collections;

    let binding = opensearch_serverless_index(storage_configuration)?;
    let state = collections::service_state(engine)?;
    let store = state.lock();
    // A collection this emulator holds no record of says nothing about what its policies
    // grant, so the binding is not judged on one.
    let collection = store.collection_by_arn(&binding.collection_arn)?;
    let resource = collections::data_access::index_resource(&collection.name, &binding.index);
    let principals = [role_arn.to_string()];
    let granted = REQUIRED_INDEX_PERMISSIONS
        .iter()
        .all(|permission| store.data_access_grants(&principals, &resource, permission));
    (!granted).then(|| STORAGE_CONFIGURATION_UNREACHABLE.to_string())
}

/// The `ValidationException` message Amazon Bedrock answers when the vector store refuses
/// the knowledge base's own role.
///
/// Fixed text for this error class: this service's own opening for an invalid storage
/// configuration, and the dependency's refusal — the status and the message the vector store
/// answered the authorization failure with — neither of which is taken from the request.
pub const STORAGE_CONFIGURATION_UNREACHABLE: &str =
    "The knowledge base storage configuration provided is invalid... Dependency error \
     document status code: 403, error message: Bad Authorization";

/// How many values a chunk's vector may carry for the bound index to be asked to hold it:
/// the maximum of the modelled `range 0..4096` a knowledge base's own
/// `BedrockEmbeddingModelConfiguration$dimensions` carries
/// (`smithy_query(query="shape",shape="BedrockEmbeddingModelConfiguration")`).
///
/// The dimension this service writes in is read off the BOUND INDEX's own mapping, which is
/// a value a caller wrote into that index with no modelled bound on it — so a declared
/// dimension past what a knowledge base's embedding model can produce is not one this
/// service writes in, and the model's own dimension is used instead. That keeps what one
/// chunk allocates bounded by the model rather than by a number in a mapping.
pub const MAX_WRITTEN_VECTOR_VALUES: i64 = 4_096;

/// How many values the vectors of the bound index hold, as that index's own mapping
/// declares for the field the storage configuration maps the embeddings to; `None` when
/// the owning service holds no such collection, index or mapped field, or declares a
/// dimension past [`MAX_WRITTEN_VECTOR_VALUES`].
///
/// LOCK ORDER: as the module doc says — call with NO guard of this service's store held.
pub fn opensearch_serverless_index_dimension<Eng: Send + Sync + 'static>(
    engine: &Eng,
    index: &OpenSearchServerlessIndex,
) -> Option<i32> {
    use ::service_com_amazonaws_opensearchserverless_opensearchserverless::state as collections;

    let state = collections::service_state(engine)?;
    let store = state.lock();
    declared_vector_dimension(
        store
            .collection_by_arn(&index.collection_arn)?
            .indexes
            .get(&index.index)?,
        &index.vector_field,
    )
}

/// How many values the `knn_vector` field `field` of the index `held` declares, within the
/// bound [`MAX_WRITTEN_VECTOR_VALUES`] states; `None` when it maps no such field or declares
/// no usable dimension for it.
fn declared_vector_dimension(
    held: &::service_com_amazonaws_opensearchserverless_opensearchserverless::state::IndexRecord,
    field: &str,
) -> Option<i32> {
    use ::service_com_amazonaws_opensearchserverless_opensearchserverless::state as collections;

    let declared = collections::index_documents::knn_vector_dimension(held, field)?;
    (1..=MAX_WRITTEN_VECTOR_VALUES)
        .contains(&declared)
        .then(|| i32::try_from(declared).ok())
        .flatten()
}

/// How far each chunk the index `index` names holds is from `query`'s own vector.
///
/// A collection the owning service holds no record of is [`ChunkRanking::Elsewhere`]: an
/// ingestion writes through to a collection only where that service holds one, so a knowledge
/// base naming a collection outside this emulator is in exactly the position of one naming a
/// store no service here serves — nothing here wrote its index. A collection that IS held but
/// cannot answer for the index — it no longer holds it, or it is not `ACTIVE`, so there is no
/// cluster behind its endpoint and its own data plane answers nothing either — is the
/// dependency the chunks lived in, gone. The transitions the clock has already made due are
/// applied first, exactly as the write does ([`write_opensearch_serverless_chunks`]), so the
/// read and the write cannot disagree about whether the collection is still there.
///
/// The distances are measured under the metric the knowledge base's own chunk vectors are
/// compared by, which is the one the ingestion record ranks the chunks of ANY knowledge base
/// of this emulator under ([`crate::state::ingestion_record`]): one notion of relevance
/// across the stores, so a retrieval's ordering does not change with where the chunks live.
/// It is deliberately NOT the `space_type` the bound index's own mapping declares: that is
/// what a client's own `knn` query at the collection endpoint is scored under, while how
/// Amazon Bedrock ranks the chunks it holds for a knowledge base is not published — so one
/// stated metric is used rather than a conversion nothing documents.
///
/// ONE pass of that metric over the documents the index holds, under one hold of the owning
/// store, bounded by how many documents one index holds and by nothing a request carries.
///
/// LOCK ORDER: as the module doc says — call with NO guard of this service's store held.
pub fn ranked_opensearch_serverless_chunks<Eng: Send + Sync + 'static>(
    engine: &Eng,
    index: &OpenSearchServerlessIndex,
    query: &str,
    fallback_dimension: i32,
) -> ChunkRanking {
    use ::service_com_amazonaws_opensearchserverless_opensearchserverless::state as collections;
    use ::service_com_amazonaws_s3vectors_s3vectors::state as vectors;

    let Some(state) = collections::service_state(engine) else {
        return ChunkRanking::Elsewhere;
    };
    let mut store = state.lock();
    store.settle_due_transitions();
    let Some(collection) = store.collection_by_arn(&index.collection_arn) else {
        return ChunkRanking::Elsewhere;
    };
    if collection.status != collections::COLLECTION_ACTIVE {
        return ChunkRanking::IndexGone;
    }
    let Some(held) = collection.indexes.get(&index.index) else {
        return ChunkRanking::IndexGone;
    };
    // The dimension the ingestion wrote its chunks in: the index's own declared one, and the
    // knowledge base's where the index declares none.
    let dimension =
        declared_vector_dimension(held, &index.vector_field).unwrap_or(fallback_dimension);
    let query_vector = document_vector(query, dimension);
    // In the index's own document order (its documents are keyed), so chunks at the same
    // distance from the query are always ranked in the same order.
    let ranked: Vec<(f32, String)> = held
        .documents
        .iter()
        .filter_map(|(id, document)| {
            let stored = stored_vector(&document.source, &index.vector_field)?;
            Some((
                vectors::vector_distance(
                    vectors::DISTANCE_METRIC_COSINE,
                    &query_vector,
                    &stored,
                ),
                id.clone(),
            ))
        })
        .collect();
    ChunkRanking::Held {
        metric: vectors::DISTANCE_METRIC_COSINE.to_string(),
        ranked,
    }
}

/// The fields the chunk held under `key` in the index `index` names carries: the published
/// knowledge-base fields the ingestion wrote beside the vector, with the chunk's text read
/// off the field the storage configuration maps it to.
///
/// The inverse of [`stored_document`]: the three fields that configuration MAPS are the
/// index's own rendering of the chunk — the embedding, the raw text and the metadata
/// document — so what is reported is the fields recorded ABOUT the chunk, and the text is
/// taken from the mapped text field. `None` for a document carrying no text there, which is
/// no chunk of this knowledge base.
///
/// LOCK ORDER: as the module doc says — call with NO guard of this service's store held.
pub fn opensearch_serverless_chunk_fields<Eng: Send + Sync + 'static>(
    engine: &Eng,
    index: &OpenSearchServerlessIndex,
    key: &str,
) -> Option<Document> {
    use ::service_com_amazonaws_opensearchserverless_opensearchserverless::state as collections;

    let state = collections::service_state(engine)?;
    let store = state.lock();
    let document = store
        .collection_by_arn(&index.collection_arn)?
        .indexes
        .get(&index.index)?
        .documents
        .get(key)?;
    chunk_fields_of(index, &document.source)
}

/// The vector one stored document holds under the field `field`, or `None` when it holds no
/// list of numbers there — a document a caller wrote into the index themselves, which is no
/// chunk of a knowledge base.
fn stored_vector(source: &Document, field: &str) -> Option<Vec<f32>> {
    let Document::Object(members) = source else {
        return None;
    };
    let Document::Array(values) = members.get(field)? else {
        return None;
    };
    values
        .iter()
        .map(|value| match value {
            Document::Number(number) => Some(number.to_f64_lossy() as f32),
            _ => None,
        })
        .collect()
}

/// The fields one stored document records ABOUT its chunk; see
/// [`opensearch_serverless_chunk_fields`], whose reading this is.
fn chunk_fields_of(index: &OpenSearchServerlessIndex, source: &Document) -> Option<Document> {
    let Document::Object(members) = source else {
        return None;
    };
    let text = match members.get(&index.text_field) {
        Some(Document::String(text)) => text.clone(),
        _ => return None,
    };
    let mut fields: HashMap<String, Document> = HashMap::new();
    for (name, value) in members {
        if name == &index.vector_field || name == &index.text_field || name == &index.metadata_field
        {
            continue;
        }
        fields.insert(name.clone(), value.clone());
    }
    fields.insert(
        ingestion_index::CHUNK_TEXT_METADATA_KEY.to_string(),
        Document::String(text),
    );
    Some(Document::Object(fields))
}

/// Write each of `chunks` into the index `index` names, as the role `role_arn`, answering
/// the keys that collection really took — or `None` when this emulator holds no OpenSearch
/// Serverless store at all, so there is nothing to reconcile the ingestion against.
///
/// The role must be granted the index write by the collection's own `data` access policies,
/// which is what the knowledge base performs the write as: a role the policies no longer
/// grant it writes nothing, and the caller accounts for those documents as failed rather
/// than indexed. The decision is the owning service's own
/// ([`data_access_grants`](::service_com_amazonaws_opensearchserverless_opensearchserverless::state::ComAmazonawsOpensearchserverlessOpensearchserverlessState::data_access_grants)),
/// the same one `CreateKnowledgeBase` resolves the binding with. A chunk whose vector does
/// not have as many values as the bound index declares is not taken either — see below.
///
/// LOCK ORDER: as the module doc says.
pub fn write_opensearch_serverless_chunks<Eng: Send + Sync + 'static>(
    engine: &Eng,
    index: &OpenSearchServerlessIndex,
    role_arn: &str,
    chunks: &[WrittenChunk],
) -> Option<Vec<String>> {
    use ::service_com_amazonaws_opensearchserverless_opensearchserverless::state as collections;

    let state = collections::service_state(engine)?;
    // What goes into the collection is assembled with no guard held, so the owning store is
    // held for the writes alone. Each pair carries the number of values its vector holds,
    // which is what the index's own mapping decides whether it can take.
    let written: Vec<(usize, (String, Document))> = chunks
        .iter()
        .map(|chunk| {
            (
                chunk.vector.len(),
                (chunk.key.clone(), stored_document(index, chunk)),
            )
        })
        .collect();
    let mut store = state.lock();
    let Some(collection) = store.collection_by_arn(&index.collection_arn) else {
        // The collection is gone, so nothing holds the chunks.
        return Some(Vec::new());
    };
    let resource = collections::data_access::index_resource(&collection.name, &index.index);
    let principals = [role_arn.to_string()];
    if !store.data_access_grants(
        &principals,
        &resource,
        collections::data_access::permission::WRITE_DOCUMENT,
    ) {
        return Some(Vec::new());
    }
    // The index's own declared dimension decides which chunks it can hold, exactly as the
    // sibling store's own rule does: a vector of another length does not fit the field it
    // would be written into, so the index takes none of that chunk and the caller accounts
    // for its document as failed rather than indexed. An index declaring no dimension for
    // the mapped field constrains nothing, so every chunk is offered to it.
    let declared = store.collection_index_vector_dimension(
        &index.collection_arn,
        &index.index,
        &index.vector_field,
    );
    let documents: Vec<(String, Document)> = written
        .into_iter()
        .filter(|(values, _)| {
            declared.is_none_or(|declared| {
                i64::try_from(*values).is_ok_and(|values| values == declared)
            })
        })
        .map(|(_, document)| document)
        .collect();
    Some(store.write_collection_index_documents(&index.collection_arn, &index.index, documents))
}

/// Take the documents held under `keys` out of the index `index` names, answering how many
/// were there to take.
///
/// LOCK ORDER: as the module doc says.
pub fn remove_opensearch_serverless_chunks<Eng: Send + Sync + 'static>(
    engine: &Eng,
    index: &OpenSearchServerlessIndex,
    keys: &[String],
) -> usize {
    use ::service_com_amazonaws_opensearchserverless_opensearchserverless::state as collections;

    let Some(state) = collections::service_state(engine) else {
        return 0;
    };
    let mut store = state.lock();
    store.remove_collection_index_documents(&index.collection_arn, &index.index, keys)
}

/// The document one chunk is stored as in the bound index: the embedding under the field the
/// storage configuration maps it to, the chunk's raw text under its own field, the metadata
/// about the vector store under the third, and the published knowledge-base metadata fields
/// the chunk carries ([`ingestion_index::chunk_metadata`]) beside them — which is what a
/// client reading that index back finds for the chunk.
///
/// The three mapped field names are the caller's, so a configuration that maps two of them
/// to one name leaves that field holding whichever of the two this order writes last; the
/// index itself is what the caller declared the fields of.
fn stored_document(index: &OpenSearchServerlessIndex, chunk: &WrittenChunk) -> Document {
    let mut members: HashMap<String, Document> = match &chunk.metadata {
        Document::Object(recorded) => recorded.clone(),
        _ => HashMap::new(),
    };
    members.insert(
        index.metadata_field.clone(),
        Document::String(metadata_text(&chunk.metadata)),
    );
    members.insert(
        index.text_field.clone(),
        Document::String(chunk.text.clone()),
    );
    members.insert(
        index.vector_field.clone(),
        Document::Array(
            chunk
                .vector
                .iter()
                .map(|value| Document::Number(::aws_smithy_types::Number::Float(f64::from(*value))))
                .collect(),
        ),
    );
    Document::Object(members)
}

/// The metadata field's own value: the fields recorded with the chunk as one JSON object,
/// which is how a knowledge base records "metadata about the vector store" in a text field
/// of its index.
///
/// Every field a chunk records is text ([`ingestion_index::chunk_metadata`]), so the members
/// rendered are the text ones, in name order — one chunk therefore has one rendering.
fn metadata_text(metadata: &Document) -> String {
    let Document::Object(recorded) = metadata else {
        return String::new();
    };
    let rendered: ::std::collections::BTreeMap<&str, &str> = recorded
        .iter()
        .filter_map(|(name, value)| match value {
            Document::String(value) => Some((name.as_str(), value.as_str())),
            _ => None,
        })
        .collect();
    ::serde_json::json!(rendered).to_string()
}
