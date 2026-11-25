// SPDX-License-Identifier: CC0-1.0

//! An XML document, read as a tree of elements and as the JSON document that tree carries.
//!
//! Several services in this tree are handed an XML document by something other than their own
//! wire: the ANSWER of a REST-XML or QUERY service one of them calls in process (the Amazon S3
//! listing a `Map` state's `ItemReader` reads, the `<PublishResponse>` document an API Gateway
//! service-proxy integration maps to a method response), and the error document a refused call
//! carries. This is the ONE reading of such a document, so no two of them disagree about what
//! an element stands for.
//!
//! The parse is the maintained pull parser's ([`::quick_xml`]) rather than anything
//! hand-written: this module adds only the shape the callers want — an element's name, its
//! text and the elements inside it — the JSON document that tree carries ([`as_value`]), and
//! the ceilings that confine a document produced somewhere else. Every value is the DOCUMENT's
//! own: nothing is defaulted, and nothing is coerced to a type the document does not state.

/// One element of an XML document: its local name, the text it carries directly, and
/// the elements nested inside it, in document order.
#[derive(Debug, Default, Clone)]
pub struct Element {
    /// The element's LOCAL name — the part after any namespace prefix — because a
    /// document may carry a prefix and the member it stands for is the same either way.
    pub name: String,
    /// The character data directly inside the element, with the XML escaping undone and
    /// the document's own indentation around it dropped ([`close`]) — the whitespace
    /// INSIDE it is the document's own and is kept, including the whitespace beside an
    /// entity reference, which the parser reports as an event of its own.
    pub text: String,
    /// The elements nested inside it, in document order.
    pub children: Vec<Element>,
}

impl Element {
    /// The first child element named `name`.
    #[must_use]
    pub fn child(&self, name: &str) -> Option<&Element> {
        self.children.iter().find(|child| child.name == name)
    }

    /// Every child element named `name`, in document order — which is what a list the
    /// protocol flattens (`ListObjectsV2`'s `Contents`) is carried as.
    pub fn each<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Element> {
        self.children.iter().filter(move |child| child.name == name)
    }

    /// The text of the first child named `name`; `None` when the element carries no
    /// such child, which is how a document reports a member it did not carry.
    #[must_use]
    pub fn text_of(&self, name: &str) -> Option<&str> {
        self.child(name).map(|child| child.text.as_str())
    }
}

/// How deeply nested an element this reads: past it the document is refused rather
/// than built, so a document produced elsewhere cannot be read into the thread's
/// stack. Far deeper than any document read through this module, whose deepest nesting is a
/// listing's entries.
const MAX_DEPTH: usize = 32;

/// How many elements one document may hold, which bounds what one document costs to
/// read. A listing page carries at most a thousand entries of some five elements
/// each, so this is several pages' worth.
const MAX_ELEMENTS: usize = 100_000;

/// `xml` as its root element, or why it could not be read.
///
/// The `Err` says what about the document was not readable; it never claims a value
/// the document did not carry.
pub fn read(xml: &[u8]) -> Result<Element, String> {
    use ::quick_xml::events::Event;

    // Every text event is read as the document wrote it: trimming each event on its own
    // would drop the whitespace that stands beside an ENTITY REFERENCE, which the parser
    // reports as an event of its own, so a sentence carrying an escaped character would
    // lose the spaces around it. The indentation a pretty-printed document puts around an
    // element's text is dropped once the element is complete ([`close`]) instead.
    let mut reader = ::quick_xml::Reader::from_reader(xml);
    reader.config_mut().trim_text(false);
    let mut open: Vec<Element> = Vec::new();
    let mut root: Option<Element> = None;
    let mut elements = 0usize;
    loop {
        match reader.read_event() {
            Ok(Event::Eof) => break,
            Ok(Event::Start(start)) => {
                elements += 1;
                if elements > MAX_ELEMENTS {
                    return Err(format!(
                        "the document holds more than the {MAX_ELEMENTS} XML elements this \
                         emulator reads"
                    ));
                }
                if open.len() >= MAX_DEPTH {
                    return Err(format!(
                        "the document nests XML elements more than the {MAX_DEPTH} levels deep \
                         this emulator reads"
                    ));
                }
                open.push(Element {
                    name: local_name(start.name().as_ref()),
                    ..Element::default()
                });
            }
            Ok(Event::Empty(empty)) => {
                elements += 1;
                if elements > MAX_ELEMENTS {
                    return Err(format!(
                        "the document holds more than the {MAX_ELEMENTS} XML elements this \
                         emulator reads"
                    ));
                }
                let element = Element {
                    name: local_name(empty.name().as_ref()),
                    ..Element::default()
                };
                close(element, &mut open, &mut root);
            }
            Ok(Event::Text(text)) => {
                let content = text.xml10_content().map_err(|error| error.to_string())?;
                if let Some(element) = open.last_mut() {
                    element.text.push_str(&content);
                }
            }
            // An entity reference is its own event, so the character it stands for is
            // resolved by the parser's own escaping rules rather than by a table here.
            Ok(Event::GeneralRef(reference)) => {
                let name = reference.decode().map_err(|error| error.to_string())?;
                let resolved = ::quick_xml::escape::unescape(&format!("&{name};"))
                    .map_err(|error| error.to_string())?
                    .into_owned();
                if let Some(element) = open.last_mut() {
                    element.text.push_str(&resolved);
                }
            }
            Ok(Event::CData(data)) => {
                let text = String::from_utf8_lossy(data.as_ref()).to_string();
                if let Some(element) = open.last_mut() {
                    element.text.push_str(&text);
                }
            }
            Ok(Event::End(_)) => {
                let element = open.pop().ok_or_else(|| {
                    "the document closes an XML element it never opened".to_string()
                })?;
                close(element, &mut open, &mut root);
            }
            Ok(_) => {}
            Err(error) => return Err(error.to_string()),
        }
    }
    root.ok_or_else(|| "the document carries no XML element at all".to_string())
}

/// Place a finished element: inside the element that encloses it, else as the
/// document's root.
///
/// The text it accumulated is trimmed HERE, once, rather than event by event: that drops
/// the indentation a pretty-printed document writes around an element's character data
/// (and leaves an element whose text is only that indentation carrying none) while
/// keeping every space inside the data itself.
fn close(mut element: Element, open: &mut [Element], root: &mut Option<Element>) {
    if element.text.trim().len() != element.text.len() {
        element.text = element.text.trim().to_string();
    }
    match open.last_mut() {
        Some(parent) => parent.children.push(element),
        None => *root = Some(element),
    }
}

/// The LOCAL part of an element's name: what follows the last `:`, since a prefix
/// names a namespace rather than the member.
fn local_name(name: &[u8]) -> String {
    let name = String::from_utf8_lossy(name).to_string();
    match name.rsplit_once(':') {
        Some((_prefix, local)) => local.to_string(),
        None => name,
    }
}

/// `element` as the JSON document its tree carries: an element with no children is the
/// TEXT it holds, one with children is the object of those children, and a name the
/// element repeats is the array of what each of them holds.
///
/// Every value is a STRING, because that is what an XML document carries: which of
/// them the document's own schema models as a number or a boolean is a fact of that
/// schema, which this module cannot read, so nothing here coerces one. An ATTRIBUTE is no
/// member either: the one every answer of a query api carries is its protocol's namespace
/// rather than a value of the result.
#[must_use]
pub fn as_value(element: &Element) -> ::serde_json::Value {
    if element.children.is_empty() {
        return ::serde_json::Value::String(element.text.clone());
    }
    let mut members = ::serde_json::Map::new();
    for child in &element.children {
        let value = as_value(child);
        match members.get_mut(&child.name) {
            // The second element of a repeated name turns what was read into the array
            // the protocol means by repeating it.
            Some(::serde_json::Value::Array(values)) => values.push(value),
            Some(held) => {
                let first = held.clone();
                members.insert(
                    child.name.clone(),
                    ::serde_json::Value::Array(vec![first, value]),
                );
            }
            None => {
                members.insert(child.name.clone(), value);
            }
        }
    }
    ::serde_json::Value::Object(members)
}

/// The whole document `xml` carries, as the object a reference walks from its top, or `None`
/// for bytes that are no XML document at all.
///
/// An XML document names its result in its ROOT ELEMENT, where a JSON document's members ARE
/// the document, so a reference that walks one from its top names the result either way: the
/// root element is a member of the object (`$.PublishResponse.PublishResult.MessageId` of a
/// `<PublishResponse>` answer) and so are the root's OWN members
/// (`$.Error.Code` of an `<ErrorResponse><Error>…` refusal). Both spellings name the same
/// value of the same document, which is what the data planes that read one off a service's
/// answer are observed to resolve.
///
/// A document whose root carries a member named like the root itself is named by the ROOT:
/// that is the one naming every document has, so it is the one that stands.
#[must_use]
pub fn rooted_document(xml: &[u8]) -> Option<::serde_json::Value> {
    let root = read(xml).ok()?;
    let value = as_value(&root);
    let mut members = ::serde_json::Map::new();
    if let ::serde_json::Value::Object(children) = &value {
        for (name, child) in children {
            members.insert(name.clone(), child.clone());
        }
    }
    members.insert(root.name, value);
    Some(::serde_json::Value::Object(members))
}

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
            "the bucket is the document's own Name, got {:?}",
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
        let root =
            read(b"<ListBucketResult><Prefix/></ListBucketResult>").expect("the listing reads");
        assert_eq!(
            root.text_of("Prefix"),
            Some(""),
            "a self-closing element carries the empty text, got {:?}",
            root.text_of("Prefix")
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

    #[test]
    fn a_whole_document_is_named_by_its_root_and_by_the_roots_own_members() {
        // The rows span what the reading generalises over: the depth a reference reaches, a
        // namespace attribute on the root, a prefixed element name, repeated siblings, an
        // element with no text at all, a root whose own text is the whole result, a name the
        // root and a child share, and bytes that are no document.
        let rows: &[(&str, &[u8], ::serde_json::Value)] = &[
            (
                "a query api's own answer",
                b"<PublishResponse xmlns=\"http://sns.amazonaws.com/doc/2010-03-31/\">\
                  <PublishResult><MessageId>8f1a</MessageId></PublishResult>\
                  <ResponseMetadata><RequestId>r-1</RequestId></ResponseMetadata>\
                  </PublishResponse>",
                ::serde_json::json!({
                    "PublishResponse": {
                        "PublishResult": { "MessageId": "8f1a" },
                        "ResponseMetadata": { "RequestId": "r-1" },
                    },
                    "PublishResult": { "MessageId": "8f1a" },
                    "ResponseMetadata": { "RequestId": "r-1" },
                }),
            ),
            (
                "an error envelope",
                b"<ErrorResponse><Error><Type>Sender</Type><Code>AuthorizationError</Code>\
                  </Error></ErrorResponse>",
                ::serde_json::json!({
                    "ErrorResponse": {
                        "Error": { "Type": "Sender", "Code": "AuthorizationError" },
                    },
                    "Error": { "Type": "Sender", "Code": "AuthorizationError" },
                }),
            ),
            (
                "a prefixed root, whose local name is the member",
                b"<ns:Result xmlns:ns=\"urn:x\"><Id>7</Id></ns:Result>",
                ::serde_json::json!({ "Result": { "Id": "7" }, "Id": "7" }),
            ),
            (
                "repeated siblings and an element with no text",
                b"<Topics><member>one</member><member>two</member><Next/></Topics>",
                ::serde_json::json!({
                    "Topics": { "member": ["one", "two"], "Next": "" },
                    "member": ["one", "two"],
                    "Next": "",
                }),
            ),
            (
                "a root whose own text is the whole result, which has no members to name",
                b"<Arn>arn:aws:sns:us-east-1:1:orders</Arn>",
                ::serde_json::json!({ "Arn": "arn:aws:sns:us-east-1:1:orders" }),
            ),
            (
                "a child named like the root, where the root's own naming stands",
                b"<Error><Error>inner</Error></Error>",
                ::serde_json::json!({ "Error": { "Error": "inner" } }),
            ),
        ];
        for (case, xml, expected) in rows {
            let read = rooted_document(xml);
            assert_eq!(
                read.as_ref(),
                Some(expected),
                "case {case:?}: expected {expected}, got {read:?}"
            );
        }
        for case in ["", "not xml at all", "{\"Code\":\"X\"}", "<unclosed>"] {
            let read = rooted_document(case.as_bytes());
            assert!(
                read.is_none(),
                "case {case:?}: bytes that are no XML document carry no document, got {read:?}"
            );
        }
    }
}
