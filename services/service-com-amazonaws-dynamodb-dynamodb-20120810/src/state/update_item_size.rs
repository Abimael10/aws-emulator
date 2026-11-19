// SPDX-License-Identifier: CC0-1.0

//! What an UPDATE's resulting item is charged against the documented item-size ceiling.
//!
//! # The rule this module implements
//!
//! A write is refused when the item it leaves behind crosses the documented item-size quota
//! ([`MAX_ITEM_SIZE_BYTES`], "the maximum item size in DynamoDB is 400 KB"), and the quota is
//! FLAT: what is charged is the bytes the item stores — "the lengths of its attribute names
//! and values", key attributes among them, exactly as [`item_size`] totals them — with
//! nothing added for the shape of the item and no part of it charged at a floor. An item
//! exactly AT the quota is stored and one byte past it is refused, whatever the item is keyed
//! by: the boundary moves with the bytes the item holds and with nothing else.
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
//! itself rather than a change to one, and is bounded by the same quota over the bytes it
//! carries ([`item_size`]).

use super::*;

/// Whether the item an update would leave behind, plus what the request's own shape costs
/// (`request_shape_charge`, zero for a change stated without an expression), crosses the
/// documented item-size quota — the one question every update surface asks before it writes.
///
/// A READ of the item it is handed: nothing is stored and nothing is modified, so asking
/// twice answers twice the same.
pub fn update_size_exceeded(item: &Item, request_shape_charge: i64) -> bool {
    item_size(item).saturating_add(request_shape_charge) > MAX_ITEM_SIZE_BYTES
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One attribute value of `bytes` storable bytes, as a `S` value of that many
    /// single-byte characters.
    fn text_of(bytes: usize) -> AttributeValue {
        AttributeValue::S("x".repeat(bytes))
    }

    /// An item of exactly `total` stored bytes: a one-character key name and value, and one
    /// payload attribute filling the rest.
    fn item_of(total: i64) -> Item {
        let mut item = Item::new();
        item.insert("k".to_string(), text_of(1));
        // 1 byte of key name + 1 of key value + 1 of the payload's name.
        let payload = usize::try_from(total - 3).expect("a total past the fixed bytes");
        item.insert("v".to_string(), text_of(payload));
        item
    }

    /// RULE: the ceiling is the quota itself over the bytes the item stores — the item at it
    /// is admitted and one byte past it is refused — and what the item is keyed by moves
    /// nothing.
    #[test]
    fn the_ceiling_is_the_flat_quota_over_the_stored_bytes() {
        let at_the_quota = item_of(MAX_ITEM_SIZE_BYTES);
        assert_eq!(
            item_size(&at_the_quota),
            MAX_ITEM_SIZE_BYTES,
            "the fixture must hold exactly the quota, got {}",
            item_size(&at_the_quota)
        );
        assert!(
            !update_size_exceeded(&at_the_quota, 0),
            "case \"at the quota\": expected the item at the quota to be admitted, got \
             {} charged",
            item_size(&at_the_quota)
        );
        let past_the_quota = item_of(MAX_ITEM_SIZE_BYTES + 1);
        assert!(
            update_size_exceeded(&past_the_quota, 0),
            "case \"one byte past the quota\": expected a refusal, got {} charged",
            item_size(&past_the_quota)
        );
    }

    /// RULE: what the request's shape costs is charged on top, so a clause stated beside the
    /// change shortens what the item may hold by exactly that much.
    #[test]
    fn the_request_shape_is_charged_on_top_of_the_item() {
        let at_the_quota = item_of(MAX_ITEM_SIZE_BYTES);
        for charge in [1i64, 19, 74] {
            assert!(
                update_size_exceeded(&at_the_quota, charge),
                "case {charge}: expected the item at the quota plus {charge} charged byte(s) \
                 to be refused"
            );
        }
        let under = item_of(MAX_ITEM_SIZE_BYTES - 19);
        assert!(
            !update_size_exceeded(&under, 19),
            "case \"19 bytes of headroom\": expected the item plus its clause's charge to \
             reach exactly the quota, got {} charged",
            item_size(&under) + 19
        );
    }
}
