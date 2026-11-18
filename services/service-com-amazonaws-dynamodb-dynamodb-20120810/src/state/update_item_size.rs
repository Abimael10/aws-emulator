// SPDX-License-Identifier: CC0-1.0

//! What an UPDATE's resulting item is charged against the documented item-size ceiling.
//!
//! # The rule this module implements
//!
//! A write is refused when the item it leaves behind crosses the documented item-size quota
//! ([`MAX_ITEM_SIZE_BYTES`], "the maximum item size in DynamoDB is 400 KB"). On the UPDATE
//! path that accounting is FLOOR-BOUND rather than flat: the KEY portion of the item — the
//! partition key attribute and, where the table has one, the sort key attribute, name and
//! value together — is charged at no less than [`MIN_ITEM_KEY_CHARGE`], so an item whose key
//! is shorter than that floor may hold correspondingly LESS than the flat quota and the
//! acceptable total tops out below it. An item whose key already reaches the floor is charged
//! its own bytes and is bounded by the quota itself.
//!
//! Recorded behavior: the quota and the sentence are documented, the floor is not — it is
//! what the recordings show the service charging, and the figure is stated once here so every
//! update surface is bounded alike.
//!
//! # Where it applies
//!
//! Every surface that applies a stated change to the stored item: `UpdateItem`, the `Update`
//! action of `TransactWriteItems` and `ExecuteTransaction`, and a PartiQL `UPDATE` through
//! `ExecuteStatement` or `BatchExecuteStatement`. A request that CARRIES a whole item
//! (`PutItem` and its batch and transactional forms) states the item itself rather than a
//! change to one, and is bounded by the quota over the bytes it carries
//! ([`item_size`]) — the accounting it has always been held to.

use super::*;

/// The least the KEY portion of an item is charged against [`MAX_ITEM_SIZE_BYTES`] on the
/// update path, in bytes.
///
/// Recorded behavior, and the one figure here that is not documented: an update of an item
/// whose whole key portion is TWO bytes (a one-character attribute name and a one-character
/// value) is refused at a total of 409,600 bytes and served at 409,580 — twenty bytes short of
/// the quota, which is exactly what charging that two-byte key as [`MIN_ITEM_KEY_CHARGE`]
/// leaves. It is charged on the KEY rather than as a flat per-update overhead because that is
/// the accounting the recording's own analysis states, and because the quota is documented
/// over "the lengths of its attribute names and values" — the key's among them — so the figure
/// belongs to the part of the item that has one, which is why an item whose key already
/// reaches the floor is bounded by the quota itself and not by the quota less a constant.
pub const MIN_ITEM_KEY_CHARGE: i64 = 22;

/// The bytes of `item` that belong to its KEY: the partition key attribute, and the sort key
/// attribute of a table that has one, each charged its name plus its value exactly as
/// [`item_size`] charges it.
///
/// An item missing one of those attributes contributes nothing for it, which cannot happen on
/// a write that resolved a key but keeps this a total of what the item actually holds.
fn key_bytes(item: &Item, partition_key: &str, sort_key: Option<&str>) -> i64 {
    [Some(partition_key), sort_key]
        .into_iter()
        .flatten()
        .filter_map(|attribute| {
            item.get(attribute).map(|value| {
                i64::try_from(attribute.len())
                    .unwrap_or(i64::MAX)
                    .saturating_add(attribute_value_size(value))
            })
        })
        .fold(0i64, i64::saturating_add)
}

/// The size the item an update leaves behind is charged against [`MAX_ITEM_SIZE_BYTES`]: its
/// own bytes ([`item_size`]) with its key portion raised to [`MIN_ITEM_KEY_CHARGE`] when the
/// key itself is shorter than that.
///
/// Never below [`item_size`], so a key at or above the floor is charged exactly what it
/// stores and this accounting only ever makes the ceiling tighter — never looser.
pub fn charged_update_size(item: &Item, partition_key: &str, sort_key: Option<&str>) -> i64 {
    let key = key_bytes(item, partition_key, sort_key);
    item_size(item).saturating_add((MIN_ITEM_KEY_CHARGE - key).max(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One attribute value of `bytes` storable bytes, as a `S` value of that many
    /// single-byte characters.
    fn text_of(bytes: usize) -> AttributeValue {
        AttributeValue::S("x".repeat(bytes))
    }

    /// RULE: the key portion is charged at no less than the floor, so an item whose key is
    /// shorter than it is charged MORE than it stores and one whose key reaches it is charged
    /// exactly what it stores.
    #[test]
    fn a_short_key_is_charged_at_the_floor() {
        for (case, key_name, key_value, padding, expected_extra) in [
            // A one-character hash key and a one-character value: two bytes of key against a
            // 22-byte floor, so twenty bytes are added.
            ("a one-byte key name and value", "k", 1usize, 0usize, 20i64),
            // The same key with a payload beside it: the payload is charged as stored, so the
            // same twenty bytes are added whatever else the item holds.
            ("a short key beside a payload", "k", 1, 1_000, 20),
            // A key that reaches the floor exactly is charged what it stores.
            ("a key exactly at the floor", "key", 19, 0, 0),
            // ...and one past it likewise.
            ("a key past the floor", "key", 40, 0, 0),
        ] {
            let mut item = Item::new();
            item.insert(key_name.to_string(), text_of(key_value));
            if padding > 0 {
                item.insert("v".to_string(), text_of(padding));
            }
            let stored = item_size(&item);
            let charged = charged_update_size(&item, key_name, None);
            assert_eq!(
                charged - stored,
                expected_extra,
                "case {case:?}: expected the key floor to add {expected_extra} byte(s) to the \
                 {stored} stored, got {charged}"
            );
        }
    }

    /// RULE: a table with a sort key charges BOTH key attributes together against the floor,
    /// so the two together are what has to reach it.
    #[test]
    fn a_composite_key_is_charged_as_one_portion() {
        let mut item = Item::new();
        item.insert("pk".to_string(), text_of(4));
        item.insert("sk".to_string(), text_of(4));
        let stored = item_size(&item);
        // 2 + 4 + 2 + 4 = 12 bytes of key against the 22-byte floor.
        let charged = charged_update_size(&item, "pk", Some("sk"));
        assert_eq!(
            charged - stored,
            MIN_ITEM_KEY_CHARGE - 12,
            "expected both key attributes to count toward the floor, got {charged} over \
             {stored} stored"
        );
    }
}
