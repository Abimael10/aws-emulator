// SPDX-License-Identifier: CC0-1.0

//! How one integration call's REQUEST is rendered: the document the invoked operation
//! is handed, the HTTP request a REST protocol binds it to, and the form fields a
//! FORM protocol spreads it over.
//!
//! One concern, split out of [`super`] so that what a call IS (the APIs, the
//! integrations a `Resource` names, the seam a walk performs one through) is read apart
//! from how its request goes on the wire. The names stay reachable unqualified from
//! [`super`] and from the rest of this module's tree, as they were when they lived in
//! that file.

use super::*;

/// The document `request` goes on the wire as: every TOP-LEVEL member under the
/// spelling the invoked service's model uses for it ([`MemberCase`]), and every
/// member the integration documents as a document the operation types as a string
/// carried as that document's JSON text ([`Call::json_text`]).
///
/// A `Task` state's integration parameters are the invoked operation's input, and
/// they are written in the integration's own convention — PascalCase — whatever the
/// invoked API spells its members in, so the spelling is settled here rather than
/// handing a service a member name its model does not carry.
///
/// Only the TOP level is re-spelled. Below it a document carries a caller's DATA
/// beside the operation's members — a DynamoDB attribute name is a map key, an
/// EventBridge detail is a caller's own document — and data is never re-spelled. A
/// member the operation types as a STRING is carried as JSON text wherever it sits,
/// top level or nested ([`Call::json_text_at`]), because that is a fact of the invoked
/// operation's own model rather than of the request's shape. A value already a string
/// is left as it is, so a member written as the text the operation types goes through
/// untouched.
pub(super) fn wire_request(call: &Call, request: &Value) -> Value {
    let Some(members) = request.as_object() else {
        return request.clone();
    };
    let mut wire = Map::new();
    for (member, value) in members {
        let member = call.api.member_case.spell(member);
        let value = match call.json_text.contains(&member.as_str()) {
            true if !value.is_string() => Value::String(value.to_string()),
            _ => value.clone(),
        };
        wire.insert(member, value);
    }
    let mut document = Value::Object(wire);
    for path in call.json_text_at {
        as_json_text(&mut document, path);
    }
    document
}

/// Replace the document the member `path` names in `document` with that document's JSON
/// text, leaving `document` as it is where the path names nothing and leaving a value
/// that is already text untouched.
///
/// The inverse of [`parse_in_place`], and the nested form of [`Call::json_text`]: a LIST
/// on the way is walked element by element, so one path names the same member of every
/// entry of a request that takes a list of them.
pub(super) fn as_json_text(document: &mut Value, path: &[&str]) {
    if let Some(items) = document.as_array_mut() {
        for item in items {
            as_json_text(item, path);
        }
        return;
    }
    let Some((member, rest)) = path.split_first() else {
        return;
    };
    let Some(node) = document.get_mut(*member) else {
        return;
    };
    if !rest.is_empty() {
        as_json_text(node, rest);
        return;
    }
    if !node.is_string() {
        *node = Value::String(node.to_string());
    }
}

/// One request, built for the protocol the invoked service's model declares.
pub(super) struct Wired {
    pub(super) method: &'static str,
    /// The request-target, which for an AWS JSON service is the root and for a REST
    /// one is the operation's own path and query.
    pub(super) uri: String,
    /// The operation's name as the AWS JSON protocol carries it
    /// (`<target prefix>.<OperationName>`); `None` for a REST request, which names
    /// the operation by its method and path.
    pub(super) target: Option<String>,
    /// The headers this operation's own members are bound to, beside the ones every
    /// request carries.
    pub(super) headers: Vec<(&'static str, String)>,
    pub(super) body: Vec<u8>,
    /// The media type the BODY carries, where it is not the one this service's protocol
    /// names for a request ([`Api::content_type`]): the XML document a REST-XML payload
    /// member is ([`Body::XmlMember`]). `None` leaves the protocol's own.
    pub(super) media_type: Option<&'static str>,
}

/// The request `call` makes with `request` as the operation's input, or why it could
/// not be built.
pub(super) fn wired(call: &Call, request: &Value) -> Result<Wired, String> {
    let document = wire_request(call, &call.operation_input(request));
    let Some(rest) = call.rest else {
        // An AWS JSON request body IS the operation's input document, posted to the
        // root and named in a header.
        let target = match call.api.wire {
            Wire::Json { target_prefix, .. } => format!("{target_prefix}.{}", call.operation),
            // An `awsQuery` request is a FORM, posted to the root, with the operation
            // named in it rather than in a header.
            Wire::Query { version, .. } => {
                return Ok(Wired {
                    method: "POST",
                    uri: "/".to_string(),
                    target: None,
                    headers: Vec::new(),
                    body: form_body(&call.operation, version, &document, call.form_maps)?
                        .into_bytes(),
                    media_type: None,
                })
            }
            // A service whose protocol is a REST one reaches this only for a call
            // carrying no binding, which [`read`] does not produce.
            Wire::Rest(_) => {
                return Err(format!(
                    "the '{}' call of {} carries no request binding for that service's REST \
                     protocol",
                    call.operation, call.api.sdk_id
                ))
            }
        };
        let body = ::serde_json::to_vec(&document).map_err(|error| error.to_string())?;
        return Ok(Wired {
            method: "POST",
            uri: "/".to_string(),
            target: Some(target),
            headers: Vec::new(),
            body,
            media_type: None,
        });
    };
    rest.wired(&document, call.payload_document)
}

/// The `application/x-www-form-urlencoded` body an `awsQuery` request carries:
/// `Action` naming the operation, `Version` naming the service's API version, and one
/// field per LEAF of the operation's input.
///
/// Each field name and each value is percent-encoded by the maintained encoder
/// ([`::percent_encoding`]), over RFC 3986's unreserved set — which is the encoding
/// AWS's own canonical request states — so a value carrying a delimiter is one field
/// rather than several, and a map key the caller chose cannot read as the protocol's
/// own `.` separator.
///
/// A member whose value is a MAP is spread as this protocol spreads one: one numbered
/// `entry` per pair, each carrying the pair's key and its value under the names that
/// map's own key and value members are written with ([`FormMap`]), and a value that is
/// a structure carrying its own members as one field each below it. Those names are the
/// invoked shape's, so a collection member no entry is carried for — any list, and a
/// map or structure this emulator does not state the field names of — is still NOT
/// bound: the call is not made for one rather than made with a field the service does
/// not carry.
pub(super) fn form_body(
    operation: &str,
    version: &str,
    document: &Value,
    maps: &[FormMap],
) -> Result<String, String> {
    let mut body = format!("Action={operation}&Version={version}");
    for (member, value) in document.as_object().into_iter().flatten() {
        let spread = match maps.iter().find(|map| map.member == *member) {
            Some(map) => Spread::Map(map),
            None => Spread::Scalar,
        };
        let mut fields = Vec::new();
        form_fields(&encoded(member), value, spread, &mut fields)?;
        for (field, value) in fields {
            body.push('&');
            body.push_str(&field);
            body.push('=');
            body.push_str(&encoded(&value));
        }
    }
    Ok(body)
}

/// What one node of a form request's document is spread over fields AS, which is a
/// fact of the member the invoked model declares at that place rather than of the
/// value.
#[derive(Debug, Clone, Copy)]
pub(super) enum Spread<'a> {
    /// One field, which is what every scalar is; a structure or a list reached here
    /// is a member whose field names this emulator does not carry, so no call is made
    /// for it.
    Scalar,
    /// A MAP, whose pairs are numbered `entry` fields under the names its key and
    /// value members are written with.
    Map(&'a FormMap),
    /// A STRUCTURE, each of whose members is one field named `<this>.<member>`.
    Structure,
}

/// The `entry` wrapper this protocol numbers a map's pairs under
/// (`<member>.entry.<n>.<key>`).
pub(super) const MAP_ENTRY: &str = "entry";

/// The fields the node `name` of a form request goes on the wire as, appended to
/// `fields` as (already-encoded name, raw value) pairs.
pub(super) fn form_fields(
    name: &str,
    value: &Value,
    spread: Spread<'_>,
    fields: &mut Vec<(String, String)>,
) -> Result<(), String> {
    // A member the request did not carry is no field at all.
    match value {
        Value::Null => Ok(()),
        Value::String(text) => {
            fields.push((name.to_string(), text.clone()));
            Ok(())
        }
        Value::Bool(_) | Value::Number(_) => {
            fields.push((name.to_string(), value.to_string()));
            Ok(())
        }
        Value::Object(members) => match spread {
            Spread::Map(map) => {
                for (index, (key, held)) in members.iter().enumerate() {
                    let entry = format!("{name}.{MAP_ENTRY}.{}", index + 1);
                    fields.push((format!("{entry}.{}", encoded(map.key)), key.clone()));
                    let at = format!("{entry}.{}", encoded(map.value));
                    let held_as = match map.structured {
                        true => Spread::Structure,
                        false => Spread::Scalar,
                    };
                    form_fields(&at, held, held_as, fields)?;
                }
                Ok(())
            }
            Spread::Structure => {
                for (member, held) in members {
                    let at = format!("{name}.{}", encoded(member));
                    form_fields(&at, held, Spread::Scalar, fields)?;
                }
                Ok(())
            }
            Spread::Scalar => Err(unbound_form_member(name, "structure")),
        },
        Value::Array(_) => Err(unbound_form_member(name, "list")),
    }
}

/// Why no call is made for a request carrying the member `name` as the collection
/// `kind`: this protocol spreads such a member over one field per leaf, under names
/// that are the invoked shape's own.
pub(super) fn unbound_form_member(name: &str, kind: &str) -> String {
    format!(
        "this task's '{name}' is a {kind}, which this emulator does not bind to that service's \
         form-field protocol"
    )
}

/// `text` percent-encoded as ONE field of a form ([`UNRESERVED_ONLY`]).
pub(super) fn encoded(text: &str) -> String {
    ::percent_encoding::utf8_percent_encode(text, UNRESERVED_ONLY).to_string()
}

/// The characters a value percent-encoded as ONE component escapes: everything but RFC
/// 3986's unreserved set, which is the encoding AWS's own canonical request states for a
/// query component and what a form field carries.
///
/// `pub(super)`, because it is the same set wherever a value of this crate is written
/// into a query or a form: the field of the form an `awsQuery` request IS
/// ([`form_body`]), and the query parameter and form field an HTTP Task sends
/// ([`super::http_task`]).
pub(crate) const UNRESERVED_ONLY: &::percent_encoding::AsciiSet =
    &::percent_encoding::NON_ALPHANUMERIC
        .remove(b'-')
        .remove(b'.')
        .remove(b'_')
        .remove(b'~');

impl Rest {
    /// The request this operation's bindings put `document` on the wire as.
    ///
    /// `payload_document` says what the payload member's value IS: a value out of a
    /// JSON document a state built, whose bytes are that value's own JSON text, or the
    /// bytes a caller handed the member already rendered ([`Call::payload_document`]).
    pub(super) fn wired(&self, document: &Value, payload_document: bool) -> Result<Wired, String> {
        let mut uri = String::new();
        let mut remaining = self.uri;
        // Each `{<member>}` of the path stands for the value the request names that
        // member with; a request naming none has no path to send, so the call is not
        // made rather than made against a path with a hole in it.
        while let Some((before, after)) = remaining.split_once('{') {
            uri.push_str(before);
            let (label, tail) = after
                .split_once('}')
                .ok_or_else(|| format!("the '{}' path binding is malformed", self.operation))?;
            // A label written `{<member>+}` is a GREEDY one, which spans as many path
            // segments as the value has: an object key is bound that way, so the
            // separator between its segments is part of the value rather than a
            // delimiter the value may not carry.
            let (label, component) = match label.strip_suffix('+') {
                Some(greedy) => (greedy, Component::GreedyLabel),
                None => (label, Component::Label),
            };
            let named = member_text(document, label).ok_or_else(|| {
                format!("this task's parameters name no '{label}', which the call's path needs")
            })?;
            uri.push_str(&one_component(&named, label, component)?);
            remaining = tail;
        }
        uri.push_str(remaining);
        for (member, parameter) in self.query {
            let Some(named) = member_text(document, member) else {
                continue;
            };
            let value = one_component(&named, member, Component::Query)?;
            // The operation's own `http.uri` may already carry a literal query — which
            // is how a service names a subresource (`?list-type=2`) — so what separates
            // this parameter from what is already there is read off the target as it
            // stands rather than assumed to be its first.
            uri.push(if uri.contains('?') { '&' } else { '?' });
            uri.push_str(parameter);
            uri.push('=');
            uri.push_str(&value);
        }
        let headers = self
            .headers
            .iter()
            .filter_map(|(member, field)| Some((*field, member_text(document, member)?)))
            .collect();
        let body = match self.request_body {
            // Every member is bound to the path, the query or a header, so there is no
            // body to send.
            Body::Empty => Vec::new(),
            // The payload member IS the body. For a value out of the document a state
            // built, the bytes are that JSON value's own TEXT — a string arrives as the
            // JSON string literal it is in that document, an object or an array as its
            // own document — because what the member carries is a value of that
            // document rather than bytes. A member handed bytes already rendered
            // carries them as they stand.
            Body::Member(member) | Body::Text(member) => match document.get(member) {
                None | Some(Value::Null) => Vec::new(),
                Some(Value::String(text)) if !payload_document => text.clone().into_bytes(),
                Some(value) => ::serde_json::to_vec(value).map_err(|error| error.to_string())?,
            },
            // The document of every member no other binding carries: one already on
            // the wire in the path, the query or a header is not in the body as well,
            // and a request whose members are all bound elsewhere carries no body.
            Body::Document => {
                let carried = self.unbound(document);
                match carried.is_empty() {
                    true => Vec::new(),
                    false => ::serde_json::to_vec(&Value::Object(carried))
                        .map_err(|error| error.to_string())?,
                }
            }
            // The payload member is the body here too, and what this protocol carries it
            // AS is an XML document whose root element is that member
            // ([`super::xml_body`]). A request naming the member nothing at all sends no
            // body, exactly as the JSON binding above does.
            Body::XmlMember {
                member,
                namespace,
                lists,
            } => match document.get(member) {
                None | Some(Value::Null) => Vec::new(),
                Some(value) => super::xml_body::request(member, namespace, value, lists)?,
            },
        };
        Ok(Wired {
            method: self.method,
            uri,
            target: None,
            headers,
            body,
            // The media type is the one the BODY is: an XML document, for a payload
            // member this protocol carries as one.
            media_type: match self.request_body {
                Body::XmlMember { .. } => Some(REST_XML_CONTENT_TYPE),
                Body::Document | Body::Member(_) | Body::Text(_) | Body::Empty => None,
            },
        })
    }

    /// The response document this operation's bindings read a 2xx answer as: the
    /// status, the headers and the body each under the member they are bound to.
    ///
    /// Every value in it is the ANSWER's own, and a member the answer carried nothing
    /// for is absent rather than filled in.
    pub(super) fn result(&self, answered: &Answered) -> Value {
        let mut members = Map::new();
        if !self.response_status.is_empty() {
            members.insert(
                self.response_status.to_string(),
                Value::Number(answered.status.into()),
            );
        }
        for (member, field) in self.response_headers {
            if let Some(value) = answered.header(field) {
                members.insert((*member).to_string(), Value::String(value));
            }
        }
        if !answered.payload.is_empty() {
            // The body is reported as the document it holds, which is what the
            // integration's result carries; a body that is not one is reported as
            // its own text rather than dropped.
            let payload = ::serde_json::from_slice::<Value>(&answered.payload).unwrap_or_else(
                |_| Value::String(String::from_utf8_lossy(&answered.payload).to_string()),
            );
            match self.response_body {
                Body::Member(member) => {
                    members.insert(member.to_string(), payload);
                }
                // A payload member the model types as a BLOB is the bytes themselves,
                // so it is reported as the TEXT it is even where those bytes parse as a
                // document: that is the string the integration answers, and the one a
                // state reads with `States.StringToJson`.
                Body::Text(member) => {
                    members.insert(
                        member.to_string(),
                        Value::String(String::from_utf8_lossy(&answered.payload).to_string()),
                    );
                }
                // The body IS the operation's output document, so the members it
                // holds are the answer's own: they are reported beside the ones the
                // status and the headers carry rather than under a member of their
                // own. A body that is not that document carries no member of this
                // operation's, and nothing is invented for one.
                Body::Document => {
                    if let Value::Object(carried) = payload {
                        members.extend(carried);
                    }
                }
                // An operation that binds no body reports none, whatever bytes the
                // answer happened to carry.
                Body::Empty => {}
                // The same binding read for a protocol that carries that member as an
                // XML DOCUMENT: the member is the document the answer's body describes
                // ([`super::xml_body::answered`]). A body that is no XML document at all
                // carries no member of this operation's, and nothing is invented for one.
                Body::XmlMember { member, lists, .. } => {
                    if let Some(document) = super::xml_body::answered(&answered.payload, lists) {
                        members.insert(member.to_string(), document);
                    }
                }
            }
        }
        Value::Object(members)
    }

    /// The members of `document` this operation binds NOWHERE else — not to its path,
    /// its query or a header — which are the ones its body carries
    /// ([`Body::Document`]).
    pub(super) fn unbound(&self, document: &Value) -> Map<String, Value> {
        let labels = self.labels();
        document
            .as_object()
            .into_iter()
            .flatten()
            .filter(|(member, _)| {
                !labels.iter().any(|label| label == member)
                    && !self.query.iter().any(|(bound, _)| bound == member)
                    && !self.headers.iter().any(|(bound, _)| bound == member)
            })
            .map(|(member, value)| (member.clone(), value.clone()))
            .collect()
    }

    /// The members this operation binds to its PATH, as the `{<member>}` holes of its
    /// `http.uri` name them, a GREEDY label's own `+` dropped.
    pub(super) fn labels(&self) -> Vec<&'static str> {
        let mut labels = Vec::new();
        let mut remaining = self.uri;
        while let Some((_, after)) = remaining.split_once('{') {
            let Some((label, tail)) = after.split_once('}') else {
                break;
            };
            labels.push(label.strip_suffix('+').unwrap_or(label));
            remaining = tail;
        }
        labels
    }

    /// The output member this operation binds the answer's BODY to, for an operation
    /// that binds one ([`Body::Member`]); `None` where the body is the output document
    /// itself or the answer carries none.
    pub(super) fn response_member(&self) -> Option<&'static str> {
        match self.response_body {
            Body::Member(member) | Body::Text(member) | Body::XmlMember { member, .. } => {
                Some(member)
            }
            Body::Document | Body::Empty => None,
        }
    }
}

/// The text `document` names `member` with: a value given as a string is that string,
/// and any other value is its JSON text, which is how a path, a query parameter or a
/// header carries one. `None` for a member the document does not carry.
pub(super) fn member_text(document: &Value, member: &str) -> Option<String> {
    match document.get(member) {
        None | Some(Value::Null) => None,
        Some(Value::String(text)) => Some(text.clone()),
        Some(value) => Some(value.to_string()),
    }
}

/// Which part of a request-target a member's value goes into, which decides one thing:
/// whether an EMPTY value is one that part can carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Component {
    /// A path label, which stands for one whole segment: an empty one would collapse
    /// the path it is part of, so there is no request to send for it.
    Label,
    /// A GREEDY path label (`{Key+}`), which stands for as many segments as the value
    /// holds: the separator between them is part of the value, so it is carried rather
    /// than refused. An empty one still collapses the path.
    GreedyLabel,
    /// A query parameter's value, which an operation may model as admitting the empty
    /// string (`Invoke`'s `Qualifier` does), and which a request carries as the
    /// parameter with nothing after its `=`.
    Query,
}

/// `value`, when it is text a request-target carries as ONE component — a path segment
/// or a query parameter's value — and `Err` naming `member` when it is not.
///
/// A member an operation binds to the path or the query is one whose own modelled
/// pattern admits only characters a URI component admits: a function's name, its ARN and
/// a version qualifier are written with the unreserved characters plus `:` and `$`, all
/// of which a component carries as they stand. A caller's parameter CAN hold anything,
/// so a value outside that set is one the request cannot address — the delimiters are
/// what would make it more than one component — and the call is not made rather than
/// made against a target assembled out of it. Nothing is escaped here: a value that
/// needed escaping names no resource of the invoked operation either.
///
/// The path SEPARATOR is the one character whose place depends on the component: a
/// greedy label spans several segments and a prefix or a key written in a query names
/// one, so each of those carries it, while a single-segment label does not.
pub(super) fn one_component(value: &str, member: &str, component: Component) -> Result<String, String> {
    let punctuation = match component {
        Component::Label => URI_COMPONENT_PUNCTUATION,
        Component::GreedyLabel | Component::Query => URI_PATH_PUNCTUATION,
    };
    let carried = value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || punctuation.contains(&byte));
    let empty = value.is_empty()
        && matches!(component, Component::Label | Component::GreedyLabel);
    if empty || !carried {
        return Err(format!(
            "this task's '{member}' is not text a request-target carries as one component, so it \
             addresses nothing the call can be made against"
        ));
    }
    Ok(value.to_string())
}

/// The punctuation a URI component carries unescaped beside the alphanumerics: RFC
/// 3986's unreserved set (`-`, `.`, `_`, `~`) and the two sub-delimiters an ARN and a
/// version qualifier are written with (`:`, `$`).
pub(super) const URI_COMPONENT_PUNCTUATION: &[u8] = b"-._~:$";

/// [`URI_COMPONENT_PUNCTUATION`] plus the path separator, for the components that span
/// segments: a greedy label (an object's key) and a query parameter naming a path (a
/// listing's prefix).
pub(super) const URI_PATH_PUNCTUATION: &[u8] = b"-._~:$/";
