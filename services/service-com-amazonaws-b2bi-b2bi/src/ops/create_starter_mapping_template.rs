// SPDX-License-Identifier: CC0-1.0

//! @op CreateStarterMappingTemplate
//! Generates a starter mapping template — a JSONata (JSON) document or an XSLT 1.0
//! stylesheet — for the X12 transaction set and version the request names, with the
//! values the request determines filled in and a PLACEHOLDER everywhere the caller must
//! supply an expression over its own input file.
//!
//! The operation mints no resource and persists nothing: its output is a single
//! `mappingTemplate` string, and the model gives a template no id and no
//! Get/List/Delete, so the template is a function of the request (and of the sample
//! object it names) and two identical requests answer with the identical template.
//!
//! ## Two sources for the template
//!
//! * WITHOUT `outputSampleLocation` the template comes from `templateDetails`: the X12
//!   envelope the requested transaction set and version determine, with the
//!   [`PLACEHOLDER`] value wherever the caller must supply an expression.
//! * WITH `outputSampleLocation` the template is the SAME document — "if you provide a
//!   sample EDI file with the same structure as the EDI files that you wish to generate,
//!   then the service can generate a mapping template" — carrying what that sample holds
//!   at each element position it describes: an ENVELOPE element takes the sample's value
//!   itself, and an element of a body segment takes the placeholder that CARRIES that
//!   value, [`SAMPLE_PLACEHOLDER_PREFIX`] followed by it ([`fill_from_sample`]). The object
//!   is read through the emulator's own Amazon S3 service and parsed as an X12 interchange
//!   ([`super::test_parsing::parse_x12`]), and only its element VALUES are read: the
//!   members the document holds and the element positions each segment asks for stay the
//!   requested transaction set's. Only a sample that genuinely does not resolve is the
//!   modelled `ResourceNotFoundException`.
//!
//! ## The generated document
//!
//! Both mapping types describe the same tree — an X12 interchange, holding a functional
//! group, holding a transaction:
//!
//! ```text
//! interchanges[0]                      ISA01..ISA15, the interchange header's data
//!   functional_groups[0]               GS01..GS08, the functional group header
//!     transactions[0]                  ST01..ST03, the transaction set header
//!       segments[0..]                   one object per segment of the transaction set
//! ```
//!
//! A template asks the caller for the fields a mapping SUPPLIES and for nothing an EDI
//! writer derives itself, so it carries no envelope TRAILER (IEA, GE and SE hold the count
//! of what the envelope encloses and the echo of its control number, which
//! `super::test_conversion` computes from the document) and no ISA16, which declares the
//! component element separator the interchange is written with rather than data in it.
//!
//! An envelope segment's elements sit directly in the object that holds the segment, keyed
//! `<SEGMENT>_<position>_<ElementName>` (`ST_01_TransactionSetIdentifierCode`). The
//! segments INSIDE the transaction sit in its `segments` list, one object each, its
//! elements keyed `<SEGMENT>_<position>` (`BEG_01`) — which is also what NAMES the segment,
//! as the X12 reader's `segment_id` marker does for a parsed document
//! ([`super::test_parsing::parse_x12`]), because a mapping written against this template is
//! evaluated into a document that reader's own output is compared with. A template
//! generated FOR A SAMPLE carries the same members, filled with what the sample holds: an
//! envelope element carries the sample's value, an element of a body segment the
//! [`SAMPLE_PLACEHOLDER_PREFIX`] placeholder carrying it, and a position the sample leaves
//! empty the bare [`PLACEHOLDER`] its table asks for. Only the reader's GROUPING of a
//! transaction set's repeating detail segment travels with those values — the sample's
//! `PO1-8000_loop` becomes the template's, one object per occurrence — so a caller maps the
//! second line item as the second entry of the loop; none of the reader's other members
//! (the `segment_id` of a segment, the envelope trailers it read) reaches a template.
//!
//! ## Shape of an operation
//!
//! Each operation is a struct that owns the shared engine and implements
//! [`smithy_server_runtime::Operation`]. The framework constructs one per service via
//! `CreateStarterMappingTemplate::new(engine)` and drives it through the centralized request walk:
//! deserialize → validate → `handle` → serialize. You write ONLY the `handle` body.
//!
//! ## Reaching this service's state
//!
//! The engine is held on `self` (constructor-injected), NOT passed per request — so
//! engine access is guaranteed by the time any request runs. Reach the store through
//! [`crate::state::service_state`] and read and write it through the guard it hands back:
//!
//! ```ignore
//!     let state = crate::state::service_state(&*self.engine)
//!         .ok_or(/* a modelled server error */)?;
//!     state.lock().your_field = /* ... */;
//! ```
//!
//! `ctx` ([`smithy_server_runtime::OperationContext`]) carries request metadata only —
//! headers, URI, method, and anything middleware placed in `ctx.parts().extensions`.
//! The engine is NOT reached through `ctx`.
//!
//! @generated by the emulator core scaffold op-stub generator.
//! Shared types live in `super::types`; the error channel in `crate::scaffolded`.
//!
//! ---
//!
//! # Modeled contract for `CreateStarterMappingTemplate`
//!
//! What AWS documents for this operation, from this service's Smithy model. HTML is
//! stripped and long entries are clipped at ` …`. For the FULL text of any one item,
//! ask the model rather than reading the raw JSON:
//!
//! ```text
//! smithy_query(query="doc",shape="<Shape>",member="<Member>",model_path="services/service-com-amazonaws-b2bi-b2bi/smithy-model/com-amazonaws-b2bi-b2bi.json")
//! ```
//!
//! Amazon Web Services B2B Data Interchange uses a mapping template in JSONata or XSLT
//! format to transform a customer input file into a JSON or XML file that can be converted
//! to EDI. If you provide a sample EDI file with the same structure as the EDI files that
//! you wish to generate, then the service can generate a mapping template. The starter
//! template contains placeholder values which you can replace with JSONata or XSLT
//! expressions to take data from your input file and insert it into the JSON or XML file
//! that is used to generate the EDI. If you do not provide a sample EDI file, then the
//! service can generate a mapping template based on the EDI settings in the templateDetails
//! parameter. Currently, we only support generating a template that can generate the input
//! to produce an Outbound X12 EDI file.
//!
//! ## Input
//!
//! * `mappingType` (MappingType, one of: JSONATA | XSLT) **required** — Specify the format
//!   for the mapping template: either JSONATA or XSLT.
//! * `outputSampleLocation` (S3Location) — Specify the location of the sample EDI file that
//!   is used to generate the mapping template.
//! * `templateDetails` (TemplateDetails) **required** — Describes the details needed for
//!   generating the template. Specify the X12 transaction set and version for which the
//!   template is used: currently, we only support X12.
//!
//! ## Output
//!
//! * `mappingTemplate` (String) **required** — Returns a string that represents the mapping
//!   template.
//!
//! ## Errors
//!
//! * `AccessDeniedException` (HTTP 403) — You do not have sufficient access to perform this
//!   action.
//! * `InternalServerException` (HTTP 500) — This exception is thrown when an error occurs in
//!   the Amazon Web Services B2B Data Interchange service.
//! * `ResourceNotFoundException` (HTTP 404) — Occurs when the requested resource does not
//!   exist, or cannot be found. In some cases, the resource exists in a region other than the
//!   region specified in the API call.
//! * `ValidationException` (HTTP 400) — Occurs when a B2BI object cannot be validated against
//!   a request from another object. This exception can be thrown during standard EDI
//!   validation or when custom validation rules fail, such as when element length constraints
//!   are violated, invalid codes are used in code list validations, or required elements are
//!   missing based on configured element requirement rules.

#[allow(unused_imports)]
use super::types::*;

// `#[operation_error]` below expands to an invocation of the LOCAL declarative
// `smithy!` macro, which this line is what emits into the module. Without it the
// expansion fails with `cannot find macro 'smithy' in this scope` and the
// `CreateStarterMappingTemplateError` enum is never produced.
smithy_macros::smithy_model!("smithy-model/com-amazonaws-b2bi-b2bi.json");

// The operation's MODELED errors. `#[operation_error]` accepts only variants the
// model declares for this operation, so the scaffold's `NotImplemented` cannot live
// here — hence the `CreateStarterMappingTemplateError` alias below. An operation that models no errors
// generates an UNINHABITED enum here, which is correct and expected: there is nothing
// to construct until the model says otherwise.
#[smithy_macros::operation_error("com.amazonaws.b2bi#CreateStarterMappingTemplate")]
#[derive(Debug)]
pub enum CreateStarterMappingTemplateErrorModeled {
    AccessDeniedException(AccessDeniedException),
    InternalServerException(InternalServerException),
    ResourceNotFoundException(ResourceNotFoundException),
    ValidationException(ValidationException),
}

/// This operation's error channel, as the generated `impl Operation` names it
/// (`crate::ops::create_starter_mapping_template::CreateStarterMappingTemplateError`). Wrapping the modeled enum is what
/// lets a scaffolded operation answer 501 for one whose model declares no errors at all.
pub type CreateStarterMappingTemplateError = crate::scaffolded::Scaffolded<CreateStarterMappingTemplateErrorModeled>;

/// Lets `?` and `.into()` lift a modeled failure into the error channel, so an
/// implemented handler writes `Err(SomeModeledError.into())` rather than naming the
/// wrapper variant. See `crate::scaffolded` for how to drop the wrapper entirely once
/// this operation is implemented.
impl From<CreateStarterMappingTemplateErrorModeled> for CreateStarterMappingTemplateError {
    fn from(modeled: CreateStarterMappingTemplateErrorModeled) -> Self {
        crate::scaffolded::Scaffolded::Modeled(modeled)
    }
}

/// The format of the document a request asks for, decoded from the modelled `MappingType`
/// value — the crate's `MAPPING_TYPE_VALUES`, the member list of
/// `smithy_query(query="shape",shape="MappingType")`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MappingFormat {
    /// A JSON document whose values the caller replaces with JSONata expressions.
    Jsonata,
    /// A stylesheet whose result tree is the file the EDI is generated from.
    Xslt,
}

impl MappingFormat {
    /// Every format this operation generates, by the modelled `MappingType` value that
    /// asks for it. Kept beside [`MappingFormat::decode`] so a refusal lists exactly the
    /// values `decode` accepts; the crate's integration test asserts this covers every
    /// member of the model's own `MAPPING_TYPE_VALUES`.
    const BY_MAPPING_TYPE: &'static [(&'static str, MappingFormat)] = &[
        ("JSONATA", MappingFormat::Jsonata),
        ("XSLT", MappingFormat::Xslt),
    ];

    /// The format `value` names, or `None` when it is not a modelled `MappingType`.
    fn decode(value: &str) -> Option<Self> {
        Self::BY_MAPPING_TYPE
            .iter()
            .find(|(mapping_type, _)| *mapping_type == value)
            .map(|(_, format)| *format)
    }

    /// The `MappingType` values this operation accepts, for a refusal message.
    fn accepted() -> String {
        Self::BY_MAPPING_TYPE
            .iter()
            .map(|(mapping_type, _)| *mapping_type)
            .collect::<Vec<&str>>()
            .join(", ")
    }
}

/// The longest template this operation answers with: the modelled `MappingTemplate` bound
/// (`len 0..350000`), the member `TestMapping`, `CreateTransformer` and `UpdateTransformer`
/// accept a template in. A template past it could not be used with the operations it is
/// generated for, so the request is refused instead of answered with one.
const MAX_TEMPLATE_CHARS: usize = 350_000;

/// The XSLT namespace every generated stylesheet declares.
const XSL_NAMESPACE: &str = "http://www.w3.org/1999/XSL/Transform";

/// The stylesheet version every generated stylesheet declares: XSLT 1.0, the version
/// every XSLT processor implements.
const XSL_VERSION: &str = "1.0";

/// GS07, the Responsible Agency Code carried by every X12 functional group: `X`, the
/// Accredited Standards Committee X12. Fixed because this operation generates X12
/// templates only — "currently, we only support generating a template that can generate
/// the input to produce an Outbound X12 EDI file".
const X12_RESPONSIBLE_AGENCY_CODE: &str = "X";

/// The X12 envelope element names, the document's member names and the release ISA11
/// changes meaning at all belong to this service's X12 READER (`super::test_parsing`),
/// which is the module that owns what an X12 document is made of: a starter template
/// describes the very document that reader builds, so the two cannot disagree about the
/// name of an element or of a member.
use super::test_parsing::{
    isa_element_names, FUNCTIONAL_GROUPS_KEY, FUNCTIONAL_GROUP_ITEM, GS_ELEMENTS,
    INTERCHANGES_KEY, INTERCHANGE_ITEM, SEGMENTS_KEY, SEGMENT_ITEM, ST_ELEMENTS, TRANSACTIONS_KEY,
    TRANSACTION_ITEM,
};

/// The value a starter template carries wherever the caller must supply the content:
/// "the starter template contains placeholder values which you can replace with JSONata
/// or XSLT expressions to take data from your input file".
///
/// One fixed word, carrying neither the element's name nor the expression language nor any
/// value: it is what a generated template reads for every such field on both paths, so a
/// caller recognises a field it must fill the same way whether or not it named a sample.
pub const PLACEHOLDER: &str = "PLACEHOLDER";

/// What a template carries at an element position of a body segment the named sample FILLS:
/// the [`PLACEHOLDER`] the caller replaces, a colon and a space, and then the value the
/// sample holds at that position ([`sampled_placeholder`]) — so a caller reading the
/// template sees WHICH field of its own input belongs at that position, which is the point
/// of supplying a sample, and still recognises the field as one it must fill.
///
/// The prefix is fixed; everything after it comes from the sample. An envelope element is
/// not prefixed: it carries the sample's value itself, because the envelope of the
/// interchange a mapping generates is the sample's envelope.
pub const SAMPLE_PLACEHOLDER_PREFIX: &str = "PLACEHOLDER: ";

/// The placeholder for a body element position the sample holds `value` at.
fn sampled_placeholder(value: &str) -> String {
    format!("{SAMPLE_PLACEHOLDER_PREFIX}{value}")
}

/// One node of a starter template's document tree, rendered to JSON for a `JSONATA`
/// template and to XML for an `XSLT` one.
///
/// It mirrors the tree a PARSED X12 interchange takes ([`super::test_parsing::Node`], the
/// reader's own): the keys a starter template carries placeholders under are the keys a
/// parsed interchange carries values under, which is what lets
/// [`fill_from_sample`] read a sample's value for the element a template key
/// names.
///
/// A map keeps its members in INSERTION order — the positional order of an X12 segment's
/// elements — which a `serde_json::Value` (a sorted map) would lose.
#[derive(Clone)]
enum Node {
    /// A leaf: either a value the request determines or a placeholder.
    Text(String),
    /// Named children, in order.
    Map(Vec<(String, Node)>),
    /// A repeatable child — an interchange, a functional group, a transaction set. `item`
    /// is the element name each repetition takes in the XML rendering; JSON renders the
    /// repetitions as an array.
    List { item: String, items: Vec<Node> },
}

impl ::serde::Serialize for Node {
    fn serialize<S: ::serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Node::Text(text) => serializer.serialize_str(text),
            Node::Map(members) => {
                serializer.collect_map(members.iter().map(|(key, child)| (key.as_str(), child)))
            }
            Node::List { items, .. } => serializer.collect_seq(items.iter()),
        }
    }
}

/// The modelled server error for a template that could not be rendered.
fn render_failed<E: ::std::fmt::Display>(error: E) -> CreateStarterMappingTemplateError {
    CreateStarterMappingTemplateErrorModeled::InternalServerException(InternalServerException {
        message: format!("the starter mapping template could not be rendered: {error}"),
        retry_after_seconds: None,
    })
    .into()
}

/// An X12 element's key in a template: the segment id, the element's two-digit position
/// within that segment and the element's name — `ST_01_TransactionSetIdentifierCode`, the
/// naming B2BI's X12 documents use.
///
/// A segment's elements sit DIRECTLY in the object that holds the segment (an interchange
/// carries its own ISA elements, a functional group its GS elements, a transaction its ST
/// elements), so the segment and position prefix is also what keeps two same-named
/// elements apart — ISA05 and ISA07 are both the Interchange ID Qualifier.
fn element_key(segment: &str, position: usize, name: &str) -> String {
    format!("{segment}_{position:02}_{name}")
}

/// The key of an element of a segment INSIDE the transaction: the segment id and the
/// element's two-digit position alone (`BEG_01`), because the X12 schema of the
/// transaction set — not the envelope — names those elements.
fn body_element_key(segment: &str, position: usize) -> String {
    format!("{segment}_{position:02}")
}

/// A transaction set's beginning segment: the segment the X12 standard puts first in
/// that transaction set's heading table, right after ST.
struct BeginningSegment {
    /// The X12 segment id — `BEG` for a purchase order.
    id: &'static str,
    /// The segment's leading elements, in positional order, by name.
    elements: &'static [&'static str],
}

/// The beginning segment of each X12 transaction set a starter template carries a body
/// for, by the transaction set code the request names.
///
/// Source: the heading table of each named transaction set in the ASC X12 standard — the
/// segment that follows ST and its leading elements. A transaction set that is not listed
/// gets the envelope and the ST/SE pair alone, because this emulator has no segment table
/// for it; the template is a STARTER either way, so the segments it does not carry are
/// the ones the caller adds.
const BEGINNING_SEGMENTS: &[(&str, BeginningSegment)] = &[
    (
        // 850 Purchase Order
        "850",
        BeginningSegment {
            id: "BEG",
            elements: &[
                "TransactionSetPurposeCode",
                "PurchaseOrderTypeCode",
                "PurchaseOrderNumber",
                "ReleaseNumber",
                "Date",
                "ContractNumber",
                "AcknowledgmentType",
                "InvoiceTypeCode",
                "ContractTypeCode",
                "SecurityLevelCode",
                "TransactionTypeCode",
                "PurchaseCategory",
            ],
        },
    ),
    (
        // 855 Purchase Order Acknowledgment
        "855",
        BeginningSegment {
            id: "BAK",
            elements: &[
                "TransactionSetPurposeCode",
                "AcknowledgmentType",
                "PurchaseOrderNumber",
                "Date",
                "ReleaseNumber",
                "RequestReferenceNumber",
                "ContractNumber",
            ],
        },
    ),
    (
        // 860 Purchase Order Change Request
        "860",
        BeginningSegment {
            id: "BCH",
            elements: &[
                "TransactionSetPurposeCode",
                "PurchaseOrderTypeCode",
                "PurchaseOrderNumber",
                "ReleaseNumber",
                "ChangeOrderSequenceNumber",
                "Date",
            ],
        },
    ),
    (
        // 865 Purchase Order Change Acknowledgment
        "865",
        BeginningSegment {
            id: "BCA",
            elements: &[
                "TransactionSetPurposeCode",
                "AcknowledgmentType",
                "PurchaseOrderNumber",
                "ReleaseNumber",
                "ChangeOrderSequenceNumber",
                "Date",
            ],
        },
    ),
    (
        // 810 Invoice
        "810",
        BeginningSegment {
            id: "BIG",
            elements: &[
                "Date",
                "InvoiceNumber",
                "PurchaseOrderDate",
                "PurchaseOrderNumber",
                "ReleaseNumber",
                "ChangeOrderSequenceNumber",
                "TransactionTypeCode",
                "TransactionSetPurposeCode",
            ],
        },
    ),
    (
        // 856 Ship Notice/Manifest
        "856",
        BeginningSegment {
            id: "BSN",
            elements: &[
                "TransactionSetPurposeCode",
                "ShipmentIdentification",
                "Date",
                "Time",
                "HierarchicalStructureCode",
                "TransactionTypeCode",
            ],
        },
    ),
    (
        // 846 Inventory Inquiry/Advice
        "846",
        BeginningSegment {
            id: "BIA",
            elements: &[
                "TransactionSetPurposeCode",
                "ReportTypeCode",
                "ReferenceIdentification",
                "Date",
                "Time",
            ],
        },
    ),
    (
        // 852 Product Activity Data
        "852",
        BeginningSegment {
            id: "XQ",
            elements: &["ReportingCode", "Date", "Date"],
        },
    ),
    (
        // 864 Text Message
        "864",
        BeginningSegment {
            id: "BMG",
            elements: &["TransactionSetPurposeCode", "Description", "MessageTypeCode"],
        },
    ),
    (
        // 867 Product Transfer and Resale Report
        "867",
        BeginningSegment {
            id: "BPT",
            elements: &[
                "TransactionSetPurposeCode",
                "ReferenceIdentification",
                "Date",
                "TransactionTypeCode",
            ],
        },
    ),
    (
        // 820 Payment Order/Remittance Advice
        "820",
        BeginningSegment {
            id: "BPR",
            elements: BPR_ELEMENTS,
        },
    ),
    (
        // 835 Health Care Claim Payment/Advice
        "835",
        BeginningSegment {
            id: "BPR",
            elements: BPR_ELEMENTS,
        },
    ),
    (
        // 824 Application Advice
        "824",
        BeginningSegment {
            id: "BGN",
            elements: BGN_ELEMENTS,
        },
    ),
    (
        // 834 Benefit Enrollment and Maintenance
        "834",
        BeginningSegment {
            id: "BGN",
            elements: BGN_ELEMENTS,
        },
    ),
    (
        // 270 Eligibility, Coverage or Benefit Inquiry
        "270",
        BeginningSegment {
            id: "BHT",
            elements: BHT_ELEMENTS,
        },
    ),
    (
        // 271 Eligibility, Coverage or Benefit Information
        "271",
        BeginningSegment {
            id: "BHT",
            elements: BHT_ELEMENTS,
        },
    ),
    (
        // 276 Health Care Claim Status Request
        "276",
        BeginningSegment {
            id: "BHT",
            elements: BHT_ELEMENTS,
        },
    ),
    (
        // 277 Health Care Claim Status Response
        "277",
        BeginningSegment {
            id: "BHT",
            elements: BHT_ELEMENTS,
        },
    ),
    (
        // 278 Health Care Services Review Information
        "278",
        BeginningSegment {
            id: "BHT",
            elements: BHT_ELEMENTS,
        },
    ),
    (
        // 837 Health Care Claim
        "837",
        BeginningSegment {
            id: "BHT",
            elements: BHT_ELEMENTS,
        },
    ),
    (
        // 214 Transportation Carrier Shipment Status Message
        "214",
        BeginningSegment {
            id: "B10",
            elements: &[
                "ReferenceIdentification",
                "ShipmentIdentificationNumber",
                "StandardCarrierAlphaCode",
            ],
        },
    ),
    (
        // 940 Warehouse Shipping Order
        "940",
        BeginningSegment {
            id: "W05",
            elements: &["ReportingCode", "DepositorOrderNumber", "PurchaseOrderNumber"],
        },
    ),
    (
        // 945 Warehouse Shipping Advice
        "945",
        BeginningSegment {
            id: "W06",
            elements: &[
                "ReportingCode",
                "DepositorOrderNumber",
                "Date",
                "ShipmentIdentificationNumber",
            ],
        },
    ),
    (
        // 997 Functional Acknowledgment
        "997",
        BeginningSegment {
            id: "AK1",
            elements: AK1_ELEMENTS,
        },
    ),
    (
        // 999 Implementation Acknowledgment
        "999",
        BeginningSegment {
            id: "AK1",
            elements: AK1_ELEMENTS,
        },
    ),
];

/// BHT, the Beginning of Hierarchical Transaction segment the HIPAA transaction sets open
/// with.
const BHT_ELEMENTS: &[&str] = &[
    "HierarchicalStructureCode",
    "TransactionSetPurposeCode",
    "ReferenceIdentification",
    "Date",
    "Time",
    "TransactionTypeCode",
];

/// BGN, the Beginning Segment.
const BGN_ELEMENTS: &[&str] = &[
    "TransactionSetPurposeCode",
    "ReferenceIdentification",
    "Date",
    "Time",
    "TimeCode",
    "OriginalReferenceNumber",
    "TransactionTypeCode",
    "ActionCode",
    "SecurityLevelCode",
];

/// BPR, the Beginning Segment for Payment Order/Remittance Advice.
const BPR_ELEMENTS: &[&str] = &[
    "TransactionHandlingCode",
    "MonetaryAmount",
    "CreditDebitFlagCode",
    "PaymentMethodCode",
    "PaymentFormatCode",
    "DfiIdNumberQualifier",
    "DfiIdentificationNumber",
    "AccountNumberQualifier",
    "AccountNumber",
    "OriginatingCompanyIdentifier",
];

/// AK1, the Functional Group Response Header an acknowledgment opens with.
const AK1_ELEMENTS: &[&str] = &[
    "FunctionalIdentifierCode",
    "GroupControlNumber",
    "VersionReleaseIndustryIdentifierCode",
];

/// The beginning segment [`BEGINNING_SEGMENTS`] holds for `transaction_set_code`, if any.
fn beginning_segment(transaction_set_code: &str) -> Option<&'static BeginningSegment> {
    BEGINNING_SEGMENTS
        .iter()
        .find(|(code, _)| *code == transaction_set_code)
        .map(|(_, segment)| segment)
}

/// A segment of a transaction set's body that follows its beginning segment: the X12
/// segment id and the number of data elements the segment carries.
///
/// A body element is keyed by its POSITION alone ([`body_element_key`]), so the COUNT is
/// everything a template needs of such a segment — unlike an envelope segment, whose
/// elements are keyed by name as well.
struct BodySegment {
    /// The X12 segment id — `REF` for a reference identification.
    id: &'static str,
    /// How many data elements the ASC X12 segment directory gives that segment.
    elements: usize,
}

/// One entry of a transaction set's table.
const fn body(id: &'static str, elements: usize) -> BodySegment {
    BodySegment { id, elements }
}

/// How many elements the CUR (Currency) segment carries in a generated template.
///
/// Read off the recorded template rather than off the X12 segment directory, which gives
/// CUR twenty-one elements: the recorded purchase-order template carries `CUR_01` through
/// `CUR_09`, so nine is what a caller writing a mapping against this template addresses.
const CUR_ELEMENTS: usize = 9;

/// The segments of a transaction set's body that FOLLOW its beginning segment, in the order
/// the ASC X12 standard's table for that transaction set places them: the reference,
/// contact, date and name segments of its heading table, the segment its detail table
/// repeats, and the totals of its summary table.
///
/// Source: the heading, detail and summary tables of each named transaction set in the ASC
/// X12 standard, and the element count the X12 segment directory gives each segment, except
/// for the currency segment: the recorded template of a purchase order carries CUR with NINE
/// elements (`CUR_01`..`CUR_09`) where the directory gives it twenty-one, so that count is
/// taken from the recorded document ([`CUR_ELEMENTS`]). A transaction set this emulator
/// carries no table for is generated with its beginning segment alone; the template is a
/// STARTER either way, so the segments it does not carry are the ones the caller adds.
const BODY_SEGMENTS: &[(&str, &[BodySegment])] = &[
    (
        // 850 Purchase Order: the baseline item segment repeats in its detail table.
        "850",
        &[
            body("CUR", CUR_ELEMENTS),
            body("REF", 4),
            body("PER", 9),
            body("DTM", 6),
            body("N1", 6),
            body("N2", 2),
            body("N3", 2),
            body("N4", 7),
            body("PO1", 25),
            body("PID", 9),
            body("CTT", 7),
        ],
    ),
    (
        // 855 Purchase Order Acknowledgment: the same item segment, acknowledged.
        "855",
        &[
            body("CUR", CUR_ELEMENTS),
            body("REF", 4),
            body("PER", 9),
            body("DTM", 6),
            body("N1", 6),
            body("N2", 2),
            body("N3", 2),
            body("N4", 7),
            body("PO1", 25),
            body("PID", 9),
            body("CTT", 7),
        ],
    ),
    (
        // 860 Purchase Order Change Request.
        "860",
        &[
            body("CUR", CUR_ELEMENTS),
            body("REF", 4),
            body("PER", 9),
            body("DTM", 6),
            body("N1", 6),
            body("N2", 2),
            body("N3", 2),
            body("N4", 7),
            body("CTT", 7),
        ],
    ),
    (
        // 865 Purchase Order Change Acknowledgment.
        "865",
        &[
            body("CUR", CUR_ELEMENTS),
            body("REF", 4),
            body("PER", 9),
            body("DTM", 6),
            body("N1", 6),
            body("N2", 2),
            body("N3", 2),
            body("N4", 7),
            body("CTT", 7),
        ],
    ),
    (
        // 810 Invoice: the baseline item data segment and the total monetary value summary.
        "810",
        &[
            body("CUR", CUR_ELEMENTS),
            body("REF", 4),
            body("PER", 9),
            body("DTM", 6),
            body("N1", 6),
            body("N2", 2),
            body("N3", 2),
            body("N4", 7),
            body("IT1", 25),
            body("PID", 9),
            body("TDS", 4),
            body("CTT", 7),
        ],
    ),
    (
        // 856 Ship Notice/Manifest: the hierarchical level segment and the item it carries.
        "856",
        &[
            body("DTM", 6),
            body("REF", 4),
            body("HL", 4),
            body("LIN", 31),
            body("N1", 6),
            body("N2", 2),
            body("N3", 2),
            body("N4", 7),
            body("CTT", 7),
        ],
    ),
    (
        // 846 Inventory Inquiry/Advice: the item identification segment.
        "846",
        &[
            body("REF", 4),
            body("DTM", 6),
            body("N1", 6),
            body("N2", 2),
            body("N3", 2),
            body("N4", 7),
            body("LIN", 31),
            body("CTT", 7),
        ],
    ),
    (
        // 997 Functional Acknowledgment: the transaction set response header and the
        // functional group response trailer.
        "997",
        &[body("AK2", 3), body("AK9", 9)],
    ),
    (
        // 999 Implementation Acknowledgment: the same pair.
        "999",
        &[body("AK2", 3), body("AK9", 9)],
    ),
];

/// Every segment of the body of `transaction_set_code`, in the order its X12 table places
/// them: the beginning segment, then the segments [`BODY_SEGMENTS`] holds for it.
fn transaction_set_segments(transaction_set_code: &str) -> Vec<BodySegment> {
    let mut segments: Vec<BodySegment> = beginning_segment(transaction_set_code)
        .map(|segment| vec![body(segment.id, segment.elements.len())])
        .unwrap_or_default();
    if let Some((_, following)) = BODY_SEGMENTS
        .iter()
        .find(|(code, _)| *code == transaction_set_code)
    {
        segments.extend(following.iter().map(|segment| body(segment.id, segment.elements)));
    }
    segments
}

/// The value a starter template carries wherever the caller must supply the content.
fn placeholder() -> Node {
    Node::Text(PLACEHOLDER.to_string())
}

/// One segment INSIDE the transaction — the transaction set's own body, which the X12
/// schema of that transaction set defines — as the object a `segments` list holds.
///
/// It carries one placeholder per element, keyed by segment id and position alone
/// (`BEG_01`) rather than by element name the way an envelope segment's elements are: the
/// segment is NAMED by each of its element keys, so an object that carries the elements
/// carries the segment, and that is the only member a generated template writes here.
fn body_segment(segment: &BodySegment) -> Node {
    Node::Map(
        (1..=segment.elements)
            .map(|position| (body_element_key(segment.id, position), placeholder()))
            .collect(),
    )
}

/// The interchange control header (ISA) as a template carries it: its fifteen DATA
/// elements, each one the value the X12 standard or the request fixes for it
/// ([`isa_element_value`]) and the placeholder wherever the element carries the caller's
/// own data (the trading parties, the interchange's date and time).
///
/// ISA16 is left out: it declares the component element SEPARATOR the interchange is written
/// with, a delimiter of the file rather than data in it, so a template that asked the caller
/// for an expression there would describe a document whose ISA16 is not a delimiter at all
/// — which this service's X12 writer refuses (`super::test_conversion`).
fn isa_elements(release: u32, version_release_industry_code: &str) -> Vec<(String, Node)> {
    let names = isa_element_names(release);
    // Every element but the component element separator, taken out BY NAME rather than by
    // position, so a reader whose element list changes still has its data elements written
    // and this still omits the one delimiter. It is the LAST element of the header, so the
    // positions of the ones kept are the positions they are keyed by.
    let data_elements: Vec<&str> = names
        .into_iter()
        .filter(|name| *name != COMPONENT_SEPARATOR_ELEMENT)
        .collect();
    data_elements
        .iter()
        .enumerate()
        .map(|(index, name)| {
            let position = index + 1;
            let element = match isa_element_value(position, name, version_release_industry_code) {
                Some(value) => Node::Text(value),
                None => placeholder(),
            };
            (element_key("ISA", position, name), element)
        })
        .collect()
}

/// The value of the interchange header element at `position`, or `None` when the element
/// carries the caller's own data and so stays a placeholder.
///
/// Source: the ASC X12 interchange control header, whose element code lists fix the value of
/// every element a generated interchange does not take from its trading parties — the
/// qualifier that says no authorization or security information is present (and the
/// fixed-width blank that goes with it), the control standards identifier, the interchange
/// control version number the requested release names, whether an acknowledgment is asked
/// for, and whether the interchange is a production one.
fn isa_element_value(position: usize, name: &str, version_code: &str) -> Option<String> {
    match position {
        1 | 3 => Some(NO_INFORMATION_QUALIFIER.to_string()),
        // ISA11 is the control standards identifier up to the release where the repetition
        // separator takes its position; a separator is a delimiter of the file, so from
        // that release on the element is left to the writer the way ISA16 is.
        11 if name == INTERCHANGE_CONTROL_STANDARDS_ELEMENT => {
            Some(INTERCHANGE_CONTROL_STANDARDS_ID.to_string())
        }
        // The interchange control version number: the five-digit form of the version X12
        // carries in GS08 (`004010` → `00401`).
        12 => Some(
            version_code
                .chars()
                .take(INTERCHANGE_VERSION_CHARS)
                .collect(),
        ),
        13 => Some(FIRST_INTERCHANGE_CONTROL_NUMBER.to_string()),
        14 => Some(NO_ACKNOWLEDGMENT_REQUESTED.to_string()),
        15 => Some(PRODUCTION_TEST_INDICATOR.to_string()),
        _ => None,
    }
}

/// ISA01 and ISA03, the qualifiers of the interchange header: `00`, no authorization and no
/// security information is present — the authorization and security information elements
/// themselves (ISA02 and ISA04) stay the caller's, the way every other element a mapping
/// supplies does.
const NO_INFORMATION_QUALIFIER: &str = "00";

/// The name the X12 reader gives ISA11 up to the release where the repetition separator
/// takes that position, which is where the element is data rather than a delimiter.
const INTERCHANGE_CONTROL_STANDARDS_ELEMENT: &str = "InterchangeControlStandardsId";

/// ISA11, the interchange control standards identifier: `U`, the U.S. EDI community of ASC
/// X12, TDCC and UCS.
const INTERCHANGE_CONTROL_STANDARDS_ID: &str = "U";

/// How many characters of the version/release code ISA12, the interchange control version
/// number, carries.
const INTERCHANGE_VERSION_CHARS: usize = 5;

/// ISA14: no interchange acknowledgment is requested for the interchange a template
/// describes.
const NO_ACKNOWLEDGMENT_REQUESTED: &str = "0";

/// ISA15: the interchange a template describes is a production one, not a test.
const PRODUCTION_TEST_INDICATOR: &str = "P";

/// ISA13, the interchange control number of the ONE interchange a template describes: the
/// first one, at the nine-digit fixed width X12 gives the element.
const FIRST_INTERCHANGE_CONTROL_NUMBER: &str = "000000001";

/// GS06, the group control number of the ONE functional group a template describes.
const FIRST_GROUP_CONTROL_NUMBER: &str = "1";

/// ST02, the transaction set control number of the ONE transaction set a template
/// describes: the first one, in the six-digit form a generated template carries it in.
const FIRST_TRANSACTION_SET_CONTROL_NUMBER: &str = "000001";

/// The name the X12 reader gives ISA16, the element that declares the component element
/// separator the interchange is written with — the one element of the interchange header a
/// template leaves out ([`isa_elements`]).
const COMPONENT_SEPARATOR_ELEMENT: &str = "ComponentSeparator";

/// The functional group header (GS): GS01 is the functional identifier code of the group the
/// requested transaction set is sent in ([`FUNCTIONAL_IDENTIFIER_CODES`]), GS06 the group's
/// control number, GS07 the fixed [`X12_RESPONSIBLE_AGENCY_CODE`] and GS08 the
/// version/release/industry identifier code the request determines; the application sender
/// and receiver codes and the group's date and time are the caller's.
fn gs_elements(
    transaction_set_code: &str,
    version_release_industry_code: &str,
) -> Vec<(String, Node)> {
    GS_ELEMENTS
        .iter()
        .enumerate()
        .map(|(index, name)| {
            let position = index + 1;
            let element = match gs_element_value(
                position,
                transaction_set_code,
                version_release_industry_code,
            ) {
                Some(value) => Node::Text(value),
                None => placeholder(),
            };
            (element_key("GS", position, name), element)
        })
        .collect()
}

/// The value of the functional group header element at `position`, or `None` when the
/// element carries the caller's own data and so stays a placeholder.
fn gs_element_value(
    position: usize,
    transaction_set_code: &str,
    version_release_industry_code: &str,
) -> Option<String> {
    match position {
        1 => functional_identifier_code(transaction_set_code).map(str::to_string),
        6 => Some(FIRST_GROUP_CONTROL_NUMBER.to_string()),
        7 => Some(X12_RESPONSIBLE_AGENCY_CODE.to_string()),
        8 => Some(version_release_industry_code.to_string()),
        _ => None,
    }
}

/// The functional identifier code (GS01) of the group each X12 transaction set is sent in,
/// by the transaction set code the request names.
///
/// Source: the ASC X12 code list of the functional identifier code element, which assigns
/// each transaction set to its functional group. A transaction set that is not listed leaves
/// GS01 a placeholder, because this emulator has no group for it.
const FUNCTIONAL_IDENTIFIER_CODES: &[(&str, &str)] = &[
    ("204", "SM"),
    ("210", "IM"),
    ("214", "QM"),
    ("270", "HS"),
    ("271", "HB"),
    ("276", "HR"),
    ("277", "HN"),
    ("278", "HI"),
    ("810", "IN"),
    ("820", "RA"),
    ("824", "AG"),
    ("830", "PS"),
    ("832", "SC"),
    ("834", "BE"),
    ("835", "HP"),
    ("837", "HC"),
    ("840", "RQ"),
    ("843", "RR"),
    ("846", "IB"),
    ("850", "PO"),
    ("852", "PD"),
    ("855", "PR"),
    ("856", "SH"),
    ("860", "PC"),
    ("861", "RC"),
    ("862", "SS"),
    ("864", "TX"),
    ("865", "PA"),
    ("867", "PT"),
    ("869", "RS"),
    ("870", "RS"),
    ("940", "OW"),
    ("945", "SW"),
    ("997", "FA"),
    ("999", "FA"),
];

/// The functional identifier code [`FUNCTIONAL_IDENTIFIER_CODES`] holds for
/// `transaction_set_code`, if any.
fn functional_identifier_code(transaction_set_code: &str) -> Option<&'static str> {
    FUNCTIONAL_IDENTIFIER_CODES
        .iter()
        .find(|(code, _)| *code == transaction_set_code)
        .map(|(_, functional_identifier)| *functional_identifier)
}

/// The transaction set header (ST): ST01 is the transaction set code the request names,
/// ST02 the control number of the one transaction set the template describes, and ST03 —
/// the implementation convention reference, an optional element — is present only when the
/// requested transaction set names an implementation guide.
fn st_elements(
    transaction_set_code: &str,
    implementation_convention: Option<&str>,
) -> Vec<(String, Node)> {
    let mut members = vec![
        (
            element_key("ST", 1, ST_ELEMENTS[0]),
            Node::Text(transaction_set_code.to_string()),
        ),
        (
            element_key("ST", 2, ST_ELEMENTS[1]),
            Node::Text(FIRST_TRANSACTION_SET_CONTROL_NUMBER.to_string()),
        ),
    ];
    if let Some(convention) = implementation_convention {
        members.push((
            element_key("ST", 3, ST_ELEMENTS[2]),
            Node::Text(convention.to_string()),
        ));
    }
    members
}

/// The starter template's document tree: one interchange, holding one functional group,
/// holding one transaction, in the X12 envelope the requested transaction set and version
/// determine.
///
/// The transaction's body — the segments between ST and SE, which differ with every
/// transaction set — sits in its `segments` list, one object per segment of the requested
/// transaction set's X12 table ([`transaction_set_segments`]); the segments after those are
/// what the caller adds to the template. The envelope carries the value the X12 standard or
/// the request fixes for each of its elements, and every element this operation does not
/// determine — the caller's own data, and every element of a body segment — is a
/// placeholder.
///
/// No envelope TRAILER is written: IEA, GE and SE carry the count of what the envelope
/// encloses and the echo of its control number, which the EDI writer derives from the
/// document itself (`super::test_conversion`), so they are not fields a mapping supplies.
fn starter_document(
    transaction_set_code: &str,
    implementation_convention: Option<&str>,
    release: u32,
    version_release_industry_code: &str,
) -> Node {
    let mut transaction = st_elements(transaction_set_code, implementation_convention);
    let body = transaction_set_segments(transaction_set_code);
    if !body.is_empty() {
        transaction.push((
            SEGMENTS_KEY.to_string(),
            Node::List {
                item: SEGMENT_ITEM.to_string(),
                items: body.iter().map(body_segment).collect(),
            },
        ));
    }

    let mut functional_group = gs_elements(transaction_set_code, version_release_industry_code);
    functional_group.push((
        TRANSACTIONS_KEY.to_string(),
        Node::List {
            item: TRANSACTION_ITEM.to_string(),
            items: vec![Node::Map(transaction)],
        },
    ));

    let mut interchange = isa_elements(release, version_release_industry_code);
    interchange.push((
        FUNCTIONAL_GROUPS_KEY.to_string(),
        Node::List {
            item: FUNCTIONAL_GROUP_ITEM.to_string(),
            items: vec![Node::Map(functional_group)],
        },
    ));

    Node::Map(vec![(
        INTERCHANGES_KEY.to_string(),
        Node::List {
            item: INTERCHANGE_ITEM.to_string(),
            items: vec![Node::Map(interchange)],
        },
    )])
}

/// The `JSONATA` rendering: the document tree as a JSON document, whose values the caller
/// replaces with JSONata expressions.
///
/// The document's SHAPE is the one `TestParsing` answers a parsed document with, over its
/// own copy of the tree — so a template and the document it maps are written in the same
/// shape — and its LAYOUT is the one the generated template is recorded with: two-space
/// indentation, a space on both sides of each member's colon, and an array on the line it
/// opens on ([`crate::state::documents::to_document_text`]).
fn render_json(document: &Node) -> Result<String, String> {
    crate::state::documents::to_document_text(document)
}

/// The `XSLT` rendering: an XSLT 1.0 stylesheet whose single template emits the same
/// document tree, so the two mapping types describe the SAME X12 envelope.
fn xslt_template(
    document: &Node,
    transaction_set_code: &str,
    version_release_industry_code: &str,
) -> Result<String, CreateStarterMappingTemplateError> {
    use ::quick_xml::events::{BytesDecl, BytesEnd, BytesStart, BytesText, Event};

    let Node::Map(roots) = document else {
        return Err(render_failed("the template document is not a map"));
    };

    let mut writer = ::quick_xml::Writer::new_with_indent(Vec::new(), b' ', 2);
    writer
        .write_event(Event::Decl(BytesDecl::new("1.0", Some("UTF-8"), None)))
        .map_err(render_failed)?;
    let mut stylesheet = BytesStart::new("xsl:stylesheet");
    stylesheet.push_attribute(("version", XSL_VERSION));
    stylesheet.push_attribute(("xmlns:xsl", XSL_NAMESPACE));
    writer
        .write_event(Event::Start(stylesheet))
        .map_err(render_failed)?;
    let mut output = BytesStart::new("xsl:output");
    output.push_attribute(("method", "xml"));
    output.push_attribute(("indent", "yes"));
    writer
        .write_event(Event::Empty(output))
        .map_err(render_failed)?;
    writer
        .write_event(Event::Comment(BytesText::new(&format!(
            "starter mapping template for X12 transaction set {transaction_set_code}, \
             version {version_release_industry_code}"
        ))))
        .map_err(render_failed)?;
    let mut template = BytesStart::new("xsl:template");
    template.push_attribute(("match", "/"));
    writer
        .write_event(Event::Start(template))
        .map_err(render_failed)?;
    for (key, child) in roots {
        write_xml_node(&mut writer, key, child).map_err(render_failed)?;
    }
    writer
        .write_event(Event::End(BytesEnd::new("xsl:template")))
        .map_err(render_failed)?;
    writer
        .write_event(Event::End(BytesEnd::new("xsl:stylesheet")))
        .map_err(render_failed)?;
    String::from_utf8(writer.into_inner()).map_err(render_failed)
}

/// One node of the document tree, as an XML element named `name`.
fn write_xml_node(
    writer: &mut ::quick_xml::Writer<Vec<u8>>,
    name: &str,
    node: &Node,
) -> Result<(), String> {
    use ::quick_xml::events::{BytesEnd, BytesStart, BytesText, Event};

    let failed = |error: ::std::io::Error| error.to_string();
    match node {
        Node::Text(text) => {
            writer
                .write_event(Event::Start(BytesStart::new(name)))
                .map_err(failed)?;
            writer
                .write_event(Event::Text(BytesText::new(text)))
                .map_err(failed)?;
            writer
                .write_event(Event::End(BytesEnd::new(name)))
                .map_err(failed)?;
        }
        Node::Map(members) if members.is_empty() => {
            writer
                .write_event(Event::Empty(BytesStart::new(name)))
                .map_err(failed)?;
        }
        Node::Map(members) => {
            writer
                .write_event(Event::Start(BytesStart::new(name)))
                .map_err(failed)?;
            for (key, child) in members {
                write_xml_node(writer, key, child)?;
            }
            writer
                .write_event(Event::End(BytesEnd::new(name)))
                .map_err(failed)?;
        }
        Node::List { item, items } => {
            writer
                .write_event(Event::Start(BytesStart::new(name)))
                .map_err(failed)?;
            for child in items {
                write_xml_node(writer, item, child)?;
            }
            writer
                .write_event(Event::End(BytesEnd::new(name)))
                .map_err(failed)?;
        }
    }
    Ok(())
}

/// The X12 reader's parsed document ([`super::test_parsing::parse_x12`]), which a sample is
/// read as.
use super::test_parsing::Node as Parsed;

/// The nested levels a starter template's envelope takes, outermost first: an interchange
/// holds functional groups, a functional group holds transactions.
///
/// One envelope segment's elements sit at each level (ISA, GS, ST), so this is also the path
/// [`fill_from_sample`] walks a sample down alongside the document it fills; the transaction
/// the path ends at is where the transaction set's own body sits.
const ENVELOPE_LEVELS: &[&str] = &[INTERCHANGES_KEY, FUNCTIONAL_GROUPS_KEY, TRANSACTIONS_KEY];

/// The element a document key names, without the element's NAME: the segment id and the
/// two-digit position (`ISA_01_AuthorizationQualifier` → `ISA_01`).
///
/// That pair is the element's identity in the X12 standard, while the name is the reading of
/// it at one release (ISA11 is the control standards identifier below release 4020 and the
/// repetition separator from it), so comparing keys by it finds a sample's value for the
/// element a template asks about however either was written.
fn element_position(key: &str) -> &str {
    let Some(after_segment) = key.find('_').map(|segment| segment + 1) else {
        return key;
    };
    match key[after_segment..].find('_') {
        Some(offset) => &key[..after_segment + offset],
        None => key,
    }
}

/// The value the parsed sample carries for the element `key` names, among the members of the
/// envelope level it was read at, or `None` when the sample carries no value there.
fn sampled_element<'a>(members: &'a [(String, Parsed)], key: &str) -> Option<&'a str> {
    let wanted = element_position(key);
    members.iter().find_map(|(member, child)| match child {
        Parsed::Text(value) if element_position(member) == wanted => Some(value.as_str()),
        _ => None,
    })
}

/// The sample's FIRST repetition of the level `key` names — the interchange, functional
/// group or transaction a generated template describes one of.
fn first_repetition<'a>(members: &'a [(String, Parsed)], key: &str) -> Option<&'a Parsed> {
    members.iter().find_map(|(member, child)| match child {
        Parsed::List { items, .. } if member == key => items.first(),
        _ => None,
    })
}

/// Fills a generated starter template from the sample the request named: at each level of
/// `levels` the value the sample carries for the element a key names replaces the one the
/// request determined, or the placeholder, the level below is read from the sample's first
/// repetition of it, and the transaction the path ends at has its BODY filled as well
/// ([`fill_body_from_sample`]).
///
/// Nothing but a value and the reader's grouping of a repeating detail segment is taken from
/// the sample. The members the document holds, the segments the transaction carries and the
/// element positions each of them asks for are the requested transaction set's
/// ([`starter_document`]), so no `segment_id` and no envelope trailer of the reader's own
/// parsed document reaches a template, an element position the sample leaves empty stays the
/// [`PLACEHOLDER`] its table asks for, and an envelope element the sample left empty stays
/// the value the request determined.
fn fill_from_sample(document: &mut Node, sample: Option<&Parsed>, levels: &[&str]) {
    let Node::Map(members) = document else {
        return;
    };
    let sampled = match sample {
        Some(Parsed::Map(sampled)) => Some(sampled.as_slice()),
        _ => None,
    };
    if let Some(sampled) = sampled {
        for (key, child) in members.iter_mut() {
            if let Node::Text(value) = child {
                if let Some(carried) = sampled_element(sampled, key) {
                    *value = carried.to_string();
                }
            }
        }
    }

    let Some((level, deeper)) = levels.split_first() else {
        // The end of the envelope path: this is the transaction, whose own segments are the
        // transaction set's body.
        if let Some(sampled) = sampled {
            fill_body_from_sample(members, sampled);
        }
        return;
    };
    let nested = sampled.and_then(|sampled| first_repetition(sampled, level));
    for (key, child) in members.iter_mut() {
        if key.as_str() != *level {
            continue;
        }
        if let Node::List { items, .. } = child {
            for item in items.iter_mut() {
                fill_from_sample(item, nested, deeper);
            }
        }
    }
}

/// One entry of the sample's transaction body, as this service's X12 reader grouped it
/// ([`super::test_parsing::parse_x12`]).
enum SampledEntry<'a> {
    /// A segment the sample carries at this point, by its X12 segment id, and its parsed
    /// members.
    Segment {
        id: &'a str,
        members: &'a [(String, Parsed)],
    },
    /// The repeating detail segment of the sample's own transaction set, grouped the way the
    /// reader groups it: the member key it sits under (`PO1-8000_loop`), the id of the
    /// segment that opens each occurrence, and one member list per occurrence.
    Loop {
        id: &'a str,
        key: &'a str,
        occurrences: Vec<&'a [(String, Parsed)]>,
    },
}

impl SampledEntry<'_> {
    /// The X12 segment id this entry carries the values of.
    fn id(&self) -> &str {
        match self {
            Self::Segment { id, .. } | Self::Loop { id, .. } => id,
        }
    }
}

/// The `segments` list of a parsed envelope level — the body of a transaction, or of one
/// occurrence of a detail loop — or nothing when it carries none.
fn sampled_segments(members: &[(String, Parsed)]) -> &[Parsed] {
    members
        .iter()
        .find_map(|(key, child)| match child {
            Parsed::List { items, .. } if key == SEGMENTS_KEY => Some(items.as_slice()),
            _ => None,
        })
        .unwrap_or_default()
}

/// The member a parsed body entry carries a DETAIL LOOP under, when it is one: the reader
/// keys a transaction set's repeating detail segment `<SEGMENT>-<position>_loop` and holds
/// one object per occurrence under it.
fn sampled_loop(members: &[(String, Parsed)]) -> Option<(&str, &[Parsed])> {
    members.iter().find_map(|(key, child)| match child {
        Parsed::List { items, .. } if key.ends_with(super::test_parsing::LOOP_KEY_SUFFIX) => {
            Some((key.as_str(), items.as_slice()))
        }
        _ => None,
    })
}

/// The X12 segment id the loop member `key` groups the occurrences of (`PO1-8000_loop` →
/// `PO1`): the loop is named for the segment that opens it.
fn loop_segment_id(key: &str) -> &str {
    key.split('-').next().unwrap_or(key)
}

/// The X12 segment id a parsed body segment reports — the reader's `segment_id` marker — or,
/// for a document that carries none, the segment its first element key names.
fn sampled_segment_id(members: &[(String, Parsed)]) -> Option<&str> {
    members
        .iter()
        .find_map(|(key, child)| match child {
            Parsed::Text(value) if key == super::test_parsing::SEGMENT_ID_KEY => {
                Some(value.as_str())
            }
            _ => None,
        })
        .or_else(|| {
            members
                .iter()
                .find_map(|(key, _)| key.split('_').next().filter(|id| !id.is_empty()))
        })
}

/// The sample's transaction body, flattened to the entries a generated segment can be filled
/// from, in document order: one per segment, one per detail loop, and — after the loop it was
/// read inside — one per segment an occurrence of that loop encloses, so a value the sample
/// carries inside the loop still reaches the segment of the table that asks for it.
fn sampled_body<'a>(transaction: &'a [(String, Parsed)]) -> Vec<SampledEntry<'a>> {
    let mut entries: Vec<SampledEntry<'a>> = Vec::new();
    for item in sampled_segments(transaction) {
        let Parsed::Map(members) = item else {
            continue;
        };
        if let Some((key, items)) = sampled_loop(members) {
            let mut occurrences: Vec<&'a [(String, Parsed)]> = Vec::new();
            let mut enclosed: Vec<SampledEntry<'a>> = Vec::new();
            for occurrence in items {
                let Parsed::Map(occurrence) = occurrence else {
                    continue;
                };
                occurrences.push(occurrence.as_slice());
                enclosed.extend(sampled_body(occurrence));
            }
            entries.push(SampledEntry::Loop {
                id: loop_segment_id(key),
                key,
                occurrences,
            });
            entries.extend(enclosed);
        } else if let Some(id) = sampled_segment_id(members) {
            entries.push(SampledEntry::Segment { id, members });
        }
    }
    entries
}

/// The X12 segment id a generated body segment carries: the segment its element keys name
/// (`BEG_01` → `BEG`), which is what names such a segment at all ([`body_element_key`]).
fn generated_segment_id(members: &[(String, Node)]) -> Option<&str> {
    members
        .first()
        .and_then(|(key, _)| key.split('_').next())
        .filter(|id| !id.is_empty())
}

/// Fills the body of a generated transaction from the sample's own transaction body: each
/// generated segment takes the values the sample carries for the element positions that
/// segment asks for, and a segment the sample repeats as a detail loop is grouped the way the
/// sample groups it, one filled object per occurrence.
///
/// The sample's entries are consumed in document order, so a transaction set whose table
/// names a segment twice fills each from its own occurrence; a segment the sample does not
/// carry keeps the bare [`PLACEHOLDER`] at every position, and a segment the requested
/// transaction set's table does not name is not added — the document's members are that
/// table's.
fn fill_body_from_sample(members: &mut [(String, Node)], sampled: &[(String, Parsed)]) {
    let mut entries = sampled_body(sampled);
    for (key, child) in members.iter_mut() {
        if key.as_str() != SEGMENTS_KEY {
            continue;
        }
        if let Node::List { items, .. } = child {
            for item in items.iter_mut() {
                fill_body_segment_from_sample(item, &mut entries);
            }
        }
    }
}

/// Fills ONE generated body segment from the first sample entry left for its segment id,
/// which that entry is then consumed by: a plain segment fills the object in place, and a
/// detail loop replaces it with the loop member the sample holds its occurrences under.
fn fill_body_segment_from_sample<'a>(segment: &mut Node, entries: &mut Vec<SampledEntry<'a>>) {
    let Node::Map(members) = segment else {
        return;
    };
    let Some(id) = generated_segment_id(members).map(str::to_string) else {
        return;
    };
    let Some(at) = entries.iter().position(|entry| entry.id() == id) else {
        return;
    };
    match entries.remove(at) {
        SampledEntry::Segment {
            members: sampled, ..
        } => fill_elements_from_sample(members, sampled),
        SampledEntry::Loop {
            key, occurrences, ..
        } => {
            // Every occurrence asks for the same element positions — the ones the table gives
            // the segment that opens the loop — each filled from its own occurrence.
            let positions = members.clone();
            let items: Vec<Node> = occurrences
                .iter()
                .map(|occurrence| {
                    let mut occurrence_members = positions.clone();
                    fill_elements_from_sample(&mut occurrence_members, occurrence);
                    Node::Map(occurrence_members)
                })
                .collect();
            *segment = Node::Map(vec![(
                key.to_string(),
                Node::List {
                    item: SEGMENT_ITEM.to_string(),
                    items,
                },
            )]);
        }
    }
}

/// The value the sample holds at each element position a generated segment asks for, as the
/// placeholder that CARRIES it ([`sampled_placeholder`]).
///
/// A position the sample leaves empty, and one it holds a COMPOSITE element at — several
/// components, which the template has no single key for — keep the bare [`PLACEHOLDER`] the
/// table asks for.
fn fill_elements_from_sample(members: &mut [(String, Node)], sampled: &[(String, Parsed)]) {
    for (key, child) in members.iter_mut() {
        let Node::Text(value) = child else {
            continue;
        };
        let carried = sampled.iter().find_map(|(member, child)| match child {
            Parsed::Text(text) if member == key && !text.is_empty() => Some(text.as_str()),
            _ => None,
        });
        if let Some(carried) = carried {
            *value = sampled_placeholder(carried);
        }
    }
}

/// The modelled refusal for a sample this service could not read, with each failure
/// reported as the error its kind calls for: a location that names no object, or an
/// object this service will not read, is the caller's `ValidationException`; an object
/// that is not there is the `ResourceNotFoundException` the operation models for exactly
/// that case; and an S3 call that failed is this service's `InternalServerException`.
fn sample_unreadable(failure: crate::state::ObjectError) -> CreateStarterMappingTemplateError {
    match &failure {
        crate::state::ObjectError::NotFound(message) => {
            CreateStarterMappingTemplateErrorModeled::ResourceNotFoundException(
                ResourceNotFoundException {
                    // The reader's own sentence, unadorned: this service words a missing
                    // object ONE way (`crate::state::s3_object_not_found`), and a prefix
                    // naming the member would make that two wordings for one failure.
                    message: message.clone(),
                },
            )
            .into()
        }
        crate::state::ObjectError::Unreadable(message) => {
            CreateStarterMappingTemplateErrorModeled::InternalServerException(
                InternalServerException {
                    message: message.clone(),
                    retry_after_seconds: None,
                },
            )
            .into()
        }
        crate::state::ObjectError::Incomplete(message)
        | crate::state::ObjectError::Rejected(message) => {
            crate::scaffolded::Scaffolded::validation(message.clone())
        }
    }
}

/// The X12 transaction set the request names, decoded from the modelled
/// `X12TransactionSet` value: every value is `X12_<three-digit transaction set code>`,
/// optionally followed by `_<implementation convention reference>` for the HIPAA
/// implementation guides — `X12_850`, `X12_837_X222`
/// (`smithy_query(query="shape",shape="X12TransactionSet")`).
///
/// The grammar is the secondary guard: the caller checks membership of the crate's
/// `X12_TRANSACTION_SET_VALUES` first, so a value shaped like one of the model's but not
/// declared by it is refused rather than generated for.
fn decode_transaction_set(value: &str) -> Option<(String, Option<String>)> {
    let rest = value.strip_prefix("X12_")?;
    let (code, convention) = match rest.split_once('_') {
        Some((code, convention)) => (code, Some(convention)),
        None => (rest, None),
    };
    if code.len() != 3 || !code.chars().all(|digit| digit.is_ascii_digit()) {
        return None;
    }
    if convention.is_some_and(|convention| {
        convention.is_empty()
            || !convention
                .chars()
                .all(|character| character.is_ascii_alphanumeric())
    }) {
        return None;
    }
    Some((code.to_string(), convention.map(str::to_string)))
}

/// The X12 version the request names, decoded from the modelled `X12Version` value: every
/// value is `VERSION_<four-digit release>`, optionally suffixed `_HIPAA` for the
/// HIPAA-constrained release (`smithy_query(query="shape",shape="X12Version")`).
///
/// Answers the release as a number — which fixes the ISA11 element's name — and the
/// six-digit version/release code X12 carries in GS08: the release zero-padded to six
/// digits, so release 4010 is `004010`. As with the transaction set, the caller checks
/// membership of the crate's `X12_VERSION_VALUES` first.
fn decode_version(value: &str) -> Option<(u32, String)> {
    let rest = value.strip_prefix("VERSION_")?;
    let release = rest.strip_suffix("_HIPAA").unwrap_or(rest);
    if release.len() != 4 || !release.chars().all(|digit| digit.is_ascii_digit()) {
        return None;
    }
    Some((release.parse().ok()?, format!("00{release}")))
}

/// The `CreateStarterMappingTemplate` operation.
///
/// Owns the shared engine (the constructor seam) and implements
/// [`smithy_server_runtime::Operation`]. Generic over the engine type `Eng`, so this
/// crate names no concrete engine type: a migrated service that needs state picks a
/// concrete engine where it mounts the service, not here.
pub struct CreateStarterMappingTemplate<Eng> {
    engine: ::std::sync::Arc<Eng>,
}

impl<Eng> CreateStarterMappingTemplate<Eng> {
    /// Construct the operation from the shared engine. INFALLIBLE — the engine is a
    /// construction-time dependency, so engine access is guaranteed by the time any
    /// request runs. Called by the generated `EmulatedService::new(engine)` constructor.
    pub fn new(engine: ::std::sync::Arc<Eng>) -> Self {
        Self { engine }
    }
}

impl<Eng: ::std::marker::Send + ::std::marker::Sync + 'static> smithy_server_runtime::Operation
    for CreateStarterMappingTemplate<Eng>
{
    type Input = CreateStarterMappingTemplateInput;
    type Output = CreateStarterMappingTemplateOutput;
    type Error = CreateStarterMappingTemplateError;

    const NAME: &'static str = "CreateStarterMappingTemplate";
    const INPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::CREATE_STARTER_MAPPING_TEMPLATE_INPUT_SCHEMA;
    const OUTPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::CREATE_STARTER_MAPPING_TEMPLATE_OUTPUT_SCHEMA;
    const SUCCESS_STATUS: u16 = 200u16;

    async fn handle(
        &self,
        input: Self::Input,
        ctx: &smithy_server_runtime::OperationContext<'_>,
    ) -> Result<Self::Output, Self::Error> {
        // No state of this service is touched: this operation mints no resource (its
        // output is one `mappingTemplate` string, and the model gives a template no id and
        // no Get/List/Delete), so the template is a function of the request and of the
        // sample object it names. Writing state here would give a generator a side effect
        // a later caller could observe, which the operation does not have.
        if let Err(denied) =
            crate::state::authorize(&*self.engine, ctx, Self::NAME, crate::state::ANY_RESOURCE)
        {
            return Err(
                CreateStarterMappingTemplateErrorModeled::AccessDeniedException(denied).into(),
            );
        }

        let Some(format) = MappingFormat::decode(&input.mapping_type) else {
            return Err(crate::scaffolded::Scaffolded::validation(format!(
                "mappingType value '{}' is not one of {}",
                input.mapping_type,
                MappingFormat::accepted()
            )));
        };

        // `TemplateDetails` is a union with one member: X12 is the only EDI standard a
        // starter template can be generated for.
        let TemplateDetails::X12(details) = input.template_details;
        let Some(transaction_set) = details.transaction_set else {
            return Err(crate::scaffolded::Scaffolded::validation(
                "templateDetails.x12.transactionSet is required to generate a starter \
                 mapping template",
            ));
        };
        let Some(version) = details.version else {
            return Err(crate::scaffolded::Scaffolded::validation(
                "templateDetails.x12.version is required to generate a starter mapping \
                 template",
            ));
        };
        let Some((transaction_set_code, implementation_convention)) =
            decode_transaction_set(&transaction_set)
                .filter(|_| X12_TRANSACTION_SET_VALUES.contains(&transaction_set.as_str()))
        else {
            return Err(crate::scaffolded::Scaffolded::validation(format!(
                "templateDetails.x12.transactionSet value '{transaction_set}' is not a \
                 modelled X12 transaction set"
            )));
        };
        let Some((release, version_code)) =
            decode_version(&version).filter(|_| X12_VERSION_VALUES.contains(&version.as_str()))
        else {
            return Err(crate::scaffolded::Scaffolded::validation(format!(
                "templateDetails.x12.version value '{version}' is not a modelled X12 version"
            )));
        };

        // GS08, the version/release/industry identifier code: the six-digit X12 version,
        // followed by the implementation convention reference when the requested
        // transaction set names one (`005010X222`).
        let version_release_industry_code = match implementation_convention.as_deref() {
            Some(convention) => format!("{version_code}{convention}"),
            None => version_code,
        };

        // The template is the document the requested transaction set and version determine,
        // whether or not the request names a sample: the segments it carries, the element
        // positions each of them asks for and the placeholder in every field the caller
        // supplies come from that transaction set's X12 table.
        let mut document = starter_document(
            &transaction_set_code,
            implementation_convention.as_deref(),
            release,
            &version_release_industry_code,
        );

        // `outputSampleLocation` fills that document from the sample: "if you provide
        // a sample EDI file with the same structure as the EDI files that you wish to
        // generate, then the service can generate a mapping template". The sample is read
        // through the emulator's own S3 service, parsed as an X12 interchange, and read for
        // the VALUE it holds at each element position the document describes
        // ([`fill_from_sample`]) — an envelope element carries that value, an element of a
        // body segment the placeholder carrying it — so the document a caller maps is the
        // same one either way. `ResourceNotFoundException` is kept for a sample that
        // genuinely cannot be read.
        if let Some(sample) = input.output_sample_location.as_ref() {
            let region = crate::state::request_region(ctx.parts());
            let text = crate::state::read_edi_object(ctx, &region, sample, "outputSampleLocation")
                .await
                .map_err(sample_unreadable)?;
            // Parsing a caller-supplied document is synchronous CPU work, so it runs on a
            // blocking thread: the `.await` leaves a cancelled request, a request timeout and
            // graceful shutdown a point to land on while it runs.
            let parsed =
                ::tokio::task::spawn_blocking(move || super::test_parsing::parse_x12(&text))
                    .await
                    .map_err(|error| {
                        CreateStarterMappingTemplateErrorModeled::InternalServerException(
                            InternalServerException {
                                message: format!(
                                    "the sample EDI file could not be parsed: {error}"
                                ),
                                retry_after_seconds: None,
                            },
                        )
                    })?
                    .map_err(|failure| {
                        crate::scaffolded::Scaffolded::validation(format!(
                            "the sample EDI file at outputSampleLocation could not be parsed: \
                             {failure}"
                        ))
                    })?;
            fill_from_sample(&mut document, Some(&parsed), ENVELOPE_LEVELS);
        }
        let mapping_template = match format {
            MappingFormat::Jsonata => render_json(&document).map_err(render_failed)?,
            MappingFormat::Xslt => xslt_template(
                &document,
                &transaction_set_code,
                &version_release_industry_code,
            )?,
        };
        // A template is used with the operations that take a `MappingTemplate`
        // (`len 0..350000`), so one past that bound could not be used at all: a transaction
        // set whose table describes a larger template than that is refused rather than
        // answered with one.
        if mapping_template.chars().count() > MAX_TEMPLATE_CHARS {
            return Err(crate::scaffolded::Scaffolded::validation(format!(
                "the template this transaction set describes is longer than the \
                 {MAX_TEMPLATE_CHARS} characters a mappingTemplate carries"
            )));
        }

        Ok(CreateStarterMappingTemplateOutput { mapping_template })
    }
}
