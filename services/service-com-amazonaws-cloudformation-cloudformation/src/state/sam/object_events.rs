// SPDX-License-Identifier: CC0-1.0

//! The OBJECT-EVENT source a SAM function declares: `Events: { <name>: { Type: S3 } }`, and
//! what the transform has to produce for the deployed function to be triggered by it.
//!
//! An S3 event source is the one event type that does not become a resource of its own.
//! "Bucket: S3 bucket name. This bucket must exist in the same template" (the `S3` event
//! source's own `Bucket` property), and the thing that makes a bucket publish to a function is
//! that bucket's OWN `NotificationConfiguration` — so the transform writes the configuration
//! into the bucket the event names, beside the grant that lets Amazon S3 invoke the function:
//!
//! * one `LambdaConfigurations` entry per event type the source lists, naming the function and
//!   carrying the key-name `Filter` the source states ([`injected_notifications`]);
//! * an `AWS::Lambda::Permission` admitting the Amazon S3 service principal, scoped to the
//!   account the stack is deployed in — which is the one scope available to it, since a
//!   grant scoped to the publishing bucket's ARN would make the bucket and the permission
//!   depend on each other ([`invoke_permission`]);
//! * and the bucket ordered AFTER that permission, because S3 validates a Lambda destination
//!   against the function's own policy when the configuration is put, so a bucket carrying the
//!   configuration before the grant exists could not be created at all ([`ordered_after`]).
//!
//! Without all three a stack whose function declares an S3 event deploys a bucket that
//! publishes nothing: the control plane reports the function and the bucket as created while
//! every object written to that bucket reaches no function.

use ::serde_json::{json, Map, Value};

/// The `Type` an object-event source states.
pub(super) const S3_EVENT: &str = "S3";

/// The resource type the bucket an S3 event source names has to be, since the configuration is
/// written into that resource's own properties.
const BUCKET_TYPE: &str = "AWS::S3::Bucket";

/// The property a bucket publishes its event notifications under, and the list of them that
/// names a function destination.
const NOTIFICATION_CONFIGURATION: &str = "NotificationConfiguration";
const LAMBDA_CONFIGURATIONS: &str = "LambdaConfigurations";

/// The service principal Amazon S3 publishes a bucket's notifications under.
const S3_PRINCIPAL: &str = "s3.amazonaws.com";

/// One S3 event source, as the transform reads it off a function's `Events`.
pub(super) struct ObjectEvent {
    /// The logical id of the function the event triggers.
    pub(super) function: String,
    /// The logical id the generated permission is added under, which is the function's own id
    /// followed by the event's name, as every other event source of this transform names the
    /// resource it synthesizes.
    pub(super) permission_id: String,
    /// The logical id of the bucket resource the source names through its `Bucket` property.
    pub(super) bucket: String,
    /// The `Condition` the function's declaration carries, which the generated permission
    /// carries too.
    pub(super) condition: Option<Value>,
    /// The event types the source lists: "Events: The S3 bucket event for which to invoke the
    /// Lambda function", stated as one value or a list of them.
    pub(super) events: Vec<Value>,
    /// The key-name `Filter` the source states, or `None` where it states none (and so every
    /// key published by one of those event types invokes the function).
    pub(super) filter: Option<Value>,
}

/// The S3 event source `declaration` is, or `None` when it is an event source of another type
/// or one that names no bucket resource of this template.
///
/// The `Bucket` has to be a reference to a bucket of the same template, because the
/// configuration is written into that resource: "This bucket must exist in the same template".
/// A source naming anything else is left alone rather than expanded against a bucket this
/// transform cannot reach.
pub(super) fn object_event(
    function: &str,
    event: &str,
    properties: &Map<String, Value>,
    condition: Option<&Value>,
) -> Option<ObjectEvent> {
    let bucket = referenced_bucket(properties.get("Bucket"))?;
    let events = match properties.get("Events") {
        Some(Value::Array(listed)) => listed.clone(),
        Some(value) => vec![value.clone()],
        None => return None,
    };
    if events.is_empty() {
        return None;
    }
    Some(ObjectEvent {
        function: function.to_string(),
        permission_id: format!("{function}{event}Permission"),
        bucket,
        condition: condition.cloned(),
        events,
        filter: properties.get("Filter").cloned(),
    })
}

/// The logical id a `Bucket` property references, for the one form that names a resource of
/// this template.
fn referenced_bucket(value: Option<&Value>) -> Option<String> {
    let referenced = value?.get("Ref")?.as_str()?;
    (!referenced.is_empty()).then(|| referenced.to_string())
}

/// The grant that lets Amazon S3 invoke the function this event triggers.
///
/// Scoped to the ACCOUNT rather than to the publishing bucket: the bucket is ordered after this
/// permission ([`ordered_after`]), so a `SourceArn` read off that bucket would make the two
/// resources depend on each other and the stack undeployable. The account scope is what the
/// grant can state without that cycle, and it is the scope AWS SAM's own expansion of this
/// event source states.
pub(super) fn invoke_permission(event: &ObjectEvent) -> Map<String, Value> {
    let mut permission = Map::new();
    permission.insert("Action".to_string(), json!("lambda:InvokeFunction"));
    permission.insert(
        "FunctionName".to_string(),
        json!({ "Fn::GetAtt": [event.function.clone(), "Arn"] }),
    );
    permission.insert("Principal".to_string(), json!(S3_PRINCIPAL));
    permission.insert(
        "SourceAccount".to_string(),
        json!({ "Ref": "AWS::AccountId" }),
    );
    permission
}

/// Write the configurations `event` owes into the bucket resource it names, and order that
/// bucket after the grant.
///
/// `bucket` is the resource the template declares under the referenced logical id, which this
/// takes as it is: a declaration of another type is left alone, since nothing but a bucket
/// publishes a `NotificationConfiguration`. An entry the bucket already carries is not added a
/// second time, so a template that declares the configuration itself and an event source for
/// the same function and event publishes it once.
pub(super) fn inject(bucket: &mut Value, event: &ObjectEvent) {
    if bucket.get("Type").and_then(Value::as_str) != Some(BUCKET_TYPE) {
        return;
    }
    let Some(members) = bucket.as_object_mut() else {
        return;
    };
    let properties = members
        .entry("Properties".to_string())
        .or_insert_with(|| json!({}));
    let Some(properties) = properties.as_object_mut() else {
        return;
    };
    let configuration = properties
        .entry(NOTIFICATION_CONFIGURATION.to_string())
        .or_insert_with(|| json!({}));
    let Some(configuration) = configuration.as_object_mut() else {
        return;
    };
    let listed = configuration
        .entry(LAMBDA_CONFIGURATIONS.to_string())
        .or_insert_with(|| json!([]));
    let Some(listed) = listed.as_array_mut() else {
        return;
    };
    for entry in injected_notifications(event) {
        if !listed.contains(&entry) {
            listed.push(entry);
        }
    }
    ordered_after(members, &event.permission_id);
}

/// The `LambdaConfigurations` entries one source contributes: one per event type it lists,
/// each naming the function and carrying the source's own `Filter`.
///
/// `Function` is the function's ARN, which is what `AWS::S3::Bucket`'s own configuration takes
/// for a Lambda destination.
fn injected_notifications(event: &ObjectEvent) -> Vec<Value> {
    event
        .events
        .iter()
        .map(|kind| {
            let mut entry = Map::new();
            entry.insert("Event".to_string(), kind.clone());
            entry.insert(
                "Function".to_string(),
                json!({ "Fn::GetAtt": [event.function.clone(), "Arn"] }),
            );
            if let Some(filter) = event.filter.clone() {
                entry.insert("Filter".to_string(), filter);
            }
            Value::Object(entry)
        })
        .collect()
}

/// Add `permission_id` to the resource's `DependsOn`, whatever form it was written in — a
/// declaration states one id as a string and several as a list, and both have to end up
/// carrying this one as well.
fn ordered_after(resource: &mut Map<String, Value>, permission_id: &str) {
    let ordered = json!(permission_id);
    match resource.get_mut("DependsOn") {
        Some(Value::Array(listed)) => {
            if !listed.contains(&ordered) {
                listed.push(ordered);
            }
        }
        Some(single) => {
            if single != &ordered {
                let held = single.clone();
                *single = Value::Array(vec![held, ordered]);
            }
        }
        None => {
            resource.insert("DependsOn".to_string(), Value::Array(vec![ordered]));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bucket resource a row injects into.
    fn bucket(properties: Value) -> Value {
        json!({ "Type": BUCKET_TYPE, "Properties": properties })
    }

    /// An event source as a template states it.
    fn source(properties: Value) -> ObjectEvent {
        let members = properties.as_object().cloned().unwrap_or_default();
        object_event("Processor", "ObjectCreated", &members, None)
            .expect("the source names a bucket of this template")
    }

    /// Every form the `Events` property takes becomes one configuration entry per event type,
    /// each naming the function and carrying the filter the source states.
    #[test]
    fn each_listed_event_type_becomes_one_configuration() {
        let rows: Vec<(&str, Value, usize)> = vec![
            (
                "one event type as a single value",
                json!({ "Bucket": { "Ref": "Data" }, "Events": "s3:ObjectCreated:*" }),
                1,
            ),
            (
                "one event type as a list",
                json!({ "Bucket": { "Ref": "Data" }, "Events": ["s3:ObjectCreated:Put"] }),
                1,
            ),
            (
                "two event types",
                json!({
                    "Bucket": { "Ref": "Data" },
                    "Events": ["s3:ObjectCreated:Put", "s3:ObjectRemoved:*"],
                }),
                2,
            ),
            (
                "an event type beside a key-name filter",
                json!({
                    "Bucket": { "Ref": "Data" },
                    "Events": "s3:ObjectCreated:*",
                    "Filter": { "S3Key": { "Rules": [{ "Name": "prefix", "Value": "in/" }] } },
                }),
                1,
            ),
        ];
        for (case, properties, expected) in rows {
            let event = source(properties.clone());
            let entries = injected_notifications(&event);
            assert_eq!(
                entries.len(),
                expected,
                "case {case:?}: expected {expected} configuration(s), got {entries:?}"
            );
            for entry in &entries {
                assert_eq!(
                    entry.get("Function"),
                    Some(&json!({ "Fn::GetAtt": ["Processor", "Arn"] })),
                    "case {case:?}: expected the entry to name the function's ARN, got {entry}"
                );
                assert_eq!(
                    entry.get("Filter"),
                    properties.get("Filter"),
                    "case {case:?}: expected the source's own filter, got {entry}"
                );
            }
        }
    }

    /// A source naming no bucket of this template, or no event at all, expands to nothing.
    #[test]
    fn a_source_that_names_no_bucket_resource_or_no_event_expands_to_nothing() {
        let rows: Vec<(&str, Value)> = vec![
            ("no properties at all", json!({})),
            (
                "a bucket named by a literal rather than a reference",
                json!({ "Bucket": "already-there", "Events": "s3:ObjectCreated:*" }),
            ),
            (
                "a reference to nothing",
                json!({ "Bucket": { "Ref": "" }, "Events": "s3:ObjectCreated:*" }),
            ),
            ("a bucket with no events", json!({ "Bucket": { "Ref": "Data" } })),
            (
                "an empty event list",
                json!({ "Bucket": { "Ref": "Data" }, "Events": [] }),
            ),
        ];
        for (case, properties) in rows {
            let members = properties.as_object().cloned().unwrap_or_default();
            let read = object_event("Processor", "ObjectCreated", &members, None);
            assert!(
                read.is_none(),
                "case {case:?}: expected no object event to be read, got one"
            );
        }
    }

    /// The injection writes into the bucket the source names: the configuration it owes, the
    /// entries already there kept, and the ordering after the grant — however the bucket's own
    /// `DependsOn` was written.
    #[test]
    fn the_bucket_carries_the_configuration_and_is_ordered_after_the_grant() {
        let held = json!({
            "Event": "s3:ObjectRemoved:*",
            "Function": { "Fn::GetAtt": ["Other", "Arn"] },
        });
        let rows: Vec<(&str, Value, usize)> = vec![
            ("a bucket declaring no properties", json!(null), 1),
            ("a bucket declaring no configuration", json!({ "BucketName": "data" }), 1),
            (
                "a bucket already carrying a configuration of its own",
                json!({ "NotificationConfiguration": { "LambdaConfigurations": [held.clone()] } }),
                2,
            ),
        ];
        let event = source(json!({ "Bucket": { "Ref": "Data" }, "Events": "s3:ObjectCreated:*" }));
        for (case, properties, expected) in rows {
            let mut resource = bucket(properties);
            if resource["Properties"].is_null() {
                resource
                    .as_object_mut()
                    .expect("a resource object")
                    .remove("Properties");
            }
            inject(&mut resource, &event);
            let listed = resource["Properties"][NOTIFICATION_CONFIGURATION]
                [LAMBDA_CONFIGURATIONS]
                .as_array()
                .cloned()
                .unwrap_or_default();
            assert_eq!(
                listed.len(),
                expected,
                "case {case:?}: expected {expected} configuration(s) on the bucket, got {listed:?}"
            );
            assert_eq!(
                resource["DependsOn"],
                json!(["ProcessorObjectCreatedPermission"]),
                "case {case:?}: expected the bucket ordered after the grant, got {resource}"
            );
            // Injecting the same source twice adds nothing: the entry and the ordering are
            // already there.
            inject(&mut resource, &event);
            let again = resource["Properties"][NOTIFICATION_CONFIGURATION]
                [LAMBDA_CONFIGURATIONS]
                .as_array()
                .cloned()
                .unwrap_or_default();
            assert_eq!(
                again.len(),
                expected,
                "case {case:?}: expected the second injection to add nothing, got {again:?}"
            );
        }
    }

    /// A bucket whose declaration already orders it after something keeps that ordering beside
    /// the grant's.
    #[test]
    fn an_existing_ordering_is_kept_beside_the_grants() {
        let event = source(json!({ "Bucket": { "Ref": "Data" }, "Events": "s3:ObjectCreated:*" }));
        let rows: Vec<(&str, Value, Value)> = vec![
            (
                "one id written as a string",
                json!("Role"),
                json!(["Role", "ProcessorObjectCreatedPermission"]),
            ),
            (
                "ids written as a list",
                json!(["Role", "Queue"]),
                json!(["Role", "Queue", "ProcessorObjectCreatedPermission"]),
            ),
            (
                "the grant's own id already there",
                json!(["ProcessorObjectCreatedPermission"]),
                json!(["ProcessorObjectCreatedPermission"]),
            ),
        ];
        for (case, declared, expected) in rows {
            let mut resource = bucket(json!({ "BucketName": "data" }));
            resource
                .as_object_mut()
                .expect("a resource object")
                .insert("DependsOn".to_string(), declared);
            inject(&mut resource, &event);
            assert_eq!(
                resource["DependsOn"], expected,
                "case {case:?}: expected {expected}, got {}",
                resource["DependsOn"]
            );
        }
    }

    /// The grant admits the Amazon S3 service principal for this account, and names the
    /// function the event triggers.
    #[test]
    fn the_grant_admits_the_service_principal_for_this_account() {
        let event = source(json!({ "Bucket": { "Ref": "Data" }, "Events": "s3:ObjectCreated:*" }));
        let permission = invoke_permission(&event);
        let expected: Vec<(&str, Value)> = vec![
            ("Action", json!("lambda:InvokeFunction")),
            ("FunctionName", json!({ "Fn::GetAtt": ["Processor", "Arn"] })),
            ("Principal", json!(S3_PRINCIPAL)),
            ("SourceAccount", json!({ "Ref": "AWS::AccountId" })),
        ];
        for (member, value) in expected {
            assert_eq!(
                permission.get(member),
                Some(&value),
                "expected {member} to be {value}, got {permission:?}"
            );
        }
        assert!(
            !permission.contains_key("SourceArn"),
            "a grant scoped to the publishing bucket would make the two depend on each other, \
             got {permission:?}"
        );
    }
}
