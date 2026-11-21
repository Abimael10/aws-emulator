// SPDX-License-Identifier: CC0-1.0

//! The rules the INDEX-MANAGEMENT operations (`CreateIndex`, `GetIndex`, `UpdateIndex`,
//! `DeleteIndex`) share, beside the index record itself ([`super::indexes`]):
//!
//! * whether the domain the request addresses serves these operations at all
//!   ([`index_management_refusal`]) — they configure an index's mappings, settings and
//!   semantic enrichment on the domain's own engine, so a domain running an engine that has
//!   none of that cannot serve them;
//! * whether the `IndexSchema` a write carries is a LEGAL index schema
//!   ([`index_schema_violation`]) — the document is a modelled `document`, so what its
//!   sections, its field mappings and its settings have to look like is the service's own
//!   rule rather than a constraint the request boundary can apply;
//! * how a READ reports the schema of one index ([`reported_index_schema`]), which mirrors
//!   the engine's own mapping API: the report is keyed by the index the request named.
//!
//! Every rule here is a pure function of the request's own values and of the stored record,
//! so the same request decides the same way twice and nothing here writes.

use ::serde_json::Value;

use super::{
    engine_type_of, index_settings, json_from_document, upgrade_rank, Domain,
    ENGINE_TYPE_OPENSEARCH, SCHEMA_MAPPINGS, SCHEMA_SETTINGS, SETTINGS_INDEX_SECTION,
};
use crate::ops::types::{DependencyFailureException, InternalException, ValidationException};

/// The engine that serves the index-management operations: they write an index's mappings,
/// its settings and its SEMANTIC ENRICHMENT configuration on the domain's own engine, and
/// the semantic field type that configuration describes is the OpenSearch engine's — the
/// Elasticsearch engine this service also offers has none of it at any release it publishes,
/// so a domain running that engine serves none of these operations.
pub const INDEX_MANAGEMENT_ENGINE_TYPE: &str = ENGINE_TYPE_OPENSEARCH;

/// The oldest OpenSearch release that serves them: these operations configure an index's
/// AUTOMATIC SEMANTIC ENRICHMENT, which is built on the engine's own `semantic` field type
/// ([`MAPPING_FIELD_TYPES`]) — a mapper the engine added in its THIRD major generation — so
/// this is the first `OpenSearch_3` release the service offers
/// ([`super::SUPPORTED_ENGINE_VERSIONS`]) and a domain on any earlier release is behind the
/// software this feature needs, whichever of the two engines it runs.
pub const INDEX_MANAGEMENT_ENGINE_VERSION: &str = "OpenSearch_3.1";

/// Whether a domain running `engine_version` serves the index-management operations: its
/// engine has to be [`INDEX_MANAGEMENT_ENGINE_TYPE`], at
/// [`INDEX_MANAGEMENT_ENGINE_VERSION`] or later.
///
/// Read off the `EngineVersion` through [`engine_type_of`] and [`upgrade_rank`], the two
/// readings this service makes of which engine a version names and how two releases of it
/// compare, so a release added to the published catalog needs nothing restated here.
#[must_use]
pub fn serves_index_management(engine_version: &str) -> bool {
    if engine_type_of(engine_version) != Some(INDEX_MANAGEMENT_ENGINE_TYPE) {
        return false;
    }
    match (
        upgrade_rank(engine_version),
        upgrade_rank(INDEX_MANAGEMENT_ENGINE_VERSION),
    ) {
        (Some(running), Some(required)) => running >= required,
        // A value that names no release this service compares is not one it offers the
        // feature on either.
        _ => false,
    }
}

/// The sentence the index-management gate is refused with, for every operation it gates and
/// every engine version it gates them on ([`INDEX_MANAGEMENT_UNSUPPORTED`]).
///
/// It is the feature these operations configure that the software cannot serve, so the
/// sentence names that feature rather than the domain or the release it runs: the whole
/// message, with no variable part. One text for the four operations and for both engines, so
/// a client can tell this gate from every other `ValidationException` they raise.
///
/// It is NOT the sentence the PACKAGE operations' own gate states
/// ([`super::DOMAIN_NOT_ON_LATEST_SOFTWARE_VERSION`]): those are two gates over two features,
/// each recorded with its own text.
pub const INDEX_MANAGEMENT_UNSUPPORTED: &str =
    "Automatic Semantic Enrichment feature is not supported on this software version.";

/// That sentence as the refusal.
pub fn index_management_unavailable() -> ValidationException {
    ValidationException {
        message: Some(INDEX_MANAGEMENT_UNSUPPORTED.to_string()),
    }
}

/// The refusal an index-management operation answers a domain whose engine does not serve
/// those operations with, or `None` for a domain that does.
///
/// Decided from the DOMAIN alone, so it is answered before the index the request names is
/// looked up: an operation the domain does not offer has not reached the index, and reporting
/// a missing index for it would send a caller to create one it could not then manage.
pub fn index_management_refusal(domain: &Domain) -> Option<ValidationException> {
    (!serves_index_management(&domain.engine_version)).then(index_management_unavailable)
}

// ---------------------------------------------------------------------------
// What a LEGAL index schema looks like
// ---------------------------------------------------------------------------

/// What is wrong with an `IndexSchema`, and which channel the service answers it on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexSchemaViolation {
    /// The sentence naming the offending part of the document and the value standing there.
    pub message: String,
    /// Whether what offends is a MAPPING section whose structure the engine's mapping parser
    /// cannot read at all — a value standing where a set of members belongs — as against a
    /// document it reads and then refuses because of what it names.
    ///
    /// The two are answered on different channels where the operation MERGES the mapping
    /// onto the one the index already carries ([`crate::ops::update_index`]): a mapping the
    /// parser cannot read leaves the service's own work unfinished, which is its
    /// `InternalException`, while a mapping that is read and names something the engine has
    /// no mapper for is the caller's input, which is its `ValidationException`.
    pub unreadable_mapping: bool,
}

/// The service's own sentence for a request its processing failed on: "request processing
/// failed because of an unknown error, exception, or internal failure" is what the modelled
/// `InternalException` reports, and this is the whole message it carries — fixed text with no
/// variable part, because nothing about the request is what a caller can act on: the only
/// remedy it states is to try again.
pub const INTERNAL_SERVICE_ERROR: &str = "Internal service error. Please retry.";

impl IndexSchemaViolation {
    /// This violation as the refusal of the REQUEST — what a write answers for a document
    /// whose offending part the caller can fix.
    pub fn validation(&self) -> ValidationException {
        ValidationException {
            message: Some(self.message.clone()),
        }
    }

    /// This violation as the SERVER's failure: a mapping its own parser could not read
    /// leaves the service's work unfinished, which it reports as the internal error it is
    /// ([`INTERNAL_SERVICE_ERROR`]) — the offending path is not part of that sentence,
    /// because an internal failure states nothing about the request.
    pub fn internal(&self) -> InternalException {
        InternalException {
            message: Some(INTERNAL_SERVICE_ERROR.to_string()),
        }
    }
}

/// The fixed sentence `UpdateIndex` refuses a schema carrying an index SETTINGS section
/// with: the operation "allows modification of field mappings and semantic search settings
/// for text fields", so the settings of the index itself are not something it writes — they
/// are chosen when the index is created and changed through the index's own settings API.
///
/// The whole message, with no variable part: WHICH setting the schema named is not what it
/// states, since no setting can be updated through this call.
pub const INDEX_SETTINGS_UPDATE_UNSUPPORTED: &str = "Updates to index settings is not supported.";

/// That sentence as the refusal.
pub fn index_settings_update_unsupported() -> ValidationException {
    ValidationException {
        message: Some(INDEX_SETTINGS_UPDATE_UNSUPPORTED.to_string()),
    }
}

/// How the service states that the domain a call was for could not serve it, and that the
/// condition passes.
///
/// The whole message, with no variable part: what a caller can act on is that the domain
/// could not take the work and that trying again is the remedy, so nothing of the request
/// stands in it — the same shape as the service's other dependency sentence
/// ([`super::NODE_INFORMATION_UNAVAILABLE`]).
pub const DOMAIN_UNAVAILABLE: &str = "The domain is not available now, please try again later.";

/// The `DependencyFailureException` payload for work the domain's own cluster could not be
/// handed.
///
/// `DependencyFailureException` is "an exception for when a failure in one of the
/// dependencies results in the service being unable to fetch details about the resource"
/// (`@httpError 424`): the cluster is that dependency, so work it cannot take is reported as
/// the dependency failure it is rather than as a verdict on the request — which is also why
/// a caller may retry it.
pub fn domain_unavailable() -> DependencyFailureException {
    DependencyFailureException {
        message: Some(DOMAIN_UNAVAILABLE.to_string()),
    }
}

/// The analysis component name an index write cannot hand the domain's cluster, or `None`
/// where `schema` defines no such name.
///
/// ONE statement of the rule both index writes apply (`crate::ops::create_index` and
/// `crate::ops::update_index`), read off the analysis sections wherever the document carries
/// them ([`super::analysis::IndexAnalysis::unnameable_component`], which states what makes a
/// name one the cluster cannot take) — so a schema defining one is refused by both writes
/// alike, before either stores anything.
#[must_use]
pub fn unnameable_analysis_component(schema: &::aws_smithy_types::Document) -> Option<String> {
    super::analysis::IndexAnalysis::of(schema)
        .unnameable_component()
        .map(str::to_owned)
}

/// Whether `schema` names an index SETTINGS section at all — what
/// [`index_settings_update_unsupported`] is answered for.
///
/// Read off the document itself rather than off the settings it resolves to, because a
/// section naming no setting is still a settings update the operation does not serve, and a
/// section that is not a set of members at all is one too.
#[must_use]
pub fn names_index_settings(schema: &::aws_smithy_types::Document) -> bool {
    match schema {
        ::aws_smithy_types::Document::Object(sections) => sections.contains_key(SCHEMA_SETTINGS),
        _ => false,
    }
}

/// The member of a field's mapping that names its TYPE.
const MAPPING_TYPE: &str = "type";

/// The member of a mapping section that holds the fields under it.
const MAPPING_PROPERTIES: &str = "properties";

/// The member of a field's mapping that holds its MULTI-FIELDS: the same value mapped a
/// second way (`title.raw`), each entry of which is a field mapping of its own.
const MAPPING_FIELDS: &str = "fields";

/// The member of a mapping section that says what the engine does with a field the mappings
/// do not map.
const MAPPING_DYNAMIC: &str = "dynamic";

/// The values `dynamic` takes: whether an unmapped field is added to the mappings
/// (`true`), ignored (`false`), refused (`strict`) or indexed as a runtime field
/// (`runtime`) — the engine's own set for that setting, which it also accepts spelled as a
/// boolean.
const DYNAMIC_VALUES: [&str; 4] = ["true", "false", "strict", "runtime"];

/// The field types a mapping may name, from the engine's own documented catalog (OpenSearch
/// "Supported field types"): a name outside it is a type the engine has no mapper for, so a
/// mapping naming one could never be built.
///
/// Grouped the way that catalog groups them — strings, numbers, dates, booleans, binary,
/// ranges, objects, autocomplete, geographic, IP, ranking, vector and the mapping-only types
/// — so an entry is read against the group it belongs to.
pub const MAPPING_FIELD_TYPES: [&str; 48] = [
    // Strings
    "text",
    "keyword",
    "match_only_text",
    "constant_keyword",
    "wildcard",
    "token_count",
    // Numbers
    "byte",
    "short",
    "integer",
    "long",
    "unsigned_long",
    "float",
    "half_float",
    "double",
    "scaled_float",
    // Dates
    "date",
    "date_nanos",
    // Booleans and binary
    "boolean",
    "binary",
    // Ranges
    "integer_range",
    "long_range",
    "float_range",
    "double_range",
    "date_range",
    "ip_range",
    // Objects and relations
    "object",
    "nested",
    "flat_object",
    "join",
    // Autocomplete
    "completion",
    "search_as_you_type",
    // Geographic
    "geo_point",
    "geo_shape",
    "xy_point",
    "xy_shape",
    // IP
    "ip",
    // Ranking
    "rank_feature",
    "rank_features",
    // Vector and semantic
    "knn_vector",
    "sparse_vector",
    "semantic",
    // Aggregates and derived values
    "aggregate_metric_double",
    "histogram",
    "derived",
    "star_tree",
    // Mapping-only types
    "alias",
    "percolator",
    "version",
];

/// The index settings whose value is a WHOLE NUMBER, by the name each has below
/// [`SETTINGS_INDEX_SECTION`] (the engine's documented index settings): a value that is
/// neither a number nor the text of one is no value for the setting at all.
const NUMERIC_INDEX_SETTINGS: [&str; 13] = [
    "number_of_shards",
    "number_of_replicas",
    "number_of_routing_shards",
    "routing_partition_size",
    "max_result_window",
    "max_inner_result_window",
    "max_rescore_window",
    "max_docvalue_fields_search",
    "max_script_fields",
    "max_ngram_diff",
    "max_shingle_diff",
    "max_refresh_listeners",
    "max_terms_count",
];

/// The index settings whose value is a BOOLEAN, read the same way: the engine takes `true`
/// and `false` and the texts spelling them, and nothing else.
const BOOLEAN_INDEX_SETTINGS: [&str; 8] = [
    "hidden",
    "knn",
    "blocks.read_only",
    "blocks.read",
    "blocks.write",
    "blocks.metadata",
    "blocks.read_only_allow_delete",
    "requests.cache.enable",
];

/// What a refusal of a malformed `IndexSchema` names the offending part of the document by:
/// the member the operation takes the document in, so the path reads as the caller wrote it
/// (`IndexSchema.mappings.properties.title.type`).
const SCHEMA_MEMBER: &str = "IndexSchema";

/// The refusal for an `IndexSchema` that is not a legal index schema, or `None` for one
/// that is.
///
/// What makes a schema legal is what the engine can BUILD an index from:
///
/// * its sections are sets of members — `mappings` names the index's mapping and `settings`
///   its settings, so a scalar or a list standing where one of them belongs names no section;
/// * every field mapping is a set of members, nested under `properties` (the fields of an
///   object field) or `fields` (the same value mapped a second way);
/// * a field's `type` names one of the engine's own field types ([`MAPPING_FIELD_TYPES`]) —
///   a name it has no mapper for could never be applied;
/// * `dynamic` carries one of its four values ([`DYNAMIC_VALUES`]);
/// * a setting the engine reads as a number or as a boolean carries one
///   ([`NUMERIC_INDEX_SETTINGS`], [`BOOLEAN_INDEX_SETTINGS`]).
///
/// The sentence names the PATH of the offending part and the value standing there, because
/// which part of the document to fix is what a caller has to act on. The whole document is
/// judged before it is stored or merged, so a refused write leaves the index exactly as it
/// was.
///
/// Walked iteratively, so a deeply nested document costs no stack, and the least offending
/// path is the one reported, so the same document is always refused with the same sentence.
pub fn index_schema_violation(
    schema: &::aws_smithy_types::Document,
) -> Option<IndexSchemaViolation> {
    let document = json_from_document(schema);
    let root = document.as_object()?;
    // The least offending path is kept, so the same document is always refused with the
    // same sentence however its members were iterated.
    let mut least: Option<IndexSchemaViolation> = None;

    // The two sections of the schema that are sets of members. The semantic enrichment
    // configuration is a section of its own whose members the engine reads as they come, so
    // nothing is stated about it here.
    for section in [SCHEMA_MAPPINGS, SCHEMA_SETTINGS] {
        let Some(value) = root.get(section) else {
            continue;
        };
        if !value.is_object() {
            keep(
                &mut least,
                IndexSchemaViolation {
                    message: must_be_object(
                        &format!("{SCHEMA_MEMBER}.{section}"),
                        value,
                        match section {
                            SCHEMA_MAPPINGS => "the index's mapping sections",
                            _ => "the index's settings",
                        },
                    ),
                    unreadable_mapping: section == SCHEMA_MAPPINGS,
                },
            );
        }
    }

    if let Some(mappings) = root.get(SCHEMA_MAPPINGS).and_then(Value::as_object) {
        // Each pending entry is one mapping SECTION — the root mapping, or the mapping of a
        // field that holds further fields — with the path it stands at.
        let mut pending = vec![(format!("{SCHEMA_MEMBER}.{SCHEMA_MAPPINGS}"), mappings)];
        while let Some((path, section)) = pending.pop() {
            if let Some(dynamic) = section.get(MAPPING_DYNAMIC) {
                if !reads_as_dynamic(dynamic) {
                    keep(
                        &mut least,
                        IndexSchemaViolation {
                            message: format!(
                                "{path}.{MAPPING_DYNAMIC} must be one of {}, not {}.",
                                DYNAMIC_VALUES.join(", "),
                                rendered(dynamic)
                            ),
                            unreadable_mapping: false,
                        },
                    );
                }
            }
            for container in [MAPPING_PROPERTIES, MAPPING_FIELDS] {
                let Some(value) = section.get(container) else {
                    continue;
                };
                let path = format!("{path}.{container}");
                let Some(fields) = value.as_object() else {
                    keep(
                        &mut least,
                        IndexSchemaViolation {
                            message: must_be_object(&path, value, "the fields it maps"),
                            unreadable_mapping: true,
                        },
                    );
                    continue;
                };
                for (field, mapping) in fields {
                    let path = format!("{path}.{field}");
                    let Some(mapping) = mapping.as_object() else {
                        keep(
                            &mut least,
                            IndexSchemaViolation {
                                message: must_be_object(&path, mapping, "that field's mapping"),
                                unreadable_mapping: true,
                            },
                        );
                        continue;
                    };
                    if let Some(kind) = mapping.get(MAPPING_TYPE) {
                        match kind.as_str() {
                            Some(named) if MAPPING_FIELD_TYPES.contains(&named) => {}
                            _ => keep(
                                &mut least,
                                IndexSchemaViolation {
                                    message: format!(
                                        "{path}.{MAPPING_TYPE} must name one of the field types \
                                         this engine maps, not {}.",
                                        rendered(kind)
                                    ),
                                    unreadable_mapping: false,
                                },
                            ),
                        }
                    }
                    pending.push((path, mapping));
                }
            }
        }
    }

    // The settings are judged as the ONE section every reader of an index's settings sees
    // ([`index_settings`]), so a setting written flattened, written under `index` or written
    // directly is held to the same rule.
    let settings = index_settings(schema);
    let settings = Value::Object(settings);
    for (name, numeric) in [
        (NUMERIC_INDEX_SETTINGS.as_slice(), true),
        (BOOLEAN_INDEX_SETTINGS.as_slice(), false),
    ]
    .into_iter()
    .flat_map(|(names, numeric)| names.iter().map(move |name| (*name, numeric)))
    {
        let Some(value) = setting_at(&settings, name) else {
            continue;
        };
        let legal = if numeric {
            reads_as_whole_number(value)
        } else {
            reads_as_boolean(value)
        };
        if !legal {
            keep(
                &mut least,
                IndexSchemaViolation {
                    message: format!(
                        "{SCHEMA_MEMBER}.{SCHEMA_SETTINGS}.{SETTINGS_INDEX_SECTION}.{name} must \
                         be {}, not {}.",
                        if numeric { "a whole number" } else { "a boolean" },
                        rendered(value)
                    ),
                    unreadable_mapping: false,
                },
            );
        }
    }

    least
}

/// Keep `violation` as the one reported, where its path is the least seen: the refusal
/// states ONE offending part of the document, and which one is decided by the path rather
/// than by the order the document was walked in.
fn keep(least: &mut Option<IndexSchemaViolation>, violation: IndexSchemaViolation) {
    if least
        .as_ref()
        .is_none_or(|held| violation.message < held.message)
    {
        *least = Some(violation);
    }
}

/// The sentence for a part of the document that has to be a set of members and is not:
/// `what` names what those members would be.
fn must_be_object(path: &str, value: &Value, what: &str) -> String {
    format!(
        "{path} must be a JSON object naming {what}, not {}.",
        rendered(value)
    )
}

/// The value standing at the offending path, as a refusal quotes it: its own JSON text,
/// which is what a caller sent.
fn rendered(value: &Value) -> String {
    match value {
        Value::Object(_) => "a JSON object".to_string(),
        Value::Array(_) => "a JSON array".to_string(),
        other => other.to_string(),
    }
}

/// Whether `value` is one of `dynamic`'s values, spelled as the text of one or as the
/// boolean the engine also takes.
fn reads_as_dynamic(value: &Value) -> bool {
    match value {
        Value::Bool(_) => true,
        Value::String(text) => DYNAMIC_VALUES.contains(&text.as_str()),
        _ => false,
    }
}

/// Whether `value` is a whole number, or the text of one: "settings are returned as
/// strings", so the engine reads a setting written either way.
fn reads_as_whole_number(value: &Value) -> bool {
    match value {
        Value::Number(number) => number.is_i64() || number.is_u64(),
        Value::String(text) => text.trim().parse::<i64>().is_ok(),
        _ => false,
    }
}

/// Whether `value` is a boolean, or the text of one — read the same way.
fn reads_as_boolean(value: &Value) -> bool {
    match value {
        Value::Bool(_) => true,
        Value::String(text) => matches!(text.trim(), "true" | "false"),
        _ => false,
    }
}

/// The value the setting `name` stands at in the settings section `settings`, where it
/// carries one: the name is the dotted path of the sections it sits under, which is how
/// [`index_settings`] holds every setting whichever spelling a request wrote it in.
fn setting_at<'a>(settings: &'a Value, name: &str) -> Option<&'a Value> {
    let mut node = settings;
    for section in name.split('.') {
        node = node.as_object()?.get(section)?;
    }
    Some(node)
}

// ---------------------------------------------------------------------------
// How a READ reports one index's schema
// ---------------------------------------------------------------------------

/// The schema of the index `index_name` as `GetIndex` reports it: the stored document keyed
/// by the index's own name.
///
/// `GetIndex` answers "the JSON schema of the index including mappings, settings, and
/// semantic enrichment configuration", and it mirrors the engine's own mapping API, which
/// reports every index it was asked about under the name that index is addressed by — so one
/// index is reported as `{"<indexName>": {"mappings": …, "settings": …}}`, not as the bare
/// set of sections a write hands over. That is also what makes the report READABLE by a
/// client that asked about an index by name: the answer says which index each schema belongs
/// to.
///
/// What stands UNDER that name is the stored document itself, section for section: the write
/// that created the index persisted it and every accepted update merged onto it, so this read
/// reports the index as those calls left it.
pub fn reported_index_schema(
    index_name: &str,
    schema: &::aws_smithy_types::Document,
) -> ::aws_smithy_types::Document {
    use ::aws_smithy_types::Document;
    use ::std::collections::HashMap;

    Document::Object(HashMap::from([(
        index_name.to_string(),
        schema.clone(),
    )]))
}
