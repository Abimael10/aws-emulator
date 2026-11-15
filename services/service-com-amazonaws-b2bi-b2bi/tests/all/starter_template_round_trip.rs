// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests that a GENERATED starter mapping template describes a document this
//! service can actually turn into EDI, driven over the wire (awsJson1_0) through the same
//! router the emulator boots.
//!
//! The rule under test: a starter template asks the caller for the fields a mapping
//! SUPPLIES — the data elements of the transaction set's segments and of the interchange,
//! group and transaction HEADERS — and for nothing an EDI writer derives itself, so the
//! document a mapping written against the template produces is one `TestConversion` writes
//! as an X12 interchange. Two members the template therefore does not carry are what this
//! suite is about: the envelope TRAILERS (IEA, GE and SE hold the count of what their
//! envelope encloses and the echo of its control number, which the writer computes) and
//! ISA16 (the component element separator, a delimiter of the file rather than data in it —
//! a placeholder there is not a delimiter at all, and the writer refuses it).
//!
//! The rows differ along what the rule generalises over: the transaction set (so the
//! segments of the body and their element counts), the X12 release and the number of
//! segments the body carries.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::onboarding_loop::{b2bi_router, call, text};

/// The `mappingTemplate` of a generated JSONATA starter template.
async fn starter_template(
    router: &MultiServiceRouter,
    case: &str,
    transaction_set: &str,
    version: &str,
) -> String {
    let (status, body) = call(
        router,
        "CreateStarterMappingTemplate",
        json!({
            "mappingType": "JSONATA",
            "templateDetails": {
                "x12": { "transactionSet": transaction_set, "version": version }
            }
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the template must be generated, got {status} {body}"
    );
    text(&body, "mappingTemplate")
}

/// RULE: the document a generated starter template describes converts to an X12
/// interchange, and the writer supplies the three trailers and the component element
/// separator the template leaves to it.
#[tokio::test]
async fn a_generated_template_is_a_document_this_service_writes_as_edi() {
    let router = b2bi_router();

    // Each row: the case, the `X12TransactionSet` and `X12Version` values, and the segment
    // the transaction set's body opens with.
    let cases: &[(&str, &str, &str, &str)] = &[
        ("850 purchase order", "X12_850", "VERSION_4010", "BEG"),
        ("810 invoice at release 5010", "X12_810", "VERSION_5010", "BIG"),
        ("856 ship notice", "X12_856", "VERSION_4060", "BSN"),
        (
            "997 functional acknowledgment",
            "X12_997",
            "VERSION_4010",
            "AK1",
        ),
        (
            "834 benefit enrollment at release 5010 HIPAA",
            "X12_834",
            "VERSION_5010_HIPAA",
            "BGN",
        ),
    ];

    for (case, transaction_set, version, opening) in cases {
        let template = starter_template(&router, case, transaction_set, version).await;
        let document: Value = serde_json::from_str(&template).unwrap_or_else(|failure| {
            panic!("case {case:?}: a JSONATA template is a JSON document: {failure}: {template}")
        });

        // The template asks for no trailer element and no component element separator.
        for absent in ["IEA_", "GE_", "SE_", "ISA_16"] {
            assert!(
                !template.contains(absent),
                "case {case:?}: a template carries no {absent} member, got {template}"
            );
        }

        // The very document the template describes, handed to the writer.
        let (status, body) = call(
            &router,
            "TestConversion",
            json!({
                "source": { "fileFormat": "JSON", "inputFile": { "fileContent": template } },
                "target": {
                    "fileFormat": "X12",
                    "formatDetails": {
                        "x12": { "transactionSet": transaction_set, "version": version }
                    }
                }
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the document the template describes must convert, got {status} {body}"
        );
        let edi = text(&body, "convertedFileContent");

        // Each segment of the body is written as the X12 segment its element keys name,
        // including the one the transaction set opens with…
        let segments: Vec<String> = document["interchanges"][0]["functional_groups"][0]
            ["transactions"][0]["segments"]
            .as_array()
            .unwrap_or_else(|| panic!("case {case:?}: the transaction carries segments: {template}"))
            .iter()
            .map(|segment| {
                let first = segment
                    .as_object()
                    .and_then(|members| members.keys().next().cloned())
                    .unwrap_or_default();
                first.split('_').next().unwrap_or_default().to_string()
            })
            .collect();
        assert_eq!(
            segments.first().map(String::as_str),
            Some(*opening),
            "case {case:?}: the body must open with {opening}, got {segments:?}"
        );
        for segment in &segments {
            assert!(
                edi.contains(&format!("{segment}*")),
                "case {case:?}: the interchange must carry the {segment} segment, got {edi}"
            );
        }

        // …and the writer supplies the trailers the template left to it, each counting what
        // its envelope really holds: the transaction set's segment count is its body plus
        // the ST/SE pair.
        let included = segments.len() + 2;
        for trailer in [
            format!("SE*{included}*"),
            "GE*1*".to_string(),
            "IEA*1*".to_string(),
        ] {
            assert!(
                edi.contains(&trailer),
                "case {case:?}: the interchange must carry {trailer:?}, got {edi}"
            );
        }
    }
}
