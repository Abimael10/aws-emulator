// SPDX-License-Identifier: CC0-1.0

//! Whether a replacement custom-metadata schema configuration may take the place of the one
//! a registry already carries, and how this service words a refusal of one that may not.
//!
//! A registry's `customMetadataSchemaConfiguration` is "the new custom metadata schema
//! configuration, which replaces the current one. You can add fields and allowed values, but
//! you can't remove a field or an allowed value, or change a field's type." So the
//! configuration a registry carries may only ever be WIDENED: the replacement has to go on
//! declaring every field the current one declares, with the same declared type and with at
//! least the allowed values it enumerates, and a replacement that withdraws any of the three
//! is refused before the registry is written.
//!
//! The rule applies per SCOPE, because that is what a schema's fields are scoped to:
//! `defaultSchema` declares "fields that records of every record type can carry" and each
//! `recordTypeSchemaOverrides` entry declares "the fields for individual record types". A
//! field dropped from a scope is withdrawn from the records that scope covers whether or not
//! some other scope still declares it, so each of the current configuration's scopes is
//! compared with the same scope of the replacement — and a scope the replacement does not
//! carry at all withdraws every field it declared.
//!
//! What a schema declares is read from its `properties`, the keyword a JSON Schema names its
//! fields in, walked to the leaves so a field nested inside an object-valued field counts as
//! a declared field too. The walk is bounded by [`MAX_SCHEMA_NODES`] on top of the model's
//! own `length 1..16384` per schema and its maximum of ten overrides, since both documents
//! are caller-supplied.
//!
//! A schema this module cannot read — one that is not a JSON document — declares no field set
//! anybody knows, so nothing is refused for it: the same stance
//! [`super::custom_metadata`] takes on a schema it cannot evaluate.

use std::collections::BTreeMap;

use ::serde_json::Value;

use crate::ops::types::CustomMetadataSchemaConfiguration;

/// The keyword a JSON Schema names its fields in.
const PROPERTIES_KEYWORD: &str = "properties";

/// The keyword a JSON Schema declares a field's type in.
const TYPE_KEYWORD: &str = "type";

/// The keyword a JSON Schema enumerates a field's allowed values in.
const ENUM_KEYWORD: &str = "enum";

/// The configuration member carrying the schema that applies to every record type, as the
/// model names it.
const DEFAULT_SCHEMA_LABEL: &str = "defaultSchema";

/// The configuration member carrying the per-record-type schemas, as the model names it.
const RECORD_TYPE_SCHEMA_LABEL: &str = "recordTypeSchemaOverrides";

/// The most JSON nodes one schema may hold for this comparison to walk it. EMULATOR-chosen:
/// the model bounds a schema's length, not its shape.
const MAX_SCHEMA_NODES: usize = 4_096;

/// What one schema declares about one of its fields: the type it gives it and the values it
/// enumerates as allowed, each as the schema wrote it, or nothing where the schema says
/// nothing about it.
#[derive(Debug, Default)]
struct Declaration {
    declared_type: Option<Value>,
    allowed_values: Option<Vec<Value>>,
}

/// One scope of a configuration: the schema every record type is held to, or the schema one
/// record type is held to.
#[derive(Debug)]
enum Scope {
    EveryRecordType,
    RecordType(String),
}

impl Scope {
    /// How a refusal names this scope: the configuration member it is carried in, and for a
    /// per-record-type schema the record type it applies to.
    fn label(&self) -> String {
        match self {
            Scope::EveryRecordType => DEFAULT_SCHEMA_LABEL.to_string(),
            Scope::RecordType(record_type) => {
                format!("{RECORD_TYPE_SCHEMA_LABEL} for record type {record_type}")
            }
        }
    }
}

/// What a replacement schema takes away from the one it replaces.
#[derive(Debug)]
enum Withdrawal {
    /// The replacement no longer declares a field the current schema declares.
    Field { path: String },
    /// The replacement declares a different type for a field than the current schema does.
    Type { path: String, from: Value, to: Value },
    /// The replacement no longer admits a value the current schema enumerates for a field.
    AllowedValue { path: String, value: Value },
}

/// The refusal message for a `wanted` custom-metadata schema configuration that withdraws
/// something the registry's `current` one declares, or `None` when it keeps every field,
/// every field's type and every allowed value — which is what "you can add fields and
/// allowed values, but you can't remove a field or an allowed value, or change a field's
/// type" admits.
///
/// Both are the whole configuration, so the first scope that loses something is the one
/// reported: a registry that carries no configuration yet has nothing to withdraw, and a
/// replacement that drops a scope withdraws the fields that scope declared.
pub fn refusal(
    current: Option<&CustomMetadataSchemaConfiguration>,
    wanted: Option<&CustomMetadataSchemaConfiguration>,
) -> Option<String> {
    let current = current?;
    scopes(current).into_iter().find_map(|(scope, schema)| {
        let declared = declarations(schema)?;
        let replacement = match wanted.and_then(|wanted| schema_of(wanted, &scope)) {
            // The replacement carries this scope: what it declares is compared field by
            // field. A schema that cannot be read declares nothing anybody knows, so
            // nothing is refused for it.
            Some(replacement) => declarations(replacement)?,
            // The replacement carries no schema for this scope at all, so every field the
            // current one declares is withdrawn from the records it covers.
            None => BTreeMap::new(),
        };
        let withdrawal = withdrawal(&declared, &replacement)?;
        Some(message(&scope, &withdrawal))
    })
}

/// Every scope `configuration` declares a schema in, with that schema.
fn scopes(configuration: &CustomMetadataSchemaConfiguration) -> Vec<(Scope, &str)> {
    let mut scopes: Vec<(Scope, &str)> = Vec::new();
    if let Some(schema) = configuration.default_schema.as_deref() {
        scopes.push((Scope::EveryRecordType, schema));
    }
    for per_type in configuration.record_type_schema_overrides.iter().flatten() {
        scopes.push((
            Scope::RecordType(per_type.record_type.clone()),
            per_type.schema.as_str(),
        ));
    }
    scopes
}

/// The schema `configuration` declares in `scope`, or `None` when it declares none there.
fn schema_of<'a>(
    configuration: &'a CustomMetadataSchemaConfiguration,
    scope: &Scope,
) -> Option<&'a str> {
    match scope {
        Scope::EveryRecordType => configuration.default_schema.as_deref(),
        Scope::RecordType(record_type) => configuration
            .record_type_schema_overrides
            .iter()
            .flatten()
            .find(|per_type| &per_type.record_type == record_type)
            .map(|per_type| per_type.schema.as_str()),
    }
}

/// What `schema` declares, keyed by the dotted path of the field — so a field nested inside
/// an object-valued field is a declared field of its own. `None` where the schema is not a
/// JSON document, or holds more nodes than this walk admits: nothing is then known about
/// what it declares.
fn declarations(schema: &str) -> Option<BTreeMap<String, Declaration>> {
    let document: Value = ::serde_json::from_str(schema).ok()?;
    let mut declared: BTreeMap<String, Declaration> = BTreeMap::new();
    let mut pending: Vec<(String, &Value)> = vec![(String::new(), &document)];
    let mut nodes: usize = 0;
    while let Some((prefix, node)) = pending.pop() {
        nodes += 1;
        if nodes > MAX_SCHEMA_NODES {
            return None;
        }
        let Some(properties) = node.get(PROPERTIES_KEYWORD).and_then(Value::as_object) else {
            continue;
        };
        for (field, subschema) in properties {
            let path = if prefix.is_empty() {
                field.clone()
            } else {
                format!("{prefix}.{field}")
            };
            declared.insert(
                path.clone(),
                Declaration {
                    declared_type: subschema.get(TYPE_KEYWORD).cloned(),
                    allowed_values: subschema
                        .get(ENUM_KEYWORD)
                        .and_then(Value::as_array)
                        .cloned(),
                },
            );
            pending.push((path, subschema));
        }
    }
    Some(declared)
}

/// The first thing `replacement` takes away from `declared`, or `None` when it takes nothing
/// away. The fields are walked in path order, so the same pair of schemas always reports the
/// same one.
fn withdrawal(
    declared: &BTreeMap<String, Declaration>,
    replacement: &BTreeMap<String, Declaration>,
) -> Option<Withdrawal> {
    declared.iter().find_map(|(path, declaration)| {
        let Some(replaced) = replacement.get(path) else {
            return Some(Withdrawal::Field { path: path.clone() });
        };
        // A field the replacement declares with another type is a changed type; a
        // replacement that says nothing about the type says nothing the current schema's
        // type contradicts, so it is not a change.
        if let (Some(from), Some(to)) = (&declaration.declared_type, &replaced.declared_type) {
            if from != to {
                return Some(Withdrawal::Type {
                    path: path.clone(),
                    from: from.clone(),
                    to: to.clone(),
                });
            }
        }
        // Enumerating fewer values than the current schema does removes an allowed value;
        // enumerating none at all admits every value, which removes none.
        let enumerated = declaration.allowed_values.as_ref()?;
        let still_allowed = replaced.allowed_values.as_ref()?;
        let dropped = enumerated
            .iter()
            .find(|value| !still_allowed.contains(value))?;
        Some(Withdrawal::AllowedValue {
            path: path.clone(),
            value: dropped.clone(),
        })
    })
}

/// How this service refuses a schema replacement that withdraws something. The ONE place
/// these sentences are written.
///
/// The fixed text names what cannot be taken away; the variable parts are the scope the
/// schema is carried in, the path of the field and the value or types concerned, all taken
/// from the two configurations the request compared.
fn message(scope: &Scope, withdrawal: &Withdrawal) -> String {
    let scope = scope.label();
    match withdrawal {
        Withdrawal::Field { path } => format!(
            "The custom metadata schema cannot remove field {path} from {scope}; fields can \
             only be added."
        ),
        Withdrawal::Type { path, from, to } => format!(
            "The custom metadata schema cannot change the type of field {path} in {scope} \
             from {} to {}.",
            render(from),
            render(to)
        ),
        Withdrawal::AllowedValue { path, value } => format!(
            "The custom metadata schema cannot remove allowed value {} of field {path} in \
             {scope}; allowed values can only be added.",
            render(value)
        ),
    }
}

/// A schema value as a refusal quotes it: a JSON string unquoted, since that is how a type
/// name or an enumerated value reads, and anything else as the JSON the schema wrote.
fn render(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}
