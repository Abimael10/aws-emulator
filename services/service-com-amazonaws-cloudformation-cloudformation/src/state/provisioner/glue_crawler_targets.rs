// SPDX-License-Identifier: CC0-1.0

//! The AMAZON S3 TARGETS an `AWS::Glue::Crawler` declaration points its crawler at: resolved
//! against this account's own object store before the crawler is submitted, so a declaration
//! naming data that is not there fails its resource instead of deploying.
//!
//! ## The rule this module implements
//!
//! A resource PROVIDER resolves the resources its declaration merely REFERENCES — as the api
//! providers resolve the definition object a `BodyS3Location` addresses
//! ([`super::provisioning::apigateway_definitions`]) — and a reference the owning service
//! answers is not there fails the resource with that service's own answer wrapped in what the
//! handler reports, which is what makes CloudFormation unwind the operation. A crawler's
//! `Targets` are such references: `S3Target$Path` is "the path to the Amazon S3 target"
//! (`smithy_query(query="shape", shape="S3Target")`), data the crawl walks rather than
//! anything the crawler's own create brings into being. So each distinct bucket the declared
//! targets name is resolved HERE, with the one reading of what the object store answers about
//! a bucket (`service_common::s3_client::bucket_is_absent`, a `HeadBucket` — "you can use
//! this operation to determine if a bucket exists"), and a bucket Amazon S3 answered it holds
//! none of fails the resource.
//!
//! Only that ANSWER refuses anything. A bucket the store holds but will not serve, an answer
//! that could not be read and an emulator with no object store to ask each establish nothing,
//! so the crawler is submitted — which is why this is the PROVIDER's step and not the owning
//! service's: AWS Glue serves a `CreateCrawler` over a bucket of another account, whose HEAD
//! answers that caller a refusal rather than an absence, and that create stays served
//! (`services/service-com-amazonaws-glue-awsglue/src/ops/create_crawler.rs` states that rule
//! and `tests/all/crawler_s3_target_resolution.rs` holds it). What a DEPLOYMENT references is
//! resolved where the deployment is made, in the account whose object store the stack's own
//! resources live in.
//!
//! A target whose bucket the SAME stack declares is created before this runs: the walk takes
//! a stack's resources in creation order — `DependsOn` and the implicit dependency a `Ref`,
//! an `Fn::GetAtt` or an `Fn::Sub` creates, and declaration order among independent resources
//! ([`super::template::creation_order`]) — and awaits each create before it reaches the next
//! declaration, so the bucket a template declares above its crawler is already held.
//!
//! ## What a failed resolution reports
//!
//! The resolution is the provider's OWN step rather than a call of the service that owns the
//! type, so what it reports is the sentence that handler words for a target that is not
//! there, naming the target, with the diagnostic of the answer the object store gave
//! ([`absent_target`]) — the shape every refused call of another service reaches a failed
//! resource in ([`super::provisioning::sdk_message`]).
//!
//! The class it is filed under is the one the code Amazon S3 answered falls in, raised to the
//! class for a REFUSED REQUEST because the bucket is a resource the declaration references
//! rather than the crawler this handler manages ([`super::Provisioner::handler_error_class`]):
//! a client reading the class sees a template to fix, not a resource that vanished.

use super::*;

/// The property an `AWS::Glue::Crawler` states the data stores to crawl in, which is the
/// `Targets` member `CreateCrawler` and `UpdateCrawler` both take.
const CRAWLER_TARGETS_PROPERTY: &str = "Targets";

/// The member of `CrawlerTargets` that carries the AMAZON S3 targets — "specifies Amazon
/// Simple Storage Service (Amazon S3) targets" — and the member of ONE such target that
/// addresses the data: "the path to the Amazon S3 target"
/// (`smithy_query(query="shape", shape="CrawlerTargets,S3Target")`).
const S3_TARGETS_MEMBER: &str = "S3Targets";
const S3_TARGET_PATH_MEMBER: &str = "Path";

/// How many DISTINCT buckets one declaration's Amazon S3 targets are resolved against.
///
/// Chosen (AWS publishes no ceiling on a crawler's targets): one deployment costs one answer
/// per distinct bucket, so the work is bounded here the way the owning service bounds the
/// targets one crawler write resolves. Past the ceiling the remaining buckets are not asked
/// about, which refuses nothing — a bound on the READING, never a refusal of its own.
const MAX_RESOLVED_TARGET_BUCKETS: usize = 10;

/// The HTTP status Amazon S3 answers a HEAD of a bucket it does not hold with: "if the bucket
/// does not exist … the HEAD request returns a generic 400 Bad Request, 403 Forbidden, or 404
/// Not Found HTTP status code" (`HeadBucket`), of which 404 is the one that means the bucket
/// is not there — the status the resolution below was answered with and the one the reported
/// diagnostic states.
const BUCKET_ABSENT_STATUS: u16 = 404;

/// The sentence the handler words for a declared Amazon S3 target that is not there.
///
/// Fixed text naming WHAT was resolved, with the target itself — the path the template
/// declared — as its one variable part.
fn absent_target_sentence(path: &str) -> String {
    format!("S3 target {path} does not exist")
}

/// The refusal the handler reports for a declared target whose bucket the object store
/// answered it holds none of: its own sentence, with the diagnostic of that answer — the
/// status Amazon S3 answers for a bucket it does not hold, and the code it answers it under.
fn absent_target(path: &str) -> Refusal {
    Refusal {
        code: BUCKET_NOT_FOUND.to_string(),
        message: provisioning::sdk_message(
            S3_SIGNING_NAME,
            BUCKET_ABSENT_STATUS,
            BUCKET_NOT_FOUND,
            &absent_target_sentence(path),
        ),
    }
}

/// Resolve every Amazon S3 target a crawler declaration states against this account's object
/// store, answering the refusal for the FIRST one that is not there — and `None` where every
/// target resolved, where a target names no bucket at all, and where nothing could be
/// established about one.
///
/// Awaited before the crawler is submitted, with no store guard held: it is one in-process
/// `HeadBucket` per distinct bucket the declaration names.
pub(super) async fn absent_target_refusal(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    declared: &Value,
) -> Option<Refusal> {
    for (path, bucket) in declared_target_buckets(declared) {
        if ::service_common::s3_client::bucket_is_absent(ctx, region, &bucket).await {
            ::tracing::debug!(
                target = %path,
                bucket = %bucket,
                "a declared Glue crawler target names a bucket Amazon S3 does not hold"
            );
            return Some(absent_target(&path));
        }
    }
    None
}

/// The DISTINCT buckets the declared Amazon S3 targets name, each against the first target
/// path that named it, in the order the declaration states them and bounded by
/// [`MAX_RESOLVED_TARGET_BUCKETS`].
///
/// A declaration stating no Amazon S3 target, a target stating no path, and a path that
/// addresses no bucket at all name nothing to resolve: what such a path means is the owning
/// service's to decide, not this reading's. WHICH bucket a path names is read by the tree's
/// one reading of an `s3://` URI (`service_common::s3_uri::bucket_name`, "the bucket the
/// `s3://` URI names, with or without anything under it"), so a target is split exactly as
/// every other service splits one — by the maintained URL parser, and only for an authority
/// Amazon S3 could hold a bucket under.
fn declared_target_buckets(declared: &Value) -> Vec<(String, String)> {
    let mut named: Vec<(String, String)> = Vec::new();
    let targets = declared
        .get(CRAWLER_TARGETS_PROPERTY)
        .and_then(|targets| targets.get(S3_TARGETS_MEMBER))
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    for target in targets {
        if named.len() >= MAX_RESOLVED_TARGET_BUCKETS {
            break;
        }
        let Some(path) = declared_text(target, S3_TARGET_PATH_MEMBER) else {
            continue;
        };
        let Some(bucket) = ::service_common::s3_uri::bucket_name(&path) else {
            continue;
        };
        if named.iter().any(|(_, held)| *held == bucket) {
            continue;
        }
        named.push((path, bucket));
    }
    named
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// The `(path, bucket)` pairs one row expects a declaration to resolve, as
    /// [`declared_target_buckets`] answers them.
    type ResolvedTargets = Vec<(&'static str, &'static str)>;

    /// RULE: the resolution reads the DISTINCT bucket of each declared Amazon S3 target, in
    /// declaration order, and reads nothing from a declaration that names no target, a target
    /// that states no path, or a path that addresses no bucket.
    ///
    /// The rows differ along what the rule generalises over: how many targets a declaration
    /// states, whether a path names a prefix or a whole bucket, whether two targets share one
    /// bucket, and the forms that address no bucket at all.
    #[test]
    fn the_declared_targets_name_the_buckets_to_resolve() {
        // (case, the declared properties, the (path, bucket) pairs to resolve)
        let rows: &[(&str, Value, ResolvedTargets)] = &[
            (
                "one target naming a prefix",
                json!({ "Targets": { "S3Targets": [{ "Path": "s3://lake/data/" }] } }),
                vec![("s3://lake/data/", "lake")],
            ),
            (
                "a target naming a whole bucket",
                json!({ "Targets": { "S3Targets": [{ "Path": "s3://lake" }] } }),
                vec![("s3://lake", "lake")],
            ),
            (
                "two targets in two buckets",
                json!({ "Targets": { "S3Targets": [
                    { "Path": "s3://raw/in/" },
                    { "Path": "s3://curated/out/" },
                ] } }),
                vec![("s3://raw/in/", "raw"), ("s3://curated/out/", "curated")],
            ),
            (
                "two targets sharing one bucket",
                json!({ "Targets": { "S3Targets": [
                    { "Path": "s3://lake/one/" },
                    { "Path": "s3://lake/two/" },
                ] } }),
                vec![("s3://lake/one/", "lake")],
            ),
            (
                "a target beside targets of other stores",
                json!({ "Targets": {
                    "DynamoDBTargets": [{ "Path": "orders" }],
                    "S3Targets": [{ "Path": "s3://lake/data/" }],
                } }),
                vec![("s3://lake/data/", "lake")],
            ),
            (
                "a path that is no s3 location",
                json!({ "Targets": { "S3Targets": [{ "Path": "/mnt/data/" }] } }),
                vec![],
            ),
            (
                "a path naming no bucket",
                json!({ "Targets": { "S3Targets": [{ "Path": "s3:///data/" }] } }),
                vec![],
            ),
            (
                "a target stating no path",
                json!({ "Targets": { "S3Targets": [{ "Exclusions": ["*.tmp"] }] } }),
                vec![],
            ),
            (
                "an empty target list",
                json!({ "Targets": { "S3Targets": [] } }),
                vec![],
            ),
            ("a declaration stating no targets", json!({}), vec![]),
        ];
        for (case, declared, expected) in rows {
            let resolved = declared_target_buckets(declared);
            let actual: Vec<(&str, &str)> = resolved
                .iter()
                .map(|(path, bucket)| (path.as_str(), bucket.as_str()))
                .collect();
            assert_eq!(
                &actual, expected,
                "case {case:?}: expected the buckets {expected:?} to resolve, got {actual:?}"
            );
        }
    }

    /// RULE: the refusal for an absent target is the handler's own sentence naming the target,
    /// with the diagnostic of the answer the object store gave — the 404 a bucket it does not
    /// hold is answered with — filed under the code that answer carried.
    ///
    /// The rows differ along the target the declaration named, which is the sentence's one
    /// variable part.
    #[test]
    fn an_absent_target_is_reported_as_the_handler_words_it() {
        for case in [
            "s3://not-a-bucket-here/data/",
            "s3://another/deeper/prefix/",
            "s3://whole-bucket",
        ] {
            let refusal = absent_target(case);
            assert_eq!(
                refusal.code, BUCKET_NOT_FOUND,
                "case {case:?}: expected the code Amazon S3 answered, got {:?}",
                refusal.code
            );
            let opening = format!("S3 target {case} does not exist (Service: S3, Status Code: 404,");
            assert!(
                refusal.message.starts_with(&opening),
                "case {case:?}: expected a message opening {opening:?}, got {:?}",
                refusal.message
            );
        }
    }
}
