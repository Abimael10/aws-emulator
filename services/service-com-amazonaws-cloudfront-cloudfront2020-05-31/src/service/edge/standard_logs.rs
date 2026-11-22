// SPDX-License-Identifier: CC0-1.0

//! STANDARD LOGGING (the distribution's own `Logging` configuration): the access-log FILE one
//! served viewer request delivers into the Amazon S3 bucket that configuration names.
//!
//! # The rule this module implements
//!
//! `DistributionConfig$Logging` is "a complex type that controls whether access logs are
//! written for the distribution": `Enabled` switches it on, `Bucket` is "the Amazon S3 bucket
//! to store the access logs in, for example, `amzn-s3-demo-bucket.s3.amazonaws.com`", `Prefix`
//! is "an optional string that you want CloudFront to prefix to the access log filenames for
//! this distribution" and `IncludeCookies` is whether "you want CloudFront to include cookies
//! in access logs". So a distribution running under such a configuration DELIVERS: every
//! viewer request its edge serves is written into that bucket, under that prefix, as one
//! gzipped log file whose records describe the requests it carries — which is what a client
//! that pointed a distribution at a log bucket then lists and reads.
//!
//! The file name is the documented layout,
//! `<prefix>/<distribution-id>.<YYYY>-<MM>-<DD>-<HH>.<unique-id>.gz`, and the unique field is
//! the identifier the edge stamped on the request the file carries ([`file_key`]), so two
//! requests of one hour are two files rather than one overwriting the other.
//!
//! One file per served request, because a record describes a request that was ANSWERED and the
//! viewer has already been answered when this runs: AWS batches the records of a period into
//! one file and delivers it later, which no reader of the bucket can tell from several files of
//! the same hour — the records, their fields and their order are what a reader consumes.
//!
//! # What one record says
//!
//! The W3C extended log file format AWS publishes for these files: a `#Version` line, a
//! `#Fields` line naming [`STANDARD_LOG_FIELDS`] in that order, and one tab-delimited record
//! per request. Every value is read off the request the edge served, the answer it served and
//! the distribution that served it, by the one reading of a CloudFront log field this service
//! has ([`super::realtime_logs::LoggedRequest`]) — so a standard log file and a real-time log
//! record of the same request cannot disagree, and a field this request has no value for
//! carries the same placeholder on both planes. The fields describing what an IN-PROCESS viewer
//! connection has none of carry that placeholder for every request: the viewer's port and
//! autonomous system, the TLS parameters and the HTTP version of a connection whose encryption
//! this plane did not negotiate, the field-level encryption of a request no profile encrypted,
//! and the range of an answer that is not a range. The EDGE LOCATION is not one of them: this
//! emulator serves every viewer from one point of presence and names it
//! ([`super::point_of_presence`]), so that field reports the same name the viewer's own
//! `Server-Timing` metrics do.
//!
//! `cs(Cookie)` is the one field the configuration itself decides: a configuration that does
//! not include cookies states none, "in which case CloudFront doesn't include cookies in the
//! logs".
//!
//! The OBJECT is written by the one write a vended-log delivery makes into a bucket
//! ([`::service_common::vended_logs::delivered_object`]): the lines newline-delimited and
//! gzipped, under the media type a gzip object carries. Delivery is BEST EFFORT, as it is for
//! every log plane here: the viewer has already been answered, so a bucket a client has since
//! deleted, or a write the object store refuses, loses the file and is reported in tracing
//! rather than failing the request.
//!
//! What the plane may ADD to the object store is bounded whatever a caller asks for, since a
//! viewer request needs no credential: one file per served request up to
//! [`MAX_DELIVERED_FILES`], each of [`STANDARD_LOG_FIELDS`] values of at most
//! [`MAX_FIELD_VALUE_BYTES`]. Both are this emulator's own ceilings, stated where they are
//! enforced.

use ::std::time::SystemTime;

use ::smithy_server_runtime::OperationContext;

use super::realtime_logs::{self, LoggedRequest};
use crate::ops::types::LoggingConfig;

/// The fields a standard access-log file states, in the order the published layout names them
/// (the CloudFront developer guide's standard log file fields, read in that order by every
/// consumer of such a file).
///
/// The four parenthesised spellings are that layout's own names for four viewer-request values
/// ([`realtime_field`] maps each to the one reading of the field this service has).
const STANDARD_LOG_FIELDS: [&str; 33] = [
    "date",
    "time",
    "x-edge-location",
    "sc-bytes",
    "c-ip",
    "cs-method",
    "cs(Host)",
    "cs-uri-stem",
    "sc-status",
    "cs(Referer)",
    "cs(User-Agent)",
    "cs-uri-query",
    "cs(Cookie)",
    "x-edge-result-type",
    "x-edge-request-id",
    "x-host-header",
    "cs-protocol",
    "cs-bytes",
    "time-taken",
    "x-forwarded-for",
    "ssl-protocol",
    "ssl-cipher",
    "x-edge-response-result-type",
    "cs-protocol-version",
    "fle-status",
    "fle-encrypted-fields",
    "c-port",
    "time-to-first-byte",
    "x-edge-detailed-result-type",
    "sc-content-type",
    "sc-content-len",
    "sc-range-start",
    "sc-range-end",
];

/// The version of the file format the first line of every file states.
const VERSION_LINE: &str = "#Version: 1.0";

/// What the second line — the one naming the fields each record states, in order — begins with.
const FIELDS_PREFIX: &str = "#Fields: ";

/// What a standard access-log file's name ends with: the gzip stream
/// [`::service_common::vended_logs::delivered_object`] writes it as.
const FILE_SUFFIX: &str = ".gz";

/// The delimiter one record's values are separated by, and the one the `#Fields` line uses for
/// the field names — the two halves of the same framing
/// ([`realtime_logs::encoded`] is what keeps a value from carrying either).
const FIELD_DELIMITER: char = '\t';
/// See [`FIELD_DELIMITER`].
const FIELD_NAME_DELIMITER: &str = " ";

/// The most bytes of ONE value a record states.
///
/// THE EMULATOR'S OWN CEILING, not a quota cited from AWS. Three of the fields carry a value the
/// VIEWER chose the length of — its `User-Agent`, its `Referer` and its cookies — so without
/// this the bytes one served request adds to the object store would be the caller's to choose.
/// It sits far above any value a client really sends in one of those headers, and a value past
/// it is cut rather than refused ([`clamped`]): the record still describes the request.
const MAX_FIELD_VALUE_BYTES: usize = 1024;

/// How many access-log FILES this plane delivers while this emulator runs.
///
/// THE EMULATOR'S OWN CEILING too, and the bound on what this plane ACCUMULATES: a viewer
/// request needs no credential, so without it any caller that can reach a distribution's edge
/// could make the object store hold one more object per request for as long as it kept asking.
/// One file per served request up to this count, each of [`STANDARD_LOG_FIELDS`] values of at
/// most [`MAX_FIELD_VALUE_BYTES`], is therefore what the whole plane can add. It sits far above
/// the traffic a client drives through an emulated distribution while reading its logs, and a
/// request past it is SERVED — only its file is not delivered, which is said in the log.
const MAX_DELIVERED_FILES: usize = 4096;

/// How many files this plane has delivered so far, which is what [`MAX_DELIVERED_FILES`] bounds.
///
/// A process-wide count rather than a per-distribution one, so the bound holds however many
/// distributions a caller creates; it is never raised past that ceiling, so the counter itself
/// cannot run away either.
static DELIVERED_FILES: ::std::sync::atomic::AtomicUsize =
    ::std::sync::atomic::AtomicUsize::new(0);

/// WHERE a distribution's standard access logs are delivered, read off the `Logging`
/// configuration the distribution runs under.
///
/// Held beside the rest of what the edge needs once the store's lock is released
/// ([`super::ServedFor`]), so the destination a request's file goes to is the one the
/// distribution carried when the request was planned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Destination {
    /// The bucket the files are delivered into, as `LoggingConfig$Bucket`'s endpoint host
    /// names it.
    bucket: String,
    /// The Region that host addresses the bucket in.
    region: String,
    /// `LoggingConfig$Prefix`, with no trailing separator: the layout below puts one between
    /// the prefix and the file's own name.
    prefix: String,
    /// `LoggingConfig$IncludeCookies`: whether a record states the viewer's cookies.
    include_cookies: bool,
}

impl Destination {
    /// The destination `logging` names, or `None` when it enables no logging at all.
    ///
    /// A configuration whose `Enabled` is false, that names no bucket, or whose `Bucket` is a
    /// host that names no bucket of this emulator's object store delivers nothing — "if you
    /// don't want to enable logging when you create a distribution … specify `false` for
    /// `Enabled`, and specify empty `Bucket` and `Prefix` elements". The host is read by the
    /// one reading of an Amazon S3 endpoint host this crate has
    /// ([`crate::service::origin_access::s3_endpoint_bucket`]), which is also what the create
    /// path resolves the logging target with (`crate::state::standard_logging`).
    pub fn of_logging(logging: Option<&LoggingConfig>) -> Option<Self> {
        let logging = logging?;
        if logging.enabled != Some(true) {
            return None;
        }
        let host = logging.bucket.as_deref().map(str::trim).unwrap_or_default();
        let (bucket, region) = crate::service::origin_access::s3_endpoint_bucket(host)?;
        Some(Self {
            bucket,
            region,
            prefix: logging
                .prefix
                .as_deref()
                .unwrap_or_default()
                .trim_matches('/')
                .to_string(),
            include_cookies: logging.include_cookies == Some(true),
        })
    }
}

/// Deliver the standard access-log file of one served viewer request into the bucket the
/// distribution's `Logging` names.
///
/// Called from the ONE seam that serves the edge with the answer in hand, beside the other log
/// planes ([`super::access_logs`], [`super::realtime_logs`]): a record describes a request that
/// was ANSWERED, so its status, its size and the span it took are part of it and cannot be
/// known earlier. Called with this service's store RELEASED — it awaits the write into the
/// object store.
///
/// Nothing here can fail the request: the viewer has already been answered.
pub(super) async fn deliver(
    ctx: &OperationContext<'_>,
    distribution_id: &str,
    destination: &Destination,
    logged: &LoggedRequest<'_>,
) {
    // What this plane may ADD to the object store, bounded whatever a caller asks for
    // ([`MAX_DELIVERED_FILES`]): the count is raised only while it is below the ceiling, so a
    // request past it is served with no file delivered for it.
    let taken = DELIVERED_FILES.try_update(
        ::std::sync::atomic::Ordering::Relaxed,
        ::std::sync::atomic::Ordering::Relaxed,
        |delivered| (delivered < MAX_DELIVERED_FILES).then(|| delivered.saturating_add(1)),
    );
    if taken.is_err() {
        ::tracing::warn!(
            files = MAX_DELIVERED_FILES,
            "this emulator has delivered as many standard access-log files as it holds, so the \
             requests it serves from now on leave none"
        );
        return;
    }
    let key = file_key(
        &destination.prefix,
        distribution_id,
        logged.at,
        logged.request_id,
    );
    // The one write a vended-log delivery makes into a bucket
    // ([`::service_common::vended_logs::delivered_object`]): the lines newline-delimited and
    // gzipped, put with the media type a gzip object carries, best effort — which is exactly
    // what a log file delivered into a bucket is, whichever plane produced it.
    ::service_common::vended_logs::delivered_object(
        ctx,
        &destination.region,
        &destination.bucket,
        &key,
        &file_lines(logged, destination.include_cookies),
    )
    .await;
}

/// The LINES of one file, each delivered as its own line: the two header lines the format
/// states and the record of the request the file carries.
fn file_lines(logged: &LoggedRequest<'_>, include_cookies: bool) -> Vec<String> {
    vec![
        VERSION_LINE.to_string(),
        format!(
            "{FIELDS_PREFIX}{}",
            STANDARD_LOG_FIELDS.join(FIELD_NAME_DELIMITER)
        ),
        record(logged, include_cookies),
    ]
}

/// The RECORD one served request states: one value per field of [`STANDARD_LOG_FIELDS`], in
/// that order, tab-delimited and framed the way the real-time plane frames a value
/// ([`realtime_logs::encoded`]).
fn record(logged: &LoggedRequest<'_>, include_cookies: bool) -> String {
    let mut line = String::new();
    for (position, field) in STANDARD_LOG_FIELDS.iter().enumerate() {
        if position > 0 {
            line.push(FIELD_DELIMITER);
        }
        line.push_str(&realtime_logs::encoded(&value(
            logged,
            field,
            include_cookies,
        )));
    }
    line
}

/// What this served request states for ONE field of the file.
///
/// The one reading of a CloudFront log field this service has
/// ([`super::access_logs::field_value`], which also answers the two calendar halves of the
/// serving instant), asked for the field this layout's own name means
/// ([`realtime_field`]) — except for the cookies, which the LOGGING CONFIGURATION decides: a
/// configuration that does not include them states the same placeholder a request carrying none
/// does.
fn value(logged: &LoggedRequest<'_>, field: &str, include_cookies: bool) -> String {
    let asked = realtime_field(field);
    if asked == COOKIE_FIELD && !include_cookies {
        return realtime_logs::NO_VALUE.to_string();
    }
    clamped(super::access_logs::field_value(logged, asked))
}

/// `value` cut to [`MAX_FIELD_VALUE_BYTES`], at a character boundary so what is written stays
/// text.
///
/// Three of the fields (`cs(User-Agent)`, `cs(Referer)`, `cs(Cookie)`) carry a value a VIEWER
/// chose the length of, so the size of one file is bounded here rather than by what a caller
/// sends.
fn clamped(value: String) -> String {
    if value.len() <= MAX_FIELD_VALUE_BYTES {
        return value;
    }
    let end = value
        .char_indices()
        .map(|(at, _character)| at)
        .take_while(|at| *at <= MAX_FIELD_VALUE_BYTES)
        .last()
        .unwrap_or_default();
    value.get(..end).unwrap_or_default().to_string()
}

/// The field the real-time plane names the value a standard log field states.
///
/// The four values this layout spells in the `cs(Header)` form are the viewer-request headers
/// the real-time field catalogue names `cs-<header>`; every other field of the layout is named
/// the same on both planes, and the two calendar halves are answered by the standard plane
/// itself.
fn realtime_field(field: &str) -> &str {
    match field {
        "cs(Host)" => "cs-host",
        "cs(Referer)" => "cs-referer",
        "cs(User-Agent)" => "cs-user-agent",
        "cs(Cookie)" => COOKIE_FIELD,
        other => other,
    }
}

/// The real-time field naming the viewer's cookies, which is the one field the logging
/// configuration decides.
const COOKIE_FIELD: &str = "cs-cookie";

/// The key one file takes: the documented layout,
/// `<prefix>/<distribution-id>.<YYYY>-<MM>-<DD>-<HH>.<unique-id>.gz`.
///
/// The prefix is the configuration's own, with one separator between it and the file's own name
/// (a configuration naming none delivers at the bucket's root). The HOUR is the hour of the
/// instant the edge finished responding, which is the hour the records belong to. The unique
/// field is the identifier the edge stamped on the request this file carries (`X-Amz-Cf-Id`),
/// so no two files of one hour collide and the file names what it describes.
fn file_key(prefix: &str, distribution_id: &str, at: SystemTime, unique: &str) -> String {
    let within = if prefix.is_empty() {
        String::new()
    } else {
        format!("{prefix}/")
    };
    format!(
        "{within}{distribution_id}.{}.{unique}{FILE_SUFFIX}",
        hour_of(at)
    )
}

/// The hour `at` falls in, as the file name states it: `YYYY-MM-DD-HH`, in UTC.
///
/// Read from the whole-second UTC `date-time` rendering this workspace's one timestamp renderer
/// produces, so the file name and the `date`/`time` fields of its records describe the same
/// instant.
fn hour_of(at: SystemTime) -> String {
    let seconds = at
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|since| i64::try_from(since.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or_default();
    let stamp = ::aws_smithy_types::DateTime::from_secs(seconds)
        .fmt(::aws_smithy_types::date_time::Format::DateTime)
        .unwrap_or_default();
    let (date, rest) = stamp.split_once('T').unwrap_or_default();
    let hour = rest.get(..2).unwrap_or_default();
    format!("{date}-{hour}")
}

#[cfg(test)]
mod tests {
    use std::io::Read;
    use std::time::Duration;

    use super::*;

    /// An instant with a known UTC calendar rendering: 2026-11-02T10:23:43Z.
    fn at() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_793_615_023)
    }

    /// RULE: the key is the documented layout — the configured prefix, the distribution, the
    /// HOUR the records belong to and the field that tells two files of that hour apart.
    ///
    /// The rows differ along the prefix's form, which is what the layout composes with: none, a
    /// plain one, one written with separators around it, and a nested one.
    #[test]
    fn a_file_key_carries_the_documented_layout() {
        let rows = [
            ("no prefix at all", "", "EDFDVBD6EXAMPLE.2026-11-02-10.CFID1.gz"),
            (
                "a one-segment prefix",
                "cf-logs",
                "cf-logs/EDFDVBD6EXAMPLE.2026-11-02-10.CFID1.gz",
            ),
            (
                "a prefix written with separators around it",
                "/cf-logs/",
                "cf-logs/EDFDVBD6EXAMPLE.2026-11-02-10.CFID1.gz",
            ),
            (
                "a nested prefix",
                "logs/edge",
                "logs/edge/EDFDVBD6EXAMPLE.2026-11-02-10.CFID1.gz",
            ),
        ];
        for (case, prefix, expected) in rows {
            let prefix = prefix.trim_matches('/');
            let actual = file_key(prefix, "EDFDVBD6EXAMPLE", at(), "CFID1");
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// RULE: a standard log field is the layout's own name for a value this service reads once,
    /// so each `cs(Header)` spelling asks for that header's own field and every other name asks
    /// for itself.
    #[test]
    fn each_layout_field_names_one_reading_of_the_value() {
        let rows = [
            ("the host header spelling", "cs(Host)", "cs-host"),
            ("the referer spelling", "cs(Referer)", "cs-referer"),
            ("the user agent spelling", "cs(User-Agent)", "cs-user-agent"),
            ("the cookie spelling", "cs(Cookie)", "cs-cookie"),
            ("a field named alike on both planes", "sc-status", "sc-status"),
            ("the two calendar halves", "date", "date"),
        ];
        for (case, field, expected) in rows {
            let actual = realtime_field(field);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// One row of [`only_an_enabled_configuration_naming_a_bucket_endpoint_names_a_destination`]:
    /// the case, the `Logging` element it is read from, and the destination it names — the
    /// bucket, the prefix and whether cookies are included — for one that names any.
    type DestinationRow = (
        &'static str,
        Option<LoggingConfig>,
        Option<(&'static str, &'static str, bool)>,
    );

    /// RULE: the configuration decides what a `Logging` element names — and one that enables no
    /// logging, names no bucket or names a host that is no bucket endpoint names no destination
    /// at all.
    #[test]
    fn only_an_enabled_configuration_naming_a_bucket_endpoint_names_a_destination() {
        let logging = |enabled: Option<bool>, bucket: &str, prefix: &str, cookies: bool| {
            Some(LoggingConfig {
                enabled,
                include_cookies: Some(cookies),
                bucket: Some(bucket.to_string()),
                prefix: Some(prefix.to_string()),
            })
        };
        let rows: Vec<DestinationRow> = vec![
            ("no Logging element at all", None, None),
            (
                "logging switched off against a named bucket",
                logging(Some(false), "edge-logs.s3.amazonaws.com", "cf/", false),
                None,
            ),
            (
                "logging on with no bucket",
                logging(Some(true), "", "cf/", false),
                None,
            ),
            (
                "logging on against a host that names no bucket",
                logging(Some(true), "not-an-endpoint", "cf/", false),
                None,
            ),
            (
                "logging on against a bucket endpoint",
                logging(Some(true), "edge-logs.s3.amazonaws.com", "cf/", false),
                Some(("edge-logs", "cf", false)),
            ),
            (
                "logging on with cookies included and a regional host",
                logging(
                    Some(true),
                    "edge-logs.s3.eu-west-1.amazonaws.com",
                    "",
                    true,
                ),
                Some(("edge-logs", "", true)),
            ),
        ];
        for (case, configured, expected) in rows {
            let destination = Destination::of_logging(configured.as_ref());
            let actual = destination
                .as_ref()
                .map(|to| (to.bucket.as_str(), to.prefix.as_str(), to.include_cookies));
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// RULE: a value a VIEWER chose the length of is cut to the ceiling one record states, at a
    /// character boundary, and a value within it is stated as it stands.
    ///
    /// The rows differ along the length and the encoding: well inside the bound, exactly at it,
    /// past it in ASCII, and past it in characters whose bytes do not divide where the bound
    /// does.
    #[test]
    fn a_record_states_at_most_the_bounded_bytes_of_one_value() {
        let rows = [
            ("a short value", "stock-client/1.0".to_string()),
            ("a value exactly at the bound", "u".repeat(MAX_FIELD_VALUE_BYTES)),
            ("a value past the bound", "u".repeat(MAX_FIELD_VALUE_BYTES * 3)),
            (
                "a multi-byte value past the bound",
                "\u{e9}".repeat(MAX_FIELD_VALUE_BYTES),
            ),
        ];
        for (case, value) in rows {
            let cut = clamped(value.clone());
            assert!(
                cut.len() <= MAX_FIELD_VALUE_BYTES,
                "case {case:?}: expected at most {MAX_FIELD_VALUE_BYTES} bytes, got {}",
                cut.len()
            );
            assert!(
                value.starts_with(&cut),
                "case {case:?}: expected a prefix of the value, got {cut:?}"
            );
            if value.len() <= MAX_FIELD_VALUE_BYTES {
                assert_eq!(
                    cut, value,
                    "case {case:?}: a value within the bound is stated as it stands"
                );
            }
        }
    }

    /// RULE: the object a file is, is a gzip stream whose lines are the two header lines the
    /// format states and one record whose values stand under the fields the `#Fields` line
    /// names.
    #[test]
    fn a_file_is_gzipped_w3c_lines_under_its_own_fields() {
        let req = ::aws_smithy_runtime_api::http::Request::try_from(
            ::http::Request::builder()
                .method("GET")
                .uri("https://d111111abcdef8.cloudfront.net/index.html")
                .header("host", "d111111abcdef8.cloudfront.net")
                .header("user-agent", "stock-client/1.0")
                .header("cookie", "session=opaque")
                .body(::aws_smithy_types::body::SdkBody::empty())
                .expect("a well-formed viewer request"),
        )
        .expect("a runtime-api request");
        let logged = LoggedRequest {
            req: &req,
            at: at(),
            taken: Duration::from_millis(12),
            origin_latency: Some(Duration::from_millis(8)),
            status: 200,
            response_bytes: 31,
            content_type: Some("text/html"),
            distribution_domain: "d111111abcdef8.cloudfront.net",
            viewer_host: "d111111abcdef8.cloudfront.net",
            viewer_path: "/index.html",
            query: "",
            country: "US",
            request_id: "CFID1",
            path_pattern: None,
            primary: None,
            result_type: realtime_logs::ResultType::Miss,
        };
        for (case, include_cookies, cookie) in [
            ("cookies included", true, "session=opaque"),
            ("cookies left out", false, realtime_logs::NO_VALUE),
        ] {
            let body = ::service_common::vended_logs::gzipped(&file_lines(
                &logged,
                include_cookies,
            ))
            .unwrap_or_else(|| panic!("case {case:?}: the file must have a body"));
            let mut decoded = String::new();
            ::flate2::read::GzDecoder::new(body.as_slice())
                .read_to_string(&mut decoded)
                .unwrap_or_else(|error| panic!("case {case:?}: not a gzip stream: {error}"));
            let mut lines = decoded.lines();
            assert_eq!(
                lines.next(),
                Some(VERSION_LINE),
                "case {case:?}: expected the version line first, got {decoded:?}"
            );
            let fields: Vec<&str> = lines
                .next()
                .unwrap_or_default()
                .trim_start_matches(FIELDS_PREFIX)
                .split(FIELD_NAME_DELIMITER)
                .collect();
            assert_eq!(
                fields,
                STANDARD_LOG_FIELDS.to_vec(),
                "case {case:?}: expected the published field list, got {fields:?}"
            );
            let values: Vec<&str> = lines
                .next()
                .unwrap_or_default()
                .split(FIELD_DELIMITER)
                .collect();
            assert_eq!(
                values.len(),
                fields.len(),
                "case {case:?}: expected one value per field, got {values:?}"
            );
            let read = |name: &str| {
                fields
                    .iter()
                    .position(|field| *field == name)
                    .and_then(|at| values.get(at).copied())
                    .unwrap_or_default()
            };
            for (field, expected) in [
                ("date", "2026-11-02"),
                ("time", "10:23:43"),
                ("sc-status", "200"),
                ("cs-method", "GET"),
                ("cs-uri-stem", "/index.html"),
                ("cs(Host)", "d111111abcdef8.cloudfront.net"),
                ("cs(User-Agent)", "stock-client/1.0"),
                ("x-edge-request-id", "CFID1"),
                ("x-edge-result-type", "Miss"),
                ("sc-bytes", "31"),
                ("cs(Cookie)", cookie),
            ] {
                let actual = read(field);
                assert_eq!(
                    actual, expected,
                    "case {case:?}: expected {field} {expected:?}, got {actual:?}"
                );
            }
        }
    }
}
