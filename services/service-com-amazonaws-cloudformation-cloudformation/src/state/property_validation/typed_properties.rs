// SPDX-License-Identifier: CC0-1.0

//! The first question a resource type's schema makes decidable about a property's VALUE: the
//! TYPE that value has to have.
//!
//! A resource type's schema states a `type` for every property it declares, so a property
//! document carrying a value of another type is one no handler of that type could ever be
//! given: the stage that runs before provisioning reports it, naming the property and the type
//! its schema states, and the operation deploys nothing. That is the question this module
//! answers, beside the five [`super`] already answers (a required property the declaration
//! omits, a member the type does not have, a group it may carry at most one of, a value outside
//! the set its property publishes, and a value outside the pattern it admits).
//!
//! ## What counts as a value OF the stated type
//!
//! A template states its values in JSON, and CloudFormation COERCES the text form of a scalar
//! to the type the schema states: a quoted integer is an integer for a whole-number property
//! and quoted `true` is a boolean for a `Boolean` one, which is how a value that travelled
//! through a template parameter — where every value is text — reaches a property of either
//! type. So a value of the stated type is one that IS of it or one whose text spells a value of
//! it, and only a scalar that spells no such value at all is refused.
//!
//! ## What this stage deliberately does not judge
//!
//! The judgement is made of the DEPLOYED property document, as every judgement of that stage
//! is, and it is the same kind of catalogue: one row per (resource type, property) whose type
//! this emulator holds, so a property no row covers is left to the service that owns the
//! resource rather than guessed at. Only a SCALAR is judged: an object or a list is no spelling
//! of a scalar either way (an expression this service left unevaluated among them), and a
//! property the deployed document does not carry has no value to judge.
//!
//! Nor is a text carrying a DYNAMIC REFERENCE judged. This stage runs before a deployment
//! resolves those references ([`crate::state::dynamic_references`] resolves them as the
//! resources are created), so the text a declaration carries then is not the value the owning
//! service will be given — and what the reference stands for is a value of the stated type as
//! often as not. Judging it would refuse a declaration that deploys.

use ::serde_json::Value;

use super::{declared_properties::property_path, PropertyFailure, StoredResource};

/// The type a property's schema states for its value, over the types this stage judges.
///
/// The token each renders in a refusal ([`PropertyType::schema_type`]) is the one
/// CloudFormation's own property validator puts in that sentence: the SCHEMA's type for the
/// property rather than the prose spelling its reference documents, so a whole-number property
/// ("Type: Integer" in the reference) is reported as `Number`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PropertyType {
    /// A whole number ("Type: Integer" in the property reference).
    Integer,
    /// A flag ("Type: Boolean").
    Boolean,
}

impl PropertyType {
    /// The type's token, as the validator's sentence renders it.
    pub fn schema_type(self) -> &'static str {
        match self {
            Self::Integer => "Number",
            Self::Boolean => "Boolean",
        }
    }

    /// Whether `declared` is a value OF this type: one already of it, or a scalar whose text
    /// spells one of it (the coercion a template's text form is carried through).
    ///
    /// A value that is not a scalar is admitted: it is no spelling of a scalar either way, and
    /// what it fails is a constraint this stage does not judge.
    pub fn admits(self, declared: &Value) -> bool {
        match declared {
            // Not a scalar: an object, a list, and the absence of a value are no spelling of a
            // scalar either way, and what they fail is a constraint this stage does not judge.
            Value::Null | Value::Array(_) | Value::Object(_) => true,
            // A whole number, written as one...
            Value::Number(number) => {
                self == Self::Integer && (number.is_i64() || number.is_u64())
            }
            // A `Boolean` is a flag, written as one...
            Value::Bool(_) => self == Self::Boolean,
            Value::String(text) => {
                // ...or as text. A text carrying a DYNAMIC REFERENCE is not the value the
                // owning service will be given at all: this stage runs before a deployment
                // resolves those references, so nothing about the value one stands for can be
                // decided here.
                if crate::state::dynamic_references::carries_a_reference(text) {
                    return true;
                }
                match self {
                    // The text of a whole number, which is how a quoted value and one that
                    // travelled through a template parameter reach such a property.
                    Self::Integer => text.trim().parse::<i64>().is_ok(),
                    // ...and the text `true`/`false` — the spelling the reference publishes for
                    // a boolean property, read however the author cased it, since a template
                    // that spells the flag out states the flag.
                    Self::Boolean => {
                        let text = text.trim();
                        text.eq_ignore_ascii_case("true") || text.eq_ignore_ascii_case("false")
                    }
                }
            }
        }
    }
}

/// One property whose type its resource type's schema states.
#[derive(Debug, Clone, Copy)]
pub struct TypedProperty {
    /// The resource type the property belongs to, as a template declares it.
    pub resource_type: &'static str,
    /// The property's name, as a template spells it.
    pub property: &'static str,
    /// The type its schema states for the property.
    pub declared_type: PropertyType,
}

/// The properties this stage knows a stated type of.
///
/// Source for each row: the `Type` the resource type's entry in the "AWS resource and property
/// types reference" of the CloudFormation User Guide publishes for that property, which is the
/// `type` the type's registry schema states — and which, for a property that IS a member of the
/// owning API, is the form that API documents for the member the property is passed to, so a
/// value of another type is one that service would refuse and this stage refuses it first.
///
/// Only the properties whose type is a SCALAR are rows: a document or a list is judged by the
/// service that owns the resource ([`super`] says why in full).
pub const TYPED_PROPERTIES: &[TypedProperty] = &[
    // `AWS::SQS::Queue`'s scalar properties ARE the queue's own attributes, and Amazon SQS
    // publishes the form of each beside it: "Valid values: An integer from 0 to 43,200 (12
    // hours)" for the visibility timeout, "An integer from 60 seconds (1 minute) to 1,209,600
    // seconds (14 days)" for the retention period, "Valid values are true and false" for the
    // flags (`::service_com_amazonaws_sqs_amazonsqs::state::SETTABLE_ATTRIBUTES` holds each
    // sentence beside its attribute). `KmsMasterKeyId`, `DeduplicationScope`,
    // `FifoThroughputLimit` and `QueueName` are text and no row: what they admit is not a type.
    TypedProperty {
        resource_type: "AWS::SQS::Queue",
        property: "ContentBasedDeduplication",
        declared_type: PropertyType::Boolean,
    },
    TypedProperty {
        resource_type: "AWS::SQS::Queue",
        property: "DelaySeconds",
        declared_type: PropertyType::Integer,
    },
    TypedProperty {
        resource_type: "AWS::SQS::Queue",
        property: "FifoQueue",
        declared_type: PropertyType::Boolean,
    },
    TypedProperty {
        resource_type: "AWS::SQS::Queue",
        property: "KmsDataKeyReusePeriodSeconds",
        declared_type: PropertyType::Integer,
    },
    TypedProperty {
        resource_type: "AWS::SQS::Queue",
        property: "MaximumMessageSize",
        declared_type: PropertyType::Integer,
    },
    TypedProperty {
        resource_type: "AWS::SQS::Queue",
        property: "MessageRetentionPeriod",
        declared_type: PropertyType::Integer,
    },
    TypedProperty {
        resource_type: "AWS::SQS::Queue",
        property: "ReceiveMessageWaitTimeSeconds",
        declared_type: PropertyType::Integer,
    },
    TypedProperty {
        resource_type: "AWS::SQS::Queue",
        property: "SqsManagedSseEnabled",
        declared_type: PropertyType::Boolean,
    },
    TypedProperty {
        resource_type: "AWS::SQS::Queue",
        property: "VisibilityTimeout",
        declared_type: PropertyType::Integer,
    },
    // `AWS::Logs::LogGroup`'s `RetentionInDays` is `PutRetentionPolicy$retentionInDays`, "the
    // number of days to retain the log events", which that API declares an integer.
    TypedProperty {
        resource_type: "AWS::Logs::LogGroup",
        property: "RetentionInDays",
        declared_type: PropertyType::Integer,
    },
    // `AWS::Lambda::Function`'s `MemorySize` and `Timeout` are `CreateFunction$MemorySize` ("the
    // amount of memory available to the function at runtime") and `CreateFunction$Timeout` ("the
    // amount of time … that Lambda allows a function to run before stopping it"), both integers
    // in that API.
    TypedProperty {
        resource_type: "AWS::Lambda::Function",
        property: "MemorySize",
        declared_type: PropertyType::Integer,
    },
    TypedProperty {
        resource_type: "AWS::Lambda::Function",
        property: "Timeout",
        declared_type: PropertyType::Integer,
    },
    // `AWS::SNS::Subscription`'s `RawMessageDelivery` is the `RawMessageDelivery` subscription
    // attribute, "enables raw message delivery", whose published values are `true` and `false`.
    TypedProperty {
        resource_type: "AWS::SNS::Subscription",
        property: "RawMessageDelivery",
        declared_type: PropertyType::Boolean,
    },
];

/// `Some(failure)` for a resource carrying a property value of a type its type's schema does
/// not state for that property ([`TYPED_PROPERTIES`]) — the first such property in the order
/// the catalogue states them; `None` for a type with no row, for a property the declaration
/// does not carry, for a value that is not a scalar, and for every value of the stated type.
pub(super) fn value_of_another_type(resource: &StoredResource) -> Option<PropertyFailure> {
    TYPED_PROPERTIES
        .iter()
        .filter(|row| row.resource_type == resource.resource_type)
        .find_map(|row| {
            let declared = resource.properties.get(row.property)?;
            if row.declared_type.admits(declared) {
                return None;
            }
            Some(PropertyFailure {
                logical_id: resource.logical_id.clone(),
                resource_type: resource.resource_type.clone(),
                reason: type_failure_reason(row.property, row.declared_type),
                path: property_path(&resource.logical_id, row.property),
            })
        })
}

/// How CloudFormation reports a property value whose type its schema does not state: fixed text
/// around the PROPERTY it is about, in the path form a resource's property is named in, and the
/// type that property is held to, as the validator's sentence renders it
/// ([`PropertyType::schema_type`]).
///
/// The value the declaration carried is deliberately NOT quoted back, as it is not for the
/// set-membership failure beside it ([`super::failure_reason`]): it may be a value a template
/// author marked `NoEcho`, and what the author has to act on is the type the property states.
pub fn type_failure_reason(property: &str, declared_type: PropertyType) -> String {
    format!(
        "Property validation failure: [Value of property {{/{property}}} does not match type \
         {{{}}}]",
        declared_type.schema_type()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::serde_json::json;
    use ::std::collections::BTreeMap;

    /// One declared resource of `resource_type` carrying `properties`.
    fn declared(logical_id: &str, resource_type: &str, properties: Value) -> StoredResource {
        StoredResource {
            logical_id: logical_id.to_string(),
            resource_type: resource_type.to_string(),
            physical_resource_id: format!("{logical_id}-physical"),
            attributes: BTreeMap::new(),
            metadata: None,
            properties,
            materialised: false,
            deletion_policy: None,
            update_replace_policy: None,
        }
    }

    /// One row of the table below: the case, the resource type declared, the property document
    /// it carries, and the `(property, type)` the stage must report — `None` where every judged
    /// value is of the type its property states.
    type TypeRow<'a> = (&'a str, &'a str, Value, Option<(&'a str, PropertyType)>);

    /// RULE: a scalar that spells no value of the type its property's schema states fails the
    /// validation, naming that property and that type at the property's own path, while a value
    /// of the stated type — including the TEXT form of one — does not.
    ///
    /// The rows span what the rule generalises over: which type is stated (integer, boolean),
    /// which resource type and property states it (four owning services, properties the report
    /// that led here did not cite among them), and the form the value takes (a number, the text
    /// of one, a fraction, a flag, the text of a flag, text that spells neither, a structure, a
    /// list, a dynamic reference nothing has resolved yet, and a property the document does not
    /// carry at all).
    #[test]
    fn a_value_of_another_type_fails_the_validation() {
        let rows: &[TypeRow] = &[
            (
                "a queue whose visibility timeout is text that spells no number",
                "AWS::SQS::Queue",
                json!({"QueueName": "q", "VisibilityTimeout": "not-a-number"}),
                Some(("VisibilityTimeout", PropertyType::Integer)),
            ),
            (
                "a queue whose visibility timeout is the TEXT of a number",
                "AWS::SQS::Queue",
                json!({"QueueName": "q", "VisibilityTimeout": "30"}),
                None,
            ),
            (
                "a queue whose visibility timeout is a number",
                "AWS::SQS::Queue",
                json!({"QueueName": "q", "VisibilityTimeout": 30}),
                None,
            ),
            (
                "a queue whose retention period is a fraction",
                "AWS::SQS::Queue",
                json!({"QueueName": "q", "MessageRetentionPeriod": 60.5}),
                Some(("MessageRetentionPeriod", PropertyType::Integer)),
            ),
            (
                "a queue whose delay is a flag",
                "AWS::SQS::Queue",
                json!({"QueueName": "q", "DelaySeconds": true}),
                Some(("DelaySeconds", PropertyType::Integer)),
            ),
            (
                "a queue whose FIFO flag is a number",
                "AWS::SQS::Queue",
                json!({"QueueName": "q.fifo", "FifoQueue": 1}),
                Some(("FifoQueue", PropertyType::Boolean)),
            ),
            (
                "a queue whose FIFO flag is the TEXT of a flag",
                "AWS::SQS::Queue",
                json!({"QueueName": "q.fifo", "FifoQueue": "true"}),
                None,
            ),
            (
                "a queue whose encryption flag is text that spells no flag",
                "AWS::SQS::Queue",
                json!({"QueueName": "q", "SqsManagedSseEnabled": "yes"}),
                Some(("SqsManagedSseEnabled", PropertyType::Boolean)),
            ),
            (
                "a queue whose encryption flag is the text of a flag, cased differently",
                "AWS::SQS::Queue",
                json!({"QueueName": "q", "SqsManagedSseEnabled": "False"}),
                None,
            ),
            (
                "a queue carrying none of the properties this stage judges",
                "AWS::SQS::Queue",
                json!({"QueueName": "q", "KmsMasterKeyId": "alias/aws/sqs"}),
                None,
            ),
            (
                "a queue whose timeout is an expression this service left unevaluated",
                "AWS::SQS::Queue",
                json!({"QueueName": "q", "VisibilityTimeout": {"Ref": "Timeout"}}),
                None,
            ),
            (
                "a queue whose timeout is a dynamic reference nothing has resolved yet",
                "AWS::SQS::Queue",
                json!({"QueueName": "q",
                       "VisibilityTimeout": "{{resolve:ssm:/queue/timeout}}"}),
                None,
            ),
            (
                "a queue whose encryption flag is a dynamic reference nothing has resolved yet",
                "AWS::SQS::Queue",
                json!({"QueueName": "q",
                       "SqsManagedSseEnabled": "{{resolve:ssm:/queue/encrypted}}"}),
                None,
            ),
            (
                "a log group whose retention is text that spells no number",
                "AWS::Logs::LogGroup",
                json!({"LogGroupName": "/g", "RetentionInDays": "a fortnight"}),
                Some(("RetentionInDays", PropertyType::Integer)),
            ),
            (
                "a log group whose retention is the text of a number",
                "AWS::Logs::LogGroup",
                json!({"LogGroupName": "/g", "RetentionInDays": "14"}),
                None,
            ),
            (
                "a function whose memory size is a list",
                "AWS::Lambda::Function",
                json!({"MemorySize": [128], "Role": "arn:aws:iam::000000000000:role/r"}),
                None,
            ),
            (
                "a function whose timeout is text that spells no number",
                "AWS::Lambda::Function",
                json!({"Timeout": "3s", "Role": "arn:aws:iam::000000000000:role/r"}),
                Some(("Timeout", PropertyType::Integer)),
            ),
            (
                "a subscription whose raw delivery flag is a number",
                "AWS::SNS::Subscription",
                json!({"TopicArn": "arn:aws:sns:us-east-1:000000000000:t",
                       "Protocol": "sqs", "RawMessageDelivery": 0}),
                Some(("RawMessageDelivery", PropertyType::Boolean)),
            ),
            (
                "a subscription whose raw delivery flag is the text of a flag",
                "AWS::SNS::Subscription",
                json!({"TopicArn": "arn:aws:sns:us-east-1:000000000000:t",
                       "Protocol": "sqs", "RawMessageDelivery": "false"}),
                None,
            ),
            (
                "a type this stage knows no stated type of",
                "AWS::SNS::Topic",
                json!({"FifoTopic": "neither true nor false"}),
                None,
            ),
        ];
        for (case, resource_type, properties, reported) in rows {
            let resources = vec![declared("R", resource_type, properties.clone())];
            let failed = super::super::failure(&resources);
            let expected = reported.map(|(property, declared_type)| {
                (
                    type_failure_reason(property, declared_type),
                    format!("/Resources/R/Properties/{property}"),
                )
            });
            assert_eq!(
                failed
                    .as_ref()
                    .map(|failure| (failure.reason.clone(), failure.path.clone())),
                expected,
                "case {case:?}: expected the failure {expected:?}, got {failed:?}"
            );
        }
    }
}
