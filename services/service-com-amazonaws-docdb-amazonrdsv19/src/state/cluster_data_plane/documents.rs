// SPDX-License-Identifier: CC0-1.0

//! The DOCUMENTS one cluster holds: its databases, their collections, the indexes over them
//! and the documents themselves.
//!
//! This is the cluster's storage, shared by every endpoint of it — "each Amazon DocumentDB
//! cluster hosts copies of its data", so a document a client writes through one member is the
//! document another member reads. The store is in this process and in memory only: nothing
//! here reaches a host path, a socket or another process.
//!
//! What it owns is the DATA and the rules that belong to the data — which documents a write
//! may add (a unique index already holding a key refuses one), what a read selects
//! ([`super::query`]) and the bounds on how much one cluster may hold. WHICH session may ask
//! is the session's business ([`super::commands`]).
//!
//! # Bounds
//!
//! Every bound below is THIS emulator's, not the engine's, and is chosen so one client
//! cannot size this process's memory. A command refused by one of them says which bound it
//! was ([`super::fault::over_capacity`]), so a client can tell it from a defect.

use std::collections::BTreeMap;

use ::bson::{Bson, Document};

use super::fault::{self, WireFault};
use super::query;

/// The largest document the engine stores, which it publishes as `maxBsonObjectSize` in its
/// handshake: 16 MiB.
pub const MAX_BSON_OBJECT_SIZE: i32 = 16 * 1024 * 1024;

/// How many databases one cluster's volume may hold. CHOSEN as a process bound.
pub const MAX_DATABASES: usize = 16;

/// How many collections one database may hold. CHOSEN as a process bound.
pub const MAX_COLLECTIONS_PER_DATABASE: usize = 64;

/// How many documents one collection may hold. CHOSEN as a process bound.
pub const MAX_DOCUMENTS_PER_COLLECTION: usize = 20_000;

/// How many indexes one collection may carry, beside the `_id` index every collection has.
/// CHOSEN as a process bound, matching the engine's own ceiling of 64 indexes per collection.
pub const MAX_INDEXES_PER_COLLECTION: usize = 64;

/// The name of the index every collection has over its `_id` field, which the engine builds
/// with the collection and which makes `_id` unique.
pub const ID_INDEX_NAME: &str = "_id_";

/// One index over a collection.
#[derive(Debug, Clone, PartialEq)]
pub struct IndexSpec {
    /// `name`, as `createIndexes` named it or as it was derived from the key.
    pub name: String,
    /// `key`: the fields and their directions.
    pub key: Document,
    /// Whether two documents may share a key.
    pub unique: bool,
}

impl IndexSpec {
    /// The index over `_id` every collection is created with.
    #[must_use]
    pub fn id_index() -> Self {
        Self {
            name: ID_INDEX_NAME.to_string(),
            key: ::bson::doc! { "_id": 1_i32 },
            unique: true,
        }
    }

    /// The name the engine derives for an index whose `createIndexes` entry named none:
    /// the fields and directions joined with underscores (`name_1`, `a_1_b_-1`).
    #[must_use]
    pub fn derived_name(key: &Document) -> String {
        key.iter()
            .map(|(field, direction)| {
                let direction = match direction {
                    Bson::Int32(value) => value.to_string(),
                    Bson::Int64(value) => value.to_string(),
                    Bson::Double(value) => (*value as i64).to_string(),
                    other => other.to_string(),
                };
                format!("{field}_{direction}")
            })
            .collect::<Vec<String>>()
            .join("_")
    }
}

/// One collection: its documents, in insertion order, and its indexes.
#[derive(Debug, Clone)]
pub struct Collection {
    documents: Vec<Document>,
    indexes: Vec<IndexSpec>,
}

impl Default for Collection {
    fn default() -> Self {
        Self {
            documents: Vec::new(),
            indexes: vec![IndexSpec::id_index()],
        }
    }
}

impl Collection {
    /// The documents it holds, in insertion order, which is the order a `find` with no sort
    /// returns them in.
    #[must_use]
    pub fn documents(&self) -> &[Document] {
        &self.documents
    }

    /// Its indexes, the `_id` index first.
    #[must_use]
    pub fn indexes(&self) -> &[IndexSpec] {
        &self.indexes
    }
}

/// One database of a cluster's volume.
#[derive(Debug, Clone, Default)]
pub struct Database {
    collections: BTreeMap<String, Collection>,
}

impl Database {
    /// The names of its collections, in order.
    #[must_use]
    pub fn collection_names(&self) -> Vec<String> {
        self.collections.keys().cloned().collect()
    }

    /// One of its collections.
    #[must_use]
    pub fn collection(&self, name: &str) -> Option<&Collection> {
        self.collections.get(name)
    }
}

/// What one `insert` did: how many documents went in, and the per-document refusals of the
/// ones that did not, as `writeErrors` reports them.
#[derive(Debug, Default)]
pub struct InsertOutcome {
    /// How many documents were stored.
    pub inserted: usize,
    /// `(index of the document in the request, why it was refused)`.
    pub errors: Vec<(usize, WireFault)>,
}

/// What one `update` statement did.
#[derive(Debug, Default)]
pub struct UpdateOutcome {
    /// `n`: how many documents matched (an upsert counts the one it inserted).
    pub matched: usize,
    /// `nModified`: how many of them the update changed.
    pub modified: usize,
    /// The `_id` of the document an upsert inserted, when it inserted one.
    pub upserted_id: Option<Bson>,
}

/// ONE cluster's whole volume.
#[derive(Debug, Default)]
pub struct Volume {
    databases: BTreeMap<String, Database>,
}

impl Volume {
    /// The names of the databases it holds, in order.
    #[must_use]
    pub fn database_names(&self) -> Vec<String> {
        self.databases.keys().cloned().collect()
    }

    /// One database.
    #[must_use]
    pub fn database(&self, name: &str) -> Option<&Database> {
        self.databases.get(name)
    }

    /// One collection.
    #[must_use]
    pub fn collection(&self, database: &str, collection: &str) -> Option<&Collection> {
        self.databases.get(database)?.collection(collection)
    }

    /// How many documents the whole volume holds, which is what a describe of it reports and
    /// what the tests of the bounds read.
    #[must_use]
    pub fn document_count(&self) -> usize {
        self.databases
            .values()
            .flat_map(|database| database.collections.values())
            .map(|collection| collection.documents.len())
            .sum()
    }

    /// Create a collection, refusing one the database already has.
    pub fn create_collection(
        &mut self,
        database: &str,
        collection: &str,
    ) -> Result<(), WireFault> {
        if self
            .databases
            .get(database)
            .is_some_and(|held| held.collections.contains_key(collection))
        {
            return Err(fault::namespace_exists(&format!(
                "{database}.{collection}"
            )));
        }
        self.collection_mut(database, collection)?;
        Ok(())
    }

    /// Drop a collection, reporting whether it was there.
    pub fn drop_collection(&mut self, database: &str, collection: &str) -> bool {
        let Some(held) = self.databases.get_mut(database) else {
            return false;
        };
        let dropped = held.collections.remove(collection).is_some();
        // A database with no collections left does not exist, which is what makes
        // `listDatabases` stop reporting it.
        if held.collections.is_empty() {
            self.databases.remove(database);
        }
        dropped
    }

    /// Drop a whole database, reporting whether it was there.
    pub fn drop_database(&mut self, database: &str) -> bool {
        self.databases.remove(database).is_some()
    }

    /// Insert documents, creating the collection where it does not exist yet — which is what
    /// the engine does for a write to a namespace that does not exist.
    ///
    /// An `ordered` insert stops at the first document it refuses; an unordered one goes on
    /// and reports every refusal.
    pub fn insert(
        &mut self,
        database: &str,
        collection: &str,
        documents: Vec<Document>,
        ordered: bool,
    ) -> Result<InsertOutcome, WireFault> {
        let namespace = format!("{database}.{collection}");
        let mut outcome = InsertOutcome::default();
        // The keys this collection's unique indexes already hold, built ONCE for the whole
        // batch and kept up to date as documents go in: a write then asks this instead of
        // scanning the collection per document, so the work one command drives grows with the
        // documents it writes rather than with their product ([`UniqueKeys`]).
        let mut taken = self
            .collection(database, collection)
            .map(UniqueKeys::of)
            .unwrap_or_default();
        for (index, mut document) in documents.into_iter().enumerate() {
            // A document the client sent without an `_id` is given one, as the engine does:
            // an object id, which carries the instant it was minted and is unique.
            if !document.contains_key("_id") {
                let mut with_id = Document::new();
                with_id.insert("_id".to_string(), Bson::ObjectId(::bson::oid::ObjectId::new()));
                for (field, value) in &document {
                    with_id.insert(field.clone(), value.clone());
                }
                document = with_id;
            }
            let refusal = self
                .check_document_size(&namespace, &document)
                .err()
                .or_else(|| self.check_room(database, collection).err())
                .or_else(|| taken.conflict(&namespace, &document, None).err());
            if let Some(refusal) = refusal {
                outcome.errors.push((index, refusal));
                if ordered {
                    break;
                }
                continue;
            }
            let held = self.collection_mut(database, collection)?;
            let position = held.documents.len();
            held.documents.push(document.clone());
            taken.hold(&document, position);
            outcome.inserted += 1;
        }
        Ok(outcome)
    }

    /// The documents a read selects, in the order it reads them.
    #[allow(clippy::too_many_arguments)]
    pub fn select(
        &self,
        database: &str,
        collection: &str,
        filter: &Document,
        sort: Option<&Document>,
        skip: usize,
        limit: Option<usize>,
        projection: Option<&Document>,
    ) -> Result<Vec<Document>, WireFault> {
        // A read of a namespace the cluster does not hold is an EMPTY result, not a refusal,
        // which is the engine's own answer for a collection that does not exist.
        let Some(held) = self.collection(database, collection) else {
            return Ok(Vec::new());
        };
        let mut selected: Vec<Document> = Vec::new();
        for document in &held.documents {
            if query::matches(document, filter)? {
                selected.push(document.clone());
            }
        }
        if let Some(sort) = sort {
            query::sort_documents(&mut selected, sort)?;
        }
        let mut selected: Vec<Document> = selected.into_iter().skip(skip).collect();
        if let Some(limit) = limit {
            selected.truncate(limit);
        }
        if let Some(projection) = projection {
            selected = selected
                .iter()
                .map(|document| query::project(document, projection))
                .collect::<Result<Vec<Document>, WireFault>>()?;
        }
        Ok(selected)
    }

    /// How many documents a filter selects.
    pub fn count(
        &self,
        database: &str,
        collection: &str,
        filter: &Document,
        skip: usize,
        limit: Option<usize>,
    ) -> Result<usize, WireFault> {
        Ok(self
            .select(database, collection, filter, None, skip, limit, None)?
            .len())
    }

    /// Apply one `update` statement.
    pub fn update(
        &mut self,
        database: &str,
        collection: &str,
        filter: &Document,
        update: &Document,
        multi: bool,
        upsert: bool,
    ) -> Result<UpdateOutcome, WireFault> {
        let namespace = format!("{database}.{collection}");
        let mut outcome = UpdateOutcome::default();
        // The positions to change are resolved first, so the change itself borrows the
        // collection once and the filter is evaluated against the documents as they were.
        let positions: Vec<usize> = match self.collection(database, collection) {
            None => Vec::new(),
            Some(held) => {
                let mut positions = Vec::new();
                for (position, document) in held.documents.iter().enumerate() {
                    if query::matches(document, filter)? {
                        positions.push(position);
                        if !multi {
                            break;
                        }
                    }
                }
                positions
            }
        };
        if positions.is_empty() {
            if !upsert {
                return Ok(outcome);
            }
            let document = query::upserted_document(filter, update)?;
            let inserted = self.insert(database, collection, vec![document], true)?;
            if let Some((_, refusal)) = inserted.errors.into_iter().next() {
                return Err(refusal);
            }
            let upserted = self
                .collection(database, collection)
                .and_then(|held| held.documents.last())
                .and_then(|document| document.get("_id").cloned());
            outcome.matched = 1;
            outcome.upserted_id = upserted;
            return Ok(outcome);
        }
        // As in [`Self::insert`]: the keys the unique indexes hold are read once for the whole
        // statement, so the work this drives grows with the documents it changes rather than
        // with their product.
        let mut taken = self
            .collection(database, collection)
            .map(UniqueKeys::of)
            .unwrap_or_default();
        for position in positions {
            let Some(current) = self
                .collection(database, collection)
                .and_then(|held| held.documents.get(position))
                .cloned()
            else {
                continue;
            };
            let mut candidate = current.clone();
            let changed = query::apply_update(&mut candidate, update)?;
            outcome.matched += 1;
            if !changed {
                continue;
            }
            self.check_document_size(&namespace, &candidate)?;
            taken.conflict(&namespace, &candidate, Some(position))?;
            if let Some(held) = self.collection_mut(database, collection)?.documents.get_mut(position)
            {
                *held = candidate.clone();
                taken.release(position);
                taken.hold(&candidate, position);
                outcome.modified += 1;
            }
        }
        Ok(outcome)
    }

    /// Delete the documents a filter selects — all of them, or at most one when `limit` is
    /// `Some(1)`, which is what the `limit` of a `delete` statement means.
    pub fn delete(
        &mut self,
        database: &str,
        collection: &str,
        filter: &Document,
        limit: Option<usize>,
    ) -> Result<usize, WireFault> {
        let positions: Vec<usize> = match self.collection(database, collection) {
            None => Vec::new(),
            Some(held) => {
                let mut positions = Vec::new();
                for (position, document) in held.documents.iter().enumerate() {
                    if query::matches(document, filter)? {
                        positions.push(position);
                        if limit.is_some_and(|limit| positions.len() >= limit) {
                            break;
                        }
                    }
                }
                positions
            }
        };
        if positions.is_empty() {
            return Ok(0);
        }
        let Some(held) = self.databases.get_mut(database) else {
            return Ok(0);
        };
        let Some(held) = held.collections.get_mut(collection) else {
            return Ok(0);
        };
        // Removed from the back, so an earlier position is not shifted by a later removal.
        for position in positions.iter().rev() {
            held.documents.remove(*position);
        }
        Ok(positions.len())
    }

    /// Build an index, creating the collection where it does not exist yet — which is what
    /// `createIndexes` does — and report how many indexes the collection had before and has
    /// now, and whether the collection was created for it.
    pub fn create_index(
        &mut self,
        database: &str,
        collection: &str,
        spec: IndexSpec,
    ) -> Result<(usize, usize, bool), WireFault> {
        let namespace = format!("{database}.{collection}");
        let created_collection = self.collection(database, collection).is_none();
        if created_collection {
            self.check_room(database, collection)?;
        }
        // A unique index can only be built where the documents already in the collection do
        // not share a key, which is the engine's own precondition for one.
        if spec.unique {
            if let Some(held) = self.collection(database, collection) {
                let mut seen: Vec<Document> = Vec::new();
                for document in &held.documents {
                    let key = query::index_key(document, &spec.key);
                    if seen.contains(&key) {
                        return Err(fault::duplicate_key(&namespace, &spec.name, &key));
                    }
                    seen.push(key);
                }
            }
        }
        let held = self.collection_mut(database, collection)?;
        let before = held.indexes.len();
        if let Some(existing) = held
            .indexes
            .iter()
            .find(|existing| existing.name == spec.name || existing.key == spec.key)
        {
            // An index that is already there is reported as such rather than built twice,
            // which is what makes `createIndexes` idempotent; one whose NAME is taken by
            // another key is refused.
            if existing.key != spec.key || existing.unique != spec.unique {
                return Err(fault::illegal_operation(format!(
                    "Index with name: {} already exists with a different name or options",
                    spec.name
                )));
            }
            return Ok((before, before, created_collection));
        }
        if before > MAX_INDEXES_PER_COLLECTION {
            return Err(fault::over_capacity(format!(
                "add index fails, too many indexes for {namespace}: {before} (this emulator \
                 holds at most {MAX_INDEXES_PER_COLLECTION} beside the _id index)"
            )));
        }
        held.indexes.push(spec);
        Ok((before, before + 1, created_collection))
    }

    /// Drop an index by name, reporting whether it was there. The `_id` index cannot be
    /// dropped, as the engine does not allow it.
    pub fn drop_index(
        &mut self,
        database: &str,
        collection: &str,
        name: &str,
    ) -> Result<bool, WireFault> {
        if name == ID_INDEX_NAME {
            return Err(fault::illegal_operation(
                "cannot drop _id index".to_string(),
            ));
        }
        let Some(held) = self.databases.get_mut(database) else {
            return Ok(false);
        };
        let Some(held) = held.collections.get_mut(collection) else {
            return Ok(false);
        };
        let before = held.indexes.len();
        held.indexes.retain(|index| index.name != name);
        Ok(held.indexes.len() != before)
    }

    /// The collection, created (with its database) if it is not there yet.
    fn collection_mut(
        &mut self,
        database: &str,
        collection: &str,
    ) -> Result<&mut Collection, WireFault> {
        self.check_room(database, collection)?;
        Ok(self
            .databases
            .entry(database.to_string())
            .or_default()
            .collections
            .entry(collection.to_string())
            .or_default())
    }

    /// Whether this volume has room for one more document in `database.collection`, and for
    /// that database and collection themselves.
    fn check_room(&self, database: &str, collection: &str) -> Result<(), WireFault> {
        let held = self.databases.get(database);
        if held.is_none() && self.databases.len() >= MAX_DATABASES {
            return Err(fault::over_capacity(format!(
                "this emulator holds at most {MAX_DATABASES} databases per cluster"
            )));
        }
        let Some(held) = held else {
            return Ok(());
        };
        let documents = held.collection(collection);
        if documents.is_none() && held.collections.len() >= MAX_COLLECTIONS_PER_DATABASE {
            return Err(fault::over_capacity(format!(
                "this emulator holds at most {MAX_COLLECTIONS_PER_DATABASE} collections per \
                 database"
            )));
        }
        if documents.is_some_and(|held| held.documents.len() >= MAX_DOCUMENTS_PER_COLLECTION) {
            return Err(fault::over_capacity(format!(
                "this emulator holds at most {MAX_DOCUMENTS_PER_COLLECTION} documents per \
                 collection"
            )));
        }
        Ok(())
    }

    /// Whether a document is within the engine's own document ceiling, and nests no deeper
    /// than this plane walks ([`query::MAX_NESTING_DEPTH`]) — so nothing deeper than that
    /// ever reaches a collection, whatever a client sent.
    fn check_document_size(&self, namespace: &str, document: &Document) -> Result<(), WireFault> {
        let size = encoded_len(document);
        if size > MAX_BSON_OBJECT_SIZE as usize {
            return Err(fault::bad_value(format!(
                "object to insert into {namespace} is {size} bytes, past the \
                 {MAX_BSON_OBJECT_SIZE} byte limit"
            )));
        }
        if !query::document_nests_within(document, query::MAX_NESTING_DEPTH) {
            return Err(fault::bad_value(format!(
                "object to insert into {namespace} nests deeper than the maximum of {}",
                query::MAX_NESTING_DEPTH
            )));
        }
        Ok(())
    }
}

/// The keys a collection's unique indexes HOLD, prepared once for one write.
///
/// A write has to answer, for every document it adds or changes, whether a unique index
/// already holds that document's key. Asked of the collection itself that is a scan per
/// document, so one command's work would grow with the PRODUCT of the documents it writes and
/// the documents the collection holds. Prepared here instead: the keys are read once, each
/// held key is the encoded form of the key document (the one form two equal keys always share)
/// paired with the position holding it, and a write asks and updates this as it goes.
#[derive(Debug, Default)]
struct UniqueKeys {
    /// One entry per unique index: its name, the fields it is over, and the key each position
    /// holds.
    indexes: Vec<(String, Document, BTreeMap<Vec<u8>, usize>)>,
}

impl UniqueKeys {
    /// The keys `collection`'s unique indexes hold now.
    fn of(collection: &Collection) -> Self {
        let indexes = collection
            .indexes
            .iter()
            .filter(|index| index.unique)
            .map(|index| {
                let held: BTreeMap<Vec<u8>, usize> = collection
                    .documents
                    .iter()
                    .enumerate()
                    .map(|(position, document)| {
                        (encoded_key(document, &index.key), position)
                    })
                    .collect();
                (index.name.clone(), index.key.clone(), held)
            })
            .collect();
        Self { indexes }
    }

    /// The refusal `document` earns where a unique index already holds its key, with the
    /// position being REPLACED — whose own key does not count against it — left out.
    fn conflict(
        &self,
        namespace: &str,
        document: &Document,
        replacing: Option<usize>,
    ) -> Result<(), WireFault> {
        for (name, fields, held) in &self.indexes {
            let key = encoded_key(document, fields);
            if held
                .get(&key)
                .is_some_and(|position| Some(*position) != replacing)
            {
                return Err(fault::duplicate_key(
                    namespace,
                    name,
                    &query::index_key(document, fields),
                ));
            }
        }
        Ok(())
    }

    /// Record that `position` now holds `document`'s keys.
    fn hold(&mut self, document: &Document, position: usize) {
        for (_, fields, held) in &mut self.indexes {
            held.insert(encoded_key(document, fields), position);
        }
    }

    /// Forget whatever keys `position` held, which a change to that document replaces.
    fn release(&mut self, position: usize) {
        for (_, _, held) in &mut self.indexes {
            held.retain(|_, holder| *holder != position);
        }
    }
}

/// One document's key for an index over `fields`, in the encoded form two equal keys always
/// share — which is what makes the map above answer in one lookup.
fn encoded_key(document: &Document, fields: &Document) -> Vec<u8> {
    let mut bytes: Vec<u8> = Vec::new();
    let key = query::index_key(document, fields);
    // A key that cannot be encoded is not one a document could carry either; the empty form
    // then stands for it, which only ever compares it with another such key.
    let _ = key.to_writer(&mut bytes);
    bytes
}

/// How many bytes a document is on the wire, which is what the engine's document ceiling is
/// measured in.
#[must_use]
pub fn encoded_len(document: &Document) -> usize {
    let mut bytes: Vec<u8> = Vec::new();
    match document.to_writer(&mut bytes) {
        Ok(()) => bytes.len(),
        // A document that cannot be encoded is not one that can be stored either; reporting
        // it as past the ceiling refuses it where the ceiling is checked.
        Err(_) => MAX_BSON_OBJECT_SIZE as usize + 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::bson::doc;

    /// RULE: an insert stores the documents it was given, a read selects them back, and the
    /// `_id` the engine mints for a document without one is on the stored document.
    #[test]
    fn documents_go_in_and_come_back() {
        let mut volume = Volume::default();
        let outcome = volume
            .insert(
                "shop",
                "widgets",
                vec![doc! { "_id": 1_i32, "name": "left" }, doc! { "name": "right" }],
                true,
            )
            .expect("the insert is well formed");
        assert_eq!(
            (outcome.inserted, outcome.errors.len()),
            (2, 0),
            "expected two documents stored and no refusal, got {outcome:?}"
        );
        let found = volume
            .select("shop", "widgets", &doc! {}, None, 0, None, None)
            .expect("the read is well formed");
        assert_eq!(found.len(), 2, "expected both documents back, got {found:?}");
        assert!(
            found[1].get("_id").is_some(),
            "expected a minted _id on the document inserted without one, got {:?}",
            found[1]
        );
        let one = volume
            .select("shop", "widgets", &doc! { "name": "left" }, None, 0, None, None)
            .expect("the read is well formed");
        assert_eq!(one.len(), 1, "expected the filter to select one, got {one:?}");
    }

    /// RULE: a unique index refuses a second document with the same key — the `_id` index
    /// every collection has, and one `createIndexes` built.
    #[test]
    fn a_unique_index_refuses_a_duplicate_key() {
        let mut volume = Volume::default();
        volume
            .insert("shop", "widgets", vec![doc! { "_id": 1_i32, "sku": "a" }], true)
            .expect("the first insert is well formed");
        let repeated = volume
            .insert("shop", "widgets", vec![doc! { "_id": 1_i32, "sku": "b" }], true)
            .expect("the insert itself is well formed");
        let refusal = repeated
            .errors
            .first()
            .map(|(_, refusal)| refusal.clone())
            .expect("the duplicate _id is refused");
        assert_eq!(
            (refusal.code, refusal.code_name),
            (11000, "DuplicateKey"),
            "expected the engine's duplicate-key refusal, got {refusal:?}"
        );
        assert!(
            refusal.message.contains("shop.widgets index: _id_ dup key: { _id: 1 }"),
            "expected the engine's duplicate-key sentence, got {:?}",
            refusal.message
        );
        volume
            .create_index(
                "shop",
                "widgets",
                IndexSpec {
                    name: "sku_1".to_string(),
                    key: doc! { "sku": 1_i32 },
                    unique: true,
                },
            )
            .expect("the index is built over documents with no duplicate");
        let same_sku = volume
            .insert("shop", "widgets", vec![doc! { "_id": 2_i32, "sku": "a" }], true)
            .expect("the insert itself is well formed");
        assert_eq!(
            same_sku.errors.len(),
            1,
            "expected the unique index to refuse the second document, got {same_sku:?}"
        );
    }

    /// RULE: an update changes the documents its filter selects and no others; an upsert
    /// that matched nothing inserts one.
    #[test]
    fn an_update_changes_what_it_selected() {
        let mut volume = Volume::default();
        volume
            .insert(
                "shop",
                "widgets",
                vec![
                    doc! { "_id": 1_i32, "stock": 1_i32 },
                    doc! { "_id": 2_i32, "stock": 1_i32 },
                ],
                true,
            )
            .expect("the insert is well formed");
        let one = volume
            .update(
                "shop",
                "widgets",
                &doc! { "_id": 1_i32 },
                &doc! { "$inc": { "stock": 4_i32 } },
                false,
                false,
            )
            .expect("the update is well formed");
        assert_eq!(
            (one.matched, one.modified),
            (1, 1),
            "expected one document matched and changed, got {one:?}"
        );
        let stocks: Vec<i32> = volume
            .select("shop", "widgets", &doc! {}, Some(&doc! { "_id": 1_i32 }), 0, None, None)
            .expect("the read is well formed")
            .iter()
            .filter_map(|document| document.get_i32("stock").ok())
            .collect();
        assert_eq!(
            stocks,
            vec![5, 1],
            "expected only the selected document to change, got {stocks:?}"
        );
        let upserted = volume
            .update(
                "shop",
                "widgets",
                &doc! { "_id": 9_i32 },
                &doc! { "$set": { "stock": 3_i32 } },
                false,
                true,
            )
            .expect("the upsert is well formed");
        assert_eq!(
            upserted.upserted_id,
            Some(Bson::Int32(9)),
            "expected the upsert to insert the document its filter pinned, got {upserted:?}"
        );
    }

    /// RULE: a delete removes the documents its filter selects, at most one where its limit
    /// says so.
    #[test]
    fn a_delete_removes_what_it_selected() {
        let mut volume = Volume::default();
        volume
            .insert(
                "shop",
                "widgets",
                vec![
                    doc! { "_id": 1_i32, "gone": true },
                    doc! { "_id": 2_i32, "gone": true },
                    doc! { "_id": 3_i32, "gone": false },
                ],
                true,
            )
            .expect("the insert is well formed");
        let one = volume
            .delete("shop", "widgets", &doc! { "gone": true }, Some(1))
            .expect("the delete is well formed");
        assert_eq!(one, 1, "expected one document removed, got {one}");
        let rest = volume
            .delete("shop", "widgets", &doc! { "gone": true }, None)
            .expect("the delete is well formed");
        assert_eq!(rest, 1, "expected the other matching document removed, got {rest}");
        assert_eq!(
            volume.document_count(),
            1,
            "expected only the unmatched document to remain, got {:?}",
            volume.database_names()
        );
    }

    /// RULE: a dropped collection is gone, and a database with no collections left is gone
    /// with it.
    #[test]
    fn a_drop_removes_the_namespace() {
        let mut volume = Volume::default();
        volume
            .insert("shop", "widgets", vec![doc! { "_id": 1_i32 }], true)
            .expect("the insert is well formed");
        assert!(
            volume.drop_collection("shop", "widgets"),
            "expected the collection to be there to drop"
        );
        assert_eq!(
            volume.database_names(),
            Vec::<String>::new(),
            "expected the database to go with its last collection"
        );
        assert!(
            !volume.drop_collection("shop", "widgets"),
            "expected a second drop to report nothing dropped"
        );
    }
}
