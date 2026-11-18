// SPDX-License-Identifier: CC0-1.0

//! The vector index a knowledge base is BOUND to, whichever store holds it: what one
//! ingested chunk is, and the three things every plane here does with the bound index —
//! read how many values its vectors carry, write the chunks of an ingestion into it, and
//! take them back out.
//!
//! `KnowledgeBaseStorageType` admits several stores and a knowledge base keeps its vectors
//! in the one its `storageConfiguration` names, so "the index this knowledge base writes
//! into" is one question with one answer per knowledge base — and the planes that ingest,
//! push and delete documents ask it the same way. This module is that answer
//! ([`BoundVectorIndex`]), so a store this emulator serves is reached by every one of them
//! and none of them decides for itself which store a knowledge base has.
//!
//! Two stores of this emulator hold a knowledge base's vectors, and a chunk written into
//! either is a chunk the OWNING service answers for from its own read — Amazon S3 Vectors
//! from `ListVectors` and `QueryVectors` ([`crate::state::vector_store`]), and an Amazon
//! OpenSearch Serverless collection from the search, count and document read served at the
//! endpoint it publishes ([`crate::state::opensearch_serverless_store`]). A knowledge base
//! bound to any other store binds to no index this emulator holds: nothing is written
//! through, and what such a knowledge base HOLDS is the chunk its ingestion recorded on the
//! document ([`crate::state::ingestion_record`]).
//!
//! A store that HOLDS the index is therefore the authority on what the knowledge base holds,
//! for the read as much as for the write: the chunks a retrieval reports are the ones that
//! store still holds ([`BoundVectorIndex::chunks_nearest`],
//! [`BoundVectorIndex::chunk_fields`]), so a chunk taken out of the index through the owning
//! service's own data plane stops being reported and an index taken out of a container this
//! emulator holds is the failed dependency it is ([`ChunkRanking`]).
//!
//! LOCK ORDER: every function here reaches the owning store and holds no guard of this
//! service's store while it does — call them with NO guard of it held.

use super::*;

/// One chunk an ingestion writes into the bound index, as every store takes it.
///
/// Each member is computed by the ingestion that read the document
/// ([`crate::state::ingestion`], [`crate::state::pushed_documents`]) and bounded by it: the
/// text by [`ingestion_index::CHUNK_MAX_BYTES`], the vector by the dimension the bound
/// index or the embedding model declares, and the metadata by the fields a knowledge base
/// records ([`ingestion_index::chunk_metadata`]).
#[derive(Debug, Clone, PartialEq)]
pub struct WrittenChunk {
    /// The key the chunk is held under in the bound index, which is what names it for a
    /// later read or delete ([`crate::state::document_vector_key`]).
    pub key: String,
    /// The chunk's own text.
    pub text: String,
    /// The vector of that text, which is what orders the chunk against a query.
    pub vector: Vec<f32>,
    /// The fields the knowledge base records with the chunk.
    pub metadata: Document,
}

/// The index a knowledge base's `storageConfiguration` binds it to, in the store that holds
/// it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BoundVectorIndex {
    /// An index of an Amazon S3 Vectors vector bucket.
    S3Vectors(S3VectorsBinding),
    /// A vector index of an Amazon OpenSearch Serverless collection, written as the role
    /// the knowledge base acts as — which is the principal that collection's own data
    /// access policies decide about.
    OpenSearchServerless {
        index: OpenSearchServerlessIndex,
        role_arn: String,
    },
}

impl BoundVectorIndex {
    /// The index `knowledge_base` is bound to, or `None` when it names no index this
    /// emulator holds a store for.
    pub fn of(knowledge_base: &KnowledgeBaseRecord) -> Option<Self> {
        let storage = knowledge_base.storage_configuration.as_ref();
        if let Some(binding) = s3_vectors_binding(storage) {
            return Some(Self::S3Vectors(binding));
        }
        opensearch_serverless_index(storage).map(|index| Self::OpenSearchServerless {
            index,
            role_arn: knowledge_base.role_arn.clone(),
        })
    }

    /// How many values the vectors of this index hold, as the store that owns it reports
    /// its own index; `None` when that store holds no such index or says nothing about its
    /// dimension.
    ///
    /// LOCK ORDER: as the module doc says.
    pub fn dimension<Eng: Send + Sync + 'static>(&self, engine: &Eng) -> Option<i32> {
        match self {
            Self::S3Vectors(binding) => s3_vectors_index_dimension(engine, binding),
            Self::OpenSearchServerless { index, .. } => {
                opensearch_serverless_index_dimension(engine, index)
            }
        }
    }

    /// Write `chunks` into this index, answering the KEYS the store really took — or `None`
    /// when this emulator holds no such store at all, so there is nothing to reconcile the
    /// ingestion against.
    ///
    /// Every store checks a chunk by its OWN rules before it holds it, so what comes back
    /// is what that store holds and the caller accounts for the rest as not indexed. A
    /// write of a key the index already holds replaces that chunk rather than adding a
    /// second, which is what makes re-ingesting a document leave one chunk behind.
    ///
    /// LOCK ORDER: as the module doc says.
    pub fn write<Eng: Send + Sync + 'static>(
        &self,
        engine: &Eng,
        chunks: &[WrittenChunk],
    ) -> Option<Vec<String>> {
        match self {
            Self::S3Vectors(binding) => write_s3_vectors_chunks(engine, binding, chunks),
            Self::OpenSearchServerless { index, role_arn } => {
                write_opensearch_serverless_chunks(engine, index, role_arn, chunks)
            }
        }
    }

    /// Take the chunks held under `keys` out of this index, answering how many were there to
    /// take.
    ///
    /// LOCK ORDER: as the module doc says.
    pub fn remove<Eng: Send + Sync + 'static>(&self, engine: &Eng, keys: &[String]) -> usize {
        match self {
            Self::S3Vectors(binding) => remove_s3_vectors(engine, binding, keys),
            Self::OpenSearchServerless { index, .. } => {
                remove_opensearch_serverless_chunks(engine, index, keys)
            }
        }
    }

    /// How far each chunk this index holds is from `query`'s own vector — the read side of
    /// [`Self::write`].
    ///
    /// The query's vector is computed the way the ingestion computed each chunk's
    /// ([`document_vector`]), in the dimension the owning store's own index declares, so the
    /// two are comparable; `fallback_dimension` is the dimension to use where the index
    /// declares none, which is the one the knowledge base's chunks carry
    /// ([`record_dimension`]). The metric each arm measures under is that arm's own, and the
    /// answer carries which one it was, so the relevance a retrieval reports is derived from
    /// the geometry the distances were taken in.
    ///
    /// Nothing is COPIED out of the store here beyond one distance and one key per chunk, so
    /// the chunks themselves are read afterwards and one at a time
    /// ([`Self::chunk_fields`]) — only the page a retrieval reports is ever copied.
    ///
    /// LOCK ORDER: as the module doc says.
    pub fn chunks_nearest<Eng: Send + Sync + 'static>(
        &self,
        engine: &Eng,
        query: &str,
        fallback_dimension: i32,
    ) -> ChunkRanking {
        match self {
            Self::S3Vectors(binding) => ranked_s3_vectors_chunks(engine, binding, query),
            Self::OpenSearchServerless { index, .. } => {
                ranked_opensearch_serverless_chunks(engine, index, query, fallback_dimension)
            }
        }
    }

    /// The fields the chunk held under `key` carries in this index: the ones an ingestion
    /// records beside the vector ([`ingestion_index::chunk_metadata`]), read under its own
    /// brief hold of the owning store.
    ///
    /// `None` when the index holds nothing under that key, and when what it holds carries
    /// none of those fields — a vector a caller wrote into the index themselves, which is no
    /// chunk of this knowledge base.
    ///
    /// LOCK ORDER: as the module doc says.
    pub fn chunk_fields<Eng: Send + Sync + 'static>(
        &self,
        engine: &Eng,
        key: &str,
    ) -> Option<Document> {
        match self {
            Self::S3Vectors(binding) => s3_vectors_chunk_fields(engine, binding, key),
            Self::OpenSearchServerless { index, .. } => {
                opensearch_serverless_chunk_fields(engine, index, key)
            }
        }
    }

    /// The index this binding names, as a refusal about it reports the resource: the
    /// container the binding names and the index inside it.
    ///
    /// The ONE spelling of it, so the refusal a retrieval answers for a
    /// [`ChunkRanking::IndexGone`] names the resource the same way whichever store it came
    /// from.
    pub fn resource_name(&self) -> String {
        match self {
            Self::S3Vectors(binding) => format!("{}/{}", binding.vector_bucket, binding.index),
            Self::OpenSearchServerless { index, .. } => {
                format!("{}/{}", index.collection_arn, index.index)
            }
        }
    }
}

/// What the store that owns a bound index answers when it is asked for the chunks nearest a
/// query.
///
/// The three outcomes beside a ranking are the three different things "this index answered
/// nothing" can mean, and a retrieval has to tell them apart: an index the owning store once
/// held and no longer does is a dependency that FAILED, an index no store of this emulator
/// holds at all lives in a vector store nothing here writes, and a store that is not on this
/// engine says nothing about the knowledge base either way.
#[derive(Debug, Clone, PartialEq)]
pub enum ChunkRanking {
    /// The store holds the index: how far each chunk it holds is from the query, keyed by the
    /// key that chunk is held under, and the metric those distances are measured under.
    Held {
        metric: String,
        ranked: Vec<(f32, String)>,
    },
    /// The owning store holds the container the binding names but cannot answer for the index
    /// inside it — it no longer holds that index, or the container itself is not in a state
    /// that serves reads — so the dependency the knowledge base's chunks lived in is gone.
    /// The resource a refusal reports for it is the index the binding names
    /// ([`BoundVectorIndex::resource_name`]).
    IndexGone,
    /// Nothing this emulator holds says that index lives here: the owning store holds no such
    /// container, so no API here wrote the index and what the knowledge base holds is its own
    /// ingestion record ([`crate::state::ingestion_record`]).
    Elsewhere,
    /// The store that owns the binding is not on this engine at all, so nothing can be said
    /// about the knowledge base's chunks.
    StoreUnavailable,
}
