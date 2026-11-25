// SPDX-License-Identifier: CC0-1.0

//! The schema a `PublicAccessBlockConfiguration` DOCUMENT has to validate against before any
//! of it is read, and how S3 refuses one that does not.
//!
//! `PutPublicAccessBlock`'s payload IS the configuration, so what the document names becomes
//! the bucket's Block Public Access posture — and a codec reads an element it does not know by
//! IGNORING it. Every member of this container is OPTIONAL, so a document that misspells,
//! misnests or invents an element parses to a configuration with the switch that element was
//! meant to carry left OFF, and there is nothing in the parsed input to say the caller ever
//! wrote it. Serving such a request would record a WEAKER posture than the document described
//! and report it back as the one in force; S3 answers instead that the document did not
//! validate against the published schema.
//!
//! Read from the BYTES the request carried (`crate::service::wire::request_document`), which
//! is the only place an element the codec ignored still exists, and walked by the one reading
//! of that question this service has ([`super::document_fault`]), so this document and every
//! other document of this service cannot drift apart on what "validates" means.

use super::*;

/// Every element the published `PublicAccessBlockConfiguration` schema declares, with the
/// elements each one may contain — the ROOT element first.
///
/// Source: the `PublicAccessBlockConfiguration` shape and the member that binds it
/// (`smithy_query(query="shape",shape="PublicAccessBlockConfiguration",
/// model_path="services/service-com-amazonaws-s3-amazons3/smithy-model/com-amazonaws-s3-amazons3.json")`
/// and `smithy_query(query="operation",operation="PutPublicAccessBlock")`): the ROOT is the
/// `@httpPayload` member's own `xmlName` (`PublicAccessBlockConfiguration`), and the four
/// switches are its members under their own `xmlName`s. Each targets the boolean `Setting`,
/// so each is a LEAF: it carries text, not elements.
///
/// What makes this a *schema*, rather than a list of words a document may contain: an element
/// is judged against the children of the element it arrived IN, so a declared name in a
/// position the schema does not declare it in fails the same way an undeclared one does.
///
/// `pub`: `tests/all/public_access_block_documents.rs` re-derives it from the model itself, so
/// a model update that renames, adds or removes one of these elements cannot leave the table
/// behind.
pub const PUBLIC_ACCESS_BLOCK_DOCUMENT_SCHEMA: &[(&str, &[&str])] = &[(
    "PublicAccessBlockConfiguration",
    &[
        "BlockPublicAcls",
        "IgnorePublicAcls",
        "BlockPublicPolicy",
        "RestrictPublicBuckets",
    ],
)];

/// The table above as the one schema a Block Public Access document is walked against.
///
/// It REQUIRES no child: "you can enable the configuration options in any combination"
/// (`PublicAccessBlockConfiguration`'s own documentation), and the model marks none of the
/// four members `@required`, so a container naming only some of them — or none — is a
/// container the published schema admits.
const PUBLIC_ACCESS_BLOCK_DOCUMENT: DocumentSchema = DocumentSchema {
    elements: PUBLIC_ACCESS_BLOCK_DOCUMENT_SCHEMA,
    required: &[],
};

/// Whether S3 would refuse the Block Public Access `document` for its SHAPE: an element the
/// published schema does not declare where it arrived, or bytes no XML reader can walk at all.
///
/// The refusal such a document earns is the one S3 answers every document that does not
/// validate — [`MALFORMED_XML_CODE`] at [`MALFORMED_XML_STATUS`], under that code's own FIXED
/// published sentence ([`MALFORMED_XML_MESSAGE`]), with nothing of the request in it — so this
/// reports WHETHER the document validates and the handler words the one refusal.
///
/// What a switch's TEXT says is NOT part of it: each of the four is a switch whose published
/// rule is that "setting this element to TRUE causes" a behaviour
/// (`smithy_query(query="shape",shape="PublicAccessBlockConfiguration")`), so text stating no
/// true leaves that switch OFF — the reading its own absence has — rather than making the
/// document one that does not validate (`crate::service::wire`).
#[must_use]
pub fn public_access_block_document_is_malformed(document: &[u8]) -> bool {
    document_fault(&PUBLIC_ACCESS_BLOCK_DOCUMENT, document).is_some()
}
