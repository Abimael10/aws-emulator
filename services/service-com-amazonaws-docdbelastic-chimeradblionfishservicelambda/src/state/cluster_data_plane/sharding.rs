// SPDX-License-Identifier: CC0-1.0

//! SHARDING on an elastic cluster: `shardCollection`, the shard key such a cluster admits, and
//! the rule that every document written to a sharded collection carries that key.
//!
//! An elastic cluster distributes a collection's documents over its shards, and what decides
//! which shard a document belongs to is the collection's SHARD KEY. A client shards a
//! collection by sending `shardCollection` with the key to shard it on
//! (`sh.shardCollection("<database>.<collection>", { "<field>": "hashed" })`), and the key an
//! elastic cluster admits is exactly one field, hashed: hash-based sharding is the one
//! strategy such a cluster has, and a hash is computed over one field's value. A key of any
//! other shape is refused rather than recorded, so no collection is ever reported sharded on a
//! key the cluster could not distribute by.
//!
//! The key is what every later write of that collection is judged against: a document carrying
//! no value at the key's path belongs to no shard, so the write is refused — the documents an
//! `insert` carries and the document an upserting `update` statement would insert alike, which
//! are the two ways a document of a collection comes into being on this wire. A collection
//! nobody sharded is not judged at all — a collection is unsharded until a client shards it,
//! and writes to it are ordinary writes.
//!
//! The keys live with the cluster's documents ([`super::ClusterStorage`]), because which
//! collections are sharded is part of what the cluster holds: they go when the cluster does,
//! and when the collection or its database is dropped.

use std::collections::BTreeMap;

use ::bson::{Bson, Document};
use ::service_com_amazonaws_docdb_amazonrdsv19::state::cluster_data_plane as document_engine;

use document_engine::query;

use super::refusal::{self, Refusal};

/// The value a key's field carries to name hash-based sharding, which is the one sharding
/// strategy an elastic cluster has.
pub const HASHED: &str = "hashed";

/// How many fields the key of a sharded collection has on an elastic cluster: one, because
/// the shard a document belongs to is the hash of one field's value.
pub const SHARD_KEY_FIELDS: usize = 1;

/// The code every refusal about a collection's shard key carries, and the name the engine
/// publishes for it: the key the collection needs was not found — neither in the key a
/// `shardCollection` named nor in a document a write carried.
const SHARD_KEY_CODE: i32 = 61;

/// The name of [`SHARD_KEY_CODE`].
const SHARD_KEY_CODE_NAME: &str = "ShardKeyNotFound";

/// The shard key of each sharded collection of one cluster, by `<database>.<collection>`.
#[derive(Debug, Default)]
pub struct ShardKeys {
    sharded: BTreeMap<String, String>,
}

impl ShardKeys {
    /// The field the collection `namespace` is sharded on, or `None` for a collection nobody
    /// sharded.
    #[must_use]
    pub fn key_of(&self, namespace: &str) -> Option<&str> {
        self.sharded.get(namespace).map(String::as_str)
    }

    /// Record that `namespace` is sharded on `field`.
    pub fn shard(&mut self, namespace: &str, field: &str) {
        self.sharded
            .insert(namespace.to_string(), field.to_string());
    }

    /// Forget the collection `namespace`, which dropping that collection does.
    pub fn forget(&mut self, namespace: &str) {
        self.sharded.remove(namespace);
    }

    /// Forget every collection of `database`, which dropping that database does.
    pub fn forget_database(&mut self, database: &str) {
        let prefix = format!("{database}.");
        self.sharded
            .retain(|namespace, _field| !namespace.starts_with(&prefix));
    }

    /// How many collections are sharded, which is what bounds this map: one entry per
    /// collection the cluster's volume holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.sharded.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.sharded.is_empty()
    }
}

/// The `<database>.<collection>` a command names, split into its two halves.
///
/// Both halves have to be there: a value that names no collection inside a database is not a
/// namespace at all, whatever else it is.
pub fn split_namespace(namespace: &str) -> Result<(String, String), Refusal> {
    let (database, collection) = namespace
        .split_once('.')
        .ok_or_else(|| refusal::invalid_namespace(namespace))?;
    if database.is_empty() || collection.is_empty() {
        return Err(refusal::invalid_namespace(namespace));
    }
    Ok((database.to_string(), collection.to_string()))
}

/// The ONE field a shard key names, for a key of the shape an elastic cluster admits — exactly
/// [`SHARD_KEY_FIELDS`] field whose value is [`HASHED`].
///
/// Every other key is refused: a key over two fields, a key naming an ascending or descending
/// order (which is range-based sharding, which such a cluster does not have), and a key that
/// is not a document at all.
pub fn hashed_field(key: &Document) -> Result<String, Refusal> {
    let mut fields = key.iter();
    let Some((field, value)) = fields.next() else {
        return Err(unsupported_shard_key(key));
    };
    if fields.next().is_some() {
        return Err(unsupported_shard_key(key));
    }
    let hashed = matches!(value, Bson::String(strategy) if strategy == HASHED);
    if !hashed || field.is_empty() {
        return Err(unsupported_shard_key(key));
    }
    // The path is bounded exactly as every other path this engine walks is.
    query::path_segments(field).map_err(Refusal::from)?;
    Ok(field.clone())
}

/// `shardCollection`: shard the collection the command names on the key it carries, and answer
/// as the engine does — the namespace that is now sharded.
///
/// The collection is created where it does not exist yet, which is what sharding a namespace
/// the cluster does not hold yet does, and the key is recorded only once the collection is
/// there: a refused create (the cluster's own collection or database ceiling) leaves nothing
/// sharded.
///
/// Sharding a collection that is ALREADY sharded on the same field is the answer it already
/// has; on another field it is refused, because the documents the cluster holds were
/// distributed by the key it has.
pub fn shard_collection(
    volume: &mut document_engine::documents::Volume,
    keys: &mut ShardKeys,
    command: &Document,
    argument: &Bson,
) -> Result<Document, Refusal> {
    let namespace = match argument {
        Bson::String(namespace) => namespace.clone(),
        _ => {
            return Err(refusal::failed_to_parse(
                "shardCollection needs the namespace to shard",
            ))
        }
    };
    let (database, collection) = split_namespace(&namespace)?;
    let Some(Bson::Document(key)) = command.get("key") else {
        return Err(refusal::failed_to_parse("shardCollection needs a key"));
    };
    let field = hashed_field(key)?;
    if let Some(sharded_on) = keys.key_of(&namespace) {
        if sharded_on != field {
            return Err(already_sharded(&namespace, sharded_on));
        }
        return Ok(::bson::doc! { "collectionsharded": namespace });
    }
    // A namespace the cluster does not hold yet comes into being here; one it already holds is
    // left exactly as it is, documents and all.
    if volume.collection(&database, &collection).is_none() {
        volume
            .create_collection(&database, &collection)
            .map_err(Refusal::from)?;
    }
    keys.shard(&namespace, &field);
    Ok(::bson::doc! { "collectionsharded": namespace })
}

/// Whether every document a write stores in `namespace` carries that collection's shard key.
///
/// A collection nobody sharded is not judged: the write is an ordinary one. A sharded
/// collection's documents each belong to the shard its key hashes to, so a document carrying
/// no value at that key's path belongs to no shard and is refused — the whole command is,
/// before any of its documents is stored, so a write of several documents does not store some
/// of them and refuse the rest.
pub fn documents_carry_the_shard_key(
    keys: &ShardKeys,
    namespace: &str,
    documents: &[Bson],
) -> Result<(), Refusal> {
    let Some(field) = keys.key_of(namespace) else {
        return Ok(());
    };
    for value in documents {
        let Bson::Document(document) = value else {
            continue;
        };
        if query::value_at(document, field).is_none() {
            return Err(shard_key_missing(field));
        }
    }
    Ok(())
}

/// Whether the document an UPSERTING statement would insert carries the collection's shard key.
///
/// A statement with `upsert: true` inserts a document when its filter selects none, and that
/// document is as much a document of the collection as one an `insert` carries — so the key
/// rule holds for it too. What such a statement would insert is derived from its filter and its
/// update ([`query::upserted_document`]), which is what is judged here, before the statement
/// runs.
///
/// A statement that upserts into a collection nobody sharded is not judged, exactly as an
/// ordinary write of one is not.
pub fn upsert_carries_the_shard_key(
    keys: &ShardKeys,
    namespace: &str,
    filter: &Document,
    update: &Document,
) -> Result<(), Refusal> {
    let Some(field) = keys.key_of(namespace) else {
        return Ok(());
    };
    let would_insert = query::upserted_document(filter, update).map_err(Refusal::from)?;
    if query::value_at(&would_insert, field).is_none() {
        return Err(shard_key_missing(field));
    }
    Ok(())
}

/// A `shardCollection` whose key is not one an elastic cluster can distribute a collection by.
///
/// The engine answers a shard key it will not shard on with the same code it answers a write
/// carrying no shard key with ([`shard_key_missing`]): both are the one shard key the
/// collection needs and does not have.
#[must_use]
fn unsupported_shard_key(key: &Document) -> Refusal {
    Refusal {
        code: SHARD_KEY_CODE,
        code_name: Some(SHARD_KEY_CODE_NAME),
        message: format!("Shard key {} must be hashed", query::render_key(key)),
    }
}

/// A `shardCollection` of a collection already sharded on another field.
#[must_use]
fn already_sharded(namespace: &str, field: &str) -> Refusal {
    Refusal {
        code: 72,
        code_name: Some("InvalidOptions"),
        message: format!(
            "sharding already enabled for collection {namespace} with shard key \
             {{ {field}: \"{HASHED}\" }}"
        ),
    }
}

/// A write whose document carries no value at the collection's shard key.
#[must_use]
fn shard_key_missing(field: &str) -> Refusal {
    Refusal {
        code: SHARD_KEY_CODE,
        code_name: Some(SHARD_KEY_CODE_NAME),
        message: format!("Document does not contain the shard key field \"{field}\""),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the shard key an elastic cluster admits is exactly one field whose value is
    /// `hashed`; every other key is refused.
    ///
    /// The rows span what the rule generalises over: how many fields the key names, what
    /// strategy each names (the hashed one, a range order in either direction, a type that is
    /// no strategy at all) and the field's own form (a plain name, a path).
    #[test]
    fn the_shard_key_is_one_hashed_field() {
        let rows: Vec<(&str, Document, Option<&str>)> = vec![
            ("one hashed field", ::bson::doc! { "tenant": HASHED }, Some("tenant")),
            (
                "one hashed field named by a path",
                ::bson::doc! { "owner.tenant": HASHED },
                Some("owner.tenant"),
            ),
            ("an ascending field", ::bson::doc! { "tenant": 1_i32 }, None),
            ("a descending field", ::bson::doc! { "tenant": -1_i32 }, None),
            (
                "two hashed fields",
                ::bson::doc! { "tenant": HASHED, "region": HASHED },
                None,
            ),
            (
                "a hashed field beside a range one",
                ::bson::doc! { "tenant": HASHED, "region": 1_i32 },
                None,
            ),
            ("no field at all", Document::new(), None),
            (
                "a strategy that is not a string",
                ::bson::doc! { "tenant": true },
                None,
            ),
            (
                "a strategy spelled otherwise",
                ::bson::doc! { "tenant": "HASHED" },
                None,
            ),
        ];
        for (case, key, expected) in rows {
            let read = hashed_field(&key);
            match expected {
                Some(field) => assert_eq!(
                    read.as_deref().ok(),
                    Some(field),
                    "case {case}: expected the key to name {field}, got {read:?}"
                ),
                None => assert!(
                    read.is_err(),
                    "case {case}: expected the key to be refused, got {read:?}"
                ),
            }
        }
    }

    /// RULE: a write of a SHARDED collection carries the shard key on every document it
    /// stores; a collection nobody sharded is not judged.
    ///
    /// The rows span the key's form (a plain field, a path), whether each document carries it,
    /// how many documents the write carries, and whether the collection is sharded at all.
    #[test]
    fn every_document_of_a_sharded_write_carries_the_key() {
        let mut keys = ShardKeys::default();
        keys.shard("shop.orders", "tenant");
        keys.shard("shop.events", "owner.tenant");
        let rows: Vec<(&str, &str, Vec<Document>, bool)> = vec![
            (
                "one document carrying the key",
                "shop.orders",
                vec![::bson::doc! { "tenant": "acme", "total": 2_i32 }],
                true,
            ),
            (
                "several documents, all carrying it",
                "shop.orders",
                vec![
                    ::bson::doc! { "tenant": "acme" },
                    ::bson::doc! { "tenant": "globex" },
                ],
                true,
            ),
            (
                "one of several documents missing it",
                "shop.orders",
                vec![
                    ::bson::doc! { "tenant": "acme" },
                    ::bson::doc! { "total": 1_i32 },
                ],
                false,
            ),
            (
                "a key named by a path, carried",
                "shop.events",
                vec![::bson::doc! { "owner": { "tenant": "acme" } }],
                true,
            ),
            (
                "a key named by a path, missing",
                "shop.events",
                vec![::bson::doc! { "owner": { "region": "eu" } }],
                false,
            ),
            (
                "a collection nobody sharded",
                "shop.carts",
                vec![::bson::doc! { "total": 1_i32 }],
                true,
            ),
            ("no documents at all", "shop.orders", Vec::new(), true),
        ];
        for (case, namespace, documents, expected) in rows {
            let documents: Vec<Bson> = documents.into_iter().map(Bson::Document).collect();
            let decided = documents_carry_the_shard_key(&keys, namespace, &documents);
            assert_eq!(
                decided.is_ok(),
                expected,
                "case {case}: expected the write to be admitted {expected}, got {decided:?}"
            );
        }
    }
}
