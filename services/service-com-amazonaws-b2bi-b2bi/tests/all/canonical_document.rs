// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the CANONICAL EDI-as-JSON document this service answers with,
//! driven over the wire (awsJson1_0, `x-amz-target: B2BI.<Operation>`) through the same
//! router the emulator boots, with the emulator's own Amazon S3 service mounted beside
//! b2bi — the EDI documents these operations read are S3 objects.
//!
//! The rule under test: the document an EDI interchange is canonicalized into names each
//! envelope element `<SEGMENT>_<position>_<canonical element name>` — the service's own
//! short form of the ASC X12 element titles, `ISA_06_SenderId` and not
//! `ISA_06_InterchangeSenderId` — and groups a transaction set's repeating detail segment
//! into ONE `<SEGMENT>-8000_loop` entry of that transaction's `segments`, holding one
//! object per occurrence. Every operation that answers the canonical document answers the
//! same member names: `TestParsing`'s `parsedFileContent` and the `mappingTemplate`
//! `CreateStarterMappingTemplate` generates, which `outputSampleLocation` fills from the
//! sample — its envelope with the sample's values themselves and each element position of a
//! body segment with the placeholder carrying the value the sample holds there.
//!
//! Each test states that rule over a TABLE whose rows differ along what it generalises
//! over: the element (and so its canonical name), the transaction set (and so the segment
//! its detail loop repeats), how many occurrences that loop has, whether an occurrence
//! holds subordinate segments, and the sample's own values.

use serde_json::{json, Value};

use crate::edi_parsing::{given_object, BUCKET};
use crate::onboarding_loop::{b2bi_router, call};
use crate::test_parsing::{
    body_segments_in_order, interchange as x12_interchange, purchase_order, STANDARD,
};
// The handler's own placeholder constant, so this suite cannot drift from the value it
// asserts on; `MAPPING_TYPE_VALUES` is the model's own member list.
use service_com_amazonaws_b2bi_b2bi::ops::create_starter_mapping_template::PLACEHOLDER;
use service_com_amazonaws_b2bi_b2bi::ops::types::MAPPING_TYPE_VALUES;

/// An X12 interchange carrying one transaction set of `transaction_set` whose body is
/// `body`, written with the classic delimiters (`*`, `>`, `~`).
///
/// The envelope values are fixed per call so a row can assert which of them the canonical
/// document carries and under which name: `sender` is ISA06 and `control` is ISA13.
fn interchange(transaction_set: &str, sender: &str, control: &str, body: &[&str]) -> String {
    let mut document = format!(
        "ISA*00*          *00*          *ZZ*{sender:<15}*ZZ*RECEIVER       \
         *240101*1200*U*00401*{control}*0*P*>~\
         GS*PO*{sender}*RECEIVER*20240101*1200*1*X*004010~\
         ST*{transaction_set}*0001~"
    );
    for segment in body {
        document.push_str(segment);
        document.push('~');
    }
    document.push_str(&format!(
        "SE*{}*0001~GE*1*1~IEA*1*{control}~",
        body.len() + 2
    ));
    document
}

/// The parsed document `TestParsing` answers for the object at `key`, as JSON.
async fn parsed_document(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    key: &str,
    transaction_set: &str,
) -> Value {
    let (status, body) = call(
        router,
        "TestParsing",
        json!({
            "inputFile": { "bucketName": BUCKET, "key": key },
            "fileFormat": "JSON",
            "ediType": {
                "x12Details": { "transactionSet": transaction_set, "version": "VERSION_4010" }
            }
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: TestParsing must parse the sample, got {status} {body}"
    );
    let content = body["parsedFileContent"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a parsedFileContent, got {body}"));
    serde_json::from_str(content).unwrap_or_else(|failure| {
        panic!("case {case:?}: parsedFileContent must be a JSON document: {failure}: {content}")
    })
}

/// The `mappingTemplate` `CreateStarterMappingTemplate` generates for the sample at `key`,
/// as JSON.
async fn derived_template(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    key: &str,
    transaction_set: &str,
) -> Value {
    let (status, body) = call(
        router,
        "CreateStarterMappingTemplate",
        json!({
            "mappingType": "JSONATA",
            "templateDetails": {
                "x12": { "transactionSet": transaction_set, "version": "VERSION_4010" }
            },
            "outputSampleLocation": { "bucketName": BUCKET, "key": key }
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the template must be derived from the sample, got {status} {body}"
    );
    let template = body["mappingTemplate"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a mappingTemplate, got {body}"));
    serde_json::from_str(template).unwrap_or_else(|failure| {
        panic!("case {case:?}: a JSONATA template must be a JSON document: {failure}: {template}")
    })
}

/// The one loop entry of `segments`, as `(member name, occurrences)`, or `None` when no
/// entry of the list is a loop.
fn detail_loop(segments: &[Value]) -> Option<(String, Vec<Value>)> {
    segments.iter().find_map(|entry| {
        let members = entry.as_object()?;
        let (key, value) = members.iter().next()?;
        if members.len() != 1 || !key.ends_with("_loop") {
            return None;
        }
        Some((key.clone(), value.as_array().cloned().unwrap_or_default()))
    })
}

/// RULE: every envelope element of the canonical document is keyed
/// `<SEGMENT>_<position>_<canonical element name>`, and the canonical name is the
/// service's own short form of the ASC X12 element title.
///
/// The rows are the elements themselves — one per envelope segment of the interchange,
/// including elements this emulator's recorded canonical documents do not cite
/// (`ISA_05_SenderQualifier`, `ISA_09_Date`, `GS_02_ApplicationSenderCode`,
/// `IEA_02_InterchangeControlNumber`) — and each row's value is the one the sample
/// interchange carries at that position, so the assertion is about the NAME the value
/// arrives under. Both operations that answer the canonical document are asserted with the
/// same table: a starter template describes the very document the reader builds, so the two
/// cannot name an element differently. The envelope TRAILERS and ISA16 are asserted of the
/// parsed document alone, because a template leaves to the EDI writer what the writer
/// derives itself (`tests/all/starter_template_round_trip.rs`).
#[tokio::test]
async fn every_envelope_element_carries_its_canonical_member_name() {
    let router = b2bi_router();

    let sample = interchange("850", "SENDER", "000000007", &["BEG*00*SA*PO-7**20240101"]);
    let key = "canonical/envelope-850.edi";
    given_object(&router, key, &sample).await;

    // Each row: the member name and the value the sample carries at that position. The
    // ISA11 name is release-dependent (release 004010 here, below the release where the
    // Repetition Separator takes position 11).
    let interchange_elements: &[(&str, &str)] = &[
        ("ISA_01_AuthorizationQualifier", "00"),
        ("ISA_03_SecurityQualifier", "00"),
        ("ISA_05_SenderQualifier", "ZZ"),
        ("ISA_06_SenderId", "SENDER         "),
        ("ISA_07_ReceiverQualifier", "ZZ"),
        ("ISA_08_ReceiverId", "RECEIVER       "),
        ("ISA_09_Date", "240101"),
        ("ISA_10_Time", "1200"),
        ("ISA_11_StandardsId", "U"),
        ("ISA_12_Version", "00401"),
        ("ISA_13_InterchangeControlNumber", "000000007"),
        ("ISA_14_AcknowledgmentRequested", "0"),
        ("ISA_15_TestIndicator", "P"),
    ];
    let group_elements: &[(&str, &str)] = &[
        ("GS_01_FunctionalIdentifierCode", "PO"),
        ("GS_02_ApplicationSenderCode", "SENDER"),
        ("GS_03_ApplicationReceiverCode", "RECEIVER"),
        ("GS_04_Date", "20240101"),
        ("GS_05_Time", "1200"),
        ("GS_06_GroupControlNumber", "1"),
        ("GS_07_ResponsibleAgencyCode", "X"),
        ("GS_08_Version", "004010"),
    ];
    let transaction_elements: &[(&str, &str)] = &[
        ("ST_01_TransactionSetIdentifierCode", "850"),
        ("ST_02_TransactionSetControlNumber", "0001"),
    ];
    // The members a parsed document carries and a template leaves to the writer.
    let trailer_elements: &[(&str, &str)] = &[
        ("ISA_16_ComponentSeparator", ">"),
        ("IEA_01_NumberOfIncludedFunctionalGroups", "1"),
        ("IEA_02_InterchangeControlNumber", "000000007"),
    ];
    let group_trailer_elements: &[(&str, &str)] = &[
        ("GE_01_NumberOfTransactionSetsIncluded", "1"),
        ("GE_02_GroupControlNumber", "1"),
    ];
    let transaction_trailer_elements: &[(&str, &str)] = &[
        ("SE_01_NumberOfIncludedSegments", "3"),
        ("SE_02_TransactionSetControlNumber", "0001"),
    ];

    let parsed = parsed_document(&router, "parsed document", key, "X12_850").await;
    let template = derived_template(&router, "derived template", key, "X12_850").await;

    for (document_case, document) in [("parsed document", &parsed), ("derived template", &template)]
    {
        let reported = &document["interchanges"][0];
        let group = &reported["functional_groups"][0];
        let transaction = &group["transactions"][0];
        for (scope, object, elements) in [
            ("interchange", reported, interchange_elements),
            ("functional group", group, group_elements),
            ("transaction set", transaction, transaction_elements),
        ] {
            for (member, value) in elements {
                assert_eq!(
                    object[member].as_str(),
                    Some(*value),
                    "case {document_case:?}: the {scope}'s {member} must carry {value:?}, got \
                     {object}"
                );
            }
        }
    }

    // The trailers and the component element separator: the parsed document carries them
    // under their canonical names, and a template carries none of them at all.
    let reported = &parsed["interchanges"][0];
    let group = &reported["functional_groups"][0];
    let transaction = &group["transactions"][0];
    for (scope, object, elements) in [
        ("interchange", reported, trailer_elements),
        ("functional group", group, group_trailer_elements),
        ("transaction set", transaction, transaction_trailer_elements),
    ] {
        for (member, value) in elements {
            assert_eq!(
                object[member].as_str(),
                Some(*value),
                "the parsed {scope}'s {member} must carry {value:?}, got {object}"
            );
            assert!(
                !template.to_string().contains(member),
                "a template leaves {member} to the EDI writer, got {template}"
            );
        }
    }

    // The canonical names REPLACE the X12 titles: a member named by the title is absent
    // from the whole document, not carried beside the canonical one.
    let rendered = format!("{parsed}{template}");
    for title in [
        "InterchangeSenderId",
        "InterchangeReceiverId",
        "AuthorizationInformationQualifier",
        "InterchangeIdQualifier",
        "UsageIndicator",
        "VersionReleaseIndustryIdentifierCode",
        "ApplicationSendersCode",
    ] {
        assert!(
            !rendered.contains(title),
            "the canonical document must not name an element {title:?}, got {rendered}"
        );
    }
}

/// One row of the detail-loop table: the case, the transaction set code, the
/// `X12TransactionSet` the request names, the body segments of the sample, the loop member
/// name the canonical document must carry (`None` for a transaction set whose detail loop
/// this emulator does not carry), how many occurrences that loop has, and the segment ids
/// the entries of `segments` must be, loops flattened.
type LoopCase<'a> = (
    &'a str,
    &'a str,
    &'a str,
    Vec<&'a str>,
    Option<&'a str>,
    usize,
    Vec<&'a str>,
);

/// RULE: a transaction set whose detail table repeats carries that repetition as ONE entry
/// of the transaction's `segments`, keyed `<SEGMENT>-8000_loop`, holding one object per
/// occurrence — and the segments the X12 standard places in the summary table stay beside
/// that loop rather than inside its last occurrence.
///
/// The rows differ along the transaction set (and so the segment its detail loop repeats),
/// the number of occurrences, whether an occurrence holds subordinate segments, whether a
/// summary segment follows the loop, and whether this emulator carries a detail loop for
/// that transaction set at all.
#[tokio::test]
async fn a_detail_loop_is_grouped_as_one_entry_per_occurrence() {
    let router = b2bi_router();

    let cases: Vec<LoopCase> = vec![
        (
            "850 purchase order with one line item",
            "850",
            "X12_850",
            vec!["BEG*00*SA*PO-1**20240101", "PO1*1*10*EA*9.99**VP*WIDGET-A"],
            Some("PO1-8000_loop"),
            1,
            vec!["BEG", "PO1"],
        ),
        (
            "850 purchase order with two line items, each with a description",
            "850",
            "X12_850",
            vec![
                "BEG*00*SA*PO-2**20240101",
                "PO1*1*10*EA*9.99**VP*WIDGET-A",
                "PID*F****First widget",
                "PO1*2*20*EA*8.88**VP*WIDGET-B",
                "PID*F****Second widget",
                "CTT*2",
            ],
            Some("PO1-8000_loop"),
            2,
            vec!["BEG", "PO1", "PID", "PO1", "PID", "CTT"],
        ),
        (
            "810 invoice, whose detail table repeats its baseline item data",
            "810",
            "X12_810",
            vec![
                "BIG*20240201*INV-9*20240101*PO-2",
                "IT1*1*5*EA*4.44**VP*WIDGET-A",
                "IT1*2*6*EA*3.33**VP*WIDGET-B",
                "TDS*4000",
            ],
            Some("IT1-8000_loop"),
            2,
            vec!["BIG", "IT1", "IT1", "TDS"],
        ),
        (
            "856 ship notice, whose detail table repeats its hierarchical levels",
            "856",
            "X12_856",
            vec![
                "BSN*00*SHIP-1*20240101*1200",
                "HL*1**S",
                "HL*2*1*O",
                "CTT*2",
            ],
            Some("HL-8000_loop"),
            2,
            vec!["BSN", "HL", "HL", "CTT"],
        ),
        (
            "997 acknowledgment, whose detail table repeats its transaction set responses",
            "997",
            "X12_997",
            vec!["AK1*PO*1", "AK2*850*0001", "AK2*850*0002", "AK9*A*2*2*2"],
            Some("AK2-8000_loop"),
            2,
            vec!["AK1", "AK2", "AK2", "AK9"],
        ),
        (
            "870 order status report, a transaction set with no detail loop here",
            "870",
            "X12_870",
            vec!["BSR*4*PO*STATUS-1*20240101", "HL*1**S", "HL*2*1*O"],
            None,
            0,
            vec!["BSR", "HL", "HL"],
        ),
    ];

    for (case, code, transaction_set, body, loop_member, occurrences, segment_ids) in cases {
        let key = format!("canonical/loop-{code}-{occurrences}.edi");
        let sample = interchange(code, "SENDER", "000000001", &body);
        given_object(&router, &key, &sample).await;

        let parsed = parsed_document(&router, case, &key, transaction_set).await;
        let segments = parsed["interchanges"][0]["functional_groups"][0]["transactions"][0]
            ["segments"]
            .as_array()
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: expected the transaction's segments"));

        match loop_member {
            Some(expected) => {
                let (member, items) = detail_loop(&segments).unwrap_or_else(|| {
                    panic!("case {case:?}: expected a {expected} entry, got {segments:?}")
                });
                assert_eq!(
                    member, expected,
                    "case {case:?}: the loop must be keyed {expected}, got {member:?}"
                );
                assert_eq!(
                    items.len(),
                    occurrences,
                    "case {case:?}: expected {occurrences} occurrences, got {items:?}"
                );
                // Each occurrence carries its own opening segment, so a mapping addresses
                // the second line item as the second entry of the loop.
                let opening = expected.split('-').next().unwrap_or_default();
                for (index, occurrence) in items.iter().enumerate() {
                    assert_eq!(
                        occurrence["segment_id"].as_str(),
                        Some(opening),
                        "case {case:?}: occurrence {index} must be a {opening} segment, got \
                         {occurrence}"
                    );
                }
            }
            None => assert!(
                detail_loop(&segments).is_none(),
                "case {case:?}: a transaction set with no detail loop here reports its segments \
                 flat, got {segments:?}"
            ),
        }

        // Whatever the grouping, every segment of the body is reported once, in document
        // order.
        let reported: Vec<String> = body_segments_in_order(&segments)
            .iter()
            .map(|segment| segment["segment_id"].as_str().unwrap_or_default().to_string())
            .collect();
        assert_eq!(
            reported, segment_ids,
            "case {case:?}: expected the body segments {segment_ids:?}, got {reported:?}"
        );
    }
}

/// One row of the sample table: the case, the sample's sender (ISA06), its interchange
/// control number (ISA13), its body segments, the (element position, sample value) pairs the
/// template must carry as the placeholder CARRYING that value outside the detail loop, and
/// one list of the same pairs per OCCURRENCE of that loop.
type SampleCase<'a> = (
    &'a str,
    &'a str,
    &'a str,
    Vec<&'a str>,
    Vec<(&'a str, &'a str)>,
    Vec<Vec<(&'a str, &'a str)>>,
);

/// The value the transaction's `segments` carry at the element position `key`, a detail
/// loop's occurrences searched with them.
fn element_value(segments: &[Value], key: &str) -> Option<String> {
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
    segments.iter().find_map(|segment| found(segment, key))
}

/// RULE: a starter mapping template generated for `outputSampleLocation` carries what THAT
/// sample holds at each element position it describes — the envelope elements the sample's
/// values themselves, so a caller's mapping writes the envelope its own documents really
/// have, and each element position of a body segment the placeholder carrying the value the
/// sample holds there, grouped into the same detail loop the reader read, one object per
/// occurrence — while the element positions the document asks for stay the requested
/// transaction set's and a position the sample leaves empty stays the bare placeholder.
///
/// The rows differ along the sample's envelope values, how many line items it holds and
/// which business elements it fills.
#[tokio::test]
async fn a_template_generated_for_a_sample_carries_its_values_as_placeholders() {
    let router = b2bi_router();

    let cases: Vec<SampleCase> = vec![
        (
            "one line item",
            "SENDER",
            "000000001",
            vec!["BEG*00*SA*PO123456**20240101", "PO1*1*10*EA*9.99**VP*WIDGET-A"],
            vec![("BEG_01", "00"), ("BEG_03", "PO123456")],
            vec![vec![("PO1_01", "1"), ("PO1_07", "WIDGET-A")]],
        ),
        (
            "another sender, another control number, two line items",
            "ACME",
            "000000042",
            vec![
                "BEG*05*RE*PO-RUSH-9**20240202",
                "PO1*1*1*CA*1.50**VP*BOLT-M6*VN*SUP-77",
                "PO1*2*2*CA*2.50**VP*NUT-M6*VN*SUP-78",
            ],
            vec![("BEG_03", "PO-RUSH-9"), ("BEG_05", "20240202")],
            vec![
                vec![("PO1_07", "BOLT-M6"), ("PO1_09", "SUP-77")],
                vec![("PO1_07", "NUT-M6"), ("PO1_09", "SUP-78")],
            ],
        ),
    ];

    for (case, sender, control, body, heading, occurrences) in cases {
        let key = format!("canonical/sample-{control}.edi");
        let sample = interchange("850", sender, control, &body);
        given_object(&router, &key, &sample).await;

        let template = derived_template(&router, case, &key, "X12_850").await;
        let reported = &template["interchanges"][0];

        // The envelope is the sample's own, verbatim: the caller does not map it. ISA06 is
        // the fixed-width field X12 pads, so it is compared trimmed.
        assert_eq!(
            reported["ISA_06_SenderId"].as_str().map(str::trim),
            Some(sender),
            "case {case:?}: ISA06 must carry the sample's sender, got {reported}"
        );
        assert_eq!(
            reported["ISA_13_InterchangeControlNumber"].as_str(),
            Some(control),
            "case {case:?}: ISA13 must carry the sample's control number, got {reported}"
        );

        // …and each element position of the body carries what the sample holds there, as the
        // placeholder carrying it.
        let transaction = &reported["functional_groups"][0]["transactions"][0];
        let segments = transaction["segments"]
            .as_array()
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: expected the transaction's segments"));
        for (position, value) in &heading {
            let expected = format!("{PLACEHOLDER}: {value}");
            let actual = element_value(&segments, position);
            assert_eq!(
                actual.as_deref(),
                Some(expected.as_str()),
                "case {case:?}: {position} must carry the sample's value as a placeholder, \
                 got {actual:?} in {segments:?}"
            );
        }
        // A position the sample leaves empty stays the bare placeholder: the element list is
        // the requested transaction set's, not the sample's.
        for position in ["BEG_04", "BEG_12", "CUR_01"] {
            let actual = element_value(&segments, position);
            assert_eq!(
                actual.as_deref(),
                Some(PLACEHOLDER),
                "case {case:?}: {position} must be the bare placeholder, got {actual:?} in \
                 {segments:?}"
            );
        }

        // The line items are the loop the reader grouped them into, one object per
        // occurrence, so a mapping addresses the second line item as the second entry.
        let (member, items) = detail_loop(&segments)
            .unwrap_or_else(|| panic!("case {case:?}: expected a line-item loop, got {segments:?}"));
        assert_eq!(
            member, "PO1-8000_loop",
            "case {case:?}: the loop must be keyed as the reader keys it, got {member:?}"
        );
        assert_eq!(
            items.len(),
            occurrences.len(),
            "case {case:?}: the loop must hold one entry per line item of the sample, got \
             {items:?}"
        );
        for (index, (occurrence, expected)) in items.iter().zip(&occurrences).enumerate() {
            assert!(
                occurrence["segment_id"].is_null(),
                "case {case:?}: occurrence {index} carries no reader marker, got {occurrence}"
            );
            for (position, value) in expected {
                let wanted = format!("{PLACEHOLDER}: {value}");
                assert_eq!(
                    occurrence[position].as_str(),
                    Some(wanted.as_str()),
                    "case {case:?}: occurrence {index} must carry {position} from the \
                     sample, got {occurrence}"
                );
            }
        }
    }
}

/// The member name of interchange header element 11 before X12 release 004020: the
/// Interchange Control Standards Identifier, in the canonical short form.
const ISA11_PRE_4020: &str = "ISA_11_StandardsId";

/// The member name of interchange header element 11 from X12 release 004020 on, where the
/// Repetition Separator takes that position.
const ISA11_FROM_4020: &str = "ISA_11_RepetitionSeparator";

/// The interchange header's data elements, in positional order, as a document at a release
/// below 004020 keys them.
///
/// Source: the canonical short form of each element's ASC X12 interchange control header
/// title, the four recorded names among them as the service's own element table records
/// (`ISA_01_AuthorizationQualifier`, `ISA_06_SenderId`,
/// `ISA_13_InterchangeControlNumber`, `ISA_15_TestIndicator`). ISA16, the component element
/// separator, is a delimiter of the FILE rather than data in it, so a starter template
/// carries no member for it.
const PRE_4020_INTERCHANGE_MEMBERS: &[&str] = &[
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
    ISA11_PRE_4020,
    "ISA_12_Version",
    "ISA_13_InterchangeControlNumber",
    "ISA_14_AcknowledgmentRequested",
    "ISA_15_TestIndicator",
];

/// One row of the release table: the case, the `X12Version` the request names, the ISA12
/// Interchange Control Version Number a sample at that release declares, and the member
/// name position 11 takes there.
///
/// Every `X12Version` value is a row
/// (`smithy_query(query="shape",shape="X12Version")`: VERSION_4010, VERSION_4030,
/// VERSION_4050, VERSION_4060, VERSION_5010, VERSION_5010_HIPAA), which spans the one bound
/// the name of position 11 follows — release 004020, below it the Interchange Control
/// Standards Identifier, from it on the Repetition Separator.
fn release_cases() -> Vec<(&'static str, &'static str, &'static str, &'static str)> {
    vec![
        ("release 4010", "VERSION_4010", "00401", ISA11_PRE_4020),
        ("release 4030", "VERSION_4030", "00403", ISA11_FROM_4020),
        ("release 4050", "VERSION_4050", "00405", ISA11_FROM_4020),
        ("release 4060", "VERSION_4060", "00406", ISA11_FROM_4020),
        ("release 5010", "VERSION_5010", "00501", ISA11_FROM_4020),
        (
            "release 5010 HIPAA",
            "VERSION_5010_HIPAA",
            "00501",
            ISA11_FROM_4020,
        ),
    ]
}

/// The `mappingTemplate` a `CreateStarterMappingTemplate` call answers with, in the named
/// mapping type and at the named version, deriving it from the sample at `sample_key` when
/// one is given.
async fn template_text(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    mapping_type: &str,
    version: &str,
    sample_key: Option<&str>,
) -> String {
    let mut request = json!({
        "mappingType": mapping_type,
        "templateDetails": { "x12": { "transactionSet": "X12_850", "version": version } }
    });
    if let Some(key) = sample_key {
        request["outputSampleLocation"] = json!({ "bucketName": BUCKET, "key": key });
    }
    let (status, body) = call(router, "CreateStarterMappingTemplate", request).await;
    assert_eq!(
        status, 200,
        "case {case:?}: a starter template must be generated, got {status} {body}"
    );
    body["mappingTemplate"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a mappingTemplate, got {body}"))
        .to_string()
}

/// The interchange object of the document `TestParsing` answers for the object at `key`,
/// read at the named version.
async fn parsed_interchange(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    key: &str,
    version: &str,
) -> Value {
    let (status, body) = call(
        router,
        "TestParsing",
        json!({
            "inputFile": { "bucketName": BUCKET, "key": key },
            "fileFormat": "JSON",
            "ediType": { "x12Details": { "transactionSet": "X12_850", "version": version } }
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: TestParsing must parse the sample, got {status} {body}"
    );
    let content = body["parsedFileContent"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a parsedFileContent, got {body}"));
    let document: Value = serde_json::from_str(content).unwrap_or_else(|failure| {
        panic!("case {case:?}: parsedFileContent must be a JSON document: {failure}: {content}")
    });
    document["interchanges"][0].clone()
}

/// Every element name of the XSLT stylesheet `xml` in document order. Panics when the
/// stylesheet is not well-formed, which is how a generated one is asserted to parse.
fn xml_element_names(case: &str, xml: &str) -> Vec<String> {
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut names: Vec<String> = Vec::new();
    loop {
        match reader.read_event() {
            Ok(quick_xml::events::Event::Eof) => break,
            Ok(quick_xml::events::Event::Start(start))
            | Ok(quick_xml::events::Event::Empty(start)) => {
                names.push(String::from_utf8_lossy(start.name().into_inner()).to_string());
            }
            Ok(_) => {}
            Err(failure) => panic!(
                "case {case:?}: an XSLT template must be well-formed XML, got {failure}: {xml}"
            ),
        }
    }
    names
}

/// The member names of the interchange header a generated template of `mapping_type`
/// carries, in positional order: the keys of the JSON document's interchange object for
/// `JSONATA`, the element names of the stylesheet for `XSLT`. The two-digit position in
/// each key makes the keys sort positionally.
fn template_interchange_members(case: &str, mapping_type: &str, template: &str) -> Vec<String> {
    let prefix = "ISA_";
    let mut members: Vec<String> = if mapping_type == "JSONATA" {
        let document: Value = serde_json::from_str(template).unwrap_or_else(|failure| {
            panic!("case {case:?}: a JSONATA template must be a JSON document: {failure}")
        });
        document["interchanges"][0]
            .as_object()
            .map(|members| members.keys().cloned().collect())
            .unwrap_or_default()
    } else {
        xml_element_names(case, template)
    };
    members.retain(|member| member.starts_with(prefix));
    members.sort();
    members
}

/// RULE: the interchange header's element at position 11 is keyed by the canonical short
/// form of the title the requested X12 release gives it — the Interchange Control Standards
/// Identifier below release 004020, the Repetition Separator from it on — and the starter
/// template (in either mapping type) and the parsed document, which describe the same
/// canonical document, key it the same way.
///
/// The rows are every `X12Version` the model declares ([`release_cases`]), crossed with
/// every `MappingType`, so the table spans both sides of the one release bound the name
/// follows and both renderings of the document.
#[tokio::test]
async fn interchange_element_eleven_is_keyed_by_its_canonical_name_at_every_release() {
    let router = b2bi_router();

    for (case, version, isa12, expected) in release_cases() {
        let other = if expected == ISA11_PRE_4020 {
            ISA11_FROM_4020
        } else {
            ISA11_PRE_4020
        };

        for mapping_type in MAPPING_TYPE_VALUES {
            let case = format!("{case} {mapping_type}");
            let template = template_text(&router, &case, mapping_type, version, None).await;
            let members = template_interchange_members(&case, mapping_type, &template);
            assert!(
                members.iter().any(|member| member == expected),
                "case {case:?}: the generated template's interchange header must carry \
                 <{expected}>, got {members:?}"
            );
            assert!(
                !members.iter().any(|member| member == other),
                "case {case:?}: element 11 must be named once, not also <{other}>, got \
                 {members:?}"
            );
        }

        // The X12 READER names the element the same way at the same release: a starter
        // template describes the very document the reader answers, so the two cannot name
        // one element differently.
        let sample = x12_interchange(
            STANDARD,
            "SENDERID",
            "RECEIVERID",
            isa12,
            &[("PO", vec![purchase_order("0001", "PO-11")])],
        );
        let key = format!("canonical/isa11-{version}.edi");
        given_object(&router, &key, &sample).await;
        let parsed = parsed_interchange(&router, case, &key, version).await;
        assert_eq!(
            parsed[expected].as_str(),
            Some("U"),
            "case {case:?}: the parsed document must report element 11 as <{expected}> \
             carrying the interchange's own value, got {parsed}"
        );
        assert!(
            parsed.get(other).is_none(),
            "case {case:?}: the parsed document must name element 11 once, not also \
             <{other}>, got {parsed}"
        );
    }
}

/// RULE: every data element of the interchange header is keyed
/// `ISA_<two-digit position>_<canonical short-form name>`
/// ([`PRE_4020_INTERCHANGE_MEMBERS`]), whichever mapping type renders the template and
/// whether or not the request names a sample to take the envelope's VALUES from.
#[tokio::test]
async fn every_interchange_header_element_is_keyed_by_its_canonical_name() {
    let router = b2bi_router();

    let sample = x12_interchange(
        STANDARD,
        "SENDERID",
        "RECEIVERID",
        "00401",
        &[("PO", vec![purchase_order("0001", "PO-12")])],
    );
    let key = "canonical/isa-names-4010.edi";
    given_object(&router, key, &sample).await;

    for mapping_type in MAPPING_TYPE_VALUES {
        for (sample_case, sample_key) in [("no sample", None), ("with a sample", Some(key))] {
            let case = format!("{mapping_type} {sample_case}");
            let template =
                template_text(&router, &case, mapping_type, "VERSION_4010", sample_key).await;
            let members = template_interchange_members(&case, mapping_type, &template);
            assert_eq!(
                members, PRE_4020_INTERCHANGE_MEMBERS,
                "case {case:?}: the interchange header's members must be the canonical \
                 short-form names, got {members:?}"
            );
        }
    }

    // The reader's own document carries the same elements under the same names, and beyond
    // them the ISA16 component element separator a template leaves to the EDI writer.
    let parsed = parsed_interchange(&router, "parsed document", key, "VERSION_4010").await;
    for expected in PRE_4020_INTERCHANGE_MEMBERS {
        assert!(
            parsed.get(expected).is_some(),
            "case \"parsed document\": the interchange header must carry <{expected}>, got \
             {parsed}"
        );
    }
}
