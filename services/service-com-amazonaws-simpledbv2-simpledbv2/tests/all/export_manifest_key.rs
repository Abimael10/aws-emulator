// SPDX-License-Identifier: CC0-1.0

//! The S3 key a finished export reports as its `exportManifest`.
//!
//! RULE: `exportManifest` is "the name of the manifest summary file for the export"
//! (`smithy_query(query="doc",shape="GetExportResponse",member="exportManifest")`), and a
//! finished export's manifest summary is the fixed `manifest-summary.json` inside that one
//! export's own key space: the `s3KeyPrefix` the start named, reproduced verbatim and
//! joined to the rest with one separator, then the service's fixed `AmazonSimpleDB`
//! segment, then the exported `domainName`, then the `exportArn` the service minted for
//! the export. A start that named no prefix has nothing to join to, so its key space
//! begins at the fixed segment.
//!
//! The rows below span what the rule generalises over — whether a prefix was named at all,
//! the form it takes (a trailing separator, none, nested elements, dots and a space, the
//! modelled 850-character maximum) and the form of the exported domain name — because the
//! variable parts of the key are exactly the request's prefix, the request's domain name
//! and the ARN this service minted; `AmazonSimpleDB` and `manifest-summary.json` are the
//! same for every export.

use serde_json::{json, Value};

use crate::domain_export_lifecycle::{
    emulator_with_domains, member, ok_call, poll_until_succeeded, LONGEST_KEY_PREFIX,
};

/// The service-fixed segment of an export's key space, and the fixed name of the manifest
/// summary file inside it — spelled out here rather than taken from the service's own
/// constants, so a change to either is a failing assertion rather than a silently
/// agreeing one. Both are the same for every export of every domain; the recorded AWS
/// answer for a SUCCEEDED export is where this layout is published.
const SERVICE_SEGMENT: &str = "AmazonSimpleDB";
const MANIFEST_FILE: &str = "manifest-summary.json";

/// The bucket every export below is pointed at — a name the modelled `S3BucketName`
/// pattern admits, so no row is refused on it.
const BUCKET: &str = "manifest-key-bucket";

/// One row: the case, the `s3KeyPrefix` the start names (`None` names none at all) and
/// the domain it exports.
type Row = (&'static str, Option<&'static str>, &'static str);

/// The rows, spanning the prefix forms an `s3KeyPrefix` may take and the domain-name
/// forms a `DomainName` may take.
fn rows() -> Vec<Row> {
    vec![
        ("no key prefix at all", None, "unprefixed-domain"),
        ("a prefix ending in a separator", Some("exports/"), "orders"),
        ("a prefix with no trailing separator", Some("exports"), "orders"),
        ("a nested prefix", Some("tenant/42/daily/"), "warehouse"),
        (
            "a prefix carrying a space and dots",
            Some("my dumps/v1.2/"),
            "events",
        ),
        (
            "a prefix at the modelled 850-character maximum",
            Some(LONGEST_KEY_PREFIX),
            "bulk",
        ),
        ("a one-character domain name", Some("exports/"), "d"),
        (
            "a domain name carrying dots, hyphens and underscores",
            Some("exports/"),
            "probe.simpledbv2-domain_9",
        ),
    ]
}

/// The key space the export `export_arn` of `domain` writes under, as the rule composes
/// it from the request's own values.
fn expected_key_space(prefix: Option<&str>, domain: &str, export_arn: &str) -> String {
    let space = format!("{SERVICE_SEGMENT}/{domain}/{export_arn}/");
    match prefix {
        Some(prefix) => format!("{prefix}/{space}"),
        None => space,
    }
}

/// The body of the start one row makes.
fn start_body(prefix: Option<&str>, domain: &str) -> Value {
    let mut request = json!({ "domainName": domain, "s3Bucket": BUCKET });
    if let Some(prefix) = prefix {
        request["s3KeyPrefix"] = json!(prefix);
    }
    request
}

#[tokio::test]
async fn a_finished_exports_manifest_key_is_the_summary_file_of_its_own_key_space() {
    let rows = rows();
    let domains: Vec<&str> = rows.iter().map(|(_, _, domain)| *domain).collect();
    let router = emulator_with_domains(&domains);

    // Every row's export is started first, so the clock runs all of them at once and no
    // row waits on another's span.
    let mut started: Vec<(Row, String)> = Vec::new();
    for row in rows {
        let (case, prefix, domain) = row;
        let accepted = ok_call(&router, "StartDomainExport", &start_body(prefix, domain)).await;
        let export_arn = member(&accepted, "exportArn")
            .unwrap_or_else(|| panic!("case {case:?}: no exportArn in {accepted:?}"));
        started.push((row, export_arn));
    }

    for ((case, prefix, domain), export_arn) in started {
        let succeeded = poll_until_succeeded(&router, case, &export_arn).await;
        let expected = format!(
            "{}{MANIFEST_FILE}",
            expected_key_space(prefix, domain, &export_arn)
        );
        let actual = member(&succeeded, "exportManifest");
        assert_eq!(
            actual.as_deref(),
            Some(expected.as_str()),
            "case {case:?}: expected the manifest key {expected:?}, got {actual:?} in \
             {succeeded:?}"
        );
        let actual = actual.unwrap_or_default();
        // The parts a client parses back out of the key: the domain it is an export of
        // and the export's own ARN both travel in it, and the modelled
        // `ExportManifestSummary` is `@length(min: 1)`.
        assert!(
            !actual.is_empty() && actual.contains(domain) && actual.contains(&export_arn),
            "case {case:?}: the key must carry the exported domain {domain:?} and the \
             export ARN {export_arn:?}, got {actual:?}"
        );
        assert!(
            actual.contains(&format!("/{SERVICE_SEGMENT}/"))
                || actual.starts_with(&format!("{SERVICE_SEGMENT}/")),
            "case {case:?}: the key must carry the service-fixed segment \
             {SERVICE_SEGMENT:?}, got {actual:?}"
        );
        if let Some(prefix) = prefix {
            assert!(
                actual.starts_with(prefix),
                "case {case:?}: the caller's prefix {prefix:?} travels verbatim at the head \
                 of the key, got {actual:?}"
            );
        }

        // A read changes nothing: the key a later caller sees is the one already stored.
        let again = ok_call(&router, "GetExport", &json!({ "exportArn": export_arn })).await;
        assert_eq!(
            member(&again, "exportManifest"),
            Some(actual.clone()),
            "case {case:?}: a second read reports the same manifest key {actual:?}, got \
             {again:?}"
        );
    }
}

/// Two exports of ONE domain into ONE prefix report different manifest keys, because the
/// export's own ARN is part of the key: neither export's manifest can overwrite the
/// other's.
#[tokio::test]
async fn two_exports_of_one_domain_into_one_prefix_keep_their_own_manifest_keys() {
    let router = emulator_with_domains(&["twice-exported"]);
    let body = start_body(Some("exports/"), "twice-exported");

    let mut keys: Vec<String> = Vec::new();
    let mut arns: Vec<String> = Vec::new();
    for round in 0..2 {
        // A fresh `clientToken` per round: the same token would be the idempotent repeat
        // of the first export rather than a second one.
        let mut request = body.clone();
        request["clientToken"] = json!(format!("token-{round}"));
        let accepted = ok_call(&router, "StartDomainExport", &request).await;
        arns.push(
            member(&accepted, "exportArn")
                .unwrap_or_else(|| panic!("case round {round}: no exportArn in {accepted:?}")),
        );
    }
    for (round, export_arn) in arns.iter().enumerate() {
        let case = format!("round {round}");
        let succeeded = poll_until_succeeded(&router, &case, export_arn).await;
        let key = member(&succeeded, "exportManifest")
            .unwrap_or_else(|| panic!("case {case}: no exportManifest in {succeeded:?}"));
        let expected = format!(
            "{}{MANIFEST_FILE}",
            expected_key_space(Some("exports/"), "twice-exported", export_arn)
        );
        assert_eq!(
            key, expected,
            "case {case}: expected {expected:?}, got {key:?}"
        );
        keys.push(key);
    }
    assert_ne!(
        keys[0], keys[1],
        "case two exports of one domain into one prefix: each export's manifest has its \
         own key, got {keys:?}"
    );
}
