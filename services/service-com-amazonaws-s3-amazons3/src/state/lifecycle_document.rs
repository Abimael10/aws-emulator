// SPDX-License-Identifier: CC0-1.0

//! The schema a lifecycle configuration DOCUMENT has to validate against before any of it is
//! read, and how S3 refuses one that does not.
//!
//! `PutBucketLifecycleConfiguration`'s payload IS the configuration, so what the document
//! names becomes the stored rule set — and a codec reads an element it does not know by
//! IGNORING it, so a document that misspells, misnests or invents one parses to a rule
//! missing the member that element was meant to carry: a tag predicate the caller wrote
//! inside a container the schema does not declare reaches the handler as a rule with no tag
//! predicate at all, and a container missing a child the schema requires reaches it carrying
//! that member's default. S3 answers such a document that it did not validate against the
//! published schema rather than storing what was left of it.
//!
//! Read from the BYTES the request carried (`crate::service::wire::request_document`), which
//! is the only place an element the codec ignored still exists, and walked by the one reading
//! of that question this service has ([`super::document_fault`]), so the lifecycle document
//! and every other document of this service cannot drift apart on what "validates" means.

use super::*;

/// Every element the published lifecycle configuration schema declares, with the elements
/// each one may contain — the ROOT element first.
///
/// Source: the `BucketLifecycleConfiguration` shape closure
/// (`smithy_query(query="closure",operation="PutBucketLifecycleConfiguration",
/// model_path="services/service-com-amazonaws-s3-amazons3/smithy-model/com-amazonaws-s3-amazons3.json")`).
/// The ROOT is the payload member's own `xmlName` (`LifecycleConfiguration`), and each
/// structure's children are its members under the element name restXml spells them with —
/// their own declared names, except the four LISTS of the closure, which carry an `xmlName`
/// and are `@xmlFlattened`: `BucketLifecycleConfiguration$Rules` is spelled `Rule`,
/// `LifecycleRule$Transitions` `Transition`, `$NoncurrentVersionTransitions`
/// `NoncurrentVersionTransition`, and `LifecycleRuleAndOperator$Tags` `Tag`. A member whose
/// target is a string, a number, a boolean, a timestamp or an enum is a LEAF, so it declares
/// no child element.
///
/// What makes this a *schema*, rather than a list of words a document may contain: an element
/// is judged against the children of the element it arrived IN, so a declared name in a
/// position the schema does not declare it in fails the same way an undeclared one does.
///
/// `pub`, with [`LIFECYCLE_DOCUMENT_REQUIRED_ELEMENTS`]:
/// `tests/all/bucket_lifecycle_configuration.rs` re-derives both from the model itself, so a
/// model update that renames, adds or removes an element of this closure cannot leave the
/// tables behind.
pub const LIFECYCLE_DOCUMENT_SCHEMA: &[(&str, &[&str])] = &[
    ("LifecycleConfiguration", &["Rule"]),
    (
        "Rule",
        &[
            "Expiration",
            "ID",
            "Prefix",
            "Filter",
            "Status",
            "Transition",
            "NoncurrentVersionTransition",
            "NoncurrentVersionExpiration",
            "AbortIncompleteMultipartUpload",
        ],
    ),
    ("Expiration", &["Date", "Days", "ExpiredObjectDeleteMarker"]),
    (
        "Filter",
        &[
            "Prefix",
            "Tag",
            "ObjectSizeGreaterThan",
            "ObjectSizeLessThan",
            "And",
        ],
    ),
    (
        "And",
        &[
            "Prefix",
            "Tag",
            "ObjectSizeGreaterThan",
            "ObjectSizeLessThan",
        ],
    ),
    ("Tag", &["Key", "Value"]),
    ("Transition", &["Date", "Days", "StorageClass"]),
    (
        "NoncurrentVersionTransition",
        &["NoncurrentDays", "StorageClass", "NewerNoncurrentVersions"],
    ),
    (
        "NoncurrentVersionExpiration",
        &["NoncurrentDays", "NewerNoncurrentVersions"],
    ),
    ("AbortIncompleteMultipartUpload", &["DaysAfterInitiation"]),
];

/// The children each of [`LIFECYCLE_DOCUMENT_SCHEMA`]'s containers REQUIRES, one row per
/// child.
///
/// Source: the same shape closure, whose `required` members are
/// `BucketLifecycleConfiguration$Rules` (spelled `Rule`), `LifecycleRule$Status`, and
/// `Tag$Key` and `$Value`. A container that names none of one of them does not validate
/// against the published schema, however well spelled the children it does name are.
///
/// `pub` for the reason [`LIFECYCLE_DOCUMENT_SCHEMA`] is: the same test re-derives it from the
/// model.
pub const LIFECYCLE_DOCUMENT_REQUIRED_ELEMENTS: &[(&str, &str)] = &[
    ("LifecycleConfiguration", "Rule"),
    ("Rule", "Status"),
    ("Tag", "Key"),
    ("Tag", "Value"),
];

/// The two tables above as the one schema a lifecycle document is walked against.
const LIFECYCLE_DOCUMENT: DocumentSchema = DocumentSchema {
    elements: LIFECYCLE_DOCUMENT_SCHEMA,
    required: LIFECYCLE_DOCUMENT_REQUIRED_ELEMENTS,
};

/// Whether S3 would not store the lifecycle configuration `document` describes, for a document
/// whose SHAPE is what is wrong with it, or `None` for one the published schema admits.
///
/// The refusal is the one S3 answers every document that does not validate: the code whose
/// own sentence states both halves of the fault together, with nothing of the request in it
/// ([`LifecycleConfigurationRejection::MalformedDocument`]). What it covers — an element the
/// schema does not declare where it arrived, a container missing a required child, and bytes
/// no XML reader can walk — is the whole of what a parsed configuration can no longer say.
///
/// A TAG predicate naming no key is the one exception: a tag is "a key value name pair"
/// (`Tag`'s own documentation), so a predicate that names no key names a key of no characters
/// — which S3 reports against the published bounds on a tag key
/// ([`tag_key_bounds_message`]), the same refusal it answers a key the document spells empty
/// with, rather than as a document whose shape is wrong.
///
/// The FIRST fault in document order is what answers, as it does for every other document of
/// this service ([`document_fault`]), so a document whose keyless predicate comes first is
/// reported against those bounds even where a later element is misshapen too. Both are
/// refused; which of the two faults the caller is told about is the one the document reaches
/// first.
#[must_use]
pub fn lifecycle_document_rejection(document: &[u8]) -> Option<LifecycleConfigurationRejection> {
    match document_fault(&LIFECYCLE_DOCUMENT, document)? {
        DocumentFault::MissingRequiredChild { container, child }
            if (container, child) == (TAG_ELEMENT, TAG_KEY_ELEMENT) =>
        {
            None
        }
        _ => Some(LifecycleConfigurationRejection::MalformedDocument),
    }
}

/// The element a tag predicate arrives in, and the element its KEY arrives in: the `Tag`
/// shape and its `Key` member, under their own declared names.
const TAG_ELEMENT: &str = "Tag";
const TAG_KEY_ELEMENT: &str = "Key";
