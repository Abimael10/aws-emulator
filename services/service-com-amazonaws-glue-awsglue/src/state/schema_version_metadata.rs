// SPDX-License-Identifier: CC0-1.0

//! The METADATA key value pairs a schema version carries, and the address every operation
//! over them resolves a version through.
//!
//! A pair is written onto ONE schema version (`PutSchemaVersionMetadata`), removed from it
//! (`RemoveSchemaVersionMetadata`) and read back off it (`QuerySchemaVersionMetadata`), so
//! the pairs live on [`SchemaVersionEntry`] beside the definition that version holds — a
//! version a delete removes takes its metadata with it, and two versions of one schema set
//! carry their own.
//!
//! What a key holds is a LIST of values, not one: the read reports one value for the key
//! plus "other metadata belonging to the same metadata key"
//! (`MetadataInfo$OtherMetadataValueList`), and a put "can be added over one or more calls".
//! The pairs are therefore kept in put order ([`SchemaVersionEntry::metadata`]) and grouped
//! per key on the way out ([`SchemaVersionEntry::metadata_views`]), where the value the key
//! REPORTS is the one most recently put under it and the values it replaced are the others.
//!
//! All three operations address the version the same way — by its own `SchemaVersionId`, or
//! by a `SchemaId` plus which of that schema set's versions is meant — and all three report
//! the same identity of what they acted on (the schema set's ARN and names, the version's
//! id and number, and whether it is the set's latest). That resolution is
//! [`ComAmazonawsGlueAwsglueState::locate_schema_version`], so no two of them can disagree
//! about what a request addressed, and the location it hands back is what a write then
//! reaches mutably ([`ComAmazonawsGlueAwsglueState::schema_version_mut`]).

use super::*;

/// How many metadata key value pairs ONE schema version will hold.
///
/// Stated by `PutSchemaVersionMetadata`: "A maximum of 10 key value pairs will be allowed
/// per schema version. They can be added over one or more calls." It counts PAIRS rather
/// than keys, because a key holds as many values as were put under it, and each of those is
/// one pair of the ten. A put past it is refused with the operation's own modelled
/// `ResourceNumberLimitExceededException`.
pub const MAX_METADATA_PAIRS_PER_SCHEMA_VERSION: usize = 10;

/// One metadata key value pair put on a schema version.
#[derive(Debug, Clone)]
pub struct SchemaVersionMetadataEntry {
    /// `MetadataKeyValuePair$MetadataKey`, "a metadata key". Several entries of one version
    /// may carry the same key, each with its own value.
    pub key: String,
    /// `MetadataKeyValuePair$MetadataValue`, "a metadata key's corresponding value".
    pub value: String,
    /// When the pair was put, reported as `MetadataInfo$CreatedTime` — "the time at which
    /// the entry was created", so it belongs to the pair and not to the key.
    pub created_time: SystemTime,
}

impl SchemaVersionMetadataEntry {
    /// The pair `key`/`value`, put at `now`.
    pub fn new(key: &str, value: &str, now: SystemTime) -> Self {
        Self {
            key: key.to_string(),
            value: value.to_string(),
            created_time: now,
        }
    }

    /// Whether this entry is the pair `key`/`value`. A pair is addressed by BOTH its
    /// members — one key holds several values — so this is what a put's collision and a
    /// remove's miss are decided on.
    pub fn is_pair(&self, key: &str, value: &str) -> bool {
        self.key == key && self.value == value
    }
}

/// One entry of the `MetadataList` a metadata query searches by: "search key-value pairs for
/// metadata, if they are not provided all the metadata information will be returned"
/// (`QuerySchemaVersionMetadataInput$MetadataList`).
///
/// Both members of a search pair are optional, so a pair may name a key, a value, or the
/// pair itself; a stored pair satisfies it when every member the search named agrees.
#[derive(Debug, Clone, Copy, Default)]
pub struct MetadataSearchPair<'a> {
    /// The `MetadataKey` searched for, or `None` for a search that named none.
    pub key: Option<&'a str>,
    /// The `MetadataValue` searched for, or `None` for a search that named none.
    pub value: Option<&'a str>,
}

impl MetadataSearchPair<'_> {
    /// Whether `entry` satisfies this search pair.
    pub fn matches(&self, entry: &SchemaVersionMetadataEntry) -> bool {
        self.key.is_none_or(|key| entry.key == key)
            && self.value.is_none_or(|value| entry.value == value)
    }
}

/// The two members a metadata pair is addressed by, or `None` when the wrapper does not
/// carry both.
///
/// A pair is addressed by BOTH `MetadataKey` and `MetadataValue`, so a write that names one
/// of them names no pair ([`messages::METADATA_PAIR_REQUIRES_BOTH`]). An absent member and
/// one sent as `""` say the same thing about which member the wrapper carried, and `""`
/// satisfies neither member's modelled pattern.
pub fn metadata_pair_members(pair: &MetadataKeyValuePair) -> Option<(&str, &str)> {
    let key = non_empty(pair.metadata_key.as_deref())?;
    let value = non_empty(pair.metadata_value.as_deref())?;
    Some((key, value))
}

/// What ONE metadata key of a schema version holds, as a read reports it: the value the key
/// reports now, and the values put under the same key before it.
#[derive(Debug, Clone)]
pub struct SchemaVersionMetadataView<'a> {
    /// The metadata key, which is the key of this entry in a `MetadataInfoMap`.
    pub key: &'a str,
    /// The pair whose value the key reports as `MetadataInfo$MetadataValue`: the one most
    /// recently put under the key.
    pub current: &'a SchemaVersionMetadataEntry,
    /// The pairs the key also holds, oldest first — "other metadata belonging to the same
    /// metadata key" (`MetadataInfo$OtherMetadataValueList`).
    pub others: Vec<&'a SchemaVersionMetadataEntry>,
}

impl SchemaVersionEntry {
    /// The pair `key`/`value` this version holds, or `None` when it holds no such pair.
    pub fn metadata_pair(&self, key: &str, value: &str) -> Option<&SchemaVersionMetadataEntry> {
        self.metadata.iter().find(|entry| entry.is_pair(key, value))
    }

    /// Whether this version can take another metadata pair, i.e. is below
    /// [`MAX_METADATA_PAIRS_PER_SCHEMA_VERSION`].
    pub fn can_store_metadata_pair(&self) -> bool {
        self.metadata.len() < MAX_METADATA_PAIRS_PER_SCHEMA_VERSION
    }

    /// Put the pair `key`/`value` on this version at `now`.
    ///
    /// The ONE write that adds a pair: it is APPENDED, which keeps [`Self::metadata`] in put
    /// order, so the value a key reports is the one put last ([`Self::metadata_views`]) and
    /// the one it replaces becomes another value of the same key. A pair the version already
    /// holds is the caller's collision to refuse ([`Self::metadata_pair`]), not something
    /// this write deduplicates.
    pub fn put_metadata(&mut self, key: &str, value: &str, now: SystemTime) {
        self.metadata
            .push(SchemaVersionMetadataEntry::new(key, value, now));
    }

    /// Remove the pair `key`/`value` from this version, reporting whether it held one.
    ///
    /// The pair is addressed by both members, so removing one value of a key leaves the
    /// other values of that key where they are.
    pub fn remove_metadata(&mut self, key: &str, value: &str) -> bool {
        let held = self.metadata.len();
        self.metadata.retain(|entry| !entry.is_pair(key, value));
        self.metadata.len() != held
    }

    /// What this version's metadata keys hold, one view per key, in key order — the
    /// `MetadataInfoMap` of a read, before it is paged.
    ///
    /// `search` filters the PAIRS considered: a pair is kept when it satisfies at least one
    /// of the search pairs, and an EMPTY search keeps every pair, because a query that
    /// provides no `MetadataList` reports "all the metadata information". A key is reported
    /// only while at least one of its pairs survives the filter.
    pub fn metadata_views(
        &self,
        search: &[MetadataSearchPair<'_>],
    ) -> Vec<SchemaVersionMetadataView<'_>> {
        let mut by_key: BTreeMap<&str, Vec<&SchemaVersionMetadataEntry>> = BTreeMap::new();
        for entry in &self.metadata {
            if !search.is_empty() && !search.iter().any(|pair| pair.matches(entry)) {
                continue;
            }
            by_key.entry(entry.key.as_str()).or_default().push(entry);
        }
        by_key
            .into_iter()
            .filter_map(|(key, mut held)| {
                // The entries are in put order, so the last is the value the key reports and
                // what is left are the values it replaced, oldest first.
                let current = held.pop()?;
                Some(SchemaVersionMetadataView {
                    key,
                    current,
                    others: held,
                })
            })
            .collect()
    }
}

impl SchemaEntry {
    /// The version this schema set holds under `version_number`, mutably.
    pub fn version_mut(&mut self, version_number: i64) -> Option<&mut SchemaVersionEntry> {
        self.versions
            .iter_mut()
            .find(|version| version.version_number == version_number)
    }
}

/// Why a request addresses no schema version this store holds.
///
/// The two carry the two different refusals every schema-registry operation answers with:
/// a request that does not address a version AT ALL is a bad parameter, and one that
/// addresses a version this endpoint does not hold is a miss.
#[derive(Debug, Clone)]
pub enum SchemaVersionFault {
    /// A bad parameter: the sentence an `InvalidInputException` carries.
    Invalid(String),
    /// A miss: the sentence an `EntityNotFoundException` carries.
    Missing(String),
}

/// WHERE a schema version this store holds lives, and what an operation over it reports
/// about the version it acted on.
///
/// Every member is read off the stored records rather than off the request, so a call that
/// addressed the version by its own id reports the same identity as one that addressed it by
/// the schema set and a version number.
#[derive(Debug, Clone)]
pub struct SchemaVersionLocation {
    /// The `RegistryName` of the registry holding the schema set.
    pub registry_name: String,
    /// The `SchemaName` of the schema set the version was registered against.
    pub schema_name: String,
    /// The version's `VersionNumber`.
    pub version_number: i64,
    /// The version's `SchemaVersionId`.
    pub version_id: String,
    /// `LatestVersion`: whether this version is the latest the schema set holds, which a
    /// later registration against the set moves onto that registration.
    pub latest_version: bool,
}

impl SchemaVersionLocation {
    /// The `SchemaArn` of the schema set this version belongs to, as served in `region` —
    /// minted the way every schema-registry read mints it ([`schema_arn`]).
    pub fn schema_arn(&self, region: &str) -> String {
        schema_arn(region, &self.registry_name, &self.schema_name)
    }
}

impl ComAmazonawsGlueAwsglueState {
    /// The schema version a request addresses, or why it addresses none.
    ///
    /// The ONE resolution the schema-version metadata operations share, so a put, a remove
    /// and a read cannot disagree about what a request named. A version is addressed either
    /// by its own `SchemaVersionId` — which "is the whole address of a version", naming
    /// neither registry nor schema — or by a `SchemaId` resolving a schema set plus a
    /// `SchemaVersionNumber` saying which of its versions is meant, exactly as a version
    /// READ resolves one (`crate::ops::get_schema_version`).
    pub fn locate_schema_version(
        &self,
        schema_id: Option<&SchemaId>,
        version_number: Option<&SchemaVersionNumber>,
        version_id: Option<&str>,
        region: &str,
    ) -> Result<SchemaVersionLocation, SchemaVersionFault> {
        /// A `SchemaId` that resolves to a schema set this endpoint holds none of — the same
        /// wording every schema-registry miss answers.
        fn schema_missing() -> SchemaVersionFault {
            SchemaVersionFault::Missing(messages::SCHEMA_NOT_FOUND.to_string())
        }

        // An absent `SchemaVersionId` and one sent as `""` say the same thing about which
        // member the request addressed a version by, and `""` satisfies the member's
        // modelled pattern no more than it names a version.
        let (schema, version) = match non_empty(version_id) {
            Some(version_id) => match self.schema_version_by_id(version_id) {
                Some(found) => found,
                // The id addresses a version WITHOUT naming a schema set, so what the
                // service reports missing is the schema the id would have belonged to.
                None => {
                    return Err(SchemaVersionFault::Missing(
                        messages::SCHEMA_NOT_FOUND_FOR_VERSION_ID.to_string(),
                    ))
                }
            },
            None => {
                let (Some(schema_id), Some(version_number)) = (schema_id, version_number) else {
                    return Err(SchemaVersionFault::Invalid(
                        SCHEMA_VERSION_REQUIRES_ID_OR_NUMBER.to_string(),
                    ));
                };
                // `SchemaId` may name the schema set by name or by ARN, and a reference that
                // names neither is a bad parameter rather than a miss.
                let reference =
                    schema_reference(schema_id, region).map_err(SchemaVersionFault::Invalid)?;
                // A schema ARN of another account or another Region names a schema of an
                // estate this endpoint does not serve, so it names none here whatever this
                // store holds under that name.
                let (registry_name, schema_name) = match reference {
                    Reference::Local(names) => names,
                    Reference::Elsewhere(_) => return Err(schema_missing()),
                };
                let Some(schema) = self.schema(&registry_name, &schema_name) else {
                    return Err(schema_missing());
                };
                // "The latest version available for the schema" or "the version number of
                // the schema": exactly one of the two names a version, so a value carrying
                // neither names none and one carrying both names two.
                let version = match (version_number.latest_version, version_number.version_number) {
                    (true, None) => schema.latest_version(),
                    (false, Some(number)) => schema.version(number),
                    _ => {
                        return Err(SchemaVersionFault::Invalid(
                            SCHEMA_VERSION_NUMBER_REQUIRES_ONE.to_string(),
                        ))
                    }
                };
                let Some(version) = version else {
                    return Err(SchemaVersionFault::Missing(schema_version_miss(
                        schema_id,
                        version_number,
                    )));
                };
                (schema, version)
            }
        };

        Ok(SchemaVersionLocation {
            registry_name: schema.registry_name.clone(),
            schema_name: schema.name.clone(),
            version_number: version.version_number,
            version_id: version.version_id.clone(),
            latest_version: schema
                .latest_version()
                .is_some_and(|latest| latest.version_id == version.version_id),
        })
    }

    /// The version a [`SchemaVersionLocation`] names \u2014 what a read projects after the
    /// location was resolved.
    pub fn schema_version(&self, location: &SchemaVersionLocation) -> Option<&SchemaVersionEntry> {
        self.schema(&location.registry_name, &location.schema_name)
            .and_then(|schema| schema.version(location.version_number))
    }

    /// The version a [`SchemaVersionLocation`] names, mutably — what a write reaches after
    /// the location was resolved.
    pub fn schema_version_mut(
        &mut self,
        location: &SchemaVersionLocation,
    ) -> Option<&mut SchemaVersionEntry> {
        self.schema_mut(&location.registry_name, &location.schema_name)
            .and_then(|schema| schema.version_mut(location.version_number))
    }
}
