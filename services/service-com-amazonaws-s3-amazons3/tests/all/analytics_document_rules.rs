// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the rules an `AnalyticsConfiguration` DOCUMENT is judged by before
//! `PutBucketAnalyticsConfiguration` records anything, and for the refusal each one carries.
//!
//! Every assertion drives a real HTTP request through the router (the harness the
//! bucket-lifecycle suite owns), so what is pinned is the wire behaviour — status, code,
//! message — rather than any handler's internals.
//!
//! The rules under test, with their sources:
//!
//! * The DOCUMENT is validated against the schema the API publishes for the payload shape:
//!   every element has to be declared in the position it arrived in and every container has
//!   to name the children the shape requires
//!   (`smithy_query(query="closure",operation="PutBucketAnalyticsConfiguration")` is where
//!   [`ANALYTICS_DOCUMENT_SCHEMA`] and [`ANALYTICS_DOCUMENT_REQUIRED_ELEMENTS`] come from).
//!   A document that is not one is refused `MalformedXML` (HTTP 400) and NOTHING is recorded
//!   — a codec reads an element it does not know by ignoring it, so what the caller spelled
//!   has to be judged from the bytes rather than from the configuration parsed out of them.
//! * A document that names no `StorageClassAnalysis` is the one fault S3 publishes a code of
//!   its own for: `MissingStorageClassAnalysis` (HTTP 400) and "Storage Class Analysis cannot
//!   be null." — that container is `@required` and is what the configuration configures.
//! * A `Filter` is a CHOICE of exactly one predicate ("a filter must have exactly one
//!   prefix, one tag, or one conjunction (AnalyticsAndOperator)", `AnalyticsFilter`), each
//!   predicate has its own rule, and a refusal names the PREDICATE rather than the document:
//!   a conjunction outside its published bounds — [`AND_OPERATOR_TAGS`] tags and at least
//!   [`MIN_AND_OPERATOR_ELEMENTS`] elements — is `InvalidAnd`, and a `Prefix` stated EMPTY is
//!   `InvalidPrefix`, while leaving the member out is a filter with no prefix condition.
//! * An export destination is "the Amazon Resource Name (ARN) of the bucket to which data is
//!   exported" (`AnalyticsS3BucketDestination$Bucket`), so every field of that ARN is judged
//!   in the ARN's own order and the refusal states the rule the value broke
//!   ([`BucketArnFault`]), under `InvalidDestination` (HTTP 400).
//! * The id-length refusal states the bound in S3's own fixed sentence, and the exclusivity
//!   refusal of a website configuration states the STATE the conflicting container was in
//!   rather than the value it carried — neither echoes anything of the request.
//! * All of those checks are made in ONE fixed order, from the widest statement about the
//!   request to the narrowest value inside it: the DOCUMENT against the published schema,
//!   then the BUCKET the path names, then the `Id` the request is addressed by, then the
//!   payload's own `Id` against it, then the filter PREDICATE, and last the value of the
//!   export DESTINATION. A request wrong in several ways answers the FIRST fault of that
//!   order, so a caller repairing one field at a time is never told about a field the
//!   service has not reached yet.
//! * The id rule bounds what a PUT may RECORD: neither `GetBucketAnalyticsConfiguration` nor
//!   `DeleteBucketAnalyticsConfiguration` judges an id's spelling or its length, so an id of
//!   ANY shape the bucket carries nothing under is the plain not-there refusal
//!   `NoSuchConfiguration` (HTTP 404) on both.
//!
//! Every refusal is followed by a read: a refused put records nothing, so
//! `GetBucketAnalyticsConfiguration` answers `NoSuchConfiguration` for the id it named.

use service_com_amazonaws_s3_amazons3::state::{
    invalid_and_message, BucketArnFault, AND_OPERATOR_TAGS, ANALYTICS_DOCUMENT_REQUIRED_ELEMENTS,
    ANALYTICS_DOCUMENT_SCHEMA, INVALID_AND_CODE, INVALID_CONFIG_ID_CODE, INVALID_CONFIG_ID_MESSAGE,
    INVALID_CONFIGURATION_ID_CODE, INVALID_CONFIGURATION_ID_MESSAGE,
    INVALID_CONFIG_ID_PATTERN_MESSAGE, INVALID_DESTINATION_CODE, INVALID_PREFIX_CODE,
    INVALID_PREFIX_MESSAGE, MALFORMED_XML_CODE, MALFORMED_XML_MESSAGE,
    MAX_CONFIGURATION_ID_CHARS, MISSING_STORAGE_CLASS_ANALYSIS_CODE,
    MISSING_STORAGE_CLASS_ANALYSIS_MESSAGE, NO_SUCH_BUCKET_CODE, NO_SUCH_BUCKET_MESSAGE,
    NO_SUCH_CONFIGURATION_CODE, NO_SUCH_CONFIGURATION_MESSAGE, NO_SUCH_CONFIGURATION_STATUS,
};
use smithy_server_runtime::MultiServiceRouter;

use super::bucket_lifecycle::{create_bucket, router, send, Answer};

/// The XML namespace botocore puts on an S3 request payload.
const XMLNS: &str = "http://s3.amazonaws.com/doc/2006-03-01/";

/// The only value `StorageClassAnalysisSchemaVersion` takes
/// (`smithy_query(query="shape",shape="StorageClassAnalysisSchemaVersion")`).
const SCHEMA_VERSION: &str = "V_1";

/// The only value `AnalyticsS3ExportFileFormat` takes
/// (`smithy_query(query="shape",shape="AnalyticsS3ExportFileFormat")`).
const EXPORT_FORMAT: &str = "CSV";

/// The destination bucket every accepted document below exports to, as the ARN form a bucket
/// ARN takes: `arn:aws:s3:::<name>`, carrying neither a Region nor an account id.
const DESTINATION_ARN: &str = "arn:aws:s3:::analytics-destination";

/// `PUT /<bucket>?analytics&id=<id>` with `payload` as the body.
async fn put_analytics(
    router: &MultiServiceRouter,
    bucket: &str,
    id: &str,
    payload: &str,
) -> Answer {
    send(
        router,
        "PUT",
        &format!("/{bucket}?analytics&id={id}"),
        &[],
        payload,
    )
    .await
}

/// `GET /<bucket>?analytics&id=<id>` — `GetBucketAnalyticsConfiguration`.
async fn get_analytics(router: &MultiServiceRouter, bucket: &str, id: &str) -> Answer {
    send(
        router,
        "GET",
        &format!("/{bucket}?analytics&id={id}"),
        &[],
        "",
    )
    .await
}

/// A `<StorageClassAnalysis>` exporting to `destination`, which is the one the accepted rows
/// use and the one every destination row varies the ARN of.
fn analysis(destination: &str) -> String {
    format!(
        "<StorageClassAnalysis><DataExport>\
         <OutputSchemaVersion>{SCHEMA_VERSION}</OutputSchemaVersion><Destination>\
         <S3BucketDestination><Format>{EXPORT_FORMAT}</Format><Bucket>{destination}</Bucket>\
         </S3BucketDestination></Destination></DataExport></StorageClassAnalysis>"
    )
}

/// One whole document for `id`, carrying `members` between the `Id` and the analysis.
fn document(id: &str, members: &str, analysis_element: &str) -> String {
    format!(
        "<AnalyticsConfiguration xmlns=\"{XMLNS}\"><Id>{id}</Id>{members}{analysis_element}\
         </AnalyticsConfiguration>"
    )
}

/// The bucket each test stands up, named after the test so the suites do not share state.
async fn bucket(router: &MultiServiceRouter, name: &str) {
    let created = create_bucket(router, name, None, None).await;
    assert_eq!(
        created.status,
        200,
        "case setup: expected the bucket {name:?} to be created, got {} ({})",
        created.status,
        String::from_utf8_lossy(&created.body)
    );
}

/// Assert that `answer` is the refusal `(code, message)` at HTTP 400, naming `case`.
fn refused(case: &str, answer: &Answer, code: &str, message: &str) {
    answered(case, answer, 400, code, message);
}

/// Assert that `answer` carries `status`, `code` and `message`, naming `case` and the value
/// that was actually answered — [`refused`] for the 400 most of the rules above carry.
fn answered(case: &str, answer: &Answer, status: u16, code: &str, message: &str) {
    assert_eq!(
        answer.status, status,
        "case {case:?}: expected HTTP {status}, got {} ({})",
        answer.status,
        String::from_utf8_lossy(&answer.body)
    );
    assert_eq!(
        answer.error_code(),
        code,
        "case {case:?}: expected the code {code:?}, got {:?}",
        answer.error_code()
    );
    assert_eq!(
        answer.error_message(),
        message,
        "case {case:?}: expected the message {message:?}, got {:?}",
        answer.error_message()
    );
}

/// A document the published schema rejects is refused `MalformedXML` and records nothing.
///
/// The rows span the ways a document fails to validate rather than one recorded body: an
/// element the schema declares nowhere, a declared element in a position the schema does not
/// declare it in, a repeated choice, a container missing a required child (one row per
/// container that requires one, taken from [`ANALYTICS_DOCUMENT_REQUIRED_ELEMENTS`] other
/// than the analysis itself, which has its own code), and bytes that are not a document at
/// all.
#[tokio::test]
async fn a_document_outside_the_published_schema_is_refused_and_records_nothing() {
    let router = router();
    let name = "analytics-schema-faults";
    bucket(&router, name).await;

    let valid = analysis(DESTINATION_ARN);
    let rows: Vec<(&str, String)> = vec![
        (
            "an element the schema declares nowhere",
            document("undeclared", "<Favourite>yes</Favourite>", &valid),
        ),
        (
            "a declared element one level too high",
            document("misnested", "<Prefix>logs/</Prefix>", &valid),
        ),
        (
            "a declared element inside a container that does not declare it",
            document(
                "misplaced",
                "<Filter><Prefix>logs/</Prefix><DataExport></DataExport></Filter>",
                &valid,
            ),
        ),
        (
            "a tag predicate missing the value it matches",
            document("tag-no-value", "<Filter><Tag><Key>team</Key></Tag></Filter>", &valid),
        ),
        (
            "a tag predicate missing the key it matches",
            document("tag-no-key", "<Filter><Tag><Value>finance</Value></Tag></Filter>", &valid),
        ),
        (
            "a data export naming no schema version",
            document(
                "no-schema-version",
                "",
                "<StorageClassAnalysis><DataExport><Destination><S3BucketDestination>\
                 <Format>CSV</Format><Bucket>arn:aws:s3:::analytics-destination</Bucket>\
                 </S3BucketDestination></Destination></DataExport></StorageClassAnalysis>",
            ),
        ),
        (
            "a data export naming no destination",
            document(
                "no-destination",
                "",
                "<StorageClassAnalysis><DataExport>\
                 <OutputSchemaVersion>V_1</OutputSchemaVersion></DataExport>\
                 </StorageClassAnalysis>",
            ),
        ),
        (
            "a destination naming no bucket",
            document(
                "no-bucket",
                "",
                "<StorageClassAnalysis><DataExport>\
                 <OutputSchemaVersion>V_1</OutputSchemaVersion><Destination>\
                 <S3BucketDestination><Format>CSV</Format></S3BucketDestination></Destination>\
                 </DataExport></StorageClassAnalysis>",
            ),
        ),
        (
            "a destination naming no format",
            document(
                "no-format",
                "",
                "<StorageClassAnalysis><DataExport>\
                 <OutputSchemaVersion>V_1</OutputSchemaVersion><Destination>\
                 <S3BucketDestination><Bucket>arn:aws:s3:::analytics-destination</Bucket>\
                 </S3BucketDestination></Destination></DataExport></StorageClassAnalysis>",
            ),
        ),
        (
            "a filter naming two predicates where the choice is exactly one",
            document(
                "two-predicates",
                "<Filter><Prefix>logs/</Prefix><Tag><Key>team</Key><Value>x</Value></Tag>\
                 </Filter>",
                &valid,
            ),
        ),
        (
            "a filter naming no predicate at all",
            document("no-predicate", "<Filter></Filter>", &valid),
        ),
        (
            "a root element that is not the payload's container",
            format!(
                "<AnalyticsConfigurations xmlns=\"{XMLNS}\"><Id>other-root</Id>{valid}\
                 </AnalyticsConfigurations>"
            ),
        ),
        (
            "bytes that are not a document at all",
            "not a document".to_string(),
        ),
    ];

    for (case, payload) in rows {
        let id = "probe";
        let answer = put_analytics(&router, name, id, &payload).await;
        refused(case, &answer, MALFORMED_XML_CODE, MALFORMED_XML_MESSAGE);
        // A refused put records nothing, so the id it named still identifies no
        // configuration.
        let read = get_analytics(&router, name, id).await;
        assert_eq!(
            read.error_code(),
            NO_SUCH_CONFIGURATION_CODE,
            "case {case:?}: a refused put stores nothing, so the read must answer \
             {NO_SUCH_CONFIGURATION_CODE}, got {} ({})",
            read.status,
            String::from_utf8_lossy(&read.body)
        );
    }
}

/// A document that names no storage class analysis carries that container's own refusal,
/// whichever way it fails to name one.
#[tokio::test]
async fn a_document_naming_no_storage_class_analysis_is_refused_by_its_own_code() {
    let router = router();
    let name = "analytics-missing-analysis";
    bucket(&router, name).await;

    let rows: Vec<(&str, String)> = vec![
        (
            "a configuration naming only its id",
            document("only-id", "", ""),
        ),
        (
            "a configuration naming a filter and nothing else",
            document("filter-only", "<Filter><Prefix>logs/</Prefix></Filter>", ""),
        ),
        (
            "the analysis container misspelled",
            document(
                "misspelled-analysis",
                "",
                "<StorageClassAnalyses></StorageClassAnalyses>",
            ),
        ),
        (
            "the analysis container one level too deep",
            format!(
                "<AnalyticsConfiguration xmlns=\"{XMLNS}\"><Id>deep-analysis</Id><Filter>{}\
                 </Filter></AnalyticsConfiguration>",
                analysis(DESTINATION_ARN)
            ),
        ),
    ];

    for (case, payload) in rows {
        let id = "probe";
        let answer = put_analytics(&router, name, id, &payload).await;
        refused(
            case,
            &answer,
            MISSING_STORAGE_CLASS_ANALYSIS_CODE,
            MISSING_STORAGE_CLASS_ANALYSIS_MESSAGE,
        );
        let read = get_analytics(&router, name, id).await;
        assert_eq!(
            read.error_code(),
            NO_SUCH_CONFIGURATION_CODE,
            "case {case:?}: a refused put stores nothing, so the read must answer \
             {NO_SUCH_CONFIGURATION_CODE}, got {} ({})",
            read.status,
            String::from_utf8_lossy(&read.body)
        );
    }
}

/// Each predicate of a filter carries its own refusal, and a filter inside the published
/// shapes round-trips.
///
/// The conjunction rows walk the published bounds: no tag, each end of
/// [`AND_OPERATOR_TAGS`], one tag past the maximum, and the element count on its own. The
/// prefix rows state the empty prefix on both members that carry one.
#[tokio::test]
async fn a_filter_predicate_outside_its_rule_is_refused_by_the_predicate_s_own_code() {
    let router = router();
    let name = "analytics-filter-predicates";
    bucket(&router, name).await;

    let valid = analysis(DESTINATION_ARN);
    // One `<Tag>` per key, so a row can name any number of them.
    let tags = |count: usize| -> String {
        (0..count)
            .map(|index| format!("<Tag><Key>team{index}</Key><Value>finance</Value></Tag>"))
            .collect()
    };
    let maximum = *AND_OPERATOR_TAGS.end();

    let and_rows: Vec<(String, String)> = vec![
        (
            "a conjunction of a prefix and no tag".to_string(),
            "<Filter><And><Prefix>logs/</Prefix></And></Filter>".to_string(),
        ),
        (
            "a conjunction naming nothing at all".to_string(),
            "<Filter><And></And></Filter>".to_string(),
        ),
        (
            "a conjunction of one tag and nothing else".to_string(),
            format!("<Filter><And>{}</And></Filter>", tags(1)),
        ),
        (
            format!("a conjunction of {} tags, one past the maximum", maximum + 1),
            format!("<Filter><And>{}</And></Filter>", tags(maximum + 1)),
        ),
        (
            format!(
                "a conjunction of a prefix and {} tags, one past the maximum",
                maximum + 1
            ),
            format!(
                "<Filter><And><Prefix>logs/</Prefix>{}</And></Filter>",
                tags(maximum + 1)
            ),
        ),
    ];
    for (case, filter) in &and_rows {
        let answer = put_analytics(&router, name, "probe", &document("probe", filter, &valid)).await;
        refused(case, &answer, INVALID_AND_CODE, &invalid_and_message());
    }

    let prefix_rows: Vec<(&str, String)> = vec![
        (
            "a prefix predicate stated empty",
            "<Filter><Prefix></Prefix></Filter>".to_string(),
        ),
        (
            "a prefix predicate stated as an empty element",
            "<Filter><Prefix/></Filter>".to_string(),
        ),
        (
            "a conjunction whose prefix is stated empty",
            format!("<Filter><And><Prefix></Prefix>{}</And></Filter>", tags(1)),
        ),
    ];
    for (case, filter) in &prefix_rows {
        let answer = put_analytics(&router, name, "probe", &document("probe", filter, &valid)).await;
        refused(case, &answer, INVALID_PREFIX_CODE, INVALID_PREFIX_MESSAGE);
    }

    // Inside every rule, and read back through the implemented reader: the bounds are
    // inclusive at each end, and a filter that names no prefix at all is a filter with no
    // prefix condition rather than one with an empty prefix.
    let accepted: Vec<(String, String)> = vec![
        (
            "no filter at all".to_string(),
            String::new(),
        ),
        (
            "a prefix predicate".to_string(),
            "<Filter><Prefix>logs/</Prefix></Filter>".to_string(),
        ),
        (
            "a tag predicate".to_string(),
            format!("<Filter>{}</Filter>", tags(1)),
        ),
        (
            format!(
                "a conjunction of {} tags, the fewest the bounds hold",
                AND_OPERATOR_TAGS.start()
            ),
            format!(
                "<Filter><And><Prefix>logs/</Prefix>{}</And></Filter>",
                tags(*AND_OPERATOR_TAGS.start())
            ),
        ),
        (
            format!("a conjunction of {maximum} tags, the most the bounds hold"),
            format!("<Filter><And>{}</And></Filter>", tags(maximum)),
        ),
    ];
    for (index, (case, filter)) in accepted.iter().enumerate() {
        let id = format!("accepted-{index}");
        let answer = put_analytics(&router, name, &id, &document(&id, filter, &valid)).await;
        assert_eq!(
            answer.status,
            204,
            "case {case:?}: expected the put to be recorded with HTTP 204, got {} ({})",
            answer.status,
            String::from_utf8_lossy(&answer.body)
        );
        let read = get_analytics(&router, name, &id).await;
        assert_eq!(
            read.status,
            200,
            "case {case:?}: expected the stored configuration to read back, got {} ({})",
            read.status,
            String::from_utf8_lossy(&read.body)
        );
        assert_eq!(
            read.element("Id").as_deref(),
            Some(id.as_str()),
            "case {case:?}: expected the read to report the id {id:?}, got {:?}",
            read.element("Id")
        );
    }
}

/// An export destination is judged as a BUCKET ARN, field by field, and the refusal states
/// the rule the value broke.
///
/// One row per class the reading tells apart, each spelled a different way, plus the ARN
/// form that is accepted.
#[tokio::test]
async fn an_export_destination_is_judged_as_a_bucket_arn() {
    let router = router();
    let name = "analytics-destinations";
    bucket(&router, name).await;

    let rows: Vec<(&str, &str, BucketArnFault)> = vec![
        (
            "a destination stated empty",
            "",
            BucketArnFault::Empty,
        ),
        (
            "a destination named by the bare bucket name",
            "analytics-destination",
            BucketArnFault::NotAnArn,
        ),
        (
            "a destination that is no ARN at all",
            "s3://analytics-destination",
            BucketArnFault::NotAnArn,
        ),
        (
            "a destination ARN of another service",
            "arn:aws:sqs:::analytics-destination",
            BucketArnFault::ForeignVendor,
        ),
        (
            "a destination ARN qualified by a Region",
            "arn:aws:s3:us-west-2::analytics-destination",
            BucketArnFault::RegionQualified,
        ),
        (
            // The ARN's own field ORDER decides which rule a value breaking two of them is
            // reported against: the Region comes before the account.
            "a destination ARN qualified by a Region and an account",
            "arn:aws:s3:us-west-2:123456789012:analytics-destination",
            BucketArnFault::RegionQualified,
        ),
        (
            "a destination ARN qualified by an account alone",
            "arn:aws:s3::123456789012:analytics-destination",
            BucketArnFault::NamespaceQualified,
        ),
        (
            "a destination ARN whose resource is a bucket name and a key prefix",
            "arn:aws:s3:::analytics-destination/exports/",
            BucketArnFault::InvalidBucketName {
                name: "analytics-destination/exports/".to_string(),
            },
        ),
        (
            "a destination ARN whose resource is no bucket name at all",
            "arn:aws:s3:::no",
            BucketArnFault::InvalidBucketName {
                name: "no".to_string(),
            },
        ),
    ];

    for (case, destination, fault) in rows {
        let answer = put_analytics(
            &router,
            name,
            "probe",
            &document("probe", "", &analysis(destination)),
        )
        .await;
        refused(case, &answer, INVALID_DESTINATION_CODE, &fault.message());
    }

    let accepted = put_analytics(
        &router,
        name,
        "accepted",
        &document("accepted", "", &analysis(DESTINATION_ARN)),
    )
    .await;
    assert_eq!(
        accepted.status,
        204,
        "case \"a bucket ARN carrying neither a Region nor an account\": expected HTTP 204, \
         got {} ({})",
        accepted.status,
        String::from_utf8_lossy(&accepted.body)
    );
}

/// Both tables the document schema is walked against are derived from the model, so every
/// element they name is one of the payload shape's closure and every required child is a
/// member that closure marks `@required`.
///
/// Source: `smithy_query(query="closure",operation="PutBucketAnalyticsConfiguration")`.
#[test]
fn the_document_schema_tables_name_the_payload_shape_s_own_elements() {
    // The ROOT of the schema is the payload's own container.
    assert_eq!(
        ANALYTICS_DOCUMENT_SCHEMA[0].0, "AnalyticsConfiguration",
        "the schema's first row is the payload container, got {:?}",
        ANALYTICS_DOCUMENT_SCHEMA[0].0
    );
    // Every child the schema declares is an element the schema declares a position for, or a
    // LEAF that carries text; and every container a required child is declared under is a
    // container the schema knows.
    for (container, child) in ANALYTICS_DOCUMENT_REQUIRED_ELEMENTS {
        let declared = ANALYTICS_DOCUMENT_SCHEMA
            .iter()
            .find(|(element, _)| element == container)
            .map(|(_, children)| children.contains(child));
        assert_eq!(
            declared,
            Some(true),
            "case {container:?}/{child:?}: a required child has to be declared inside its \
             container, got {declared:?}"
        );
    }
}

/// The id-length refusal carries S3's own fixed sentence, which states the bound rather than
/// echoing the id: the bound is inclusive, so the longest id S3 records is
/// [`MAX_CONFIGURATION_ID_CHARS`] characters and one past it is refused.
#[tokio::test]
async fn the_id_length_refusal_states_the_published_bound() {
    let router = router();
    let name = "analytics-id-length";
    bucket(&router, name).await;

    let analysis_element = analysis(DESTINATION_ARN);
    for length in [
        MAX_CONFIGURATION_ID_CHARS + 1,
        MAX_CONFIGURATION_ID_CHARS + 2,
        MAX_CONFIGURATION_ID_CHARS * 2,
    ] {
        let id = "i".repeat(length);
        let case = format!("an id of {length} characters");
        let answer = put_analytics(&router, name, &id, &document(&id, "", &analysis_element)).await;
        refused(
            &case,
            &answer,
            INVALID_CONFIG_ID_CODE,
            INVALID_CONFIG_ID_MESSAGE,
        );
    }

    let id = "i".repeat(MAX_CONFIGURATION_ID_CHARS);
    let answer = put_analytics(&router, name, &id, &document(&id, "", &analysis_element)).await;
    assert_eq!(
        answer.status,
        204,
        "case \"an id exactly at the published maximum\": expected HTTP 204, got {} ({})",
        answer.status,
        String::from_utf8_lossy(&answer.body)
    );
}

/// A destination written as the BARE bucket name, which is no ARN at all.
const BARE_NAME_DESTINATION: &str = "analytics-destination";

/// The same destination bucket written as an ARN qualified by a Region, which a bucket ARN
/// never carries.
const REGION_QUALIFIED_DESTINATION: &str = "arn:aws:s3:us-west-2::analytics-destination";

/// An id spelled with a character the id rule's own class does not hold.
const MALFORMED_ID: &str = "bad~id";

/// One row of the refusal-order table: what is wrong with the request, the request itself,
/// and the one refusal it must answer.
struct OrderRow {
    case: &'static str,
    /// Whether the bucket the request addresses is one this emulator holds: the bucket is
    /// judged after the document and before everything else, so one row addresses a bucket
    /// that is not there.
    bucket_held: bool,
    /// The `id` query parameter the request is addressed by.
    id: String,
    /// The whole request payload.
    payload: String,
    /// The status, the code and the message that refusal carries.
    expected: (u16, &'static str, String),
}

/// A `<StorageClassAnalysis>` exporting to `destination` under `account`, which is the one
/// form [`analysis`] cannot spell: `AnalyticsS3BucketDestination$BucketAccountId` is "the
/// account ID that owns the destination S3 bucket", and the rows below write it EMPTY.
///
/// Its children are the ones the published schema declares for that container
/// (`smithy_query(query="closure",operation="PutBucketAnalyticsConfiguration")`).
fn analysis_owned_by(destination: &str, account: &str) -> String {
    format!(
        "<StorageClassAnalysis><DataExport>\
         <OutputSchemaVersion>{SCHEMA_VERSION}</OutputSchemaVersion><Destination>\
         <S3BucketDestination><Format>{EXPORT_FORMAT}</Format>\
         <BucketAccountId>{account}</BucketAccountId><Bucket>{destination}</Bucket>\
         </S3BucketDestination></Destination></DataExport></StorageClassAnalysis>"
    )
}

/// The refusal-order table: every row is a request at least one check refuses, and the rows
/// differ along WHICH checks it breaks at once — so each pair pins which of the two is
/// reported.
///
/// The first three rows break one check alone (the later check does fire when it is the only
/// one broken); every row after them breaks the document, the bucket, the id, the payload's
/// own id, the filter or the destination in more than one place at once, with the earlier
/// check's refusal expected.
fn order_rows() -> Vec<OrderRow> {
    let long_id = "x".repeat(MAX_CONFIGURATION_ID_CHARS + 1);
    let bare_name = analysis(BARE_NAME_DESTINATION);
    let region_qualified = analysis(REGION_QUALIFIED_DESTINATION);
    let well_formed = analysis(DESTINATION_ARN);
    let invalid_destination = |fault: BucketArnFault| -> (u16, &'static str, String) {
        (400, INVALID_DESTINATION_CODE, fault.message())
    };
    let malformed_document = || -> (u16, &'static str, String) {
        (400, MALFORMED_XML_CODE, MALFORMED_XML_MESSAGE.to_string())
    };

    vec![
        OrderRow {
            case: "a destination written as the bare bucket name, and nothing else wrong",
            bucket_held: true,
            id: "probe".to_string(),
            payload: document("probe", "", &bare_name),
            expected: invalid_destination(BucketArnFault::NotAnArn),
        },
        OrderRow {
            case: "a destination ARN qualified by a Region, and nothing else wrong",
            bucket_held: true,
            id: "probe".to_string(),
            payload: document("probe", "", &region_qualified),
            expected: invalid_destination(BucketArnFault::RegionQualified),
        },
        OrderRow {
            case: "a destination account id written empty, and nothing else wrong",
            bucket_held: true,
            id: "probe".to_string(),
            payload: document("probe", "", &analysis_owned_by(DESTINATION_ARN, "")),
            expected: malformed_document(),
        },
        OrderRow {
            case: "an element the schema declares nowhere, over a malformed id and a \
                   bare-name destination",
            bucket_held: true,
            id: MALFORMED_ID.to_string(),
            payload: document(MALFORMED_ID, "<Favourite>yes</Favourite>", &bare_name),
            expected: malformed_document(),
        },
        OrderRow {
            case: "a document naming no storage class analysis, over a malformed id",
            bucket_held: true,
            id: MALFORMED_ID.to_string(),
            payload: document(MALFORMED_ID, "", ""),
            expected: (
                400,
                MISSING_STORAGE_CLASS_ANALYSIS_CODE,
                MISSING_STORAGE_CLASS_ANALYSIS_MESSAGE.to_string(),
            ),
        },
        OrderRow {
            case: "a bucket this emulator does not hold, over a malformed id and a \
                   bare-name destination",
            bucket_held: false,
            id: MALFORMED_ID.to_string(),
            payload: document(MALFORMED_ID, "", &bare_name),
            expected: (404, NO_SUCH_BUCKET_CODE, NO_SUCH_BUCKET_MESSAGE.to_string()),
        },
        OrderRow {
            case: "a malformed id, over a bare-name destination",
            bucket_held: true,
            id: MALFORMED_ID.to_string(),
            payload: document(MALFORMED_ID, "", &bare_name),
            expected: (
                400,
                INVALID_CONFIG_ID_CODE,
                INVALID_CONFIG_ID_PATTERN_MESSAGE.to_string(),
            ),
        },
        OrderRow {
            case: "an id past the published maximum, over a Region-qualified destination",
            bucket_held: true,
            id: long_id.clone(),
            payload: document(&long_id, "", &region_qualified),
            expected: (
                400,
                INVALID_CONFIG_ID_CODE,
                INVALID_CONFIG_ID_MESSAGE.to_string(),
            ),
        },
        OrderRow {
            case: "an id stated empty, over a bare-name destination",
            bucket_held: true,
            id: String::new(),
            payload: document("", "", &bare_name),
            expected: (
                400,
                INVALID_CONFIGURATION_ID_CODE,
                INVALID_CONFIGURATION_ID_MESSAGE.to_string(),
            ),
        },
        OrderRow {
            case: "a malformed id, over a payload Id that is not the one addressed",
            bucket_held: true,
            id: MALFORMED_ID.to_string(),
            payload: document("another-id", "", &well_formed),
            expected: (
                400,
                INVALID_CONFIG_ID_CODE,
                INVALID_CONFIG_ID_PATTERN_MESSAGE.to_string(),
            ),
        },
        OrderRow {
            case: "a payload Id that is not the one addressed, over a bare-name destination",
            bucket_held: true,
            id: "probe".to_string(),
            payload: document("another-id", "", &bare_name),
            expected: malformed_document(),
        },
        OrderRow {
            case: "a prefix predicate stated empty, over a bare-name destination",
            bucket_held: true,
            id: "probe".to_string(),
            payload: document("probe", "<Filter><Prefix></Prefix></Filter>", &bare_name),
            expected: (
                400,
                INVALID_PREFIX_CODE,
                INVALID_PREFIX_MESSAGE.to_string(),
            ),
        },
        OrderRow {
            case: "a conjunction of one element, over a bare-name destination",
            bucket_held: true,
            id: "probe".to_string(),
            payload: document(
                "probe",
                "<Filter><And><Prefix>logs/</Prefix></And></Filter>",
                &bare_name,
            ),
            expected: (400, INVALID_AND_CODE, invalid_and_message()),
        },
        OrderRow {
            case: "a bare-name destination, over a destination account id written empty",
            bucket_held: true,
            id: "probe".to_string(),
            payload: document("probe", "", &analysis_owned_by(BARE_NAME_DESTINATION, "")),
            expected: invalid_destination(BucketArnFault::NotAnArn),
        },
    ]
}

/// A request wrong in several ways answers the FIRST check of the put's order, and records
/// nothing whichever check refused it.
#[tokio::test]
async fn a_put_reports_the_first_check_its_request_fails() {
    let router = router();
    let name = "analytics-refusal-order";
    bucket(&router, name).await;

    for row in order_rows() {
        let addressed = match row.bucket_held {
            true => name,
            // A bucket this emulator does not hold: the row addresses one that was never
            // created.
            false => "analytics-refusal-order-absent",
        };
        let answer = put_analytics(&router, addressed, &row.id, &row.payload).await;
        let (status, code, message) = &row.expected;
        answered(row.case, &answer, *status, code, message);

        // Nothing the refused put named was recorded, so the id it addressed still
        // identifies no configuration. An id stated empty names none to read.
        if row.bucket_held && !row.id.is_empty() {
            let read = get_analytics(&router, name, &row.id).await;
            answered(
                row.case,
                &read,
                NO_SUCH_CONFIGURATION_STATUS,
                NO_SUCH_CONFIGURATION_CODE,
                NO_SUCH_CONFIGURATION_MESSAGE,
            );
        }
    }

    // And the bucket's listing names nothing: every row was refused before anything was
    // written.
    let listing = send(&router, "GET", &format!("/{name}?analytics"), &[], "").await;
    let reported = listing.elements("Id");
    assert!(
        reported.is_empty(),
        "case \"after the refusal-order table\": every row was refused, so the listing must \
         name no configuration, got {reported:?}"
    );

    // The order lets a request that breaks none of the checks through, which is what makes
    // each refusal above a statement about that request rather than about the subresource.
    let accepted = put_analytics(
        &router,
        name,
        "accepted",
        &document(
            "accepted",
            "<Filter><Prefix>logs/</Prefix></Filter>",
            &analysis(DESTINATION_ARN),
        ),
    )
    .await;
    assert_eq!(
        accepted.status,
        204,
        "case \"a request no check refuses\": expected HTTP 204, got {} ({})",
        accepted.status,
        String::from_utf8_lossy(&accepted.body)
    );
    let read = get_analytics(&router, name, "accepted").await;
    assert_eq!(
        read.element("Id").as_deref(),
        Some("accepted"),
        "case \"a request no check refuses\": expected the read to report the id it was \
         recorded under, got {:?}",
        read.element("Id")
    );
}

/// Neither the read nor the delete judges the id it names: an id of ANY shape the bucket
/// carries nothing under is the plain not-there refusal on both.
///
/// The rows differ along what the PUT's id rule generalises over — each kind of character
/// outside its class, and a length one past the published maximum and far past it — plus an
/// id the rule admits that was simply never recorded.
#[tokio::test]
async fn a_read_and_a_delete_never_judge_the_id_they_name() {
    let router = router();
    let name = "analytics-unjudged-ids";
    bucket(&router, name).await;

    let rows: Vec<(&str, String)> = vec![
        (
            "an id the rule admits, never recorded",
            "export7".to_string(),
        ),
        ("an id spelled with a tilde", MALFORMED_ID.to_string()),
        ("an id spelled with a colon", "has:colon".to_string()),
        (
            "an id spelled with a percent-encoded space",
            "has%20space".to_string(),
        ),
        (
            "an id spelled with a percent-encoded slash",
            "has%2Fslash".to_string(),
        ),
        (
            "an id spelled with a percent-encoded non-ASCII letter",
            "caf%C3%A9".to_string(),
        ),
        (
            "an id one character past the published maximum",
            "x".repeat(MAX_CONFIGURATION_ID_CHARS + 1),
        ),
        (
            "an id far past the published maximum",
            "x".repeat(MAX_CONFIGURATION_ID_CHARS * 4),
        ),
    ];

    for (case, id) in rows {
        let read = get_analytics(&router, name, &id).await;
        answered(
            case,
            &read,
            NO_SUCH_CONFIGURATION_STATUS,
            NO_SUCH_CONFIGURATION_CODE,
            NO_SUCH_CONFIGURATION_MESSAGE,
        );
        let deleted = send(
            &router,
            "DELETE",
            &format!("/{name}?analytics&id={id}"),
            &[],
            "",
        )
        .await;
        answered(
            case,
            &deleted,
            NO_SUCH_CONFIGURATION_STATUS,
            NO_SUCH_CONFIGURATION_CODE,
            NO_SUCH_CONFIGURATION_MESSAGE,
        );
    }
}

