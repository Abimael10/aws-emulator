// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the QUERY PARAMETERS an object READ carries: the ones that address
//! part of the object, the ones that override a response header, and the one that addresses a
//! stored version.
//!
//! Every assertion drives a real HTTP request through the router (the harness
//! [`super::bucket_lifecycle`], [`super::object_and_tagging_roundtrip`] and
//! [`super::object_versioning`] already own), so what is pinned is the wire behaviour —
//! status, error code and the error document's own members — rather than any handler's
//! internals.
//!
//! The rules under test, each asserted over a SET of requests rather than one:
//!
//! 1. A part number is "a positive integer between 1 and 10,000"
//!    (`UploadPartRequest$PartNumber`, the published range for the member every part-scoped
//!    call binds). A `partNumber` outside that — text that is no integer among it — addresses
//!    no part of ANY object, so it is a refused ARGUMENT: `InvalidArgument` (400) with the
//!    fixed sentence stating the range, naming `partNumber` in `<ArgumentName>` and echoing
//!    the value as sent in `<ArgumentValue>`. It is NOT the 416 a span that cannot be served
//!    earns: that is a well-formed number inside the range the object has no part under. The
//!    same parameter on the WRITE that addresses a part by number (`UploadPart`) is refused
//!    with the same code and sentence, since the range is the parameter's and not one
//!    reader's.
//! 2. That 416 names the numbers it was decided from: `<PartNumberRequested>`, the number the
//!    read asked for, and `<ActualPartCount>`, how many parts the object actually is made of
//!    — one for an object a single write stored whole, since a part-scoped read of it is
//!    served its single part. The sentence states neither, so the pair is the whole of what a
//!    client clamps its next read to.
//! 3. A `Range` "downloads the specified byte range of an object" (`GetObjectRequest$Range`)
//!    and a `partNumber` "effectively performs a 'ranged' GET request for the part specified"
//!    (`$PartNumber`): each selects a span of its own, so a read stating BOTH names no one
//!    answer and is refused `InvalidRequest` (400) with the sentence naming the two
//!    parameters, rather than served whichever span the service happened to honour.
//! 4. The response headers a read may override are the ones the model binds a member for,
//!    each under its own `response-`-prefixed query parameter
//!    (`smithy_query(query="operation",operation="GetObject")` reports `response-cache-control`,
//!    `response-content-disposition`, `response-content-encoding`, `response-content-language`,
//!    `response-content-type` and `response-expires`). One of those is honoured; a
//!    `response-`-prefixed parameter outside the set is refused `InvalidArgument` (400) with
//!    the sentence naming it, rather than served with the override silently dropped.
//! 5. A `versionId` stated with NO text names no version because it names nothing, which S3
//!    words in a sentence of its own — distinct from the one a token that is not a version id
//!    earns — wherever the parameter arrives.
//!
//! "A HEAD request has the same options as a GET operation on an object"
//! (`HeadObject`'s own documentation), so every rule is pinned on the metadata read as well;
//! a HEAD carries no document, so its status is the whole of its answer.

use service_com_amazonaws_s3_amazons3::state::{MAX_PART_NUMBER, MIN_PART_NUMBER};
use smithy_server_runtime::MultiServiceRouter;

use super::bucket_lifecycle::{router, send_bytes, Answer};
use super::object_and_tagging_roundtrip::{bucket, get_object, put_object, text};
use super::object_partial_reads::{completed_multipart_object, seeded_payload};
use super::object_versioning::{delete_object, head_object};

/// The code S3 refuses a value a request field cannot carry with.
const INVALID_ARGUMENT: &str = "InvalidArgument";

/// The code S3 refuses a request it cannot serve as given with.
const INVALID_REQUEST: &str = "InvalidRequest";

/// The code S3 refuses a span of the object it cannot serve with.
const INVALID_PART_NUMBER: &str = "InvalidPartNumber";

/// The fixed sentence a `partNumber` outside the published range earns, spelled out here
/// rather than read from the crate under test so a wording regression fails this suite.
const PART_NUMBER_RANGE_MESSAGE: &str =
    "Part number must be an integer between 1 and 10000, inclusive";

/// The fixed sentence the 416 carries — the text that code's entry in Amazon S3's list of
/// error codes publishes.
const PART_NUMBER_UNSATISFIABLE_MESSAGE: &str = "The requested partnumber is not satisfiable";

/// The fixed sentence a read selecting a span both ways earns.
const RANGE_AND_PART_NUMBER_MESSAGE: &str =
    "Cannot specify both Range header and partNumber query parameter";

/// The fixed sentence a `versionId` stated with no text earns.
const EMPTY_VERSION_ID_MESSAGE: &str = "Version id cannot be the empty string";

/// The fixed sentence a token that is not a version id earns — the OTHER sentence of the same
/// parameter, which is what rule 5 is about.
const INVALID_VERSION_ID_MESSAGE: &str = "Invalid version id specified";

/// The fewest bytes a part other than the last may carry: "Part size: 5 MiB to 5 GiB. There
/// is no minimum size limit on the last part of your multipart upload" (Amazon S3 User Guide,
/// *Amazon S3 multipart upload limits*).
const MIN_MULTIPART_PART_BYTES: usize = 5 * 1024 * 1024;

/// The QUERY PARAMETERS a read may override a response header with, as the model binds them —
/// `smithy_query(query="operation",operation="GetObject")` reports one `@httpQuery` member per
/// published override — each with a value a client would send and the response header it sets.
const PUBLISHED_OVERRIDES: [(&str, &str, &str); 5] = [
    ("response-content-type", "text/plain", "content-type"),
    (
        "response-cache-control",
        "max-age=60",
        "cache-control",
    ),
    (
        "response-content-disposition",
        "attachment",
        "content-disposition",
    ),
    ("response-content-encoding", "identity", "content-encoding"),
    ("response-content-language", "en-GB", "content-language"),
];

/// `GET /<bucket>/<key>?attributes<query>` — `GetObjectAttributes`, which takes a `versionId`
/// too, carrying the field the read requires so what a case varies is the parameter.
async fn get_object_attributes(
    router: &MultiServiceRouter,
    name: &str,
    key: &str,
    query: &str,
) -> Answer {
    send_bytes(
        router,
        "GET",
        &format!("/{name}/{key}?attributes{query}"),
        &[("x-amz-object-attributes", "ETag")],
        b"",
    )
    .await
}

/// Assert `answer` is a refusal carrying `code`, `message` and the `<ArgumentName>` /
/// `<ArgumentValue>` pair naming `argument` and the value as sent.
///
/// The value is read as the empty string when the document carries no text for it, which is
/// what a client reading a bound member sees for a parameter stated empty.
fn assert_refused_argument(
    answer: &Answer,
    case: &str,
    status: u16,
    code: &str,
    message: &str,
    argument: &str,
    value: &str,
) {
    assert_eq!(
        answer.status, status,
        "case {case:?}: expected HTTP {status}, got {} with body {:?}",
        answer.status,
        text(answer)
    );
    assert_eq!(
        answer.error_code(),
        code,
        "case {case:?}: expected the {code:?} code, got {:?}",
        answer.error_code()
    );
    assert_eq!(
        answer.error_message(),
        message,
        "case {case:?}: expected the published sentence {message:?}, got {:?}",
        answer.error_message()
    );
    assert_eq!(
        answer.element("ArgumentName").as_deref(),
        Some(argument),
        "case {case:?}: the envelope must name the field it rejected, got {:?} in {:?}",
        answer.element("ArgumentName"),
        text(answer)
    );
    assert_eq!(
        answer.element("ArgumentValue").unwrap_or_default(),
        value,
        "case {case:?}: the envelope must echo the value that field carried, got {:?} in {:?}",
        answer.element("ArgumentValue"),
        text(answer)
    );
}

#[tokio::test]
async fn a_part_number_outside_the_published_range_is_refused_as_a_bad_argument() {
    let router = router();
    let name = "read-parameters-part-number";
    bucket(&router, name).await;
    let key = "written.bin";
    let body = seeded_payload(0x0f17_2b55, 1_000);
    let written = put_object(&router, name, key, &body, &[]).await;
    assert_eq!(
        written.status, 200,
        "the object must be stored first, got {} with body {:?}",
        written.status,
        text(&written)
    );

    // The rows differ along HOW the value fails to name a part: each bound of the published
    // range crossed by one, the far ends of the member's own integer type, a word, a decimal,
    // a hexadecimal spelling, a number past what the type can hold, and the parameter stated
    // with no text at all.
    let past_the_type = format!("{}0", i64::from(i32::MAX));
    let rows: Vec<(&str, String)> = vec![
        ("one below the lowest part number", (MIN_PART_NUMBER - 1).to_string()),
        ("one above the highest part number", (MAX_PART_NUMBER + 1).to_string()),
        ("a negative number", String::from("-7")),
        ("the lowest value the member's type holds", i32::MIN.to_string()),
        ("the highest value the member's type holds", i32::MAX.to_string()),
        ("a word", String::from("one")),
        ("a decimal", String::from("1.5")),
        ("a hexadecimal spelling", String::from("0x1")),
        ("a number past the member's type", past_the_type),
        ("no text at all", String::new()),
    ];

    for (case, value) in &rows {
        let query = format!("?partNumber={value}");
        let refused = get_object(&router, name, key, &query, &[]).await;
        assert_refused_argument(
            &refused,
            &format!("{case}/GetObject"),
            400,
            INVALID_ARGUMENT,
            PART_NUMBER_RANGE_MESSAGE,
            "partNumber",
            value,
        );
        let headed = head_object(&router, name, key, &query).await;
        assert_eq!(
            headed.status, 400,
            "case {case:?}/HeadObject: expected HTTP 400, got {}",
            headed.status
        );
    }

    // Each published bound itself is NOT a bad argument: the lowest names the one part a
    // whole-object write stored (served as that span), and the highest is a number the object
    // simply has no part under — the 416 of rule 2, not the 400 of rule 1.
    let lowest = get_object(
        &router,
        name,
        key,
        &format!("?partNumber={MIN_PART_NUMBER}"),
        &[],
    )
    .await;
    assert_eq!(
        lowest.status, 206,
        "case \"the lowest part number\": expected the partial-content status, got {} with \
         body {:?}",
        lowest.status,
        text(&lowest)
    );
    let highest = get_object(
        &router,
        name,
        key,
        &format!("?partNumber={MAX_PART_NUMBER}"),
        &[],
    )
    .await;
    assert_eq!(
        highest.status, 416,
        "case \"the highest part number\": expected HTTP 416, got {} with body {:?}",
        highest.status,
        text(&highest)
    );
    assert_eq!(
        highest.error_code(),
        INVALID_PART_NUMBER,
        "case \"the highest part number\": expected the {INVALID_PART_NUMBER:?} code, got {:?}",
        highest.error_code()
    );

    // Every refusal above left the object as it was.
    let read = get_object(&router, name, key, "", &[]).await;
    assert_eq!(
        read.body.as_ref(),
        body.as_slice(),
        "a refused parameter must leave the object as it was, got {} bytes",
        read.body.len()
    );

    // The same parameter on the WRITE that addresses a part by number: "part number of part
    // being uploaded. This is a positive integer between 1 and 10,000"
    // (`UploadPartRequest$PartNumber`) is the same published range, so a value that addresses
    // no part is refused with the same code and the same sentence there — the rule is the
    // parameter's, not one reader's.
    let initiated = send_bytes(&router, "POST", &format!("/{name}/parted.bin?uploads"), &[], b"")
        .await;
    assert_eq!(
        initiated.status, 200,
        "the upload must be initiated first, got {} with body {:?}",
        initiated.status,
        text(&initiated)
    );
    let upload_id = initiated
        .element("UploadId")
        .expect("an initiation must report an UploadId");
    for (case, value) in [("a word", "one"), ("a number below the lowest", "0")] {
        let refused = send_bytes(
            &router,
            "PUT",
            &format!("/{name}/parted.bin?partNumber={value}&uploadId={upload_id}"),
            &[],
            b"part-bytes",
        )
        .await;
        assert_eq!(
            refused.status, 400,
            "case {case:?}/UploadPart: expected HTTP 400, got {} with body {:?}",
            refused.status,
            text(&refused)
        );
        assert_eq!(
            refused.error_code(),
            INVALID_ARGUMENT,
            "case {case:?}/UploadPart: expected the {INVALID_ARGUMENT:?} code, got {:?}",
            refused.error_code()
        );
        assert_eq!(
            refused.error_message(),
            PART_NUMBER_RANGE_MESSAGE,
            "case {case:?}/UploadPart: expected the published sentence, got {:?}",
            refused.error_message()
        );
    }
}

#[tokio::test]
async fn a_part_number_the_object_holds_no_part_under_names_the_count_it_was_judged_against() {
    let router = router();
    let name = "read-parameters-part-count";
    bucket(&router, name).await;

    // The rows differ along how many parts the object IS made of — a whole-object write, a
    // two-part upload and a three-part one — and along how far past that count the read asked.
    let whole = seeded_payload(0x1234_5678, 2_048);
    let written = put_object(&router, name, "whole.bin", &whole, &[]).await;
    assert_eq!(
        written.status, 200,
        "the whole-object write must succeed, got {} with body {:?}",
        written.status,
        text(&written)
    );
    let two = vec![
        seeded_payload(1, MIN_MULTIPART_PART_BYTES),
        seeded_payload(2, 512),
    ];
    completed_multipart_object(&router, name, "two-parts.bin", &two, &[]).await;
    let three = vec![
        seeded_payload(3, MIN_MULTIPART_PART_BYTES),
        seeded_payload(4, MIN_MULTIPART_PART_BYTES),
        seeded_payload(5, 64),
    ];
    completed_multipart_object(&router, name, "three-parts.bin", &three, &[]).await;

    let rows: [(&str, &str, i32, i32); 6] = [
        ("the part after the only one", "whole.bin", 2, 1),
        ("the highest part number on a whole-object write", "whole.bin", MAX_PART_NUMBER, 1),
        ("the part after the last of two", "two-parts.bin", 3, 2),
        ("far past the last of two", "two-parts.bin", 500, 2),
        ("the part after the last of three", "three-parts.bin", 4, 3),
        ("the highest part number on a three-part upload", "three-parts.bin", MAX_PART_NUMBER, 3),
    ];

    for (case, key, requested, parts) in rows {
        let query = format!("?partNumber={requested}");
        let refused = get_object(&router, name, key, &query, &[]).await;
        assert_eq!(
            refused.status, 416,
            "case {case:?}: expected HTTP 416, got {} with body {:?}",
            refused.status,
            text(&refused)
        );
        assert_eq!(
            refused.error_code(),
            INVALID_PART_NUMBER,
            "case {case:?}: expected the {INVALID_PART_NUMBER:?} code, got {:?}",
            refused.error_code()
        );
        assert_eq!(
            refused.error_message(),
            PART_NUMBER_UNSATISFIABLE_MESSAGE,
            "case {case:?}: expected the published sentence, got {:?}",
            refused.error_message()
        );
        assert_eq!(
            refused.element("PartNumberRequested").as_deref(),
            Some(requested.to_string().as_str()),
            "case {case:?}: the envelope must state the part number asked for, got {:?} in \
             {:?}",
            refused.element("PartNumberRequested"),
            text(&refused)
        );
        assert_eq!(
            refused.element("ActualPartCount").as_deref(),
            Some(parts.to_string().as_str()),
            "case {case:?}: the envelope must state how many parts the object has, got {:?} \
             in {:?}",
            refused.element("ActualPartCount"),
            text(&refused)
        );
        let headed = head_object(&router, name, key, &query).await;
        assert_eq!(
            headed.status, 416,
            "case {case:?}/HeadObject: expected HTTP 416, got {}",
            headed.status
        );
    }
}

#[tokio::test]
async fn a_read_selecting_a_span_both_ways_at_once_is_refused() {
    let router = router();
    let name = "read-parameters-two-spans";
    bucket(&router, name).await;
    let parts = vec![
        seeded_payload(6, MIN_MULTIPART_PART_BYTES),
        seeded_payload(7, 1_024),
    ];
    let object: Vec<u8> = parts.concat();
    completed_multipart_object(&router, name, "assembled.bin", &parts, &[]).await;

    // The rows differ along WHICH part the read names and along the FORM of the range beside
    // it — a closed span, an open-ended one, a suffix one, and one the object cannot satisfy
    // — because the refusal is about the two parameters being stated together, whatever
    // either of them says.
    let rows: [(&str, i32, &str); 5] = [
        ("the first part beside a closed range", 1, "bytes=0-9"),
        ("the last part beside a closed range", 2, "bytes=0-9"),
        ("a part beside an open-ended range", 1, "bytes=1024-"),
        ("a part beside a suffix range", 2, "bytes=-16"),
        (
            "a part beside a range the object cannot satisfy",
            1,
            "bytes=99999999-",
        ),
    ];

    for (case, part_number, range) in rows {
        let query = format!("?partNumber={part_number}");
        let refused = get_object(&router, name, "assembled.bin", &query, &[("range", range)]).await;
        assert_eq!(
            refused.status, 400,
            "case {case:?}: expected HTTP 400, got {} with body {:?}",
            refused.status,
            text(&refused)
        );
        assert_eq!(
            refused.error_code(),
            INVALID_REQUEST,
            "case {case:?}: expected the {INVALID_REQUEST:?} code, got {:?}",
            refused.error_code()
        );
        assert_eq!(
            refused.error_message(),
            RANGE_AND_PART_NUMBER_MESSAGE,
            "case {case:?}: expected the published sentence, got {:?}",
            refused.error_message()
        );
        let headed = send_bytes(
            &router,
            "HEAD",
            &format!("/{name}/assembled.bin{query}"),
            &[("range", range)],
            b"",
        )
        .await;
        assert_eq!(
            headed.status, 400,
            "case {case:?}/HeadObject: expected HTTP 400, got {}",
            headed.status
        );
    }

    // Either selector ALONE still serves its span, so what is refused is the combination.
    let part = get_object(&router, name, "assembled.bin", "?partNumber=2", &[]).await;
    assert_eq!(
        part.status, 206,
        "a part-scoped read alone must answer the partial-content status, got {} with body \
         {:?}",
        part.status,
        text(&part)
    );
    let ranged = get_object(
        &router,
        name,
        "assembled.bin",
        "",
        &[("range", "bytes=0-9")],
    )
    .await;
    assert_eq!(
        ranged.status, 206,
        "a ranged read alone must answer the partial-content status, got {} with body {:?}",
        ranged.status,
        text(&ranged)
    );
    assert_eq!(
        ranged.body.as_ref(),
        &object[..10],
        "a ranged read alone must answer its span, got {} bytes",
        ranged.body.len()
    );
}

#[tokio::test]
async fn a_response_header_override_outside_the_published_set_is_refused() {
    let router = router();
    let name = "read-parameters-overrides";
    bucket(&router, name).await;
    let key = "overridden.txt";
    let body = b"a,b\n1,2\n";
    let written = put_object(
        &router,
        name,
        key,
        body,
        &[("content-type", "text/csv")],
    )
    .await;
    assert_eq!(
        written.status, 200,
        "the object must be stored first, got {} with body {:?}",
        written.status,
        text(&written)
    );

    // Every PUBLISHED override is honoured: the response carries the header the parameter
    // sets, which is what makes the refusals below about the set rather than about overrides.
    for (parameter, value, header) in PUBLISHED_OVERRIDES {
        let served = get_object(&router, name, key, &format!("?{parameter}={value}"), &[]).await;
        assert_eq!(
            served.status, 200,
            "case {parameter:?}: expected HTTP 200, got {} with body {:?}",
            served.status,
            text(&served)
        );
        assert_eq!(
            served.header(header).as_deref(),
            Some(value),
            "case {parameter:?}: the answer must carry the override on {header}, got {:?}",
            served.header(header)
        );
    }

    // The rows differ along HOW the parameter misses the set: a published name with its last
    // character dropped, one with a character added, one with the words run together, and a
    // name that is no override at all.
    let rows: [(&str, &str, &str); 4] = [
        ("a published name one character short", "response-content-typ", "text/plain"),
        ("a published name with a character added", "response-expiress", "0"),
        ("a published name run together", "response-contenttype", "text/plain"),
        ("a name that is no override", "response-charset", "utf-8"),
    ];

    for (case, parameter, value) in rows {
        let query = format!("?{parameter}={value}");
        let refused = get_object(&router, name, key, &query, &[]).await;
        assert_refused_argument(
            &refused,
            &format!("{case}/GetObject"),
            400,
            INVALID_ARGUMENT,
            &format!(
                "{parameter} is not in the set of overridable response headers. Please refer \
                 to the S3 API documentation for a complete list of overridable response \
                 headers."
            ),
            parameter,
            value,
        );
        let headed = head_object(&router, name, key, &query).await;
        assert_eq!(
            headed.status, 400,
            "case {case:?}/HeadObject: expected HTTP 400, got {}",
            headed.status
        );
    }
}

#[tokio::test]
async fn a_version_id_stated_with_no_text_is_refused_with_its_own_sentence() {
    let router = router();
    let name = "read-parameters-version-id";
    bucket(&router, name).await;
    let key = "ledger.bin";
    let body = b"version-id-parameter-subject";
    let written = put_object(&router, name, key, body, &[]).await;
    assert_eq!(
        written.status, 200,
        "the object must be stored first, got {} with body {:?}",
        written.status,
        text(&written)
    );

    // The two sentences of ONE parameter: the empty value has its own, and every token that
    // is not a version id keeps the other. The rows span both, so neither is reused for the
    // other.
    let rows: [(&str, &str, &str); 3] = [
        ("stated with no text", "", EMPTY_VERSION_ID_MESSAGE),
        ("a word that is no id", "not-a-version", INVALID_VERSION_ID_MESSAGE),
        (
            "a token of the published width with a character outside the alphabet",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa!",
            INVALID_VERSION_ID_MESSAGE,
        ),
    ];

    for (case, version_id, message) in rows {
        for (call, answer) in [
            (
                "GetObject",
                get_object(&router, name, key, &format!("?versionId={version_id}"), &[]).await,
            ),
            (
                "DeleteObject",
                delete_object(&router, name, key, &format!("?versionId={version_id}")).await,
            ),
            (
                "GetObjectAttributes",
                get_object_attributes(&router, name, key, &format!("&versionId={version_id}"))
                    .await,
            ),
        ] {
            assert_refused_argument(
                &answer,
                &format!("{case}/{call}"),
                400,
                INVALID_ARGUMENT,
                message,
                "versionId",
                version_id,
            );
        }
        let headed = head_object(&router, name, key, &format!("?versionId={version_id}")).await;
        assert_eq!(
            headed.status, 400,
            "case {case:?}/HeadObject: expected HTTP 400, got {}",
            headed.status
        );
    }

    // Every refusal above left the object readable at its own address.
    let read = get_object(&router, name, key, "", &[]).await;
    assert_eq!(
        read.body.as_ref(),
        body,
        "a refused parameter must leave the object as it was, got {:?}",
        text(&read)
    );
}
