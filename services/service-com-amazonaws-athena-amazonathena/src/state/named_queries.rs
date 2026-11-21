// SPDX-License-Identifier: CC0-1.0

//! The named (saved) queries each workgroup of this account holds.
//!
//! `CreateNamedQuery` writes a [`NamedQueryRecord`] into the [`NamedQueryRegistry`] on this
//! service's store, `UpdateNamedQuery` revises that same record in place
//! ([`NamedQueryRecord::revise`]) and `GetNamedQuery` answers out of it, so every value a
//! read reports is one some earlier write persisted rather than one derived from the
//! reading request.
//!
//! A named query is addressed by the `NamedQueryId` the create minted, which is why the
//! registry is keyed by that id; the workgroup it belongs to is held ON the record, because
//! `CreateNamedQueryInput$WorkGroup` is "the name of the workgroup in which the named query
//! is being created" and `ListNamedQueries` lists "the saved queries for the primary
//! workgroup" when a request names none — so a named query always belongs to exactly one
//! workgroup, and that membership is a VALUE on the record rather than the query's address.
//! Nothing but [`NamedQueryRegistry::remove`] takes a query out of the registry: a delete of
//! the workgroup it names removes the workgroup, and the saved queries keep resolving under
//! their own ids, reporting the workgroup they were saved in.
//!
//! There is no lifecycle the clock owes one: Athena answers a create with a query a client
//! can already read back, so the record is stored in its terminal shape and a read never has
//! to materialise a pending transition. A named query carries no instant at all — the
//! modelled `NamedQuery` shape declares none.

use std::collections::BTreeMap;

use crate::ops::types::{NamedQuery, UnprocessedNamedQueryId};

/// How many named queries one workgroup holds in this emulator.
///
/// The EMULATOR's own bound, with no counterpart in the model: it is only what keeps a
/// sequence of creates from growing the emulator's heap without limit. A create past it is
/// refused with the operation's own `InvalidRequestException`.
pub const MAX_NAMED_QUERIES_PER_WORK_GROUP: usize = 1000;

/// How many bytes of named queries this account holds in this emulator, across every
/// workgroup.
///
/// The emulator's own bound as well, and the one that makes the held total a bounded
/// product: the count above is PER workgroup and this service admits
/// [`super::MAX_WORK_GROUPS`] of them, while one query's `QueryString` is modelled up to
/// 262144 bytes, so without a total the reachable heap would be the product of all three.
/// The same shape as [`super::MAX_PREPARED_STATEMENT_BYTES_TOTAL`] beside its per-workgroup
/// count. A write past it is refused with the operation's own `InvalidRequestException`.
pub const MAX_NAMED_QUERY_BYTES_TOTAL: usize = 32 * 1024 * 1024;

/// One named query, as this service persists it.
#[derive(Debug, Clone)]
pub struct NamedQueryRecord {
    /// The `NamedQueryId` this query was minted under, which is also its key in the
    /// registry. Reported as `NamedQuery$NamedQueryId`.
    pub id: String,
    /// The query name the create carried, reported as `NamedQuery$Name`.
    pub name: String,
    /// The description the create carried, if any, reported as `NamedQuery$Description`.
    /// `None` for a create that carried none, which is what keeps an absent description
    /// absent on the way back out rather than reported as an empty one.
    pub description: Option<String>,
    /// The database the query belongs to, reported as `NamedQuery$Database`.
    pub database: String,
    /// The statements the query stands for, reported as `NamedQuery$QueryString`.
    pub query_string: String,
    /// The workgroup the query belongs to — the one the create named, or the account's
    /// default workgroup when it named none. Reported as `NamedQuery$WorkGroup`.
    pub work_group: String,
}

impl NamedQueryRecord {
    /// Write `name`, `query_string` and `description` onto the query, as an update does.
    ///
    /// The database and the workgroup are NOT touched: "The database or workgroup cannot be
    /// updated" (`UpdateNamedQuery`), so a query stays in the database and the workgroup its
    /// create put it in. The description is REPLACED rather than merged, because the update
    /// carries the whole query: one called without a description leaves the query with none,
    /// exactly as a create without one does.
    pub fn revise(&mut self, name: String, query_string: String, description: Option<String>) {
        self.name = name;
        self.query_string = query_string;
        self.description = description;
    }

    /// The record as the modelled `NamedQuery` shape a read answers with.
    pub fn to_named_query(&self) -> NamedQuery {
        NamedQuery {
            name: self.name.clone(),
            description: self.description.clone(),
            database: self.database.clone(),
            query_string: self.query_string.clone(),
            named_query_id: Some(self.id.clone()),
            work_group: Some(self.work_group.clone()),
        }
    }

    /// How many bytes of caller-supplied text this record holds — the size
    /// [`MAX_NAMED_QUERY_BYTES_TOTAL`] bounds the sum of.
    pub fn stored_bytes(&self) -> usize {
        named_query_bytes(
            &self.name,
            &self.database,
            &self.query_string,
            self.description.as_deref(),
        )
    }
}

/// How many bytes a named query written with these values holds, so a write can be measured
/// BEFORE the record is built and the registry grown.
pub fn named_query_bytes(
    name: &str,
    database: &str,
    query_string: &str,
    description: Option<&str>,
) -> usize {
    name.len()
        .saturating_add(database.len())
        .saturating_add(query_string.len())
        .saturating_add(description.map_or(0, str::len))
}

/// Every named query this account holds, and the create tokens that already minted one.
///
/// A struct of its own rather than loose fields on the store, so the bounds above and the
/// scoping by workgroup are applied in the ONE place a query is written and the ONE place
/// one is looked up, and cannot be honoured on one operation's path and skipped on
/// another's.
#[derive(Debug, Default)]
pub struct NamedQueryRegistry {
    /// The queries, keyed by `NamedQueryId`. A `BTreeMap` so a walk of the registry is in
    /// one stable order — the order a listing of one workgroup's queries pages through,
    /// which must not depend on how the registry was filled.
    queries: BTreeMap<String, NamedQueryRecord>,
    /// Which `NamedQueryId` each `ClientRequestToken` already minted, so the documented
    /// idempotency of a create ("the same response is returned and another query is not
    /// created") is answered from state rather than by creating twice. An entry is dropped
    /// with the query it names.
    create_tokens: BTreeMap<String, String>,
}

impl NamedQueryRegistry {
    /// The query `id` names, or `None` when this account holds none.
    pub fn query(&self, id: &str) -> Option<&NamedQueryRecord> {
        self.queries.get(id)
    }

    /// The same query, for a caller that is going to write it (an update).
    pub fn query_mut(&mut self, id: &str) -> Option<&mut NamedQueryRecord> {
        self.queries.get_mut(id)
    }

    /// The query a `ClientRequestToken` already minted, if it did.
    pub fn query_of_token(&self, token: &str) -> Option<&NamedQueryRecord> {
        self.queries.get(self.create_tokens.get(token)?)
    }

    /// The ids of the queries `work_group` holds, in registry order.
    pub fn ids_in(&self, work_group: &str) -> Vec<&str> {
        self.queries
            .values()
            .filter(|record| record.work_group == work_group)
            .map(|record| record.id.as_str())
            .collect()
    }

    /// How many queries `work_group` holds — the count
    /// [`MAX_NAMED_QUERIES_PER_WORK_GROUP`] bounds, and the count a delete of that
    /// workgroup weighs.
    pub fn len_in(&self, work_group: &str) -> usize {
        self.queries
            .values()
            .filter(|record| record.work_group == work_group)
            .count()
    }

    /// How many bytes of named queries this account holds, the total
    /// [`MAX_NAMED_QUERY_BYTES_TOTAL`] bounds.
    pub fn stored_bytes(&self) -> usize {
        self.queries.values().fold(0usize, |bytes, record| {
            bytes.saturating_add(record.stored_bytes())
        })
    }

    /// Whether a create of `query_bytes` would EXCEED the bytes this account may hold. The
    /// count a workgroup may hold is the create's other bound ([`Self::len_in`]), reported
    /// with its own sentence.
    pub fn would_exceed_stored_bytes(&self, query_bytes: usize) -> bool {
        self.stored_bytes_would_exceed(query_bytes, 0)
    }

    /// Whether an UPDATE writing `query_bytes` over the query `id` names would EXCEED that
    /// same bound.
    ///
    /// The bytes it overwrites are freed by the same write, so they are not counted twice;
    /// the count bound is not in question, an update adding no query.
    pub fn would_exceed_stored_bytes_on_revise(&self, id: &str, query_bytes: usize) -> bool {
        let freed = self
            .query(id)
            .map(NamedQueryRecord::stored_bytes)
            .unwrap_or_default();
        self.stored_bytes_would_exceed(query_bytes, freed)
    }

    /// Whether holding `added` more bytes, once `freed` are released, EXCEEDS
    /// [`MAX_NAMED_QUERY_BYTES_TOTAL`].
    fn stored_bytes_would_exceed(&self, added: usize, freed: usize) -> bool {
        self.stored_bytes().saturating_sub(freed).saturating_add(added) > MAX_NAMED_QUERY_BYTES_TOTAL
    }

    /// Store `record`, under `token` when the request carried one.
    ///
    /// The caller has already asked the bounds above; this is the one place a query enters
    /// the registry, so the token index cannot come to name a query the map does not hold.
    pub fn insert(&mut self, record: NamedQueryRecord, token: Option<String>) {
        if let Some(token) = token {
            self.create_tokens.insert(token, record.id.clone());
        }
        self.queries.insert(record.id.clone(), record);
    }

    /// Remove the query `id` names, as `DeleteNamedQuery` does, and answer with it when
    /// there was one.
    ///
    /// Any create token that named it goes with it: a token whose query is gone cannot
    /// answer with the same id, so it is not kept pointing at nothing.
    pub fn remove(&mut self, id: &str) -> Option<NamedQueryRecord> {
        let removed = self.queries.remove(id)?;
        self.create_tokens.retain(|_, minted| minted != id);
        Some(removed)
    }

}

/// A fresh `NamedQueryId`: a version-4 UUID in its hyphenated, lower-case form, which is
/// the form Athena mints a saved query's id in and satisfies the modelled `NamedQueryId`
/// (1 to 128 characters, `^\S+$`). Minted from the maintained crate rather than laid out
/// here, beside [`super::new_query_execution_id`].
pub fn new_named_query_id() -> String {
    ::uuid::Uuid::new_v4().as_hyphenated().to_string()
}

/// The message an operation addressing a named query this account holds none of answers
/// with: a read of an id no `CreateNamedQuery` minted, a read of one a `DeleteNamedQuery`
/// took, and that delete itself.
///
/// Amazon Athena's own wording for this error class: the fixed text is `NamedQuery: ` and
/// ` does not exist`, and the one variable part is the `NamedQueryId` the CALLER supplied. The
/// ONE place it is worded, so the read of a single query and the per-id report a batch read
/// carries for an id it could not resolve say the same thing about the same miss.
pub fn named_query_not_found(id: &str) -> String {
    format!("NamedQuery: {id} does not exist")
}

/// The error code a miss of a `NamedQueryId` is classified under: the id is well formed and
/// this account simply holds no saved query behind it.
///
/// Its own class rather than [`super::ERROR_CODE_INVALID_INPUT`], which is for a request the
/// caller must correct (a malformed id, a value out of range): a client branching on the
/// class of one id's failure can tell "this saved query does not exist" — drop the id, or
/// create the query again — from "your request is wrong". The same shape as
/// [`super::ERROR_CODE_QUERY_EXECUTION_NOT_FOUND`] and
/// [`super::ERROR_CODE_STATEMENT_NOT_FOUND`] for the other resources a batch read resolves
/// by id.
pub const ERROR_CODE_NAMED_QUERY_NOT_FOUND: &str = "NAMED_QUERY_NOT_FOUND";

/// How a batch read of named queries reports ONE requested id it could not resolve.
///
/// A batch read answers in two parts rather than refusing the whole request, and the entry
/// it reports an id under carries the CLASS of that id's failure under `ErrorCode`, which is
/// what a client branches on for a partial answer. The two classes are decided one before
/// the other:
///
/// * an id that is not a named query identifier at all — one outside the modelled
///   `NamedQueryId`, 1 to 128 characters matching `^\S+$` — is the request itself being
///   wrong, so the whole request is refused with the class a bad request carries
///   ([`super::ERROR_CODE_INVALID_INPUT`]) before any handler sees it (see
///   [`crate::service::input_validation`]);
/// * an id that IS one and that this account holds no saved query under failed only at
///   RESOLVING, which is its own class ([`ERROR_CODE_NAMED_QUERY_NOT_FOUND`]).
///
/// So an id reaching this far is well formed and the entry states the miss, in the one
/// sentence a miss of that id is stated in ([`named_query_not_found`]). The ONE place such
/// an entry is built, beside [`super::unprocessed_query_execution`], so every batch read of
/// saved queries classifies a miss the same way.
pub fn unprocessed_named_query(id: &str) -> UnprocessedNamedQueryId {
    UnprocessedNamedQueryId {
        named_query_id: Some(id.to_string()),
        error_code: Some(ERROR_CODE_NAMED_QUERY_NOT_FOUND.to_string()),
        error_message: Some(named_query_not_found(id)),
    }
}

/// The refusal message for a create that would take `work_group` past
/// [`MAX_NAMED_QUERIES_PER_WORK_GROUP`].
pub fn too_many_named_queries(work_group: &str) -> String {
    format!(
        "Cannot create another named query: WorkGroup {work_group} already holds the \
         maximum of {MAX_NAMED_QUERIES_PER_WORK_GROUP} named queries."
    )
}

/// The refusal message for a write — a create, or an update revising a query — that would
/// take the account past [`MAX_NAMED_QUERY_BYTES_TOTAL`].
pub fn named_queries_too_large() -> String {
    format!(
        "Cannot store this named query: this account already holds the maximum of \
         {MAX_NAMED_QUERY_BYTES_TOTAL} bytes of named queries."
    )
}
