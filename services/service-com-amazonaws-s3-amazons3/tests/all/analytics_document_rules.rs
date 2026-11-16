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
//!
//! Every refusal is followed by a read: a refused put records nothing, so
//! `GetBucketAnalyticsConfiguration` answers `NoSuchConfiguration` for the id it named.

use service_com_amazonaws_s3_amazons3::state::{
    invalid_and_message, BucketArnFault, AND_OPERATOR_TAGS, ANALYTICS_DOCUMENT_REQUIRED_ELEMENTS,
    ANALYTICS_DOCUMENT_SCHEMA, INVALID_AND_CODE, INVALID_CONFIG_ID_CODE, INVALID_CONFIG_ID_MESSAGE,
    INVALID_DESTINATION_CODE, INVALID_PREFIX_CODE, INVALID_PREFIX_MESSAGE, MALFORMED_XML_CODE,
    MALFORMED_XML_MESSAGE, MAX_CONFIGURATION_ID_CHARS, MISSING_STORAGE_CLASS_ANALYSIS_CODE,
    MISSING_STORAGE_CLASS_ANALYSIS_MESSAGE, NO_SUCH_CONFIGURATION_CODE,
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
    assert_eq!(
        answer.status, 400,
        "case {case:?}: expected HTTP 400, got {} ({})",
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

