// SPDX-License-Identifier: CC0-1.0

//! How a collection of a cluster's Kubernetes API answers a LIST.
//!
//! A list is answered in CHUNKS. The Kubernetes API's list contract is that a
//! client may ask for a chunk size with `limit`, that "the server may choose to
//! return fewer items than the client asked for", and that a list the server did
//! not finish carries a `continue` token in its `metadata` for the client to
//! resume with, beside the `remainingItemCount` of what it has not sent yet. A
//! client that asks for no chunk size is answered the collection as far as the
//! server chose to send it, with the same token when there is more.
//!
//! Chunking is also what BOUNDS this address. A namespace holds up to
//! [`super::kubernetes_objects::MAX_PODS_PER_NAMESPACE`] objects and each of them
//! is as large as the client that created it made it, so an answer that
//! serialised every one of them would let one small `GET` build hundreds of
//! megabytes in memory while this service's store is held. One answer therefore
//! stops at [`MAX_LIST_RESPONSE_BYTES`] and hands back a token instead.
//!
//! A list also answers WHICH objects the client asked for: `labelSelector` is "a
//! selector to restrict the list of returned objects by their labels; defaults to
//! everything", so an object whose labels do not satisfy every requirement the
//! selector states is not in the answer. It is read here, once, for every
//! collection this server lists — the nodes a client asks for by node group among
//! them — and a selector this server cannot read (the set-based half of the
//! grammar, a key or a value outside the label syntax) is REFUSED the way the API
//! server refuses an unparsable one, never answered with an empty list.
//!
//! `fieldSelector` narrows the same answer by the objects' OWN members — "field
//! selectors let you select Kubernetes objects based on the value of one or more
//! resource fields" — which is how a client asks for the events about ONE pod. It
//! is read here as well, and WHICH fields a collection may be selected by is that
//! collection's own answer ([`ListRequest::fields_limited_to`]), because the API
//! server serves a field label per resource and refuses the ones it does not
//! index.

use super::kubernetes_api::MAX_REQUEST_BYTES;
use super::kubernetes_objects::{ApiRefusal, CORE_API_VERSION};

/// The most bytes of objects ONE list answer carries, after which the answer
/// stops and reports a `continue` token.
///
/// It is the ceiling the API server already applies to one request
/// ([`MAX_REQUEST_BYTES`]), so no answer this address writes is larger than the
/// largest body it reads, whatever a namespace holds.
pub const MAX_LIST_RESPONSE_BYTES: usize = MAX_REQUEST_BYTES;

/// How many requirements ONE `labelSelector` is read with.
///
/// CHOSEN, not published: the Kubernetes API puts no limit on how many
/// requirements a selector states, and each one is matched against every listed
/// object, so what one list costs must not grow with what a caller wrote in the
/// query. It is far above the handful a selector states — a selector past it is
/// refused rather than read in part, since dropping a requirement would ADMIT
/// objects the client asked to leave out.
pub const MAX_LABEL_REQUIREMENTS: usize = 32;

/// ONE requirement of a `labelSelector`: what an object's labels must say for it
/// to be in the answer.
///
/// The EQUALITY-BASED forms the Kubernetes label selector grammar defines —
/// `key=value` and its `key==value` spelling, `key!=value`, the bare `key` that
/// requires the label to be set, and `!key` that requires it not to be. A
/// requirement in the set-based half of that grammar (`key in (a, b)`,
/// `key notin (a, b)`), and one naming a key or a value outside the label syntax,
/// are REFUSED rather than read as something else: a selector this server cannot
/// read is answered the way the API server answers an unparsable one, so a client
/// is told its selector was rejected instead of being told the collection holds
/// nothing matching it.
#[derive(Debug)]
enum LabelRequirement {
    Equals(String, String),
    NotEquals(String, String),
    Exists(String),
    NotExists(String),
}

/// The longest a label key's name part and a label value may be, which is the
/// length Kubernetes holds both to.
const MAX_LABEL_SEGMENT_LEN: usize = 63;

/// The longest the optional `<prefix>/` part of a label key may be: a DNS
/// subdomain.
const MAX_LABEL_PREFIX_LEN: usize = 253;

/// Whether `candidate` has the form of a label key's NAME part, and of a label
/// VALUE, which Kubernetes holds to the same syntax: at most
/// [`MAX_LABEL_SEGMENT_LEN`] alphanumerics, `-`, `_` and `.`, beginning and
/// ending alphanumeric.
fn is_label_segment(candidate: &str) -> bool {
    let bytes = candidate.as_bytes();
    let is_alphanumeric = |byte: u8| byte.is_ascii_alphanumeric();
    !bytes.is_empty()
        && bytes.len() <= MAX_LABEL_SEGMENT_LEN
        && is_alphanumeric(bytes[0])
        && bytes[bytes.len() - 1].is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|byte| is_alphanumeric(*byte) || matches!(byte, b'-' | b'_' | b'.'))
}

/// Whether `candidate` has the form of a label KEY: a name part, optionally
/// prefixed by a DNS subdomain and a `/` ("eks.amazonaws.com/nodegroup").
fn is_label_key(candidate: &str) -> bool {
    match candidate.split_once('/') {
        Some((prefix, name)) => {
            !prefix.is_empty()
                && prefix.len() <= MAX_LABEL_PREFIX_LEN
                && prefix.split('.').all(is_label_segment)
                && is_label_segment(name)
        }
        None => is_label_segment(candidate),
    }
}

/// Whether `candidate` has the form of a label VALUE: a name part, or nothing —
/// "a valid label must be an empty string or consist of alphanumeric characters".
fn is_label_value(candidate: &str) -> bool {
    candidate.is_empty() || is_label_segment(candidate)
}

impl LabelRequirement {
    /// The requirement `term` states, `None` for a term that states none (an
    /// empty one, which is what a trailing comma leaves), or the detail of the
    /// refusal a term this server cannot read as a requirement earns.
    fn parse(term: &str) -> Result<Option<Self>, String> {
        let term = term.trim();
        if term.is_empty() {
            return Ok(None);
        }
        if let Some(key) = term.strip_prefix('!') {
            let key = key.trim();
            check(key, None)?;
            return Ok(Some(Self::NotExists(key.to_string())));
        }
        if let Some((key, value)) = term.split_once("!=") {
            let (key, value) = (key.trim(), value.trim());
            check(key, Some(value))?;
            return Ok(Some(Self::NotEquals(key.to_string(), value.to_string())));
        }
        match term.split_once('=') {
            // `key==value` is the same requirement as `key=value`, so a SECOND `=`
            // belongs to the operator rather than to the value — and a third does
            // not: `key===value` is no requirement the grammar defines, so it is
            // read as a value outside the label syntax and refused.
            Some((key, value)) => {
                let (key, value) = (key.trim(), value.strip_prefix('=').unwrap_or(value).trim());
                check(key, Some(value))?;
                Ok(Some(Self::Equals(key.to_string(), value.to_string())))
            }
            None => {
                check(term, None)?;
                Ok(Some(Self::Exists(term.to_string())))
            }
        }
    }

    /// Whether `labels` satisfies this requirement.
    fn admits(&self, labels: Option<&::serde_json::Map<String, ::serde_json::Value>>) -> bool {
        let held = |key: &str| {
            labels
                .and_then(|labels| labels.get(key))
                .and_then(::serde_json::Value::as_str)
        };
        match self {
            Self::Equals(key, value) => held(key) == Some(value.as_str()),
            Self::NotEquals(key, value) => held(key) != Some(value.as_str()),
            Self::Exists(key) => held(key).is_some(),
            Self::NotExists(key) => held(key).is_none(),
        }
    }
}

/// `Ok(())` when `key` and `value` are a label key and value this server can read
/// a requirement over, else the detail of the refusal — in the wording
/// apimachinery words an invalid requirement in.
///
/// This is also what refuses the set-based half of the grammar: `key in (a, b)`
/// carries its operator and values inside what would have to be the key, and no
/// label key has that form.
fn check(key: &str, value: Option<&str>) -> Result<(), String> {
    if !is_label_key(key) {
        return Err(format!(
            "key: Invalid value: \"{key}\": name part must consist of alphanumeric \
             characters, '-', '_' or '.', and must start and end with an alphanumeric \
             character"
        ));
    }
    match value {
        Some(value) if !is_label_value(value) => Err(format!(
            "values[0][{key}]: Invalid value: \"{value}\": a valid label must be an empty \
             string or consist of alphanumeric characters, '-', '_' or '.', and must start \
             and end with an alphanumeric character"
        )),
        _ => Ok(()),
    }
}

/// ONE requirement of a `fieldSelector`: what a FIELD of the object itself must
/// say for it to be in the answer.
///
/// "Field selectors let you select Kubernetes objects based on the value of one or
/// more resource fields", with the `=`/`==` and `!=` operators the field selector
/// grammar defines and a `,` between requirements — so a requirement names a
/// field by the dotted path of the member it is ([`field_value`]) and the value
/// that member must carry.
#[derive(Debug)]
enum FieldRequirement {
    Equals(String, String),
    NotEquals(String, String),
}

impl FieldRequirement {
    /// The requirement `term` states, and `None` for a term that states none (an
    /// empty one, which is what a trailing comma leaves).
    fn parse(term: &str) -> Option<Self> {
        let term = term.trim();
        if term.is_empty() {
            return None;
        }
        if let Some((field, value)) = term.split_once("!=") {
            return Some(Self::NotEquals(
                field.trim().to_string(),
                value.trim().to_string(),
            ));
        }
        let (field, value) = term.split_once('=')?;
        Some(Self::Equals(
            field.trim().to_string(),
            value.strip_prefix('=').unwrap_or(value).trim().to_string(),
        ))
    }

    /// The field this requirement is about.
    fn field(&self) -> &str {
        match self {
            Self::Equals(field, _) | Self::NotEquals(field, _) => field,
        }
    }

    /// Rename the field this requirement is about to the member of the object it
    /// names, for a field label the resource serves under another path.
    fn addressing(&mut self, member: &str) {
        match self {
            Self::Equals(field, _) | Self::NotEquals(field, _) => *field = member.to_string(),
        }
    }

    /// Whether `item` satisfies this requirement. A field the object does not
    /// carry reads as the empty value, which is how the API server compares an
    /// unset field.
    fn admits(&self, item: &::serde_json::Value) -> bool {
        match self {
            Self::Equals(field, value) => field_value(item, field) == value.as_str(),
            Self::NotEquals(field, value) => field_value(item, field) != value.as_str(),
        }
    }
}

/// The value the object `item` carries at the dotted path `field`, and the empty
/// string for a path it does not carry a scalar at — which is how the API server
/// compares a field an object has not set.
fn field_value<'item>(item: &'item ::serde_json::Value, field: &str) -> &'item str {
    let mut held = item;
    for segment in field.split('.') {
        match held.get(segment) {
            Some(next) => held = next,
            None => return "",
        }
    }
    held.as_str().unwrap_or_default()
}

/// What a list request asked for: the chunk size, where to resume, and which
/// objects' labels it asked for.
#[derive(Debug, Default)]
pub struct ListRequest {
    /// `limit`: "the maximum number of responses to return for a list call".
    /// `None` when the request named none, and for `limit=0`, which is how the
    /// parameter's own default states "no limit".
    limit: Option<usize>,
    /// The `continue` token the client sent back: the name to resume AFTER.
    resume_after: Option<String>,
    /// `labelSelector`: "a selector to restrict the list of returned objects by
    /// their labels; defaults to everything" — the requirements EVERY object in
    /// the answer satisfies. Empty for a request that named none, which is that
    /// default.
    selector: Vec<LabelRequirement>,
    /// `fieldSelector`: "a selector to restrict the list of returned objects by
    /// their fields; defaults to everything" — the requirements over the objects'
    /// OWN members that every object in the answer satisfies.
    fields: Vec<FieldRequirement>,
}

impl ListRequest {
    /// The list parameters `query` carries, or the `Status` the API server
    /// refuses a `limit` that is not a number with.
    ///
    /// The tokens this server mints are object names — lowercase RFC 1123
    /// subdomains ([`is_kubernetes_name`]) — so a client echoes one back
    /// unchanged and there is nothing to decode. A parameter this server does
    /// not serve is ignored, as the API server ignores one it does not know.
    pub fn from_query(query: Option<&str>) -> Result<Self, ApiRefusal> {
        let mut parsed = Self::default();
        for parameter in query.unwrap_or_default().split('&') {
            let (name, value) = match parameter.split_once('=') {
                Some(pair) => pair,
                None => continue,
            };
            match name {
                "limit" => {
                    // An empty value is the unset parameter, as is "0": the
                    // conversion from the query string's text to the parameter's
                    // own integer type reads both as "no limit".
                    if value.is_empty() {
                        continue;
                    }
                    let limit: usize = value.parse().map_err(|_| ApiRefusal {
                        status: 400,
                        reason: "BadRequest",
                        message: format!("unable to parse query parameter \"limit\": {value}"),
                        resource: None,
                    })?;
                    parsed.limit = (limit > 0).then_some(limit);
                }
                // An empty token is the unset value of the parameter, as a client
                // that has not resumed anything sends.
                "continue" if !value.is_empty() => {
                    parsed.resume_after = Some(value.to_string());
                }
                // A selector is written with characters a query string escapes
                // (`=` between a key and its value, `/` inside a key), so the
                // value is read through the one query decoder of this workspace
                // before it is parsed. A selector this server cannot read is
                // REFUSED, in the API server's own wording, rather than matched
                // by nothing.
                "labelSelector" if !value.is_empty() => {
                    let selector = ::smithy_server_runtime::percent_decode(value);
                    let mut requirements = Vec::new();
                    for term in selector.split(',') {
                        // The bound is applied as the terms are read rather than
                        // after: a selector past it is refused without the rest of
                        // it being parsed or held.
                        if requirements.len() >= MAX_LABEL_REQUIREMENTS {
                            return Err(unparsable_selector(&format!(
                                "unable to parse requirement: at most \
                                 {MAX_LABEL_REQUIREMENTS} requirements are read"
                            )));
                        }
                        match LabelRequirement::parse(term) {
                            Ok(Some(requirement)) => requirements.push(requirement),
                            Ok(None) => {}
                            Err(detail) => {
                                return Err(unparsable_selector(&format!(
                                    "unable to parse requirement: {detail}"
                                )))
                            }
                        }
                    }
                    parsed.selector = requirements;
                }
                // A field selector is written with the same escaped characters a
                // label selector is, so it is read through the same decoder. Its
                // grammar has no form this server cannot read — only `=`, `==`
                // and `!=` — and WHICH fields a resource may be selected by is
                // the resource's own answer ([`ListRequest::fields_limited_to`]).
                "fieldSelector" if !value.is_empty() => {
                    let selector = ::smithy_server_runtime::percent_decode(value);
                    let mut requirements = Vec::new();
                    for term in selector.split(',') {
                        // The same bound the label selector is read under: what
                        // one list costs must not grow with what a caller wrote
                        // in the query.
                        if requirements.len() >= MAX_LABEL_REQUIREMENTS {
                            return Err(unparsable_selector(&format!(
                                "unable to parse requirement: at most \
                                 {MAX_LABEL_REQUIREMENTS} requirements are read"
                            )));
                        }
                        if let Some(requirement) = FieldRequirement::parse(term) {
                            requirements.push(requirement);
                        }
                    }
                    parsed.fields = requirements;
                }
                _ => {}
            }
        }
        Ok(parsed)
    }

    /// Whether `item` is an object this request asked for: one whose
    /// `metadata.labels` satisfies every requirement of its selector, and whose
    /// own fields satisfy every requirement of its field selector. True for a
    /// request that named neither, which asked for everything.
    fn admits(&self, item: &::serde_json::Value) -> bool {
        let labels = item
            .get("metadata")
            .and_then(|metadata| metadata.get("labels"))
            .and_then(::serde_json::Value::as_object);
        self.selector
            .iter()
            .all(|requirement| requirement.admits(labels))
            && self
                .fields
                .iter()
                .all(|requirement| requirement.admits(item))
    }

    /// Hold this request's `fieldSelector` to the fields the resource being listed
    /// is selectable by: each pair is `(the field label, the member of the object
    /// it names)`, which are the same path for every field a resource serves under
    /// its own name and differ for the one it serves under another.
    ///
    /// A requirement naming a field the resource does not serve is REFUSED in the
    /// API server's own wording, rather than answered with an empty list: a client
    /// that selected on a field this server cannot index is told so.
    pub fn fields_limited_to(&mut self, served: &[(&str, &str)]) -> Result<(), ApiRefusal> {
        for requirement in &mut self.fields {
            let member = served
                .iter()
                .find(|(label, _)| *label == requirement.field())
                .map(|(_, member)| *member)
                .ok_or_else(|| ApiRefusal {
                    status: 400,
                    reason: "BadRequest",
                    message: format!("field label not supported: {}", requirement.field()),
                    resource: None,
                })?;
            requirement.addressing(member);
        }
        Ok(())
    }
}

/// The `Status` the API server refuses a `labelSelector` it cannot read with:
/// `400 BadRequest`, carrying what could not be read.
fn unparsable_selector(message: &str) -> ApiRefusal {
    ApiRefusal {
        status: 400,
        reason: "BadRequest",
        message: message.to_string(),
        resource: None,
    }
}

/// One chunk of the collection `names` names, as the `<Kind>List` a client reads.
///
/// `names` is the collection in the order it is held, which is the order a
/// resumed read continues in; `render` answers the object document for one name,
/// and a name it cannot render is not in the answer. An object the request's
/// `labelSelector` does not ask for is not in it either — the selector decides
/// which objects the answer CARRIES, so the client's `limit` is a bound on the
/// objects it asked for. The chunk ends at that `limit`, at
/// [`MAX_LIST_RESPONSE_BYTES`], or at the end of the collection — and the first
/// object of a chunk is always carried, so a collection holding one object larger
/// than the budget can still be walked.
pub fn listed(
    kind: &str,
    names: &[String],
    request: &ListRequest,
    render: impl Fn(&str) -> Option<::serde_json::Value>,
) -> ::serde_json::Value {
    let start = match request.resume_after.as_deref() {
        Some(after) => names.partition_point(|name| name.as_str() <= after),
        None => 0,
    };
    let remaining = names.get(start..).unwrap_or_default();
    let mut items: Vec<::serde_json::Value> = Vec::new();
    let mut bytes = 0usize;
    let mut sent = 0usize;
    for name in remaining {
        if request.limit.is_some_and(|limit| items.len() >= limit) {
            break;
        }
        if !items.is_empty() && bytes >= MAX_LIST_RESPONSE_BYTES {
            break;
        }
        sent = sent.saturating_add(1);
        let Some(item) = render(name) else {
            continue;
        };
        if !request.admits(&item) {
            continue;
        }
        bytes = bytes.saturating_add(item.to_string().len());
        items.push(item);
    }
    let unsent = remaining.len().saturating_sub(sent);
    let mut metadata = ::serde_json::Map::new();
    if unsent > 0 {
        // The token names the last object this answer carried, which is where the
        // next chunk resumes after.
        if let Some(last) = remaining.get(sent.saturating_sub(1)) {
            metadata.insert("continue".to_string(), ::serde_json::json!(last));
        }
        metadata.insert(
            "remainingItemCount".to_string(),
            ::serde_json::json!(unsent),
        );
    }
    ::serde_json::json!({
        "kind": kind,
        "apiVersion": CORE_API_VERSION,
        "metadata": ::serde_json::Value::Object(metadata),
        "items": items,
    })
}
