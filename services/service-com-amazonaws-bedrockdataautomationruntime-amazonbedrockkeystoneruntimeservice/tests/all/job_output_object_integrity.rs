// SPDX-License-Identifier: CC0-1.0

//! The INTEGRITY headers a client reads a finished data-automation job's output with.
//!
//! THE RULE: an object this service writes into the caller's bucket on the job's behalf is a
//! stored object like any other, so the read the asynchronous API leaves a client — a
//! `GetObject` on the job's own output key once the job reports a terminal success — answers
//! 200 and reports the integrity metadata of the bytes it is serving: the whole-object CRC32
//! digest of those very bytes on `x-amz-checksum-crc32` with `x-amz-checksum-type`
//! `FULL_OBJECT`, an `ETag`, a `Last-Modified`, `accept-ranges` `bytes`, the store's own
//! default media type for a write that named none, and the bucket's default server-side
//! encryption. The digest is a fact about the object's content, so it is recomputed here from
//! the served body rather than compared with a value from any one run, and every read path
//! (`GetObject`, `HeadObject`, a second `GetObject`) reports the same one.
//!
//! The rows differ along what the rule generalises over — the asset's FORMAT and so the
//! modality and file type of the output document, how many PAGES it covers, and how many
//! BYTES the output document ends up being (a few hundred for an image's metadata-only
//! document, several kilobytes for a long text extraction) — because the checksum is a
//! function of the content and a rule about it has to hold whatever the content is.
//!
//! The suite about the layout and the body of those documents is
//! `job_standard_output_objects`; this one is about what reading them reports.

use std::io::Cursor;
use std::sync::Arc;

use serde_json::json;
use service_com_amazonaws_bedrockdataautomationruntime_amazonbedrockkeystoneruntimeservice as bda;
use service_com_amazonaws_s3_amazons3 as s3;
use smithy_server_runtime::MultiServiceRouter;

use crate::support::{
    create_bucket, head_object, poll_until_terminal, put_object, read_object, DEFAULT_MEDIA_TYPE,
    FULL_OBJECT, SSE_S3,
};

/// The data-automation project every submission names — the member that says WHAT to extract.
/// Arranged in the owning (build-time) service's store, because a submission naming a project
/// nobody created is refused before any rule here could be reached.
const PROJECT_ARN: &str =
    "arn:aws:bedrock:us-west-2:000000000000:data-automation-project/integrity";

/// The bucket the input assets are staged in.
const ASSET_BUCKET: &str = "bda-integrity-assets";

/// The bucket every job below writes its output into.
const OUTPUT_BUCKET: &str = "bda-integrity-output";

/// The prefix every submission below names inside [`OUTPUT_BUCKET`].
const OUTPUT_PREFIX: &str = "output";

/// The `ChecksumAlgorithm` value the integrity header of a job's output object carries, and
/// the algorithm its value is a digest under.
const CRC32: &str = "CRC32";

/// One emulator: this service and Amazon S3 on a single shared engine behind the production
/// router, with the referenced project arranged and both buckets created.
async fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    crate::support::seed_project(&engine, PROJECT_ARN, "data-automation-integrity");
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(bda::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(s3::EmulatedService::new(Arc::clone(&engine)));
    for bucket in [ASSET_BUCKET, OUTPUT_BUCKET] {
        create_bucket(&router, bucket).await;
    }
    router
}

/// Stage one input asset at `key` in [`ASSET_BUCKET`], the way a caller uploads it before
/// submitting.
async fn stage_asset(router: &MultiServiceRouter, case: &str, key: &str, bytes: &[u8]) {
    put_object(router, case, ASSET_BUCKET, key, bytes).await;
}

/// Submit one invocation reading `input_s3_uri` and writing under [`OUTPUT_PREFIX`], and
/// return its `invocationArn`.
async fn submit(router: &MultiServiceRouter, case: &str, input_s3_uri: &str) -> String {
    let answer = crate::support::call(
        router,
        "InvokeDataAutomationAsync",
        json!({
            "inputConfiguration": {"s3Uri": input_s3_uri},
            "outputConfiguration": {"s3Uri": format!("s3://{OUTPUT_BUCKET}/{OUTPUT_PREFIX}")},
            "dataAutomationProfileArn": crate::support::PROFILE_ARN,
            "dataAutomationConfiguration": {"dataAutomationProjectArn": PROJECT_ARN},
        }),
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: expected 200 from InvokeDataAutomationAsync, got {} / {}",
        answer.status, answer.body
    );
    answer.body["invocationArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no invocationArn in {}", answer.body))
        .to_string()
}

/// The Base64 digest of `data` under the `ChecksumAlgorithm` value `algorithm`, as every
/// `Checksum*` member carries it ("the Base64 encoded, … checksum").
///
/// Computed through `aws-smithy-checksums`, the AWS SDK's own implementation of these
/// algorithms, over the bytes the emulator SERVED — so the expectation is a digest of the
/// object's own content rather than a value recorded from one run.
fn digest_of(algorithm: &str, data: &[u8]) -> String {
    let parsed: aws_smithy_checksums::ChecksumAlgorithm = algorithm
        .to_ascii_lowercase()
        .parse()
        .unwrap_or_else(|error| {
            panic!("{algorithm} must be a checksum this crate computes: {error}")
        });
    let mut checksum = parsed.into_impl();
    checksum.update(data);
    aws_smithy_types::base64::encode(checksum.finalize())
}

/// The header the digest under the `ChecksumAlgorithm` value `algorithm` travels on: every
/// `Checksum*` member of the S3 model binds `x-amz-checksum-` followed by its algorithm in
/// lowercase.
fn checksum_header(algorithm: &str) -> String {
    format!("x-amz-checksum-{}", algorithm.to_ascii_lowercase())
}

/// A PNG image `side` pixels square, encoded by the `image` crate — an asset of the IMAGE
/// modality, inside the dimensions data automation processes.
fn png_image(side: u32) -> Vec<u8> {
    let image = ::image::RgbImage::from_fn(side, side, |x, y| {
        ::image::Rgb([(x % 256) as u8, (y % 256) as u8, 64])
    });
    let mut bytes: Vec<u8> = Vec::new();
    image
        .write_to(&mut Cursor::new(&mut bytes), ::image::ImageFormat::Png)
        .expect("the test image encodes as PNG");
    bytes
}

/// A plain-text asset of `lines` lines, each one naming its own index so the extracted text
/// grows with the count rather than repeating one block.
fn text_asset(lines: usize) -> Vec<u8> {
    (0..lines)
        .map(|index| format!("Line {index} of the delivery manifest, item {}\n", index * 7))
        .collect::<String>()
        .into_bytes()
}

/// One row: the asset a job reads, and roughly how large the output document it produces is.
struct Case {
    /// The key the asset is staged under.
    key: &'static str,
    /// The asset's bytes.
    bytes: Vec<u8>,
    /// The fewest bytes the standard-output document may be, so a row that was meant to
    /// exercise a LARGE body cannot silently become a small one.
    at_least_bytes: usize,
}

/// Assert the integrity headers one read of `key` reports are those of the bytes it served.
///
/// Returns the digest the object reported, so the caller can check two different documents
/// never report the same one.
async fn assert_object_reports_its_own_digest(
    router: &MultiServiceRouter,
    case: &str,
    key: &str,
) -> String {
    let answer = read_object(router, OUTPUT_BUCKET, key).await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: the output object at {key:?} must be readable once the job reports a \
         terminal success, got {} / {}",
        answer.status,
        answer.text()
    );

    let header = checksum_header(CRC32);
    let expected = digest_of(CRC32, &answer.body);
    assert_eq!(
        answer.header(&header).as_deref(),
        Some(expected.as_str()),
        "case {case:?}: expected {header} on {key:?} to be the {CRC32} digest {expected:?} of \
         the {} served bytes, got {:?}",
        answer.body.len(),
        answer.header(&header)
    );
    for (name, want) in [
        ("x-amz-checksum-type", FULL_OBJECT),
        ("content-type", DEFAULT_MEDIA_TYPE),
        ("accept-ranges", "bytes"),
        ("x-amz-server-side-encryption", SSE_S3),
        ("content-length", answer.body.len().to_string().as_str()),
    ] {
        assert_eq!(
            answer.header(name).as_deref(),
            Some(want),
            "case {case:?}: expected {name} {want:?} on {key:?}, got {:?}",
            answer.header(name)
        );
    }
    for name in ["etag", "last-modified"] {
        assert!(
            answer.header(name).is_some_and(|value| !value.is_empty()),
            "case {case:?}: expected {name} present on {key:?}, got {:?}",
            answer.header(name)
        );
    }

    // Every read path reports the same stored object: the metadata-only read and a second
    // read of the body agree with the first, so nothing about the object is minted per read.
    let head = head_object(router, OUTPUT_BUCKET, key).await;
    assert_eq!(
        head.status, 200,
        "case {case:?}: a metadata read of {key:?} must answer 200, got {} / {}",
        head.status,
        head.text()
    );
    for name in [
        header.as_str(),
        "x-amz-checksum-type",
        "etag",
        "last-modified",
        "content-length",
    ] {
        assert_eq!(
            head.header(name),
            answer.header(name),
            "case {case:?}: a metadata read of {key:?} must report the same {name} as the body \
             read ({:?}), got {:?}",
            answer.header(name),
            head.header(name)
        );
    }
    let again = read_object(router, OUTPUT_BUCKET, key).await;
    assert_eq!(
        again.body, answer.body,
        "case {case:?}: a second read of {key:?} must serve the same bytes"
    );
    assert_eq!(
        again.header(&header),
        answer.header(&header),
        "case {case:?}: a second read of {key:?} must report the same digest"
    );
    expected
}

#[tokio::test]
async fn a_finished_jobs_output_objects_report_the_crc32_of_the_bytes_they_serve() {
    let router = emulator().await;

    let rows: &[(&str, Case)] = &[
        (
            "a one-page PDF",
            Case {
                key: "input/invoice.pdf",
                bytes: crate::support::pdf_document(&[&[
                    "Invoice 4821 for Pelagic Shipping",
                    "Total due 1200 USD",
                ]]),
                at_least_bytes: 1,
            },
        ),
        (
            "a three-page PDF, a longer extraction",
            Case {
                key: "input/statement.pdf",
                bytes: crate::support::pdf_document(&[
                    &["Statement page one of three", "Opening balance 40 USD"],
                    &["Statement page two of three", "Interest 2 USD"],
                    &["Statement page three of three", "Closing balance 95 USD"],
                ]),
                at_least_bytes: 1,
            },
        ),
        (
            "a short UTF-8 text asset",
            Case {
                key: "notes.txt",
                bytes: b"Delivery note 77\nSigned by Ada Lovelace\n".to_vec(),
                at_least_bytes: 1,
            },
        ),
        (
            "a many-line UTF-8 text asset, a multi-kilobyte output document",
            Case {
                key: "input/manifest.txt",
                bytes: text_asset(200),
                at_least_bytes: 4096,
            },
        ),
        (
            "a PNG image, a metadata-only output document",
            Case {
                key: "input/scan.png",
                bytes: png_image(64),
                at_least_bytes: 1,
            },
        ),
    ];

    let mut digests: Vec<(&str, String)> = Vec::new();
    for (case, row) in rows {
        stage_asset(&router, case, row.key, &row.bytes).await;
        let arn = submit(&router, case, &format!("s3://{ASSET_BUCKET}/{}", row.key)).await;
        let id = crate::support::invocation_id(&arn).to_string();

        let status = poll_until_terminal(&router, case, &arn).await;
        assert_eq!(
            status["status"].as_str(),
            Some("Success"),
            "case {case:?}: a job whose asset the store holds must succeed, got {status}"
        );

        // Both objects the job wrote: the per-asset standard output a client reads the
        // extraction out of, and the index at the top of the job's own segment.
        let result_key = format!("{OUTPUT_PREFIX}/{id}/0/standard_output/0/result.json");
        let index_key = format!("{OUTPUT_PREFIX}/{id}/job_metadata.json");

        let served = read_object(&router, OUTPUT_BUCKET, &result_key).await;
        assert!(
            served.body.len() >= row.at_least_bytes,
            "case {case:?}: expected a standard output of at least {} bytes, got {}",
            row.at_least_bytes,
            served.body.len()
        );

        for key in [&result_key, &index_key] {
            let digest = assert_object_reports_its_own_digest(&router, case, key).await;
            digests.push((case, digest));
        }
    }

    // The digest is a function of the content, so no two of these documents — different
    // assets, different jobs, different sizes — report the same one: a constant would.
    for (left, (left_case, left_digest)) in digests.iter().enumerate() {
        for (right_case, right_digest) in digests.iter().skip(left + 1) {
            assert_ne!(
                left_digest, right_digest,
                "two different output documents (case {left_case:?} and case {right_case:?}) \
                 must not report the same digest {left_digest:?}"
            );
        }
    }
}

#[tokio::test]
async fn an_output_key_no_job_wrote_is_still_absent() {
    // The other half of the rule: these headers belong to an object that is really there.
    // A key under the same prefix that no job wrote earns the object store's own
    // absent-object answer, and none of the integrity headers.
    let router = emulator().await;
    let case = "a key no job wrote";
    let key = format!("{OUTPUT_PREFIX}/00000000-0000-4000-8000-000000000000/0/standard_output/0/result.json");

    let answer = read_object(&router, OUTPUT_BUCKET, &key).await;
    assert_eq!(
        answer.status, 404,
        "case {case:?}: expected 404 for {key:?}, got {} / {}",
        answer.status,
        answer.text()
    );
    for name in [
        checksum_header(CRC32).as_str(),
        "x-amz-checksum-type",
        "etag",
        "accept-ranges",
        "x-amz-server-side-encryption",
    ] {
        assert_eq!(
            answer.header(name),
            None,
            "case {case:?}: an absent object must report no {name}, got {:?}",
            answer.header(name)
        );
    }
}
