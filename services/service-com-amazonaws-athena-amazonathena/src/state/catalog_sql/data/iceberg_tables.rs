// SPDX-License-Identifier: CC0-1.0

//! The data files a table of the ICEBERG open table format holds, read through the table's
//! own metadata.
//!
//! A Hive table's data is every object at its location, so a read of one lists that prefix
//! ([`super::DataSource::rows`]). A table of an open table format is not read that way: the
//! catalog entry carries only a pointer (`metadata_location`) to the table's METADATA
//! document, and that document — not the prefix — says what the table holds. The document
//! names the table's snapshots and which of them is CURRENT; the current snapshot names a
//! manifest list; the manifest list names the manifests; and each manifest names the data
//! files, one entry per file, with the status that entry carries.
//!
//! So the rows a query over such a table reads are the records of the data files the table's
//! current COMMITTED snapshot reaches, and nothing else at the table's location is its data:
//! not the metadata, the manifest lists or the manifests, and not a data file no snapshot has
//! committed. A table whose document names no current snapshot — the table a create has just
//! minted — therefore holds no row and a read of it goes through no data at all.
//!
//! The documents and the Avro containers read here are the ones the writers of such a table
//! in this emulator produce, each written as the Iceberg table specification lays it out.

use ::service_com_amazonaws_s3_amazons3::state as s3_state;

use super::{cannot_open_split, parse_s3_uri, Budget, Decoder, Failure, Relation};

/// The `content` an entry of a DATA file carries, from Iceberg's own file-content
/// enumeration: a data file is `0`, and the delete files that carry removals are `1` and
/// `2`.
const CONTENT_DATA: i64 = 0;

/// The `status` of a manifest entry whose file the snapshot has REMOVED, from Iceberg's
/// manifest-entry status enumeration (`0` existing, `1` added, `2` deleted): such a file is
/// no part of what the snapshot holds.
const STATUS_DELETED: i64 = 2;

/// The `current-snapshot-id` of a table that has committed nothing: Iceberg's own "no
/// current snapshot" value.
const NO_CURRENT_SNAPSHOT: i64 = -1;

/// The file format Iceberg writes a table's data files in unless the table asks for another
/// (`write.format.default`), which is what a manifest entry naming no format is read as.
const DEFAULT_DATA_FILE_FORMAT: &str = "PARQUET";

/// How many entries of ONE Avro container this reader walks.
///
/// A CHOSEN bound: a manifest list and a manifest both live in the object store, where a
/// caller can write one, so this is what keeps a read from being handed an unbounded number
/// of entries. Far above what the writers of such a table in this emulator produce, each
/// commit adding one manifest holding one data file.
const MAX_CONTAINER_ENTRIES: usize = 100_000;

/// How many data files ONE read of an Iceberg table resolves, for the same reason.
const MAX_DATA_FILES: usize = 100_000;

/// How many unions deep a read opens one value of a container: a CHOSEN bound, so the
/// nesting a caller's own container declares is never what decides how far this reader
/// walks. Well above the one union an optional member of a manifest entry is.
const MAX_UNION_DEPTH: usize = 8;

/// How many bytes ONE metadata container this reader reads may be stored as.
///
/// A CHOSEN bound, for the same reason as the counts above: the container is an object a
/// caller can write, and these bytes are what bound the values reading it builds. Far above
/// what the writers of such a table in this emulator produce — a manifest list entry per
/// commit and a manifest entry per data file, a few hundred bytes each.
const MAX_CONTAINER_BYTES: usize = 1024 * 1024;

/// How deep one value of a container, or the schema document declaring it, nests before this
/// reader refuses to walk it, and how many nodes of either it walks: two more CHOSEN bounds.
/// Well above an Iceberg manifest entry, whose three levels (the entry, its data file, and
/// that file's partition) the document declaring them writes a few members deep each.
const MAX_CONTAINER_DEPTH: usize = 32;
/// See [`MAX_CONTAINER_DEPTH`].
const MAX_CONTAINER_NODES: usize = 4096;

/// What one entry of a container costs a read at the LEAST.
///
/// A CHOSEN floor: some Avro values are encoded in no bytes at all, so what an entry retains
/// can be larger than what it was stored as — and an entry charged nothing would let a
/// container hold entries the budget never bounds.
const MIN_ENTRY_BYTES: usize = 1024;

/// One DATA FILE of a table's current snapshot: where it lives, and the format it holds its
/// records in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DataFile {
    /// The `s3://` location of the file, as its manifest entry records it.
    pub path: String,
    /// The format the file's records are written in, as its manifest entry records it.
    pub format: String,
}

impl DataFile {
    /// How this file's records are decoded, or `None` for a format this emulator's query
    /// engine does not read.
    pub fn decoder(&self) -> Option<Decoder> {
        self.format
            .trim()
            .eq_ignore_ascii_case(DEFAULT_DATA_FILE_FORMAT)
            .then_some(Decoder::Parquet)
    }
}

/// The data files the table whose metadata lives at `metadata_location` currently holds: the
/// files its current snapshot's manifests name and have not removed, in the order the
/// manifests name them.
///
/// EMPTY for a table whose metadata names no current snapshot, and for one whose current
/// snapshot names no manifest: a table nothing has committed to holds no data file, which is
/// not a failure of the read.
pub(super) fn committed_data_files(
    objects: &s3_state::ComAmazonawsS3Amazons3State,
    metadata_location: &str,
    relation: &Relation,
    budget: &mut Budget,
) -> Result<Vec<DataFile>, Failure> {
    budget.charge_deadline()?;
    let document = metadata_document(objects, metadata_location, relation, budget)?;
    let Some(snapshot) = current_snapshot(&document) else {
        return Ok(Vec::new());
    };
    let Some(manifest_list) = snapshot
        .get("manifest-list")
        .and_then(::serde_json::Value::as_str)
        .filter(|at| !at.trim().is_empty())
    else {
        return Ok(Vec::new());
    };
    let mut files: Vec<DataFile> = Vec::new();
    for manifest in manifest_paths(objects, manifest_list, relation, budget)? {
        for file in data_files(objects, &manifest, relation, budget)? {
            if files.len() >= MAX_DATA_FILES {
                return Err(too_many(relation, MAX_DATA_FILES, "data files"));
            }
            files.push(file);
        }
    }
    Ok(files)
}

/// The table's METADATA DOCUMENT, read out of the object store at the location the catalog
/// points at.
///
/// A pointer naming an object the store does not hold is a table whose metadata cannot be
/// dereferenced, which is the same filesystem class as a location that is not there
/// ([`Failure::location_not_readable`]) — the read never reaches a row of the table. One
/// whose bytes are not that document at all is a split this reader cannot open.
fn metadata_document(
    objects: &s3_state::ComAmazonawsS3Amazons3State,
    metadata_location: &str,
    relation: &Relation,
    budget: &mut Budget,
) -> Result<::serde_json::Value, Failure> {
    let body = held_object(objects, metadata_location, relation, budget)?;
    ::serde_json::from_slice(&body).map_err(|reason| {
        cannot_open_split(metadata_location, &reason.to_string())
    })
}

/// The snapshot a metadata document reports as CURRENT, by its `current-snapshot-id`, and
/// `None` for a document that reports none — the value Iceberg states for a table nothing
/// has committed to, a document carrying no such member, and one whose snapshot list holds
/// no snapshot of that id.
fn current_snapshot(document: &::serde_json::Value) -> Option<&::serde_json::Value> {
    let current = document
        .get("current-snapshot-id")
        .and_then(::serde_json::Value::as_i64)
        .filter(|id| *id != NO_CURRENT_SNAPSHOT)?;
    document
        .get("snapshots")?
        .as_array()?
        .iter()
        .find(|snapshot| {
            snapshot.get("snapshot-id").and_then(::serde_json::Value::as_i64) == Some(current)
        })
}

/// The manifests the snapshot's MANIFEST LIST at `manifest_list` names: every entry that
/// holds data files, by the path it records.
fn manifest_paths(
    objects: &s3_state::ComAmazonawsS3Amazons3State,
    manifest_list: &str,
    relation: &Relation,
    budget: &mut Budget,
) -> Result<Vec<String>, Failure> {
    let entries = container_entries(objects, manifest_list, relation, budget)?;
    Ok(entries
        .iter()
        .filter(|entry| holds_data(entry))
        .filter_map(|entry| text(entry, "manifest_path"))
        .collect())
}

/// The data files ONE manifest names: every entry whose file is a data file the snapshot has
/// not removed.
fn data_files(
    objects: &s3_state::ComAmazonawsS3Amazons3State,
    manifest: &str,
    relation: &Relation,
    budget: &mut Budget,
) -> Result<Vec<DataFile>, Failure> {
    let entries = container_entries(objects, manifest, relation, budget)?;
    Ok(entries.iter().filter_map(data_file_of).collect())
}

/// The data file ONE manifest entry contributes to what the snapshot holds, and `None` for
/// an entry that contributes none: one the snapshot REMOVED, and one naming a delete file
/// rather than a data file.
fn data_file_of(entry: &::apache_avro::types::Value) -> Option<DataFile> {
    if number(entry, "status") == Some(STATUS_DELETED) {
        return None;
    }
    let file = field(entry, "data_file")?;
    if !holds_data(file) {
        return None;
    }
    Some(DataFile {
        path: text(file, "file_path")?,
        format: text(file, "file_format")
            .unwrap_or_else(|| DEFAULT_DATA_FILE_FORMAT.to_string()),
    })
}

/// Whether an entry — of a manifest list, or of a manifest — is about DATA rather than about
/// the removals a delete file carries. An entry naming no content is a data entry: the
/// `content` member is version 2 of the format's, and a version-1 container has no member
/// for it because every file it names is a data file.
fn holds_data(entry: &::apache_avro::types::Value) -> bool {
    number(entry, "content").unwrap_or(CONTENT_DATA) == CONTENT_DATA
}

/// The records of the Avro container at `location`, read by the Apache implementation of
/// that format — the same one the writers of such a table in this emulator write it with.
///
/// A container is an object a CALLER can write, so what reading one may hold is bounded
/// before the first entry is built and again for every entry kept:
///
/// * the stored object is no larger than [`MAX_CONTAINER_BYTES`], and those bytes are charged
///   against the read's budget;
/// * the container has to be one this reader can bound a value of ([`readable`]): its blocks
///   stored as they stand, and its declared schema naming no collection, so an entry's
///   decoded size stays within a constant factor of the bytes it was encoded from rather than
///   of a count the container merely declares;
/// * every entry kept is charged for what it retains, at a floor, so an entry whose encoding
///   is shorter than the value it builds is never free;
/// * and a value that nests deeper, or holds more nodes, than this reader walks is refused
///   rather than charged for part of itself.
fn container_entries(
    objects: &s3_state::ComAmazonawsS3Amazons3State,
    location: &str,
    relation: &Relation,
    budget: &mut Budget,
) -> Result<Vec<::apache_avro::types::Value>, Failure> {
    let body = held_object(objects, location, relation, budget)?;
    if body.len() > MAX_CONTAINER_BYTES {
        return Err(too_many(relation, MAX_CONTAINER_BYTES, "bytes in one metadata object"));
    }
    readable(&body, location)?;
    // The ceiling the format's own implementation allocates under while decoding ONE value,
    // asked for before a caller-written container is read. It keeps one such ceiling for the
    // process and the first caller to need it settles it, so this asks for a bound and can
    // never raise one; the bounds above are what this read rests on.
    let _ceiling = ::apache_avro::max_allocation_bytes(MAX_CONTAINER_BYTES);
    let reader = ::apache_avro::Reader::new(&body[..])
        .map_err(|reason| cannot_open_split(location, &reason.to_string()))?;
    let mut entries = Vec::new();
    for held in reader {
        budget.charge_deadline()?;
        if entries.len() >= MAX_CONTAINER_ENTRIES {
            return Err(too_many(relation, MAX_CONTAINER_ENTRIES, "entries of one \
                 manifest container"));
        }
        let entry = held.map_err(|reason| cannot_open_split(location, &reason.to_string()))?;
        let retained = retained_bytes(&entry).ok_or_else(|| {
            too_many(relation, MAX_CONTAINER_NODES, "nodes in one entry of a metadata object")
        })?;
        budget.charge_decoded(relation, retained.max(MIN_ENTRY_BYTES))?;
        entries.push(entry);
    }
    Ok(entries)
}

/// The Avro object-container HEADER, as the Avro specification declares its schema: the magic
/// bytes, the metadata map every container carries its codec and its schema in, and the sync
/// marker. It is read with the format's OWN decoder against this schema, so nothing here
/// takes the container's bytes apart by hand.
const CONTAINER_HEADER_SCHEMA: &str = r#"{
  "type": "record",
  "name": "org.apache.avro.file.Header",
  "fields": [
    { "name": "magic", "type": { "type": "fixed", "name": "Magic", "size": 4 } },
    { "name": "meta", "type": { "type": "map", "values": "bytes" } },
    { "name": "sync", "type": { "type": "fixed", "name": "Sync", "size": 16 } }
  ]
}"#;

/// The header member a container declares the codec its blocks are coded with in.
const CODEC_METADATA: &str = "avro.codec";

/// The header member a container declares the schema of its entries in.
const SCHEMA_METADATA: &str = "avro.schema";

/// The codec of a container whose blocks are stored AS THEY STAND, which is the one read
/// here and the one the writers of such a table in this emulator write.
const NULL_CODEC: &str = "null";

/// The type names of the Avro COLLECTIONS: the two whose values a container declares a COUNT
/// of rather than carrying each of them, which is what makes the decoded size of one entry
/// independent of the bytes that entry was stored as. A schema document writes either as an
/// object naming the type, which is the one place this is read from.
const COLLECTION_TYPES: &[&str] = &["array", "map"];

/// Whether the container `body` is one a read of it can be bounded: its blocks stored as they
/// stand, and its entries declared by a schema naming no collection.
///
/// Both halves are about the same thing — what reading ONE entry may hold. A COMPRESSED block
/// is decompressed whole, by the format's own reader, before one entry of it is handed back,
/// and what it expands to is not bounded by the bytes it was stored as. A COLLECTION is
/// encoded as a declared count followed by its items, and an item of some types is encoded in
/// no bytes at all, so a container of a few hundred bytes can declare a very large number of
/// values to build. Neither is bounded by the stored object this read was charged for, which
/// is the one bound a caller-written object has — so a container that is either is refused
/// rather than read. What is left expands by no more than the constant factor a decoded value
/// costs over its own encoding, and every entry built out of it is charged for what it
/// retains, so the bytes this query may hold bound the read.
///
/// The header is read with the format's OWN decoder against the schema the Avro
/// specification declares for it ([`CONTAINER_HEADER_SCHEMA`]), and the schema the header
/// carries is the JSON document it is written as — so nothing here takes a container apart by
/// hand.
fn readable(body: &[u8], at: &str) -> Result<(), Failure> {
    use ::apache_avro::types::Value;

    let schema = ::apache_avro::Schema::parse_str(CONTAINER_HEADER_SCHEMA)
        .map_err(|reason| cannot_open_split(at, &reason.to_string()))?;
    let mut reading = body;
    let header = ::apache_avro::from_avro_datum(&schema, &mut reading, None)
        .map_err(|reason| cannot_open_split(at, &reason.to_string()))?;
    let Value::Record(members) = header else {
        return Err(cannot_open_split(at, "it declares no container header"));
    };
    let metadata = members
        .iter()
        .find(|(member, _)| member == "meta")
        .and_then(|(_, value)| match value {
            Value::Map(metadata) => Some(metadata),
            _ => None,
        });
    let declared = |name: &str| {
        metadata
            .and_then(|metadata| metadata.get(name))
            .and_then(|value| match value {
                Value::Bytes(text) => Some(String::from_utf8_lossy(text).to_string()),
                _ => None,
            })
    };
    // A container declaring NO codec is one stored as it stands, which is what the format
    // reads a header carrying no such member as.
    match declared(CODEC_METADATA) {
        None => {}
        Some(codec) if codec.trim() == NULL_CODEC => {}
        Some(codec) => {
            return Err(Failure::emulator(format!(
                "the Iceberg metadata object {at} holds its entries compressed with the \
                 {codec} codec, which this emulator's query engine does not decompress"
            )))
        }
    }
    // A container declaring no schema at all is one the format's own reader refuses, which it
    // reports when it opens the container below.
    let Some(entries) = declared(SCHEMA_METADATA) else {
        return Ok(());
    };
    let entries: ::serde_json::Value = ::serde_json::from_str(&entries)
        .map_err(|reason| cannot_open_split(at, &reason.to_string()))?;
    collection_free(&entries, at)
}

/// Whether the schema document `declared` names no collection, and nests no deeper and holds
/// no more nodes than this reader walks.
///
/// Iterative, with a node budget and a depth bound: how far a walk of a caller-written
/// document goes is this reader's own business and never the document's.
fn collection_free(declared: &::serde_json::Value, at: &str) -> Result<(), Failure> {
    let collection = |name: &str| COLLECTION_TYPES.contains(&name);
    let refused = |named: &str| {
        Failure::emulator(format!(
            "the Iceberg metadata object {at} declares its entries with the {named} type, \
             whose values this emulator's query engine does not read"
        ))
    };
    let mut walking: Vec<(&::serde_json::Value, usize)> = vec![(declared, 0)];
    let mut nodes = 0usize;
    while let Some((node, depth)) = walking.pop() {
        nodes = nodes.saturating_add(1);
        if nodes > MAX_CONTAINER_NODES || depth > MAX_CONTAINER_DEPTH {
            return Err(refused("more involved than this reader walks, with a"));
        }
        match node {
            ::serde_json::Value::Object(members) => {
                if let Some(named) = members
                    .get("type")
                    .and_then(::serde_json::Value::as_str)
                    .filter(|named| collection(named))
                {
                    return Err(refused(named));
                }
                for (_, value) in members {
                    walking.push((value, depth.saturating_add(1)));
                }
            }
            // A JSON array in a schema document is a UNION of the schemas it holds.
            ::serde_json::Value::Array(held) => {
                for value in held {
                    walking.push((value, depth.saturating_add(1)));
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// How many bytes one decoded entry RETAINS, near enough to charge a read for it: every
/// node's own width, the names its records and maps carry, and the text and bytes its values
/// hold. `None` for a value that nests deeper, or holds more nodes, than this reader walks —
/// a value this read refuses rather than charging for part of.
///
/// Iterative and bounded, for the same reason [`collection_free`] is: the walk is this
/// reader's, not the container's.
fn retained_bytes(entry: &::apache_avro::types::Value) -> Option<usize> {
    use ::apache_avro::types::Value;

    let width = ::std::mem::size_of::<Value>();
    let mut bytes = 0usize;
    let mut nodes = 0usize;
    let mut walking: Vec<(&Value, usize)> = vec![(entry, 0)];
    while let Some((value, depth)) = walking.pop() {
        nodes = nodes.saturating_add(1);
        if nodes > MAX_CONTAINER_NODES || depth > MAX_CONTAINER_DEPTH {
            return None;
        }
        bytes = bytes.saturating_add(width);
        let deeper = depth.saturating_add(1);
        match value {
            Value::String(text) | Value::Enum(_, text) => {
                bytes = bytes.saturating_add(text.len());
            }
            Value::Bytes(data) | Value::Fixed(_, data) => {
                bytes = bytes.saturating_add(data.len());
            }
            Value::Record(members) => {
                for (name, member) in members {
                    bytes = bytes.saturating_add(name.len());
                    walking.push((member, deeper));
                }
            }
            Value::Map(members) => {
                for (name, member) in members {
                    bytes = bytes.saturating_add(name.len());
                    walking.push((member, deeper));
                }
            }
            Value::Array(held) => {
                for member in held {
                    walking.push((member, deeper));
                }
            }
            Value::Union(_, inner) => walking.push((inner, deeper)),
            _ => {}
        }
    }
    Some(bytes)
}

/// The bytes the object store holds at the `s3://` location `at`.
///
/// What the metadata of a table costs to read is charged against the read's budget, and not
/// against what the query SCANNED: the data a query scans is its table's data files, which
/// is what `DataScannedInBytes` reports — an aggregate a snapshot answers without reaching
/// one scans no data at all.
fn held_object(
    objects: &s3_state::ComAmazonawsS3Amazons3State,
    at: &str,
    relation: &Relation,
    budget: &mut Budget,
) -> Result<Vec<u8>, Failure> {
    budget.charge_deadline()?;
    let Some((bucket, key)) = parse_s3_uri(at) else {
        return Err(Failure::location_not_readable(at));
    };
    let Some(held) = objects
        .bucket(&bucket)
        .and_then(|bucket| bucket.object(&key))
    else {
        return Err(Failure::location_not_readable(at));
    };
    budget.charge_decoded(relation, held.body.len())?;
    Ok(held.body.clone())
}

/// Why a table whose metadata names more of something than one read of it walks is refused.
fn too_many(relation: &Relation, bound: usize, what: &str) -> Failure {
    Failure::emulator(format!(
        "the Iceberg metadata of {}.{} names more than the {bound} {what} this emulator \
         reads for one query",
        relation.database, relation.table
    ))
}

/// The member `name` of one Avro record, with the union it may be wrapped in opened.
fn field<'held>(
    record: &'held ::apache_avro::types::Value,
    name: &str,
) -> Option<&'held ::apache_avro::types::Value> {
    let ::apache_avro::types::Value::Record(members) = held(record) else {
        return None;
    };
    members
        .iter()
        .find(|(member, _)| member == name)
        .map(|(_, value)| held(value))
}

/// The text the member `name` of one Avro record carries.
fn text(record: &::apache_avro::types::Value, name: &str) -> Option<String> {
    match field(record, name)? {
        ::apache_avro::types::Value::String(text) => Some(text.clone()),
        _ => None,
    }
}

/// The number the member `name` of one Avro record carries, whichever integer width it was
/// written at.
fn number(record: &::apache_avro::types::Value, name: &str) -> Option<i64> {
    match field(record, name)? {
        ::apache_avro::types::Value::Int(value) => Some(i64::from(*value)),
        ::apache_avro::types::Value::Long(value) => Some(*value),
        _ => None,
    }
}

/// The value one Avro value HOLDS: a union carries its value inside it, and every other
/// value is itself.
///
/// Iterative, and bounded: the union nesting a caller's own container could declare is not
/// this reader's recursion to follow.
fn held(value: &::apache_avro::types::Value) -> &::apache_avro::types::Value {
    let mut holding = value;
    for _ in 0..MAX_UNION_DEPTH {
        match holding {
            ::apache_avro::types::Value::Union(_, inner) => holding = inner,
            other => return other,
        }
    }
    holding
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Avro schema one container of these tests holds.
    const ENTRY_SCHEMA: &str = r#"{
      "type": "record",
      "name": "entry",
      "fields": [{ "name": "manifest_path", "type": "string" }]
    }"#;

    /// One container holding one entry, written by the format's own writer under `codec`.
    fn container(codec: ::apache_avro::Codec) -> Vec<u8> {
        let schema = ::apache_avro::Schema::parse_str(ENTRY_SCHEMA).expect("the schema");
        let mut writer = ::apache_avro::Writer::with_codec(&schema, Vec::new(), codec);
        writer
            .append(::apache_avro::types::Value::Record(vec![(
                "manifest_path".to_string(),
                // Compressible text, so the coded container really is smaller than the
                // entries it holds.
                ::apache_avro::types::Value::String("s3://lake/t/metadata/".repeat(64)),
            )]))
            .expect("the container takes its entry");
        writer.into_inner().expect("the container")
    }

    /// The Avro schema of a container whose entries hold a COLLECTION, whose values its own
    /// bytes do not bound.
    const COLLECTION_SCHEMA: &str = r#"{
      "type": "record",
      "name": "entry",
      "fields": [{ "name": "held", "type": { "type": "array", "items": "null" } }]
    }"#;

    /// One container whose entry holds a collection of `count` values, written by the
    /// format's own writer: the values are encoded in no bytes at all, so the container stays
    /// small however many it declares.
    fn collection_container(count: usize) -> Vec<u8> {
        let schema = ::apache_avro::Schema::parse_str(COLLECTION_SCHEMA).expect("the schema");
        let mut writer = ::apache_avro::Writer::new(&schema, Vec::new());
        writer
            .append(::apache_avro::types::Value::Record(vec![(
                "held".to_string(),
                ::apache_avro::types::Value::Array(vec![
                    ::apache_avro::types::Value::Null;
                    count
                ]),
            )]))
            .expect("the container takes its entry");
        writer.into_inner().expect("the container")
    }

    /// A container a read goes through is one whose entries its own stored bytes bound: its
    /// blocks stored as they stand, and its schema naming no collection — because what a
    /// compressed block expands to, and how many values a declared count names, are both
    /// independent of the bytes the container was stored as.
    #[test]
    fn only_a_container_its_own_bytes_bound_is_read() {
        // (case, the container, whether a read takes it)
        let cases: Vec<(&str, Vec<u8>, bool)> = vec![
            (
                "stored as it stands",
                container(::apache_avro::Codec::Null),
                true,
            ),
            (
                "stored compressed",
                container(::apache_avro::Codec::Deflate),
                false,
            ),
            (
                "entries declared with a collection",
                collection_container(4),
                false,
            ),
            (
                "entries declaring a collection of many values in few bytes",
                collection_container(100_000),
                false,
            ),
        ];

        for (case, body, readable_container) in cases {
            assert!(
                body.len() <= MAX_CONTAINER_BYTES,
                "case {case:?}: this container is small enough to be read at all, got {} \
                 bytes",
                body.len()
            );
            let taken = readable(&body, "s3://lake/t/metadata/snap-1.avro");
            assert_eq!(
                taken.is_ok(),
                readable_container,
                "case {case:?}: expected readable={readable_container}, got {taken:?}"
            );
        }
    }

    /// What one entry retains is charged for every node of it, and a value nesting deeper
    /// than this reader walks is refused rather than charged for part of itself.
    #[test]
    fn an_entry_is_charged_for_every_node_it_holds() {
        use ::apache_avro::types::Value;

        let width = ::std::mem::size_of::<Value>();
        let flat = Value::Record(vec![(
            "manifest_path".to_string(),
            Value::String("s3://lake/t/metadata/0-m0.avro".to_string()),
        )]);
        let charged = retained_bytes(&flat).expect("a flat entry is charged");
        assert!(
            charged >= "manifest_path".len() + "s3://lake/t/metadata/0-m0.avro".len() + width,
            "a flat entry is charged for its member's name and text, got {charged}"
        );
        // A collection is charged for every element it holds, not for the one node naming it.
        let held = Value::Record(vec![(
            "held".to_string(),
            Value::Array(vec![Value::Long(1); 16]),
        )]);
        let charged = retained_bytes(&held).expect("a collection is charged");
        assert!(
            charged >= 16 * width,
            "a collection is charged for every element it holds, got {charged}"
        );
        // A value nesting past the bound is refused.
        let mut deep = Value::Null;
        for _ in 0..(MAX_CONTAINER_DEPTH + 2) {
            deep = Value::Array(vec![deep]);
        }
        let charged = retained_bytes(&deep);
        assert_eq!(
            charged, None,
            "a value nesting past the bound is refused, got {charged:?}"
        );
    }

    /// A manifest entry as the Avro reader hands one back: the status, and the data file
    /// with the content, path and format its members carry.
    fn manifest_entry(
        status: i32,
        content: i32,
        path: &str,
        format: Option<&str>,
    ) -> ::apache_avro::types::Value {
        use ::apache_avro::types::Value;
        let mut file = vec![
            ("content".to_string(), Value::Int(content)),
            ("file_path".to_string(), Value::String(path.to_string())),
        ];
        if let Some(format) = format {
            file.push((
                "file_format".to_string(),
                Value::Union(1, Box::new(Value::String(format.to_string()))),
            ));
        }
        Value::Record(vec![
            ("status".to_string(), Value::Int(status)),
            (
                "snapshot_id".to_string(),
                Value::Union(1, Box::new(Value::Long(7))),
            ),
            ("data_file".to_string(), Value::Record(file)),
        ])
    }

    /// Which manifest entries a read takes the data files of: an added or existing data
    /// file, and neither a deleted one nor a delete file. The format a read decodes such a
    /// file as is the one the entry names, and Iceberg's default where it names none.
    #[test]
    fn a_read_takes_the_data_files_the_entries_hold() {
        // (case, the entry, the data file it contributes)
        let cases: Vec<(&str, ::apache_avro::types::Value, Option<DataFile>)> = vec![
            (
                "an added data file",
                manifest_entry(1, 0, "s3://lake/t/data/a.parquet", Some("PARQUET")),
                Some(DataFile {
                    path: "s3://lake/t/data/a.parquet".to_string(),
                    format: "PARQUET".to_string(),
                }),
            ),
            (
                "an existing data file",
                manifest_entry(0, 0, "s3://lake/t/data/b.parquet", Some("parquet")),
                Some(DataFile {
                    path: "s3://lake/t/data/b.parquet".to_string(),
                    format: "parquet".to_string(),
                }),
            ),
            (
                "an entry naming no format",
                manifest_entry(1, 0, "s3://lake/t/data/c.parquet", None),
                Some(DataFile {
                    path: "s3://lake/t/data/c.parquet".to_string(),
                    format: DEFAULT_DATA_FILE_FORMAT.to_string(),
                }),
            ),
            (
                "a deleted data file",
                manifest_entry(2, 0, "s3://lake/t/data/d.parquet", Some("PARQUET")),
                None,
            ),
            (
                "a delete file carrying position deletes",
                manifest_entry(1, 1, "s3://lake/t/data/e.parquet", Some("PARQUET")),
                None,
            ),
        ];

        for (case, entry, expected) in cases {
            let taken = data_file_of(&entry);
            assert_eq!(
                taken, expected,
                "case {case:?}: expected {expected:?}, got {taken:?}"
            );
        }
    }

    /// The snapshot a document reports as current: the one its `current-snapshot-id` names,
    /// and none for a table that has committed nothing or names an id its list does not
    /// hold.
    #[test]
    fn the_current_snapshot_is_the_one_the_document_names() {
        // (case, the document, the manifest list its current snapshot names)
        let cases: Vec<(&str, ::serde_json::Value, Option<&str>)> = vec![
            (
                "a table nothing has committed to",
                ::serde_json::json!({ "current-snapshot-id": -1, "snapshots": [] }),
                None,
            ),
            (
                "a document carrying no current snapshot at all",
                ::serde_json::json!({ "snapshots": [] }),
                None,
            ),
            (
                "one commit",
                ::serde_json::json!({
                    "current-snapshot-id": 11,
                    "snapshots": [
                        { "snapshot-id": 11, "manifest-list": "s3://lake/t/metadata/snap-11.avro" }
                    ],
                }),
                Some("s3://lake/t/metadata/snap-11.avro"),
            ),
            (
                "the later of two commits",
                ::serde_json::json!({
                    "current-snapshot-id": 12,
                    "snapshots": [
                        { "snapshot-id": 11, "manifest-list": "s3://lake/t/metadata/snap-11.avro" },
                        { "snapshot-id": 12, "manifest-list": "s3://lake/t/metadata/snap-12.avro" },
                    ],
                }),
                Some("s3://lake/t/metadata/snap-12.avro"),
            ),
            (
                "an id the snapshot list does not hold",
                ::serde_json::json!({
                    "current-snapshot-id": 99,
                    "snapshots": [
                        { "snapshot-id": 11, "manifest-list": "s3://lake/t/metadata/snap-11.avro" }
                    ],
                }),
                None,
            ),
        ];

        for (case, document, expected) in cases {
            let actual = current_snapshot(&document)
                .and_then(|snapshot| snapshot.get("manifest-list"))
                .and_then(::serde_json::Value::as_str);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }
}
