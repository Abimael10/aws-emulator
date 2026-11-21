// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for `CreateStarterMappingTemplate`, driven over the wire
//! (awsJson1_0, `x-amz-target: B2BI.CreateStarterMappingTemplate`) through the same
//! router the emulator boots.
//!
//! The rule under test: a starter mapping template is a document in the requested
//! `mappingType` (a JSON document for `JSONATA`, an XSLT stylesheet for `XSLT`) whose
//! X12 envelope carries the value the X12 standard or the request fixes for each of its
//! elements — ST01 the transaction set code, ST03 the implementation convention reference
//! when the transaction set names one, GS01 the functional identifier code of the group the
//! transaction set is sent in, GS08 the six-digit version/release code followed by that
//! convention, the interchange header's own standard codes and the control numbers of the
//! one interchange, group and transaction it describes — with a placeholder wherever the
//! caller must supply an expression (its own trading parties, dates and times, and every
//! element of the transaction set's own segments), and with nothing an EDI writer derives
//! itself (no envelope trailer, no ISA16); and it persists nothing, so the same request
//! always answers with the same template.
//!
//! Each test asserts that rule over a TABLE whose rows differ along what it generalises
//! over: mapping type, transaction set (with and without an implementation guide) and
//! version release.

use quick_xml::events::Event;
use serde_json::{json, Value};
// `PLACEHOLDER` and `SAMPLE_PLACEHOLDER_PREFIX` are the handler's own constants, so this
// test cannot drift from the values it asserts on; `MAPPING_TYPE_VALUES` is the model's own
// member list.
use service_com_amazonaws_b2bi_b2bi::ops::create_starter_mapping_template::{
    PLACEHOLDER, SAMPLE_PLACEHOLDER_PREFIX,
};
use service_com_amazonaws_b2bi_b2bi::ops::types::MAPPING_TYPE_VALUES;
use service_com_amazonaws_b2bi_b2bi::scaffolded::NOT_FOUND_STATUS;

// The wire harness this test target shares: one b2bi service on one shared engine, a
// signed awsJson1_0 call, and the `__type` of an error body.
use crate::edi_parsing::{given_object, X12_850 as X12_850_SAMPLE, BUCKET};
use crate::onboarding_loop::{b2bi_router, call, error_code, s3, SIGNING_REGION};

/// The request body for one starter template.
fn request(mapping_type: &str, transaction_set: &str, version: &str) -> Value {
    json!({
        "mappingType": mapping_type,
        "templateDetails": { "x12": { "transactionSet": transaction_set, "version": version } }
    })
}

/// The `mappingTemplate` a successful call answered with.
fn mapping_template(case: &str, status: u16, body: &Value) -> String {
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200, got {status} with body {body:?}"
    );
    body["mappingTemplate"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a mappingTemplate, got {body:?}"))
        .to_string()
}

/// The transaction, functional group and interchange objects of a JSONATA template,
/// which is a JSON document. Each one carries its own HEADER segment's elements directly:
/// the transaction its ST elements, the group its GS elements, the interchange its ISA
/// elements.
fn segments(case: &str, template: &str) -> (Value, Value, Value) {
    let document: Value = serde_json::from_str(template).unwrap_or_else(|error| {
        panic!("case {case:?}: a JSONATA template must be a JSON document, got {error}: {template}")
    });
    let interchange = &document["interchanges"][0];
    let group = &interchange["functional_groups"][0];
    let transaction = &group["transactions"][0];
    (transaction.clone(), group.clone(), interchange.clone())
}

/// Every element of `xml` in document order as `(name, text)`, the text empty for an
/// element with children. Panics when `xml` is not well-formed, which is how a generated
/// stylesheet is asserted to parse.
fn xml_elements(case: &str, xml: &str) -> Vec<(String, String)> {
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut elements: Vec<(String, String)> = Vec::new();
    let mut open: Vec<usize> = Vec::new();
    loop {
        match reader.read_event() {
            Ok(Event::Eof) => break,
            Ok(Event::Start(start)) => {
                let name = String::from_utf8_lossy(start.name().into_inner()).to_string();
                elements.push((name, String::new()));
                open.push(elements.len() - 1);
            }
            Ok(Event::Empty(start)) => {
                let name = String::from_utf8_lossy(start.name().into_inner()).to_string();
                elements.push((name, String::new()));
            }
            Ok(Event::Text(text)) => {
                if let Some(&index) = open.last() {
                    let decoded = String::from_utf8_lossy(text.into_inner().as_ref()).to_string();
                    elements[index].1.push_str(decoded.trim());
                }
            }
            Ok(Event::End(_)) => {
                open.pop();
            }
            Ok(_) => {}
            Err(error) => panic!(
                "case {case:?}: an XSLT template must be well-formed XML, got {error}: {xml}"
            ),
        }
    }
    elements
}

/// The text of the one element named `name`.
fn xml_text(case: &str, elements: &[(String, String)], name: &str) -> String {
    let matching: Vec<&(String, String)> = elements.iter().filter(|(key, _)| key == name).collect();
    assert_eq!(
        matching.len(),
        1,
        "case {case:?}: expected exactly one <{name}> element, got {}",
        matching.len()
    );
    matching[0].1.clone()
}

/// One row of the template table: the case name, the `mappingType`, the
/// `X12TransactionSet` and `X12Version` values, and what the request determines — the
/// ST01 transaction set code, the GS08 version/release/industry identifier code, the
/// ST03 implementation convention reference (absent when the transaction set names none)
/// and the name ISA11 takes at that version.
type TemplateCase<'a> = (
    &'a str,
    &'a str,
    &'a str,
    &'a str,
    &'a str,
    &'a str,
    Option<&'a str>,
    &'a str,
);

/// Element 11 of the interchange control header before X12 release 004020.
const ISA11_PRE_4020: &str = "ISA_11_InterchangeControlStandardsId";
/// Element 11 of the interchange control header from X12 release 004020 on.
const ISA11_FROM_4020: &str = "ISA_11_RepetitionSeparator";

/// The table every template test below spans. Transaction sets and versions are
/// `X12TransactionSet` / `X12Version` values
/// (`smithy_query(query="shape",shape="X12TransactionSet,X12Version")`), and include
/// transaction sets the issue did not cite as well as HIPAA implementation guides.
fn template_cases() -> Vec<TemplateCase<'static>> {
    vec![
        (
            "jsonata 850 release 4010",
            "JSONATA",
            "X12_850",
            "VERSION_4010",
            "850",
            "004010",
            None,
            ISA11_PRE_4020,
        ),
        (
            "jsonata 810 release 4030",
            "JSONATA",
            "X12_810",
            "VERSION_4030",
            "810",
            "004030",
            None,
            ISA11_FROM_4020,
        ),
        (
            "jsonata 204 release 4060",
            "JSONATA",
            "X12_204",
            "VERSION_4060",
            "204",
            "004060",
            None,
            ISA11_FROM_4020,
        ),
        (
            "jsonata 837 implementation guide X222, release 5010 HIPAA",
            "JSONATA",
            "X12_837_X222",
            "VERSION_5010_HIPAA",
            "837",
            "005010X222",
            Some("X222"),
            ISA11_FROM_4020,
        ),
        (
            "xslt 850 release 4010",
            "XSLT",
            "X12_850",
            "VERSION_4010",
            "850",
            "004010",
            None,
            ISA11_PRE_4020,
        ),
        (
            "xslt 856 release 5010",
            "XSLT",
            "X12_856",
            "VERSION_5010",
            "856",
            "005010",
            None,
            ISA11_FROM_4020,
        ),
        (
            "xslt 270 implementation guide X279, release 5010 HIPAA",
            "XSLT",
            "X12_270_X279",
            "VERSION_5010_HIPAA",
            "270",
            "005010X279",
            Some("X279"),
            ISA11_FROM_4020,
        ),
    ]
}

#[tokio::test]
async fn starter_template_carries_the_values_the_request_determines() {
    let router = b2bi_router();

    for (case, mapping_type, transaction_set, version, st01, gs08, st03, isa11) in template_cases()
    {
        let (status, body) = call(
            &router,
            "CreateStarterMappingTemplate",
            request(mapping_type, transaction_set, version),
        )
        .await;
        let template = mapping_template(case, status, &body);

        if mapping_type == "JSONATA" {
            let (st, gs, isa) = segments(case, &template);
            assert_eq!(
                st["ST_01_TransactionSetIdentifierCode"].as_str(),
                Some(st01),
                "case {case:?}: ST01 must be the requested transaction set code, got {st:?}"
            );
            assert_eq!(
                st["ST_03_ImplementationConventionReference"].as_str(),
                st03,
                "case {case:?}: ST03 must be the transaction set's implementation convention, got {st:?}"
            );
            assert_eq!(
                gs["GS_08_Version"].as_str(),
                Some(gs08),
                "case {case:?}: GS08 must be the requested version/release code, got {gs:?}"
            );
            assert_eq!(
                gs["GS_07_ResponsibleAgencyCode"].as_str(),
                Some("X"),
                "case {case:?}: GS07 must be the X12 responsible agency code, got {gs:?}"
            );
            assert!(
                isa.get(isa11).is_some(),
                "case {case:?}: ISA11 must be <{isa11}> at this version, got {isa:?}"
            );
            let sender = isa["ISA_06_SenderId"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            assert_eq!(
                sender, PLACEHOLDER,
                "case {case:?}: ISA06 must be a placeholder, got {sender:?}"
            );
        } else {
            let elements = xml_elements(case, &template);
            assert_eq!(
                elements.first().map(|(name, _)| name.as_str()),
                Some("xsl:stylesheet"),
                "case {case:?}: the stylesheet's root element must be xsl:stylesheet, got {:?}",
                elements.first()
            );
            assert!(
                template.contains("version=\"1.0\""),
                "case {case:?}: the stylesheet must declare its XSLT version, got {template}"
            );
            assert!(
                template.contains("http://www.w3.org/1999/XSL/Transform"),
                "case {case:?}: the stylesheet must declare the XSLT namespace, got {template}"
            );
            assert_eq!(
                xml_text(case, &elements, "ST_01_TransactionSetIdentifierCode"),
                st01,
                "case {case:?}: ST01 must be the requested transaction set code"
            );
            assert_eq!(
                xml_text(case, &elements, "GS_08_Version"),
                gs08,
                "case {case:?}: GS08 must be the requested version/release code"
            );
            assert_eq!(
                xml_text(case, &elements, "GS_07_ResponsibleAgencyCode"),
                "X",
                "case {case:?}: GS07 must be the X12 responsible agency code"
            );
            let convention: Vec<&(String, String)> = elements
                .iter()
                .filter(|(name, _)| name == "ST_03_ImplementationConventionReference")
                .collect();
            match st03 {
                Some(expected) => assert!(
                    convention.len() == 1 && convention[0].1 == expected,
                    "case {case:?}: ST03 must be {expected:?}, got {convention:?}"
                ),
                None => assert!(
                    convention.is_empty(),
                    "case {case:?}: ST03 must be absent, got {convention:?}"
                ),
            }
            assert!(
                elements.iter().any(|(name, _)| name == isa11),
                "case {case:?}: ISA11 must be <{isa11}> at this version, got {:?}",
                elements.iter().map(|(name, _)| name).collect::<Vec<_>>()
            );
            assert_eq!(
                xml_text(case, &elements, "ISA_06_SenderId"),
                PLACEHOLDER,
                "case {case:?}: ISA06 must be a placeholder, got {:?}",
                xml_text(case, &elements, "ISA_06_SenderId")
            );
        }
    }
}

#[tokio::test]
async fn a_template_carries_the_transaction_sets_body_segments() {
    let router = b2bi_router();

    // Each row: the case, the `X12TransactionSet` and `X12Version` values, and the X12
    // segment id that opens that transaction set's heading table — `None` for a
    // transaction set this emulator has no segment table for, whose template carries the
    // envelope and the ST header alone.
    let cases: Vec<(&str, &str, &str, Option<&str>)> = vec![
        ("850 purchase order", "X12_850", "VERSION_4010", Some("BEG")),
        ("810 invoice", "X12_810", "VERSION_4010", Some("BIG")),
        ("856 ship notice", "X12_856", "VERSION_5010", Some("BSN")),
        (
            "837 health care claim",
            "X12_837_X222",
            "VERSION_5010_HIPAA",
            Some("BHT"),
        ),
        (
            "997 functional acknowledgment",
            "X12_997",
            "VERSION_4010",
            Some("AK1"),
        ),
        (
            "100 insurance plan description",
            "X12_100",
            "VERSION_4010",
            None,
        ),
    ];

    for (case, transaction_set, version, segment) in cases {
        let (status, body) = call(
            &router,
            "CreateStarterMappingTemplate",
            request("JSONATA", transaction_set, version),
        )
        .await;
        let template = mapping_template(case, status, &body);
        let (transaction, _, _) = segments(case, &template);
        let keys: Vec<String> = transaction
            .as_object()
            .unwrap_or_else(|| panic!("case {case:?}: the transaction must be an object"))
            .keys()
            .cloned()
            .collect();

        match segment {
            Some(id) => {
                // The body segments sit in the transaction's `segments` list, each one an
                // object whose elements are keyed by segment and position — which is what
                // names the segment, so the object carries no further member.
                let body_segment = &transaction["segments"][0];
                let elements = body_segment.as_object().unwrap_or_else(|| {
                    panic!("case {case:?}: a segment is an object, got {body_segment}")
                });
                let first = format!("{id}_01");
                assert_eq!(
                    body_segment[&first].as_str(),
                    Some(PLACEHOLDER),
                    "case {case:?}: {first} must be a placeholder, got {body_segment:?}"
                );
                for (position, key) in elements.keys().enumerate() {
                    assert_eq!(
                        key,
                        &format!("{id}_{:02}", position + 1),
                        "case {case:?}: the body's first segment carries the elements of {id} \
                         keyed by position, got {body_segment}"
                    );
                }
            }
            None => assert!(
                keys.iter().all(|key| key.starts_with("ST_")),
                "case {case:?}: with no segment table the transaction carries its ST header \
                 alone, got {keys:?}"
            ),
        }
    }
}

#[tokio::test]
async fn template_elements_keep_their_positional_order() {
    let router = b2bi_router();

    // The ISA elements, in the positional order an X12 interchange control header has
    // them in; a template that sorted its members would interleave these.
    for (case, mapping_type, transaction_set, version, _, _, _, isa11) in template_cases() {
        let (status, body) = call(
            &router,
            "CreateStarterMappingTemplate",
            request(mapping_type, transaction_set, version),
        )
        .await;
        let template = mapping_template(case, status, &body);

        let positions: Vec<usize> = [
            "ISA_01_AuthorizationQualifier",
            "ISA_02_AuthorizationInformation",
            "ISA_03_SecurityQualifier",
            "ISA_04_SecurityInformation",
            "ISA_05_SenderQualifier",
            "ISA_06_SenderId",
            "ISA_07_ReceiverQualifier",
            "ISA_08_ReceiverId",
            "ISA_09_Date",
            "ISA_10_Time",
            isa11,
            "ISA_12_Version",
            "ISA_13_InterchangeControlNumber",
            "ISA_14_AcknowledgmentRequested",
            "ISA_15_TestIndicator",
        ]
        .iter()
        .map(|element| {
            template.find(element).unwrap_or_else(|| {
                panic!("case {case:?}: the template must carry {element}, got {template}")
            })
        })
        .collect();

        assert!(
            positions.windows(2).all(|pair| pair[0] < pair[1]),
            "case {case:?}: ISA elements must appear in positional order, got offsets {positions:?}"
        );
        // ISA16 declares the component element separator the interchange is written with — a
        // delimiter of the file rather than data in it — so a template asks for no
        // expression there.
        assert!(
            !template.contains("ISA_16"),
            "case {case:?}: a template carries no ISA16, got {template}"
        );
    }
}

#[tokio::test]
async fn the_same_request_always_answers_with_the_same_template() {
    let router = b2bi_router();

    // The operation mints nothing and persists nothing, so it is a pure function of the
    // request: a repeat cannot differ, and the templates of two DIFFERENT requests must.
    let mut seen: Vec<(&str, String)> = Vec::new();
    for (case, mapping_type, transaction_set, version, _, _, _, _) in template_cases() {
        let body = request(mapping_type, transaction_set, version);
        let (first_status, first_body) = call(&router, "CreateStarterMappingTemplate", body.clone()).await;
        let (second_status, second_body) = call(&router, "CreateStarterMappingTemplate", body).await;
        let first = mapping_template(case, first_status, &first_body);
        let second = mapping_template(case, second_status, &second_body);
        assert_eq!(
            first, second,
            "case {case:?}: a repeated request must answer with the same template"
        );
        if let Some((earlier, _)) = seen.iter().find(|(_, template)| *template == first) {
            panic!("case {case:?}: must not answer the template case {earlier:?} answered with");
        }
        seen.push((case, first));
    }
}

/// A sample 810 invoice at release 005010, written with `|` between elements — the sample's
/// own delimiters, transaction set and release, none of them any request's.
const SAMPLE_810: &str = concat!(
    "ISA|00|          |00|          |ZZ|INVSENDER      |ZZ|INVRECEIVER    ",
    "|240201|0900|^|00501|000000009|0|P|>\n",
    "GS|IN|INVSENDER|INVRECEIVER|20240201|0900|9|X|005010\n",
    "ST|810|0009\n",
    "BIG|20240201|INV-42|20240101|PO-77\n",
    "SE|3|0009\n",
    "GE|1|9\n",
    "IEA|1|000000009\n",
);

/// A sample 850 whose detail table REPEATS: two line items, a description inside the first
/// one, a reference whose value is a composite (two components, `>` between them, as this
/// interchange's own ISA16 declares) and a transaction-totals summary.
const SAMPLE_850_LINE_ITEMS: &str = concat!(
    "ISA*00*          *00*          *ZZ*ITEMSENDER     *ZZ*ITEMRECEIVER   ",
    "*240301*0800*U*00401*000000011*0*P*>~",
    "GS*PO*ITEMSENDER*ITEMRECEIVER*20240301*0800*11*X*004010~",
    "ST*850*0011~",
    "BEG*00*SA*PO-ITEMS**20240301~",
    "REF*DP*DEPT>01~",
    "PO1*1*10*EA*25.00**BP*WIDGET001*VN*WIDGET-A~",
    "PID*F****First widget~",
    "PO1*2*5*EA*30.00**BP*WIDGET002*VN*WIDGET-B~",
    "CTT*2~",
    "SE*8*0011~",
    "GE*1*11~",
    "IEA*1*000000011~",
);

/// The 850's own detail loop member, as this service's X12 reader keys it.
const ITEM_LOOP: &str = "PO1-8000_loop";

/// One row of the sample table.
struct SampleCase<'a> {
    /// What this row is.
    case: &'a str,
    /// The object key the sample is stored at, and its content.
    key: &'a str,
    sample: &'a str,
    /// The `mappingType`, `X12TransactionSet` and `X12Version` the request names.
    mapping_type: &'a str,
    transaction_set: &'a str,
    version: &'a str,
    /// Envelope values of the sample the template must carry verbatim.
    envelope: &'a [&'a str],
    /// `(element position, the sample's value at it)` pairs the template must carry as the
    /// placeholder CARRYING that value.
    filled: &'a [(&'a str, &'a str)],
    /// Element positions that stay the bare placeholder.
    bare: &'a [&'a str],
    /// Text no part of the template may carry.
    absent: &'a [&'a str],
    /// For a sample whose detail table repeats: the loop member, one element position of its
    /// opening segment, and the sample's value for that position in each occurrence, in
    /// order.
    detail_loop: Option<(&'a str, &'a str, &'a [&'a str])>,
}

/// The value a JSONATA template's transaction body carries at the element position `key`.
///
/// The body's segments are searched in document order, and the occurrences of a detail loop
/// with them, so a position inside the loop is found as readily as one beside it.
fn body_element(transaction: &Value, key: &str) -> Option<String> {
    fn found(node: &Value, key: &str) -> Option<String> {
        match node {
            Value::Array(items) => items.iter().find_map(|item| found(item, key)),
            Value::Object(members) => members.iter().find_map(|(member, child)| {
                if member == key {
                    child.as_str().map(str::to_string)
                } else {
                    found(child, key)
                }
            }),
            _ => None,
        }
    }
    found(&transaction["segments"], key)
}

/// The occurrences a JSONATA template's transaction body holds under the detail loop member
/// `loop_key`.
fn loop_occurrences(transaction: &Value, loop_key: &str) -> Vec<Value> {
    transaction["segments"]
        .as_array()
        .map(|segments| {
            segments
                .iter()
                .filter_map(|segment| segment.get(loop_key))
                .filter_map(|occurrences| occurrences.as_array().cloned())
                .flatten()
                .collect()
        })
        .unwrap_or_default()
}

/// RULE: a request that names an `outputSampleLocation` is answered with the template the
/// requested transaction set and version determine, carrying what that sample holds at each
/// element position it describes — an envelope element the sample's value itself, an element
/// of a body segment the placeholder that CARRIES that value (the fixed
/// [`SAMPLE_PLACEHOLDER_PREFIX`] and then the value), and a position the sample leaves empty
/// the bare placeholder — with the reader's grouping of a repeating detail segment travelling
/// with those values and none of its other members (a `segment_id`, an envelope trailer)
/// reaching the template.
///
/// The rows differ along what the rule generalises over: the requested mapping type and
/// transaction set, the sample's own transaction set and release, the delimiters it is
/// written with, which element positions it fills, whether its detail table repeats, and
/// whether the value it holds at a position is one element or a composite.
#[tokio::test]
async fn a_sample_fills_the_element_values_of_the_generated_template() {
    let router = b2bi_router();

    let cases: Vec<SampleCase<'_>> = vec![
        SampleCase {
            case: "an 850 sample, JSONATA",
            key: "samples/sample-850.edi",
            sample: X12_850_SAMPLE,
            mapping_type: "JSONATA",
            transaction_set: "X12_850",
            version: "VERSION_4010",
            envelope: &["SENDER", "000000001", "0001", "004010"],
            filled: &[
                ("BEG_01", "00"),
                ("BEG_02", "SA"),
                ("BEG_03", "PO12345"),
                ("BEG_05", "20240101"),
                ("N1_01", "ST"),
                ("N1_02", "Acme Receiving"),
            ],
            // BEG04 is empty in the sample, and the sample carries no currency or item
            // segment at all, so each of these stays the placeholder its table asks for.
            bare: &["BEG_04", "BEG_12", "CUR_01", "PO1_01"],
            absent: &[],
            detail_loop: None,
        },
        SampleCase {
            case: "an 850 sample, XSLT",
            key: "samples/sample-850.edi",
            sample: X12_850_SAMPLE,
            mapping_type: "XSLT",
            transaction_set: "X12_850",
            version: "VERSION_4010",
            envelope: &["SENDER", "000000001", "0001", "004010"],
            filled: &[("BEG_03", "PO12345"), ("N1_02", "Acme Receiving")],
            bare: &["BEG_04", "CUR_01"],
            absent: &[],
            detail_loop: None,
        },
        SampleCase {
            case: "an 850 sample whose detail table repeats, JSONATA",
            key: "samples/sample-850-items.edi",
            sample: SAMPLE_850_LINE_ITEMS,
            mapping_type: "JSONATA",
            transaction_set: "X12_850",
            version: "VERSION_4010",
            envelope: &["ITEMSENDER", "000000011", "0011"],
            filled: &[
                ("BEG_03", "PO-ITEMS"),
                ("REF_01", "DP"),
                // The description sits INSIDE the first line item, and still reaches the
                // position the requested transaction set's table gives it.
                ("PID_05", "First widget"),
                ("CTT_01", "2"),
            ],
            // REF02 holds a composite in the sample — several components, which no single
            // element position of the template asks for — and PO1_10 is past the nine
            // elements each line item fills.
            bare: &["REF_02", "BEG_04", "PO1_10"],
            absent: &["DEPT>01"],
            detail_loop: Some((ITEM_LOOP, "PO1_09", &["WIDGET-A", "WIDGET-B"])),
        },
        SampleCase {
            case: "an 850 sample whose detail table repeats, XSLT",
            key: "samples/sample-850-items.edi",
            sample: SAMPLE_850_LINE_ITEMS,
            mapping_type: "XSLT",
            transaction_set: "X12_850",
            version: "VERSION_4010",
            envelope: &["ITEMSENDER", "000000011", "0011"],
            filled: &[("BEG_03", "PO-ITEMS"), ("CTT_01", "2")],
            bare: &["REF_02", "BEG_04"],
            absent: &[],
            detail_loop: Some((ITEM_LOOP, "PO1_09", &["WIDGET-A", "WIDGET-B"])),
        },
        SampleCase {
            case: "an 810 sample at release 5010 with pipe delimiters, requested as an 810",
            key: "samples/sample-810.edi",
            sample: SAMPLE_810,
            mapping_type: "JSONATA",
            transaction_set: "X12_810",
            version: "VERSION_5010",
            envelope: &["INVSENDER", "000000009", "0009", "005010"],
            filled: &[
                ("BIG_01", "20240201"),
                ("BIG_02", "INV-42"),
                ("BIG_04", "PO-77"),
            ],
            bare: &["BIG_05", "CUR_01"],
            absent: &[],
            detail_loop: None,
        },
        SampleCase {
            // The document's members stay the REQUESTED transaction set's: the 850's table
            // names no invoice beginning segment, so no position of this template asks for
            // what the sample holds in one.
            case: "an 810 sample against a request for an 850, JSONATA",
            key: "samples/sample-810.edi",
            sample: SAMPLE_810,
            mapping_type: "JSONATA",
            transaction_set: "X12_850",
            version: "VERSION_4010",
            envelope: &["INVSENDER", "000000009", "0009", "005010"],
            filled: &[],
            bare: &["BEG_01", "BEG_03", "CUR_01"],
            absent: &["INV-42", "BIG_01", "PLACEHOLDER: "],
            detail_loop: None,
        },
    ];

    for row in cases {
        let case = row.case;
        given_object(&router, row.key, row.sample).await;
        let mut request = request(row.mapping_type, row.transaction_set, row.version);
        request["outputSampleLocation"] = json!({ "bucketName": BUCKET, "key": row.key });
        let (status, response) = call(&router, "CreateStarterMappingTemplate", request).await;
        let template = mapping_template(case, status, &response);

        // The envelope values come from the SAMPLE, not from the request: a sample at
        // another release, written with other delimiters, proves the envelope was filled
        // from the file.
        for expected in row.envelope {
            assert!(
                template.contains(expected),
                "case {case:?}: the template's envelope must carry the sample's \
                 {expected:?}, got {template}"
            );
        }
        // None of the X12 reader's OWN members travels with those values.
        for absent in ["segment_id", "SE_01", "GE_01", "IEA_01"]
            .iter()
            .chain(row.absent)
        {
            assert!(
                !template.contains(absent),
                "case {case:?}: a template carries no {absent:?}, got {template}"
            );
        }

        if row.mapping_type == "JSONATA" {
            let (transaction, _, _) = segments(case, &template);
            for (position, sampled) in row.filled {
                let expected = format!("{SAMPLE_PLACEHOLDER_PREFIX}{sampled}");
                let actual = body_element(&transaction, position);
                assert_eq!(
                    actual.as_deref(),
                    Some(expected.as_str()),
                    "case {case:?}: {position} must carry the sample's value as a \
                     placeholder, got {actual:?} in {transaction}"
                );
            }
            for position in row.bare {
                let actual = body_element(&transaction, position);
                assert_eq!(
                    actual.as_deref(),
                    Some(PLACEHOLDER),
                    "case {case:?}: {position} must be the bare placeholder, got \
                     {actual:?} in {transaction}"
                );
            }
            match row.detail_loop {
                Some((loop_key, position, values)) => {
                    let occurrences = loop_occurrences(&transaction, loop_key);
                    assert_eq!(
                        occurrences.len(),
                        values.len(),
                        "case {case:?}: {loop_key} must hold one entry per occurrence the \
                         sample repeats, got {occurrences:?}"
                    );
                    for (index, (occurrence, sampled)) in
                        occurrences.iter().zip(values).enumerate()
                    {
                        let expected = format!("{SAMPLE_PLACEHOLDER_PREFIX}{sampled}");
                        assert_eq!(
                            occurrence[position].as_str(),
                            Some(expected.as_str()),
                            "case {case:?}: occurrence {index} of {loop_key} must carry \
                             {position} from the sample, got {occurrence}"
                        );
                    }
                }
                None => assert!(
                    !template.contains("_loop"),
                    "case {case:?}: a sample whose detail table does not repeat leaves the \
                     template no loop member, got {template}"
                ),
            }
        } else {
            let elements = xml_elements(case, &template);
            let carries = |name: &str, text: &str| {
                elements
                    .iter()
                    .any(|(element, value)| element == name && value == text)
            };
            for (position, sampled) in row.filled {
                let expected = format!("{SAMPLE_PLACEHOLDER_PREFIX}{sampled}");
                assert!(
                    carries(position, &expected),
                    "case {case:?}: <{position}> must carry {expected:?}, got {elements:?}"
                );
            }
            for position in row.bare {
                assert!(
                    carries(position, PLACEHOLDER),
                    "case {case:?}: <{position}> must be the bare placeholder, got \
                     {elements:?}"
                );
            }
            if let Some((loop_key, position, values)) = row.detail_loop {
                assert!(
                    elements.iter().any(|(element, _)| element == loop_key),
                    "case {case:?}: the stylesheet must carry the sample's {loop_key} \
                     grouping, got {elements:?}"
                );
                for sampled in values {
                    let expected = format!("{SAMPLE_PLACEHOLDER_PREFIX}{sampled}");
                    assert!(
                        carries(position, &expected),
                        "case {case:?}: one occurrence's <{position}> must carry \
                         {expected:?}, got {elements:?}"
                    );
                }
            }
        }
    }
}

/// RULE: the placeholder a sample-derived element carries is the same sentinel the caller
/// replaces everywhere else, followed by the value — so a client's "replace every
/// PLACEHOLDER" step still finds every field it must fill.
#[test]
fn the_sample_placeholder_carries_the_sentinel_the_caller_replaces() {
    assert_eq!(
        SAMPLE_PLACEHOLDER_PREFIX,
        format!("{PLACEHOLDER}: "),
        "the sample placeholder is the sentinel, a colon and a space, got \
         {SAMPLE_PLACEHOLDER_PREFIX:?}"
    );
}

/// One row of the mapped-document table: the case, the sample's key and content, and the
/// (element position, sample value) pairs the mapped document must carry as placeholders
/// carrying those values.
type MappedCase<'a> = (&'a str, &'a str, &'a str, Vec<(&'a str, &'a str)>);

/// RULE: a starter template is a document the mapping operations evaluate, so the values a
/// sample filled it with survive into `TestMapping`'s `mappedFileContent` — a caller handing
/// the generated template straight to `TestMapping` gets back the document the sample
/// described.
///
/// The rows differ along the sample's values and whether its detail table repeats, so a
/// value inside the loop is asserted beside one outside it.
#[tokio::test]
async fn the_sample_derived_values_survive_into_the_mapped_document() {
    let router = b2bi_router();

    // Each row: the case, the sample's key and content, and the (element position, sample
    // value) pairs the MAPPED document must carry as placeholders carrying those values.
    let cases: Vec<MappedCase<'_>> = vec![
        (
            "an 850 sample",
            "mapped/sample-850.edi",
            X12_850_SAMPLE,
            vec![("BEG_03", "PO12345"), ("N1_02", "Acme Receiving")],
        ),
        (
            "an 850 sample whose detail table repeats",
            "mapped/sample-850-items.edi",
            SAMPLE_850_LINE_ITEMS,
            vec![("BEG_03", "PO-ITEMS"), ("PO1_09", "WIDGET-A"), ("CTT_01", "2")],
        ),
    ];

    for (case, key, sample, expected) in cases {
        given_object(&router, key, sample).await;
        let mut template_request = request("JSONATA", "X12_850", "VERSION_4010");
        template_request["outputSampleLocation"] = json!({ "bucketName": BUCKET, "key": key });
        let (status, response) =
            call(&router, "CreateStarterMappingTemplate", template_request).await;
        let template = mapping_template(case, status, &response);

        // The generated template, applied to an empty input document the way a caller tries
        // one out.
        let (status, mapped) = call(
            &router,
            "TestMapping",
            json!({
                "fileFormat": "JSON",
                "inputFileContent": "{}",
                "mappingTemplate": template,
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the generated template must map, got {status} {mapped}"
        );
        // `mappedFileContent` carries the mapped file's TEXT as a JSON string, so the
        // document takes two parses — the way a client reads it.
        let member = mapped["mappedFileContent"].as_str().unwrap_or_else(|| {
            panic!("case {case:?}: expected a mappedFileContent, got {mapped}")
        });
        let text: String = serde_json::from_str(member).unwrap_or_else(|failure| {
            panic!("case {case:?}: mappedFileContent must hold the mapped text: {failure}")
        });
        let document: Value = serde_json::from_str(&text).unwrap_or_else(|failure| {
            panic!("case {case:?}: the mapped file must be a JSON document: {failure}: {text}")
        });
        let transaction =
            &document["interchanges"][0]["functional_groups"][0]["transactions"][0];
        for (position, sampled) in expected {
            let wanted = format!("{SAMPLE_PLACEHOLDER_PREFIX}{sampled}");
            let actual = body_element(transaction, position);
            assert_eq!(
                actual.as_deref(),
                Some(wanted.as_str()),
                "case {case:?}: the mapped document must carry {position} as the sample's \
                 placeholder, got {actual:?} in {text}"
            );
        }
    }
}
type BodyTableCase<'a> = (&'a str, &'a str, &'a str, Vec<(&'a str, usize)>);

/// RULE: every element of a segment INSIDE the transaction is the bare placeholder, one per
/// element position of that segment's X12 table — the recorded 850 template carries BEG_01
/// through BEG_12 and CUR_01 through CUR_09 — so a caller's mapping can address every field
/// the standard defines for the segment, not only the ones a sample happened to fill.
///
/// The rows are the segments of the requested transaction set's table, with the count the
/// X12 segment directory gives each one.
#[tokio::test]
async fn a_body_segment_carries_every_element_position_of_its_table() {
    let router = b2bi_router();

    // Each row: the case, the `X12TransactionSet`, and the (segment id, element count)
    // pairs the first segments of its body carry. Counts: the ASC X12 segment directory,
    // except the currency segment, whose nine elements are the recorded template's.
    let cases: Vec<BodyTableCase<'_>> = vec![
        (
            "850 purchase order",
            "X12_850",
            "VERSION_4010",
            vec![("BEG", 12), ("CUR", 9), ("REF", 4)],
        ),
        (
            "997 functional acknowledgment",
            "X12_997",
            "VERSION_4010",
            vec![("AK1", 3), ("AK2", 3), ("AK9", 9)],
        ),
    ];

    for (case, transaction_set, version, expected) in cases {
        let (status, body) = call(
            &router,
            "CreateStarterMappingTemplate",
            request("JSONATA", transaction_set, version),
        )
        .await;
        let template = mapping_template(case, status, &body);
        let (transaction, _, _) = segments(case, &template);
        for (index, (segment, count)) in expected.iter().enumerate() {
            let reported = &transaction["segments"][index];
            let positions: Vec<String> = reported
                .as_object()
                .unwrap_or_else(|| panic!("case {case:?}: a segment is an object, got {reported}"))
                .keys()
                .cloned()
                .collect();
            let wanted: Vec<String> = (1..=*count)
                .map(|position| format!("{segment}_{position:02}"))
                .collect();
            assert_eq!(
                positions, wanted,
                "case {case:?}: segment {index} must carry every element position of \
                 {segment}, got {positions:?}"
            );
            for position in &positions {
                assert_eq!(
                    reported[position].as_str(),
                    Some(PLACEHOLDER),
                    "case {case:?}: {position} must be the bare placeholder, got {reported}"
                );
            }
        }
    }
}

/// RULE: the envelope of a generated template carries the value the X12 standard or the
/// request fixes for each of its elements — the qualifiers that say no authorization or
/// security information is present, the control standards identifier, the interchange
/// control version number of the requested release, the acknowledgment and test indicators,
/// the control numbers of the one interchange, group and transaction it describes, the
/// functional identifier code of the group the requested transaction set is sent in, and the
/// responsible agency and version codes — and the placeholder wherever the element is the
/// caller's own data.
///
/// The rows differ along what the rule generalises over: the transaction set (and so its
/// functional group), including one this emulator has no group for, and the release (and so
/// the interchange control version number and whether ISA11 is data at all).
#[tokio::test]
async fn the_envelope_carries_the_values_the_standard_and_the_request_fix() {
    let router = b2bi_router();

    // Each row: the case, the `X12TransactionSet` and `X12Version` values, the functional
    // identifier code (GS01) of the group that transaction set is sent in, the interchange
    // control version number (ISA12) of that release, and the ISA11 member name with the
    // value it must carry. GS01 codes: the ASC X12 code list of the functional identifier
    // code element; the 810 invoice's `IN` is not a code the issue cited.
    let cases: Vec<(&str, &str, &str, &str, &str, &str, &str)> = vec![
        (
            "850 purchase order at release 4010",
            "X12_850",
            "VERSION_4010",
            "PO",
            "00401",
            ISA11_PRE_4020,
            "U",
        ),
        (
            "810 invoice at release 5010",
            "X12_810",
            "VERSION_5010",
            "IN",
            "00501",
            ISA11_FROM_4020,
            PLACEHOLDER,
        ),
        (
            "997 functional acknowledgment at release 4030",
            "X12_997",
            "VERSION_4030",
            "FA",
            "00403",
            ISA11_FROM_4020,
            PLACEHOLDER,
        ),
        (
            "100 insurance plan description, a transaction set with no group here",
            "X12_100",
            "VERSION_4010",
            PLACEHOLDER,
            "00401",
            ISA11_PRE_4020,
            "U",
        ),
    ];

    for (case, transaction_set, version, gs01, isa12, isa11, isa11_value) in cases {
        let (status, body) = call(
            &router,
            "CreateStarterMappingTemplate",
            request("JSONATA", transaction_set, version),
        )
        .await;
        let template = mapping_template(case, status, &body);
        let (transaction, group, interchange) = segments(case, &template);

        for (scope, object, member, expected) in [
            ("interchange", &interchange, "ISA_01_AuthorizationQualifier", "00"),
            ("interchange", &interchange, "ISA_03_SecurityQualifier", "00"),
            ("interchange", &interchange, isa11, isa11_value),
            ("interchange", &interchange, "ISA_12_Version", isa12),
            (
                "interchange",
                &interchange,
                "ISA_13_InterchangeControlNumber",
                "000000001",
            ),
            (
                "interchange",
                &interchange,
                "ISA_14_AcknowledgmentRequested",
                "0",
            ),
            ("interchange", &interchange, "ISA_15_TestIndicator", "P"),
            ("functional group", &group, "GS_01_FunctionalIdentifierCode", gs01),
            ("functional group", &group, "GS_06_GroupControlNumber", "1"),
            ("functional group", &group, "GS_07_ResponsibleAgencyCode", "X"),
            (
                "transaction set",
                &transaction,
                "ST_02_TransactionSetControlNumber",
                "000001",
            ),
        ] {
            assert_eq!(
                object[member].as_str(),
                Some(expected),
                "case {case:?}: the {scope}'s {member} must carry {expected:?}, got {object}"
            );
        }

        // The elements that carry the caller's own data are the caller's to map.
        for (scope, object, member) in [
            (
                "interchange",
                &interchange,
                "ISA_02_AuthorizationInformation",
            ),
            ("interchange", &interchange, "ISA_04_SecurityInformation"),
            ("interchange", &interchange, "ISA_05_SenderQualifier"),
            ("interchange", &interchange, "ISA_06_SenderId"),
            ("interchange", &interchange, "ISA_08_ReceiverId"),
            ("interchange", &interchange, "ISA_09_Date"),
            ("interchange", &interchange, "ISA_10_Time"),
            ("functional group", &group, "GS_02_ApplicationSenderCode"),
            ("functional group", &group, "GS_04_Date"),
            ("functional group", &group, "GS_05_Time"),
        ] {
            assert_eq!(
                object[member].as_str(),
                Some(PLACEHOLDER),
                "case {case:?}: the {scope}'s {member} is the caller's to supply, got {object}"
            );
        }
    }
}

#[tokio::test]
async fn every_modelled_mapping_type_generates_a_template() {
    let router = b2bi_router();

    // The model's own member list, so a mapping type the model gains cannot silently stay
    // unimplemented (it would be refused as not one of the formats the handler accepts).
    for mapping_type in MAPPING_TYPE_VALUES {
        let (status, body) = call(
            &router,
            "CreateStarterMappingTemplate",
            request(mapping_type, "X12_850", "VERSION_4010"),
        )
        .await;
        let template = mapping_template(mapping_type, status, &body);
        assert!(
            template.contains("850"),
            "case {mapping_type:?}: the template must carry the requested transaction set code, got {template}"
        );
    }
}

#[tokio::test]
async fn a_request_the_model_does_not_declare_is_refused() {
    let router = b2bi_router();

    // Each row: the case, the request, and the member the refusal must name (`None` where
    // the refusal may come from the boundary validator, whose wording is its own).
    let cases: Vec<(&str, Value, Option<&str>)> = vec![
        (
            "no transactionSet",
            json!({
                "mappingType": "JSONATA",
                "templateDetails": { "x12": { "version": "VERSION_4010" } }
            }),
            Some("transactionSet"),
        ),
        (
            "no version",
            json!({
                "mappingType": "XSLT",
                "templateDetails": { "x12": { "transactionSet": "X12_850" } }
            }),
            Some("version"),
        ),
        (
            "neither",
            json!({ "mappingType": "JSONATA", "templateDetails": { "x12": {} } }),
            Some("transactionSet"),
        ),
        (
            // Shaped like a modelled X12TransactionSet but not one of its values.
            "transaction set the model does not declare",
            json!({
                "mappingType": "JSONATA",
                "templateDetails": { "x12": { "transactionSet": "X12_999_ZZZZ", "version": "VERSION_4010" } }
            }),
            None,
        ),
        (
            // Shaped like a modelled X12Version but not one of its values.
            "version the model does not declare",
            json!({
                "mappingType": "JSONATA",
                "templateDetails": { "x12": { "transactionSet": "X12_850", "version": "VERSION_9999" } }
            }),
            None,
        ),
        (
            "mapping type the model does not declare",
            json!({
                "mappingType": "jsonata",
                "templateDetails": { "x12": { "transactionSet": "X12_850", "version": "VERSION_4010" } }
            }),
            None,
        ),
    ];

    for (case, body, member) in cases {
        let (status, response) = call(&router, "CreateStarterMappingTemplate", body).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with body {response:?}"
        );
        assert_eq!(
            error_code(&response),
            "ValidationException",
            "case {case:?}: expected a ValidationException, got {response:?}"
        );
        if let Some(member) = member {
            let message = response["message"].as_str().unwrap_or_default();
            assert!(
                message.contains(member),
                "case {case:?}: the refusal must name {member:?}, got {message:?}"
            );
        }
    }
}

/// RULE: `outputSampleLocation` is resolved through Amazon S3, and only a sample that
/// genuinely does not resolve is the modelled `ResourceNotFoundException`: a missing
/// bucket and a missing key both are. A location that names no object at all — no bucket,
/// or no key — is instead the caller's own `ValidationException` (400), because it
/// addresses nothing to look for.
#[tokio::test]
async fn a_sample_edi_file_that_cannot_be_read_is_refused_as_not_found() {
    let router = b2bi_router();

    // One bucket really exists, so the rows separate "this bucket holds no such object"
    // from "there is no such bucket". The create names its `LocationConstraint`, which a
    // create sent to a regional endpoint must.
    let (status, _) = s3(
        &router,
        "PUT",
        "/edi-samples",
        format!(
            "<CreateBucketConfiguration xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">\
             <LocationConstraint>{SIGNING_REGION}</LocationConstraint>\
             </CreateBucketConfiguration>"
        )
        .as_bytes(),
    )
    .await;
    assert_eq!(status, 200, "the sample bucket must be created, got {status}");

    // (case, the outputSampleLocation, the expected status and error code)
    let cases: Vec<(&str, Value, u16, &str)> = vec![
        (
            "a key that bucket does not hold",
            json!({ "bucketName": "edi-samples", "key": "outbound/850.edi" }),
            NOT_FOUND_STATUS,
            "ResourceNotFoundException",
        ),
        (
            "a bucket that does not exist",
            json!({ "bucketName": "no-such-samples", "key": "outbound/850.edi" }),
            NOT_FOUND_STATUS,
            "ResourceNotFoundException",
        ),
        (
            "bucket only",
            json!({ "bucketName": "edi-samples" }),
            400,
            "ValidationException",
        ),
        ("empty location", json!({}), 400, "ValidationException"),
    ];

    for (case, location, expected_status, expected_code) in cases {
        let mut body = request("JSONATA", "X12_850", "VERSION_4010");
        body["outputSampleLocation"] = location;
        let (status, response) = call(&router, "CreateStarterMappingTemplate", body).await;
        assert_eq!(
            status, expected_status,
            "case {case:?}: expected HTTP {expected_status}, got {status} with body {response:?}"
        );
        assert_eq!(
            error_code(&response),
            expected_code,
            "case {case:?}: expected a {expected_code}, got {response:?}"
        );
    }
}
