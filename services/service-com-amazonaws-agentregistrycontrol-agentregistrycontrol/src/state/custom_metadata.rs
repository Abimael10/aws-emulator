// SPDX-License-Identifier: CC0-1.0

//! Whether a registry record's custom metadata satisfies the custom-metadata schema its
//! registry carries, and how this service words a refusal of metadata that does not.
//!
//! A registry's `CustomMetadataSchemaConfiguration` is "the custom metadata fields that
//! records in the registry can carry. `defaultSchema` defines the common attributes for
//! every record type, and `recordTypeSchemaOverrides` adds fields for individual record
//! types" — each of them "A JSON Schema". So the schemas that apply to one record are the
//! registry's `defaultSchema` and the override whose `recordType` is the record's own, and
//! a record's metadata has to satisfy BOTH: "records of a record type can carry the common
//! attributes defined in `defaultSchema` plus that record type's fields".
//!
//! That makes the configuration a constraint on every write of a record's metadata rather
//! than a document that merely round-trips, and it makes
//! `CustomMetadataSchemaComplianceStatus` a verdict computed against the registry's CURRENT
//! schema: "COMPLIANT means all required fields are present and all values match their
//! declared types. NON_COMPLIANT means the metadata does not satisfy the current schema, for
//! example because the schema was updated after the record was last modified."
//!
//! The verdict is decided in process by the `boon` evaluator, and it is BOUNDED on every
//! axis a caller controls, because both documents are caller-supplied:
//!
//! * size — a schema is a `CustomMetadataSchemaDefinition` (`length 1..16384`) and a
//!   registry carries at most ten overrides beside its default schema;
//! * depth and breadth — a schema deeper than [`MAX_SCHEMA_DEPTH`] or holding more JSON
//!   nodes than [`MAX_SCHEMA_NODES`] yields no verdict, which rules out the nested-combinator
//!   schema whose cost grows with its own depth rather than with its size;
//! * references — a schema carrying more reference keywords than [`MAX_SCHEMA_REFS`] yields
//!   none either: a reference makes the schema a graph rather than a tree, so one subschema
//!   can be walked once per path that reaches it;
//! * steps — a schema node is visited at most once per metadata node per such path, so
//!   [`MAX_EVALUATION_PAIRS`] caps the PRODUCT of the two node counts and the paths the
//!   schema's references unfold into, rather than each factor alone. The budget belongs to
//!   the REQUEST, not to one evaluation ([`SchemaEvaluation`]): an operation that reports a
//!   verdict for a whole page of records spends one budget across the page, so the work does
//!   not grow with how many records the page holds.
//!
//! No reference is resolved outside the schema document itself ([`NoSchemaResources`]), so a
//! `$ref` naming a URL reaches neither the host's filesystem nor any network.
//!
//! A schema this evaluator cannot read or cannot evaluate inside those bounds yields NO
//! verdict: nothing is then known about the metadata, so a write is not refused for it and
//! no compliance status is reported — a status reported for it would be a verdict nothing
//! gave.
//!
//! Where a whole PAGE of records is reported on, the evaluation runs through
//! [`page_compliance_statuses`], and a single record's through [`verdict_of`]: both off the
//! async worker and with no state lock held, under a wall-clock budget as well, so one
//! caller's schema cannot occupy a worker or make every other caller of this service wait on
//! the store. A handler therefore reads what it needs out of the store, releases it, and only
//! then asks for a verdict.

use std::collections::HashMap;
use std::time::Duration;

use ::aws_smithy_types::{Document, Number};
use ::serde_json::{Map, Value};

use crate::ops::types::CustomMetadataSchemaConfiguration;

/// The `CustomMetadataSchemaComplianceStatus` values, as the model lists them
/// (`crate::ops::types::CUSTOM_METADATA_SCHEMA_COMPLIANCE_STATUS_VALUES`), spelled once here
/// because the verdict keys off them.
pub const COMPLIANT: &str = "COMPLIANT";
/// See [`COMPLIANT`].
pub const NON_COMPLIANT: &str = "NON_COMPLIANT";

/// The deepest a registry's schema may nest for this emulator to evaluate it. EMULATOR-chosen:
/// the model bounds a schema's length, not its shape.
const MAX_SCHEMA_DEPTH: usize = 24;

/// The most JSON nodes one of a registry's schemas may hold for this emulator to evaluate it.
/// EMULATOR-chosen, for the same reason as [`MAX_SCHEMA_DEPTH`].
const MAX_SCHEMA_NODES: usize = 4096;

/// The most reference keywords one of a registry's schemas may carry for this emulator to
/// evaluate it. EMULATOR-chosen, for the same reason as [`MAX_SCHEMA_DEPTH`].
const MAX_SCHEMA_REFS: usize = 8;

/// The most `(schema node, metadata node)` pairs ONE REQUEST may have to consider, however
/// many records it reports a verdict for.
///
/// A schema node is visited at most once per metadata node per path the schema's references
/// unfold into, so this bounds the keyword evaluations a request performs — the step budget
/// the evaluator itself does not offer. It is held by [`SchemaEvaluation`], which one request
/// creates once and spends across every record it reports on, so neither a large schema, a
/// large metadata document nor a large page can multiply it. EMULATOR-chosen: the model
/// bounds a schema's length, not the work of applying it.
const MAX_EVALUATION_PAIRS: usize = 1_000_000;

/// How long the verdicts for one PAGE of records may take.
///
/// The work is bounded by [`MAX_EVALUATION_PAIRS`] before it starts; this bounds the WAIT,
/// so a request that is cancelled or times out is not left waiting on a blocking thread that
/// cannot be cancelled. EMULATOR-chosen: no published limit covers it.
const PAGE_CHECK_BUDGET: Duration = Duration::from_secs(5);

/// The deepest a metadata document is walked when it is read as JSON for the evaluation;
/// a branch below this becomes a null, the same bound the schema's own depth is held to.
const MAX_METADATA_DEPTH: usize = MAX_SCHEMA_DEPTH;

/// Every keyword a schema names another subschema with, counted because each of them is what
/// can make the evaluation walk one subschema more than once. All of the forms, not just
/// `$ref`: a schema that DECLARES a dialect is read under that dialect, and `$dynamicRef`
/// (2020-12) and `$recursiveRef` (2019-09) re-apply a subschema to the same node exactly as
/// `$ref` does.
const REFERENCE_KEYWORDS: [&str; 3] = ["$ref", "$dynamicRef", "$recursiveRef"];

/// What applying a registry's custom-metadata schema to a record's metadata reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchemaVerdict {
    /// The metadata satisfies every schema that applies to the record's type.
    Compliant,
    /// It does not: the failure, as `<json pointer of the offending node>: <what failed>`.
    Violation(String),
    /// No schema that applies could be read or evaluated inside the bounds above, so nothing
    /// is known about the metadata.
    NoVerdict,
}

/// One request's evaluation of a registry's custom-metadata schema.
///
/// It owns the two things that have to be shared across the records of one request: the
/// step budget ([`MAX_EVALUATION_PAIRS`]), so the work does not grow with the size of a
/// page, and the compiled schemas, so a schema is parsed and compiled once per record type
/// however many records of that type the request reports on.
pub struct SchemaEvaluation<'a> {
    /// The registry's configuration, or `None` for a registry that declares no schema.
    configuration: Option<&'a CustomMetadataSchemaConfiguration>,
    /// The schemas that apply to a record type, prepared on first use.
    prepared: HashMap<String, Vec<Prepared>>,
    /// What is left of this request's step budget.
    budget: usize,
}

impl<'a> SchemaEvaluation<'a> {
    /// The evaluation one request runs, with the whole step budget to spend.
    pub fn for_request(configuration: Option<&'a CustomMetadataSchemaConfiguration>) -> Self {
        Self {
            configuration,
            prepared: HashMap::new(),
            budget: MAX_EVALUATION_PAIRS,
        }
    }

    /// The verdict this registry's schema gives the `metadata` of a record of `record_type`.
    ///
    /// The schemas that apply are the configuration's `defaultSchema` and the override for
    /// this record type, applied in that order — the common attributes before the record
    /// type's own fields — and the first failure is the one reported, since a refusal carries
    /// one message. Metadata the request did not carry is evaluated as the empty object it
    /// is: a schema that requires a property is not satisfied by a record that carries no
    /// metadata at all.
    pub fn verdict(&mut self, record_type: &str, metadata: Option<&Document>) -> SchemaVerdict {
        let Some(configuration) = self.configuration else {
            // The registry constrains nothing, so there is nothing the metadata can violate.
            return SchemaVerdict::Compliant;
        };
        if !self.prepared.contains_key(record_type) {
            let prepared = prepare(configuration, record_type);
            self.prepared.insert(record_type.to_string(), prepared);
        }
        let Some(prepared) = self.prepared.get(record_type) else {
            return SchemaVerdict::NoVerdict;
        };
        if prepared.is_empty() {
            // No schema applies to this record type, so nothing constrains its metadata.
            return SchemaVerdict::Compliant;
        }
        if self.budget == 0 {
            // This request has spent its steps; the metadata is not even read, so what is
            // left to do cannot grow with the records that follow.
            return SchemaVerdict::NoVerdict;
        }

        let instance = metadata.map_or_else(
            || Value::Object(Map::new()),
            |document| document_as_json(document, 0),
        );
        let instance_nodes = shape(&instance).nodes;
        // Reading the metadata is itself work this request did, so it is charged too.
        self.budget = self.budget.saturating_sub(instance_nodes);

        let mut verdict = SchemaVerdict::Compliant;
        for entry in prepared {
            match entry {
                // A schema that yields nothing is remembered and only reported once every
                // other schema has been satisfied, since what one schema does not say about
                // the metadata another may.
                Prepared::Unevaluable => verdict = SchemaVerdict::NoVerdict,
                Prepared::Ready {
                    schemas,
                    index,
                    unfolded,
                } => {
                    let cost = unfolded.saturating_mul(instance_nodes);
                    if cost > self.budget {
                        tracing::warn!(
                            cost,
                            remaining = self.budget,
                            "applying a registry's custom-metadata schema would take more \
                             steps than this emulator evaluates for one request"
                        );
                        verdict = SchemaVerdict::NoVerdict;
                        continue;
                    }
                    self.budget -= cost;
                    if let Err(failed) = schemas.validate(&instance, *index) {
                        return SchemaVerdict::Violation(failure(&failed, &instance));
                    }
                }
            }
        }
        verdict
    }
}

/// The verdict a registry's schema gives ONE record's metadata, computed the same way a
/// page's are ([`verdicts`]): off the async worker and with no state lock held.
///
/// Every operation that writes or reports on one record goes through this, so a single
/// record's evaluation is bounded on the same axes a page's is — the request's step budget
/// and [`PAGE_CHECK_BUDGET`] — rather than running unbounded on the worker that serves the
/// request.
pub async fn verdict_of(
    configuration: Option<CustomMetadataSchemaConfiguration>,
    record_type: &str,
    metadata: Option<&Document>,
) -> SchemaVerdict {
    verdicts(
        configuration,
        vec![(record_type.to_string(), metadata.cloned())],
    )
    .await
    .into_iter()
    .next()
    // One record was asked about, so one verdict comes back; a missing one claims nothing.
    .unwrap_or(SchemaVerdict::NoVerdict)
}

/// The `customMetadataSchemaComplianceStatus` a `verdict` reads back as: `COMPLIANT`,
/// `NON_COMPLIANT`, or nothing at all where no verdict was reached — a status reported for
/// that would be a verdict nothing gave.
pub fn status_of(verdict: &SchemaVerdict) -> Option<String> {
    match verdict {
        SchemaVerdict::Compliant => Some(COMPLIANT.to_string()),
        SchemaVerdict::Violation(_) => Some(NON_COMPLIANT.to_string()),
        SchemaVerdict::NoVerdict => None,
    }
}

/// The refusal message a `verdict` carries for a write of a record of `record_type`, or
/// `None` where the metadata satisfies the schema (or no verdict was reached).
pub fn refusal_of(verdict: &SchemaVerdict, record_type: &str) -> Option<String> {
    match verdict {
        SchemaVerdict::Violation(failure) => {
            Some(validation_failure_message(record_type, failure))
        }
        SchemaVerdict::Compliant | SchemaVerdict::NoVerdict => None,
    }
}

/// The `customMetadataSchemaComplianceStatus` of every record of one PAGE, in the order they
/// were given: one verdict per `(recordType, customMetadata)` pair.
pub async fn page_compliance_statuses(
    configuration: Option<CustomMetadataSchemaConfiguration>,
    records: Vec<(String, Option<Document>)>,
) -> Vec<Option<String>> {
    verdicts(configuration, records)
        .await
        .iter()
        .map(status_of)
        .collect()
}

/// The verdict this registry's schema gives each of `records`, in the order they were given.
///
/// Called with the configuration and the records CLONED out of the store, so the evaluation
/// holds no lock on it: the verdicts are computed off the async worker (a blocking thread,
/// since the evaluation has no yield point of its own) and under [`PAGE_CHECK_BUDGET`], and
/// they share the one request budget [`SchemaEvaluation`] carries. A set whose verdicts are
/// not reached inside that wait yields NO verdict for any of them, rather than a verdict
/// nothing gave.
async fn verdicts(
    configuration: Option<CustomMetadataSchemaConfiguration>,
    records: Vec<(String, Option<Document>)>,
) -> Vec<SchemaVerdict> {
    let count = records.len();
    let Some(configuration) = configuration else {
        // The registry declares no schema, so no record's metadata is read and no schema is
        // compiled: the verdicts cost nothing and are reached here rather than on a thread
        // of their own. The rule is still the one [`SchemaEvaluation`] states.
        let mut evaluation = SchemaEvaluation::for_request(None);
        return records
            .iter()
            .map(|(record_type, metadata)| evaluation.verdict(record_type, metadata.as_ref()))
            .collect();
    };
    let evaluated = ::tokio::time::timeout(
        PAGE_CHECK_BUDGET,
        ::tokio::task::spawn_blocking(move || {
            let mut evaluation = SchemaEvaluation::for_request(Some(&configuration));
            records
                .iter()
                .map(|(record_type, metadata)| evaluation.verdict(record_type, metadata.as_ref()))
                .collect::<Vec<SchemaVerdict>>()
        }),
    )
    .await;
    match evaluated {
        Ok(Ok(verdicts)) => verdicts,
        // The evaluation did not finish inside the wait, or the thread it ran on did not come
        // back with an answer: nothing is known about these records' metadata, so nothing is
        // claimed about it.
        Ok(Err(_)) | Err(_) => {
            tracing::warn!(
                "the custom-metadata compliance verdicts for {count} registry record(s) did \
                 not answer inside the budget this emulator allows"
            );
            vec![SchemaVerdict::NoVerdict; count]
        }
    }
}

/// How this service refuses a record write whose custom metadata the registry's schema
/// reports non-compliant. The ONE place this sentence is written, so a create and an edit
/// word the same error class the same way.
///
/// Its fixed text names what failed and the record type the schema was selected for; the
/// variable parts are that record type, taken from the request, and the failure itself —
/// the JSON pointer of the offending node in the request's metadata and the constraint of
/// the registry's schema it does not satisfy, both taken from the evaluation that ran.
pub fn validation_failure_message(record_type: &str, failure: &str) -> String {
    format!("Custom metadata validation failed for record type {record_type}: {failure}")
}

/// The compiled schemas are the evaluator's own opaque values, so what is reported is what
/// this evaluation has done with them: the record types it has prepared schemas for, and
/// what is left of the request's step budget.
impl ::std::fmt::Debug for SchemaEvaluation<'_> {
    fn fmt(&self, formatter: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        formatter
            .debug_struct("SchemaEvaluation")
            .field("declares_a_schema", &self.configuration.is_some())
            .field(
                "prepared_record_types",
                &self.prepared.keys().collect::<Vec<&String>>(),
            )
            .field("budget", &self.budget)
            .finish()
    }
}

/// One schema that applies to a record type, as far as it could be made ready to apply.
enum Prepared {
    /// Compiled, with the node count its references unfold into — the factor the cost of
    /// applying it to one metadata node grows with.
    Ready {
        schemas: ::boon::Schemas,
        index: ::boon::SchemaIndex,
        unfolded: usize,
    },
    /// Not a schema this evaluator can read, compile, or evaluate inside the shape bounds
    /// this module documents.
    Unevaluable,
}

/// The schemas that apply to a record of `record_type`, compiled: the registry's
/// `defaultSchema` first, then the override declared for this record type — the common
/// attributes before the record type's own fields.
fn prepare(
    configuration: &CustomMetadataSchemaConfiguration,
    record_type: &str,
) -> Vec<Prepared> {
    let mut prepared: Vec<Prepared> = Vec::new();
    if let Some(schema) = configuration.default_schema.as_deref() {
        prepared.push(compile(schema, DEFAULT_SCHEMA_LABEL));
    }
    if let Some(overrides) = &configuration.record_type_schema_overrides {
        for declared in overrides
            .iter()
            .filter(|declared| declared.record_type == record_type)
        {
            prepared.push(compile(&declared.schema, RECORD_TYPE_SCHEMA_LABEL));
        }
    }
    prepared
}

/// The label the registry's `defaultSchema` is compiled under; see [`schema_url`].
const DEFAULT_SCHEMA_LABEL: &str = "defaultSchema";
/// The label a `recordTypeSchemaOverrides` entry's schema is compiled under; see
/// [`schema_url`].
const RECORD_TYPE_SCHEMA_LABEL: &str = "recordTypeSchemaOverrides";

/// The URL one of a registry's schemas is compiled as a resource under: the member of the
/// configuration it came from, which is the only resource the evaluation resolves.
fn schema_url(label: &str) -> String {
    format!("https://agent-registry.amazonaws.com/customMetadataSchemaConfiguration/{label}")
}

/// Read and compile one of a registry's schema documents, inside the shape bounds this
/// module documents. A document that is not JSON, is no schema this evaluator can compile, or
/// is beyond those bounds yields no verdict about any metadata.
fn compile(document: &str, label: &str) -> Prepared {
    let Ok(schema) = ::serde_json::from_str::<Value>(document) else {
        return Prepared::Unevaluable;
    };
    let measured = shape(&schema);
    if measured.depth > MAX_SCHEMA_DEPTH
        || measured.nodes > MAX_SCHEMA_NODES
        || measured.refs > MAX_SCHEMA_REFS
    {
        tracing::warn!(
            schema_depth = measured.depth,
            schema_nodes = measured.nodes,
            schema_refs = measured.refs,
            "a registry's custom-metadata schema is beyond the shape this emulator evaluates"
        );
        return Prepared::Unevaluable;
    }

    let url = schema_url(label);
    let mut compiler = ::boon::Compiler::new();
    // The only schema resource is the registry's own document; see [`NoSchemaResources`].
    compiler.use_loader(Box::new(NoSchemaResources));
    if compiler.add_resource(&url, schema).is_err() {
        return Prepared::Unevaluable;
    }
    let mut schemas = ::boon::Schemas::new();
    let Ok(index) = compiler.compile(&url, &mut schemas) else {
        return Prepared::Unevaluable;
    };
    Prepared::Ready {
        schemas,
        index,
        // A reference lets one subschema be reached by several paths, so the schema's node
        // count enters the step bound multiplied by the paths its references unfold into.
        unfolded: measured.nodes.saturating_mul(1 << measured.refs),
    }
}

/// The failure a refusal and a `NON_COMPLIANT` verdict are built from: the JSON pointer of
/// the offending node in the metadata, then what the schema wanted there.
///
/// The report is of the DEEPEST first finding — the keyword applied to the most specific
/// node — which is the one that names the member a caller has to change. The root node's
/// pointer is the empty text, as JSON pointers write it.
fn failure(failed: &::boon::ValidationError, instance: &Value) -> String {
    let leaf = deepest(failed);
    let location = leaf.instance_location.to_string();
    let node = instance.pointer(&location);
    format!("{location}: {}", finding(&leaf.kind, node))
}

/// The deepest cause of `failed`: a schema that fails through a reference or a subschema
/// reports the keyword that actually failed under the keyword that led to it. Iterative
/// rather than recursive, so walking a nested report does not grow the stack with it.
fn deepest<'a>(failed: &'a ::boon::ValidationError) -> &'a ::boon::ValidationError<'a, 'a> {
    let mut leaf = failed;
    while let Some(cause) = leaf.causes.first() {
        leaf = cause;
    }
    leaf
}

/// What the schema wanted at the offending node, as this error class words it: the keyword
/// that failed and what it asked for, with the values the schema declares. `node` is the
/// metadata node the keyword was applied to, where the report's pointer resolves to one.
/// Every keyword outside this set is reported with the evaluator's own finding — a finding
/// invented per keyword here would be wording no validator produced.
fn finding(kind: &::boon::ErrorKind, node: Option<&Value>) -> String {
    match kind {
        // The property the schema requires and the metadata does not carry. One per
        // message: a report names the member a caller has to add.
        ::boon::ErrorKind::Required { want } => match want.first() {
            Some(property) => format!("required property '{property}' not found"),
            None => kind.to_string(),
        },
        // The node's own JSON type beside the ones the schema declares. The type is read
        // off the metadata node rather than off the evaluator's report, so a whole number
        // is named the integer JSON Schema counts it as.
        ::boon::ErrorKind::Type { got, want } => format!(
            "{} found, {} expected",
            node.map_or_else(|| got.to_string(), json_type),
            want.iter()
                .map(|allowed| allowed.to_string())
                .collect::<Vec<String>>()
                .join(", ")
        ),
        // The member list the schema enumerates, each value as JSON writes it.
        ::boon::ErrorKind::Enum { want } => format!(
            "does not have a value in the enumeration [{}]",
            want.iter()
                .map(|value| value.to_string())
                .collect::<Vec<String>>()
                .join(", ")
        ),
        other => other.to_string(),
    }
}

/// The JSON type of a metadata node, as JSON Schema names the types a `type` keyword
/// declares. A number with no fractional part is an `integer`, which is the type JSON Schema
/// counts it as whichever way it was written.
fn json_type(node: &Value) -> String {
    match node {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(number) => {
            if number.as_f64().is_some_and(|value| value.fract() == 0.0) {
                "integer"
            } else {
                "number"
            }
        }
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
    .to_string()
}

/// How deep `document` nests, how many JSON nodes it holds and how many reference keywords
/// it carries — what the bounds on one evaluation are decided from. Iterative, and bounded by
/// the document itself: every node is visited once.
fn shape(document: &Value) -> DocumentShape {
    let mut pending: Vec<(&Value, usize)> = vec![(document, 1)];
    let mut shape = DocumentShape::default();
    while let Some((value, level)) = pending.pop() {
        shape.nodes += 1;
        shape.depth = shape.depth.max(level);
        match value {
            Value::Object(members) => {
                shape.refs += members
                    .keys()
                    .filter(|key| REFERENCE_KEYWORDS.contains(&key.as_str()))
                    .count();
                pending.extend(members.values().map(|member| (member, level + 1)));
            }
            Value::Array(items) => {
                pending.extend(items.iter().map(|item| (item, level + 1)));
            }
            _ => {}
        }
    }
    shape
}

/// What [`shape`] reports about one document.
#[derive(Debug, Default)]
struct DocumentShape {
    /// The deepest nesting level in it, the root counting as one.
    depth: usize,
    /// How many JSON nodes it holds.
    nodes: usize,
    /// How many reference keywords it carries — the ones that make a schema a graph.
    refs: usize,
}

/// A record's `customMetadata` as the JSON document a schema is applied to. The walk stops
/// at [`MAX_METADATA_DEPTH`], past which a branch becomes a null, so a deeply nested document
/// cannot grow the stack.
fn document_as_json(document: &Document, depth: usize) -> Value {
    if depth > MAX_METADATA_DEPTH {
        return Value::Null;
    }
    match document {
        Document::Object(members) => Value::Object(
            members
                .iter()
                .map(|(name, member)| (name.clone(), document_as_json(member, depth + 1)))
                .collect(),
        ),
        Document::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| document_as_json(item, depth + 1))
                .collect(),
        ),
        Document::Number(Number::PosInt(value)) => Value::from(*value),
        Document::Number(Number::NegInt(value)) => Value::from(*value),
        Document::Number(Number::Float(value)) => {
            ::serde_json::Number::from_f64(*value).map_or(Value::Null, Value::Number)
        }
        Document::String(value) => Value::String(value.clone()),
        Document::Bool(value) => Value::Bool(*value),
        Document::Null => Value::Null,
    }
}

/// The schema resources a compiled schema resolves a `$ref` against: NOTHING but the
/// document the compilation was handed.
///
/// A registry's schema is a document any caller stores and `$ref` is a dynamic resolution
/// mechanism, so a schema could name a resource by any URL — including a `file:` one.
/// Registering this as the compiler's loader REPLACES the one it comes with, so a reference
/// the compiled document does not satisfy resolves to an error instead of reaching the host's
/// filesystem or a network.
#[derive(Debug)]
struct NoSchemaResources;

impl ::boon::UrlLoader for NoSchemaResources {
    fn load(&self, _url: &str) -> Result<Value, Box<dyn ::std::error::Error>> {
        // The reference itself is part of a `@sensitive` schema document, so it is not
        // written to the log: that a reference was not resolved is what a developer needs
        // here.
        tracing::warn!(
            "a registry's custom-metadata schema references a schema resource outside its \
             own document, which this emulator resolves nothing for"
        );
        Err("this emulator resolves no schema resource outside the registry's own \
             custom-metadata schema document"
            .into())
    }
}
