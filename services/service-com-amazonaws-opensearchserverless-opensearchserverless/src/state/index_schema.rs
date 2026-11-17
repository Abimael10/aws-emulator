// SPDX-License-Identifier: CC0-1.0

//! What an index's stored schema becomes when a writer submits another one.
//!
//! A schema write on an existing index MODIFIES the definition the index already carries:
//! `UpdateIndex` "allows you to modify the index schema, including adding new fields or
//! changing field mappings" (`smithy_query(query="operation", operation="UpdateIndex")`),
//! and a mapping write over the collection's own REST endpoint is the same request from the
//! other side. So the submitted document is MERGED into the stored one — an object is
//! merged member by member, at every level, and anything that is not an object replaces
//! what was held in its place — which is what leaves the members the request does not name
//! exactly as they were. A field added that way does not unmap the fields that were there,
//! and writing the same schema twice lands the same state, which is what `@idempotent` on
//! that operation asks for.
//!
//! One rule, one implementation: the control plane's `UpdateIndex` and the REST plane's
//! `PUT /{index}/_mapping` both write an index's schema through here, so the two planes
//! cannot disagree about what a schema write leaves behind.

use ::aws_smithy_types::Document;

/// The schema an index holds after `submitted` is written over `held`.
///
/// `held` is the schema the index carries now (`None` for an index created without one, in
/// which case the submitted document IS the new schema). Both documents are already within
/// [`super::MAX_DOCUMENT_DEPTH`] — nothing is stored that is not — so the walk below is
/// bounded by that depth; a level past it takes the submitted value rather than descending
/// further.
///
/// The RESULT is not bounded by this function: merging two documents that each fit can make
/// one that does not, so a caller that STORES the result holds it to
/// [`super::MAX_INDEX_SCHEMA_NODES`] through [`super::document_within_bounds`] exactly as it
/// holds a submitted one.
pub fn merged_index_schema(held: Option<&Document>, submitted: Document) -> Document {
    match held {
        Some(held) => merged_at(held, submitted, 1),
        None => submitted,
    }
}

/// [`merged_index_schema`] at one level of the walk, `depth` levels down.
fn merged_at(held: &Document, submitted: Document, depth: usize) -> Document {
    match (held, submitted) {
        (Document::Object(held), Document::Object(submitted)) => {
            if depth >= super::MAX_DOCUMENT_DEPTH {
                return Document::Object(submitted);
            }
            let mut merged = held.clone();
            for (name, member) in submitted {
                let written = match merged.get(&name) {
                    Some(held) => merged_at(held, member, depth + 1),
                    None => member,
                };
                merged.insert(name, written);
            }
            Document::Object(merged)
        }
        // Anything that is not an object on either side is ONE value, and a write of one
        // value replaces the value that was held.
        (_, submitted) => submitted,
    }
}
