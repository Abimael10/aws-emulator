// SPDX-License-Identifier: CC0-1.0

//! The XML answer of a REST-XML service, read as a tree of elements.
//!
//! One service this crate calls answers XML rather than JSON: Amazon S3, whose
//! protocol is `restXml` — the dataset a `Map` state's `ItemReader` names lives there
//! ([`super::item_reader`]), and the listing that reader reads is an XML document, as
//! is the error document a refused read carries. Nothing else in this crate reads XML,
//! so this is the ONE place that does.
//!
//! The document itself is read by the tree's ONE reading of an XML document
//! ([`::service_common::xml_document`]) — the maintained pull parser, the element tree it
//! builds and the ceilings that confine it — re-exported here so this crate's callers go on
//! naming one reading. What this module adds is the one shape that is this crate's own
//! question: the flat object an ERROR document is read as ([`as_document`]), which is the
//! same pair of members under three protocols' envelopes.

pub use ::service_common::xml_document::{as_value, read, Element};

/// The document `xml` carries, as the flat object its root element's text-only
/// children make: what an XML refusal is read as, so one reading of an error answer
/// serves both protocols.
///
/// S3's model sets `noErrorWrapping`, so a refusal's `Code` and `Message` are the
/// root's own children; an `awsQuery` refusal wraps the same pair in an `Error`
/// element, and an `ec2Query` one wraps that element in an `Errors` element inside the
/// response — each read through to the same object. `None` for a body that is not an
/// XML document at all.
///
/// The ENVELOPE's own text children are carried beside the error's, which is where a form
/// protocol names the request it answers (`<RequestId>` in the `awsQuery` envelope,
/// `<RequestID>` in the `ec2Query` one): an exception a client raises reports that
/// identifier in its metadata, so it is part of the one reading of a refusal. The error's
/// own members win, because they are what the refusal itself states.
#[must_use]
pub fn as_document(xml: &[u8]) -> Option<::serde_json::Value> {
    let root = read(xml).ok()?;
    let carried = root
        .child(ERROR_ELEMENT)
        .or_else(|| {
            root.child(ERRORS_ELEMENT)
                .and_then(|errors| errors.child(ERROR_ELEMENT))
        })
        .unwrap_or(&root);
    let mut members = ::serde_json::Map::new();
    for child in carried.children.iter().chain(root.children.iter()) {
        if child.children.is_empty() {
            members
                .entry(child.name.clone())
                .or_insert_with(|| ::serde_json::Value::String(child.text.clone()));
        }
    }
    Some(::serde_json::Value::Object(members))
}

/// The element an `awsQuery` refusal wraps its `Code` and `Message` in.
const ERROR_ELEMENT: &str = "Error";

/// The element an `ec2Query` refusal wraps that one in:
/// `<Response><Errors><Error>…</Error></Errors></Response>`, which is that protocol's
/// own error envelope.
const ERRORS_ELEMENT: &str = "Errors";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_element_tree_carries_each_child_in_document_order() {
        let xml = br#"<ListBucketResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/">
            <Name>bucket</Name>
            <IsTruncated>false</IsTruncated>
            <Contents><Key>a</Key><Size>1</Size></Contents>
            <Contents><Key>b</Key><Size>2</Size></Contents>
        </ListBucketResult>"#;
        let root = read(xml).expect("the listing reads");
        assert_eq!(root.name, "ListBucketResult", "the root is the result element");
        assert_eq!(
            root.text_of("Name"),
            Some("bucket"),
            "the bucket is the answer's own Name, got {:?}",
            root.text_of("Name")
        );
        let keys: Vec<&str> = root
            .each("Contents")
            .filter_map(|entry| entry.text_of("Key"))
            .collect();
        assert_eq!(
            keys,
            vec!["a", "b"],
            "every Contents entry is read in document order, got {keys:?}"
        );
    }

    #[test]
    fn an_empty_element_carries_no_text() {
        let root = read(b"<ListBucketResult><Prefix/></ListBucketResult>")
            .expect("the listing reads");
        assert_eq!(
            root.text_of("Prefix"),
            Some(""),
            "a self-closing element carries the empty text, got {:?}",
            root.text_of("Prefix")
        );
    }

    #[test]
    fn a_refusal_document_is_read_as_its_code_and_message() {
        let document = as_document(
            b"<Error><Code>NoSuchKey</Code><Message>The specified key does not exist.</Message>\
              </Error>",
        )
        .expect("the refusal reads");
        assert_eq!(
            document.get("Code").and_then(::serde_json::Value::as_str),
            Some("NoSuchKey"),
            "the code is the document's own, got {document:?}"
        );
        assert_eq!(
            document
                .get("Message")
                .and_then(::serde_json::Value::as_str),
            Some("The specified key does not exist."),
            "the message is the document's own, got {document:?}"
        );
    }

    #[test]
    fn an_elements_text_keeps_the_whitespace_beside_an_escaped_character() {
        // (case, the document, the text the element carries)
        let rows: &[(&str, &[u8], &str)] = &[
            (
                "an escaped apostrophe mid-sentence",
                b"<Message>Could not cast message attribute &apos;count&apos; value to \
                  number.</Message>",
                "Could not cast message attribute 'count' value to number.",
            ),
            (
                "the same character by its code point",
                b"<Message>attribute &#39;a b&#39; is invalid</Message>",
                "attribute 'a b' is invalid",
            ),
            (
                "an escaped ampersand and angle bracket",
                b"<Message>a &amp; b &lt; c</Message>",
                "a & b < c",
            ),
            (
                "the indentation around character data is not part of it",
                b"<Message>\n            a plain sentence\n        </Message>",
                "a plain sentence",
            ),
        ];
        for (case, xml, expected) in rows {
            let element = read(xml).expect("the document reads");
            assert_eq!(
                element.text, *expected,
                "case {case:?}: expected {expected:?}, got {:?}",
                element.text
            );
        }
    }

    #[test]
    fn a_body_that_is_not_xml_carries_no_document() {
        for case in ["", "not xml at all", "{\"Code\":\"X\"}"] {
            let read = as_document(case.as_bytes());
            let empty = read
                .as_ref()
                .and_then(::serde_json::Value::as_object)
                .is_some_and(::serde_json::Map::is_empty);
            assert!(
                read.is_none() || empty,
                "case {case:?}: a body that is not an XML document carries no members, got \
                 {read:?}"
            );
        }
    }

    #[test]
    fn a_wrapped_refusal_is_read_through_its_error_element() {
        let document = as_document(
            b"<ErrorResponse><Error><Type>Sender</Type><Code>ValidationError</Code>\
              <Message>1 validation error detected</Message></Error>\
              <RequestId>r-1</RequestId></ErrorResponse>",
        )
        .expect("the refusal reads");
        assert_eq!(
            document.get("Code").and_then(::serde_json::Value::as_str),
            Some("ValidationError"),
            "the code is read through the Error element, got {document:?}"
        );
    }

    #[test]
    fn an_ec2_refusal_is_read_through_its_errors_element() {
        let document = as_document(
            b"<Response><Errors><Error><Code>InvalidInstanceID.NotFound</Code>\
              <Message>The instance ID 'i-1' does not exist</Message></Error></Errors>\
              <RequestID>r-1</RequestID></Response>",
        )
        .expect("the refusal reads");
        assert_eq!(
            document.get("Code").and_then(::serde_json::Value::as_str),
            Some("InvalidInstanceID.NotFound"),
            "the code is read through the Errors element, got {document:?}"
        );
        assert_eq!(
            document
                .get("Message")
                .and_then(::serde_json::Value::as_str),
            Some("The instance ID 'i-1' does not exist"),
            "the message is read through the Errors element, got {document:?}"
        );
    }

    #[test]
    fn an_element_tree_is_read_as_the_document_it_carries() {
        let rows: &[(&str, &[u8], ::serde_json::Value)] = &[
            (
                "text only",
                b"<Arn>arn:aws:iam::1:user/u</Arn>",
                ::serde_json::json!("arn:aws:iam::1:user/u"),
            ),
            (
                "an object of members",
                b"<Result><Account>1</Account><UserId>U</UserId></Result>",
                ::serde_json::json!({ "Account": "1", "UserId": "U" }),
            ),
            (
                "a repeated name is an array",
                b"<Result><Tag>a</Tag><Tag>b</Tag><Tag>c</Tag></Result>",
                ::serde_json::json!({ "Tag": ["a", "b", "c"] }),
            ),
            (
                "nesting is nesting",
                b"<Result><User><Id>U</Id></User></Result>",
                ::serde_json::json!({ "User": { "Id": "U" } }),
            ),
        ];
        for (case, xml, expected) in rows {
            let element = read(xml).expect("the document reads");
            let value = as_value(&element);
            assert_eq!(
                value, *expected,
                "case {case:?}: expected {expected}, got {value}"
            );
        }
    }
}
