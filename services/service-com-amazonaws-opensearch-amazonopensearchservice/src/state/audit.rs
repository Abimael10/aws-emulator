// SPDX-License-Identifier: CC0-1.0

//! The security plugin's AUDIT CONFIGURATION on a domain of this service: which of the
//! decisions the cluster takes about a request are RECORDED, and what each record says.
//!
//! `LogType`'s `AUDIT_LOGS` is the log type whose "audit logs contain records of user
//! requests for access to the domain", and a domain publishing it delivers those records
//! into the CloudWatch Logs group its option names ([`super::log_publishing`]). WHICH
//! requests leave a record is not the control plane's to say: it is the security plugin's
//! own configuration, read and written through the plugin's REST API on the endpoint the
//! domain published ([`crate::service::domain_rest`]) — so a domain that has only named the
//! group records nothing until that configuration says so.
//!
//! # The configuration
//!
//! One document per domain, in the shape the plugin's API reads and writes it: `enabled` for
//! the whole of auditing, an `audit` section saying which LAYER is audited and which
//! CATEGORIES are not (the plugin states the exclusions, never the inclusions), and a
//! `compliance` section beside it. It is held as that document rather than as fields of its
//! own because the write is an RFC 6902 JSON Patch over it: what a patch may name is every
//! path the document has, so the document IS the state.
//!
//! Auditing starts switched OFF: enabling it is the step a client takes through this API
//! once the domain's `AUDIT_LOGS` option names a log group, and the categories the plugin's
//! own defaults exclude are exactly the ones such a client then clears.
//!
//! # What a decision leaves
//!
//! The decisions this emulator's data plane really takes, one record each, at the layer the
//! plugin states them at: the cluster's AUTHENTICATION of a request, at the REST layer
//! ([`CATEGORY_AUTHENTICATED`], [`CATEGORY_FAILED_LOGIN`]), and its AUTHORIZATION of the
//! action that request asks for, at the transport layer ([`CATEGORY_GRANTED_PRIVILEGES`],
//! [`CATEGORY_MISSING_PRIVILEGES`]). A record states the decision and nothing else — no
//! response status, because the decision is taken before the request is answered — so two
//! requests the cluster decided alike leave the same record whatever each was answered.

use ::std::time::SystemTime;

use ::serde_json::{json, Map, Value};
use ::service_common::iam_authorization::wildcard_match;

use super::{fgac, Domain};

/// `LogType`'s `AUDIT_LOGS` — the log of the requests for access to the domain.
pub const LOG_TYPE_AUDIT: &str = "AUDIT_LOGS";

/// The category of a request the cluster ADMITTED, stated at the REST layer.
pub const CATEGORY_AUTHENTICATED: &str = "AUTHENTICATED";

/// The category of a credential it refused, stated at the same layer.
pub const CATEGORY_FAILED_LOGIN: &str = "FAILED_LOGIN";

/// The category of an action the cluster PERMITTED the caller, stated at the transport
/// layer.
pub const CATEGORY_GRANTED_PRIVILEGES: &str = "GRANTED_PRIVILEGES";

/// The category of one it did not permit, stated at the same layer.
pub const CATEGORY_MISSING_PRIVILEGES: &str = "MISSING_PRIVILEGES";

/// The format version the plugin stamps every audit record with, which is what tells a
/// reader of the group how to read the fields beside it.
const AUDIT_FORMAT_VERSION: i64 = 4;

/// The section of the configuration that decides what is audited.
const AUDIT_SECTION: &str = "audit";

/// The member the whole configuration sits under in the API's own envelope, and so the
/// first token of every JSON Patch path a write states.
const CONFIG: &str = "config";

/// The member switching the whole of auditing on.
const ENABLED: &str = "enabled";

/// The member switching the REST layer on, and the one switching the transport layer on.
const ENABLE_REST: &str = "enable_rest";
const ENABLE_TRANSPORT: &str = "enable_transport";

/// The members listing the categories NOT recorded at each layer.
const DISABLED_REST_CATEGORIES: &str = "disabled_rest_categories";
const DISABLED_TRANSPORT_CATEGORIES: &str = "disabled_transport_categories";

/// The member listing the users whose requests leave no record.
const IGNORE_USERS: &str = "ignore_users";

/// The member listing the request paths that leave none.
const IGNORE_REQUESTS: &str = "ignore_requests";

/// The paths of the configuration the plugin does not let its API write, as it reports
/// them: pointers into the document below `config`, which is the form the `_readonly` array
/// of a read carries. A write is judged by the configuration it would LEAVE at each of them
/// ([`AuditConfig::restates_readonly`]) rather than by the paths it names.
pub const READONLY_PATHS: [&str; 3] = [
    "/audit/exclude_sensitive_headers",
    "/compliance/internal_config",
    "/compliance/external_config",
];

/// The most operations one patch of the configuration is applied from.
///
/// The emulator's OWN bound, like [`crate::service::domain_rest`]'s bound on a request
/// body: every operation is applied to a document in this process's heap, so a patch past
/// this is refused rather than applied. The configuration has fewer paths than this, so no
/// patch a client writes to change it reaches the bound.
const MAX_PATCH_OPERATIONS: usize = 64;

/// The most values one audit configuration holds, and the deepest they nest.
///
/// The emulator's OWN bounds, for the same reason the operation count is one: a patch may
/// `copy` a whole subtree into the document, which doubles what the next operation copies,
/// so a document measured only at the end could have grown by a factor per operation on the
/// way there. The configuration this resource holds has a few dozen values nested three
/// deep, so no write a client makes to change it reaches either bound.
const MAX_CONFIGURATION_VALUES: usize = 4_096;
const MAX_CONFIGURATION_DEPTH: usize = 32;

/// The LAYER of the cluster a decision was taken at — which is what decides whether it is
/// audited at all, and which fields its record carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layer {
    /// The REST layer, where the cluster decides WHO a request is.
    Rest,
    /// The transport layer, where it decides what that identity may DO.
    Transport,
}

impl Layer {
    /// The layer's name, as a record states it.
    fn stated(self) -> &'static str {
        match self {
            Self::Rest => "REST",
            Self::Transport => "TRANSPORT",
        }
    }

    /// The member of the `audit` section that switches this layer on.
    fn enable_member(self) -> &'static str {
        match self {
            Self::Rest => ENABLE_REST,
            Self::Transport => ENABLE_TRANSPORT,
        }
    }

    /// The member listing the categories this layer does NOT record.
    fn disabled_categories_member(self) -> &'static str {
        match self {
            Self::Rest => DISABLED_REST_CATEGORIES,
            Self::Transport => DISABLED_TRANSPORT_CATEGORIES,
        }
    }
}

/// ONE decision the cluster took about one request — what the audit log may have a record
/// of.
///
/// Built by the plane that took the decision ([`crate::service::domain_rest`]), out of the
/// request and the decision itself; whether it is recorded at all ([`records`]) and what
/// the record says ([`message`]) are this module's.
#[derive(Debug, Clone)]
pub struct Event<'a> {
    /// The plugin's category for the decision — one of the `CATEGORY_*` constants above.
    pub category: &'a str,
    /// The layer it was taken at.
    pub layer: Layer,
    /// The identity it was taken for, as the cluster knows it.
    pub effective_user: &'a str,
    /// The path of the REST request that led to it, as the caller addressed it.
    pub rest_path: &'a str,
    /// That request's method.
    pub rest_method: &'a str,
    /// The cluster action the decision was about, for a decision of the transport layer.
    pub privilege: Option<&'a str>,
    /// The index that action was about, where it was about one.
    pub index: Option<&'a str>,
}

/// The audit configuration one domain holds, as the plugin's API reads and writes it.
#[derive(Debug, Clone)]
pub struct AuditConfig {
    /// The `config` document itself. Private, so every read of it goes through a method
    /// here and no caller can read a member under a name the document does not use.
    config: Value,
}

impl Default for AuditConfig {
    fn default() -> Self {
        Self {
            config: plugin_defaults(),
        }
    }
}

impl AuditConfig {
    /// The document a read of the resource answers: the paths its API will not write, and
    /// the configuration itself.
    #[must_use]
    pub fn reported(&self) -> Value {
        json!({ "_readonly": READONLY_PATHS, CONFIG: self.config.clone() })
    }

    /// Apply the JSON Patch `operations` to the configuration, leaving it unchanged on a
    /// refusal.
    ///
    /// The patch is applied to a document whose root carries the configuration under
    /// `config`, which is what every path of such a patch is written against
    /// (`/config/audit/enable_rest`). It is applied to a COPY, so a patch that fails
    /// part-way leaves the stored configuration as it was, and ONE OPERATION AT A TIME,
    /// holding the document to its bounds after each — which is where an operation that
    /// duplicates a subtree is stopped ([`MAX_CONFIGURATION_VALUES`]).
    pub fn patched(&mut self, operations: &Value) -> Result<(), Refusal> {
        let patch: ::json_patch::Patch =
            ::serde_json::from_value(operations.clone()).map_err(|_| Refusal::Unreadable)?;
        if patch.0.len() > MAX_PATCH_OPERATIONS {
            return Err(Refusal::TooManyOperations);
        }
        let mut document = json!({ CONFIG: self.config.clone() });
        for operation in &patch.0 {
            let step = ::json_patch::Patch(vec![operation.clone()]);
            ::json_patch::patch(&mut document, &step).map_err(|_| Refusal::Unreadable)?;
            if !within_bounds(&document) {
                return Err(Refusal::TooLarge);
            }
        }
        // Only the WHOLE patch has to leave a configuration: an intermediate value of any
        // shape is RFC 6902's business, and a later operation may put the object back.
        let Some(Value::Object(patched)) = document.get(CONFIG) else {
            return Err(Refusal::Unreadable);
        };
        let patched = Value::Object(patched.clone());
        if self.restates_readonly(&patched) {
            return Err(Refusal::ReadOnly);
        }
        self.config = patched;
        Ok(())
    }

    /// Replace the whole configuration with `config` — the write that states the document
    /// rather than the change to it.
    pub fn replaced(&mut self, config: &Value) -> Result<(), Refusal> {
        if !config.is_object() {
            return Err(Refusal::Unreadable);
        }
        if !within_bounds(config) {
            return Err(Refusal::TooLarge);
        }
        if self.restates_readonly(config) {
            return Err(Refusal::ReadOnly);
        }
        self.config = config.clone();
        Ok(())
    }

    /// Whether `config` would leave one of the read-only paths other than the way it stands.
    ///
    /// Judged on the CONFIGURATION the write would leave rather than on the paths the write
    /// names: an operation naming an object ABOVE a read-only path changes it just as surely
    /// as one naming the path itself, and a write that drops it changes it too. The one
    /// check both writes apply, so which paths the API writes cannot depend on which of its
    /// two writes a client used.
    fn restates_readonly(&self, config: &Value) -> bool {
        READONLY_PATHS
            .iter()
            .any(|path| config.pointer(path) != self.config.pointer(path))
    }

    /// Whether the whole of auditing is switched on.
    fn enabled(&self) -> bool {
        self.config
            .get(ENABLED)
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }

    /// Whether the `audit` section's flag `name` is set.
    fn flag(&self, name: &str) -> bool {
        self.config
            .get(AUDIT_SECTION)
            .and_then(|section| section.get(name))
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }

    /// The strings the `audit` section's list `name` holds, empty for a member it does not
    /// hold or holds something other than a list of strings.
    fn list(&self, name: &str) -> Vec<&str> {
        self.config
            .get(AUDIT_SECTION)
            .and_then(|section| section.get(name))
            .and_then(Value::as_array)
            .map(|values| values.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default()
    }
}

/// Why a write of the audit configuration was not taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// The body is not a document this resource's write is stated in.
    Unreadable,
    /// It would leave one of the read-only paths other than the way it stands.
    ReadOnly,
    /// It carries more operations than [`MAX_PATCH_OPERATIONS`].
    TooManyOperations,
    /// It would leave a configuration past [`MAX_CONFIGURATION_VALUES`] or
    /// [`MAX_CONFIGURATION_DEPTH`].
    TooLarge,
}

impl Refusal {
    /// What the security API says about this refusal.
    #[must_use]
    pub fn message(self) -> String {
        match self {
            Self::Unreadable => "Could not parse content of request.".to_string(),
            Self::ReadOnly => "Attempted to update read-only property.".to_string(),
            Self::TooManyOperations => format!(
                "a JSON patch of more than {MAX_PATCH_OPERATIONS} operations is not applied"
            ),
            Self::TooLarge => format!(
                "an audit configuration of more than {MAX_CONFIGURATION_VALUES} values, or \
                 nested more than {MAX_CONFIGURATION_DEPTH} deep, is not stored"
            ),
        }
    }
}

/// Whether `domain`'s audit configuration has a record of `event`.
///
/// Every condition the plugin's own configuration states, in the order it states them: the
/// whole switch, the layer's switch, the categories that layer does not record, the users
/// whose requests are ignored and the paths that are. The audit log is the SECURITY
/// plugin's, so a domain without fine-grained access control keeps none at all.
#[must_use]
pub fn records(domain: &Domain, event: &Event<'_>) -> bool {
    if !fgac::enforced(domain) {
        return false;
    }
    let config = &domain.audit_config;
    config.enabled()
        && config.flag(event.layer.enable_member())
        && !config
            .list(event.layer.disabled_categories_member())
            .iter()
            .any(|disabled| disabled.eq_ignore_ascii_case(event.category))
        && !config
            .list(IGNORE_USERS)
            .iter()
            .any(|ignored| wildcard_match(ignored, event.effective_user, true))
        && !config
            .list(IGNORE_REQUESTS)
            .iter()
            .any(|ignored| wildcard_match(ignored, event.rest_path, true))
}

/// The record `event` leaves, as one line of the audit log: the plugin's own audit
/// document, whose every field is read from the decision, the request it was taken about
/// and the cluster that took it.
#[must_use]
pub fn message(domain: &Domain, event: &Event<'_>, node: &str, at: SystemTime) -> String {
    let mut record = Map::new();
    record.insert(
        "audit_format_version".to_string(),
        json!(AUDIT_FORMAT_VERSION),
    );
    record.insert(
        "@timestamp".to_string(),
        json!(super::log_publishing::stated_time(at)),
    );
    record.insert("audit_category".to_string(), json!(event.category));
    record.insert(
        "audit_request_layer".to_string(),
        json!(event.layer.stated()),
    );
    // Every request this plane serves arrived over the REST API, which is where the
    // transport action it asks for ORIGINATES as well.
    record.insert(
        "audit_request_origin".to_string(),
        json!(Layer::Rest.stated()),
    );
    record.insert(
        "audit_cluster_name".to_string(),
        json!(super::cluster_name(domain)),
    );
    record.insert("audit_node_id".to_string(), json!(node));
    record.insert("audit_node_name".to_string(), json!(node));
    record.insert(
        "audit_request_effective_user".to_string(),
        json!(event.effective_user),
    );
    match event.layer {
        // A decision of the REST layer is about the request itself, so its record names the
        // path and the method the caller sent.
        Layer::Rest => {
            record.insert(
                "audit_rest_request_path".to_string(),
                json!(event.rest_path),
            );
            record.insert(
                "audit_rest_request_method".to_string(),
                json!(event.rest_method),
            );
        }
        // One of the transport layer is about an ACTION, so its record names the privilege
        // that action asks for and the indexes it resolved to.
        Layer::Transport => {
            if let Some(privilege) = event.privilege {
                record.insert("audit_request_privilege".to_string(), json!(privilege));
            }
            if let Some(index) = event.index {
                record.insert("audit_trace_indices".to_string(), json!([index]));
                record.insert("audit_trace_resolved_indices".to_string(), json!([index]));
            }
        }
    }
    Value::Object(record).to_string()
}

/// The configuration a domain holds before anything writes one: the plugin's own defaults,
/// with auditing itself switched off.
///
/// The plugin ships this document — both layers on, the two categories every request would
/// otherwise produce excluded, the Dashboards service account ignored — and the whole switch
/// is what a client turns on once the domain publishes the log type, which is why it starts
/// off here.
fn plugin_defaults() -> Value {
    json!({
        ENABLED: false,
        AUDIT_SECTION: {
            ENABLE_REST: true,
            DISABLED_REST_CATEGORIES: [CATEGORY_AUTHENTICATED, CATEGORY_GRANTED_PRIVILEGES],
            ENABLE_TRANSPORT: true,
            DISABLED_TRANSPORT_CATEGORIES: [
                CATEGORY_AUTHENTICATED,
                CATEGORY_GRANTED_PRIVILEGES,
            ],
            "resolve_bulk_requests": false,
            "log_request_body": true,
            "resolve_indices": true,
            "exclude_sensitive_headers": true,
            IGNORE_USERS: ["kibanaserver"],
            IGNORE_REQUESTS: [],
        },
        "compliance": {
            ENABLED: true,
            "write_log_diffs": false,
            "read_watched_fields": {},
            "read_ignore_users": ["kibanaserver"],
            "write_watched_indices": [],
            "write_ignore_users": ["kibanaserver"],
            "read_metadata_only": true,
            "write_metadata_only": true,
            "external_config": false,
            "internal_config": true,
        },
    })
}

/// Whether `document` holds no more than [`MAX_CONFIGURATION_VALUES`] values, nested no
/// deeper than [`MAX_CONFIGURATION_DEPTH`].
///
/// Walked ITERATIVELY, with an explicit stack of the values still to look at, so measuring
/// a document deeper than this resource will hold cannot itself exhaust the thread's stack
/// — which a recursive walk of exactly such a document is how it would. It stops at the
/// first value past either bound, so the work is bounded by the bounds themselves.
fn within_bounds(document: &Value) -> bool {
    let mut pending = vec![(document, 1_usize)];
    let mut values = 0_usize;
    while let Some((value, depth)) = pending.pop() {
        values += 1;
        if values > MAX_CONFIGURATION_VALUES || depth > MAX_CONFIGURATION_DEPTH {
            return false;
        }
        match value {
            Value::Array(items) => pending.extend(items.iter().map(|item| (item, depth + 1))),
            Value::Object(members) => {
                pending.extend(members.values().map(|member| (member, depth + 1)));
            }
            _ => {}
        }
    }
    true
}
