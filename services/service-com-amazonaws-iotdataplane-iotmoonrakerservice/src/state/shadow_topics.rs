// SPDX-License-Identifier: CC0-1.0

//! The reserved MQTT topics the Device Shadow service announces a shadow's own changes on,
//! and the messages it publishes there.
//!
//! A device does not poll a shadow: it SUBSCRIBES to that shadow's reserved topics and acts
//! on what the service pushes. So an accepted change is not only answered to the caller
//! that made it — it is announced to the shadow's subscribers:
//!
//! * `…/update/accepted` — the response state document the accepted update answered with.
//! * `…/update/documents` — the document as it stood BEFORE the update and the document
//!   after it, so a subscriber sees what changed without diffing anything itself.
//! * `…/update/delta` — the desired state the reported state does not match, published only
//!   when there is one. It is the difference of the whole merged `desired` against the whole
//!   merged `reported` at that moment ([`super::Shadow::delta`]), not the keys the request
//!   happened to write, so a leaf the device has since reported drops out of the next delta
//!   instead of being pushed at it again.
//! * `…/delete/accepted` — the version the deleted document stood at.
//!
//! The topics are the shadow's own: `$aws/things/{thingName}/shadow/…` for the unnamed
//! ("classic") shadow and `$aws/things/{thingName}/shadow/name/{shadowName}/…` for a named
//! one. They are in the `$` namespace this service refuses a CLIENT publish to
//! ([`crate::ops::publish`]) exactly because they are the service's own to write.
//!
//! The messages go out through the broker's own fan-out
//! ([`::service_common::iot_mqtt_broker::publish_to_subscribers`]), so a subscriber that is
//! offline with a persistent session has them queued as any other publication is, and a
//! shadow nobody subscribes to reaches nobody.

use ::serde_json::{Map, Value};

use super::{reply_with_client_token, Shadow};

/// The first level of the reserved namespace a thing's shadow topics live under.
pub const THINGS_PREFIX: &str = "$aws/things";

/// The level that names the shadow within a thing's reserved namespace, and the one that
/// introduces a NAMED shadow's own name.
pub const SHADOW_LEVEL: &str = "shadow";
/// See [`SHADOW_LEVEL`].
pub const NAME_LEVEL: &str = "name";

/// The QoS the service's own announcements are published at: the highest this broker
/// delivers at ([`::service_common::iot_mqtt_broker::wire::MAX_GRANTED_QOS`]), so a
/// subscriber that asked for an acknowledged delivery is given one.
const ANNOUNCEMENT_QOS: i32 = ::service_common::iot_mqtt_broker::wire::MAX_GRANTED_QOS;

/// The topic prefix of one shadow's reserved namespace — everything this module publishes
/// hangs off it.
#[must_use]
pub fn shadow_topic_prefix(thing_name: &str, shadow_name: Option<&str>) -> String {
    match shadow_name.filter(|name| !name.is_empty()) {
        None => format!("{THINGS_PREFIX}/{thing_name}/{SHADOW_LEVEL}"),
        Some(shadow_name) => {
            format!("{THINGS_PREFIX}/{thing_name}/{SHADOW_LEVEL}/{NAME_LEVEL}/{shadow_name}")
        }
    }
}

/// One accepted update, as its announcements are built from it.
pub struct AcceptedUpdate<'a> {
    /// The response state document the call answered with, which is what
    /// `…/update/accepted` carries.
    pub accepted: &'a Value,
    /// The stored document as it was BEFORE this update, and the version it stood at.
    /// `None` for a shadow this update created, which has no previous document at all.
    pub previous: Option<&'a Shadow>,
    /// See [`AcceptedUpdate::previous`].
    pub previous_version: i64,
    /// The stored document as this update left it, and the version it now stands at.
    pub current: &'a Shadow,
    /// See [`AcceptedUpdate::current`].
    pub version: i64,
    /// When the update was accepted, in epoch SECONDS — the unit every shadow document
    /// carries a timestamp in.
    pub timestamp_secs: i64,
    /// The request's own correlation token, echoed into the announcements that carry one.
    pub client_token: Option<&'a Value>,
}

/// Announce one accepted update on the shadow's reserved topics: `…/update/accepted` and
/// `…/update/documents` always, and `…/update/delta` when the merged desired state is not
/// matched by the merged reported state.
pub fn announce_update<Eng: Send + Sync + 'static>(
    engine: &Eng,
    thing_name: &str,
    shadow_name: Option<&str>,
    update: &AcceptedUpdate<'_>,
) {
    let Some(broker) = super::broker(engine) else {
        return;
    };
    let prefix = shadow_topic_prefix(thing_name, shadow_name);

    announce(
        &broker,
        format!("{prefix}/update/accepted"),
        update.accepted.clone(),
    );

    // The pair of documents: what the shadow was, and what it now is. A shadow this
    // update created was nothing before it, which is what a null `previous` says.
    let mut documents = Map::new();
    documents.insert(
        "previous".to_string(),
        match update.previous {
            None => Value::Null,
            Some(previous) => document_snapshot(previous, update.previous_version),
        },
    );
    documents.insert(
        "current".to_string(),
        document_snapshot(update.current, update.version),
    );
    documents.insert(
        "timestamp".to_string(),
        Value::from(update.timestamp_secs),
    );
    announce(
        &broker,
        format!("{prefix}/update/documents"),
        reply_with_client_token(Value::Object(documents), update.client_token),
    );

    // The delta, and only when there is one: a shadow whose reported state already
    // matches everything desired has nothing outstanding to push, which is what makes a
    // converging report announce the update without pushing a delta at all.
    if let Some(delta) = update.current.delta() {
        let mut message = Map::new();
        let metadata = update
            .current
            .desired
            .as_ref()
            .map_or_else(Map::new, |desired| desired.metadata.clone());
        message.insert("state".to_string(), delta.clone());
        message.insert("metadata".to_string(), metadata_of(&delta, &metadata));
        message.insert("version".to_string(), Value::from(update.version));
        message.insert("timestamp".to_string(), Value::from(update.timestamp_secs));
        // No correlation token: the delta is pushed at every subscriber of the shadow,
        // not only at the caller whose update made it outstanding.
        announce(
            &broker,
            format!("{prefix}/update/delta"),
            Value::Object(message),
        );
    }
}

/// Announce one accepted delete on `…/delete/accepted`: the version the document stood at
/// when it was taken away, which is the version the shadow's counter consumed for it.
pub fn announce_delete<Eng: Send + Sync + 'static>(
    engine: &Eng,
    thing_name: &str,
    shadow_name: Option<&str>,
    version: i64,
    timestamp_secs: i64,
    client_token: Option<&Value>,
) {
    let Some(broker) = super::broker(engine) else {
        return;
    };
    let prefix = shadow_topic_prefix(thing_name, shadow_name);
    let mut message = Map::new();
    message.insert("version".to_string(), Value::from(version));
    message.insert("timestamp".to_string(), Value::from(timestamp_secs));
    announce(
        &broker,
        format!("{prefix}/delete/accepted"),
        reply_with_client_token(Value::Object(message), client_token),
    );
}

/// One shadow document as a `…/update/documents` message carries it: its state and the
/// metadata mirror of that state, at the version it stood at.
///
/// `state.delta` is not part of it: the pair is the document BEFORE and AFTER the update,
/// and what is outstanding between the two sections is the delta message's own subject.
fn document_snapshot(shadow: &Shadow, version: i64) -> Value {
    let mut state = Map::new();
    let mut metadata = Map::new();
    for (name, section) in [
        ("desired", shadow.desired.as_ref()),
        ("reported", shadow.reported.as_ref()),
    ] {
        let Some(section) = section else { continue };
        state.insert(name.to_string(), Value::Object(section.state.clone()));
        metadata.insert(name.to_string(), Value::Object(section.metadata.clone()));
    }
    let mut document = Map::new();
    document.insert("state".to_string(), Value::Object(state));
    document.insert("metadata".to_string(), Value::Object(metadata));
    document.insert("version".to_string(), Value::from(version));
    Value::Object(document)
}

/// The metadata of the leaves `state` holds, cut out of the section's whole metadata
/// mirror: a message that carries PART of a section carries the metadata of that part
/// alone, so the delta's metadata names the leaves the delta names.
fn metadata_of(state: &Value, metadata: &Map<String, Value>) -> Value {
    let Value::Object(state) = state else {
        return Value::Object(Map::new());
    };
    let mut cut = Map::new();
    for (key, value) in state {
        let Some(held) = metadata.get(key) else {
            continue;
        };
        match (value, held) {
            (Value::Object(_), Value::Object(held)) => {
                cut.insert(key.clone(), metadata_of(value, held));
            }
            // A leaf's metadata is the leaf's own record, whatever the leaf is.
            _ => {
                cut.insert(key.clone(), held.clone());
            }
        }
    }
    Value::Object(cut)
}

/// Publish one announcement: the message goes to every session subscribed to a filter that
/// covers its topic, and a shadow nobody subscribes to reaches nobody.
fn announce(
    broker: &::engine::ServiceState<::service_common::iot_mqtt_broker::MqttBrokerState>,
    topic: String,
    body: Value,
) {
    let payload = ::serde_json::to_vec(&body).unwrap_or_default();
    ::service_common::iot_mqtt_broker::publish_to_subscribers(
        broker,
        &::service_common::iot_mqtt_broker::DirectMessage {
            topic,
            payload,
            qos: ANNOUNCEMENT_QOS,
            delivered_at_millis: super::now_millis(),
            ..::service_common::iot_mqtt_broker::DirectMessage::default()
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: a shadow's reserved topics are its own — the thing's `shadow` namespace for
    /// the unnamed shadow, and that namespace's `name/<shadowName>` subtree for a named
    /// one — so two shadows of one thing never announce on the same topic.
    ///
    /// The rows span what the rule generalises over: the unnamed shadow asked for as
    /// `None` and as the empty name the store keys it by, and named shadows whose names
    /// differ.
    #[test]
    fn each_shadow_announces_under_its_own_reserved_prefix() {
        let rows: &[(&str, &str, Option<&str>, &str)] = &[
            (
                "the unnamed shadow",
                "thermostat-1",
                None,
                "$aws/things/thermostat-1/shadow",
            ),
            (
                "the unnamed shadow, as the store keys it",
                "thermostat-1",
                Some(""),
                "$aws/things/thermostat-1/shadow",
            ),
            (
                "a named shadow",
                "thermostat-1",
                Some("firmware"),
                "$aws/things/thermostat-1/shadow/name/firmware",
            ),
            (
                "another named shadow of the same thing",
                "thermostat-1",
                Some("telemetry"),
                "$aws/things/thermostat-1/shadow/name/telemetry",
            ),
        ];
        for (case, thing_name, shadow_name, expected) in rows {
            let prefix = shadow_topic_prefix(thing_name, *shadow_name);
            assert_eq!(
                prefix, *expected,
                "case {case:?}: expected {expected:?}, got {prefix:?}"
            );
        }
    }

    /// RULE: a message carrying part of a section carries the metadata of that part alone.
    #[test]
    fn the_metadata_of_a_partial_state_names_that_states_leaves() {
        let metadata: Map<String, Value> = ::serde_json::from_value(::serde_json::json!({
            "alpha": {"timestamp": 11},
            "beta": {"timestamp": 12},
            "nested": {"inner": {"timestamp": 13}, "other": {"timestamp": 14}},
        }))
        .expect("the metadata mirror is an object");
        let rows: &[(&str, Value, Value)] = &[
            (
                "one leaf",
                ::serde_json::json!({"alpha": 1}),
                ::serde_json::json!({"alpha": {"timestamp": 11}}),
            ),
            (
                "two leaves",
                ::serde_json::json!({"alpha": 1, "beta": 2}),
                ::serde_json::json!({"alpha": {"timestamp": 11}, "beta": {"timestamp": 12}}),
            ),
            (
                "one leaf of a nested object",
                ::serde_json::json!({"nested": {"inner": 3}}),
                ::serde_json::json!({"nested": {"inner": {"timestamp": 13}}}),
            ),
            (
                "a leaf the mirror does not hold",
                ::serde_json::json!({"gamma": 4}),
                ::serde_json::json!({}),
            ),
            (
                "no state at all",
                ::serde_json::json!({}),
                ::serde_json::json!({}),
            ),
        ];
        for (case, state, expected) in rows {
            let cut = metadata_of(state, &metadata);
            assert_eq!(
                cut, *expected,
                "case {case:?}: expected {expected}, got {cut}"
            );
        }
    }
}
