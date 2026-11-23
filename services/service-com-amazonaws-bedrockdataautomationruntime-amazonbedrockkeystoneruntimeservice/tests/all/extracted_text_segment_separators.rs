// SPDX-License-Identifier: CC0-1.0

//! The SHAPE of the extracted text a finished data-automation job writes into its
//! standard-output document, read back out of the object store the job wrote it to.
//!
//! THE RULE: the extraction assembles `representation.text` by concatenating each text
//! segment it read with the separator that FOLLOWS that segment, so the separator belongs to
//! the segment it trails and the concatenation is never trimmed: every word carries one
//! space, the last word of a line included, and the lines of a page (and the pages of a
//! document) stay joined by the line separator. A consumer that slices the text by offsets
//! taken from the sibling blocks, hashes or diffs it, or appends the next extraction to it
//! depends on exactly that tail.
//!
//! The rows differ along what the rule generalises over: how many SEGMENTS a line holds (one
//! word, several), how many LINES the text holds (one, several, with blank lines around and
//! between them), how the source SPACED those segments (single spaces, tabs, runs of spaces,
//! a trailing run at the end of a line), the segments' own bytes (ASCII, multi-byte), and
//! which extraction read them (a UTF-8 text asset, and a PDF document parsed page by page,
//! over one page and two).
//!
//! The text is read back over Amazon S3's own wire protocol, because the object store owns
//! the object the job wrote — S3 is mounted beside this service on one engine, which is what
//! `core/router` assembles.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_bedrockdataautomationruntime_amazonbedrockkeystoneruntimeservice as bda;
use service_com_amazonaws_s3_amazons3 as s3;
use smithy_server_runtime::MultiServiceRouter;

use crate::support::{self, pdf_document, PROFILE_ARN, PROJECT_ARN, REGION};

/// The bucket the input assets are staged in.
const ASSET_BUCKET: &str = "bda-extraction-assets";

/// The bucket every job below writes its output into.
const OUTPUT_BUCKET: &str = "bda-extraction-output";

/// The prefix inside [`OUTPUT_BUCKET`] every submission below names.
const OUTPUT_PREFIX: &str = "extractions";

/// What follows every extracted word — the separator the concatenation keeps after the LAST
/// word as well, which is the whole subject of this suite.
const WORD_SEPARATOR: &str = " ";

/// What separates one reconstructed line of extracted text from the next.
const LINE_SEPARATOR: &str = "\n";

/// How long a poll may wait for a job to reach a terminal status.
///
/// GENEROUS on purpose: the loop below waits for the CONDITION, not for a duration, so this
/// is only the deadline that stops a broken build from hanging.
const TERMINAL_DEADLINE: Duration = Duration::from_secs(60);

/// How long one poll sleeps before asking again.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// One emulator: this service and Amazon S3 on a single shared engine behind the production
/// router, with the referenced project arranged and both buckets created.
async fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    support::seed_project(&engine, PROJECT_ARN, "data-automation-receipts");
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(bda::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(s3::EmulatedService::new(Arc::clone(&engine)));
    for bucket in [ASSET_BUCKET, OUTPUT_BUCKET] {
        // A region-specific endpoint needs the location constraint stated.
        let configuration = format!(
            "<CreateBucketConfiguration><LocationConstraint>{REGION}</LocationConstraint>\
             </CreateBucketConfiguration>"
        );
        let (status, body) = s3_call(
            &router,
            "PUT",
            &format!("/{bucket}"),
            configuration.as_bytes(),
        )
        .await;
        assert_eq!(
            status, 200,
            "the bucket {bucket:?} must be created, got {status} / {}",
            String::from_utf8_lossy(&body)
        );
    }
    router
}

/// One path-style request to the emulator's own Amazon S3 front door: the bucket and the key
/// live in the URI, the body is the object's own bytes. The credential scope names `s3`,
/// which is what claims it. Answered as the status and the response body.
async fn s3_call(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: &[u8],
) -> (u16, Vec<u8>) {
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://s3.{REGION}.amazonaws.com{path}"))
        .header("host", format!("s3.{REGION}.amazonaws.com"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/s3/aws4_request, \
                 SignedHeaders=host, Signature=0000"
            ),
        )
        .body(Bytes::copy_from_slice(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    (
        response.status().as_u16(),
        response.body().as_ref().to_vec(),
    )
}

/// The `$.<modality>.representation.text` of the standard output of one job over `bytes`:
/// stage the asset, submit the job, poll it to its terminal status and read the document it
/// wrote back out of the object store.
async fn extracted_text(router: &MultiServiceRouter, case: &str, key: &str, bytes: &[u8]) -> String {
    let (status, body) = s3_call(router, "PUT", &format!("/{ASSET_BUCKET}/{key}"), bytes).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the asset {key:?} must be staged, got {status} / {}",
        String::from_utf8_lossy(&body)
    );

    let answer = support::call(
        router,
        "InvokeDataAutomationAsync",
        json!({
            "inputConfiguration": {"s3Uri": format!("s3://{ASSET_BUCKET}/{key}")},
            "outputConfiguration": {"s3Uri": format!("s3://{OUTPUT_BUCKET}/{OUTPUT_PREFIX}")},
            "dataAutomationProfileArn": PROFILE_ARN,
            "dataAutomationConfiguration": {"dataAutomationProjectArn": PROJECT_ARN},
        }),
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: expected 200 from InvokeDataAutomationAsync, got {} / {}",
        answer.status, answer.body
    );
    let invocation_arn = answer.body["invocationArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no invocationArn in {}", answer.body))
        .to_string();

    let terminal = poll_until_terminal(router, case, &invocation_arn).await;
    assert_eq!(
        terminal["status"].as_str(),
        Some("Success"),
        "case {case:?}: the job must succeed before its output is read, got {terminal}"
    );

    let result_key = format!(
        "{OUTPUT_PREFIX}/{}/0/standard_output/0/result.json",
        support::invocation_id(&invocation_arn)
    );
    let (status, body) = s3_call(router, "GET", &format!("/{OUTPUT_BUCKET}/{result_key}"), &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the standard output at {result_key:?} must be readable, got {status} / {}",
        String::from_utf8_lossy(&body)
    );
    let document: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    document["document"]["representation"]["text"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case:?}: the standard output carries no extracted text: {document}")
        })
        .to_string()
}

/// Poll `GetDataAutomationStatus` until the job reaches a terminal status, and answer the
/// first body that reported it.
async fn poll_until_terminal(router: &MultiServiceRouter, case: &str, invocation_arn: &str) -> Value {
    let deadline = Instant::now() + TERMINAL_DEADLINE;
    loop {
        let answer = support::call(
            router,
            "GetDataAutomationStatus",
            json!({"invocationArn": invocation_arn}),
        )
        .await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected 200 from GetDataAutomationStatus, got {} / {}",
            answer.status, answer.body
        );
        let status = answer.body["status"].as_str().unwrap_or_default();
        if matches!(status, "Success" | "ClientError" | "ServiceError") {
            return answer.body;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the job never reached a terminal status, last read {}",
            answer.body
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}


/// the separator of its last segment, every line ends with one, no segment carries more than
/// one separator, and the segments themselves are the words the asset holds, in order.
fn assert_segment_separators(case: &str, text: &str, words: &[&str]) {
    assert!(
        text.ends_with(WORD_SEPARATOR),
        "case {case:?}: expected the concatenation to keep the separator after its last \
         segment, got {text:?}"
    );
    for (index, line) in text.split(LINE_SEPARATOR).enumerate() {
        assert!(
            line.ends_with(WORD_SEPARATOR),
            "case {case:?}: expected line {index} to keep the separator after its last \
             segment, got {line:?} of {text:?}"
        );
    }
    assert!(
        !text.contains("  "),
        "case {case:?}: expected exactly one separator per segment, got {text:?}"
    );
    let read: Vec<&str> = text.split_whitespace().collect();
    assert_eq!(
        read, words,
        "case {case:?}: expected the segments {words:?}, got {read:?} of {text:?}"
    );
}

/// One row: a PDF document, and the segments its pages hold in reading order.
struct PdfCase {
    /// What the row varies.
    case: &'static str,
    /// The key the asset is staged under.
    key: &'static str,
    /// The document's pages, each as its own lines.
    pages: &'static [&'static [&'static str]],
    /// The segments the whole document holds, in reading order.
    words: &'static [&'static str],
}

/// One row: a UTF-8 text asset, and the extracted text the rule requires of it in full.
struct TextCase {
    /// What the row varies.
    case: &'static str,
    /// The key the asset is staged under.
    key: &'static str,
    /// The asset's own bytes — a text asset IS its own text.
    asset: &'static str,
    /// `$.document.representation.text`, byte for byte.
    text: &'static str,
}

/// The rows: one segment and several, one line and several, every spelling of the source's
/// own spacing (single spaces, tabs, runs, a run at the end of a line, blank lines), and
/// multi-byte segments. Each `text` is the asset's words with ONE separator after each, the
/// last of a line included, and the lines joined by the line separator.
const TEXT_CASES: &[TextCase] = &[
    TextCase {
        case: "one line of several segments",
        key: "invoices/invoice.txt",
        asset: "Invoice 4711 total due 250 dollars\n",
        text: "Invoice 4711 total due 250 dollars ",
    },
    TextCase {
        case: "a single segment",
        key: "invoices/word.txt",
        asset: "Receipt",
        text: "Receipt ",
    },
    TextCase {
        case: "several lines",
        key: "invoices/note.txt",
        asset: "Delivery note 77\nSigned by Ada Lovelace\n",
        text: "Delivery note 77 \nSigned by Ada Lovelace ",
    },
    TextCase {
        case: "segments the source separated by tabs and runs of spaces",
        key: "invoices/spaced.txt",
        asset: "Total\t\t42   USD\n",
        text: "Total 42 USD ",
    },
    TextCase {
        case: "a line whose last segment the source already trailed with spaces",
        key: "invoices/trailing.txt",
        asset: "Invoice 1   \nPaid 2  \n",
        text: "Invoice 1 \nPaid 2 ",
    },
    TextCase {
        case: "blank lines around and between the lines of segments",
        key: "invoices/blank_lines.txt",
        asset: "\n\nInvoice 9\n\n\nPaid in full\n\n",
        text: "Invoice 9 \nPaid in full ",
    },
    TextCase {
        case: "multi-byte segments",
        key: "invoices/unicode.txt",
        asset: "Façade naïve 10 €\n",
        text: "Façade naïve 10 € ",
    },
];

/// RULE: the extracted text of a UTF-8 text asset is its segments, each followed by the
/// separator that trails it — the last one included.
#[tokio::test]
async fn every_segment_of_an_extracted_text_asset_keeps_its_trailing_separator() {
    let router = emulator().await;
    for row in TEXT_CASES {
        let case = row.case;
        let text = extracted_text(&router, case, row.key, row.asset.as_bytes()).await;
        assert_eq!(
            text, row.text,
            "case {case:?}: expected {:?}, got {text:?}",
            row.text
        );
        let words: Vec<&str> = row.asset.split_whitespace().collect();
        assert_segment_separators(case, &text, &words);
    }
}

/// RULE, over the other extraction this service makes: a PDF document's pages are parsed and
/// rendered by the same assembly, so each of their segments carries its separator too —
/// including the last segment of the last page, where the recorded divergence showed.
#[tokio::test]
async fn every_segment_of_an_extracted_pdf_document_keeps_its_trailing_separator() {
    let router = emulator().await;
    let rows: &[PdfCase] = &[
        PdfCase {
            case: "one page of one line",
            key: "pdf/one_line.pdf",
            pages: &[&["Invoice 4711 total due 250 dollars"]],
            words: &["Invoice", "4711", "total", "due", "250", "dollars"],
        },
        PdfCase {
            case: "one page of several lines",
            key: "pdf/lines.pdf",
            pages: &[&["Opening balance 40 USD", "Closing balance 95 USD"]],
            words: &[
                "Opening", "balance", "40", "USD", "Closing", "balance", "95", "USD",
            ],
        },
        PdfCase {
            case: "two pages",
            key: "pdf/pages.pdf",
            pages: &[&["Opening balance 40 USD"], &["Closing balance 95 USD"]],
            words: &[
                "Opening", "balance", "40", "USD", "Closing", "balance", "95", "USD",
            ],
        },
    ];
    for row in rows {
        let text = extracted_text(&router, row.case, row.key, &pdf_document(row.pages)).await;
        assert_segment_separators(row.case, &text, row.words);
    }
}
