// SPDX-License-Identifier: CC0-1.0

//! What an UPDATE's resulting item is charged against the documented item-size ceiling.
//!
//! # The rule this module implements
//!
//! A write is refused when what the item it leaves behind is CHARGED crosses the documented
//! item-size quota ([`MAX_ITEM_SIZE_BYTES`], "the maximum item size in DynamoDB is 400 KB").
//! What is charged is the bytes the item stores — "the lengths of its attribute names and
//! values", key attributes among them, exactly as [`item_size`] totals them — with the KEY
//! PORTION charged at a floor ([`KEY_PORTION_FLOOR_BYTES`]): an item whose primary-key
//! attributes hold fewer bytes than that floor is charged the floor for them anyway, so a
//! short key buys no headroom. The ceiling an update tops out at is therefore the quota less
//! the part of the floor the key does not fill, which for every key below the floor is the SAME
//! ceiling over the item's non-key bytes: the boundary does not move from one item to the next
//! because one is keyed by fewer bytes than the other. A key at or above the floor fills it and
//! is charged the bytes it holds, so the ceiling there is the quota itself.
//!
//! Beside those bytes, an update stated as an EXPRESSION is charged for the clauses it states
//! ([`ParsedUpdate::request_shape_charge`]), because such a request asks for work over and
//! above the item it leaves; a request that states its change without an expression — the
//! legacy `AttributeUpdates` map, and a PartiQL statement — states no clauses and is charged
//! the item alone.
//!
//! # Where it applies
//!
//! Every surface that applies a stated change to the stored item states the ceiling through
//! [`update_size_exceeded`]: `UpdateItem`, the `Update` action of `TransactWriteItems` and
//! `ExecuteTransaction`, and a PartiQL `UPDATE` through `ExecuteStatement` or
//! `BatchExecuteStatement` — so the five of them stop at the same item. A request that
//! CARRIES a whole item (`PutItem` and its batch and transactional forms) states the item
//! itself rather than a change to one, and is bounded by the quota over the bytes it carries
//! ([`item_size`]) with no floor of its own.

use super::*;

/// The least the primary-key portion of an item is charged against the item-size quota on the
/// update path.
///
/// MEASURED behavior, not a modelled or documented constraint: neither the Smithy model nor the
/// Developer Guide's item-size quota ("Service, Account, and Table Quotas") states a floor, and
/// the recorded refusal boundary is what pins it — an update on a table with a one-byte hash
/// key name and a one-byte key value (two key bytes) is refused at a 409,600-byte item and
/// admitted twenty bytes under it, which is this floor less those two bytes. It is stated here
/// as a byte count of the RULE rather than of any one item, exactly as the per-clause frames
/// beside it are ([`ParsedUpdate::request_shape_charge`]).
pub const KEY_PORTION_FLOOR_BYTES: i64 = 22;

/// What a key portion holding `key_bytes` stored bytes is charged: its own bytes once it fills
/// [`KEY_PORTION_FLOOR_BYTES`], and the floor while it does not.
pub fn key_portion_charge(key_bytes: i64) -> i64 {
    key_bytes.max(KEY_PORTION_FLOOR_BYTES)
}

/// What `item` is charged against [`MAX_ITEM_SIZE_BYTES`] on the update path: its non-key bytes,
/// plus what its key portion is charged ([`key_portion_charge`]), plus what the request's own
/// shape costs (`request_shape_charge`, zero for a change stated without an expression).
///
/// The key portion is the item's primary-key attributes as the table's key schema names them
/// ([`Table::key_attributes`]), weighed the way every other part of the item is
/// ([`item_size`]), so a composite key is charged across both of its attributes.
///
/// A READ of the table and the item it is handed: nothing is stored and nothing is modified.
pub fn update_charged_size(table: &Table, item: &Item, request_shape_charge: i64) -> i64 {
    let key_bytes = item_size(&table.key_attributes(item));
    item_size(item)
        .saturating_sub(key_bytes)
        .saturating_add(key_portion_charge(key_bytes))
        .saturating_add(request_shape_charge)
}

/// Whether the item an update would leave behind, charged as [`update_charged_size`] charges
/// it, crosses the documented item-size quota — the one question every update surface asks
/// before it writes.
///
/// A READ of the item it is handed: nothing is stored and nothing is modified, so asking
/// twice answers twice the same.
pub fn update_size_exceeded(table: &Table, item: &Item, request_shape_charge: i64) -> bool {
    update_charged_size(table, item, request_shape_charge) > MAX_ITEM_SIZE_BYTES
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: a key portion under the floor is charged the floor, so the headroom an update has
    /// over the rest of the item is the same for every such key; one at or above it is charged
    /// the bytes it holds.
    #[test]
    fn the_key_portion_is_charged_at_its_floor() {
        // Key portions below the floor, each of which must be charged the floor itself: the
        // smallest a table can have (a one-byte name beside a one-byte value), the two sizes
        // whose one-byte difference moved the recorded boundary, and the byte under the floor.
        for key_bytes in [2i64, 9, 10, KEY_PORTION_FLOOR_BYTES - 1] {
            let charged = key_portion_charge(key_bytes);
            assert_eq!(
                charged, KEY_PORTION_FLOOR_BYTES,
                "case \"{key_bytes}-byte key\": expected the floor to be charged, got {charged}"
            );
        }
        // The floor itself and key portions past it, each charged its own bytes.
        for key_bytes in [KEY_PORTION_FLOOR_BYTES, KEY_PORTION_FLOOR_BYTES + 1, 300] {
            let charged = key_portion_charge(key_bytes);
            assert_eq!(
                charged, key_bytes,
                "case \"{key_bytes}-byte key\": expected the key's own bytes to be charged, got \
                 {charged}"
            );
        }
    }
}
