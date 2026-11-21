// SPDX-License-Identifier: CC0-1.0

//! The MODEL VALIDATION this service runs at its FRONT DOOR, against the resource type's
//! CloudFormation registry schema.
//!
//! ## The rule this module implements
//!
//! A resource model a request carries — the `DesiredState` of a create, the model a patch
//! leaves behind on an update — is validated against the resource type's registry schema
//! BEFORE anything the request names is resolved, before any handler is asked and before a
//! resource operation request is minted. A property whose value is outside a constraint that
//! schema declares for it is therefore a fault in the CALL: it is refused synchronously, with
//! the model validator's own diagnostic naming the property's JSON pointer in the validated
//! document and the schema keyword the value failed ([`failure`]).
//!
//! That is a different plane from a handler's verdict about the input. A handler verdict is
//! reached asynchronously, after the request has been accepted, and is reported on the
//! request's own terminal event — so a caller has to poll for it. A value the type's schema
//! could never admit needs no handler to decide it, and a caller must not have to poll to
//! learn that the model it wrote is not a model of the type. Everything the schema does NOT
//! state — the owning service's own naming rules, the prefixes it keeps for itself, a quota,
//! a key policy — stays the handler's, where it is reached when the work falls due.
//!
//! ## What a row carries, and where it comes from
//!
//! [`TYPE_SCHEMAS`] is a catalogue: one row per resource type, carrying the properties of
//! that type whose value its registry schema constrains, each with the JSON `type` the
//! schema declares and the constraint keywords it states for it ([`Constraint`]). Catalog
//! source: the `properties` object of each type's resource provider schema, as
//! CloudFormation publishes it (`DescribeType`). Where a schema republishes a value set or a
//! bound the OWNING service's own API publishes — a parameter's tiers, a log group's
//! retention catalogue, an alarm's evaluation periods — the row reads that service's own
//! published set rather than restating it, so the two cannot drift apart.
//!
//! The catalogue is deliberately PARTIAL and fail-open, as every schema catalogue in this
//! crate is: a property no row names, and a type with no row at all, are left to the owning
//! service's handler, which is the authority on everything the schema does not state. Only
//! the properties of the type ITSELF are judged here, at the document's own root, so a value
//! nested inside a property's own object or list is left there too.
//!
//! PRESENCE is not this stage's question: which properties a model must carry, and which it
//! may carry at all, are the schema's `required` and `additionalProperties` keywords and are
//! answered where each type's model is read.

use ::std::collections::BTreeMap;
use ::std::sync::OnceLock;

use ::serde_json::Value;
use ::service_com_amazonaws_cloudwatchlogs_logs_20140328::ops::types::LOG_GROUP_CLASS_VALUES;
use ::service_com_amazonaws_cloudwatchlogs_logs_20140328::state::RETENTION_IN_DAYS_VALUES;
use ::service_com_amazonaws_sfn_awsstepfunctions::ops::types::STATE_MACHINE_TYPE_VALUES;
use ::service_com_amazonaws_ssm_amazonssm::ops::types::PARAMETER_TIER_VALUES;
use ::service_com_amazonaws_ssm_amazonssm::state as ssm;

/// The JSON type a property's schema declares under its `type` keyword, named as the model
/// validator's own diagnostic names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeclaredType {
    /// `"type": "string"`.
    Text,
    /// `"type": "integer"` — a number with nothing after the decimal point.
    Integer,
    /// `"type": "number"`, which every integer is one of.
    Number,
}

impl DeclaredType {
    /// The name the validator's diagnostic states this type under.
    fn name(self) -> &'static str {
        match self {
            Self::Text => "String",
            Self::Integer => "Integer",
            Self::Number => "Number",
        }
    }

    /// Whether `value` is of this type. An integer satisfies `number`, as JSON Schema's own
    /// numeric types nest; nothing else nests.
    fn admits(self, value: &Value) -> bool {
        match self {
            Self::Text => value.is_string(),
            Self::Integer => is_integer(value),
            Self::Number => value.is_number(),
        }
    }
}

/// Whether `value` is a number with nothing after the decimal point, which is what JSON
/// Schema's `integer` type admits.
fn is_integer(value: &Value) -> bool {
    value
        .as_f64()
        .is_some_and(|number| number.fract() == 0.0_f64)
}

/// The type name the validator's diagnostic reports for a value it FOUND, which is the JSON
/// type of the value itself.
fn found_type(value: &Value) -> &'static str {
    match value {
        Value::Null => "Null",
        Value::Bool(_) => "Boolean",
        Value::Number(_) if is_integer(value) => DeclaredType::Integer.name(),
        Value::Number(_) => DeclaredType::Number.name(),
        Value::String(_) => DeclaredType::Text.name(),
        Value::Array(_) => "Array",
        Value::Object(_) => "Object",
    }
}

/// One constraint keyword a property's schema states, and what it states.
///
/// A constraint judges only values of the type the property declares: the `type` keyword is
/// what judges the type itself, and the validator reports ONE failure per value.
#[derive(Debug, Clone, Copy)]
enum Constraint {
    /// `pattern`: the regular expression the whole value must match.
    Pattern(&'static str),
    /// `enum`, over the strings the schema publishes.
    AdmittedText(&'static [&'static str]),
    /// `enum`, over the integers the schema publishes.
    AdmittedIntegers(&'static [i32]),
    /// `minLength`: the fewest characters the value may carry.
    MinLength(usize),
    /// `maxLength`: the most characters the value may carry.
    MaxLength(usize),
    /// `minimum`: the smallest number the value may be.
    Minimum(f64),
}

impl Constraint {
    /// The schema keyword this constraint is stated under, which is the keyword a failure
    /// naming it reports.
    fn keyword(self) -> &'static str {
        match self {
            Self::Pattern(_) => "pattern",
            Self::AdmittedText(_) | Self::AdmittedIntegers(_) => "enum",
            Self::MinLength(_) => "minLength",
            Self::MaxLength(_) => "maxLength",
            Self::Minimum(_) => "minimum",
        }
    }

    /// Whether `value` satisfies this constraint — true for a value of another JSON type,
    /// which the `type` keyword is what judges.
    fn admits(self, value: &Value) -> bool {
        match self {
            Self::Pattern(pattern) => value
                .as_str()
                .is_none_or(|text| matches_pattern(pattern, text)),
            Self::AdmittedText(admitted) => {
                value.as_str().is_none_or(|text| admitted.contains(&text))
            }
            Self::AdmittedIntegers(admitted) => value.as_i64().is_none_or(|number| {
                i32::try_from(number).is_ok_and(|number| admitted.contains(&number))
            }),
            Self::MinLength(fewest) => value
                .as_str()
                .is_none_or(|text| text.chars().count() >= fewest),
            Self::MaxLength(most) => value
                .as_str()
                .is_none_or(|text| text.chars().count() <= most),
            Self::Minimum(smallest) => value.as_f64().is_none_or(|number| number >= smallest),
        }
    }
}

/// One property of a resource type's schema whose value that schema constrains.
#[derive(Debug, Clone, Copy)]
struct PropertySchema {
    /// The property, as the type's schema names it — which is the name a resource model
    /// carries it under.
    property: &'static str,
    /// The JSON type the schema declares for it (`type`).
    declared: DeclaredType,
    /// The constraint keywords the schema states for it, in the order a failure is looked
    /// for: the first one the value fails is the one reported.
    constraints: &'static [Constraint],
}

impl PropertySchema {
    /// The diagnostic for `value` at this property, or `None` where the schema admits it.
    ///
    /// The declared TYPE is judged first, because a constraint over a value of another type
    /// states nothing about it.
    fn failure(&self, value: &Value) -> Option<String> {
        if !self.declared.admits(value) {
            return Some(type_failure(
                self.property,
                self.declared.name(),
                found_type(value),
            ));
        }
        let violated = self
            .constraints
            .iter()
            .find(|constraint| !constraint.admits(value))?;
        Some(match violated.keyword() {
            ENUM_KEYWORD => enum_failure(self.property),
            keyword => failure(self.property, keyword),
        })
    }
}

/// One resource type's constrained properties.
#[derive(Debug, Clone, Copy)]
struct TypeSchema {
    /// The type, as a request names it (`TypeName`).
    type_name: &'static str,
    /// Its properties this stage judges, in the order the schema publishes them — which is
    /// observable only in which property a model breaking several of them is reported for.
    properties: &'static [PropertySchema],
}

/// The character class `AWS::Logs::LogGroup`'s and `AWS::Logs::LogStream`'s schemas state for
/// a log group name (`"pattern": "^[\\.\\-_/#A-Za-z0-9]{1,512}"`, anchored at both ends): the
/// letters, digits and punctuation CloudWatch Logs holds a group name of, and nothing else —
/// so a name carrying a space, a colon or a wildcard is a name no model of either type can
/// state.
const LOG_GROUP_NAME_PATTERN: &str = r"^[\.\-_/#A-Za-z0-9]{1,512}\z";

/// The character class `AWS::Logs::LogStream`'s schema states for a log stream name
/// (`"pattern": "^[^:*]*"`, anchored at both ends): any character but the two CloudWatch Logs
/// keeps for addressing and matching.
const LOG_STREAM_NAME_PATTERN: &str = r"^[^:*]*\z";

/// The most characters a log group or log stream name may carry (`maxLength`), and the fewest
/// (`minLength`), as both Amazon CloudWatch Logs types' schemas state them.
const LOG_NAME_LENGTH: (usize, usize) = (1, 512);

/// The resource types whose model this stage validates — see the module docs for what a row
/// carries and where it comes from.
const TYPE_SCHEMAS: &[TypeSchema] = &[
    // `AWS::CloudWatch::Alarm`: the metric alarm's own schema republishes the bounds
    // `cloudwatch:PutMetricAlarm` declares for the members it is built from — `AlarmName`
    // length 1..255, and the three counts of periods with a smallest value of one — and the
    // threshold the alarm compares against is a number rather than text.
    TypeSchema {
        type_name: "AWS::CloudWatch::Alarm",
        properties: &[
            PropertySchema {
                property: "AlarmName",
                declared: DeclaredType::Text,
                constraints: &[Constraint::MinLength(1), Constraint::MaxLength(255)],
            },
            PropertySchema {
                property: "ComparisonOperator",
                declared: DeclaredType::Text,
                constraints: &[],
            },
            PropertySchema {
                property: "DatapointsToAlarm",
                declared: DeclaredType::Integer,
                constraints: &[Constraint::Minimum(1.0)],
            },
            PropertySchema {
                property: "EvaluationPeriods",
                declared: DeclaredType::Integer,
                constraints: &[Constraint::Minimum(1.0)],
            },
            PropertySchema {
                property: "Period",
                declared: DeclaredType::Integer,
                constraints: &[Constraint::Minimum(1.0)],
            },
            PropertySchema {
                property: "Statistic",
                declared: DeclaredType::Text,
                constraints: &[],
            },
            PropertySchema {
                property: "Threshold",
                declared: DeclaredType::Number,
                constraints: &[],
            },
        ],
    },
    // `AWS::Logs::LogGroup`: the name's class and length, the retention catalogue and the
    // class set — the last two being the value sets Amazon CloudWatch Logs' own API publishes,
    // read from that service rather than restated here — and the bound on the key id.
    TypeSchema {
        type_name: "AWS::Logs::LogGroup",
        properties: &[
            PropertySchema {
                property: "KmsKeyId",
                declared: DeclaredType::Text,
                constraints: &[Constraint::MaxLength(256)],
            },
            PropertySchema {
                property: "LogGroupClass",
                declared: DeclaredType::Text,
                constraints: &[Constraint::AdmittedText(LOG_GROUP_CLASS_VALUES)],
            },
            PropertySchema {
                property: "LogGroupName",
                declared: DeclaredType::Text,
                constraints: &[
                    Constraint::MinLength(LOG_NAME_LENGTH.0),
                    Constraint::MaxLength(LOG_NAME_LENGTH.1),
                    Constraint::Pattern(LOG_GROUP_NAME_PATTERN),
                ],
            },
            PropertySchema {
                property: "RetentionInDays",
                declared: DeclaredType::Integer,
                constraints: &[Constraint::AdmittedIntegers(&RETENTION_IN_DAYS_VALUES)],
            },
        ],
    },
    // `AWS::Logs::LogStream`: the group the stream is in, held to the same name rule as the
    // group's own type, and the stream's own name.
    TypeSchema {
        type_name: "AWS::Logs::LogStream",
        properties: &[
            PropertySchema {
                property: "LogGroupName",
                declared: DeclaredType::Text,
                constraints: &[
                    Constraint::MinLength(LOG_NAME_LENGTH.0),
                    Constraint::MaxLength(LOG_NAME_LENGTH.1),
                    Constraint::Pattern(LOG_GROUP_NAME_PATTERN),
                ],
            },
            PropertySchema {
                property: "LogStreamName",
                declared: DeclaredType::Text,
                constraints: &[
                    Constraint::MinLength(LOG_NAME_LENGTH.0),
                    Constraint::MaxLength(LOG_NAME_LENGTH.1),
                    Constraint::Pattern(LOG_STREAM_NAME_PATTERN),
                ],
            },
        ],
    },
    // `AWS::SSM::Parameter`: the two types a parameter of this resource type is of — the
    // encrypted type is not among them, the schema declaring only the two a template may
    // create — and the tiers AWS Systems Manager publishes for a parameter.
    TypeSchema {
        type_name: "AWS::SSM::Parameter",
        properties: &[
            PropertySchema {
                property: "Tier",
                declared: DeclaredType::Text,
                constraints: &[Constraint::AdmittedText(PARAMETER_TIER_VALUES)],
            },
            PropertySchema {
                property: "Type",
                declared: DeclaredType::Text,
                constraints: &[Constraint::AdmittedText(PARAMETER_TYPES)],
            },
        ],
    },
    // `AWS::StepFunctions::StateMachine`: the name's length and the workflow types AWS Step
    // Functions publishes, read from that service's own model.
    TypeSchema {
        type_name: "AWS::StepFunctions::StateMachine",
        properties: &[
            PropertySchema {
                property: "StateMachineName",
                declared: DeclaredType::Text,
                constraints: &[Constraint::MinLength(1), Constraint::MaxLength(80)],
            },
            PropertySchema {
                property: "StateMachineType",
                declared: DeclaredType::Text,
                constraints: &[Constraint::AdmittedText(STATE_MACHINE_TYPE_VALUES)],
            },
        ],
    },
];

/// The parameter types `AWS::SSM::Parameter`'s schema publishes: the two AWS Systems Manager
/// names that this resource type may be of, as that service's own model spells them.
const PARAMETER_TYPES: &[&str] = &[ssm::TYPE_STRING, ssm::TYPE_STRING_LIST];

/// The keyword an `enum` violation is stated under, which is the one class of failure the
/// validator reports with a composition line of its own ([`enum_failure`]).
const ENUM_KEYWORD: &str = "enum";

/// The LOCATION a failure about a property of the type itself is reported at: the root of the
/// document that was validated, which is the resource model the request carried.
const ROOT: &str = "#";

/// The diagnostic Cloud Control API refuses `model` as a resource of `type_name` with, or
/// `None` where the type's registry schema admits every value it carries.
///
/// The properties are judged in the order the type's row publishes them, so a model breaking
/// several constraints is always reported for the same one.
pub fn model_validation_failure(type_name: &str, model: &Value) -> Option<String> {
    let schema = TYPE_SCHEMAS
        .iter()
        .find(|schema| schema.type_name == type_name)?;
    let properties = model.as_object()?;
    schema.properties.iter().find_map(|property| {
        properties
            .get(property.property)
            .and_then(|value| property.failure(value))
    })
}

/// How the model validator states a value outside a constraint its schema declares: its own
/// fixed text, around the JSON pointer of the offending property in the validated document
/// and the name of the schema keyword the value failed.
fn failure(pointer: &str, keyword: &str) -> String {
    format!(
        "Model validation failed ({ROOT}/{pointer}: failed validation constraint for keyword \
         [{keyword}])"
    )
}

/// How the model validator states a value outside the set its schema publishes: the
/// composition the validator matched the value against first, and then the constraint that
/// failed — two lines, both located at the offending property.
fn enum_failure(pointer: &str) -> String {
    format!(
        "Model validation failed ({ROOT}/{pointer}: {ROOT}: only 1 subschema matches out of \
         2\n{ROOT}/{pointer}: failed validation constraint for keyword [{ENUM_KEYWORD}])"
    )
}

/// How the model validator states a value whose JSON type is not the one its schema declares:
/// the type the schema declared and the type of the value it found.
fn type_failure(pointer: &str, declared: &str, found: &str) -> String {
    format!("Model validation failed ({ROOT}/{pointer}: expected type: {declared}, found: {found})")
}

/// Whether `text` matches the whole regular expression `pattern` a schema declares.
///
/// The expressions are the catalogue's own, compiled ONCE into
/// [`PATTERN_MATCHERS`]; a pattern that would not compile is one this stage judges nothing
/// by, so the value is left to the owning service rather than refused by a broken rule.
fn matches_pattern(pattern: &str, text: &str) -> bool {
    pattern_matchers()
        .get(pattern)
        .is_none_or(|matcher| matcher.is_match(text))
}

/// The compiled form of every `pattern` the catalogue declares, keyed by the expression
/// itself.
static PATTERN_MATCHERS: OnceLock<BTreeMap<&'static str, ::regex::Regex>> = OnceLock::new();

/// [`PATTERN_MATCHERS`], compiled on first use.
fn pattern_matchers() -> &'static BTreeMap<&'static str, ::regex::Regex> {
    PATTERN_MATCHERS.get_or_init(|| {
        declared_patterns()
            .filter_map(|pattern| {
                ::regex::Regex::new(pattern)
                    .ok()
                    .map(|matcher| (pattern, matcher))
            })
            .collect()
    })
}

/// Every regular expression the catalogue declares.
fn declared_patterns() -> impl Iterator<Item = &'static str> {
    TYPE_SCHEMAS
        .iter()
        .flat_map(|schema| schema.properties)
        .flat_map(|property| property.constraints)
        .filter_map(|constraint| match constraint {
            Constraint::Pattern(pattern) => Some(*pattern),
            _ => None,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: a value outside a constraint the type's schema declares is refused with the
    /// validator's diagnostic naming THAT property and THAT keyword, whichever type,
    /// property and keyword it is — and a value the schema admits is refused by nothing
    /// here.
    ///
    /// The rows span every keyword the catalogue states, across several types, and each
    /// keyword's own admitted side.
    #[test]
    fn a_value_outside_a_declared_constraint_is_refused_naming_the_keyword() {
        let cases: &[(&str, &str, Value, Option<String>)] = &[
            (
                "a log group name carrying a character the class excludes",
                "AWS::Logs::LogGroup",
                ::serde_json::json!({"LogGroupName": "orders:1"}),
                Some(failure("LogGroupName", "pattern")),
            ),
            (
                "a log group name of the punctuation the class admits",
                "AWS::Logs::LogGroup",
                ::serde_json::json!({"LogGroupName": "/orders.v2-1_a/#b"}),
                None,
            ),
            (
                "an empty log group name",
                "AWS::Logs::LogGroup",
                ::serde_json::json!({"LogGroupName": ""}),
                Some(failure("LogGroupName", "minLength")),
            ),
            (
                "a log group name one character past the most it may carry",
                "AWS::Logs::LogGroup",
                ::serde_json::json!({"LogGroupName": "g".repeat(513)}),
                Some(failure("LogGroupName", "maxLength")),
            ),
            (
                "a log group name of exactly the most it may carry",
                "AWS::Logs::LogGroup",
                ::serde_json::json!({"LogGroupName": "g".repeat(512)}),
                None,
            ),
            (
                "a retention outside the published catalogue",
                "AWS::Logs::LogGroup",
                ::serde_json::json!({"LogGroupName": "orders", "RetentionInDays": 2}),
                Some(enum_failure("RetentionInDays")),
            ),
            (
                "a retention the catalogue publishes",
                "AWS::Logs::LogGroup",
                ::serde_json::json!({"LogGroupName": "orders", "RetentionInDays": 400}),
                None,
            ),
            (
                "a retention that is not a whole number",
                "AWS::Logs::LogGroup",
                ::serde_json::json!({"LogGroupName": "orders", "RetentionInDays": 1.5}),
                Some(type_failure("RetentionInDays", "Integer", "Number")),
            ),
            (
                "a log group class outside the published set",
                "AWS::Logs::LogGroup",
                ::serde_json::json!({"LogGroupName": "orders", "LogGroupClass": "PLATINUM"}),
                Some(enum_failure("LogGroupClass")),
            ),
            (
                "a log stream name carrying a character the class excludes",
                "AWS::Logs::LogStream",
                ::serde_json::json!({"LogGroupName": "orders", "LogStreamName": "host:1"}),
                Some(failure("LogStreamName", "pattern")),
            ),
            (
                "a parameter type the resource type does not publish",
                "AWS::SSM::Parameter",
                ::serde_json::json!({"Name": "/orders/db", "Type": "SecureString"}),
                Some(enum_failure("Type")),
            ),
            (
                "a parameter tier the owning service publishes",
                "AWS::SSM::Parameter",
                ::serde_json::json!({"Name": "/orders/db", "Tier": "Intelligent-Tiering"}),
                None,
            ),
            (
                "a threshold stated as text",
                "AWS::CloudWatch::Alarm",
                ::serde_json::json!({"AlarmName": "cpu", "Threshold": "80"}),
                Some(type_failure("Threshold", "Number", "String")),
            ),
            (
                "a threshold stated as a whole number, which a number admits",
                "AWS::CloudWatch::Alarm",
                ::serde_json::json!({"AlarmName": "cpu", "Threshold": 80}),
                None,
            ),
            (
                "no period at all to evaluate over",
                "AWS::CloudWatch::Alarm",
                ::serde_json::json!({"AlarmName": "cpu", "EvaluationPeriods": 0}),
                Some(failure("EvaluationPeriods", "minimum")),
            ),
            (
                "the fewest periods the alarm may evaluate over",
                "AWS::CloudWatch::Alarm",
                ::serde_json::json!({"AlarmName": "cpu", "EvaluationPeriods": 1}),
                None,
            ),
            (
                "a workflow type Step Functions does not publish",
                "AWS::StepFunctions::StateMachine",
                ::serde_json::json!({"StateMachineType": "BATCH"}),
                Some(enum_failure("StateMachineType")),
            ),
            (
                "a state machine name past the most it may carry",
                "AWS::StepFunctions::StateMachine",
                ::serde_json::json!({"StateMachineName": "s".repeat(81)}),
                Some(failure("StateMachineName", "maxLength")),
            ),
            (
                "a type the catalogue holds no row for",
                "AWS::SQS::Queue",
                ::serde_json::json!({"QueueName": "orders:1"}),
                None,
            ),
            (
                "a model that is not an object of properties at all",
                "AWS::Logs::LogGroup",
                Value::Null,
                None,
            ),
        ];
        for (case, type_name, model, expected) in cases {
            let refused = model_validation_failure(type_name, model);
            assert_eq!(
                refused.as_deref(),
                expected.as_deref(),
                "case {case:?}: expected {expected:?}, got {refused:?}"
            );
        }
    }

    /// The diagnostics themselves: fixed text around the property's pointer and the keyword,
    /// with nothing of one request in them.
    #[test]
    fn the_diagnostics_are_the_validators_own_wording() {
        assert_eq!(
            failure("LogGroupName", "pattern"),
            "Model validation failed (#/LogGroupName: failed validation constraint for keyword \
             [pattern])",
            "expected the validator's wording for a failed constraint"
        );
        assert_eq!(
            type_failure("Threshold", "Number", "String"),
            "Model validation failed (#/Threshold: expected type: Number, found: String)",
            "expected the validator's wording for a declared type"
        );
        assert_eq!(
            enum_failure("Tier"),
            "Model validation failed (#/Tier: #: only 1 subschema matches out of 2\n#/Tier: \
             failed validation constraint for keyword [enum])",
            "expected the validator's wording for a published set"
        );
    }

    /// Every regular expression the catalogue declares compiles, so no row is a rule this
    /// stage silently judges nothing by.
    #[test]
    fn every_declared_pattern_compiles() {
        for pattern in declared_patterns() {
            assert!(
                pattern_matchers().contains_key(pattern),
                "case {pattern:?}: expected the declared expression to compile"
            );
        }
    }

    /// Every row names one type once and every property of a row once, and a row's declared
    /// constraints are ones its declared type can fail: a length or a pattern over a number,
    /// or a smallest value over text, would be a rule nothing could break.
    #[test]
    fn every_row_states_constraints_its_declared_type_can_fail() {
        for schema in TYPE_SCHEMAS {
            let rows = TYPE_SCHEMAS
                .iter()
                .filter(|other| other.type_name == schema.type_name)
                .count();
            assert_eq!(
                rows, 1,
                "case {:?}: expected one row per type, got {rows}",
                schema.type_name
            );
            for property in schema.properties {
                let named = schema
                    .properties
                    .iter()
                    .filter(|other| other.property == property.property)
                    .count();
                assert_eq!(
                    named, 1,
                    "case {:?}: expected one row per property, got {named} for {:?}",
                    schema.type_name, property.property
                );
                for constraint in property.constraints {
                    let fits = matches!(
                        (property.declared, constraint),
                        (
                            DeclaredType::Text,
                            Constraint::Pattern(_)
                                | Constraint::AdmittedText(_)
                                | Constraint::MinLength(_)
                                | Constraint::MaxLength(_),
                        ) | (
                            DeclaredType::Integer | DeclaredType::Number,
                            Constraint::AdmittedIntegers(_) | Constraint::Minimum(_),
                        )
                    );
                    assert!(
                        fits,
                        "case {:?}: {:?} declares {:?}, which a {:?} cannot fail",
                        schema.type_name,
                        property.property,
                        constraint,
                        property.declared
                    );
                }
            }
        }
    }
}
