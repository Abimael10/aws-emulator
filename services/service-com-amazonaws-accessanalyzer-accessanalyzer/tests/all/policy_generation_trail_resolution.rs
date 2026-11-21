// SPDX-License-Identifier: CC0-1.0

//! Which TRAIL a policy generation may analyze: `StartPolicyGeneration` against the
//! CloudTrail of this same emulator, driven over the real router.
//!
//! RULE: `cloudTrailDetails.trails[].cloudTrailArn` "specifies the ARN of the trail" a
//! generation analyzes (`smithy_query(query="shape",shape="Trail,CloudTrailDetails")`), and a
//! trail is CloudTrail's resource, not this service's. So a start RESOLVES every ARN it names
//! where CloudTrail holds it, synchronously and before any job exists: a trail that service
//! holds is accepted and the job is minted, and one it does not hold is refused with the
//! modelled `ValidationException` (HTTP 400) — `Invalid cloudTrailArn: <the ARN sent>. Unknown
//! trail: <the trail name that ARN carries> for the user: <the account>` — with nothing
//! recorded.
//!
//! The rows span what the rule generalises over: how many trails a request names and WHICH
//! of them CloudTrail does not hold, the form of the trail name the ARN carries, the Region
//! the trail is homed in versus the one the start is signed for, a trail that existed and was
//! then deleted (existence is read when the start is made, not once), and the ceiling on how
//! many trails one start resolves, at its bound and one past it.
//!
//! Every trail is created through CloudTrail's OWN API and the ARN the generation names is the
//! one that create reported, so a trail is believed only where the service that owns it says
//! so. Its log-delivery destination — a bucket whose policy lets CloudTrail write — is
//! recorded in the account's object store first, which is what a client does with
//! `CreateBucket` and `PutBucketPolicy` before pointing a trail anywhere.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_accessanalyzer_accessanalyzer::state::MAX_RESOLVED_TRAILS;

use crate::harness::{role_arn, wire_reason, Emulator, Reply, REGION};

/// The status a success that reports content carries.
const OK: u16 = 200;

/// The status every refusal of this operation carries.
const BAD_REQUEST: u16 = 400;

/// The error code of that refusal.
const VALIDATION: &str = "ValidationException";

/// CloudTrail's signing name, which is what routes a call to it.
const CLOUD_TRAIL_SIGNING_NAME: &str = "cloudtrail";

/// The `X-Amz-Target` prefix every SDK sends for CloudTrail's `awsJson1_1` API.
const CLOUD_TRAIL_TARGET_PREFIX: &str = "com.amazonaws.cloudtrail.v20131101.CloudTrail_20131101.";

/// The Region a trail is created in unless a row names another.
const TRAIL_REGION: &str = REGION;

/// Another Region of the same account, for the row whose trail is homed away from the Region
/// the start is signed for.
const OTHER_REGION: &str = "us-west-2";

/// The bucket every trail below delivers into.
const LOG_BUCKET: &str = "accessanalyzer-trail-logs";

/// A CloudTrail trail ARN as a CLIENT writes one, in the form `CloudTrailArn`'s pattern
/// requires (`^arn:[^:]*:cloudtrail:[^:]*:[^:]*:trail/.{1,576}$`).
///
/// Used for the trails CloudTrail does NOT hold — a trail it does hold is named by the ARN its
/// own create reported.
fn unheld_trail_arn(region: &str, name: &str) -> String {
    format!(
        "arn:aws:cloudtrail:{region}:{}:trail/{name}",
        engine::emulator_account_id()
    )
}

/// The bucket policy a client attaches so CloudTrail may deliver a trail's log files into
/// `bucket`: the ACL check on the bucket and the object write under it, granted to the
/// CloudTrail service principal, which is the policy CloudTrail's documentation states.
fn log_delivery_policy(bucket: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [
            {
                "Sid": "AWSCloudTrailAclCheck",
                "Effect": "Allow",
                "Principal": { "Service": "cloudtrail.amazonaws.com" },
                "Action": "s3:GetBucketAcl",
                "Resource": format!("arn:aws:s3:::{bucket}"),
            },
            {
                "Sid": "AWSCloudTrailWrite",
                "Effect": "Allow",
                "Principal": { "Service": "cloudtrail.amazonaws.com" },
                "Action": "s3:PutObject",
                "Resource": format!("arn:aws:s3:::{bucket}/*"),
                "Condition": {
                    "StringEquals": { "s3:x-amz-acl": "bucket-owner-full-control" },
                },
            },
        ],
    })
    .to_string()
}

/// One emulator with this service and CloudTrail mounted on the same engine, with the
/// log-delivery destination every trail below names already recorded in the account's object
/// store.
///
/// The object store holds ONE bucket namespace for the account and resolves a destination by
/// name, so the same bucket serves a trail created in any Region.
fn emulator_with_cloud_trail() -> Emulator {
    let (emulator, engine) =
        Emulator::beside(service_com_amazonaws_cloudtrail_cloudtrail_20131101::EmulatedService::new);
    record_log_bucket(&engine, LOG_BUCKET, TRAIL_REGION);
    emulator
}

/// Record `bucket` in the account's one object store, carrying the log-delivery policy —
/// the `CreateBucket` plus `PutBucketPolicy` a client makes before pointing a trail at it.
fn record_log_bucket(engine: &Arc<engine::InMemoryEngine>, bucket: &str, region: &str) {
    let state = service_com_amazonaws_s3_amazons3::state::service_state(&**engine)
        .expect("the in-memory engine carries the object store");
    let mut store = state.lock();
    let _ = store.create_bucket(bucket, region);
    if let Some(held) = store.bucket_mut(bucket) {
        held.policy = Some(log_delivery_policy(bucket));
    }
}

/// Create a trail through CloudTrail's own API and answer the `TrailARN` it reported.
async fn create_trail(emulator: &Emulator, case: &str, region: &str, name: &str) -> String {
    let reply = emulator
        .send_rpc(
            CLOUD_TRAIL_SIGNING_NAME,
            &format!("{CLOUD_TRAIL_TARGET_PREFIX}CreateTrail"),
            region,
            json!({ "Name": name, "S3BucketName": LOG_BUCKET }),
        )
        .await;
    assert_eq!(
        (reply.status, reply.code()),
        (OK, ""),
        "case {case:?}: CloudTrail must create the trail {name:?} in {region}, got {reply:?}"
    );
    reply
        .member("TrailARN")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!("case {case:?}: CreateTrail must report a TrailARN, got {reply:?}")
        })
        .to_string()
}

/// Delete a trail through CloudTrail's own API.
async fn delete_trail(emulator: &Emulator, case: &str, region: &str, trail_arn: &str) {
    let reply = emulator
        .send_rpc(
            CLOUD_TRAIL_SIGNING_NAME,
            &format!("{CLOUD_TRAIL_TARGET_PREFIX}DeleteTrail"),
            region,
            json!({ "Name": trail_arn }),
        )
        .await;
    assert!(
        reply.status < 300,
        "case {case:?}: CloudTrail must delete {trail_arn}, got {reply:?}"
    );
}

/// A `StartPolicyGeneration` body naming `trails`, over a range that is a range.
fn start_body(principal: &str, trails: &[String]) -> Value {
    json!({
        "policyGenerationDetails": { "principalArn": principal },
        "cloudTrailDetails": {
            "trails": trails
                .iter()
                .map(|arn| json!({ "cloudTrailArn": arn }))
                .collect::<Vec<_>>(),
            "accessRole": role_arn("AccessAnalyzerMonitorServiceRole"),
            "startTime": "2026-01-01T00:00:00Z",
            "endTime": "2026-01-02T00:00:00Z",
        },
    })
}

/// The message AWS words a trail-existence refusal with, for the ARN the request sent.
///
/// Its three variable parts come from the request and the caller's identity: the ARN exactly
/// as sent, the trail name that ARN carries (the part after `trail/`), and the calling
/// account.
fn unknown_trail_message(cloud_trail_arn: &str) -> String {
    let name = cloud_trail_arn
        .rsplit("trail/")
        .next()
        .expect("a trail ARN carries a name after its resource type");
    format!(
        "Invalid cloudTrailArn: {cloud_trail_arn}. Unknown trail: {name} for the user: {}",
        engine::emulator_account_id()
    )
}

/// Every `jobId` a listing reports.
fn listed_ids(reply: &Reply) -> Vec<String> {
    reply
        .member("policyGenerations")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(|item| {
                    item.get("jobId")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string()
                })
                .collect()
        })
        .unwrap_or_default()
}

/// One row of the accepted table: which trails to create through CloudTrail, each as the
/// Region it is homed in and its name, and the suffix that keeps the row's principal its
/// own.
struct HeldCase {
    /// What this row varies, named in every assertion.
    case: &'static str,
    trails: &'static [(&'static str, &'static str)],
    suffix: &'static str,
}

/// RULE: a start naming only trails CloudTrail HOLDS is accepted, and the job it minted is the
/// one a later read resolves.
///
/// The rows differ along how many trails the request names, the form of the trail name, and
/// the Region the trail is homed in versus the one the start is signed for.
#[tokio::test]
async fn a_start_naming_trails_cloudtrail_holds_is_served() {
    let emulator = emulator_with_cloud_trail();

    let rows: [HeldCase; 4] = [
        HeldCase {
            case: "one trail of this Region",
            trails: &[(TRAIL_REGION, "pgtr-one")],
            suffix: "one",
        },
        HeldCase {
            case: "two trails, both held",
            trails: &[(TRAIL_REGION, "pgtr-first"), (TRAIL_REGION, "pgtr-second")],
            suffix: "two",
        },
        HeldCase {
            case: "a trail name carrying the separators a trail name may hold",
            trails: &[(TRAIL_REGION, "org.pgtr-audit_2026")],
            suffix: "separators",
        },
        HeldCase {
            case: "a trail homed in another Region, named by its ARN",
            trails: &[(OTHER_REGION, "pgtr-elsewhere")],
            suffix: "elsewhere",
        },
    ];

    for HeldCase {
        case,
        trails,
        suffix,
    } in rows
    {
        let mut arns = Vec::new();
        for (region, name) in trails {
            arns.push(create_trail(&emulator, case, region, name).await);
        }
        let principal = role_arn(&format!("pgtr_ok_{suffix}"));
        let started = emulator
            .start_policy_generation(start_body(&principal, &arns))
            .await;
        assert_eq!(
            (started.status, started.code()),
            (OK, ""),
            "case {case:?}: expected StartPolicyGeneration to answer {OK}, got {started:?}"
        );
        let job_id = started
            .member("jobId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        assert!(
            !job_id.is_empty(),
            "case {case:?}: expected a jobId in the response, got {started:?}"
        );

        // The job exists where a later caller reads it, and reports the very trails the
        // request named.
        let read = emulator.get_generated_policy(&job_id, "").await;
        assert_eq!(
            (read.status, read.code()),
            (OK, ""),
            "case {case:?}: expected GetGeneratedPolicy to answer {OK}, got {read:?}"
        );
        let reported: Vec<String> = read
            .member("generatedPolicyResult")
            .and_then(|result| result.get("properties"))
            .and_then(|properties| properties.get("cloudTrailProperties"))
            .and_then(|cloud_trail| cloud_trail.get("trailProperties"))
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .map(|item| {
                        item.get("cloudTrailArn")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string()
                    })
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(
            reported, arns,
            "case {case:?}: expected the generation to report the trails the request named, \
             got {read:?}"
        );
    }
}

/// One row of the refused table: the trails the request names, the ARN the refusal must be
/// worded for, and the suffix that keeps the row's principal its own.
struct UnheldCase {
    /// What this row varies, named in every assertion.
    case: &'static str,
    trails: Vec<String>,
    refused_arn: String,
    suffix: &'static str,
}

/// RULE: a start naming a trail CloudTrail does NOT hold is refused with the modelled
/// `ValidationException`, worded for that ARN, and records nothing.
///
/// The rows differ along WHICH entry of the list is the unknown one, the form of the trail
/// name the refusal has to report, the Region the ARN names, and whether the trail never
/// existed or was deleted after it did.
#[tokio::test]
async fn a_start_naming_a_trail_cloudtrail_does_not_hold_is_refused() {
    let emulator = emulator_with_cloud_trail();

    // A trail that exists, so a row can put the unknown one BESIDE a known one.
    let held = create_trail(&emulator, "setup", TRAIL_REGION, "pgtr-refuse-held").await;
    // A trail that existed and was then deleted: existence is read when the start is made.
    let deleted = create_trail(&emulator, "setup", TRAIL_REGION, "pgtr-refuse-deleted").await;
    delete_trail(&emulator, "setup", TRAIL_REGION, &deleted).await;

    let rows: Vec<UnheldCase> = vec![
        UnheldCase {
            case: "the only trail named was never created",
            trails: vec![unheld_trail_arn(TRAIL_REGION, "pgtr-notrail")],
            refused_arn: unheld_trail_arn(TRAIL_REGION, "pgtr-notrail"),
            suffix: "only",
        },
        UnheldCase {
            case: "the SECOND of two trails is the unknown one",
            trails: vec![
                held.clone(),
                unheld_trail_arn(TRAIL_REGION, "pgtr-second-notrail"),
            ],
            refused_arn: unheld_trail_arn(TRAIL_REGION, "pgtr-second-notrail"),
            suffix: "second",
        },
        UnheldCase {
            case: "an unknown trail whose name carries the separators a trail name may hold",
            trails: vec![unheld_trail_arn(TRAIL_REGION, "org.pgtr-missing_2026")],
            refused_arn: unheld_trail_arn(TRAIL_REGION, "org.pgtr-missing_2026"),
            suffix: "separators",
        },
        UnheldCase {
            case: "an unknown trail of another Region",
            trails: vec![unheld_trail_arn(OTHER_REGION, "pgtr-notrail-elsewhere")],
            refused_arn: unheld_trail_arn(OTHER_REGION, "pgtr-notrail-elsewhere"),
            suffix: "elsewhere",
        },
        UnheldCase {
            case: "a trail CloudTrail held and no longer holds",
            trails: vec![deleted.clone()],
            refused_arn: deleted.clone(),
            suffix: "deleted",
        },
    ];

    for UnheldCase {
        case,
        trails,
        refused_arn,
        suffix,
    } in rows
    {
        let principal = role_arn(&format!("pgtr_refuse_{suffix}"));
        let reply = emulator
            .start_policy_generation(start_body(&principal, &trails))
            .await;
        assert_eq!(
            (reply.status, reply.code()),
            (BAD_REQUEST, VALIDATION),
            "case {case:?}: expected {BAD_REQUEST} {VALIDATION}, got {reply:?}"
        );
        assert_eq!(
            reply.member("message").and_then(Value::as_str),
            Some(unknown_trail_message(&refused_arn).as_str()),
            "case {case:?}: expected the refusal worded for {refused_arn}, got {reply:?}"
        );
        assert_eq!(
            reply.member("reason").and_then(Value::as_str),
            Some(wire_reason("fieldValidationFailed")),
            "case {case:?}: expected the modelled reason, got {reply:?}"
        );
        let blamed: Vec<&str> = reply
            .member("fieldList")
            .and_then(Value::as_array)
            .map(|fields| {
                fields
                    .iter()
                    .filter_map(|field| field.get("name").and_then(Value::as_str))
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(
            blamed,
            vec!["cloudTrailDetails.trails.cloudTrailArn"],
            "case {case:?}: expected the refusal to blame the trail ARN member, got {reply:?}"
        );

        // Refused synchronously means no job exists to track.
        let listed = emulator
            .list_policy_generations(&format!("?principalArn={principal}"))
            .await;
        assert_eq!(
            (listed.status, listed.code()),
            (OK, ""),
            "case {case:?}: expected ListPolicyGenerations to answer {OK}, got {listed:?}"
        );
        assert_eq!(
            listed_ids(&listed),
            Vec::<String>::new(),
            "case {case:?}: a refused start records no job, got {listed:?}"
        );
    }
}

/// RULE: the number of trails one start resolves is bounded, so a request cannot drive
/// unbounded work by naming trails — and the bound is on the DISTINCT ARNs, since the same
/// trail is one question however many times the list repeats it.
///
/// The rows sit at the published bound ([`MAX_RESOLVED_TRAILS`]) and one past it, which is
/// what the ceiling has to be asserted at.
#[tokio::test]
async fn the_trails_one_start_resolves_are_bounded() {
    let emulator = emulator_with_cloud_trail();
    let held = create_trail(&emulator, "setup", TRAIL_REGION, "pgtr-bound-held").await;

    // AT the bound: the list is as long as the ceiling allows, and names ONE trail over and
    // over, so a resolution per distinct ARN is a single question.
    let at_bound: Vec<String> = ::std::iter::repeat_n(held.clone(), MAX_RESOLVED_TRAILS).collect();
    let principal = role_arn("pgtr_bound_at");
    let served = emulator
        .start_policy_generation(start_body(&principal, &at_bound))
        .await;
    assert_eq!(
        (served.status, served.code()),
        (OK, ""),
        "case \"at the bound\": expected a list of {MAX_RESOLVED_TRAILS} trails to be served, \
         got {served:?}"
    );

    // ONE PAST it: refused before any trail is resolved, naming the count and the ceiling,
    // and nothing is recorded.
    let past_bound: Vec<String> =
        ::std::iter::repeat_n(held, MAX_RESOLVED_TRAILS + 1).collect();
    let principal = role_arn("pgtr_bound_past");
    let refused = emulator
        .start_policy_generation(start_body(&principal, &past_bound))
        .await;
    assert_eq!(
        (refused.status, refused.code()),
        (BAD_REQUEST, VALIDATION),
        "case \"one past the bound\": expected {BAD_REQUEST} {VALIDATION}, got {refused:?}"
    );
    assert_eq!(
        refused.member("message").and_then(Value::as_str),
        Some(
            format!(
                "cloudTrailDetails.trails names {} trails, more than the {MAX_RESOLVED_TRAILS} \
                 this emulator resolves for one policy generation",
                MAX_RESOLVED_TRAILS + 1
            )
            .as_str()
        ),
        "case \"one past the bound\": expected the refusal to name the count and the ceiling, \
         got {refused:?}"
    );
    let listed = emulator
        .list_policy_generations(&format!("?principalArn={principal}"))
        .await;
    assert_eq!(
        listed_ids(&listed),
        Vec::<String>::new(),
        "case \"one past the bound\": a refused start records no job, got {listed:?}"
    );
}

/// RULE's other side: with no CloudTrail to ask, NOTHING is established about a trail, so a
/// start is served exactly as it was before the resolution existed — the refusal is grounded
/// in CloudTrail's own answer, never in this service's failure to reach it.
#[tokio::test]
async fn a_start_is_served_where_no_cloudtrail_answers_about_the_trail() {
    let emulator = Emulator::new();
    let principal = role_arn("pgtr_unresolved");
    let reply = emulator
        .start_policy_generation(start_body(
            &principal,
            &[unheld_trail_arn(TRAIL_REGION, "pgtr-unresolvable")],
        ))
        .await;
    assert_eq!(
        (reply.status, reply.code()),
        (OK, ""),
        "expected a start to be served where no CloudTrail answers, got {reply:?}"
    );
    assert!(
        reply
            .member("jobId")
            .and_then(Value::as_str)
            .is_some_and(|job_id| !job_id.is_empty()),
        "expected a jobId in the response, got {reply:?}"
    );
}
