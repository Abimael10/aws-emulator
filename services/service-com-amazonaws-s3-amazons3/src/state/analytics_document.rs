// SPDX-License-Identifier: CC0-1.0

//! The schema an `AnalyticsConfiguration` DOCUMENT has to validate against before any of it
//! is read, and how S3 refuses one that does not.
//!
//! `PutBucketAnalyticsConfiguration`'s payload IS the configuration, so what the document
//! names becomes the stored configuration — and a codec reads an element it does not know by
//! IGNORING it, so a document that misspells, misnests or invents one parses to a
//! configuration missing the member that element was meant to carry. S3 answers such a
//! document that it did not validate against the published schema rather than storing what
//! was left of it, and a configuration naming no storage class analysis at all gets a refusal
//! of its own, because that container is what the configuration IS.
//!
//! So a document this module reports a rejection for is withheld from the codec in full
//! (`crate::service::wire`), leaving `PutBucketAnalyticsConfiguration` the one place such a
//! document's refusal is worded — read from the BYTES the request carried
//! (`crate::service::wire::request_document`), which is the only place an element the codec
//! ignored still exists.

use super::*;

/// Every element the published `AnalyticsConfiguration` schema declares, with the elements
/// each one may contain — the ROOT element first.
///
/// Source: the `AnalyticsConfiguration` shape closure
/// (`smithy_query(query="closure",operation="PutBucketAnalyticsConfiguration",
/// model_path="services/service-com-amazonaws-s3-amazons3/smithy-model/com-amazonaws-s3-amazons3.json")`).
/// Each structure's children are its members under the element name restXml spells them with
/// — their own declared names, no member of the closure being renamed — and the one list in
/// it, `AnalyticsAndOperator$Tags`, under its `xmlName` (`Tag`, `@xmlFlattened`). A member
/// whose target is a plain string or an enum is a LEAF, so it declares no child element.
///
/// What makes this a *schema*, rather than a list of words a document may contain: an element
/// is judged against the children of the element it arrived IN, so a declared name in a
/// position the schema does not declare it in fails the same way an undeclared one does.
///
/// `pub`, with [`ANALYTICS_DOCUMENT_REQUIRED_ELEMENTS`]:
/// `tests/all/bucket_analytics_document_schema.rs` re-derives both from the model itself, so a
/// model update that renames, adds or removes an element of this closure cannot leave the
/// table behind.
pub const ANALYTICS_DOCUMENT_SCHEMA: &[(&str, &[&str])] = &[
    (
        "AnalyticsConfiguration",
        &["Id", "Filter", "StorageClassAnalysis"],
    ),
    ("Filter", &["Prefix", "Tag", "And"]),
    ("And", &["Prefix", "Tag"]),
    ("Tag", &["Key", "Value"]),
    ("StorageClassAnalysis", &["DataExport"]),
    ("DataExport", &["OutputSchemaVersion", "Destination"]),
    ("Destination", &["S3BucketDestination"]),
    (
        "S3BucketDestination",
        &["Format", "BucketAccountId", "Bucket", "Prefix"],
    ),
];

/// The children each of [`ANALYTICS_DOCUMENT_SCHEMA`]'s containers REQUIRES, one row per
/// child.
///
/// Source: the same shape closure, whose `required` members are `AnalyticsConfiguration$Id`
/// and `$StorageClassAnalysis`, `Tag$Key` and `$Value`,
/// `StorageClassAnalysisDataExport$OutputSchemaVersion` and `$Destination`,
/// `AnalyticsExportDestination$S3BucketDestination`, and `AnalyticsS3BucketDestination$Format`
/// and `$Bucket`. A container that names none of one of them does not validate against the
/// published schema, however well spelled the children it does name are.
///
/// `pub` for the reason [`ANALYTICS_DOCUMENT_SCHEMA`] is: the same test re-derives it from the
/// model.
pub const ANALYTICS_DOCUMENT_REQUIRED_ELEMENTS: &[(&str, &str)] = &[
    ("AnalyticsConfiguration", "Id"),
    ("AnalyticsConfiguration", STORAGE_CLASS_ANALYSIS_ELEMENT),
    ("Tag", "Key"),
    ("Tag", "Value"),
    ("DataExport", "OutputSchemaVersion"),
    ("DataExport", "Destination"),
    ("Destination", "S3BucketDestination"),
    ("S3BucketDestination", "Format"),
    ("S3BucketDestination", "Bucket"),
];

/// The element the analysis a configuration IS arrives in: `AnalyticsConfiguration`'s
/// `StorageClassAnalysis` member, under its own declared name (the model renames none of this
/// closure's members).
pub const STORAGE_CLASS_ANALYSIS_ELEMENT: &str = "StorageClassAnalysis";

/// The two tables above as the one schema a document is walked against.
const ANALYTICS_DOCUMENT: DocumentSchema = DocumentSchema {
    elements: ANALYTICS_DOCUMENT_SCHEMA,
    required: ANALYTICS_DOCUMENT_REQUIRED_ELEMENTS,
};

/// Why S3 would not record the analytics configuration a document describes, and how it
/// refuses it.
///
/// Each variant answers the three things S3's own refusal states: the wire code
/// ([`Self::code`]), the HTTP status ([`Self::status`]) and the sentence
/// ([`Self::message`]) — each of them FIXED text for that class of fault, with nothing of the
/// request in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnalyticsDocumentRejection {
    /// The document does not validate against the schema S3 publishes for it — including one
    /// that is not well-formed XML at all.
    MalformedDocument,
    /// The document is one the schema otherwise admits, and it names no
    /// [`STORAGE_CLASS_ANALYSIS_ELEMENT`]: the analysis the configuration IS
    /// (`AnalyticsConfiguration$StorageClassAnalysis` is `@required`), which S3 names in a
    /// refusal of its own rather than reporting the document as unreadable.
    MissingStorageClassAnalysis,
}

impl AnalyticsDocumentRejection {
    /// The S3 error code a put carrying such a document is refused with.
    pub fn code(self) -> &'static str {
        match self {
            Self::MalformedDocument => MALFORMED_XML_CODE,
            Self::MissingStorageClassAnalysis => MISSING_STORAGE_CLASS_ANALYSIS_CODE,
        }
    }

    /// The HTTP status that refusal is rendered with — 400 for both: the document is an
    /// argument of the request, and a request S3 cannot act on is a bad request.
    pub fn status(self) -> u16 {
        match self {
            Self::MalformedDocument => MALFORMED_XML_STATUS,
            Self::MissingStorageClassAnalysis => MISSING_STORAGE_CLASS_ANALYSIS_STATUS,
        }
    }

    /// The `<Message>` that refusal carries: the fixed sentence S3 publishes for the code,
    /// with nothing of the request in it.
    pub fn message(self) -> &'static str {
        match self {
            Self::MalformedDocument => MALFORMED_XML_MESSAGE,
            Self::MissingStorageClassAnalysis => MISSING_STORAGE_CLASS_ANALYSIS_MESSAGE,
        }
    }
}

/// What S3 refuses the analytics document `document` for, read from the DOCUMENT itself
/// rather than from the configuration parsed out of it, or `None` for a document it refuses
/// nothing about.
///
/// A readable document that names NO storage class analysis is the first thing judged,
/// because that container is what the configuration configures: S3 publishes a code of its
/// own for it, and answers it however else the document fails to validate. What is left — an
/// element the schema does not declare where it arrived, a container missing a required
/// child, a `Filter` that is not a choice of exactly one predicate, or bytes no XML reader
/// can walk — is the one schema refusal.
#[must_use]
pub fn analytics_document_rejection(document: &[u8]) -> Option<AnalyticsDocumentRejection> {
    let fault = document_fault(&ANALYTICS_DOCUMENT, document);
    if names_no_storage_class_analysis(document) == Some(true) {
        return Some(AnalyticsDocumentRejection::MissingStorageClassAnalysis);
    }
    if let Some(fault) = fault {
        return match fault {
            DocumentFault::MissingRequiredChild { child, .. }
                if child == STORAGE_CLASS_ANALYSIS_ELEMENT =>
            {
                Some(AnalyticsDocumentRejection::MissingStorageClassAnalysis)
            }
            _ => Some(AnalyticsDocumentRejection::MalformedDocument),
        };
    }
    filter_without_exactly_one_predicate(document)
        .then_some(AnalyticsDocumentRejection::MalformedDocument)
}

/// Whether `document` names NO [`STORAGE_CLASS_ANALYSIS_ELEMENT`] where the configuration
/// carries one — a child of the `AnalyticsConfiguration` the payload IS.
///
/// `None` for a document whose root is not that container or whose bytes no XML reader can
/// walk, which says nothing about the analysis either way.
///
/// The POSITION is what is read, not the name alone: a container spelled one level too deep
/// is one a reader finds no analysis in, the same as a document that names none at all.
fn names_no_storage_class_analysis(document: &[u8]) -> Option<bool> {
    let mut reader = ::quick_xml::Reader::from_reader(document);
    // How deep in the element tree the reader is: the payload's own container opens at one,
    // so the members of the configuration are the elements that open at two.
    let mut depth = 0usize;
    let mut named = false;
    // Whether the document has a root element at all: bytes that are not markup describe no
    // configuration, and so say nothing about its analysis.
    let mut rooted = false;
    loop {
        let (element, opened) = match reader.read_event() {
            Ok(::quick_xml::events::Event::Start(start)) => (start.name().as_ref().to_vec(), true),
            Ok(::quick_xml::events::Event::Empty(empty)) => (empty.name().as_ref().to_vec(), false),
            Ok(::quick_xml::events::Event::End(_)) => {
                depth = depth.saturating_sub(1);
                continue;
            }
            Ok(::quick_xml::events::Event::Eof) => return rooted.then_some(!named),
            // Not readable XML: nothing can be said about what it names.
            Err(_) => return None,
            Ok(_) => continue,
        };
        // The ROOT: a document whose payload container is another element describes no
        // analytics configuration at all, so its analysis is not what is wrong with it.
        if depth == 0 {
            if ANALYTICS_DOCUMENT.root().map(str::as_bytes) != Some(element.as_slice()) {
                return None;
            }
            rooted = true;
        }
        if depth == 1 && element == STORAGE_CLASS_ANALYSIS_ELEMENT.as_bytes() {
            named = true;
        }
        if opened {
            depth += 1;
        }
    }
}

/// Whether the `Filter` of the configuration `document` describes names anything other than
/// EXACTLY ONE predicate.
///
/// A filter is a CHOICE: "a filter must have exactly one prefix, one tag, or one conjunction
/// (AnalyticsAndOperator)" (`AnalyticsFilter`'s own documentation), so a container naming two
/// of them — or none at all — is not a filter of that shape, which is a document the
/// published schema does not validate. Read from the DOCUMENT because a codec binds a union
/// naming two members, or none, to the same ABSENT filter a document naming one unknown
/// element binds to, so what reaches a handler no longer says which it was.
fn filter_without_exactly_one_predicate(document: &[u8]) -> bool {
    let mut reader = ::quick_xml::Reader::from_reader(document);
    let mut depth = 0usize;
    // The depth the filter's own children arrive at while the reader is inside one, and how
    // many of them it has named.
    let mut inside: Option<usize> = None;
    let mut predicates = 0usize;
    loop {
        let (element, opened) = match reader.read_event() {
            Ok(::quick_xml::events::Event::Start(start)) => (start.name().as_ref().to_vec(), true),
            Ok(::quick_xml::events::Event::Empty(empty)) => (empty.name().as_ref().to_vec(), false),
            Ok(::quick_xml::events::Event::End(_)) => {
                if inside == Some(depth) {
                    if predicates != 1 {
                        return true;
                    }
                    inside = None;
                }
                depth = depth.saturating_sub(1);
                continue;
            }
            Ok(::quick_xml::events::Event::Eof) => return false,
            Err(_) => return false,
            Ok(_) => continue,
        };
        if inside == Some(depth) {
            predicates += 1;
        } else if depth == 1 && element == FILTER_ELEMENT.as_bytes() {
            // A self-closing `<Filter/>` names no predicate at all.
            if !opened {
                return true;
            }
            inside = Some(depth + 1);
            predicates = 0;
        }
        if opened {
            depth += 1;
        }
    }
}

/// The element a configuration's filter arrives in: `AnalyticsConfiguration`'s `Filter`
/// member, under its own declared name.
const FILTER_ELEMENT: &str = "Filter";
