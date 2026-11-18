// SPDX-License-Identifier: CC0-1.0

//! The INGEST PIPELINE engine of a domain's data plane: what a pipeline's processors are,
//! and what they do to a document.
//!
//! "An ingest pipeline is a sequence of processors that are applied to documents as they
//! are ingested into an index": a pipeline is DECLARED through the cluster's pipeline API
//! and RUN on a document's source — by a write that names the pipeline, and by the simulate
//! API, which "executes a pipeline against a set of documents provided in the body" and so
//! answers exactly what a write through that pipeline would store.
//!
//! One reading of a pipeline serves both: [`parse`] validates the declaration a client
//! wrote and answers the processors it declares, and [`Processor::apply`] runs ONE of them
//! over a source. The simulate API runs them one at a time so it can report each step, and a
//! write runs them in order over the document it is about to store, so neither can predict
//! something the other would not do.
//!
//! The processors served are the ones this engine can carry out over a JSON source — `set`,
//! `rename`, `remove`, `convert`, `lowercase`, `uppercase` and `trim`. A declaration naming
//! any other type is REFUSED when the pipeline is written, with the engine's own sentence for
//! a processor type it does not register, rather than stored and silently skipped: a pipeline
//! that is accepted is a pipeline that runs whole.

use serde_json::{Map, Value};

/// Why a pipeline could not be declared, or could not be run over a document: the engine's
/// own error type and the sentence it states.
#[derive(Debug, Clone)]
pub struct ProcessorError {
    /// The engine's error type.
    pub error_type: &'static str,
    /// The reason, as the engine words it.
    pub reason: String,
}

/// The engine's error type for a pipeline declaration it will not read, and the one for a
/// processor that cannot act on the document it was given — each the crate's ONE spelling of
/// that engine error type, reached rather than restated so two sites cannot drift apart.
use super::PARSE_EXCEPTION;
use crate::state::ingest::ILLEGAL_ARGUMENT_EXCEPTION;

/// The most processors one pipeline may declare.
///
/// The emulator's OWN bound on the work one write does: every processor runs on every
/// document the pipeline is named for, so the chain is bounded rather than unbounded.
pub const MAX_PROCESSORS: usize = 100;

/// The most segments one field path may name — `a.b.c` names three.
///
/// A path is walked, never recursed on, and this bounds how far one walk goes.
pub const MAX_FIELD_DEPTH: usize = 20;

/// One processor of a pipeline, as [`parse`] validated it.
#[derive(Debug, Clone)]
pub struct Processor {
    /// The processor type's own name, which the simulate API reports each step under.
    kind: &'static str,
    action: Action,
}

/// What a processor DOES, with the options its type declares.
#[derive(Debug, Clone)]
enum Action {
    /// `set`: "sets one field and associates it with the specified value".
    Set {
        field: String,
        value: Value,
        /// `override`: whether an existing value is replaced. Default true.
        replaces: bool,
    },
    /// `rename`: "renames an existing field".
    Rename {
        field: String,
        target_field: String,
        ignore_missing: bool,
    },
    /// `remove`: "removes existing fields".
    Remove {
        fields: Vec<String>,
        ignore_missing: bool,
    },
    /// `lowercase`, `uppercase` and `trim`: each rewrites one field's STRING value.
    Text {
        field: String,
        target_field: String,
        ignore_missing: bool,
        change: TextChange,
    },
    /// `convert`: "converts a field in the currently ingested document to a different
    /// type, such as converting a string to an integer".
    Convert {
        field: String,
        target_field: String,
        ignore_missing: bool,
        to: ConvertType,
    },
}

/// The type a `convert` processor converts a field's value to.
///
/// The catalog is the processor's own `type` option — "integer, long, float, double, string,
/// boolean and auto" ([`CONVERT_TYPES`]) — and a value outside it names no conversion this
/// processor performs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConvertType {
    Integer,
    Long,
    Float,
    Double,
    String,
    Boolean,
    /// `auto`: "attempts to automatically convert the field value to the closest
    /// non-string type", leaving a value no other type reads as the string it is.
    Auto,
}

/// The conversions a `convert` processor performs, each under the name its `type` option
/// states it as.
const CONVERT_TYPES: [(&str, ConvertType); 7] = [
    ("integer", ConvertType::Integer),
    ("long", ConvertType::Long),
    ("float", ConvertType::Float),
    ("double", ConvertType::Double),
    ("string", ConvertType::String),
    ("boolean", ConvertType::Boolean),
    ("auto", ConvertType::Auto),
];

/// Which rewriting of a string a text processor performs.
#[derive(Debug, Clone, Copy)]
enum TextChange {
    Lower,
    Upper,
    Trim,
}

/// The processor types this engine registers, each with the rewriting it performs.
const TEXT_PROCESSORS: [(&str, TextChange); 3] = [
    ("lowercase", TextChange::Lower),
    ("uppercase", TextChange::Upper),
    ("trim", TextChange::Trim),
];

/// The processors a pipeline's `processors` list declares, or the engine's refusal of the
/// declaration.
///
/// A pipeline declaring no `processors` at all declares none, which is a pipeline that
/// changes nothing — the engine takes it. Anything else about the list that is not a list of
/// single-entry objects is a declaration the engine will not read.
pub fn parse(pipeline: &Value) -> Result<Vec<Processor>, ProcessorError> {
    let declared = match pipeline.get("processors") {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(Value::Array(declared)) => declared,
        Some(_) => {
            return Err(ProcessorError {
                error_type: PARSE_EXCEPTION,
                reason: "[processors] property isn't a list, but of type [object]".to_string(),
            })
        }
    };
    if declared.len() > MAX_PROCESSORS {
        return Err(ProcessorError {
            error_type: PARSE_EXCEPTION,
            reason: format!(
                "[processors] declares {} processors, past the maximum of {MAX_PROCESSORS} this \
                 cluster runs in one pipeline",
                declared.len()
            ),
        });
    }
    declared.iter().map(one).collect()
}

/// One processor of the list.
fn one(declared: &Value) -> Result<Processor, ProcessorError> {
    let Some(entry) = declared.as_object() else {
        return Err(ProcessorError {
            error_type: PARSE_EXCEPTION,
            reason: "[processors] each processor must be an object naming its type".to_string(),
        });
    };
    let mut entries = entry.iter();
    let (Some((kind, options)), None) = (entries.next(), entries.next()) else {
        return Err(ProcessorError {
            error_type: PARSE_EXCEPTION,
            reason: "[processors] each processor must name exactly one type".to_string(),
        });
    };
    let empty = Map::new();
    let options = options.as_object().unwrap_or(&empty);
    if let Some((name, change)) = TEXT_PROCESSORS
        .iter()
        .find(|(name, _)| *name == kind.as_str())
    {
        let field = required_text(options, "field")?;
        let target_field = options
            .get("target_field")
            .and_then(Value::as_str)
            .unwrap_or(&field)
            .to_string();
        return Ok(Processor {
            kind: name,
            action: Action::Text {
                field,
                target_field,
                ignore_missing: flag(options, "ignore_missing", false),
                change: *change,
            },
        });
    }
    match kind.as_str() {
        "convert" => {
            let field = required_text(options, "field")?;
            let named = required_text(options, "type")?;
            let Some((_, to)) = CONVERT_TYPES
                .iter()
                .find(|(name, _)| *name == named.as_str())
            else {
                return Err(ProcessorError {
                    error_type: PARSE_EXCEPTION,
                    reason: format!("[type] type [{named}] not supported, cannot convert field."),
                });
            };
            let target_field = options
                .get("target_field")
                .and_then(Value::as_str)
                .unwrap_or(&field)
                .to_string();
            Ok(Processor {
                kind: "convert",
                action: Action::Convert {
                    field,
                    target_field,
                    ignore_missing: flag(options, "ignore_missing", false),
                    to: *to,
                },
            })
        }
        "set" => Ok(Processor {
            kind: "set",
            action: Action::Set {
                field: required_text(options, "field")?,
                value: options
                    .get("value")
                    .cloned()
                    .ok_or_else(|| missing("value"))?,
                replaces: flag(options, "override", true),
            },
        }),
        "rename" => Ok(Processor {
            kind: "rename",
            action: Action::Rename {
                field: required_text(options, "field")?,
                target_field: required_text(options, "target_field")?,
                ignore_missing: flag(options, "ignore_missing", false),
            },
        }),
        "remove" => {
            // "field: Fields to be removed" — one name or a list of them.
            let fields = match options.get("field") {
                Some(Value::String(field)) => vec![field.clone()],
                Some(Value::Array(fields)) => fields
                    .iter()
                    .map(|field| {
                        field
                            .as_str()
                            .map(str::to_string)
                            .ok_or_else(|| missing("field"))
                    })
                    .collect::<Result<Vec<String>, ProcessorError>>()?,
                _ => return Err(missing("field")),
            };
            Ok(Processor {
                kind: "remove",
                action: Action::Remove {
                    fields,
                    ignore_missing: flag(options, "ignore_missing", false),
                },
            })
        }
        other => Err(ProcessorError {
            error_type: PARSE_EXCEPTION,
            reason: format!("No processor type exists with name [{other}]"),
        }),
    }
}

/// One required string option of a processor.
fn required_text(options: &Map<String, Value>, name: &str) -> Result<String, ProcessorError> {
    options
        .get(name)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| missing(name))
}

/// The engine's sentence for a processor option its type requires and the declaration does
/// not carry.
fn missing(name: &str) -> ProcessorError {
    ProcessorError {
        error_type: PARSE_EXCEPTION,
        reason: format!("[{name}] required property is missing"),
    }
}

/// A boolean option, defaulting the way the processor's own documentation does.
fn flag(options: &Map<String, Value>, name: &str, default: bool) -> bool {
    match options.get(name) {
        Some(Value::Bool(value)) => *value,
        Some(Value::String(value)) => value == "true",
        _ => default,
    }
}

impl Processor {
    /// The processor type's own name — what the simulate API reports each step under.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        self.kind
    }

    /// Run this processor over `source`, or answer the engine's refusal of the document.
    pub fn apply(&self, source: &mut Value) -> Result<(), ProcessorError> {
        match &self.action {
            Action::Set {
                field,
                value,
                replaces,
            } => {
                if *replaces || read_path(source, field).is_none() {
                    write_path(source, field, value.clone())?;
                }
                Ok(())
            }
            Action::Rename {
                field,
                target_field,
                ignore_missing,
            } => {
                let Some(value) = read_path(source, field).cloned() else {
                    return self.absent(field, *ignore_missing);
                };
                write_path(source, target_field, value)?;
                remove_path(source, field);
                Ok(())
            }
            Action::Remove {
                fields,
                ignore_missing,
            } => {
                for field in fields {
                    if read_path(source, field).is_none() {
                        self.absent(field, *ignore_missing)?;
                        continue;
                    }
                    remove_path(source, field);
                }
                Ok(())
            }
            Action::Text {
                field,
                target_field,
                ignore_missing,
                change,
            } => {
                let Some(value) = read_path(source, field).cloned() else {
                    return self.absent(field, *ignore_missing);
                };
                let Some(text) = value.as_str() else {
                    return Err(ProcessorError {
                        error_type: ILLEGAL_ARGUMENT_EXCEPTION,
                        reason: format!(
                            "field [{field}] of type [{}] cannot be cast to a string",
                            json_type(&value)
                        ),
                    });
                };
                let rewritten = match change {
                    TextChange::Lower => text.to_lowercase(),
                    TextChange::Upper => text.to_uppercase(),
                    TextChange::Trim => text.trim().to_string(),
                };
                write_path(source, target_field, Value::String(rewritten))
            }
            Action::Convert {
                field,
                target_field,
                ignore_missing,
                to,
            } => {
                let Some(value) = read_path(source, field).cloned() else {
                    return self.absent(field, *ignore_missing);
                };
                // "If the field value is an array, all members will be converted": the
                // conversion is of the VALUES the field holds, so a field holding several
                // holds the same number afterwards, each converted on its own.
                let converted = match value {
                    Value::Array(held) => Value::Array(
                        held.into_iter()
                            .map(|element| convert(&element, *to))
                            .collect::<Result<Vec<Value>, ProcessorError>>()?,
                    ),
                    one => convert(&one, *to)?,
                };
                write_path(source, target_field, converted)
            }
        }
    }

    /// What a processor does about a field the document does not carry: nothing when it was
    /// declared `ignore_missing`, and the engine's refusal otherwise.
    fn absent(&self, field: &str, ignore_missing: bool) -> Result<(), ProcessorError> {
        if ignore_missing {
            return Ok(());
        }
        Err(ProcessorError {
            error_type: ILLEGAL_ARGUMENT_EXCEPTION,
            reason: format!("field [{field}] doesn't exist"),
        })
    }
}

/// The value `value` converted to `to`, or the engine's refusal of a value that type reads
/// nothing from.
///
/// Every conversion is read off the value's own TEXT, which is what the processor converts: a
/// number written as a string and one written as a number convert alike, and a text no number
/// can be read from is refused rather than quietly stored as something else.
fn convert(value: &Value, to: ConvertType) -> Result<Value, ProcessorError> {
    let text = convert_text(value);
    match to {
        ConvertType::Integer => text
            .parse::<i32>()
            .map(Value::from)
            .map_err(|_| unconvertible(&text, "integer")),
        ConvertType::Long => text
            .parse::<i64>()
            .map(Value::from)
            .map_err(|_| unconvertible(&text, "long")),
        ConvertType::Float => text
            .parse::<f32>()
            .ok()
            .and_then(|number| ::serde_json::Number::from_f64(f64::from(number)))
            .map(Value::Number)
            .ok_or_else(|| unconvertible(&text, "float")),
        ConvertType::Double => text
            .parse::<f64>()
            .ok()
            .and_then(::serde_json::Number::from_f64)
            .map(Value::Number)
            .ok_or_else(|| unconvertible(&text, "double")),
        ConvertType::String => Ok(Value::String(text)),
        ConvertType::Boolean => boolean(&text).ok_or_else(|| ProcessorError {
            error_type: ILLEGAL_ARGUMENT_EXCEPTION,
            reason: format!("[{text}] is not a boolean value, cannot convert to boolean"),
        }),
        // "auto: attempts to automatically convert the field value to the closest
        // non-string type": a value that is not text already HAS a type, and a text is read
        // as the first of the types below it states, falling back to the text itself.
        ConvertType::Auto => {
            if !value.is_string() {
                return Ok(value.clone());
            }
            for narrower in [
                ConvertType::Boolean,
                ConvertType::Integer,
                ConvertType::Long,
                ConvertType::Float,
                ConvertType::Double,
            ] {
                if let Ok(converted) = convert(value, narrower) {
                    return Ok(converted);
                }
            }
            Ok(value.clone())
        }
    }
}

/// The TEXT a conversion is read off: the string a text field holds, and the JSON spelling of
/// anything else.
fn convert_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// The boolean `text` states, and `None` for a text that states neither — the two words the
/// processor reads a boolean from, in either case.
fn boolean(text: &str) -> Option<Value> {
    if text.eq_ignore_ascii_case("true") {
        return Some(Value::Bool(true));
    }
    if text.eq_ignore_ascii_case("false") {
        return Some(Value::Bool(false));
    }
    None
}

/// The engine's refusal of a value the named type reads nothing from.
fn unconvertible(text: &str, to: &str) -> ProcessorError {
    ProcessorError {
        error_type: ILLEGAL_ARGUMENT_EXCEPTION,
        reason: format!("unable to convert [{text}] to {to}"),
    }
}

/// A value's JSON type, as a refusal names it.
fn json_type(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// The segments of a field path: `a.b` names the member `b` of the object under `a`.
///
/// Bounded at [`MAX_FIELD_DEPTH`], so a path a caller wrote cannot make one walk unbounded;
/// a longer path resolves to nothing, which is a field the document does not carry.
fn segments(field: &str) -> Option<Vec<&str>> {
    let parts: Vec<&str> = field.split('.').filter(|part| !part.is_empty()).collect();
    (!parts.is_empty() && parts.len() <= MAX_FIELD_DEPTH).then_some(parts)
}

/// The value at `field`, or `None` when the document carries nothing there.
fn read_path<'s>(source: &'s Value, field: &str) -> Option<&'s Value> {
    let mut at = source;
    for segment in segments(field)? {
        at = at.as_object()?.get(segment)?;
    }
    Some(at)
}

/// Write `value` at `field`, creating the objects on the way to it.
///
/// A path whose prefix is held by something that is NOT an object cannot be written through,
/// which is the engine's own refusal rather than a value quietly replaced.
fn write_path(source: &mut Value, field: &str, value: Value) -> Result<(), ProcessorError> {
    let Some(parts) = segments(field) else {
        return Err(ProcessorError {
            error_type: ILLEGAL_ARGUMENT_EXCEPTION,
            reason: format!("field path [{field}] is not a path this cluster writes"),
        });
    };
    let mut at = source;
    for segment in &parts[..parts.len() - 1] {
        if !at.is_object() {
            return Err(not_an_object(field));
        }
        at = at
            .as_object_mut()
            .ok_or_else(|| not_an_object(field))?
            .entry((*segment).to_string())
            .or_insert_with(|| Value::Object(Map::new()));
    }
    let last = parts[parts.len() - 1].to_string();
    at.as_object_mut()
        .ok_or_else(|| not_an_object(field))?
        .insert(last, value);
    Ok(())
}

/// The engine's refusal of a path that runs through a value holding no members.
fn not_an_object(field: &str) -> ProcessorError {
    ProcessorError {
        error_type: ILLEGAL_ARGUMENT_EXCEPTION,
        reason: format!("cannot set [{field}]: a value on its path is not an object"),
    }
}

/// Remove whatever the document carries at `field`.
fn remove_path(source: &mut Value, field: &str) {
    let Some(parts) = segments(field) else {
        return;
    };
    let mut at = source;
    for segment in &parts[..parts.len() - 1] {
        let Some(next) = at.as_object_mut().and_then(|held| held.get_mut(*segment)) else {
            return;
        };
        at = next;
    }
    if let Some(held) = at.as_object_mut() {
        held.remove(parts[parts.len() - 1]);
    }
}
