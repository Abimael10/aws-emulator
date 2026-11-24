// SPDX-License-Identifier: CC0-1.0

//! WHICH list a paginated read is paging over.
//!
//! A `nextToken` names a position in ONE list, so what a page's token has to
//! carry is the identity of that list: the operation that answered the page, and
//! the request members that decide which of the lists that operation can answer
//! the page was cut out of — the grouping, the narrowing, the ordering and the
//! period or account scope. An operation's name alone is not that identity: the
//! same operation answers a different list for every value of those members, and
//! a position in one of them names no row of another.
//!
//! A read builds its identity here, member by member, and hands it to
//! [`super::paginate`] (through [`crate::service::page`]), which puts its digest
//! in every token it issues and accepts a token only for the identity it names.
//! The members that bound or resume the page itself — `maxResults` and
//! `nextToken` — are not part of it: they say where in a list to read, not which
//! list, so a caller may walk one list with a different page size per call.

use ::serde_json::{json, Value};
use ::sha2::{Digest, Sha256};

use crate::ops::types::{Filter, OrderBy, TimePeriod};

/// The identity of the list a paginated read answers a page of: the members the
/// read names here, each with the value the request carries for it, in the order
/// the read adds them.
///
/// Rendered as a nested array rather than as prose, so no member's value can
/// spell another member's rendering and two different requests cannot share one
/// identity.
#[derive(Debug, Default)]
pub struct ListScope {
    members: Vec<Value>,
}

impl ListScope {
    /// The identity of a list selected by no member at all — one an operation
    /// answers in exactly one form — which a read then adds its own selecting
    /// members to.
    pub fn new() -> Self {
        Self::default()
    }

    /// A member carrying one string, or carrying nothing: an absent member
    /// selects a different list from any value of it, so the two are rendered
    /// apart.
    pub fn text(self, member: &str, value: Option<&str>) -> Self {
        self.member(member, json!(value))
    }

    /// A member carrying a flag the request always resolves (a `@default`ed
    /// boolean).
    pub fn flag(self, member: &str, value: bool) -> Self {
        self.member(member, json!(value))
    }

    /// A `filter`: every member of it narrows the list, so every member of it is
    /// part of which list this is.
    pub fn filter(self, member: &str, filter: Option<&Filter>) -> Self {
        self.member(member, filter.map_or(Value::Null, filter_value))
    }

    /// An `orderBy`: the order the rows are in decides which of them a position
    /// names, so it is part of which list this is.
    pub fn order_by(self, member: &str, order_by: Option<&OrderBy>) -> Self {
        self.member(member, order_by.map_or(Value::Null, order_by_value))
    }

    /// A `timePeriod`: the interval the read reports over.
    pub fn time_period(self, member: &str, period: &TimePeriod) -> Self {
        self.member(
            member,
            json!([["start", &period.start], ["end", &period.end]]),
        )
    }

    /// The identity's digest, which is what a token carries: a fixed-width,
    /// opaque stand-in for the members, so a token tells its holder nothing
    /// about the request that minted it and stays the same length however much
    /// that request narrowed.
    pub(super) fn digest(&self) -> String {
        let rendering = Value::Array(self.members.clone()).to_string();
        ::hex::encode(Sha256::digest(rendering.as_bytes()))
    }

    fn member(mut self, member: &str, value: Value) -> Self {
        self.members.push(json!([member, value]));
        self
    }
}

/// Every member of a `Filter`, each named as the model spells it
/// (`smithy_query(query="shape", shape="Filter")`), so a filter that narrows the
/// list differently is a different list — including through the members an
/// operation refuses, which never reach a page.
fn filter_value(filter: &Filter) -> Value {
    json!([
        ["accountIds", json!(&filter.account_ids)],
        ["actionTypes", json!(&filter.action_types)],
        [
            "implementationEfforts",
            json!(&filter.implementation_efforts)
        ],
        ["recommendationIds", json!(&filter.recommendation_ids)],
        ["regions", json!(&filter.regions)],
        ["resourceArns", json!(&filter.resource_arns)],
        ["resourceIds", json!(&filter.resource_ids)],
        ["resourceTypes", json!(&filter.resource_types)],
        ["restartNeeded", json!(&filter.restart_needed)],
        ["rollbackPossible", json!(&filter.rollback_possible)],
        ["tags", tags_value(filter)],
    ])
}

/// A filter's `tags`, each as the pair it is: `Tag` carries no `Serialize`, so
/// the two members are rendered the way every other member here is.
fn tags_value(filter: &Filter) -> Value {
    match &filter.tags {
        None => Value::Null,
        Some(tags) => Value::Array(
            tags.iter()
                .map(|tag| json!([["key", &tag.key], ["value", &tag.value]]))
                .collect(),
        ),
    }
}

/// Both members of an `OrderBy` (`smithy_query(query="shape", shape="OrderBy")`):
/// which values the rows are ordered by, and in which direction.
fn order_by_value(order_by: &OrderBy) -> Value {
    json!([
        ["dimension", &order_by.dimension],
        ["order", &order_by.order],
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::types::Tag;

    /// A tag the cases narrow by, and a second one differing from it only in its
    /// value, so the pair pins that a nested member is part of the identity.
    fn tag(key: &str, value: &str) -> Tag {
        Tag {
            key: Some(key.to_string()),
            value: Some(value.to_string()),
        }
    }

    /// Two identities built from the same members and values digest the same —
    /// so a token a list issued is accepted on a later read of that same list —
    /// and identities differing in any one member digest apart.
    #[test]
    fn an_identity_digests_to_itself_and_apart_from_every_other() {
        let base = || ListScope::new().text("groupBy", Some("ActionType"));
        assert_eq!(
            base().digest(),
            base().digest(),
            "case same members: expected one digest for one identity"
        );

        let mut others: Vec<(&str, String)> = vec![
            (
                "another groupBy",
                ListScope::new().text("groupBy", Some("Region")).digest(),
            ),
            ("no groupBy", ListScope::new().text("groupBy", None).digest()),
            ("no member at all", ListScope::new().digest()),
            (
                "the same value under another member",
                ListScope::new().text("granularity", Some("ActionType")).digest(),
            ),
            (
                "a filter beside it",
                base()
                    .filter("filter", Some(&Filter::default()))
                    .digest(),
            ),
        ];
        let filter = |tags: Vec<Tag>| Filter {
            tags: Some(tags),
            ..Filter::default()
        };
        others.push((
            "a filter narrowing by one tag",
            base()
                .filter("filter", Some(&filter(vec![tag("env", "prod")])))
                .digest(),
        ));
        others.push((
            "a filter narrowing by that tag's key with another value",
            base()
                .filter("filter", Some(&filter(vec![tag("env", "test")])))
                .digest(),
        ));
        others.push((
            "an ordering beside it",
            base()
                .order_by(
                    "orderBy",
                    Some(&OrderBy {
                        dimension: Some("Region".to_string()),
                        order: Some("Desc".to_string()),
                    }),
                )
                .digest(),
        ));

        let base = base().digest();
        for (case, digest) in &others {
            assert_ne!(
                *digest, base,
                "case {case:?}: expected an identity of its own, got the base identity's digest"
            );
        }
        for (case, digest) in &others {
            let shared = others
                .iter()
                .filter(|(other, _)| other != case)
                .any(|(_, other)| other == digest);
            assert!(
                !shared,
                "case {case:?}: expected a digest no other identity has, got {digest}"
            );
        }
    }
}
