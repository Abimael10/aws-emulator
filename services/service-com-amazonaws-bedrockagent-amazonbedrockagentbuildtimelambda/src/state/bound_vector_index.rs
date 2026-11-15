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
}
