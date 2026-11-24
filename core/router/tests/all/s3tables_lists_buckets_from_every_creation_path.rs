// SPDX-License-Identifier: CC0-1.0

//! Amazon S3 Tables lists a table bucket whatever created it: its own `CreateTableBucket`, or
//! a CloudFormation stack that declared one.
//!
//! The rule under test: **`ListTableBuckets` reports every table bucket the account holds in
//! the Region the request is made against — each as the whole `TableBucketSummary` its model
//! declares, and each the same record `GetTableBucket` answers for that bucket's ARN —
//! because a table bucket is a record of its OWNING service and exists independently of the
//! path that created it.** A resource a stack reports created therefore appears in the
//! listing beside one a client created over the API, with the same members filled the same
//! way, and the Region-scoped listing reports the buckets of the Region it was asked in and
//! no others.
//!
//! Two crates have to agree for that to hold (CloudFormation makes the create, Amazon S3
//! Tables serves and reports it), so the suite lives here rather than in either of them.
//!
//! The rows differ along what the rule generalises over: the creation path (the S3 Tables
//! API, a stack that NAMES its bucket, and a stack that leaves the name to CloudFormation),
//! the Region the bucket lives in, how many buckets the Region holds, and the filters and the
//! page size the listing is asked with (`prefix`, `type`, `maxBuckets` — the `@httpQuery`
//! members `smithy_query(query="operation", operation="ListTableBuckets")` reports on that
//! service's model). Every expected value is computed from what the row created, never read
//! back out of the answer being checked — except a GENERATED bucket name, which is
//! CloudFormation's to compute: what is checked about one is that the handle the stack
//! published is that name's table-bucket ARN.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use aws_smithy_types::date_time::Format;
use aws_smithy_types::DateTime;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The XML element readers this crate writes ONCE (`tests/all/query_wire.rs`).
use super::query_wire::{opt_element, physical_ids_of};
// The four-service router, the two wire callers, the handle writer, the table bucket ARN form
// and the published name form the provisioning suite for this type already writes: every
// suite over these two services drives the SAME emulator through the same calls.
use super::cloudformation_provisions_s3tables_table_buckets::{
    build_router, cloudformation, is_s3tables_name, label, s3tables, table_bucket_arn,
};

/// How long a poll waits for a stack to reach a terminal status. The create's clock-owed span
/// falls due one transition delay after the call that opened it; this bound is generous enough
/// that a loaded machine never reads a status that is merely not due yet.
const TERMINAL_WAIT: Duration = Duration::from_secs(60);

/// How often that poll asks, which is how a client watches a stack.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// How many pages of a listing this suite drains. Each Region here holds a handful of table
/// buckets, so the bound only stops a token the service failed to retire from looping forever.
const MAX_PAGES: usize = 16;

/// The statuses a `CreateStack` can settle at, as `StackStatus` spells them.
const TERMINAL_STATUSES: &[&str] = &[
    "CREATE_COMPLETE",
    "CREATE_FAILED",
    "ROLLBACK_COMPLETE",
    "ROLLBACK_FAILED",
];

/// The Regions the rows deploy in: a table bucket's ARN names the Region it lives in, and the
/// listing is Region-scoped, so the same names are created in both.
const REGIONS: &[&str] = &["us-east-1", "eu-west-1"];

/// The resource type a stack declares a table bucket as.
const TABLE_BUCKET_TYPE: &str = "AWS::S3Tables::TableBucket";

/// The name the row creates over the S3 Tables API, and the one its stack DECLARES. Both are
/// inside the published name form (`CreateTableBucket.name`: `@length(min: 3, max: 63)`,
/// pattern `^[0-9a-z-]*$`) and share a prefix, which is what the `prefix` filter is asked
/// with.
const PREFIX: &str = "every-path-";
const API_MADE: &str = "every-path-api-made";
const STACK_NAMED: &str = "every-path-stack-named";

/// A prefix no bucket here carries, for the filter that must then report none.
const ABSENT_PREFIX: &str = "no-such-bucket-prefix";

/// The logical ids the stack gives its two table buckets.
const NAMED: &str = "Named";
const GENERATED: &str = "Generated";

/// The `TableBucketType` a bucket a CUSTOMER created carries, and the other value that enum
/// declares — the one Amazon Web Services' own buckets carry
/// (`smithy_query(query="shape", shape="TableBucketType")`: `customer`, `aws`).
const CUSTOMER: &str = "customer";
const AWS_OWNED: &str = "aws";

/// Every member `TableBucketSummary` declares, from that model's own shape
/// (`smithy_query(query="shape", shape="TableBucketSummary")`): `arn`, `name`,
/// `ownerAccountId` and `createdAt` are `@required`, with `tableBucketId` and `type` beside
/// them — and `GetTableBucketResponse` reports exactly these six, so a listing entry and that
/// read are comparable member by member.
const SUMMARY_MEMBERS: &[&str] = &[
    "arn",
    "name",
    "ownerAccountId",
    "createdAt",
    "tableBucketId",
    "type",
];

/// The modelled length of an `AccountId` (`@length(min: 12, max: 12)`, pattern `^[0-9].*$`).
const ACCOUNT_ID_DIGITS: usize = 12;

/// The template a row deploys: one table bucket the template NAMES, and one it leaves for
/// CloudFormation to name.
fn template() -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Description": "a named table bucket and one CloudFormation names",
        "Resources": {
            NAMED: {
                "Type": TABLE_BUCKET_TYPE,
                "Properties": { "TableBucketName": STACK_NAMED }
            },
            GENERATED: { "Type": TABLE_BUCKET_TYPE }
        }
    })
    .to_string()
}

/// One table bucket a row created, and the path it came to exist by.
struct Made {
    /// The bucket's ARN, which is how S3 Tables keys it and what a stack publishes for one.
    arn: String,
    /// The bucket's own name.
    name: String,
    /// How it was created, for the case names the assertions quote.
    path: &'static str,
}

/// The name a table bucket ARN carries: the segment after its `bucket/`, which is how that
/// service spells the resource part of one.
fn name_in(arn: &str) -> String {
    arn.rsplit_once("bucket/")
        .map(|(_, name)| name.to_string())
        .unwrap_or_default()
}

/// The text of one member of a JSON answer, or `None` where the answer carries no such member.
fn text(answer: &Value, member: &str) -> Option<String> {
    answer
        .get(member)
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// The member names one answer carries, so two answers can be compared on WHICH members they
/// filled rather than only on the values of the ones that were looked at.
fn member_keys(answer: &Value) -> BTreeSet<String> {
    answer
        .as_object()
        .map(|members| members.keys().cloned().collect())
        .unwrap_or_default()
}

/// Poll `DescribeStacks` until the stack SETTLES, and answer the terminal status it settled at
/// — which is how a client waits for a stack.
async fn settled_status(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    stack_name: &str,
) -> String {
    let deadline = Instant::now() + TERMINAL_WAIT;
    loop {
        let (status, described) =
            cloudformation(router, region, "DescribeStacks", &[("StackName", stack_name)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeStacks on {stack_name:?}, got \
             {status} with body {described}"
        );
        let reached = opt_element(&described, "StackStatus").unwrap_or_default();
        if TERMINAL_STATUSES.contains(&reached.as_str()) {
            return reached;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {stack_name:?} to settle at a terminal status within \
             {TERMINAL_WAIT:?}, it is still {reached:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The stack's resource rows as a client reads them: `ListStackResources` drained through its
/// `NextToken`, so a row is never missed because it fell on a later page.
async fn resource_rows(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    stack_name: &str,
) -> BTreeMap<String, String> {
    let mut rows = BTreeMap::new();
    let mut next_token: Option<String> = None;
    for _ in 0..MAX_PAGES {
        let token = next_token.take();
        let mut members: Vec<(&str, &str)> = vec![("StackName", stack_name)];
        if let Some(token) = token.as_deref() {
            members.push(("NextToken", token));
        }
        let (status, listed) = cloudformation(router, region, "ListStackResources", &members).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from ListStackResources, got {status} with body \
             {listed}"
        );
        rows.extend(physical_ids_of(&listed));
        next_token = opt_element(&listed, "NextToken");
        if next_token.is_none() {
            return rows;
        }
    }
    panic!(
        "case {case:?}: ListStackResources did not retire its NextToken within {MAX_PAGES} pages"
    );
}

/// The physical id one logical id reports, or a panic quoting the rows that were read.
fn published(case: &str, rows: &BTreeMap<String, String>, logical_id: &str) -> String {
    rows.get(logical_id)
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: expected a physical id for {logical_id:?}, got the rows {rows:?}"))
}

/// Create one table bucket each way in `region`: over the S3 Tables API, and through a stack
/// that declares two — one it names and one CloudFormation names. The handles the stack
/// publishes are checked here to be the table bucket ARNs the Region and each name compute,
/// so what the listing is then held to is a value this suite computed rather than read back.
async fn every_creation_path(router: &MultiServiceRouter, region: &str) -> Vec<Made> {
    let case = format!("{region}: a table bucket created each way");

    let (status, created) = s3tables(
        router,
        region,
        "PUT",
        "/buckets",
        Some(&json!({ "name": API_MADE })),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateTableBucket, got {status} with body \
         {created}"
    );
    let api_arn = text(&created, "arn").unwrap_or_default();
    assert_eq!(
        api_arn,
        table_bucket_arn(region, API_MADE),
        "case {case:?}: expected CreateTableBucket to mint the ARN {:?}, got {api_arn:?}",
        table_bucket_arn(region, API_MADE)
    );

    let stack_name = format!("table-bucket-listing-{region}");
    let (status, body) = cloudformation(
        router,
        region,
        "CreateStack",
        &[("StackName", &stack_name), ("TemplateBody", &template())],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {body}"
    );
    let reached = settled_status(router, &case, region, &stack_name).await;
    assert_eq!(
        reached, "CREATE_COMPLETE",
        "case {case:?}: expected the stack to reach CREATE_COMPLETE, got {reached:?}"
    );

    let rows = resource_rows(router, &case, region, &stack_name).await;
    let named_arn = published(&case, &rows, NAMED);
    assert_eq!(
        named_arn,
        table_bucket_arn(region, STACK_NAMED),
        "case {case:?}: expected the declared bucket's handle to be the ARN {:?}, got \
         {named_arn:?}",
        table_bucket_arn(region, STACK_NAMED)
    );
    let generated_arn = published(&case, &rows, GENERATED);
    let generated_name = name_in(&generated_arn);
    assert!(
        is_s3tables_name(&generated_name),
        "case {case:?}: expected the generated bucket name inside the published name form, got \
         {generated_name:?}"
    );
    assert_eq!(
        generated_arn,
        table_bucket_arn(region, &generated_name),
        "case {case:?}: expected the generated bucket's handle to be the ARN {:?}, got \
         {generated_arn:?}",
        table_bucket_arn(region, &generated_name)
    );

    vec![
        Made {
            arn: api_arn,
            name: API_MADE.to_string(),
            path: "created over the S3 Tables API",
        },
        Made {
            arn: named_arn,
            name: STACK_NAMED.to_string(),
            path: "created by a stack that named it",
        },
        Made {
            arn: generated_arn,
            name: generated_name,
            path: "created by a stack that left the name to CloudFormation",
        },
    ]
}

/// `ListTableBuckets` in `region`, with the `@httpQuery` members of the request written as the
/// query string a client sends (empty for a listing asked with none). Its `tableBuckets`
/// member is `@required`, so an answer that carries no list at all is a failure rather than an
/// empty page.
async fn listed(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    query: &str,
) -> Vec<Value> {
    let uri = if query.is_empty() {
        "/buckets".to_string()
    } else {
        format!("/buckets?{query}")
    };
    let (status, answered) = s3tables(router, region, "GET", &uri, None).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from ListTableBuckets on {uri:?}, got {status} with \
         body {answered}"
    );
    answered
        .get("tableBuckets")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: tableBuckets is a required member, so {uri:?} must answer a \
                 list, got {answered}"
            )
        })
}

/// Every entry of a listing keyed by the ARN it reports, which is the bucket's identity.
fn by_arn(entries: &[Value]) -> BTreeMap<String, Value> {
    entries
        .iter()
        .map(|entry| (text(entry, "arn").unwrap_or_default(), entry.clone()))
        .collect()
}

/// The ARNs of a listing, as the set a case compares against.
fn arns(entries: &[Value]) -> BTreeSet<String> {
    entries
        .iter()
        .map(|entry| text(entry, "arn").unwrap_or_default())
        .collect()
}

/// Drain a listing one `maxBuckets`-sized page at a time, following the `continuationToken`
/// the previous page answered — the pagination that operation's `@paginated` trait declares.
async fn drained(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    max_buckets: usize,
) -> Vec<Value> {
    let mut all = Vec::new();
    let mut token: Option<String> = None;
    for _ in 0..MAX_PAGES {
        let mut query = format!("maxBuckets={max_buckets}");
        if let Some(carried) = token.as_deref() {
            query.push_str(&format!("&continuationToken={carried}"));
        }
        let uri = format!("/buckets?{query}");
        let (status, answered) = s3tables(router, region, "GET", &uri, None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from ListTableBuckets on {uri:?}, got {status} \
             with body {answered}"
        );
        let page = answered
            .get("tableBuckets")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        assert!(
            page.len() <= max_buckets,
            "case {case:?}: expected at most {max_buckets} entries on a page, got {}",
            page.len()
        );
        all.extend(page);
        token = text(&answered, "continuationToken");
        if token.is_none() {
            return all;
        }
    }
    panic!("case {case:?}: ListTableBuckets did not retire its continuationToken within {MAX_PAGES} pages");
}

/// The listing reports a bucket a stack created beside one the API created, with every member
/// of the summary filled the same way — and reports it in the Region it lives in and in no
/// other.
#[tokio::test]
async fn every_table_bucket_is_listed_however_it_was_created() {
    let router = build_router();

    // Both Regions are populated BEFORE either listing is read, so a listing that reported
    // another Region's buckets would be caught rather than merely not yet have any to report.
    let mut made: Vec<(&str, Vec<Made>)> = Vec::new();
    for region in REGIONS {
        made.push((region, every_creation_path(&router, region).await));
    }

    for (region, buckets) in &made {
        let case = format!("{region}: every table bucket the Region holds");
        let entries = listed(&router, &case, region, "").await;
        let by_arn = by_arn(&entries);
        assert_eq!(
            arns(&entries),
            buckets.iter().map(|made| made.arn.clone()).collect(),
            "case {case:?}: expected the listing to report exactly the buckets created in this \
             Region, got {:?}",
            arns(&entries)
        );

        // The API-created bucket is the reference: the rule is that a stack-created bucket is
        // reported with the SAME members, so the member set of this entry is what the others
        // are held to.
        let reference = by_arn
            .get(&buckets[0].arn)
            .unwrap_or_else(|| panic!("case {case:?}: the API-created bucket must be listed"));
        let reference_members = member_keys(reference);
        let reference_owner = text(reference, "ownerAccountId").unwrap_or_default();
        assert_eq!(
            reference_owner.len(),
            ACCOUNT_ID_DIGITS,
            "case {case:?}: expected a {ACCOUNT_ID_DIGITS}-digit ownerAccountId, got \
             {reference_owner:?}"
        );
        assert!(
            reference_owner.chars().all(|digit| digit.is_ascii_digit()),
            "case {case:?}: expected ownerAccountId to be digits, got {reference_owner:?}"
        );

        let mut identifiers: BTreeSet<String> = BTreeSet::new();
        for made in buckets {
            let case = format!("{region}: the table bucket {}", made.path);
            let entry = by_arn.get(&made.arn).unwrap_or_else(|| {
                panic!(
                    "case {case:?}: expected the bucket {:?} among the listed table buckets, got \
                     {:?}",
                    made.arn,
                    arns(&entries)
                )
            });
            assert_eq!(
                text(entry, "arn").as_deref(),
                Some(made.arn.as_str()),
                "case {case:?}: expected the entry to report the ARN {:?}, got {entry}",
                made.arn
            );
            assert_eq!(
                text(entry, "name").as_deref(),
                Some(made.name.as_str()),
                "case {case:?}: expected the entry to report the name {:?}, got {entry}",
                made.name
            );
            assert_eq!(
                text(entry, "ownerAccountId").as_deref(),
                Some(reference_owner.as_str()),
                "case {case:?}: expected the owning account {reference_owner:?}, whatever \
                 created the bucket, got {entry}"
            );
            assert_eq!(
                text(entry, "type").as_deref(),
                Some(CUSTOMER),
                "case {case:?}: a bucket a customer's own call created is of type \
                 {CUSTOMER:?}, got {entry}"
            );
            let created_at = text(entry, "createdAt").unwrap_or_else(|| {
                panic!(
                    "case {case:?}: createdAt is modelled @timestampFormat(\"date-time\"), so it \
                     must be a string, got {entry}"
                )
            });
            assert!(
                DateTime::from_str(&created_at, Format::DateTime).is_ok(),
                "case {case:?}: expected createdAt in the modelled date-time wire form, got \
                 {created_at:?}"
            );
            let identifier = text(entry, "tableBucketId").unwrap_or_default();
            assert!(
                !identifier.is_empty(),
                "case {case:?}: expected a system-assigned tableBucketId, got {entry}"
            );
            assert!(
                identifiers.insert(identifier.clone()),
                "case {case:?}: expected a tableBucketId of its own, {identifier:?} is already \
                 another bucket's"
            );
            assert_eq!(
                member_keys(entry),
                reference_members,
                "case {case:?}: expected the same summary members as a bucket created over the \
                 API ({reference_members:?}), got {entry}"
            );

            // And the entry is the record that service's own read answers for the bucket.
            let uri = format!("/buckets/{}", label(&made.arn));
            let (status, read) = s3tables(&router, region, "GET", &uri, None).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected HTTP 200 from GetTableBucket on {uri:?}, got {status} \
                 with body {read}"
            );
            for member in SUMMARY_MEMBERS {
                assert_eq!(
                    text(&read, member),
                    text(entry, member),
                    "case {case:?}: expected GetTableBucket and the listing to report the same \
                     {member}, got {read} and {entry}"
                );
            }
        }
    }
}

/// The filters and the page size a listing is asked with select among the buckets the Region
/// holds — whatever created each one.
#[tokio::test]
async fn listing_filters_select_among_the_buckets_whatever_created_them() {
    let router = build_router();
    for region in REGIONS {
        let buckets = every_creation_path(&router, region).await;
        let case = format!("{region}: filtered listings");
        let entries = listed(&router, &case, region, "").await;

        // `prefix` reports exactly the buckets whose NAME begins with it, which spans both
        // creation paths: the API-created bucket and the stack's declared one share one.
        let prefixed: BTreeSet<String> = entries
            .iter()
            .filter(|entry| {
                text(entry, "name").is_some_and(|name| name.starts_with(PREFIX))
            })
            .map(|entry| text(entry, "arn").unwrap_or_default())
            .collect();
        assert!(
            prefixed.len() >= 2,
            "case {case:?}: the rows must leave at least two buckets under the prefix \
             {PREFIX:?}, got {prefixed:?}"
        );
        let answered = listed(&router, &case, region, &format!("prefix={PREFIX}")).await;
        assert_eq!(
            arns(&answered),
            prefixed,
            "case {case:?}: expected the buckets named under {PREFIX:?}, got {:?}",
            arns(&answered)
        );

        let answered = listed(&router, &case, region, &format!("prefix={ABSENT_PREFIX}")).await;
        assert!(
            answered.is_empty(),
            "case {case:?}: expected no bucket under the prefix {ABSENT_PREFIX:?}, got {:?}",
            arns(&answered)
        );

        // `type` reports exactly the buckets of that type: every bucket a customer's own call
        // created is a `customer` bucket, by either path, and none of them is an `aws` one.
        let answered = listed(&router, &case, region, &format!("type={CUSTOMER}")).await;
        assert_eq!(
            arns(&answered),
            buckets.iter().map(|made| made.arn.clone()).collect(),
            "case {case:?}: expected every bucket of this Region under type={CUSTOMER:?}, got \
             {:?}",
            arns(&answered)
        );
        let answered = listed(&router, &case, region, &format!("type={AWS_OWNED}")).await;
        assert!(
            answered.is_empty(),
            "case {case:?}: expected no bucket under type={AWS_OWNED:?}, got {:?}",
            arns(&answered)
        );

        // And a listing drained one page at a time reports the same buckets as one page of
        // every bucket, whatever created each.
        for max_buckets in [1, 2] {
            let case = format!("{case}, {max_buckets} per page");
            let pages = drained(&router, &case, region, max_buckets).await;
            assert_eq!(
                arns(&pages),
                arns(&entries),
                "case {case:?}: expected the drained pages to report the whole listing, got {:?}",
                arns(&pages)
            );
        }
    }
}
