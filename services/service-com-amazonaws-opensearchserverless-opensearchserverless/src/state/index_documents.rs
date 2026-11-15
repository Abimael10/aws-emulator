// SPDX-License-Identifier: CC0-1.0

//! The documents of one index of a collection, as written by the plane of ANOTHER service
//! that was handed this collection as its vector store, and the field mapping that index
//! declares.
//!
//! A collection's own REST surface is what a CLIENT writes documents through
//! ([`crate::service::collection_rest`]). A service of this emulator that a caller BOUND to
//! one of this collection's indexes writes them too: Amazon Bedrock's knowledge base names
//! a collection and a vector index in its storage configuration and records the vector
//! embeddings of its data sources in the fields that configuration maps, so the chunks its
//! ingestion takes in belong in THIS store — the one a client's own search, count and
//! document read are answered out of. Without that, the collection would report an empty
//! index while the knowledge base reported documents indexed into it, and the published
//! endpoint would answer a client nothing about work that really happened.
//!
//! A write here goes through the SAME record the REST plane writes
//! ([`super::IndexRecord::write_document`]) and under the same bounds
//! ([`super::MAX_DOCUMENTS_PER_INDEX`], [`super::MAX_DOCUMENT_SOURCE_NODES`]), so a document
//! written through this seam is a document a client reads back, versioned and sequenced the
//! way the plane beside it versions and sequences one. A collection that is not ACTIVE has
//! no cluster behind its endpoint yet, so it takes nothing — exactly as the REST plane
//! answers such a collection nothing.
//!
//! What is NOT decided here is whether the writer MAY write: who may act on a collection's
//! data is the `data` access policies' question ([`super::data_access`]), which the calling
//! service asks about the principal it acts as, exactly as the REST plane asks it about the
//! principal a request is signed by.

use ::aws_smithy_types::{Document, Number};

use super::{ComAmazonawsOpensearchserverlessOpensearchserverlessState as ServiceState, IndexRecord};

/// The member of an index's stored schema that maps its fields, as a `PUT /{index}` body and
/// the control plane's `indexSchema` both spell it.
const SCHEMA_MAPPINGS: &str = "mappings";

/// The member of a mapping holding one entry per mapped field.
const MAPPING_PROPERTIES: &str = "properties";

/// The member a `knn_vector` field's mapping declares its length under.
const KNN_VECTOR_DIMENSION: &str = "dimension";

/// The mapping the index declares for the field `field`, or `None` when it maps no such
/// field.
///
/// A dotted `field` names a member of an object field, which the engine addresses through
/// that field's own `properties` — so one level of the path is one `properties` hop. This is
/// the ONE place this crate knows where a field's mapping lives in a stored index schema:
/// the REST plane's k-NN query reads a field's space type through it, and the seam below
/// reads a vector field's length through it.
pub fn index_field_mapping<'i>(index: &'i IndexRecord, field: &str) -> Option<&'i Document> {
    let mut held = member(index.index_schema.as_ref()?, SCHEMA_MAPPINGS)?;
    for segment in field.split('.') {
        held = member(member(held, MAPPING_PROPERTIES)?, segment)?;
    }
    Some(held)
}

/// How many values the `knn_vector` field `field` of `index` holds, as its mapping declares
/// it; `None` when the index maps no such field or declares no whole-number dimension for
/// it.
pub fn knn_vector_dimension(index: &IndexRecord, field: &str) -> Option<i64> {
    match member(index_field_mapping(index, field)?, KNN_VECTOR_DIMENSION)? {
        Document::Number(Number::PosInt(values)) => i64::try_from(*values).ok(),
        Document::Number(Number::NegInt(values)) => Some(*values),
        // A dimension is a count of values, so a fraction declares none.
        _ => None,
    }
}

/// One named member of a document object, or `None` for anything else.
fn member<'d>(document: &'d Document, name: &str) -> Option<&'d Document> {
    match document {
        Document::Object(members) => members.get(name),
        _ => None,
    }
}

impl ServiceState {
    /// How many values the `knn_vector` field `field` of the index `index` of the collection
    /// `collection_arn` holds, or `None` when this store holds no such collection, index or
    /// mapped field.
    ///
    /// What it is FOR: a service bound to that index writes vectors of the length the index
    /// itself declares rather than of a length it assumed.
    pub fn collection_index_vector_dimension(
        &self,
        collection_arn: &str,
        index: &str,
        field: &str,
    ) -> Option<i64> {
        knn_vector_dimension(
            self.collection_by_arn(collection_arn)?.indexes.get(index)?,
            field,
        )
    }

    /// Whether this store holds the index `index` of the collection `collection_arn` — the
    /// existence question a service that was handed the pair asks before it binds to it.
    pub fn holds_collection_index(&self, collection_arn: &str, index: &str) -> bool {
        self.collection_by_arn(collection_arn)
            .is_some_and(|collection| collection.indexes.contains_key(index))
    }

    /// Write each of `documents` — an id and the source held under it — into the index
    /// `index` of the collection `collection_arn`, answering the ids the index really TOOK,
    /// in the order they were written.
    ///
    /// A write of an id the index already holds replaces that document and advances its
    /// version, as a write through the collection's own endpoint does. Nothing is taken for
    /// a collection or an index this store does not hold, for a collection that is not
    /// ACTIVE, for a source past the bounds one document is held under, or past the number
    /// of documents one index holds — so what comes back is what the collection holds and
    /// the caller accounts for the rest as not indexed.
    pub fn write_collection_index_documents(
        &mut self,
        collection_arn: &str,
        index: &str,
        documents: Vec<(String, Document)>,
    ) -> Vec<String> {
        // The transitions the clock has already made due are applied first, so a collection
        // whose create has settled is ACTIVE here and one whose accepted deletion has been
        // purged is gone.
        self.settle_due_transitions();
        let Some(collection) = self.collection_by_arn_mut(collection_arn) else {
            return Vec::new();
        };
        if collection.status != super::COLLECTION_ACTIVE {
            return Vec::new();
        }
        let Some(held) = collection.indexes.get_mut(index) else {
            return Vec::new();
        };
        let mut written: Vec<String> = Vec::new();
        for (id, source) in documents {
            if !super::document_within_bounds(&source, super::MAX_DOCUMENT_SOURCE_NODES) {
                continue;
            }
            if !held.documents.contains_key(&id)
                && held.documents.len() >= super::MAX_DOCUMENTS_PER_INDEX
            {
                break;
            }
            held.write_document(&id, source);
            written.push(id);
        }
        written
    }

    /// Take the documents held under `ids` out of the index `index` of the collection
    /// `collection_arn`, answering how many of them were there to take.
    ///
    /// Every id addressed takes a sequence number from the index, whether or not a document
    /// was held under it, exactly as a delete through the collection's own endpoint does — a
    /// delete is a write of the id it addresses, which is what lets a client order it against
    /// the write before it. A collection that is not ACTIVE has no cluster behind its
    /// endpoint, so nothing is taken out of it either.
    pub fn remove_collection_index_documents(
        &mut self,
        collection_arn: &str,
        index: &str,
        ids: &[String],
    ) -> usize {
        self.settle_due_transitions();
        let Some(collection) = self.collection_by_arn_mut(collection_arn) else {
            return 0;
        };
        if collection.status != super::COLLECTION_ACTIVE {
            return 0;
        }
        let Some(held) = collection.indexes.get_mut(index) else {
            return 0;
        };
        let mut removed = 0usize;
        for id in ids {
            let taken = held.documents.remove(id);
            held.take_seq_no();
            removed += usize::from(taken.is_some());
        }
        removed
    }
}
