// SPDX-License-Identifier: CC0-1.0

//! STANDARD LOGGING to Amazon S3: the access-log FILES a distribution running under a
//! `Logging` configuration delivers into the bucket that configuration names, read back
//! through Amazon S3 itself with the object store mounted beside CloudFront on one engine.
//!
//! RULE: a distribution whose `Logging` is `Enabled` against a bucket delivers, for EVERY
//! viewer request its edge serves, one gzipped W3C-extended-format access-log file into that
//! bucket under that `Prefix` — the `#Version` line, the `#Fields` line naming the published
//! field list and one tab-delimited record whose values are that request's own (its method, the
//! path it asked for, the status the edge answered) — and `IncludeCookies` decides whether the
//! record states the viewer's cookies at all. A distribution that enables no logging delivers
//! nothing.
//!
//! Only a suite that mounts the object store can state this: the sibling
//! `standard_logging_bucket` suite pins what a CREATE makes of the log bucket it names (the
//! Object Ownership read and the log-delivery grant), and `edge_standard_logging_v2` pins the
//! OTHER access-log plane, the vended-log deliveries configured in CloudWatch Logs. Neither
//! says that a served viewer request leaves a file in the bucket a client pointed the
//! distribution at, which is what a client that then lists that bucket reads.
//!
//! The rows differ along what the rule generalises over: the `Prefix`'s form (none, one
//! segment written with a trailing separator, a nested one written without), `IncludeCookies`
//! both ways, both spellings of the bucket's endpoint host, and whether the configuration
//! enables logging at all. Each row drives TWO viewer requests whose answers differ (an object
//! the origin holds, and a key it does not), so what is asserted of the delivered files is that
//! each served request produced one and that the records carry that request's own values.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::time::Duration;

use crate::distribution_lifecycle::{awaited_deployment, call, API_VERSION};
use crate::edge_signed_access::{view, CACHING_DISABLED};
use crate::standard_logging_bucket::{Emulator, Ownership, REGION};

/// The bucket the origin object lives in, and the object the viewer asks for.
const ORIGIN_BUCKET: &str = "edge-logging-origin";
/// See [`ORIGIN_BUCKET`].
const OBJECT_KEY: &str = "index.html";
/// See [`ORIGIN_BUCKET`].
const OBJECT_BODY: &str = "<html>the logged object</html>";

/// A key the origin bucket does NOT hold, so the second request of each row is answered an
/// error — a request the edge served, and therefore logged, like any other.
const ABSENT_KEY: &str = "no-such-page.html";

/// The cookie every viewer request carries, which `IncludeCookies` decides the record states.
const VIEWER_COOKIE: &str = "session=opaque";

/// The placeholder a record carries for a field this request has no value for — and the one a
/// configuration that includes no cookies states for the cookie field.
const NO_VALUE: &str = "-";

/// The two header lines of the file format and the delimiters its two halves use.
const VERSION_LINE: &str = "#Version: 1.0";
const FIELDS_PREFIX: &str = "#Fields: ";
const FIELD_DELIMITER: char = '\t';
const FIELD_NAME_DELIMITER: char = ' ';

/// How long the suite waits for the files of the served requests to appear in the bucket, and
/// how often it looks. The wait is on the CONDITION — the objects the bucket holds — with a
/// deadline far past the work one delivery is, so this bound only decides when a delivery that
/// never happens is reported as a failure.
const POLL_EVERY: Duration = Duration::from_millis(25);
const DELIVERED_WITHIN: Duration = Duration::from_secs(30);

/// One row: a `Logging` configuration, and what the bucket it names must hold afterwards.
struct Row {
    case: &'static str,
    /// The bucket the files are delivered into.
    bucket: &'static str,
    /// The Region the configured endpoint host addresses, or `None` for the global form
    /// (`<bucket>.s3.amazonaws.com`), which addresses none.
    host_region: Option<&'static str>,
    /// `LoggingConfig$Enabled`.
    enabled: bool,
    /// `LoggingConfig$IncludeCookies`.
    include_cookies: bool,
    /// `LoggingConfig$Prefix`, as the configuration spells it.
    prefix: &'static str,
    /// The key prefix the delivered files sit under, which is that value without its
    /// separators.
    within: &'static str,
}

impl Row {
    /// `LoggingConfig$Bucket`: "the Amazon S3 bucket to store the access logs in, for example,
    /// `amzn-s3-demo-bucket.s3.amazonaws.com`" — the bucket's endpoint host.
    fn logging_host(&self) -> String {
        match self.host_region {
            Some(region) => format!("{}.s3.{region}.amazonaws.com", self.bucket),
            None => format!("{}.s3.amazonaws.com", self.bucket),
        }
    }

    /// The key prefix this row's files sit under, separator included; the empty string for a
    /// configuration naming no prefix.
    fn within_prefix(&self) -> String {
        if self.within.is_empty() {
            String::new()
        } else {
            format!("{}/", self.within)
        }
    }

    /// The `DistributionConfig` of this row: the origin bucket as an Amazon S3 origin, a
    /// behaviour that caches nothing (so every viewer request reaches the origin), and this
    /// row's own `Logging` element, in the order `DistributionConfig` declares its members.
    fn config(&self, reference: &str) -> String {
        format!(
            "<DistributionConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
             <CallerReference>{reference}</CallerReference>\
             <Origins><Quantity>1</Quantity><Items><Origin><Id>origin-bucket</Id>\
             <DomainName>{ORIGIN_BUCKET}.s3.{REGION}.amazonaws.com</DomainName>\
             <S3OriginConfig><OriginAccessIdentity></OriginAccessIdentity></S3OriginConfig>\
             </Origin></Items></Origins>\
             <DefaultCacheBehavior><TargetOriginId>origin-bucket</TargetOriginId>\
             <ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy>\
             <CachePolicyId>{CACHING_DISABLED}</CachePolicyId></DefaultCacheBehavior>\
             <Comment>standard logging delivery</Comment>\
             <Logging><Enabled>{}</Enabled><IncludeCookies>{}</IncludeCookies>\
             <Bucket>{}</Bucket><Prefix>{}</Prefix></Logging>\
             <Enabled>true</Enabled></DistributionConfig>",
            self.enabled,
            self.include_cookies,
            self.logging_host(),
            self.prefix
        )
    }
}

/// The rows.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "logging on, no prefix, cookies left out",
            bucket: "edge-logs-root",
            host_region: None,
            enabled: true,
            include_cookies: false,
            prefix: "",
            within: "",
        },
        Row {
            case: "logging on under one prefix segment, cookies included",
            bucket: "edge-logs-prefixed",
            host_region: None,
            enabled: true,
            include_cookies: true,
            prefix: "cf-logs/",
            within: "cf-logs",
        },
        Row {
            case: "logging on under a nested prefix written without a separator, regional host",
            bucket: "edge-logs-nested",
            host_region: Some(REGION),
            enabled: true,
            include_cookies: false,
            prefix: "logs/edge",
            within: "logs/edge",
        },
        Row {
            case: "logging switched off against a bucket that could receive them",
            bucket: "edge-logs-disabled",
            host_region: None,
            enabled: false,
            include_cookies: true,
            prefix: "cf-logs/",
            within: "",
        },
    ]
}

/// One delivered log file, taken apart: the key it was delivered under, and the record its one
/// line states, read by field name.
struct Delivered {
    key: String,
    record: BTreeMap<String, String>,
}

/// `CreateDistribution` with this row's configuration, answering `(Id, DomainName)`.
async fn created(emulator: &Emulator, row: &Row, reference: &str) -> (String, String) {
    let case = row.case;
    let answered = call(
        &emulator.router,
        "POST",
        &format!("/{API_VERSION}/distribution"),
        Some(&row.config(reference)),
        None,
    )
    .await;
    assert_eq!(
        answered.status, 201,
        "case {case:?}: CreateDistribution answered {}: {}",
        answered.status, answered.raw
    );
    let read = |member: &str| {
        answered
            .root()
            .find_text(member)
            .unwrap_or_else(|| panic!("case {case:?}: the created distribution reports no {member}"))
            .to_string()
    };
    (read("Id"), read("DomainName"))
}

/// The keys this row's log bucket holds.
async fn listed(emulator: &Emulator, row: &Row) -> Vec<String> {
    let listed = emulator
        .s3()
        .list_objects_v2()
        .bucket(row.bucket)
        .send()
        .await
        .unwrap_or_else(|error| {
            panic!("case {:?}: listing {} must succeed: {error:?}", row.case, row.bucket)
        });
    listed
        .contents()
        .iter()
        .filter_map(|object| object.key())
        .map(str::to_string)
        .collect()
}

/// Every file this row's bucket holds, taken apart — once it holds at least `expected` of them,
/// or as many as it ever held within [`DELIVERED_WITHIN`].
///
/// The wait is on the CONDITION the bucket reports, never on a span of the clock: a delivery
/// that has happened is one the listing shows.
async fn delivered(emulator: &Emulator, row: &Row, expected: usize) -> Vec<Delivered> {
    let polls = DELIVERED_WITHIN.as_millis() / POLL_EVERY.as_millis().max(1);
    let mut keys: Vec<String> = Vec::new();
    for _ in 0..polls {
        keys = listed(emulator, row).await;
        if keys.len() >= expected {
            break;
        }
        ::tokio::time::sleep(POLL_EVERY).await;
    }
    let mut files = Vec::new();
    for key in keys {
        files.push(Delivered {
            record: record_of(emulator, row, &key).await,
            key,
        });
    }
    files
}

/// The ONE record of the file at `key`, read by field name: the object is gunzipped, its header
/// lines are held to being the format's own, and the record's values are paired with the names
/// its own `#Fields` line states.
async fn record_of(emulator: &Emulator, row: &Row, key: &str) -> BTreeMap<String, String> {
    let case = row.case;
    let object = emulator
        .s3()
        .get_object()
        .bucket(row.bucket)
        .key(key)
        .send()
        .await
        .unwrap_or_else(|error| {
            panic!("case {case:?}: reading {}/{key} must succeed: {error:?}", row.bucket)
        });
    let bytes = object
        .body
        .collect()
        .await
        .unwrap_or_else(|error| panic!("case {case:?}: reading {key} must stream: {error:?}"))
        .into_bytes();
    let mut text = String::new();
    ::flate2::read::GzDecoder::new(bytes.as_ref())
        .read_to_string(&mut text)
        .unwrap_or_else(|error| {
            panic!("case {case:?}: {key} must be a gzip stream, got {error}")
        });
    assert!(
        key.starts_with(&row.within_prefix()),
        "case {case:?}: {key} must be delivered under {:?}",
        row.within
    );
    let mut lines = text.lines();
    assert_eq!(
        lines.next(),
        Some(VERSION_LINE),
        "case {case:?}: {key} must state the format version first, got {text:?}"
    );
    let fields: Vec<&str> = lines
        .next()
        .unwrap_or_default()
        .trim_start_matches(FIELDS_PREFIX)
        .split(FIELD_NAME_DELIMITER)
        .collect();
    for named in ["date", "time", "cs-method", "cs-uri-stem", "sc-status", "cs(Cookie)"] {
        assert!(
            fields.contains(&named),
            "case {case:?}: {key} must name the published field {named:?}, got {fields:?}"
        );
    }
    let values: Vec<&str> = lines
        .next()
        .unwrap_or_default()
        .split(FIELD_DELIMITER)
        .collect();
    assert_eq!(
        values.len(),
        fields.len(),
        "case {case:?}: {key} must state one value per field, got {values:?} for {fields:?}"
    );
    fields
        .into_iter()
        .zip(values)
        .map(|(field, value)| (field.to_string(), value.to_string()))
        .collect()
}

/// Stand up the origin: one object, readable by everyone, so each row's viewer request is
/// served by the origin itself and the LOG is what the row is about.
async fn staged_origin(emulator: &Emulator) {
    emulator
        .bucket(ORIGIN_BUCKET, Ownership::BucketOwnerPreferred)
        .await;
    emulator
        .s3()
        .put_object()
        .bucket(ORIGIN_BUCKET)
        .key(OBJECT_KEY)
        .content_type("text/html")
        .body(::aws_sdk_s3::primitives::ByteStream::from(
            OBJECT_BODY.as_bytes().to_vec(),
        ))
        .send()
        .await
        .unwrap_or_else(|error| panic!("putting the origin object must succeed: {error:?}"));
    emulator
        .s3()
        .put_public_access_block()
        .bucket(ORIGIN_BUCKET)
        .public_access_block_configuration(
            ::aws_sdk_s3::types::PublicAccessBlockConfiguration::builder()
                .block_public_acls(false)
                .ignore_public_acls(false)
                .block_public_policy(false)
                .restrict_public_buckets(false)
                .build(),
        )
        .send()
        .await
        .unwrap_or_else(|error| {
            panic!("the origin's public access settings must be stored: {error:?}")
        });
    emulator
        .s3()
        .put_bucket_policy()
        .bucket(ORIGIN_BUCKET)
        .policy(format!(
            "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"Allow\",\
             \"Principal\":\"*\",\"Action\":\"s3:GetObject\",\
             \"Resource\":\"arn:aws:s3:::{ORIGIN_BUCKET}/*\"}}]}}"
        ))
        .send()
        .await
        .unwrap_or_else(|error| panic!("the origin's policy must be stored: {error:?}"));
}

/// The whole rule, over the table.
#[tokio::test]
async fn a_distribution_logging_to_a_bucket_delivers_every_served_request_into_it() {
    let emulator = Emulator::new();
    staged_origin(&emulator).await;

    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        // The log bucket, with ACLs enabled: a configuration naming a bucket whose ACLs are
        // disabled is refused at the create, which the sibling suite pins.
        emulator
            .bucket(row.bucket, Ownership::BucketOwnerPreferred)
            .await;
        let (id, domain) = created(&emulator, &row, &format!("standard-logging-{index}")).await;
        awaited_deployment(&emulator.router, case, &id).await;

        // One request the origin answers with the object, and one it answers with its own
        // error: both are requests the edge SERVED, so both are logged.
        let served = view(
            &emulator.router,
            &domain,
            &format!("/{OBJECT_KEY}"),
            &[("cookie", VIEWER_COOKIE)],
        )
        .await;
        assert_eq!(
            served.status, 200,
            "case {case:?}: the viewer must be served the object, got {} with {:?}",
            served.status, served.body
        );
        let absent = view(
            &emulator.router,
            &domain,
            &format!("/{ABSENT_KEY}"),
            &[("cookie", VIEWER_COOKIE)],
        )
        .await;
        assert_eq!(
            absent.status, 404,
            "case {case:?}: a key the origin does not hold must be answered 404, got {} with \
             {:?}",
            absent.status, absent.body
        );

        let expected = if row.enabled { 2 } else { 0 };
        let files = delivered(&emulator, &row, expected).await;
        assert_eq!(
            files.len(),
            expected,
            "case {case:?}: expected {expected} delivered log file(s), got {:?}",
            files.iter().map(|file| &file.key).collect::<Vec<_>>()
        );
        for file in &files {
            // The name is the published layout: the distribution, the hour the records belong
            // to, and a field that tells two files of that hour apart.
            let name = file.key.rsplit('/').next().unwrap_or_default().to_string();
            assert!(
                name.starts_with(&format!("{id}.")) && name.ends_with(".gz"),
                "case {case:?}: expected a file named for the distribution, got {name:?}"
            );
            let hour = name.split('.').nth(1).unwrap_or_default().to_string();
            let halves: Vec<&str> = hour.split('-').collect();
            assert!(
                halves.len() == 4
                    && halves.iter().all(|half| {
                        !half.is_empty() && half.chars().all(|digit| digit.is_ascii_digit())
                    }),
                "case {case:?}: expected the hour the records belong to in the name, got \
                 {hour:?} in {name:?}"
            );
            let read = |field: &str| file.record.get(field).cloned().unwrap_or_default();
            assert_eq!(
                read("cs-method"),
                "GET",
                "case {case:?}: expected the request's own method in {}, got {:?}",
                file.key,
                file.record
            );
            let expected_cookie = if row.include_cookies {
                VIEWER_COOKIE.to_string()
            } else {
                NO_VALUE.to_string()
            };
            let cookie = read("cs(Cookie)");
            assert_eq!(
                cookie, expected_cookie,
                "case {case:?}: expected the cookie field {expected_cookie:?} in {}, got \
                 {cookie:?}",
                file.key
            );
        }
        // The two served requests are the two records, each carrying its own path and status.
        let answered: BTreeSet<(String, String)> = files
            .iter()
            .map(|file| {
                (
                    file.record.get("cs-uri-stem").cloned().unwrap_or_default(),
                    file.record.get("sc-status").cloned().unwrap_or_default(),
                )
            })
            .collect();
        let served_requests: BTreeSet<(String, String)> = if row.enabled {
            [
                (format!("/{OBJECT_KEY}"), "200".to_string()),
                (format!("/{ABSENT_KEY}"), "404".to_string()),
            ]
            .into_iter()
            .collect()
        } else {
            BTreeSet::new()
        };
        assert_eq!(
            answered, served_requests,
            "case {case:?}: expected one record per served request, got {answered:?}"
        );
    }
}
