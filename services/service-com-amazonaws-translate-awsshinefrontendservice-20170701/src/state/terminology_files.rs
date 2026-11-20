// SPDX-License-Identifier: CC0-1.0

//! WHERE a custom terminology's files are, and what makes the locations this
//! service announces for them fetchable.
//!
//! `GetTerminology` reports `TerminologyDataLocation`, "The Amazon S3 location of
//! the most recent custom terminology input file that was successfully imported
//! into Amazon Translate. The location is returned as a presigned URL that has a
//! 30-minute expiration", and both it and `ImportTerminology` report
//! `AuxiliaryDataLocation`, "The Amazon S3 location of a file that provides any
//! errors or warnings that were produced by your input file … returned as a
//! presigned URL" (`smithy_query shape=TerminologyDataLocation member=Location`,
//! `smithy_query shape=ImportTerminologyResponse member=AuxiliaryDataLocation`).
//!
//! THE ONE RULE both follow: a location an operation hands a CALLER to fetch is
//! an address on the endpoint that caller reached this emulator on, serving the
//! object it names. This emulator is reachable at its own endpoint alone and no
//! production AWS hostname resolves to it, so a location on such a hostname can
//! never be followed from the environment the API was called in, however exactly
//! its bucket and key are spelled — the operation's only useful output member
//! would be a dead link.
//!
//! So a location is PRESIGNED through the framework's own signer
//! ([`presigned_object_location`]), which signs for the base URL this emulator is
//! reached on, and the object behind it is a real one in this account's object
//! store: [`store_terminology_files`] writes the imported file there over the
//! in-process callback as part of the import, under the bucket
//! ([`terminology_bucket`]) and key ([`terminology_object_key`]) the announced
//! address carries, and [`withdraw_terminology_files`] takes them back when the
//! resource is deleted. So the address reported and the object stored cannot
//! drift apart, and a client that follows the documented workflow — read the
//! terminology, then download its input file from the returned URL — is served
//! the bytes the import stored.
//!
//! The file is stored in each format the read may announce
//! ([`announced_formats`]): the imported format holds the uploaded bytes
//! THEMSELVES, and a uni-directional resource — which `GetTerminology` may be
//! asked for in another format — holds the other two as renderings of the stored
//! terms, so every location this service announces names an object that is there.

use std::time::Duration;

use super::{Terminology, FORMAT_CSV, FORMAT_TMX, FORMAT_TSV};
use crate::ops::types::TerminologyDataLocation;

/// The repository a terminology's files live in, as `RepositoryType` names it
/// ("S3"): this account's object store, which this emulator serves.
const REPOSITORY_TYPE_S3: &str = "S3";

/// The signing name of that store, which is the service a fetch of an announced
/// location reaches: the files are written there over the in-process callback, so
/// the signed address names the very service that holds them.
const OBJECT_STORE_SIGNING_NAME: &str = "s3";

/// How long an announced location stays valid: "The location is returned as a
/// presigned URL that has a 30-minute expiration" (`smithy_query
/// shape=TerminologyDataLocation member=Location`).
const PRESIGNED_EXPIRY: Duration = Duration::from_secs(30 * 60);

/// The Region whose buckets are created with no location constraint, which is the
/// one Amazon S3 takes as the absence of one.
const UNCONSTRAINED_BUCKET_REGION: &str = "us-east-1";

/// What Amazon S3 answers a `CreateBucket` for a name the one bucket namespace
/// already holds, which is this service's own bucket already being there.
const BUCKET_EXISTS_STATUS: u16 = 409;

/// The media type each stored rendering carries, by the format it is in.
const CONTENT_TYPE_CSV: &str = "text/csv";
/// A tab-separated rendering's media type.
const CONTENT_TYPE_TSV: &str = "text/tab-separated-values";
/// A TMX rendering's media type: TMX is an XML vocabulary.
const CONTENT_TYPE_TMX: &str = "application/xml";

/// How many warnings the auxiliary file reports. The input file is caller-sized
/// and every one of its rows may be a skipped one, so the report is bounded like
/// every other answer computed from it; the row it ends on is the last one the
/// file names, and the count the resource reports
/// (`TerminologyProperties$SkippedTermCount`) still covers them all.
pub const MAX_AUXILIARY_WARNINGS: usize = 100;

/// The bucket this service keeps one Region's terminology files in.
///
/// Per-account and per-region, because a custom terminology is a regional
/// resource of one account: a terminology imported into one Region is not an
/// object of another's.
#[must_use]
pub fn terminology_bucket(region: &str) -> String {
    format!(
        "translate-terminology-{}-{region}",
        ::engine::emulator_account_id()
    )
}

/// The key the terminology `name`'s input file is held under, in `format`.
///
/// ONE place states the form, so the address a read announces and the object an
/// import wrote cannot disagree about it. The key carries no revision: the
/// location is documented as "the MOST RECENT custom terminology input file that
/// was successfully imported", so a re-import of one name replaces the object its
/// locations name.
#[must_use]
pub fn terminology_object_key(name: &str, format: &str) -> String {
    format!("{name}/{name}.{}", format.to_ascii_lowercase())
}

/// The key the auxiliary file of the terminology `name` is held under.
#[must_use]
pub fn auxiliary_object_key(name: &str) -> String {
    format!("{name}/{name}-auxiliary.csv")
}

/// One announced location: the object's own path in the store, presigned against
/// the base URL this emulator is reached on.
///
/// Signed through the framework's URL signer, which is what makes the address both
/// a presigned one and one this emulator answers — it signs for the emulator's
/// current base URL, so the announced host is the host the caller reached this
/// service on rather than a host only AWS serves. The object is addressed
/// path-style, which is how the emulated object store routes.
#[must_use]
pub fn presigned_object_location(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    bucket: &str,
    key: &str,
) -> String {
    let path = format!("/{bucket}/{key}");
    let request = smithy_server_runtime::SignedUrlRequest::new(
        smithy_server_runtime::http::Method::GET,
        OBJECT_STORE_SIGNING_NAME,
        &path,
    );
    match ctx.url_signer().sign(request, PRESIGNED_EXPIRY) {
        Ok(signed) => signed.as_str().to_string(),
        // Nothing about a stored name or this emulator's own endpoint makes a
        // `GET` unsignable, so this is unreachable in practice. The object IS at
        // that address either way, so the address is what is announced rather
        // than no location at all. The endpoint is `scheme://host[:port]` with no
        // path of its own, and a trailing separator is dropped so the object's
        // path is the only one the URL carries.
        Err(error) => {
            ::tracing::warn!(
                %error,
                path = %path,
                "a terminology data location could not be signed"
            );
            format!("{}{path}", ctx.endpoint().as_str().trim_end_matches('/'))
        }
    }
}

/// Where the terminology's input file is reported from, in `format`.
#[must_use]
pub fn terminology_data_location(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    record: &Terminology,
    format: &str,
) -> TerminologyDataLocation {
    TerminologyDataLocation {
        repository_type: REPOSITORY_TYPE_S3.to_string(),
        location: presigned_object_location(
            ctx,
            &terminology_bucket(region),
            &terminology_object_key(&record.name, format),
        ),
    }
}

/// The auxiliary location, which reports "any errors or warnings that were
/// produced by your input file" (`smithy_query shape=ImportTerminologyResponse
/// member=AuxiliaryDataLocation`) — so it exists only for an import that had
/// something to report, i.e. one whose file held rows the service skipped, which
/// is exactly when [`store_terminology_files`] wrote that file.
#[must_use]
pub fn auxiliary_data_location(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    record: &Terminology,
) -> Option<TerminologyDataLocation> {
    if record.skipped_term_count <= 0 {
        return None;
    }
    Some(TerminologyDataLocation {
        repository_type: REPOSITORY_TYPE_S3.to_string(),
        location: presigned_object_location(
            ctx,
            &terminology_bucket(region),
            &auxiliary_object_key(&record.name),
        ),
    })
}

/// Every format a read of `record` may announce a location in, which is what
/// [`store_terminology_files`] stores a file for.
///
/// "If you don't specify this parameter, Amazon Translate returns a file with the
/// same format as the file that was imported … If you specify this parameter when
/// you retrieve a MULTI-DIRECTIONAL terminology resource, you must specify the
/// same format as the input file that was imported to create it. Otherwise,
/// Amazon Translate throws an error" (`smithy_query
/// shape=GetTerminologyRequest member=TerminologyDataFormat`) — so a
/// multi-directional resource is only ever reported in its own format, and a
/// uni-directional one in any of the three.
#[must_use]
pub fn announced_formats(record: &Terminology) -> Vec<String> {
    if record.is_multi_directional() {
        return vec![record.format.clone()];
    }
    let mut formats = vec![record.format.clone()];
    for format in [FORMAT_CSV, FORMAT_TSV, FORMAT_TMX] {
        if format != record.format {
            formats.push(format.to_string());
        }
    }
    formats
}

/// Store the files the locations of `record` name: the imported file at the key
/// its own format's location carries, a rendering of the stored terms at each
/// other format a read may be asked for, and the auxiliary report when the import
/// had warnings to report.
///
/// Called from `ImportTerminology` alone — the one operation that creates the
/// content these locations name. A read reports a location, it never writes one.
///
/// Every key this resource may announce is settled here: a key that is no longer
/// announced (a format a now multi-directional resource is not reported in, the
/// auxiliary report of an import that skipped nothing) is withdrawn, so what the
/// store holds is exactly what the read announces.
///
/// Object storage answering nothing says nothing about this service's record —
/// the control plane is real whether or not the emulator was assembled with
/// Amazon S3 — so a failed write is logged and the import still answers; it never
/// turns a successful import into a refusal.
///
/// Call it with NO state guard held — it awaits.
pub async fn store_terminology_files(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    record: &Terminology,
    imported_file: &[u8],
    warnings: &[String],
) {
    let bucket = terminology_bucket(region);
    let client = ::service_common::s3_client::s3_client(ctx.callback(), region);
    // The files need a bucket to live in, and this one is the service's own
    // rather than anything a caller created, so the write stands it up first.
    create_terminology_bucket(&client, &bucket, region).await;

    let announced = announced_formats(record);
    for format in [FORMAT_CSV, FORMAT_TSV, FORMAT_TMX] {
        let key = terminology_object_key(&record.name, format);
        if !announced.iter().any(|announced| announced == format) {
            withdraw(&client, &bucket, &key).await;
            continue;
        }
        // The resource's OWN format reports "the custom terminology input file
        // that was successfully imported", which is the uploaded bytes
        // themselves; another format is that same content rendered into it.
        let body = if format == record.format {
            imported_file.to_vec()
        } else {
            rendered_file(record, format)
        };
        store(&client, &bucket, &key, body, content_type(format)).await;
    }

    let auxiliary = auxiliary_object_key(&record.name);
    if warnings.is_empty() {
        withdraw(&client, &bucket, &auxiliary).await;
    } else {
        store(
            &client,
            &bucket,
            &auxiliary,
            auxiliary_report(warnings),
            CONTENT_TYPE_CSV,
        )
        .await;
    }
}

/// Withdraw every file of `record`, which a delete of the resource does: the
/// resource is gone, so nothing is served where its locations pointed.
///
/// Call it with NO state guard held — it awaits.
pub async fn withdraw_terminology_files(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    record: &Terminology,
) {
    let bucket = terminology_bucket(region);
    let client = ::service_common::s3_client::s3_client(ctx.callback(), region);
    for format in [FORMAT_CSV, FORMAT_TSV, FORMAT_TMX] {
        withdraw(
            &client,
            &bucket,
            &terminology_object_key(&record.name, format),
        )
        .await;
    }
    withdraw(&client, &bucket, &auxiliary_object_key(&record.name)).await;
}

/// The media type a file in `format` carries.
fn content_type(format: &str) -> &'static str {
    match format {
        FORMAT_TSV => CONTENT_TYPE_TSV,
        FORMAT_TMX => CONTENT_TYPE_TMX,
        _ => CONTENT_TYPE_CSV,
    }
}

/// Write one file of this store.
async fn store(
    client: &::aws_sdk_s3::Client,
    bucket: &str,
    key: &str,
    body: Vec<u8>,
    content_type: &str,
) {
    match client
        .put_object()
        .bucket(bucket)
        .key(key)
        .body(::aws_sdk_s3::primitives::ByteStream::from(body))
        .content_type(content_type)
        .send()
        .await
    {
        Ok(_) => ::tracing::info!(
            bucket = %bucket,
            key = %key,
            "the file a terminology data location names was stored"
        ),
        Err(error) => ::tracing::warn!(
            bucket = %bucket,
            key = %key,
            detail = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
            "the file a terminology data location names was not stored"
        ),
    }
}

/// Take one file of this store back. An answer saying the bucket or the object is
/// not there already means nothing is served there, which is why it is not a
/// warning.
async fn withdraw(client: &::aws_sdk_s3::Client, bucket: &str, key: &str) {
    if let Err(error) = client
        .delete_object()
        .bucket(bucket)
        .key(key)
        .send()
        .await
    {
        ::tracing::debug!(
            bucket = %bucket,
            key = %key,
            detail = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
            "no file was withdrawn for this terminology"
        );
    }
}

/// Stand up the Region's bucket, so a write into it has a bucket to live in.
///
/// Idempotent in effect: a name the one bucket namespace already holds answers
/// [`BUCKET_EXISTS_STATUS`], which is this bucket already being there.
async fn create_terminology_bucket(client: &::aws_sdk_s3::Client, bucket: &str, region: &str) {
    let configuration = (region != UNCONSTRAINED_BUCKET_REGION).then(|| {
        ::aws_sdk_s3::types::CreateBucketConfiguration::builder()
            .location_constraint(::aws_sdk_s3::types::BucketLocationConstraint::from(region))
            .build()
    });
    if let Err(error) = client
        .create_bucket()
        .bucket(bucket)
        .set_create_bucket_configuration(configuration)
        .send()
        .await
    {
        use ::aws_smithy_types::error::metadata::ProvideErrorMetadata;

        let status = error
            .raw_response()
            .map(|response| response.status().as_u16());
        if status != Some(BUCKET_EXISTS_STATUS) {
            ::tracing::warn!(
                bucket = %bucket,
                code = error.code().unwrap_or_default(),
                detail = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                "the bucket this service keeps its terminology files in was not created"
            );
        }
    }
}

/// The stored terminology rendered as a file in `format`: the content a read asked
/// for in a format other than the imported one is served from.
///
/// Every value comes from the stored record — its languages, in the order the
/// imported file declared them, and its terms — so the rendering carries the same
/// terminology the import stored and nothing else.
#[must_use]
pub fn rendered_file(record: &Terminology, format: &str) -> Vec<u8> {
    match format {
        FORMAT_TSV => rendered_delimited(record, b'\t'),
        FORMAT_TMX => rendered_tmx(record),
        _ => rendered_delimited(record, b','),
    }
}

/// The record as a delimited file: the first row names the languages of the
/// columns, every later row is one term, which is the layout the import reads back
/// (`crate::ops::import_terminology`). The quoting is the `csv` crate's, so a term
/// holding the delimiter, a quote or a newline survives the round trip.
fn rendered_delimited(record: &Terminology, delimiter: u8) -> Vec<u8> {
    let languages: Vec<&str> = record.languages().collect();
    let mut writer = ::csv::WriterBuilder::new()
        .delimiter(delimiter)
        .from_writer(Vec::new());
    let mut rows = vec![languages.clone()];
    for entry in &record.terms {
        rows.push(
            languages
                .iter()
                .map(|language| entry.text(language).unwrap_or_default())
                .collect(),
        );
    }
    for row in rows {
        if let Err(error) = writer.write_record(&row) {
            ::tracing::warn!(%error, "a terminology row could not be rendered");
        }
    }
    writer.into_inner().unwrap_or_default()
}

/// The record as a TMX document: `<header srclang=…>` names the source language
/// and each `<tu>` holds one `<tuv xml:lang=…><seg>` per language the term states,
/// which is the layout the import reads back
/// (`crate::ops::import_terminology::parse_tmx`). The escaping is `quick-xml`'s,
/// so a term holding `<`, `&` or a quote survives the round trip.
fn rendered_tmx(record: &Terminology) -> Vec<u8> {
    let mut document = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<tmx version=\"1.4\">\n");
    document.push_str(&format!(
        "  <header srclang=\"{}\" adminlang=\"{}\" datatype=\"plaintext\" \
         segtype=\"sentence\" creationtool=\"{TMX_CREATION_TOOL}\" \
         creationtoolversion=\"{TMX_CREATION_TOOL_VERSION}\" o-tmf=\"{TMX_CREATION_TOOL}\"/>\n",
        ::quick_xml::escape::escape(&record.source_language_code),
        ::quick_xml::escape::escape(&record.source_language_code),
    ));
    document.push_str("  <body>\n");
    for entry in &record.terms {
        document.push_str("    <tu>\n");
        for language in record.languages() {
            if let Some(text) = entry.text(language) {
                document.push_str(&format!(
                    "      <tuv xml:lang=\"{}\"><seg>{}</seg></tuv>\n",
                    ::quick_xml::escape::escape(language),
                    ::quick_xml::escape::escape(text),
                ));
            }
        }
        document.push_str("    </tu>\n");
    }
    document.push_str("  </body>\n</tmx>\n");
    document.into_bytes()
}

/// What a TMX document this emulator renders names as the tool that produced it,
/// which the TMX header declares.
const TMX_CREATION_TOOL: &str = "Amazon Translate";

/// That tool's version, as the same header declares it.
const TMX_CREATION_TOOL_VERSION: &str = "1.0";

/// The auxiliary file: one row per warning the import produced, under a header row
/// naming the columns.
///
/// Amazon Translate publishes no layout for this file, so it is reported the way
/// the input file itself is laid out — a delimited file whose first row names the
/// columns — with the row of the input file each warning is about and what the
/// service did with it.
fn auxiliary_report(warnings: &[String]) -> Vec<u8> {
    let mut writer = ::csv::Writer::from_writer(Vec::new());
    let mut rows: Vec<Vec<String>> = vec![vec![
        AUXILIARY_ROW_COLUMN.to_string(),
        AUXILIARY_WARNING_COLUMN.to_string(),
    ]];
    for (index, warning) in warnings.iter().enumerate() {
        rows.push(vec![(index + 1).to_string(), warning.clone()]);
    }
    for row in rows {
        if let Err(error) = writer.write_record(&row) {
            ::tracing::warn!(%error, "an auxiliary warning could not be rendered");
        }
    }
    writer.into_inner().unwrap_or_default()
}

/// The column naming which warning of the import a row is.
const AUXILIARY_ROW_COLUMN: &str = "Number";

/// The column holding the warning itself.
const AUXILIARY_WARNING_COLUMN: &str = "Warning";
