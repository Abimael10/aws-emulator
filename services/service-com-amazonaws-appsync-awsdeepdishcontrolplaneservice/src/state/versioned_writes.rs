// SPDX-License-Identifier: CC0-1.0

//! The TURN each item of a VERSIONED `AMAZON_DYNAMODB` data source is written under.
//!
//! A write through such a data source is not one call: the version it stamps is the one AFTER
//! the version the item carries, so the write READS the item, compares the version the request
//! states against the stored one, and only then writes
//! (`crate::service::graphql_api::dynamodb::versioned`). Two writes of ONE item that read the
//! same version would stamp the same version and the second would discard the first — the very
//! lost update that detection exists to prevent — so every write of one item takes a turn: the
//! next write reads what the write before it left, is compared against THAT version, and
//! advances the version once.
//!
//! One turn per item, keyed by the Region, the table and the key the write addresses, so writes
//! of different items are not held up by each other. Only the items a write is in flight on are
//! kept ([`VersionedWrites::turn`]), so the turns do not grow with the number of items a table
//! has ever held.

use std::collections::BTreeMap;
use std::sync::Arc;

use ::serde_json::Value as Json;
use ::tokio::sync::Mutex;

/// The turns the items being written through versioned data sources are written under.
#[derive(Debug, Default)]
pub struct VersionedWrites {
    /// One turn per item a write is in flight on, keyed by [`addressed`].
    items: BTreeMap<String, Arc<Mutex<()>>>,
}

impl VersionedWrites {
    /// The turn a write of the item `key` of `table` in `region` is made under: the caller
    /// holds it for the whole read-compare-write, so the next write of that item reads what
    /// this one left.
    ///
    /// Forgets the items no write holds a turn on any more, which is every item whose turn is
    /// held by this map alone: a turn is kept only for as long as a write of that item is in
    /// flight, and two writes in flight on one item are handed the same turn because the first
    /// one's own handle keeps it here.
    pub fn turn(&mut self, region: &str, table: &str, key: &Json) -> Arc<Mutex<()>> {
        self.items.retain(|_, turn| Arc::strong_count(turn) > 1);
        Arc::clone(self.items.entry(addressed(region, table, key)).or_default())
    }
}

/// The one text an item's turn is held under: the Region, the table and every attribute of the
/// key the write addresses, in the attributes' own name order.
///
/// Ordered by name rather than as the request wrote them, so two writes of ONE item take the
/// same turn whichever order each request's own key object named that item's key attributes in.
fn addressed(region: &str, table: &str, key: &Json) -> String {
    let attributes: BTreeMap<&str, String> = key
        .as_object()
        .map(|members| {
            members
                .iter()
                .map(|(name, value)| (name.as_str(), value.to_string()))
                .collect()
        })
        .unwrap_or_default();
    let mut held = format!("{region}\u{1f}{table}");
    for (name, value) in attributes {
        held.push('\u{1f}');
        held.push_str(name);
        held.push('\u{1e}');
        held.push_str(&value);
    }
    held
}

#[cfg(test)]
mod tests {
    use super::*;

    use ::serde_json::json;

    #[test]
    fn two_writes_of_one_item_take_the_same_turn() {
        let mut turns = VersionedWrites::default();
        let key = json!({"id": {"S": "A-1"}});
        let first = turns.turn("us-west-2", "items", &key);
        // The rows differ along what makes a write address ANOTHER item: the key, the table and
        // the Region; the same key written in another member order is the same item.
        let cases: &[(&str, &str, &str, Json, bool)] = &[
            (
                "the same item",
                "us-west-2",
                "items",
                json!({"id": {"S": "A-1"}}),
                true,
            ),
            (
                "another key of the same table",
                "us-west-2",
                "items",
                json!({"id": {"S": "A-2"}}),
                false,
            ),
            (
                "the same key of another table",
                "us-west-2",
                "others",
                json!({"id": {"S": "A-1"}}),
                false,
            ),
            (
                "the same key of the same table in another Region",
                "eu-west-1",
                "items",
                json!({"id": {"S": "A-1"}}),
                false,
            ),
        ];
        for (case, region, table, key, shared) in cases {
            let other = turns.turn(region, table, key);
            let same = Arc::ptr_eq(&first, &other);
            assert_eq!(
                same, *shared,
                "case {case:?}: expected sharing the turn to be {shared}, got {same}"
            );
        }

        // A composite key is one item however its attributes were written out.
        let composite = turns.turn("us-west-2", "items", &json!({"owner": {"S": "ada"}, "seq": {"N": "1"}}));
        let reversed = turns.turn("us-west-2", "items", &json!({"seq": {"N": "1"}, "owner": {"S": "ada"}}));
        assert!(
            Arc::ptr_eq(&composite, &reversed),
            "case a composite key written in either order: expected one turn, got two"
        );
    }

    #[test]
    fn a_turn_no_write_holds_is_forgotten() {
        let mut turns = VersionedWrites::default();
        let key = json!({"id": {"S": "A-1"}});
        let held = turns.turn("us-west-2", "items", &key);
        assert_eq!(
            turns.items.len(),
            1,
            "case a write in flight: expected its turn to be kept, got {:?}",
            turns.items
        );
        drop(held);
        let _next = turns.turn("us-west-2", "others", &key);
        assert_eq!(
            turns.items.len(),
            1,
            "case a write that finished: expected only the turn in flight to be kept, got {:?}",
            turns.items
        );
    }
}
