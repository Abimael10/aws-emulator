// SPDX-License-Identifier: CC0-1.0

//! What a VECTOR is: the one reading of the attribute a vector index indexes items by.
//!
//! "Each element is a 32-bit IEEE-754 floating point number, provided in DynamoDB list
//! format" (`SearchVectors$SearchVector`, which is the same shape
//! `VectorIndex$VectorAttribute` names on an item), so a vector is a LIST whose every
//! element is a number such a float can hold. Three rules are decided by that reading —
//! which items a write may store, which items an index holds an entry for, and what a
//! search compares — so it is stated once here rather than once per caller. It is
//! re-exported up to `crate::state`, so every caller names it `crate::state::…`.

use crate::ops::types::AttributeValue;

/// The numbers `value` holds as a vector, or `None` when it is not one: a value of
/// another type, or a list carrying an element no 32-bit float can hold.
///
/// The length is the caller's to judge — an index requires exactly its `Dimensions`,
/// while a search vector is compared against the index it names — so this answers only
/// what the value IS.
pub fn vector_values(value: &AttributeValue) -> Option<Vec<f32>> {
    let AttributeValue::L(values) = value else {
        return None;
    };
    values.iter().map(vector_component).collect()
}

/// The number ONE element of a vector holds, or `None` when the element is no component
/// of one: a value of another type, or a number no 32-bit float can hold.
///
/// This is the per-element half of [`vector_values`], stated once so that a caller which
/// has to report WHICH element is at fault walks the list by the same reading as a caller
/// that only asks whether the whole value is a vector.
pub fn vector_component(element: &AttributeValue) -> Option<f32> {
    match element {
        AttributeValue::N(text) => text
            .parse::<f32>()
            .ok()
            .filter(|number| number.is_finite()),
        _ => None,
    }
}

/// The vector attribute as an INDEX HOLDS it, or `None` when the value is no vector.
///
/// "Each element is a 32-bit IEEE-754 floating point number" (`SearchVectors$SearchVector`),
/// so what the index stores for a coordinate is the float that type can hold, not the
/// decimal the request spelled it with: a number a 32-bit float cannot represent exactly is
/// held as the one it rounds to, and a read of the index reports THAT. A table's own item
/// keeps the decimal it was written with, which is what the item store holds and what a
/// `GetItem` of it answers — the two differ exactly where the rounding does.
pub fn stored_vector(value: &AttributeValue) -> Option<AttributeValue> {
    Some(AttributeValue::L(
        vector_values(value)?
            .into_iter()
            .map(|component| AttributeValue::N(rendered_vector_component(component)))
            .collect(),
    ))
}

/// One held coordinate as the decimal a response carries it in: the shortest decimal that
/// reads back as the same 32-bit float, carrying a fractional part so a coordinate whose
/// float is a whole number still reads as the floating-point value it is.
///
/// NOT the canonical `N` text the ITEM store keeps a number in
/// (`super::canonical_number_text`, which trims a whole number's fractional part): what
/// the index holds is a FLOAT, and the recorded answers of a search report it as one — a
/// coordinate whose float is a whole number comes back with its `.0`, where the same number
/// read off the table's own item comes back without it. The two renderings differ because
/// the two stores hold different things, which is the whole of this module's rule.
///
/// It is the shortest round-trip rendering of that float and nothing else, so the exponent
/// form it takes at the magnitudes where a positional decimal would be longer is this
/// renderer's own: no recorded answer of this service reaches that magnitude, so there is
/// nothing to pin it to and nothing is invented for it.
fn rendered_vector_component(component: f32) -> String {
    format!("{component:?}")
}

/// What one dimension of a STORED vector costs: a vector attribute is a list of 32-bit
/// IEEE-754 floats ([`vector_values`]), so each dimension is four bytes once it is held as
/// the index holds it rather than as the decimal text the request sent.
///
/// It is what sizes a vector wherever this service meters one — the capacity a read or a
/// write of the item consumes, the bytes a write of an index entry meters, and the bytes a
/// search meters for each vector it scored — so none of those figures depends on how many
/// digits a coordinate was written with.
pub const BYTES_PER_VECTOR_DIMENSION: i64 = 4;

/// The least one vector request meters: a vector index is metered in BYTES
/// ("VectorWriteRequestBytes", "VectorSearchRequestBytes" — `VectorCapacity`) with a
/// one-kilobyte floor, so a small vector costs the floor rather than its own few bytes, the
/// way a write of a small item still costs one capacity unit.
pub const MIN_VECTOR_REQUEST_BYTES: i64 = 1024;
