// SPDX-License-Identifier: CC0-1.0

//! The event source types the ONE `rds` front door publishes a category map for, and the
//! categories of each.
//!
//! Amazon RDS, Amazon DocumentDB and Amazon Neptune are three emulated views of that one
//! front door, and `DescribeEventCategories` answers every one of them from the SAME
//! published catalog — the same source types, in the same order, each with the same
//! category list — so the table lives here rather than once per view. A view that held its
//! own copy answered a client a catalog the front door does not publish, and the copies
//! drifted apart: one led with a different source type, one omitted the categories only a
//! cluster has, one sorted what the service publishes unsorted.
//!
//! The ORDER is the order the report publishes, entry for entry and category for category:
//! neither the model nor the API reference states a sort rule for either list, so the
//! sequence is the front door's own published one and a handler emits it unchanged — a
//! client that indexes this report positionally, or renders it as the ordered list it is,
//! reads the source type the service names at that position. It is NOT the order the
//! `SourceType` member's prose happens to name the types in, and not an alphabetical sort
//! of either list.
//!
//! The SETS are the ones published in "Amazon RDS event categories and event messages"
//! (Amazon RDS User Guide), one entry per source type this front door publishes a map for.

/// The event source types and their categories, as `DescribeEventCategories` reports them
/// for every view of this front door.
///
/// `db-instance` carries the two categories that table lists beyond the ones a DB instance
/// shares with the other source types — `backtrack` (an Aurora MySQL backtrack event) and
/// `security` — because a client builds a `CreateEventSubscription` `EventCategories`
/// argument out of this map and AWS accepts both for that source type;
/// `db-cluster-snapshot` carries `notification` beside `backup` for the same reason.
///
/// A source type a model's `SourceType` enum
/// (`smithy_query(query="shape",shape="SourceType")`) carries but that member's prose does
/// not list is published too, at the position the published report names it in:
/// `custom-engine-version` is one — the published custom-engine-version event table carries a
/// single `failure` event — and a client that subscribes to a source type has to be able to
/// read its categories from this one map. The three NEWER source types of that enum
/// (`blue-green-deployment`, `db-shard-group`, `zero-etl`) are published at the end of the
/// table, after the type the report names last, each with the categories the recorded report
/// publishes for it: the categories of a source type are recorded behavior, and a source
/// type this report leaves out is one a client concludes does not exist and cannot subscribe
/// to.
pub const EVENT_CATEGORIES: &[(&str, &[&str])] = &[
    (
        "db-snapshot",
        &["notification", "creation", "deletion", "restoration"],
    ),
    ("db-parameter-group", &["configuration change"]),
    (
        // The DB instance entry's own published sequence, which is NOT an alphabetical sort
        // of its categories: the report publishes each source type's categories in the order
        // that source type's own entry declares them, and this one declares them as the
        // published report names them.
        "db-instance",
        &[
            "backup",
            "deletion",
            "availability",
            "creation",
            "low storage",
            "restoration",
            "configuration change",
            "failover",
            "maintenance",
            "failure",
            "notification",
            "read replica",
            "recovery",
            "security",
            "backtrack",
            "security patching",
        ],
    ),
    ("db-security-group", &["configuration change", "failure"]),
    ("db-cluster-snapshot", &["backup", "notification"]),
    (
        "db-cluster",
        &[
            "failover",
            // The two categories only a CLUSTER publishes: it is migrated onto the
            // service (`migration`), and it runs serverless capacity (`serverless`).
            // Both are rows of the published DB cluster event table and both are
            // categories a cluster subscription may name.
            "migration",
            "failure",
            "notification",
            "serverless",
            "creation",
            "deletion",
            "maintenance",
            "configuration change",
            "global-failover",
        ],
    ),
    // The published custom-engine-version event table has one row, a failed CEV creation.
    ("custom-engine-version", &["failure"]),
    (
        "db-proxy",
        &["creation", "configuration change", "deletion", "failure"],
    ),
    // The newer source types of the `SourceType` enum, published after the ones above: a
    // blue/green deployment, a DB shard group and a zero-ETL integration each publish the
    // events of their own lifecycle.
    (
        "blue-green-deployment",
        &["creation", "failure", "deletion", "notification"],
    ),
    (
        "db-shard-group",
        &[
            "creation",
            "deletion",
            "availability",
            "maintenance",
            "failure",
            "notification",
        ],
    ),
    (
        "zero-etl",
        &["creation", "deletion", "failure", "notification"],
    ),
];

/// The event categories of `source_type`, or `None` when it names no published source
/// type. Matching is exact: the documented values are all lowercase.
pub fn event_categories_of(source_type: &str) -> Option<&'static [&'static str]> {
    EVENT_CATEGORIES
        .iter()
        .find(|(published, _)| *published == source_type)
        .map(|(_, categories)| *categories)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The catalog is published in ITS OWN order, not sorted: the report leads with the
    /// source type the front door names first, and each entry's categories keep the
    /// published sequence. A sorted entry would report a sequence the service never
    /// publishes, which is exactly how the per-view copies diverged — so an entry whose
    /// published sequence is not an alphabetical one must not be held in alphabetical order
    /// here, while an entry the service happens to publish in alphabetical order (`zero-etl`)
    /// keeps its own, which is why the check is that the table as a whole is not a sort.
    #[test]
    fn the_catalog_keeps_its_published_order() {
        let source_types: Vec<&str> = EVENT_CATEGORIES
            .iter()
            .map(|(source_type, _)| *source_type)
            .collect();
        assert_eq!(
            source_types.first().copied(),
            Some("db-snapshot"),
            "the report leads with db-snapshot, got {source_types:?}"
        );
        let mut sorted = source_types.clone();
        sorted.sort_unstable();
        assert_ne!(
            source_types, sorted,
            "the source-type sequence is the published one, not an alphabetical sort"
        );
        let unsorted = EVENT_CATEGORIES
            .iter()
            .filter(|(_, categories)| {
                let mut sorted_categories = categories.to_vec();
                sorted_categories.sort_unstable();
                categories.len() > 2 && *categories != sorted_categories.as_slice()
            })
            .count();
        assert!(
            unsorted > 1,
            "the category sequences are the published ones, not sorts: only {unsorted} entry \
             of {} is held unsorted",
            EVENT_CATEGORIES.len()
        );
    }

    /// Every source type is published once, and the lookup answers for each of them and for
    /// nothing else.
    #[test]
    fn every_published_source_type_resolves_once() {
        let mut seen: Vec<&str> = Vec::new();
        for (source_type, categories) in EVENT_CATEGORIES {
            assert!(
                !seen.contains(source_type),
                "{source_type}: published twice"
            );
            seen.push(source_type);
            assert_eq!(
                event_categories_of(source_type),
                Some(*categories),
                "{source_type}: the lookup must answer with this entry's categories"
            );
            assert!(
                !categories.is_empty(),
                "{source_type}: a published map carries at least one category"
            );
        }
        for absent in ["db-cluster-group", "db-shard", "DB-CLUSTER", ""] {
            assert_eq!(
                event_categories_of(absent),
                None,
                "{absent:?}: no map is published for it, so the lookup answers None"
            );
        }
    }
}
