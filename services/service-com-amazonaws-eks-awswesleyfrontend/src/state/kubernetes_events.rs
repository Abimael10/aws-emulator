// SPDX-License-Identifier: CC0-1.0

//! The EVENTS a cluster's API server reports about its own objects.
//!
//! An `Event` is the Kubernetes record of "an event in the cluster" — what one of
//! its components did to one object, with the reason it gives and the sentence it
//! words it in — and it is the resource a client reads to learn what became of a
//! pod it created: `kubectl describe pod` and
//! `GET /api/v1/namespaces/{namespace}/events` both read exactly these objects.
//!
//! Every event here is recorded by the cluster ITSELF, for something it really
//! did: the scheduler binding a pod to a node, the node's kubelet pulling the
//! image a container runs ([`super::kubernetes_image_pull`]), and the container
//! start that pull was made for. Nothing creates an event through this API — the
//! `events` resource is served for reading, as the collection a client watches —
//! and no event is written for work this emulator did not perform.
//!
//! The objects live with the rest of the namespace's objects
//! ([`super::kubernetes_objects`]), keyed by the name the cluster gave each event,
//! so deleting the cluster takes its events with it and two clusters never share
//! one.

use super::kubernetes_lists::{listed, ListRequest};
use super::kubernetes_objects::{new_object_uid, rfc3339_seconds, ApiRefusal, CORE_API_VERSION};
use super::*;

/// How many events ONE namespace retains.
///
/// CHOSEN, not published: a cluster component records an event per thing it does,
/// so the collection grows with the cluster's own work and needs a bound of its
/// own. At the bound the OLDEST event is dropped rather than the newest refused,
/// which is what an event recorder does — "events are not a reliable, long-term
/// record" — so the recent history a client reads is always there.
pub const MAX_EVENTS_PER_NAMESPACE: usize = 256;

/// `Event$type` of an event reporting something the cluster expected.
pub const EVENT_TYPE_NORMAL: &str = "Normal";

/// `Event$type` of an event reporting something that went wrong.
pub const EVENT_TYPE_WARNING: &str = "Warning";

/// The component that reports a scheduling decision: the Kubernetes scheduler,
/// under the name the default scheduler of a cluster reports itself as.
pub const EVENT_SOURCE_SCHEDULER: &str = "default-scheduler";

/// The component that reports what became of a pod ON a node: that node's
/// kubelet, which is what pulls the images and starts the containers.
pub const EVENT_SOURCE_KUBELET: &str = "kubelet";

/// The `kind` of the objects recorded here, and of the list they are answered in.
const EVENT_KIND: &str = "Event";
const EVENT_LIST_KIND: &str = "EventList";

/// The `involvedObject.kind` of a pod, which these events are about.
pub const INVOLVED_KIND_POD: &str = "Pod";

/// What a component of the cluster RECORDED about one object: the object, what it
/// did and the sentence it said it in.
///
/// Carried as one value because an event is one thing, not ten parameters, and
/// because the name the cluster gives it is derived from several of them at once
/// ([`event_name`]).
#[derive(Debug)]
pub struct RecordedEvent<'event> {
    /// `involvedObject.kind`/`name`/`uid`: WHICH object the event is about, named
    /// and identified as that object's own read reports it.
    pub involved_kind: &'event str,
    pub involved_name: &'event str,
    pub involved_uid: &'event str,
    /// `involvedObject.fieldPath`: "if referring to a piece of an object instead
    /// of an entire object" — the container of a pod an event about one image is
    /// about, in the `spec.containers{<name>}` form a kubelet writes. `None` for
    /// an event about the whole object.
    pub field_path: Option<String>,
    /// `reason`: "this should be a short, machine understandable string that
    /// gives the reason for the transition into the object's current status".
    pub reason: &'event str,
    /// `message`: "a human-readable description of the status of this operation".
    pub message: String,
    /// `type`: [`EVENT_TYPE_NORMAL`] or [`EVENT_TYPE_WARNING`].
    pub event_type: &'event str,
    /// `source.component`: the component that recorded it.
    pub component: &'event str,
    /// `source.host`: the node it was recorded on, for an event a kubelet
    /// recorded; `None` for one a control-plane component recorded.
    pub host: Option<String>,
    /// When the component recorded it.
    pub at: SystemTime,
}

/// One event of a namespace, as the cluster recorded it.
#[derive(Debug, Clone)]
pub struct EventObject {
    /// The event's own `metadata.name`, in the `<object>.<id>` form Kubernetes
    /// names an event after the object it is about ([`event_name`]).
    pub name: String,
    /// Its `metadata.uid`.
    pub uid: String,
    pub involved_kind: String,
    pub involved_name: String,
    pub involved_uid: String,
    pub field_path: Option<String>,
    pub reason: String,
    pub message: String,
    pub event_type: String,
    pub component: String,
    pub host: Option<String>,
    /// When the component recorded it, which is both the first and the last
    /// instant it reports: each event here is recorded for ONE thing that
    /// happened, so nothing aggregates into it.
    pub at: SystemTime,
}

impl EventObject {
    /// The event `recorded` describes.
    #[must_use]
    pub fn of(recorded: RecordedEvent<'_>) -> Self {
        Self {
            name: event_name(&recorded),
            uid: new_object_uid(),
            involved_kind: recorded.involved_kind.to_string(),
            involved_name: recorded.involved_name.to_string(),
            involved_uid: recorded.involved_uid.to_string(),
            field_path: recorded.field_path,
            reason: recorded.reason.to_string(),
            message: recorded.message,
            event_type: recorded.event_type.to_string(),
            component: recorded.component.to_string(),
            host: recorded.host,
            at: recorded.at,
        }
    }

    /// This event as the `Event` document a client reads.
    #[must_use]
    pub fn to_json(&self, namespace: &str) -> ::serde_json::Value {
        let at = rfc3339_seconds(self.at);
        let mut involved = ::serde_json::json!({
            "kind": self.involved_kind,
            "namespace": namespace,
            "name": self.involved_name,
            "uid": self.involved_uid,
            "apiVersion": CORE_API_VERSION,
        });
        if let Some(field_path) = &self.field_path {
            involved["fieldPath"] = ::serde_json::json!(field_path);
        }
        let mut source = ::serde_json::json!({ "component": self.component });
        if let Some(host) = &self.host {
            source["host"] = ::serde_json::json!(host);
        }
        let mut document = ::serde_json::json!({
            "kind": EVENT_KIND,
            "apiVersion": CORE_API_VERSION,
            "metadata": {
                "name": self.name,
                "namespace": namespace,
                "uid": self.uid,
                "creationTimestamp": at,
            },
            "involvedObject": involved,
            "reason": self.reason,
            "message": self.message,
            "source": source,
            "firstTimestamp": at,
            "lastTimestamp": at,
            // One event per thing that happened, so nothing is aggregated into
            // it: the count a client reads is the one occurrence it reports.
            "count": 1,
            "type": self.event_type,
            "reportingComponent": self.component,
        });
        if let Some(host) = &self.host {
            document["reportingInstance"] = ::serde_json::json!(host);
        }
        document
    }
}

/// The name Kubernetes gives an event: the object it is about, a dot, and an
/// identifier that tells the events of that object apart.
///
/// Derived from what the event IS — the object's uid, the reason, the part of the
/// object it is about, the instant it was recorded at and the sentence it carries
/// — so a read of the collection reports the same names on every call, two events
/// of one object never collide, and nothing in a request decides a name.
fn event_name(recorded: &RecordedEvent<'_>) -> String {
    let recorded_at = recorded
        .at
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |since_epoch| since_epoch.as_secs());
    let digest = Sha256::digest(
        format!(
            "event/{}/{}/{}/{recorded_at}/{}",
            recorded.involved_uid,
            recorded.reason,
            recorded.field_path.as_deref().unwrap_or_default(),
            recorded.message,
        )
        .as_bytes(),
    );
    format!("{}.{}", recorded.involved_name, hex::encode(&digest[..8]))
}

impl super::kubernetes_objects::ClusterObjects {
    /// Record `event` in `namespace`, dropping the oldest event of that namespace
    /// when it is already holding [`MAX_EVENTS_PER_NAMESPACE`].
    ///
    /// An event about an object of a namespace this cluster no longer holds is
    /// dropped: there is nothing for it to be about. One already recorded is kept
    /// as it was recorded, which is what makes recording it again — a second read
    /// materialising the same clock-owed step — change nothing a caller sees.
    pub fn record_event(&mut self, namespace: &str, event: EventObject) {
        let Some(held) = self.namespaces.get_mut(namespace) else {
            return;
        };
        if held.events.contains_key(&event.name) {
            return;
        }
        while held.events.len() >= MAX_EVENTS_PER_NAMESPACE {
            let oldest = held
                .events
                .iter()
                .min_by_key(|(name, event)| (event.at, (*name).clone()))
                .map(|(name, _)| name.clone());
            match oldest {
                Some(name) => {
                    held.events.remove(&name);
                }
                None => break,
            }
        }
        held.events.insert(event.name.clone(), event);
    }

    /// `GET /api/v1/namespaces/{namespace}/events/{name}`.
    pub fn event_json(
        &self,
        namespace: &str,
        name: &str,
    ) -> Result<::serde_json::Value, ApiRefusal> {
        self.namespace(namespace)?
            .events
            .get(name)
            .map(|event| event.to_json(namespace))
            .ok_or_else(|| ApiRefusal::not_found("events", name))
    }

    /// `GET /api/v1/namespaces/{namespace}/events`: the namespace's events, as the
    /// `EventList` a client reads — one chunk of it ([`super::kubernetes_lists`]),
    /// narrowed by the selectors the request carried.
    pub fn event_list_json(
        &self,
        namespace: &str,
        request: &ListRequest,
    ) -> Result<::serde_json::Value, ApiRefusal> {
        let names: Vec<String> = self.namespace(namespace)?.events.keys().cloned().collect();
        Ok(listed(EVENT_LIST_KIND, &names, request, |name| {
            self.event_json(namespace, name).ok()
        }))
    }
}

/// The fields an event may be SELECTED by, as `(the field label a client writes,
/// the member of the event document it names)`.
///
/// They are the fields the Kubernetes API server's own event registry indexes: the
/// object an event is about, the reason and the type it carries, the component
/// that reported it, and its own name and namespace — `source` being the label
/// that registry serves the reporting component under. A `fieldSelector` naming
/// anything else is refused the way the API server refuses a field label it does
/// not index ([`ListRequest::fields_limited_to`]).
pub const EVENT_FIELD_SELECTORS: &[(&str, &str)] = &[
    ("involvedObject.apiVersion", "involvedObject.apiVersion"),
    ("involvedObject.fieldPath", "involvedObject.fieldPath"),
    ("involvedObject.kind", "involvedObject.kind"),
    ("involvedObject.name", "involvedObject.name"),
    ("involvedObject.namespace", "involvedObject.namespace"),
    ("involvedObject.uid", "involvedObject.uid"),
    ("metadata.name", "metadata.name"),
    ("metadata.namespace", "metadata.namespace"),
    ("reason", "reason"),
    ("reportingComponent", "reportingComponent"),
    ("source", "source.component"),
    ("type", "type"),
];
