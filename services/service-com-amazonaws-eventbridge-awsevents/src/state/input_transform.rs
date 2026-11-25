// SPDX-License-Identifier: CC0-1.0

//! EventBridge's INPUT TRANSFORMATION: what a rule's target receives when the
//! target does not take the whole event envelope.
//!
//! Moved out of `crate::state` unchanged, so one file holds one concern; the
//! rule it implements is stated where the module is declared
//! ([`crate::state`]).

use ::serde_json::Value;

/// The prefix EventBridge reserves for the placeholders it fills itself:
/// "the keys cannot start with `Amazon Web Services.`"
/// (`InputTransformer$InputPathsMap`), which is the `aws.` namespace on the
/// wire — so a key in it is never a caller's path and is always one of
/// [`reserved`]'s.
const RESERVED_KEY_PREFIX: &str = "aws.";

/// The placeholders EventBridge fills from the DELIVERY rather than from the
/// event: which rule matched, under which ARN, and when the event was
/// ingested.
#[derive(Debug, Clone, Copy)]
pub struct Delivery<'a> {
    /// The name of the rule whose target is being delivered to.
    pub rule: &'a str,
    /// That rule's ARN.
    pub rule_arn: &'a str,
    /// The instant the event carries, as the envelope's `time` renders it.
    pub ingestion_time: &'a str,
}

/// The placeholder standing for the whole event, and the one standing for its
/// JSON text.
const EVENT_PLACEHOLDER: &str = "aws.events.event";
const EVENT_JSON_PLACEHOLDER: &str = "aws.events.event.json";

/// The placeholders standing for the DELIVERY: the matched rule's ARN and name,
/// and the instant the event was ingested.
const RULE_ARN_PLACEHOLDER: &str = "aws.events.rule-arn";
const RULE_NAME_PLACEHOLDER: &str = "aws.events.rule-name";
const INGESTION_TIME_PLACEHOLDER: &str = "aws.events.event.ingestion-time";

/// Every placeholder of the reserved namespace EventBridge fills itself, named
/// once: what [`reserved`] fills and what [`fills_itself`] recognises are the
/// same five, so a template may carry exactly the placeholders a rendering can
/// fill.
const RESERVED_PLACEHOLDERS: [&str; 5] = [
    EVENT_PLACEHOLDER,
    EVENT_JSON_PLACEHOLDER,
    RULE_ARN_PLACEHOLDER,
    RULE_NAME_PLACEHOLDER,
    INGESTION_TIME_PLACEHOLDER,
];

/// Whether `key` is a placeholder EventBridge fills from the delivery rather
/// than from a caller's own path ([`RESERVED_PLACEHOLDERS`]).
fn fills_itself(key: &str) -> bool {
    RESERVED_PLACEHOLDERS.contains(&key)
}

/// The member of the envelope that carries what the publisher published, which
/// the placeholder standing for the EVENT does not reach.
const DETAIL_FIELD: &str = "detail";

/// What the reserved placeholder `key` is filled with, or `None` when the
/// reserved namespace holds no such placeholder.
///
/// The five EventBridge fills itself: the event's envelope WITHOUT its detail
/// (`aws.events.event`), the whole event as JSON text
/// (`aws.events.event.json`), the matched rule's ARN and name, and the event's
/// ingestion time.
///
/// The two event placeholders stand for DIFFERENT readings of the event, and that
/// is what each is for: `<aws.events.event>` is the event's metadata — the
/// envelope with the `detail` member left out — while `<aws.events.event.json>`
/// is the whole event, detail included. So a template that needs the envelope
/// without the payload has a placeholder for exactly that, and one that needs the
/// payload reaches it through the JSON placeholder or through a path of its own
/// map.
///
/// A placeholder standing for an event carries a JSON DOCUMENT wherever it stands
/// — [`Fill::Document`] — because the event is a JSON object and a reading of one
/// as characters is not a thing any consumer is handed: a template that is nothing
/// but `<aws.events.event.json>` renders the event, which is what the placeholder
/// is for. The other three carry text a caller may put anywhere, so they fill like
/// a selected value does.
fn reserved(key: &str, event: &Value, delivery: &Delivery<'_>) -> Option<Fill> {
    match key {
        EVENT_PLACEHOLDER => Some(Fill::Document(without_detail(event).to_string())),
        EVENT_JSON_PLACEHOLDER => Some(Fill::Document(event.to_string())),
        RULE_ARN_PLACEHOLDER => Some(Fill::Selected(Value::String(
            delivery.rule_arn.to_string(),
        ))),
        RULE_NAME_PLACEHOLDER => Some(Fill::Selected(Value::String(
            delivery.rule.to_string(),
        ))),
        INGESTION_TIME_PLACEHOLDER => Some(Fill::Selected(Value::String(
            delivery.ingestion_time.to_string(),
        ))),
        _ => None,
    }
}

/// `event` without its [`DETAIL_FIELD`] member, which is what the placeholder
/// standing for the event resolves to.
///
/// Every other member is carried over as it stands, in the envelope's own order:
/// what is left out is the detail and nothing else. An event that is not an object
/// carries no member to leave out, so it stands as it is.
fn without_detail(event: &Value) -> Value {
    let mut envelope = event.clone();
    if let Value::Object(members) = &mut envelope {
        members.remove(DETAIL_FIELD);
    }
    envelope
}

/// What ONE placeholder is filled with, told apart by how it is WRITTEN into the
/// template.
#[derive(Debug, Clone)]
enum Fill {
    /// A value the event carries: its JSON text where the placeholder stands as a
    /// JSON value, and its reading as a string ([`text`]) where it stands inside
    /// one of the template's own strings.
    Selected(Value),
    /// A JSON document, which is written as it stands wherever the placeholder is
    /// — an event is one.
    Document(String),
}

/// What a path selected, told apart the way a RENDERING has to tell it
/// apart: the value standing at the place the path named, and the absence of
/// any such place.
///
/// The two are different facts about the event — "the member is there and
/// holds `null`" is not "the event has no such member" — and a placeholder
/// stands for a different thing in each ([`render`]), so the selection
/// carries which of them it was rather than collapsing both onto one value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selection {
    /// The value at the one place the path named, or the list of what a slice
    /// in it spans.
    Value(Value),
    /// The path named no place this event carries.
    Nothing,
}

/// What `path` selects out of `event`: the value at the one place the path
/// names, the list of what a slice or wildcard in it spans, or
/// [`Selection::Nothing`] for a path that selects NO place of this event.
///
/// `Err` is the path this service did NOT evaluate: a form the bounded
/// reader does not read, or a selection past the places it selects. That is
/// not a path that selected nothing, so it is never rendered as an absent
/// value — the caller refuses the delivery instead of handing the target a
/// payload built from a path nobody evaluated. The message names the path,
/// which is what a consumer of the invocation log has to see to fix it.
pub fn selection(path: &str, event: &Value) -> Result<Selection, String> {
    use ::service_common::json_reference_path::{select_many, selection_segments, Selected};

    let Some(named) = selection_segments(path) else {
        return Err(format!(
            "the path {path} is not one this emulator evaluates"
        ));
    };
    match select_many(&named, event) {
        Selected::Value(value) => Ok(Selection::Value(value)),
        Selected::NoMatch => Ok(Selection::Nothing),
        Selected::NotSelected => Err(format!(
            "the path {path} names more places than the {} this emulator selects",
            ::service_common::json_reference_path::MAX_SELECTED_ELEMENTS
        )),
    }
}

/// [`selection`] as ONE value: a path that selects no place of the event
/// selects JSON's empty value, `null`.
///
/// This is the reading a target carrying `InputPath` is handed ([`by_input_path`]),
/// where what travels is the JSON TEXT of the selected part of the event and
/// an absent part has no text of its own. It is NOT how a placeholder of an
/// `InputTransformer` template is filled, which is a substitution rather than
/// a value and is stated at [`render`].
pub fn selected(path: &str, event: &Value) -> Result<Value, String> {
    selection(path, event).map(|selection| match selection {
        Selection::Value(value) => value,
        Selection::Nothing => Value::Null,
    })
}

/// What a target carrying `InputPath` receives: the JSON text of the one
/// part of the event that path selects — and `Err` for a path this service
/// did not evaluate ([`selected`]).
pub fn by_input_path(path: &str, event: &Value) -> Result<String, String> {
    selected(path, event).map(|selected| selected.to_string())
}

/// What a target carrying an `InputTransformer` receives: `template` with
/// each `<key>` placeholder replaced by what `paths` selects for that key.
///
/// A substitution is TEXTUAL: every placeholder stands for the characters its
/// value contributes where it stands, and the template's own characters are
/// delivered as written. Where a placeholder stands decides which characters
/// those are, because the documented templates are of two kinds:
///
/// * a STRUCTURED one, opening a JSON OBJECT or ARRAY, carries its placeholders
///   "in quotes or out" (`InputTransformer$InputTemplate`), so a placeholder
///   standing as a JSON value carries the selected value's JSON text —
///   `{"n": <n>}` renders a number — and one standing inside one of the
///   template's own string literals carries that value's reading as TEXT
///   ([`text`]), written as that literal carries those characters
///   ([`escaped_for_a_string`]): a quote, a newline or a backslash in a selected
///   string reaches the target as the character it is and the template stays the
///   JSON it was written as;
/// * a PLAIN-TEXT one — a sentence, and the quoted string a sentence is written
///   as (`"<instance> is in state <status>"`) — is text, so every placeholder in
///   it carries that same reading as TEXT and NOTHING is escaped for the text
///   around it.
///
/// So a plain-text template splicing a value that carries a quote or a newline
/// renders exactly those characters, which leaves a rendering that is not a JSON
/// document: nothing re-quotes a value to keep a template's text parseable,
/// because what the target would then receive is not what the template says. Such
/// an input is never handed to a target: the invocation fails and the event is
/// dead-lettered ([`crate::ops::put_events`] decides that).
///
/// A placeholder standing for an EVENT carries that event's JSON document
/// ([`Fill::Document`]) wherever it stands, for the reason [`reserved`] gives.
///
/// A key the map does not hold and the reserved namespace does not name is
/// not a placeholder at all: the text is left exactly as the template wrote
/// it, since nothing says what it stands for — and a TARGET carrying such a
/// template is refused where it is configured ([`unfilled_placeholder`]), so no
/// delivery is built from a placeholder nothing fills.
///
/// A placeholder whose path selects NO place of the event is filled with the
/// EMPTY STRING, so `{"k": <absent>}` renders the JSON string `""` and
/// `{"k": "<absent>"}` renders an empty string too — which is not what a
/// placeholder for a place that IS there and holds `null` renders as (`null`
/// unquoted, the characters `null` inside a string). The two are different
/// facts about the event ([`Selection`]), so a consumer of the payload can
/// tell "no such element" from "the element was null".
///
/// `Err` is a transformation this service will not build: a path it did not
/// evaluate ([`selected`]), or a rendering past [`MAX_RENDERED_BYTES`] — a
/// template may repeat one placeholder as often as its own length allows and
/// each fill carries a value as large as the event, so the PRODUCT of the
/// two is what that bound refuses. Either way nothing is delivered rather
/// than a payload the target never asked for.
pub fn render(
    template: &str,
    paths: &::std::collections::BTreeMap<String, String>,
    event: &Value,
    delivery: &Delivery<'_>,
) -> Result<String, String> {
    // Where the scan stands as it walks the template: whether the template's own
    // JSON is kept, whether the scan is inside one of its strings, and whether
    // the character before escaped this one ([`Scan`]).
    let mut scan = Scan::of(template);
    let mut rendered = String::with_capacity(template.len());
    let mut rest = template;
    while !rest.is_empty() {
        // A placeholder is read before the character opening it is
        // classified: `<` is never part of a template's own JSON structure.
        if let Some((key, tail)) = placeholder(rest).filter(|_| !scan.escaped()) {
            if let Some(fill) = filling(key, paths, event, delivery)? {
                match fill {
                    // A JSON document stands as it is wherever the placeholder is.
                    Fill::Document(document) => rendered.push_str(&document),
                    // A value standing where a STRUCTURED template takes a VALUE
                    // is written as its JSON text.
                    Fill::Selected(value) if scan.structured() && !scan.in_string() => {
                        rendered.push_str(&value.to_string());
                    }
                    // A STRING standing inside one of a structured template's own
                    // string literals is written as that literal carries those
                    // characters, so the template stays the JSON it was written as.
                    Fill::Selected(Value::String(characters)) if scan.in_string() => {
                        rendered.push_str(&escaped_for_a_string(&characters));
                    }
                    // Everywhere else — a plain-text template, and a container
                    // inside a structured template's own string, which no quoting
                    // of a JSON string would make a value of — the placeholder
                    // carries the value's reading as TEXT, as it reads ([`text`]).
                    Fill::Selected(value) => rendered.push_str(&text(&value)),
                }
                if rendered.len() > MAX_RENDERED_BYTES {
                    return Err(format!(
                        "the transformation renders more than the {MAX_RENDERED_BYTES} \
                         bytes this emulator renders"
                    ));
                }
                rest = tail;
                continue;
            }
        }
        let character = rest.chars().next().unwrap_or_default();
        scan.advance(character);
        rendered.push(character);
        rest = &rest[character.len_utf8()..];
    }
    Ok(rendered)
}

/// Where a walk of a template STANDS, which is what tells a placeholder from
/// text the template wrote: whether the template is the structured JSON whose
/// own punctuation the rendering keeps, whether the walk is inside one of that
/// JSON's string literals, and whether the character before escaped this one.
///
/// One state machine, read by the rendering ([`render`]) and by the check that a
/// template's placeholders can be filled at all ([`unfilled_placeholder`]), so
/// the two cannot disagree about what a template carries: a `<` the template
/// escaped inside one of its strings opens no placeholder for either of them.
struct Scan {
    structured: bool,
    in_string: bool,
    escaped: bool,
}

impl Scan {
    /// The state a walk of `template` starts in. A template that opens a JSON
    /// OBJECT or ARRAY is the structured kind, whose own JSON the rendering keeps:
    /// a placeholder in it stands either where a JSON value does or inside one of
    /// the template's own string literals, and what lands inside such a literal is
    /// written as that literal carries it ([`escaped_for_a_string`]).
    ///
    /// Every other template is PLAIN TEXT — a sentence, and the quoted string a
    /// sentence is written as to make its rendering a JSON document — so the
    /// template's own characters are delivered as written, a placeholder in it
    /// stands for the characters its value reads as ([`text`]) and NOTHING is
    /// escaped for the text around it: a value carrying a quote or a newline
    /// renders exactly those characters, which is what can leave such a rendering
    /// no JSON document at all ([`render`] states what becomes of one).
    fn of(template: &str) -> Self {
        Self {
            structured: template.trim_start().starts_with(['{', '[']),
            in_string: false,
            escaped: false,
        }
    }

    /// Whether the template's own JSON is kept.
    fn structured(&self) -> bool {
        self.structured
    }

    /// Whether the walk stands inside a string literal of a structured template.
    fn in_string(&self) -> bool {
        self.structured && self.in_string
    }

    /// Whether the character the walk stands on was escaped by the one before it,
    /// so a `<` there is the template's own text rather than a placeholder.
    fn escaped(&self) -> bool {
        self.escaped
    }

    /// Read one character of the template.
    fn advance(&mut self, character: char) {
        if !self.structured {
            return;
        }
        if self.in_string {
            if self.escaped {
                self.escaped = false;
            } else if character == '\\' {
                self.escaped = true;
            } else if character == '"' {
                self.in_string = false;
            }
        } else if character == '"' {
            self.in_string = true;
        }
    }
}

/// The most bytes ONE rendering may produce.
///
/// This service's own bound, not a limit AWS publishes: `InputTemplate` is
/// modelled at 8192 characters and each placeholder in it may carry a value
/// as large as a published entry ([`super::MAX_ENTRY_SIZE_BYTES`]), so their
/// product is not bounded by either. One rendering is one target's payload,
/// so it is held to the size of the event it was built from.
pub const MAX_RENDERED_BYTES: usize = super::MAX_ENTRY_SIZE_BYTES;

/// What the placeholder `key` is filled with: what the caller's own path for
/// it selects, or the reserved placeholder of that name — `Ok(None)` when
/// neither names it, which is what leaves the template's text alone, and
/// `Err` for a path this service did not evaluate ([`selected`]).
fn filling(
    key: &str,
    paths: &::std::collections::BTreeMap<String, String>,
    event: &Value,
    delivery: &Delivery<'_>,
) -> Result<Option<Fill>, String> {
    if let Some(path) = paths.get(key) {
        return selection(path, event).map(|selection| {
            Some(Fill::Selected(match selection {
                Selection::Value(value) => value,
                // Nothing was selected, so the placeholder stands for no
                // characters at all: the empty string, which is a JSON string
                // where the placeholder stands as a value and nothing where it
                // stands inside one.
                Selection::Nothing => Value::String(String::new()),
            }))
        });
    }
    Ok(key
        .starts_with(RESERVED_KEY_PREFIX)
        .then(|| reserved(key, event, delivery))
        .flatten())
}

/// The key the placeholder at the head of `text` names, with the text after
/// it — `None` when `text` does not open one.
///
/// A placeholder is `<key>` with no `<`, `>` or newline inside it, so an
/// unterminated `<` and a `<` that merely occurs in prose are left alone.
fn placeholder(text: &str) -> Option<(&str, &str)> {
    let after = text.strip_prefix('<')?;
    let end = after.find('>')?;
    let key = &after[..end];
    if key.is_empty() || key.contains(['<', '\n', '\r', '"']) {
        return None;
    }
    Some((key, &after[end + 1..]))
}

/// The first placeholder of `template` that NOTHING would fill: one whose key
/// `declared` does not name and which is not one EventBridge fills itself
/// ([`RESERVED_PLACEHOLDERS`]) — `None` when every placeholder it carries has a
/// value to carry.
///
/// A template's placeholders "will be filled with the values of the keys from
/// `InputPathsMap`" (`InputTransformer$InputTemplate`), so a key that map does
/// not hold names no value: the transformation is a configuration the service
/// refuses when the target is written, rather than one that delivers the
/// placeholder's own text to the target. `declared` answers whether the target's
/// own map holds a key, and the walk stands where [`render`]'s does ([`Scan`]),
/// so this reads the SAME placeholders that rendering fills — a `<` the template
/// escaped inside one of its own strings is text to both — and an accepted
/// target is one every delivery can render.
pub fn unfilled_placeholder<'a>(
    template: &'a str,
    declared: &dyn Fn(&str) -> bool,
) -> Option<&'a str> {
    let mut scan = Scan::of(template);
    let mut rest = template;
    while !rest.is_empty() {
        if let Some((key, tail)) = placeholder(rest).filter(|_| !scan.escaped()) {
            if !declared(key) && !fills_itself(key) {
                return Some(key);
            }
            rest = tail;
            continue;
        }
        let character = rest.chars().next().unwrap_or_default();
        scan.advance(character);
        rest = &rest[character.len_utf8()..];
    }
    None
}

/// What a placeholder standing where a JSON VALUE does is read as while a
/// template is being JUDGED, before anything fills it: JSON's empty value, which
/// every fill stands in the place of.
const A_PLACEHOLDERS_VALUE: &str = "null";

/// Whether `template` states a JSON document: whether it is one once every
/// placeholder stands for what it will be filled with.
///
/// What an `InputTransformer` delivers is the JSON text a target is handed, and a
/// template is that document with holes in it: a placeholder standing where a
/// value does will be filled with a value, and one standing inside a string
/// literal with characters of that string. So the template is read with each
/// placeholder replaced by what it stands for — a JSON value where a value
/// stands, nothing inside a structured template's own string literal — and what
/// is left must parse. A template that is nothing but one placeholder states a
/// document (`<aws.events.event.json>` is the event), an object or array with
/// placeholders in it states one, a quoted string states the string it is, and a
/// bare unquoted sentence states none.
///
/// The walk stands where [`render`]'s does ([`Scan`]), so this reads the same
/// placeholders a rendering fills and judges the same template the delivery will
/// build. What it judges is the template's own FORM: a template that states a
/// document can still RENDER text that is not one, because what a placeholder
/// carries is a value the event supplies rather than the `null` read here — that
/// half is charged at delivery time, where such an input is dead-lettered rather
/// than handed over (`crate::ops::put_events`).
pub fn states_a_json_document(template: &str) -> bool {
    let mut scan = Scan::of(template);
    let mut neutral = String::with_capacity(template.len());
    let mut rest = template;
    while !rest.is_empty() {
        if let Some((_, tail)) = placeholder(rest).filter(|_| !scan.escaped()) {
            if !scan.in_string() {
                neutral.push_str(A_PLACEHOLDERS_VALUE);
            }
            rest = tail;
            continue;
        }
        let character = rest.chars().next().unwrap_or_default();
        scan.advance(character);
        neutral.push(character);
        rest = &rest[character.len_utf8()..];
    }
    ::serde_json::from_str::<Value>(neutral.trim()).is_ok()
}

/// The characters a selected value contributes where a placeholder stands for it
/// AS TEXT: inside one of a structured template's own string literals, or
/// anywhere in a plain-text template.
///
/// A STRING contributes its own characters, with none of JSON's quoting of a
/// string: `{"o": "<order>"}` renders the order id and not a second pair of
/// quotes around it. A scalar contributes its literal — a number its digits,
/// `null` the four characters `null`.
///
/// A CONTAINER contributes its elements' and members' own readings as text,
/// inside its own brackets and separated by commas, with JSON's quoting of the
/// strings in it dropped and a member written `name:value`: an array of two ids
/// reads `[o1,o2]` and a list of objects `[{id:o1,v:1},{id:o2}]`. The reading is
/// the value as TEXT at every depth, which is what a template splicing a
/// container into a sentence delivers — JSON's own punctuation belongs to the
/// JSON TEXT of the value, which is what a placeholder standing where a JSON
/// value does carries instead ([`render`]).
///
/// The walk is as deep as the SELECTED value nests, which is as deep as the
/// event it was selected out of parsed ([`crate::state::parse_delivered_event`]
/// reads it with `serde_json`, whose own nesting bound the document had to pass),
/// and it reads one value of one event, whose size [`super::MAX_ENTRY_SIZE_BYTES`]
/// bounds.
fn text(value: &Value) -> String {
    match value {
        Value::String(characters) => characters.clone(),
        Value::Array(elements) => {
            let read: Vec<String> = elements.iter().map(text).collect();
            format!("[{}]", read.join(","))
        }
        Value::Object(members) => {
            let read: Vec<String> = members
                .iter()
                .map(|(name, value)| format!("{name}:{}", text(value)))
                .collect();
            format!("{{{}}}", read.join(","))
        }
        scalar => scalar.to_string(),
    }
}

/// `characters` written as the JSON string literal they land in carries them, so
/// a string value holding a quote, a backslash or a control character is the
/// string it is rather than text that breaks the template in half.
fn escaped_for_a_string(characters: &str) -> String {
    let quoted = Value::String(characters.to_string()).to_string();
    // `Value::String`'s rendering is the QUOTED form, and what the placeholder
    // stands for is its content: the quotes around it belong to the template's
    // own string, which already carries them.
    quoted
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .unwrap_or(&quoted)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::{render, selected, Delivery};


    use ::serde_json::json;

    /// The event every row below transforms.
    fn event() -> ::serde_json::Value {
        json!({
            "id": "e-1",
            "detail-type": "Order Created",
            "source": "com.demo",
            "time": "2026-01-01T00:00:00Z",
            "detail": {
                "order": "A-1",
                "amount": 7,
                "items": ["a", "b", "c"],
                "lines": [{ "sku": "s1" }, { "sku": "s2" }],
                "nul": null,
                "quoted": "he said \"hi\"",
            },
        })
    }

    fn delivery() -> Delivery<'static> {
        Delivery {
            rule: "r",
            rule_arn: "arn:aws:events:eu-west-2:000000000000:rule/r",
            ingestion_time: "2026-01-01T00:00:00Z",
        }
    }

    /// What a path selects, over a TABLE whose rows differ along the KIND of
    /// place the path names: a member, an element, a slice, a place the
    /// event does not carry, and a path this service does not evaluate at
    /// all — which is NOT a path that selected nothing.
    #[test]
    fn a_path_selects_its_place_and_an_absent_one_selects_null() {
        // (case, path, the value it selects, `None` for a path this service
        // did not evaluate)
        let rows = [
            ("a member", "$.detail.order", Some(json!("A-1"))),
            ("a number", "$.detail.amount", Some(json!(7))),
            (
                "a hyphenated member",
                "$.detail-type",
                Some(json!("Order Created")),
            ),
            ("an element", "$.detail.items[1]", Some(json!("b"))),
            ("a slice", "$.detail.items[0:2]", Some(json!(["a", "b"]))),
            ("an open slice", "$.detail.items[1:]", Some(json!(["b", "c"]))),
            ("an absent member", "$.detail.nope", Some(json!(null))),
            (
                "an element past the array",
                "$.detail.items[9]",
                Some(json!(null)),
            ),
            // Each of these is a form the bounded reader declines, so the
            // selection is REFUSED rather than answered with the empty
            // value: nothing was evaluated, so nothing may be rendered.
            ("a wildcard member", "$.detail.*", None),
            ("a descendant", "$..order", None),
            ("a union", "$.detail.items[0,1]", None),
            ("a filter", "$.detail.items[?@=='a']", None),
            ("a negative index", "$.detail.items[-1]", None),
            ("a slice with a step", "$.detail.items[0:2:1]", None),
            ("text that is not a path", "detail.order", None),
        ];
        let event = event();
        for (case, path, expected) in rows {
            let actual = selected(path, &event);
            assert_eq!(
                actual.clone().ok(),
                expected,
                "case {case:?}: expected {path} to select {expected:?}, got {actual:?}"
            );
            if expected.is_none() {
                assert!(
                    actual.is_err_and(|refusal| refusal.contains(path)),
                    "case {case:?}: the refusal names the path that was not evaluated"
                );
            }
        }
    }

    /// How a placeholder renders, over a TABLE whose rows differ along where
    /// the placeholder stands (in a string or out), what the path selects,
    /// and whether the template is JSON at all.
    #[test]
    fn a_placeholder_renders_the_value_where_it_stands() {
        let paths: ::std::collections::BTreeMap<String, String> = [
            ("order", "$.detail.order"),
            ("amount", "$.detail.amount"),
            ("items", "$.detail.items[0:2]"),
            ("lines", "$.detail.lines"),
            ("absent", "$.detail.nope"),
            ("beyond", "$.detail.items[9]"),
            ("nul", "$.detail.nul"),
            ("quoted", "$.detail.quoted"),
        ]
        .into_iter()
        .map(|(key, path)| (key.to_string(), path.to_string()))
        .collect();
        let rows = [
            ("a bare value", "{\"n\": <amount>}", "{\"n\": 7}"),
            (
                "a value in a string",
                "{\"n\": \"<amount>\"}",
                "{\"n\": \"7\"}",
            ),
            (
                "a string in a string",
                "{\"o\": \"<order>\"}",
                "{\"o\": \"A-1\"}",
            ),
            ("a bare string", "{\"o\": <order>}", "{\"o\": \"A-1\"}"),
            ("a slice", "{\"i\": <items>}", "{\"i\": [\"a\",\"b\"]}"),
            // A path that selects no place of the event fills the
            // placeholder with the empty string, wherever it stands.
            ("an absent path", "{\"a\": <absent>}", "{\"a\": \"\"}"),
            (
                "an absent path in a string",
                "{\"a\": \"<absent>\"}",
                "{\"a\": \"\"}",
            ),
            (
                "an index past the array",
                "{\"a\": <beyond>}",
                "{\"a\": \"\"}",
            ),
            (
                "an absent element in a sentence",
                "before <absent> after",
                "before  after",
            ),
            // A place that IS there and holds `null` is the other case the
            // rendering keeps apart: the literal unquoted, its characters
            // quoted.
            ("a null path", "{\"n\": <nul>}", "{\"n\": null}"),
            ("a null path in a string", "{\"n\": \"<nul>\"}", "{\"n\": \"null\"}"),
            (
                // The quotes a selected STRING carries are written as the string
                // it lands in carries them, so the template stays the JSON it was
                // written as.
                "a value carrying quotes",
                "{\"q\": \"<quoted>\"}",
                "{\"q\": \"he said \\\"hi\\\"\"}",
            ),
            (
                // A TEXT template escapes nothing: the value's characters are
                // written as they read, which is what can leave a rendering that
                // is no JSON document at all.
                "a value carrying quotes in a sentence",
                "he told me: <quoted>",
                "he told me: he said \"hi\"",
            ),
            (
                "a quoted-string template keeps its own quoting",
                "\"<order>\"",
                "\"A-1\"",
            ),
            (
                // A selected list has no reading as a JSON value inside one of
                // the template's own strings, so it reads as TEXT there: its
                // elements' own readings, inside its brackets, unquoted.
                "a list inside the template's own string",
                "{\"i\": \"<items>\"}",
                "{\"i\": \"[a,b]\"}",
            ),
            (
                "a list in a sentence",
                "items: <items>",
                "items: [a,b]",
            ),
            (
                "a list of objects in a sentence",
                "lines: <lines>",
                "lines: [{sku:s1},{sku:s2}]",
            ),
            (
                // A quoted-string template is PLAIN TEXT: its own quoting is
                // delivered as written and the list reads as text inside it.
                "a list inside a quoted-string template",
                "\"<items>\"",
                "\"[a,b]\"",
            ),
            (
                // Nothing is escaped for a plain-text template, so a value
                // carrying a quote renders that quote — which is what leaves such
                // a rendering no JSON document.
                "a value carrying quotes in a quoted-string template",
                "\"he said <quoted>\"",
                "\"he said he said \"hi\"\"",
            ),
            (
                "a sentence template",
                "<order> is for <amount>",
                "A-1 is for 7",
            ),
            (
                "a reserved placeholder",
                "{\"r\": \"<aws.events.rule-name>\"}",
                "{\"r\": \"r\"}",
            ),
            (
                // Nothing says what an unknown key stands for, so the
                // template's own text is delivered.
                "a key nothing names",
                "{\"u\": \"<nope>\"}",
                "{\"u\": \"<nope>\"}",
            ),
        ];
        let event = event();
        for (case, template, expected) in rows {
            let actual = render(template, &paths, &event, &delivery());
            assert_eq!(
                actual.as_deref(),
                Ok(expected),
                "case {case:?}: expected {template:?} to render {expected:?}, got {actual:?}"
            );
        }
    }

    /// What each reserved placeholder resolves to, over a TABLE whose rows differ
    /// along the placeholder: the event's envelope WITHOUT its detail, the whole
    /// event with it, and the three the delivery itself supplies.
    #[test]
    fn a_reserved_placeholder_resolves_to_the_event_or_the_delivery() {
        // (case, the placeholder, the value it resolves to)
        let event = event();
        let mut envelope = event.clone();
        envelope
            .as_object_mut()
            .expect("the event is an object")
            .remove("detail");
        let rows = [
            // "<aws.events.event>" is the metadata: the envelope's own members
            // with the published payload left out.
            ("the event without its detail", "aws.events.event", envelope),
            ("the whole event", "aws.events.event.json", event.clone()),
            (
                "the matched rule's ARN",
                "aws.events.rule-arn",
                json!(delivery().rule_arn),
            ),
            (
                "the matched rule's name",
                "aws.events.rule-name",
                json!(delivery().rule),
            ),
            (
                "the event's ingestion time",
                "aws.events.event.ingestion-time",
                json!(delivery().ingestion_time),
            ),
        ];
        let paths = ::std::collections::BTreeMap::new();
        for (case, key, expected) in rows {
            let template = format!("{{\"v\": <{key}>}}");
            let rendered = render(&template, &paths, &event, &delivery())
                .unwrap_or_else(|refusal| panic!("case {case:?}: {template} was refused: {refusal}"));
            let actual: ::serde_json::Value = ::serde_json::from_str(&rendered)
                .unwrap_or_else(|error| {
                    panic!("case {case:?}: {rendered} is not JSON ({error})")
                });
            assert_eq!(
                actual["v"], expected,
                "case {case:?}: expected <{key}> to resolve to {expected}, got {rendered}"
            );
        }
    }

    /// Which templates STATE a JSON document, over a TABLE whose rows differ along
    /// the form of the template and along where its placeholders stand.
    #[test]
    fn a_template_states_a_json_document_or_is_not_one() {
        // (case, the template, whether it states a JSON document)
        let rows = [
            ("an object with a bare placeholder", "{\"n\": <amount>}", true),
            ("an object with a quoted placeholder", "{\"n\": \"<amount>\"}", true),
            (
                "an object with a placeholder inside a longer string",
                "{\"n\": \"x<amount>y\"}",
                true,
            ),
            ("an array", "[<amount>, \"<order>\"]", true),
            ("an object with no placeholder at all", "{\"n\": 1}", true),
            ("leading whitespace", "  {\"n\": <amount>}", true),
            ("a quoted string", "\"<order> is for <amount>\"", true),
            (
                "nothing but a reserved placeholder",
                "<aws.events.event.json>",
                true,
            ),
            // A placeholder standing alone states the value it will be filled
            // with. What that value turns out to be is the delivery's business:
            // a selected STRING renders characters that are no document, which
            // is an input the delivery dead-letters rather than one this refuses.
            ("nothing but a caller's own placeholder", "<order>", true),
            // A bare, unquoted sentence states no document: rendered, it could
            // only produce text no target may be handed.
            ("a bare sentence", "<order> is for <amount>", false),
            ("a sentence carrying no placeholder", "a fixed sentence", false),
            ("an object that is never closed", "{\"n\": <amount>", false),
            ("a string that is never closed", "\"<order>", false),
            ("nothing at all", "", false),
            ("an object with text after it", "{\"n\": 1} and more", false),
        ];
        for (case, template, states) in rows {
            let actual = super::states_a_json_document(template);
            assert_eq!(
                actual, states,
                "case {case:?}: expected {template:?} to state a document={states}, got {actual}"
            );
        }
    }

    /// A rendering past [`super::MAX_RENDERED_BYTES`] is refused rather than
    /// built: a template may repeat one placeholder as often as its own
    /// length allows, so the product of the template's length and the size
    /// of what a placeholder carries is what the bound holds.
    #[test]
    fn a_rendering_past_the_bound_is_refused() {
        let large = "x".repeat(super::MAX_RENDERED_BYTES / 2 + 1);
        let event = json!({ "detail": { "large": large } });
        let paths: ::std::collections::BTreeMap<String, String> =
            [("large".to_string(), "$.detail.large".to_string())]
                .into_iter()
                .collect();
        // (case, the template, whether it renders at all)
        let rows = [
            ("one fill, inside the bound", "{\"a\": \"<large>\"}", true),
            ("two fills, past it", "{\"a\": \"<large><large>\"}", false),
        ];
        for (case, template, renders) in rows {
            let actual = render(template, &paths, &event, &delivery());
            assert_eq!(
                actual.is_ok(),
                renders,
                "case {case:?}: expected a rendering={renders} for a template filling \
                 {} bytes at a time, got {:?}",
                large.len(),
                actual.map(|rendered| rendered.len())
            );
        }
    }
}
