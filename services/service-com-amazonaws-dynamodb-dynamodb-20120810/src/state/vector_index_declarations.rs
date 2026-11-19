// SPDX-License-Identifier: CC0-1.0

//! How a request's VECTOR-INDEX declarations are refused.
//!
//! Three facts about the vector indexes a request DECLARES rather than about what the
//! store holds, so they live here and not in the operations that read them:
//!
//! * more vector indexes on one table than a table may carry
//!   ([`check_vector_index_count`]). The sentence states the per-table limit in force and
//!   nothing of the table or of the count that was sent, because the fact is the quota,
//!   which is the same for every table of every account;
//! * a `SearchSchema` element naming an attribute the declaration states no
//!   `AttributeDefinition` for ([`SEARCH_SCHEMA_ATTRIBUTE_UNDECLARED_MESSAGE`]). A search
//!   schema partitions and filters by that attribute's VALUE, so its type has to be
//!   declared — and the whole sentence is fixed, naming neither the index nor the
//!   attribute, because what is wrong is that one ELEMENT of the schema has no definition;
//! * two vector indexes over one vector attribute that disagree about how many dimensions
//!   its vectors have ([`check_vector_dimensions_agree`]): "If multiple vector indexes
//!   reference the same attribute, they must all use the same number of dimensions"
//!   (`CreateVectorIndexAction$VectorAttribute`), so a declaration stating two dimension
//!   counts for one attribute describes no vector the table could hold.
//!
//! Each sentence opens with the clause every refusal of a parameter value in this service
//! carries ([`super::INVALID_PARAMETER_CLAUSE`]), and each rule holds wherever a vector
//! index is declared — `CreateTable$VectorIndexes`, the table creation parameters
//! `ImportTable` declares the same table through, and the `Create` action of
//! `UpdateTable$VectorIndexUpdates` — so every one of those surfaces draws it from here
//! rather than stating a copy. `super` re-exports this module, so every caller still names
//! them `crate::state::…`.

use super::{RequestError, VectorIndex, INVALID_PARAMETER_CLAUSE, MAX_VECTOR_INDEXES};

/// AWS's text for a table that would carry more vector indexes than one may:
/// fixed throughout but for the limit, which the sentence states as the per-table limit in
/// force ([`MAX_VECTOR_INDEXES`]).
pub fn vector_index_count_limit_message(limit: usize) -> String {
    format!("{INVALID_PARAMETER_CLAUSE}: VectorIndex count exceeds the per-table limit of {limit}")
}

/// Hold the vector indexes a table would CARRY to the per-table limit
/// ([`MAX_VECTOR_INDEXES`]).
///
/// `count` is what the table ends up carrying, so one rule answers a table declared with
/// too many at once and a table taken past the limit one added index at a time.
pub fn check_vector_index_count(count: usize) -> Result<(), RequestError> {
    if count > MAX_VECTOR_INDEXES {
        return Err(RequestError::Validation(vector_index_count_limit_message(
            MAX_VECTOR_INDEXES,
        )));
    }
    Ok(())
}

/// AWS's fixed text for a `SearchSchema` element naming an attribute the declaration
/// states no `AttributeDefinition` for: the sentence says that ONE element of the schema
/// has no definition, and interpolates nothing — not the index, not the attribute name,
/// which may be any string the member admits (`SearchSchemaElement$AttributeName` is
/// `@length 0..65535`, the empty string included).
pub const SEARCH_SCHEMA_ATTRIBUTE_UNDECLARED_MESSAGE: &str =
    "One or more parameter values were invalid: One element in SearchSchema is not defined in \
     attribute definitions";

/// AWS's text for two vector indexes that disagree about one vector attribute's
/// dimensions: the attribute is named, quoted, because WHICH attribute carries the two
/// readings is what the caller has to reconcile, and the rule itself is the fixed
/// sentence after it.
pub fn conflicting_vector_dimensions_message(attribute: &str) -> String {
    format!(
        "{INVALID_PARAMETER_CLAUSE}: Conflicting attribute definition for '{attribute}'. All \
         VectorIndexes on the same vector attribute must use the same dimensions."
    )
}

/// Hold every vector index of one table to ONE dimension count per vector attribute.
///
/// "If multiple vector indexes reference the same attribute, they must all use the same
/// number of dimensions" (`CreateVectorIndexAction$VectorAttribute`): the dimensions are a
/// property of the vectors the attribute HOLDS, so two indexes over it cannot each have
/// their own — and an item carrying that attribute is held to the same single count when
/// it is written ([`super::Table::check_vector_attributes`]), which is only possible
/// because this refuses the declaration that states two.
///
/// Asked of the indexes the table would end up CARRYING, so it answers a table declared
/// with the conflicting pair and a table that would be taken into that state by an index
/// added later. The walk is quadratic in a list the per-table limit above bounds to
/// [`MAX_VECTOR_INDEXES`].
pub fn check_vector_dimensions_agree(indexes: &[VectorIndex]) -> Result<(), RequestError> {
    for (position, index) in indexes.iter().enumerate() {
        let attribute = index.vector_attribute.attribute_name.as_str();
        let conflicting = indexes[..position].iter().any(|earlier| {
            earlier.vector_attribute.attribute_name == attribute
                && earlier.dimensions != index.dimensions
        });
        if conflicting {
            return Err(RequestError::Validation(
                conflicting_vector_dimensions_message(attribute),
            ));
        }
    }
    Ok(())
}
