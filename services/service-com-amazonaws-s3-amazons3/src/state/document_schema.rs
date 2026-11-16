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
//! documents cannot drift apart on what "validates" means.

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
