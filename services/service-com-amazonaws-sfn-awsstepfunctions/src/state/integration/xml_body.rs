// SPDX-License-Identifier: CC0-1.0

//! The BODY of one call to a service whose protocol carries an operation's members as an
//! XML document (`restXml`), in both directions.
//!
//! A REST protocol spreads an operation's input over the path, the query, the headers and
//! one payload, and WHAT that payload is, is the protocol's: `restJson1` carries the
//! payload member as the JSON value it is ([`super::Body::Member`]), while `restXml`
//! carries it as an XML DOCUMENT whose root element is that member
//! ([`super::Body::XmlMember`]). This module is that rendering, and the reading of the
//! same document back out of an answer.
//!
//! A request reaches here as the JSON document its caller WROTE — a `Task` state's
//! parameters, or the `Input` of an EventBridge Scheduler universal target — so every
//! name in it is a caller's: a member whose name is not a name an XML element can carry
//! is refused rather than written into a document, and a value's text is escaped by the
//! maintained writer ([`::quick_xml`]) rather than by anything here.
//!
//! What the two directions need beyond the document itself is one fact of the INVOKED
//! model, which this crate cannot read: a LIST's entries are written under the element
//! the list's own member names (`Path` for CloudFront's `PathList`), which the binding
//! states per operation ([`super::Body::XmlMember`]). A list member no entry names is one
//! no call is made for, exactly as an unbound form member is
//! ([`super::unbound_form_member`]).
//!
//! The reading is the one XML parse this crate has
//! ([`crate::state::xml_answer`]); this module adds only the shape the protocol gives a
//! list.

use ::serde_json::Value;

use super::Lists;

/// How deeply nested a document is WALKED, in both directions: past it a request is
/// refused rather than written and an answer's own nesting is not descended into, so a
/// document neither side of the call is trusted to have bounded cannot be walked into
/// this thread's stack. Far deeper than any operation's own input or output, whose
/// deepest nesting is a few structures.
const MAX_DEPTH: usize = 32;

/// The XML document `member` carries as the body of a request, with `namespace` as the
/// root element's own namespace (the invoked service's `xmlNamespace`), or why no body
/// could be written for it.
pub(super) fn request(
    member: &str,
    namespace: &str,
    value: &Value,
    lists: Lists,
) -> Result<Vec<u8>, String> {
    let mut writer = ::quick_xml::Writer::new(Vec::new());
    let mut root = ::quick_xml::events::BytesStart::new(element_name(member)?);
    if !namespace.is_empty() {
        root.push_attribute(("xmlns", namespace));
    }
    write(&mut writer, root, value, lists, 0)?;
    Ok(writer.into_inner())
}

/// Write one element: `start` as it opens, and `value` as what it holds.
fn write(
    writer: &mut ::quick_xml::Writer<Vec<u8>>,
    start: ::quick_xml::events::BytesStart<'_>,
    value: &Value,
    lists: Lists,
    depth: usize,
) -> Result<(), String> {
    use ::quick_xml::events::{BytesEnd, BytesText, Event};

    if depth > MAX_DEPTH {
        return Err(format!(
            "the request nests members more than the {MAX_DEPTH} levels deep this emulator \
             writes as an XML document"
        ));
    }
    let name = String::from_utf8_lossy(start.name().as_ref()).to_string();
    let failed = |error: ::std::io::Error| error.to_string();
    writer.write_event(Event::Start(start)).map_err(failed)?;
    match value {
        // A member the request carries as nothing at all is written as the element with
        // nothing in it, which is what the document it wrote says.
        Value::Null => {}
        Value::String(text) => writer
            .write_event(Event::Text(BytesText::new(text)))
            .map_err(failed)?,
        // Every other scalar goes in as the text its own JSON rendering is, which is how
        // a number and a boolean are written in XML too.
        Value::Bool(_) | Value::Number(_) => writer
            .write_event(Event::Text(BytesText::new(&value.to_string())))
            .map_err(failed)?,
        Value::Object(members) => {
            for (member, held) in members {
                let child = ::quick_xml::events::BytesStart::new(element_name(member)?);
                write(writer, child, held, lists, depth + 1)?;
            }
        }
        Value::Array(entries) => {
            // The entries of a list are written under the element the list's own member
            // names, which is the invoked model's fact rather than this document's.
            let entry = lists
                .iter()
                .find(|(list, _)| *list == name)
                .map(|(_, entry)| *entry)
                .ok_or_else(|| {
                    format!(
                        "this request's '{name}' is a list, which this emulator does not bind to \
                         that service's XML protocol"
                    )
                })?;
            for held in entries {
                let child = ::quick_xml::events::BytesStart::new(element_name(entry)?);
                write(writer, child, held, lists, depth + 1)?;
            }
        }
    }
    writer
        .write_event(Event::End(BytesEnd::new(&name)))
        .map_err(failed)?;
    Ok(())
}

/// `member` when it is a name an XML element carries, and `Err` when it is not.
///
/// A member of a document a CALLER wrote is whatever that caller named it, and an element
/// name is not free text: the names an operation's own members have are ASCII letters and
/// digits beginning with a letter, so a member named with anything else — markup, a space,
/// a character outside that set — names no member of the invoked operation either, and the
/// call is not made rather than made against a document assembled out of it. Nothing is
/// escaped here — an element name cannot be escaped — which is why this is a refusal.
fn element_name(member: &str) -> Result<&str, String> {
    let named = !member.is_empty()
        && member.bytes().all(|byte| byte.is_ascii_alphanumeric())
        && member.starts_with(|first: char| first.is_ascii_alphabetic());
    match named {
        true => Ok(member),
        false => Err(format!(
            "this request's '{member}' is not a name an XML element carries, so it names no \
             member of the invoked operation"
        )),
    }
}

/// The document an XML answer's body carries, read as the invoked operation's own
/// payload member; `None` for a body that is no XML document at all.
///
/// Every value in it is the answer's own, and a LIST is reported as the list it is rather
/// than as the wrapper element its entries sit under ([`unwrapped`]).
///
/// The body is DATA off the wire, so the walks over it are bounded exactly as the walk
/// that writes one is: the parse refuses a document nesting elements deeper than it reads
/// ([`crate::state::xml_answer::read`]), and the walk below stops at [`MAX_DEPTH`] of its
/// own rather than resting on that one.
pub(super) fn answered(payload: &[u8], lists: Lists) -> Option<Value> {
    let root = crate::state::xml_answer::read(payload).ok()?;
    let mut document = crate::state::xml_answer::as_value(&root);
    unwrapped(&mut document, lists, 0);
    Some(document)
}

/// Report every list member of `document` as the list of its entries.
///
/// An XML list is the entries THEMSELVES, each under the element the list's member names,
/// so the parse of such a wrapper reads them as that element's own member — one value for
/// one entry and an array for several ([`crate::state::xml_answer::as_value`]). The
/// operation's output member is the LIST, so it is reported as one however many entries
/// the answer carried, and a wrapper the answer left empty is the empty list.
///
/// `depth` is how deep in the answer's own document this is: past [`MAX_DEPTH`] nothing
/// is descended into, so a document an answer carried cannot be walked into this thread's
/// stack. What is already read stands as the answer wrote it — the list shape is what
/// goes unreported below that depth, not the values.
fn unwrapped(document: &mut Value, lists: Lists, depth: usize) {
    if depth > MAX_DEPTH {
        return;
    }
    match document {
        Value::Array(entries) => {
            for entry in entries {
                unwrapped(entry, lists, depth + 1);
            }
        }
        Value::Object(members) => {
            for (member, held) in members.iter_mut() {
                unwrapped(held, lists, depth + 1);
                let Some((_, entry)) = lists.iter().find(|(list, _)| *list == member.as_str())
                else {
                    continue;
                };
                *held = entries_of(held, entry);
            }
        }
        _ => {}
    }
}

/// The entries the wrapper `held` carries under the element `entry`, as the list the
/// operation's member is.
fn entries_of(held: &Value, entry: &str) -> Value {
    let carried = match held {
        Value::Object(members) => members.get(entry).cloned(),
        // A wrapper the answer carried nothing in reads as the empty text, which is a
        // list of no entries.
        _ => None,
    };
    match carried {
        Some(Value::Array(entries)) => Value::Array(entries),
        Some(one) => Value::Array(vec![one]),
        None => Value::Array(Vec::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The list members of the one request this module's callers write: CloudFront's
    /// invalidation paths.
    const PATHS: Lists = &[("Items", "Path")];

    /// A request is written as the XML document the protocol carries: the payload member
    /// is the root element, each member of the document an element of its own, and a
    /// list's entries the element its own member names — so reading that document back
    /// reports the members the request stated, every value as the TEXT an XML document
    /// carries it as. The rows differ along the shape of the value: a structure, a nested
    /// structure, a list of none, of one and of several, a scalar of each JSON kind, and a
    /// member the document carries nothing for.
    ///
    /// Asserted over the document the writing and the reading ROUND TRIP to rather than
    /// over the bytes, because the ORDER a request's members are written in is the order
    /// the document carries them — which is the caller's own only while the build resolves
    /// `serde_json`'s `preserve_order` feature — while the document those elements
    /// describe is the same either way, and it is the document the invoked operation
    /// deserializes.
    #[test]
    fn a_request_is_the_xml_document_its_members_describe() {
        // (case, the payload member's value, the document that document carries)
        let rows: &[(&str, Value, Value)] = &[
            (
                "a structure of text",
                ::serde_json::json!({ "CallerReference": "ref-1" }),
                ::serde_json::json!({ "CallerReference": "ref-1" }),
            ),
            (
                "a nested structure carrying a list of one",
                ::serde_json::json!({
                    "Paths": { "Quantity": 1, "Items": ["/assets/*"] },
                    "CallerReference": "ref-2",
                }),
                ::serde_json::json!({
                    "Paths": { "Quantity": "1", "Items": ["/assets/*"] },
                    "CallerReference": "ref-2",
                }),
            ),
            (
                "a list of several entries",
                ::serde_json::json!({ "Paths": { "Items": ["/a", "/b", "/c"] } }),
                ::serde_json::json!({ "Paths": { "Items": ["/a", "/b", "/c"] } }),
            ),
            (
                "a list of no entries",
                ::serde_json::json!({ "Paths": { "Items": [] } }),
                ::serde_json::json!({ "Paths": { "Items": [] } }),
            ),
            (
                "a boolean and a number",
                ::serde_json::json!({ "Enabled": true, "Quantity": 7 }),
                ::serde_json::json!({ "Enabled": "true", "Quantity": "7" }),
            ),
            (
                "a member the request carries nothing for",
                ::serde_json::json!({ "CallerReference": null }),
                ::serde_json::json!({ "CallerReference": "" }),
            ),
            (
                // The text of a value is escaped by the writer, so a value carrying XML
                // punctuation is one VALUE rather than markup — which is what reading it
                // back as that same text establishes.
                "a value carrying XML punctuation",
                ::serde_json::json!({ "CallerReference": "a & b <c>" }),
                ::serde_json::json!({ "CallerReference": "a & b <c>" }),
            ),
        ];
        for (case, value, expected) in rows {
            let written = request("InvalidationBatch", "", value, PATHS)
                .unwrap_or_else(|why| panic!("case {case:?}: the document is written: {why}"));
            let text = String::from_utf8_lossy(&written).to_string();
            assert!(
                text.starts_with("<InvalidationBatch>") && text.ends_with("</InvalidationBatch>"),
                "case {case:?}: the payload member is the root element, got {text}"
            );
            let read = answered(&written, PATHS);
            assert_eq!(
                read.as_ref(),
                Some(expected),
                "case {case:?}: expected {expected}, got {read:?} out of {text}"
            );
        }
    }

    /// A value carrying XML punctuation is escaped where it is written, so the document is
    /// one element carrying that text rather than markup the value spelled.
    #[test]
    fn a_values_text_is_escaped_in_the_document() {
        let written = request(
            "InvalidationBatch",
            "",
            &::serde_json::json!({ "CallerReference": "a & b <c>" }),
            PATHS,
        )
        .expect("the document is written");
        let text = String::from_utf8_lossy(&written).to_string();
        assert_eq!(
            text,
            "<InvalidationBatch><CallerReference>a &amp; b &lt;c&gt;</CallerReference>\
             </InvalidationBatch>",
            "expected the value's punctuation escaped, got {text}"
        );
    }

    /// The root element carries the invoked service's own XML namespace.
    #[test]
    fn the_root_element_carries_the_services_namespace() {
        let namespace = "http://cloudfront.amazonaws.com/doc/2020-05-31/";
        let written = request(
            "InvalidationBatch",
            namespace,
            &::serde_json::json!({ "CallerReference": "r" }),
            PATHS,
        )
        .expect("the document is written");
        let written = String::from_utf8_lossy(&written).to_string();
        assert_eq!(
            written,
            format!(
                "<InvalidationBatch xmlns=\"{namespace}\">\
                 <CallerReference>r</CallerReference></InvalidationBatch>"
            ),
            "the root carries the namespace, got {written}"
        );
    }

    /// No document is written for a request this protocol cannot carry: a member whose
    /// name is no element name, and a list the binding names no entry element for.
    #[test]
    fn a_request_this_protocol_cannot_carry_is_no_document() {
        // (case, the payload member's value)
        let rows: &[(&str, Value)] = &[
            (
                "a member named with markup",
                ::serde_json::json!({ "<Caller>": "r" }),
            ),
            (
                "a member named with a space",
                ::serde_json::json!({ "Caller Reference": "r" }),
            ),
            ("a member with no name at all", ::serde_json::json!({ "": "r" })),
            (
                // Not every alphanumeric character is a name an XML element carries: the
                // set is the ASCII letters and digits the invoked model's own members are
                // written with.
                "a member named outside the ASCII letters and digits",
                ::serde_json::json!({ "Caller\u{b2}": "r" }),
            ),
            (
                "a member whose name opens with a digit",
                ::serde_json::json!({ "2Paths": "r" }),
            ),
            (
                "a list the binding names no entry element for",
                ::serde_json::json!({ "Aliases": ["one.example.com"] }),
            ),
        ];
        for (case, value) in rows {
            let written = request("InvalidationBatch", "", value, PATHS);
            assert!(
                written.is_err(),
                "case {case:?}: expected no document, got {:?}",
                written.map(|written| String::from_utf8_lossy(&written).to_string())
            );
        }
    }

    /// An answer's payload is read as the member's own document, with every list reported
    /// as the list it is: the rows differ along how many entries the wrapper carried.
    #[test]
    fn an_answer_reports_each_list_as_the_list_it_is() {
        // (case, the answer's body, the document it is read as)
        let rows: &[(&str, &str, Value)] = &[
            (
                "one entry",
                "<Invalidation><Id>I1</Id><InvalidationBatch><Paths><Quantity>1</Quantity>\
                 <Items><Path>/a</Path></Items></Paths></InvalidationBatch></Invalidation>",
                ::serde_json::json!({
                    "Id": "I1",
                    "InvalidationBatch": {
                        "Paths": { "Quantity": "1", "Items": ["/a"] },
                    },
                }),
            ),
            (
                "several entries",
                "<Invalidation><InvalidationBatch><Paths><Items><Path>/a</Path><Path>/b</Path>\
                 </Items></Paths></InvalidationBatch></Invalidation>",
                ::serde_json::json!({
                    "InvalidationBatch": { "Paths": { "Items": ["/a", "/b"] } },
                }),
            ),
            (
                "a wrapper carrying no entry",
                "<Invalidation><InvalidationBatch><Paths><Items/></Paths></InvalidationBatch>\
                 </Invalidation>",
                ::serde_json::json!({
                    "InvalidationBatch": { "Paths": { "Items": [] } },
                }),
            ),
        ];
        for (case, body, expected) in rows {
            let read = answered(body.as_bytes(), PATHS);
            assert_eq!(
                read.as_ref(),
                Some(expected),
                "case {case:?}: expected {expected}, got {read:?}"
            );
        }
    }

    /// A body that is no XML document at all is read as no document.
    #[test]
    fn a_body_that_is_no_document_is_read_as_none() {
        for case in ["", "not xml at all", "{\"Id\":\"I1\"}"] {
            let read = answered(case.as_bytes(), PATHS);
            assert!(
                read.is_none(),
                "case {case:?}: expected no document, got {read:?}"
            );
        }
    }

    /// Neither direction walks a document deeper than it reads: a REQUEST nesting members
    /// past the bound is refused rather than written, and an ANSWER nesting elements past
    /// it carries no document — so neither a caller's parameters nor an answer's body is
    /// walked into this thread's stack.
    #[test]
    fn neither_direction_walks_past_the_depth_it_reads() {
        let past = MAX_DEPTH + 2;
        let mut nested = Value::String("deep".to_string());
        for _ in 0..past {
            nested = ::serde_json::json!({ "Paths": nested });
        }
        let written = request("InvalidationBatch", "", &nested, PATHS);
        assert!(
            written.is_err(),
            "expected no document for a request nested {past} deep, got {:?}",
            written.map(|written| String::from_utf8_lossy(&written).len())
        );

        let mut body = String::new();
        for _ in 0..past {
            body.push_str("<Paths>");
        }
        body.push_str("deep");
        for _ in 0..past {
            body.push_str("</Paths>");
        }
        let read = answered(body.as_bytes(), PATHS);
        assert!(
            read.is_none(),
            "expected no document for an answer nested {past} deep, got {read:?}"
        );
    }
}
