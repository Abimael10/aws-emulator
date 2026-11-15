// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests that `GetDocumentContent` is SERVED from the stores that hold the
//! document, and that serving it is a REPORT rather than a mutation.
//!
//! THE RULE UNDER TEST: a content read answers every call from the state the stores hold at
//! the moment of the call — the document's record on the build-time plane and the bytes the
//! store holding them reports — so the same read repeated answers the very same modelled
//! result (a 200 carrying that content's own media type and byte count, or the resource's
//! own 404), and it changes nothing a later caller sees: the documents the plane holds are
//! the same afterwards, and an address an earlier read handed out still yields the same
//! bytes after later reads of the same and of other documents.
//!
//! This is the invariant the operation's first implementation owes: while it was a scaffold
//! every one of these calls was answered `NotImplemented` (501) and nothing was read at
//! all, so each row below both pins that the call is served and pins that being served
//! consumes nothing — no single-use address, no counter, no status moved by a read.
//!
//! The table spans what the rule generalises over: where the bytes live (text that
//! travelled in the ingestion, bytes that travelled in it under their own declared media
//! type, an object stored with a content type, an object stored without one) and which half
//! of the identity the plane does not hold (the knowledge base, the data source, the
//! document). Every resource is created through the owning plane's own API, as the suite in
//! `tests/all/document_content.rs` does.

use serde_json::json;

use service_com_amazonaws_s3_amazons3::state as s3_state;

use crate::agent_memory::{Emulator, ACCOUNT_KEY, NOT_FOUND};
use crate::document_content::{
    inline_byte_document, inline_text_document, s3_document, Ingested, UNKNOWN_DATA_SOURCE,
    UNKNOWN_KNOWLEDGE_BASE,
};

/// How many times each row of the table is read: enough that a read which consumed, moved
/// or counted anything would answer differently on a later pass.
const PASSES: usize = 3;

/// One read, and what every repetition of it has to answer.
struct Row {
    case: &'static str,
    /// The `knowledgeBaseId` the request carries, or `None` for the one that was created.
    knowledge_base: Option<&'static str>,
    /// The `dataSourceId` the request carries, or `None` for the one that was created.
    data_source: Option<&'static str>,
    document: &'static str,
    /// The status every read of this row answers.
    expected_status: u16,
    /// The media type a served read reports, and `None` for a row that is refused.
    expected_mime_type: Option<String>,
    /// The bytes the address a served read hands out yields.
    expected_content: &'static str,
}

/// What one read answered, so the next read of the same document can be held to it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Observed {
    status: u16,
    error_code: String,
    mime_type: Option<String>,
    content_length: Option<i64>,
}

/// RULE: a content read reports what the stores hold, so repeating it answers the same
/// modelled result and leaves every later read unchanged.
#[tokio::test]
async fn a_content_read_answers_the_same_thing_however_often_it_is_made() {
    let emulator = Emulator::new();
    let source = emulator.custom_source("repeated-content-reads").await;
    let rows = seed(&emulator, &source).await;

    // The documents the build-time plane holds BEFORE any content read, read through that
    // plane's own list operation: the independent view a later caller has of the state a
    // read must not disturb.
    let documents_before = emulator.documents_of(&source, "before any content read").await;

    let mut first: Vec<Observed> = Vec::new();
    let mut addresses: Vec<Option<String>> = Vec::new();
    for pass in 1..=PASSES {
        for (index, row) in rows.iter().enumerate() {
            let case = format!("{} (pass {pass})", row.case);
            let reply = emulator
                .document_content(
                    ACCOUNT_KEY,
                    row.knowledge_base.unwrap_or(&source.knowledge_base_id),
                    row.data_source.unwrap_or(&source.data_source_id),
                    row.document,
                    None,
                )
                .await;
            let answer = reply.json(&case);
            let observed = Observed {
                status: reply.status,
                error_code: reply.error_code(&case),
                mime_type: answer["mimeType"].as_str().map(str::to_string),
                content_length: answer["documentContentLength"].as_i64(),
            };
            assert_eq!(
                observed.status, row.expected_status,
                "case {case:?}: expected {}, got {observed:?}",
                row.expected_status
            );
            if row.expected_status == NOT_FOUND.1 {
                assert_eq!(
                    observed.error_code, NOT_FOUND.0,
                    "case {case:?}: expected {}, got {observed:?}",
                    NOT_FOUND.0
                );
            } else {
                assert_eq!(
                    observed.mime_type, row.expected_mime_type,
                    "case {case:?}: expected the content's own media type {:?}, got {observed:?}",
                    row.expected_mime_type
                );
                assert_eq!(
                    observed.content_length,
                    Some(row.expected_content.len() as i64),
                    "case {case:?}: expected the content's own byte count {}, got {observed:?}",
                    row.expected_content.len()
                );
            }

            let address = answer["presignedUrl"].as_str().map(str::to_string);
            if pass == 1 {
                first.push(observed);
                addresses.push(address);
                continue;
            }
            assert_eq!(
                observed, first[index],
                "case {case:?}: a repeated read must answer what the first read answered \
                 ({:?}), got {observed:?}",
                first[index]
            );
            // The address itself is signed anew on every read — what the rule binds is
            // where it leads, which the fetch below checks against the FIRST read's one.
            assert_eq!(
                address.is_some(),
                addresses[index].is_some(),
                "case {case:?}: a repeated read must hand out an address exactly when the \
                 first read did ({:?}), got {address:?}",
                addresses[index]
            );
        }
    }

    // The address the FIRST read handed out, followed after every later read: a reporting
    // operation hands out nothing single-use, so it still yields the ingested bytes.
    for (index, row) in rows.iter().enumerate() {
        let Some(url) = addresses[index].clone() else {
            assert_eq!(
                row.expected_status, NOT_FOUND.1,
                "case {:?}: a served read must hand out an address, got none",
                row.case
            );
            continue;
        };
        let (status, body) = emulator.follow(row.case, &url).await;
        assert_eq!(
            status,
            200,
            "case {:?}: the first read's address must still serve after {PASSES} reads, got \
             {status} {:?}",
            row.case,
            String::from_utf8_lossy(&body)
        );
        assert_eq!(
            String::from_utf8_lossy(&body),
            row.expected_content,
            "case {:?}: the first read's address must still yield the ingested bytes",
            row.case
        );
    }

    // And the plane a read reads holds exactly the documents it held before it.
    let documents_after = emulator
        .documents_of(&source, "after every content read")
        .await;
    assert_eq!(
        documents_after, documents_before,
        "the documents the plane holds must be unchanged by content reads: expected \
         {documents_before:?}, got {documents_after:?}"
    );
}

/// Ingest one document of every kind the rule spans and answer the table of reads over
/// them.
async fn seed(emulator: &Emulator, source: &Ingested) -> Vec<Row> {
    let typed_object = "repeat/typed.csv";
    let untyped_object = "repeat/untyped.bin";
    emulator.put_object(typed_object, b"a,b\n1,2\n", Some("text/csv"));
    emulator.put_object(untyped_object, b"opaque bytes", None);

    let documents = json!([
        inline_text_document("repeat-inline-text", "the valve is replaced every 400 hours"),
        inline_byte_document("repeat-inline-bytes", "application/pdf", b"%PDF-1.4 tiny"),
        s3_document("repeat-object-typed", typed_object),
        s3_document("repeat-object-untyped", untyped_object),
    ]);
    emulator
        .ingest_documents("the documents every read below is made over", source, documents)
        .await;

    vec![
        Row {
            case: "text that travelled in the ingestion",
            knowledge_base: None,
            data_source: None,
            document: "repeat-inline-text",
            expected_status: 200,
            expected_mime_type: Some("text/plain".to_string()),
            expected_content: "the valve is replaced every 400 hours",
        },
        Row {
            case: "bytes that travelled in the ingestion under their declared media type",
            knowledge_base: None,
            data_source: None,
            document: "repeat-inline-bytes",
            expected_status: 200,
            expected_mime_type: Some("application/pdf".to_string()),
            expected_content: "%PDF-1.4 tiny",
        },
        Row {
            case: "an object stored with a content type",
            knowledge_base: None,
            data_source: None,
            document: "repeat-object-typed",
            expected_status: 200,
            expected_mime_type: Some("text/csv".to_string()),
            expected_content: "a,b\n1,2\n",
        },
        Row {
            // S3's documented default for an object written without a content type, which
            // is what a `GetObject` of the same object reports.
            case: "an object stored without a content type",
            knowledge_base: None,
            data_source: None,
            document: "repeat-object-untyped",
            expected_status: 200,
            expected_mime_type: Some(s3_state::DEFAULT_CONTENT_TYPE.to_string()),
            expected_content: "opaque bytes",
        },
        Row {
            case: "a knowledge base the plane does not hold",
            knowledge_base: Some(UNKNOWN_KNOWLEDGE_BASE),
            data_source: None,
            document: "repeat-inline-text",
            expected_status: NOT_FOUND.1,
            expected_mime_type: None,
            expected_content: "",
        },
        Row {
            case: "a data source the plane does not hold",
            knowledge_base: None,
            data_source: Some(UNKNOWN_DATA_SOURCE),
            document: "repeat-inline-text",
            expected_status: NOT_FOUND.1,
            expected_mime_type: None,
            expected_content: "",
        },
        Row {
            case: "a document that was never ingested",
            knowledge_base: None,
            data_source: None,
            document: "repeat-never-ingested",
            expected_status: NOT_FOUND.1,
            expected_mime_type: None,
            expected_content: "",
        },
    ]
}

impl Emulator {
    /// The identifiers of the documents the build-time plane holds for `source`, sorted, as
    /// its own `ListKnowledgeBaseDocuments` reports them.
    ///
    /// The identifiers alone, never the per-document status: a document's status is moved
    /// by that plane's own clock, which owes the transition whether or not anybody calls.
    async fn documents_of(&self, source: &Ingested, case: &str) -> Vec<String> {
        let reply = self
            .bedrock(
                ACCOUNT_KEY,
                "POST",
                &format!(
                    "/knowledgebases/{}/datasources/{}/documents",
                    source.knowledge_base_id, source.data_source_id
                ),
                Some(json!({})),
            )
            .await;
        assert_eq!(
            reply.status,
            200,
            "case {case:?}: ListKnowledgeBaseDocuments must answer 200, got {} {:?}",
            reply.status,
            reply.json(case)
        );
        let answer = reply.json(case);
        let mut identifiers: Vec<String> = answer["documentDetails"]
            .as_array()
            .unwrap_or_else(|| panic!("case {case:?}: no documentDetails in {answer:?}"))
            .iter()
            .map(|detail| {
                let identifier = &detail["identifier"];
                assert!(
                    identifier.is_object(),
                    "case {case:?}: every document detail must carry the identifier the \
                     document was ingested under, got {detail:?}"
                );
                identifier.to_string()
            })
            .collect();
        identifiers.sort();
        identifiers
    }
}
