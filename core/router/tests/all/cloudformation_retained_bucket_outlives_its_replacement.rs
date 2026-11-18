// SPDX-License-Identifier: CC0-1.0

//! Replacing an `AWS::S3::Bucket` that declares `UpdateReplacePolicy: Retain`, where the bucket
//! being replaced still HOLDS objects.
//!
//! RULE under test: **an update that asks a resource for a value one of its create-only
//! properties cannot take REPLACES that resource, and what becomes of the OLD physical resource
//! is that resource's own `UpdateReplacePolicy` — under `Retain` CloudFormation asks NOTHING of
//! the owning service about it, so no delete of it is issued, it keeps its contents, and no
//! refusal such a delete would have drawn can fail the update; under any other value the owning
//! service deletes it.** Either way the update settles `UPDATE_COMPLETE`, the logical resource
//! publishes the NEW physical id, and the resource that replaced it carries the provenance tags
//! CloudFormation records on everything it provisions.
//!
//! Two crates have to agree for that to hold — CloudFormation decides whether a delete is
//! issued at all, and Amazon S3 is the service that holds the bucket with its objects and that
//! refuses the delete of a bucket still holding any ("the bucket you tried to delete is not
//! empty") — so the suite lives here rather than in either of them.
//!
//! Without the rule the retained bucket is deleted anyway, and because that refusal is exactly
//! what a bucket holding data draws, replacing such a bucket fails the whole update and rolls it
//! back: a template the attribute was written for cannot be deployed at all, and the client is
//! told its own data blocked the change.
//!
//! The rows differ along what the rule generalises over: the `UpdateReplacePolicy` the
//! declaration carries (`Retain`, and a declaration stating none, whose documented default is to
//! delete), how much the replaced bucket HOLDS (nothing, one object, several under nested
//! prefixes) — which is what decides whether a delete of it would be refused at all — and
//! whether an earlier replacement of the same stack was already refused and rolled back first,
//! so the attribute is read off the record that rollback restored rather than off a freshly
//! created one.
//!
//! Every assertion is a client's: the stack is read through `DescribeStacks`,
//! `DescribeStackResource` and `DescribeStackEvents`, and both buckets through Amazon S3's own
//! wire, so a bucket reported as retained is one that service really still holds and whose bytes
//! it really still answers with.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The awsQuery form encoder and the XML element readers this crate writes ONCE
// (`tests/all/query_wire.rs`).
use crate::query_wire::{blocks_of, form_encode, members_of, opt_element};

/// The Region every call here is signed for and addressed in. A Region outside Amazon S3's own
/// `us-east-1`, where `CreateBucket` on a bucket the caller already owns is refused rather than
/// answered 200 — which is what makes the refused replacement below a refusal.
const REGION: &str = "us-west-2";

/// The XML namespace a client's Amazon S3 payload carries.
const S3_XMLNS: &str = "http://s3.amazonaws.com/doc/2006-03-01/";

/// The CloudFormation API version an awsQuery body carries.
const CLOUDFORMATION_VERSION: &str = "2010-05-15";

/// The logical id the bucket is declared under, and the template parameter its `BucketName`
/// reads — the parameter is what makes a rename a change of the stack's INPUT rather than of its
/// template, which is how a client renames a resource in practice.
const LOGICAL_ID: &str = "Bucket";
const PARAMETER: &str = "BucketNameParam";

/// The value of `UpdateReplacePolicy` that keeps the old physical resource: "CloudFormation
/// keeps the resource without deleting the resource or its contents". Written out rather than
/// read off a crate constant: it is a word a template author writes, so a literal is what
/// catches the attribute being read under another spelling.
const RETAIN: &str = "Retain";

/// The provenance tag CloudFormation records on every resource it provisions, whose value is the
/// owning stack's name. Source: the `aws:cloudformation:*` tags CloudFormation's own
/// documentation names.
const STACK_NAME_TAG: &str = "aws:cloudformation:stack-name";

/// The statuses a stack operation can settle at, as `StackStatus` spells them: anything that is
/// not an operation in flight.
const IN_FLIGHT_SUFFIX: &str = "_IN_PROGRESS";

/// What a completed stack update reports, what a rolled-back one reports, and the two resource
/// statuses a replacement's cleanup phase can conclude with. Spelled out for the reason [`RETAIN`]
/// is: these are the words a client reads off the wire.
const UPDATE_COMPLETE: &str = "UPDATE_COMPLETE";
const UPDATE_ROLLBACK_COMPLETE: &str = "UPDATE_ROLLBACK_COMPLETE";
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const DELETE_SKIPPED: &str = "DELETE_SKIPPED";
const DELETE_COMPLETE: &str = "DELETE_COMPLETE";
const DELETE_FAILED: &str = "DELETE_FAILED";

/// How long a poll waits for a stack to reach a terminal status, and how often it looks. The
/// deadline is far longer than the span this emulator gives a stack's transitions, so a loaded
/// host cannot fail a case: what is waited FOR is the status appearing, never a length of time.
const SETTLE_DEADLINE: Duration = Duration::from_secs(60);
const SETTLE_POLL: Duration = Duration::from_millis(50);

/// CloudFormation and Amazon S3 over ONE shared engine, as `core/router/src/lib.rs` mounts them
/// — so the buckets a stack creates are the buckets this suite's own S3 calls reach.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        engine,
    ));
    router
}

/// A SigV4 `Authorization` header scoped to `service` in [`REGION`], which is what routes the
/// request and what each handler reads its Region from. The signature is never checked: this
/// router runs with request authentication off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=abc"
    )
}

/// One CloudFormation operation over its awsQuery wire.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(String, String)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={CLOUDFORMATION_VERSION}");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("cloudformation.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("cloudformation"))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (
        status,
        String::from_utf8_lossy(response.into_body().as_ref()).to_string(),
    )
}

/// One request member, as the pair [`cloudformation`] takes.
fn member(key: &str, value: &str) -> (String, String) {
    (key.to_string(), value.to_string())
}

/// The `Parameters` members a call carries for one key, indexed from 1 as an AWS SDK sends them.
fn parameter(value: &str) -> Vec<(String, String)> {
    vec![
        member("Parameters.member.1.ParameterKey", PARAMETER),
        member("Parameters.member.1.ParameterValue", value),
    ]
}

/// One Amazon S3 REST call, path-style as a client makes it against the emulator's front door.
async fn s3(router: &MultiServiceRouter, method: &str, path: &str, body: &[u8]) -> (u16, String) {
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://s3.{REGION}.amazonaws.com{path}"))
        .header("host", format!("s3.{REGION}.amazonaws.com"))
        .header("authorization", authorization("s3"))
        .body(Bytes::copy_from_slice(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (
        status,
        String::from_utf8_lossy(response.into_body().as_ref()).to_string(),
    )
}

/// Poll `DescribeStacks` the way a client's waiter does and answer the `StackStatus` the stack
/// SETTLED on — the first status that is not an operation in flight.
async fn settled_status(case: &str, router: &MultiServiceRouter, stack: &str) -> String {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    let mut seen: Vec<String> = Vec::new();
    loop {
        let (code, body) =
            cloudformation(router, "DescribeStacks", &[member("StackName", stack)]).await;
        assert_eq!(
            code, 200,
            "case {case:?}: DescribeStacks({stack}) must answer 200, got {code} {body}"
        );
        let reported = opt_element(&body, "StackStatus").unwrap_or_default();
        if !reported.is_empty() && !reported.ends_with(IN_FLIGHT_SUFFIX) {
            return reported;
        }
        if seen.last().map(String::as_str) != Some(reported.as_str()) {
            seen.push(reported);
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {stack} to settle, saw {seen:?}"
        );
        tokio::time::sleep(SETTLE_POLL).await;
    }
}

/// The `PhysicalResourceId` the stack records for the bucket right now.
async fn recorded_physical_id(case: &str, router: &MultiServiceRouter, stack: &str) -> String {
    let (code, body) = cloudformation(
        router,
        "DescribeStackResource",
        &[
            member("StackName", stack),
            member("LogicalResourceId", LOGICAL_ID),
        ],
    )
    .await;
    assert_eq!(
        code, 200,
        "case {case:?}: the bucket's record must be readable, got {code} {body}"
    );
    opt_element(&body, "PhysicalResourceId").unwrap_or_default()
}

/// One stack event, reduced to the three members this suite reads.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Event {
    logical_id: String,
    physical_id: String,
    status: String,
}

/// Every event the stack has recorded, OLDEST FIRST. `DescribeStackEvents` answers "events …
/// with the most recently added events first", so the list is reversed.
async fn stack_events(case: &str, router: &MultiServiceRouter, stack: &str) -> Vec<Event> {
    let (code, body) =
        cloudformation(router, "DescribeStackEvents", &[member("StackName", stack)]).await;
    assert_eq!(
        code, 200,
        "case {case:?}: DescribeStackEvents({stack}) must answer 200, got {code} {body}"
    );
    let mut events: Vec<Event> = members_of(&body)
        .into_iter()
        .map(|event| Event {
            logical_id: opt_element(event, "LogicalResourceId").unwrap_or_default(),
            physical_id: opt_element(event, "PhysicalResourceId").unwrap_or_default(),
            status: opt_element(event, "ResourceStatus").unwrap_or_default(),
        })
        .collect();
    events.reverse();
    events
}

/// The tag set Amazon S3 reports for `bucket`, as the map a client reads it as. The empty map is
/// a bucket carrying no tag set, which Amazon S3 answers `NoSuchTagSet` for.
async fn bucket_tags(router: &MultiServiceRouter, bucket: &str) -> BTreeMap<String, String> {
    let (status, body) = s3(router, "GET", &format!("/{bucket}?tagging"), &[]).await;
    if status != 200 {
        return BTreeMap::new();
    }
    blocks_of(&body, "Tag")
        .into_iter()
        .filter_map(|entry| Some((opt_element(entry, "Key")?, opt_element(entry, "Value")?)))
        .collect()
}

/// Whether Amazon S3 still holds `bucket`, as `HEAD /<bucket>` answers it.
async fn still_held(router: &MultiServiceRouter, bucket: &str) -> bool {
    s3(router, "HEAD", &format!("/{bucket}"), &[]).await.0 == 200
}

/// The template one revision deploys: the bucket named by the parameter, carrying `policy` as its
/// `UpdateReplacePolicy` when the row declares one. `BucketName` is create-only — Amazon S3
/// publishes no call that renames a bucket — so a revision asking for another name asks for a
/// resource the one that exists cannot become.
fn template(policy: Option<&str>) -> String {
    let mut declaration = json!({
        "Type": "AWS::S3::Bucket",
        "Properties": {"BucketName": {"Ref": "BucketNameParam"}},
    });
    if let (Some(policy), Some(members)) = (policy, declaration.as_object_mut()) {
        members.insert(
            "UpdateReplacePolicy".to_string(),
            Value::String(policy.to_string()),
        );
    }
    json!({
        "Parameters": {"BucketNameParam": {"Type": "String"}},
        "Resources": {"Bucket": declaration},
    })
    .to_string()
}

/// One row: the declaration's `UpdateReplacePolicy`, what the bucket holds when it is replaced,
/// and whether an earlier replacement of the same stack was refused and rolled back first.
struct Row {
    case: &'static str,
    /// The segment the row's stack and bucket names are built from, so no two rows collide.
    slug: &'static str,
    /// `None` is a declaration that states no `UpdateReplacePolicy` at all.
    policy: Option<&'static str>,
    /// The `(key, bytes)` pairs written into the bucket before it is replaced.
    held: &'static [(&'static str, &'static str)],
    /// Whether the row first renames onto a bucket somebody else already owns, which the stack
    /// cannot create and therefore rolls back.
    refused_rename_first: bool,
    /// Whether the replaced bucket must survive the update — true exactly for `Retain`.
    retains_the_old: bool,
}

/// The rows: the `Retain` declaration over the amounts a bucket can hold and over the two ways
/// its record can have reached the replacing update, plus the declaration that states no policy,
/// whose documented default deletes the old resource.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a retained bucket holding one object",
            slug: "one-object",
            policy: Some(RETAIN),
            held: &[("doc.txt", "the bytes a rename must not touch")],
            refused_rename_first: false,
            retains_the_old: true,
        },
        Row {
            case: "a retained bucket holding several objects under nested prefixes",
            slug: "many-objects",
            policy: Some(RETAIN),
            held: &[
                ("top.txt", "the first"),
                ("nested/second.txt", "the second"),
                ("nested/deeper/third.bin", "the third"),
            ],
            refused_rename_first: false,
            retains_the_old: true,
        },
        Row {
            case: "a retained bucket holding nothing at all",
            slug: "empty",
            policy: Some(RETAIN),
            held: &[],
            refused_rename_first: false,
            retains_the_old: true,
        },
        Row {
            case: "a retained bucket holding an object, replaced after an earlier replacement of \
                   the same stack was refused and rolled back",
            slug: "after-a-rollback",
            policy: Some(RETAIN),
            held: &[("doc.txt", "the bytes a rolled-back rename must not touch")],
            refused_rename_first: true,
            retains_the_old: true,
        },
        Row {
            case: "an empty bucket replaced with no UpdateReplacePolicy declared",
            slug: "no-policy",
            policy: None,
            held: &[],
            refused_rename_first: false,
            retains_the_old: false,
        },
    ]
}

/// RULE: see the module header — a replacement's old physical resource is governed by its own
/// `UpdateReplacePolicy`, and `Retain` leaves it, and everything it holds, alone.
#[tokio::test]
async fn a_retained_bucket_and_its_objects_outlive_the_replacement_that_renamed_it() {
    for row in rows() {
        let Row {
            case,
            slug,
            policy,
            held,
            refused_rename_first,
            retains_the_old,
        } = row;
        let router = emulator();
        let stack = format!("retained-replacement-{slug}");
        let first = format!("retained-replacement-{slug}-first");
        let second = format!("retained-replacement-{slug}-second");
        let owned_elsewhere = format!("retained-replacement-{slug}-owned");
        let body = template(policy);

        // A bucket nobody's stack manages, holding an object of its own: the name the refused
        // replacement below asks for, so what that replacement runs into is a name already taken.
        if refused_rename_first {
            let configuration = format!(
                "<CreateBucketConfiguration xmlns=\"{S3_XMLNS}\">\
                 <LocationConstraint>{REGION}</LocationConstraint></CreateBucketConfiguration>"
            );
            let (status, answer) = s3(
                &router,
                "PUT",
                &format!("/{owned_elsewhere}"),
                configuration.as_bytes(),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: the bucket owned outside the stack must be created, got {status} \
                 {answer}"
            );
            let (status, answer) = s3(
                &router,
                "PUT",
                &format!("/{owned_elsewhere}/outside.txt"),
                b"the bytes of a bucket no stack manages",
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: the object outside the stack must be written, got {status} \
                 {answer}"
            );
        }

        // The revision that CREATES the bucket, under the first name.
        let mut members = vec![
            member("StackName", &stack),
            member("TemplateBody", &body),
        ];
        members.extend(parameter(&first));
        let (code, answer) = cloudformation(&router, "CreateStack", &members).await;
        assert_eq!(
            code, 200,
            "case {case:?}: the create must be accepted, got {code} {answer}"
        );
        let created = settled_status(case, &router, &stack).await;
        assert_eq!(
            created, CREATE_COMPLETE,
            "case {case:?}: expected the create to complete, got {created:?}"
        );
        assert!(
            still_held(&router, &first).await,
            "case {case:?}: expected Amazon S3 to hold {first:?} after the create"
        );

        // ...and what the bucket HOLDS when it is replaced, written by a client of that bucket.
        for (key, bytes) in held {
            let (status, answer) =
                s3(&router, "PUT", &format!("/{first}/{key}"), bytes.as_bytes()).await;
            assert_eq!(
                status, 200,
                "case {case:?}: the write of {key:?} must be accepted, got {status} {answer}"
            );
        }

        // The replacement a row asks for FIRST, onto a name another owner already holds: nothing
        // the stack can create, so it rolls back and the bucket it manages is still the first one.
        if refused_rename_first {
            let mut refused = vec![
                member("StackName", &stack),
                member("UsePreviousTemplate", "true"),
            ];
            refused.extend(parameter(&owned_elsewhere));
            let (code, answer) = cloudformation(&router, "UpdateStack", &refused).await;
            assert_eq!(
                code, 200,
                "case {case:?}: the refused replacement must be accepted, got {code} {answer}"
            );
            let rolled_back = settled_status(case, &router, &stack).await;
            assert_eq!(
                rolled_back, UPDATE_ROLLBACK_COMPLETE,
                "case {case:?}: expected the replacement onto an owned name to roll back, got \
                 {rolled_back:?}"
            );
            let recorded = recorded_physical_id(case, &router, &stack).await;
            assert_eq!(
                recorded, first,
                "case {case:?}: expected the rolled-back stack to still record {first:?}, got \
                 {recorded:?}"
            );
            // ...and the bucket it could not take is left exactly as its own owner left it: the
            // stack never created it, so its rollback owes no call about it.
            let (status, answer) = s3(
                &router,
                "GET",
                &format!("/{owned_elsewhere}/outside.txt"),
                &[],
            )
            .await;
            assert_eq!(
                (status, answer.as_str()),
                (200, "the bytes of a bucket no stack manages"),
                "case {case:?}: expected the bucket owned outside the stack to be untouched by \
                 the rollback, got {status} {answer:?}"
            );
        }

        // The replacement under test: a name nobody holds, which the bucket that exists cannot
        // take, so the stack creates a new one and the OLD one's fate is its own attribute.
        let mut replacing = vec![
            member("StackName", &stack),
            member("UsePreviousTemplate", "true"),
        ];
        replacing.extend(parameter(&second));
        let (code, answer) = cloudformation(&router, "UpdateStack", &replacing).await;
        assert_eq!(
            code, 200,
            "case {case:?}: the replacement must be accepted, got {code} {answer}"
        );
        let updated = settled_status(case, &router, &stack).await;
        assert_eq!(
            updated, UPDATE_COMPLETE,
            "case {case:?}: expected the replacement to complete, got {updated:?}"
        );

        // The stack publishes the resource that replaced the old one...
        let recorded = recorded_physical_id(case, &router, &stack).await;
        assert_eq!(
            recorded, second,
            "case {case:?}: expected the stack to record the replacement {second:?}, got \
             {recorded:?}"
        );
        assert!(
            still_held(&router, &second).await,
            "case {case:?}: expected Amazon S3 to hold the replacement {second:?}"
        );
        // ...carrying the provenance CloudFormation records on every resource it provisions, so
        // the replacement is a resource of this stack and not merely a bucket of the same name.
        let tags = bucket_tags(&router, &second).await;
        assert_eq!(
            tags.get(STACK_NAME_TAG).map(String::as_str),
            Some(stack.as_str()),
            "case {case:?}: expected the replacement to carry {STACK_NAME_TAG}={stack}, got \
             {tags:?}"
        );

        // ...and the bucket it replaced is exactly where its attribute leaves it, with everything
        // it held still readable through Amazon S3's own wire.
        let old_is_there = still_held(&router, &first).await;
        assert_eq!(
            old_is_there, retains_the_old,
            "case {case:?}: expected Amazon S3 to {} {first:?} after the replacement, it {}",
            if retains_the_old {
                "still hold"
            } else {
                "no longer hold"
            },
            if old_is_there { "does" } else { "does not" }
        );
        if retains_the_old {
            for (key, bytes) in held {
                let (status, answer) = s3(&router, "GET", &format!("/{first}/{key}"), &[]).await;
                assert_eq!(
                    (status, answer.as_str()),
                    (200, *bytes),
                    "case {case:?}: expected {key:?} of the retained {first:?} to still answer \
                     with its bytes, got {status} {answer:?}"
                );
            }
        }

        // What the event stream says about the old physical resource: the cleanup phase SKIPPED
        // its deletion where the attribute retains it, and completed it otherwise...
        let events = stack_events(case, &router, &stack).await;
        let about_the_old: Vec<&Event> = events
            .iter()
            .filter(|event| event.logical_id == LOGICAL_ID && event.physical_id == first)
            .collect();
        let concluded = about_the_old
            .last()
            .unwrap_or_else(|| panic!("case {case:?}: expected events naming {first:?}, got {events:?}"));
        let owed = if retains_the_old {
            DELETE_SKIPPED
        } else {
            DELETE_COMPLETE
        };
        assert_eq!(
            concluded.status, owed,
            "case {case:?}: expected the cleanup of {first:?} to conclude {owed}, got \
             {about_the_old:?}"
        );
        // ...and nothing anywhere in this stack's history reports a deletion that FAILED: a
        // delete that is never issued cannot be refused, and the one that is goes to a bucket
        // whose owning service has nothing to object to.
        let failures: Vec<&Event> = events
            .iter()
            .filter(|event| event.status == DELETE_FAILED)
            .collect();
        assert!(
            failures.is_empty(),
            "case {case:?}: expected no failed deletion in the stack's history, got {failures:?}"
        );
    }
}
