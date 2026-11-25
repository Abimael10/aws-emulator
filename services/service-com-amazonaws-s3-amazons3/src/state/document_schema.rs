// SPDX-License-Identifier: CC0-1.0

//! Whether a request DOCUMENT validates against the schema the API publishes for it.
//!
//! One reading for every XML document this service is handed, because the rule is one rule:
//! the schema declares, for each container, the elements that may appear INSIDE it and the
//! ones it must carry, so an element is judged against the container it arrived in and a
//! container against the children it owes. A document that misspells, misnests or invents an
//! element, or that leaves out a child the schema requires, does not validate — and a codec
//! reads an element it does not know by IGNORING it, so what reaches a handler no longer says
//! whether the caller spelled one. That is why this reads the BYTES the request carried.
//!
//! Each document's own schema is a table beside the record it describes
//! ([`super::WEBSITE_DOCUMENT_SCHEMA`], [`super::ANALYTICS_DOCUMENT_SCHEMA`]), derived from
//! the payload shape's closure in the model; this module is the walk over one of them, so two
//! documents cannot drift apart on what "validates" means. The LEXICAL SPACE a document states
//! a boolean in ([`document_boolean`]) is here for the same reason: it is one question about
//! every document of this service, not about one of them.

/// The schema one request document is judged against: the elements each container declares,
/// and the children each container requires.
#[derive(Debug, Clone, Copy)]
pub struct DocumentSchema {
    /// Every element the published schema declares, with the elements each one may contain —
    /// the ROOT element FIRST. A container whose row lists no child, and an element with no
    /// row at all, is a LEAF: it carries text, not elements.
    pub elements: &'static [(&'static str, &'static [&'static str])],
    /// The children each container REQUIRES, one row per child, so a container owing several
    /// names several rows.
    pub required: &'static [(&'static str, &'static str)],
}

impl DocumentSchema {
    /// The element this schema declares under `parent` — or as the ROOT, for a `parent` of
    /// `None` — whose name is `element`, or `None` for a name the schema does not declare
    /// there.
    fn declared(&self, parent: Option<&str>, element: &[u8]) -> Option<&'static str> {
        let declared: &[&'static str] = match parent {
            // The root element of the document: the container the payload IS, which is this
            // schema's first row.
            None => match self.root() {
                Some(root) => return (root.as_bytes() == element).then_some(root),
                None => &[],
            },
            Some(parent) => self
                .elements
                .iter()
                .find(|(name, _)| *name == parent)
                // A LEAF carries text, not elements, so nothing is declared inside it.
                .map_or(&[], |(_, children)| *children),
        };
        declared
            .iter()
            .copied()
            .find(|name| name.as_bytes() == element)
    }

    /// The ROOT element this schema declares: its first row, which is the container the
    /// payload IS.
    #[must_use]
    pub fn root(&self) -> Option<&'static str> {
        self.elements.first().map(|(element, _)| *element)
    }

    /// The children the schema requires inside `element`, in the order the table names them.
    fn required_children(&self, element: &str) -> Vec<&'static str> {
        self.required
            .iter()
            .filter(|(container, _)| *container == element)
            .map(|(_, required)| *required)
            .collect()
    }
}

/// Why a document does not validate against the schema published for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocumentFault {
    /// The document is not a well-formed XML document at all, or it names an element the
    /// schema does not declare in the position it arrived in.
    Undeclared,
    /// A container named none of a child the schema requires inside it.
    MissingRequiredChild {
        /// The container that owes the child.
        container: &'static str,
        /// The child element it named none of.
        child: &'static str,
    },
}

/// Why `document` — the XML a request carried as its payload — does not validate against
/// `schema`, or `None` when it does.
///
/// A document that is not well-formed XML at all — bytes no XML reader can walk, an element
/// left unclosed, an end tag naming another element, or no element whatever — is
/// [`DocumentFault::Undeclared`] too: S3 answers both with the one code, whose own sentence
/// states them together ("not well-formed or did not validate against our published schema").
///
/// The FIRST fault in document order is what answers, the way every other refusal of this
/// service names one; a container's missing child is noticed where that container closes.
#[must_use]
pub fn document_fault(schema: &DocumentSchema, document: &[u8]) -> Option<DocumentFault> {
    let mut reader = ::quick_xml::Reader::from_reader(document);
    // The trail of element names from the root down to the element being read, each with the
    // children the schema still owes inside it.
    let mut open: Vec<(&'static str, Vec<&'static str>)> = Vec::new();
    // Whether the document has a root element at all, which an XML document has by
    // definition and bytes that are not markup do not.
    let mut rooted = false;
    loop {
        let event = match reader.read_event() {
            Ok(event) => event,
            // Not well formed.
            Err(_) => return Some(DocumentFault::Undeclared),
        };
        let parent = open.last().map(|(element, _)| *element);
        match event {
            ::quick_xml::events::Event::Start(start) => {
                let Some(element) = schema.declared(parent, start.name().as_ref()) else {
                    return Some(DocumentFault::Undeclared);
                };
                rooted = true;
                satisfy(&mut open, element);
                let owed = schema.required_children(element);
                open.push((element, owed));
            }
            // A self-closing element (`<Condition/>`) is an element with no children at all:
            // the name is judged the same way, and the children it owes are owed still.
            ::quick_xml::events::Event::Empty(start) => {
                let Some(element) = schema.declared(parent, start.name().as_ref()) else {
                    return Some(DocumentFault::Undeclared);
                };
                if let Some(child) = schema.required_children(element).first() {
                    return Some(DocumentFault::MissingRequiredChild {
                        container: element,
                        child,
                    });
                }
                rooted = true;
                satisfy(&mut open, element);
            }
            ::quick_xml::events::Event::End(_) => match open.pop() {
                // A container that never named a child the schema requires inside it.
                Some((container, owed)) => {
                    if let Some(child) = owed.first() {
                        return Some(DocumentFault::MissingRequiredChild { container, child });
                    }
                }
                None => return Some(DocumentFault::Undeclared),
            },
            ::quick_xml::events::Event::Eof => {
                return match rooted && open.is_empty() {
                    true => None,
                    false => Some(DocumentFault::Undeclared),
                }
            }
            _ => {}
        }
    }
}

/// Note `element` as named inside the container it arrived in, which is what satisfies that
/// container's requirement for it.
fn satisfy(open: &mut [(&'static str, Vec<&'static str>)], element: &'static str) {
    if let Some((_, owed)) = open.last_mut() {
        owed.retain(|child| *child != element);
    }
}

/// The two words the lexical space of a boolean is written with, which is also what the codec
/// reading a request document binds a boolean member from: the numeric spelling of each is
/// folded onto these before that codec reads it (`crate::service::wire`).
pub const DOCUMENT_BOOLEAN_TRUE: &str = "true";
/// See [`DOCUMENT_BOOLEAN_TRUE`].
pub const DOCUMENT_BOOLEAN_FALSE: &str = "false";

/// Every spelling a request document of this service states a boolean with, and the value each
/// one states — the TRUE spellings first.
///
/// Source: the schema an S3 request document is validated against declares such an element an
/// `xsd:boolean`, whose lexical space is `true`, `false`, `1` and `0` (W3C XML Schema Part 2,
/// §3.2.2), the last two being the numeric spellings of the first two.
///
/// `pub`: `tests/all/zzz_batch_probe.rs` drives every row of it through the operations whose
/// documents carry such an element, so a spelling named here is one a request really states.
pub const DOCUMENT_BOOLEAN_SPELLINGS: &[(&str, bool)] = &[
    (DOCUMENT_BOOLEAN_TRUE, true),
    ("1", true),
    (DOCUMENT_BOOLEAN_FALSE, false),
    ("0", false),
];

/// The boolean the TEXT of a request document's boolean element states, or `None` for text that
/// states none at all.
///
/// The text is read TRIMMED, because an element's surrounding whitespace is markup rather than
/// value, and against [`DOCUMENT_BOOLEAN_SPELLINGS`] and nothing else: text in no spelling of
/// the published space — an element carrying no text included — states no boolean. What such
/// text makes of the request is the business of the member it was written for: a member stating
/// a VALUE the service keeps is one the published schema cannot read, while one whose rule is
/// that stating `true` turns a switch on states no `true`, so the switch stays off
/// (`crate::service::wire::XML_MODE_SWITCH_DOCUMENT_ELEMENTS`).
#[must_use]
pub fn document_boolean(text: &str) -> Option<bool> {
    let spelled = text.trim();
    DOCUMENT_BOOLEAN_SPELLINGS
        .iter()
        .find(|(spelling, _)| *spelling == spelled)
        .map(|(_, stated)| *stated)
}

/// Whether `text` states the boolean TRUE: the reading the text of a SWITCH is given — an
/// element whose published rule is that stating `true` turns it on
/// (`crate::service::wire::XML_MODE_SWITCH_DOCUMENT_ELEMENTS`).
///
/// The CASE is not part of what such text states: the published documentation of these members
/// spells the value `TRUE` ("Setting this element to TRUE causes the following behavior",
/// `smithy_query(query="shape",shape="PublicAccessBlockConfiguration")`) where the lexical space
/// spells it lower case, so the word is read in any case. Text stating anything else — the
/// boolean false, a word that is no boolean at all, a number outside the space, no text whatever
/// — states no true, so the switch it was written for stays OFF: the reading its own absence
/// has, which is why such text does not make the document one that fails to validate.
#[must_use]
pub fn document_switch_is_on(text: &str) -> bool {
    let spelled = text.trim();
    spelled.eq_ignore_ascii_case(DOCUMENT_BOOLEAN_TRUE) || document_boolean(spelled) == Some(true)
}
