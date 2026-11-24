// SPDX-License-Identifier: CC0-1.0

//! What TWO trails of one account deliver for the same S3 OBJECT calls on the emulator a
//! customer STARTS — the binary, on its own port, with request authentication on, reached over
//! TLS with SigV4-signed calls — when one of them EXCLUDES part of its own ARN scope.
//!
//! RULE: a logging trail delivers one log file per call on the CONTENTS of an object that one of
//! its advanced event selectors admits, under the documented key layout beneath the trail's own
//! `S3KeyPrefix` (`<key-prefix>/AWSLogs/<account>/CloudTrail/<region>/YYYY/MM/DD/`, each file
//! named `<account>_CloudTrail_<region>_<minute>_<unique>.json.gz`); and a NEGATIVE operator
//! stated on the same field selector as the positive one it narrows EXCLUDES what it matches
//! from what that positive admits, so the calls on the excluded objects reach that trail's
//! prefix never, while the trail carrying only the positive operator receives all of them.
//!
//! Two trails of one account, delivering into ONE bucket under their own key prefixes, are the
//! instrument: what arrived for one selector is then never read as the other's, and the control
//! prefix is the positive evidence an absence under the excluding prefix is judged against.
//! Each trail's own delivery area is what the bucket's policy grants CloudTrail — the policy
//! CloudTrail's own documentation states, written once per key prefix, which is what a client
//! running two trails in one bucket attaches.
//!
//! What a delivered FILE is (a gzip stream whose JSON holds its records under `Records`) and
//! what each RECORD carries (the category, the request's bucket and key, the one
//! `AWS::S3::Object` resource it names) are pinned where the gzip reader lives: the CloudTrail
//! crate's own `tests/all/trail_log_file_delivery_of_s3_object_data_events.rs`.
//! `core/router/Cargo.toml` is a pinned path in this repository, so this test crate can take no
//! decompressor; here a delivered file is asserted by its key layout and by WHICH trail's prefix
//! it arrived under, which is what the selector decides.
//!
//! The started binary is driven through the harness this test crate publishes for exactly that
//! ([`crate::cfn_deployed_pipeline_on_a_started_emulator::Started`]), so how the emulator is
//! started, trusted and signed for is stated once for every suite that starts it.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::cfn_deployed_pipeline_on_a_started_emulator::{Started, REGION};
use crate::query_wire::elements;

/// The account every generated emulator presents (`engine::EMULATOR_ACCOUNT_ID`), which owns the
/// buckets and the trails and names every delivered log file's prefix.
const ACCOUNT: &str = "000000000000";

/// The bucket both trails deliver into, and the bucket whose objects the calls act on.
const LOG_BUCKET: &str = "s3-arn-exclusion-trail-logs";
const DATA_BUCKET: &str = "s3-arn-exclusion-subject";

/// The key prefix each trail delivers under: the one whose selector carries the exclusion, and
/// the one whose selector carries only the positive narrowing.
const EXCLUDING_PREFIX: &str = "excl";
const CONTROL_PREFIX: &str = "ctrl";

/// The two trails themselves.
const EXCLUDING_TRAIL: &str = "s3-arn-exclusion-excluding";
const CONTROL_TRAIL: &str = "s3-arn-exclusion-control";

/// CloudTrail's `X-Amz-Target` prefix and the `awsJson1_1` content type it speaks.
const CLOUDTRAIL_TARGET_PREFIX: &str = "CloudTrail_20131101.";
const JSON_CONTENT_TYPE: &str = "application/x-amz-json-1.1";

/// The `eventCategory` an advanced event selector pins a data event by, and the type an object is
/// named by in a record's `resources` and in a selector's `resources.type` field.
const DATA_CATEGORY: &str = "Data";
const OBJECT_RESOURCE_TYPE: &str = "AWS::S3::Object";

/// The key prefix of the data bucket the excluding trail's selector leaves out, and the one its
/// positive operand admits.
const EXCLUDED_KEY_PREFIX: &str = "excluded/";
const INCLUDED_KEY_PREFIX: &str = "included/";

/// The segments of a CloudTrail log-file key, and what such a file's name ends with.
const LOG_FILE_ROOT_SEGMENT: &str = "AWSLogs";
const LOG_FILE_SERVICE_SEGMENT: &str = "CloudTrail";
const LOG_FILE_SUFFIX: &str = ".json.gz";

/// How long a delivery is waited for, and how often it is looked for. The wait is on a CONDITION
/// and the span is generous, so a loaded host spends none of it; what it bounds is a delivery
/// that never comes.
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);
const LOOK_INTERVAL: Duration = Duration::from_millis(200);

/// How many times the excluding prefix is looked at again before its CONTENT is believed. The
/// delivery is made while the call that caused it is being served — the control leg finds its
/// files on the first look — so a prefix that still holds only the admitted files after this many
/// looks holds them because the exclusion withheld the rest.
const ABSENCE_LOOKS: usize = 10;

/// One CloudTrail call that has to be served.
fn cloudtrail(started: &Started, operation: &str, body: &Value) -> Value {
    let (status, text) = started.rpc(
        "cloudtrail",
        &format!("{CLOUDTRAIL_TARGET_PREFIX}{operation}"),
        JSON_CONTENT_TYPE,
        body,
    );
    assert_eq!(
        status,
        200,
        "CloudTrail {operation} must answer 200, got {status} with {text}; the log:\n{}",
        started.log()
    );
    serde_json::from_str(&text).unwrap_or(Value::Null)
}

/// Create `named`.
fn create_bucket(started: &Started, named: &str) {
    let (status, text) = started.call(
        "s3",
        "PUT",
        &format!("/{named}"),
        &[("content-type", "application/xml")],
        Vec::new(),
    );
    assert_eq!(
        status, 200,
        "CreateBucket({named:?}) must succeed, got {status} with {text}"
    );
}

/// Attach the policy that lets CloudTrail deliver into `named` under each key prefix of
/// `key_prefixes`: the ACL check on the bucket, and the object write under that prefix's own
/// delivery area — `<key-prefix>/AWSLogs/<account>/*`, which is exactly the area the delivery
/// writes into.
fn grant_delivery(started: &Started, named: &str, key_prefixes: &[&str]) {
    let mut statements = vec![json!({
        "Sid": "AWSCloudTrailAclCheck",
        "Effect": "Allow",
        "Principal": {"Service": "cloudtrail.amazonaws.com"},
        "Action": "s3:GetBucketAcl",
        "Resource": format!("arn:aws:s3:::{named}"),
    })];
    for key_prefix in key_prefixes {
        statements.push(json!({
            "Sid": format!("AWSCloudTrailWrite-{key_prefix}"),
            "Effect": "Allow",
            "Principal": {"Service": "cloudtrail.amazonaws.com"},
            "Action": "s3:PutObject",
            "Resource": format!(
                "arn:aws:s3:::{named}/{key_prefix}/{LOG_FILE_ROOT_SEGMENT}/{ACCOUNT}/*"
            ),
            "Condition": {"StringEquals": {"s3:x-amz-acl": "bucket-owner-full-control"}},
        }));
    }
    let document = json!({"Version": "2012-10-17", "Statement": statements}).to_string();
    let (status, text) = started.call(
        "s3",
        "PUT",
        &format!("/{named}?policy"),
        &[("content-type", "application/json")],
        document.into_bytes(),
    );
    assert!(
        (200..300).contains(&status),
        "PutBucketPolicy({named:?}) must succeed, got {status} with {text}"
    );
}

/// The advanced event selector that admits the object data events of the data bucket, with
/// `exclusion` — an operator member and its operand — stated on the SAME `resources.ARN` field
/// selector as the positive narrowing when there is one.
fn object_selector(name: &str, exclusion: Option<(&str, String)>) -> Value {
    let mut arn_statement = json!({
        "Field": "resources.ARN",
        "StartsWith": [format!("arn:aws:s3:::{DATA_BUCKET}/")],
    });
    if let Some((operator, operand)) = exclusion {
        arn_statement[operator] = json!([operand]);
    }
    json!([{
        "Name": name,
        "FieldSelectors": [
            {"Field": "eventCategory", "Equals": [DATA_CATEGORY]},
            {"Field": "resources.type", "Equals": [OBJECT_RESOURCE_TYPE]},
            arn_statement,
        ],
    }])
}

/// Point a trail at the log bucket under `key_prefix`, install `selectors` on it addressed by the
/// ARN its create answered, read them back and start it logging — the five calls a client
/// installs such a configuration with.
fn logging_trail(started: &Started, name: &str, key_prefix: &str, selectors: &Value) {
    let created = cloudtrail(
        started,
        "CreateTrail",
        &json!({
            "Name": name,
            "S3BucketName": LOG_BUCKET,
            "S3KeyPrefix": key_prefix,
            "IsMultiRegionTrail": false,
            "IncludeGlobalServiceEvents": false,
        }),
    );
    let trail_arn = created["TrailARN"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateTrail must report a TrailARN, got {created}"))
        .to_string();
    cloudtrail(
        started,
        "PutEventSelectors",
        &json!({"TrailName": trail_arn, "AdvancedEventSelectors": selectors}),
    );
    let read = cloudtrail(
        started,
        "GetEventSelectors",
        &json!({"TrailName": trail_arn}),
    );
    assert_eq!(
        read.get("AdvancedEventSelectors"),
        Some(selectors),
        "trail {name:?}: GetEventSelectors must report the selectors as submitted, got {read}"
    );
    cloudtrail(started, "StartLogging", &json!({"Name": trail_arn}));
    let status = cloudtrail(started, "GetTrailStatus", &json!({"Name": trail_arn}));
    assert_eq!(
        status["IsLogging"],
        json!(true),
        "trail {name:?} must report it is logging, got {status}; the log:\n{}",
        started.log()
    );
}

/// Every object key the log bucket holds under the whole delivery area of the trail delivering
/// beneath `key_prefix`, as Amazon S3's own list reports them.
fn delivered_keys(started: &Started, key_prefix: &str) -> Vec<String> {
    let prefix = format!("{key_prefix}/{LOG_FILE_ROOT_SEGMENT}/");
    let (status, text) = started.call(
        "s3",
        "GET",
        &format!("/{LOG_BUCKET}?list-type=2&prefix={prefix}"),
        &[],
        Vec::new(),
    );
    assert_eq!(
        status, 200,
        "ListObjectsV2({LOG_BUCKET:?}, {prefix:?}) must succeed, got {status} with {text}"
    );
    let mut keys = elements(&text, "Key");
    keys.sort();
    keys
}

/// Look under one trail's delivery area until it holds at least `wanted` objects, or
/// [`SETTLE_DEADLINE`] has passed — the CONDITION is what is waited for, and the deadline only
/// bounds a wait that will never be satisfied.
fn settled(started: &Started, key_prefix: &str, wanted: usize) -> Vec<String> {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let keys = delivered_keys(started, key_prefix);
        if keys.len() >= wanted || Instant::now() >= deadline {
            return keys;
        }
        std::thread::sleep(LOOK_INTERVAL);
    }
}

/// Assert `key` is a log-file key of the trail delivering under `key_prefix`: the documented
/// layout down to the DAY, and a file name carrying the account, the service and the Region.
fn assert_log_file_key(case: &str, key_prefix: &str, key: &str) {
    let area = format!(
        "{key_prefix}/{LOG_FILE_ROOT_SEGMENT}/{ACCOUNT}/{LOG_FILE_SERVICE_SEGMENT}/{REGION}/"
    );
    let Some(rest) = key.strip_prefix(&area) else {
        panic!("case {case:?}: expected a key under {area:?}, got {key:?}");
    };
    let (day, name) = rest
        .rsplit_once('/')
        .unwrap_or_else(|| panic!("case {case:?}: expected a day folder in {rest:?}"));
    let parts: Vec<&str> = day.split('/').collect();
    let dated = parts.len() == 3
        && parts[0].len() == 4
        && parts[1].len() == 2
        && parts[2].len() == 2
        && parts
            .iter()
            .all(|part| part.chars().all(|digit| digit.is_ascii_digit()));
    assert!(
        dated,
        "case {case:?}: expected the YYYY/MM/DD of the records, got {day:?} in {key:?}"
    );
    let expected_name = format!("{ACCOUNT}_{LOG_FILE_SERVICE_SEGMENT}_{REGION}_");
    assert!(
        name.starts_with(&expected_name) && name.ends_with(LOG_FILE_SUFFIX),
        "case {case:?}: expected a file named {expected_name}<minute>_<unique>\
         {LOG_FILE_SUFFIX}, got {name:?}"
    );
}

#[test]
fn an_arn_exclusion_withholds_only_its_own_objects_from_one_of_two_logging_trails() {
    let started = Started::start();
    create_bucket(&started, LOG_BUCKET);
    grant_delivery(&started, LOG_BUCKET, &[EXCLUDING_PREFIX, CONTROL_PREFIX]);
    create_bucket(&started, DATA_BUCKET);

    // Both selectors admit the data events of the one bucket; the excluding trail's leaves out
    // one key prefix of it, stated on the same field selector as the positive narrowing.
    logging_trail(
        &started,
        EXCLUDING_TRAIL,
        EXCLUDING_PREFIX,
        &object_selector(
            "objects-except-excluded-prefix",
            Some((
                "NotStartsWith",
                format!("arn:aws:s3:::{DATA_BUCKET}/{EXCLUDED_KEY_PREFIX}"),
            )),
        ),
    );
    logging_trail(
        &started,
        CONTROL_TRAIL,
        CONTROL_PREFIX,
        &object_selector("all-objects-in-bucket", None),
    );

    // The calls of one round: the admitted object written and read, the excluded object written
    // and read — so the excluded calls are as eligible for delivery as the rest — and a last
    // write of an admitted object, which is what tells a withheld record from one that had not
    // landed yet.
    let admitted = format!("{INCLUDED_KEY_PREFIX}report.csv");
    let excluded = format!("{EXCLUDED_KEY_PREFIX}report.csv");
    let barrier = format!("{INCLUDED_KEY_PREFIX}barrier.csv");
    for (case, method, key) in [
        ("a write inside the ARN scope", "PUT", &admitted),
        ("a read inside the ARN scope", "GET", &admitted),
        ("a write of an excluded object", "PUT", &excluded),
        ("a read of an excluded object", "GET", &excluded),
        ("the last write inside the ARN scope", "PUT", &barrier),
    ] {
        let body = if method == "PUT" {
            b"ledger".to_vec()
        } else {
            Vec::new()
        };
        let (status, text) = started.call(
            "s3",
            method,
            &format!("/{DATA_BUCKET}/{key}"),
            &[("content-type", "text/plain")],
            body,
        );
        assert_eq!(
            status, 200,
            "case {case:?}: {method} {key:?} must be served, got {status} with {text}"
        );
    }

    // The CONTROL trail, whose selector narrows the bucket and excludes nothing: one file per
    // call of the round.
    let control = settled(&started, CONTROL_PREFIX, 5);
    assert_eq!(
        control.len(),
        5,
        "case \"a selector that excludes nothing\": expected one delivered file per object call \
         under {CONTROL_PREFIX:?}, got {control:?}; the log:\n{}",
        started.log()
    );
    for key in &control {
        assert_log_file_key(
            "a selector that excludes nothing",
            CONTROL_PREFIX,
            key,
        );
    }

    // The EXCLUDING trail: one file per call OUTSIDE the excluded key prefix, and nothing for the
    // two calls under it — looked at again after the control leg's own files landed, so the
    // absence is read after the delivery latency that leg demonstrates rather than before it.
    let excluding = settled(&started, EXCLUDING_PREFIX, 3);
    let case = "a selector excluding one key prefix of the same bucket";
    for look in 1..=ABSENCE_LOOKS {
        let keys = delivered_keys(&started, EXCLUDING_PREFIX);
        assert_eq!(
            keys.len(),
            3,
            "case {case:?}: expected one delivered file per call outside \
             {EXCLUDED_KEY_PREFIX:?} and none under it (look {look} of {ABSENCE_LOOKS}), got \
             {keys:?}; the log:\n{}",
            started.log()
        );
        std::thread::sleep(LOOK_INTERVAL);
    }
    for key in &excluding {
        assert_log_file_key(case, EXCLUDING_PREFIX, key);
    }
}
