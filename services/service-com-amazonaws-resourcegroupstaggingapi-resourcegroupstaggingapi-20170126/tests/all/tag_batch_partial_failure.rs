// SPDX-License-Identifier: CC0-1.0

//! The BATCH contract of `TagResources` and `UntagResources`: both answer HTTP 200 and
//! report the outcome PER RESOURCE: a request mixing ARNs an owning service resolves with
//! ARNs it does not names exactly the unresolvable ones in `FailedResourcesMap` — each
//! carrying that service's own `ErrorCode`, `ErrorMessage` and `StatusCode` — while every
//! resolvable ARN of the SAME request is written.
//!
//! The sibling suite [`crate::elb_tagged_resource_index`] spans the resource KINDS an
//! Elastic Load Balancing ARN can name; this one spans the shape of the BATCH — how many
//! ARNs it carries, how many of them resolve, and where in the list those sit — because
//! the selection a caller reads out of `FailedResourcesMap` is keyed by ARN, not by
//! position, and an entry that is absent from the map is a claim that the write landed.
//!
//! The harness (one engine carrying this API and Elastic Load Balancing, the awsJson and
//! awsQuery wires, the owning service's own tag reader) is that suite's, reused rather
//! than copied.
//!
//! The second half of the suite spans the other dimension of the same batch: WHICH SERVICE
//! owns each ARN. A `FailedResourcesMap` entry carries the refusal of the service that
//! hosts that one resource, so a batch addressing several owning services at once answers
//! one entry per unresolvable ARN, each with its own owner's status, code and sentence, and
//! none for the ARNs that resolved.
//!
//! The last part spans a batch whose ARNs belong to two owners that each keep their
//! resources' OWN tag sets — CloudWatch Logs and AWS Lambda — because that is where the two
//! halves of the contract meet: the same call that earns one entry per unresolvable ARN
//! writes the tag into each live resource's own tag set, so one key written across two
//! services is afterwards selected by one `TagFilters` entry.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use serde_json::{json, Value};

use service_com_amazonaws_cloudwatchlogs_logs_20140328 as owner_logs;
use service_com_amazonaws_lambda_awsgirapiservice as owner_lambda;
use service_com_amazonaws_resourcegroupstaggingapi_resourcegroupstaggingapi_20170126::EmulatedService as TaggingService;

use crate::elb_tagged_resource_index::{
    account, action, elb, elb_arn, elb_ok, elb_tags, emulator, expected, p, page_arns, page_tags,
    require, resources_for, tag_fields, tagging, REGION,
};
use crate::lambda_tagged_resource_index::{create_function, failures, lambda_tags};
use crate::sns_tagged_resource_index::{emulator as sns_emulator, sns_tags, topic};

/// The most ARNs one call may carry: `ResourceARNList` of `TagResources` and
/// `UntagResources` targets `ResourceARNListForTagUntag`, `@length(1, 20)`
/// (`smithy_query(query="operation", operation="TagResources,UntagResources")`).
const MAX_ARNS_PER_CALL: usize = 20;

/// The tag every batch below writes, and whose key the untag batch removes.
const WRITTEN: (&str, &str) = ("CostCentre", "cc-42");

/// The tag each live resource is CREATED with, so a batch write is shown to merge into
/// the set the owning service already held rather than to replace it.
const BASELINE: (&str, &str) = ("estate", "live");

/// What one slot of a batch's `ResourceARNList` holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Slot {
    /// A target group this emulator really created.
    Live,
    /// A well-formed target group ARN no target group answers to.
    GhostTargetGroup,
    /// A well-formed load balancer ARN no load balancer answers to — a second kind, so
    /// the failure each slot earns is shown to come from the ARN rather than from the
    /// batch.
    GhostLoadBalancer,
}

impl Slot {
    /// The wire error code the owning service refuses this slot's ARN with, or `None`
    /// when the slot resolves and therefore earns no entry in `FailedResourcesMap`.
    fn error_code(self) -> Option<&'static str> {
        match self {
            Slot::Live => None,
            Slot::GhostTargetGroup => Some("TargetGroupNotFound"),
            Slot::GhostLoadBalancer => Some("LoadBalancerNotFound"),
        }
    }
}

/// One batch, as the caller writes it: the ARNs of the slots in the order the request
/// carries them, beside the slot each ARN came from.
struct Batch {
    arns: Vec<String>,
    slots: Vec<Slot>,
}

impl Batch {
    /// The ARNs of every slot that resolves.
    fn live(&self) -> Vec<&String> {
        self.pick(|slot| slot == Slot::Live)
    }

    /// The ARNs of every slot that does not.
    fn ghosts(&self) -> Vec<&String> {
        self.pick(|slot| slot != Slot::Live)
    }

    fn pick(&self, keep: impl Fn(Slot) -> bool) -> Vec<&String> {
        self.slots
            .iter()
            .zip(self.arns.iter())
            .filter(|(slot, _)| keep(**slot))
            .map(|(_, arn)| arn)
            .collect()
    }

    /// The failure each slot earns, keyed by ARN: the whole `FailedResourcesMap` this
    /// batch must answer with, so an ARN missing from it and an extra ARN in it are both
    /// caught.
    fn expected_codes(&self) -> BTreeMap<&String, &'static str> {
        self.slots
            .iter()
            .zip(self.arns.iter())
            .filter_map(|(slot, arn)| slot.error_code().map(|code| (arn, code)))
            .collect()
    }
}

/// Create one target group per [`Slot::Live`] and fabricate an ARN of the right kind for
/// every other slot, in the order the slots are given.
///
/// Each live target group carries [`BASELINE`] from its create; each ghost ARN is
/// well-formed in this endpoint's partition, Region and account (so the call is not
/// refused at request level) and names a resource no create ever minted.
async fn batch(router: &smithy_server_runtime::MultiServiceRouter, slots: &[Slot]) -> Batch {
    let mut arns = Vec::with_capacity(slots.len());
    for (index, slot) in slots.iter().enumerate() {
        arns.push(match slot {
            Slot::Live => {
                let mut form = action("CreateTargetGroup");
                form.push(p("Name", &format!("batch-live-{index}")));
                form.push(p("Protocol", "HTTP"));
                form.push(p("Port", "80"));
                form.push(p("VpcId", "vpc-12345678"));
                form.extend(tag_fields(&[BASELINE]));
                require(&elb_ok(router, &form).await, "TargetGroupArn")
            }
            Slot::GhostTargetGroup => {
                elb_arn(&format!("targetgroup/batch-ghost-{index}/0123456789abcdef"))
            }
            Slot::GhostLoadBalancer => {
                elb_arn(&format!("loadbalancer/app/batch-ghost-{index}/0123456789abcdef"))
            }
        });
    }
    Batch {
        arns,
        slots: slots.to_vec(),
    }
}

/// The `ErrorCode` of every entry of a `FailedResourcesMap`, keyed by ARN.
fn failed_codes(body: &Value) -> BTreeMap<String, String> {
    body["FailedResourcesMap"]
        .as_object()
        .unwrap_or_else(|| panic!("a body with no FailedResourcesMap object: {body}"))
        .iter()
        .map(|(arn, failure)| {
            (
                arn.clone(),
                failure["ErrorCode"]
                    .as_str()
                    .unwrap_or_else(|| panic!("a failure with no ErrorCode: {failure}"))
                    .to_string(),
            )
        })
        .collect()
}

/// The batch shapes every test below runs: the rows differ in how many ARNs the call
/// carries, how many of them resolve, WHERE the unresolvable ones sit, and which kind
/// they name. The recorded instance — one absent target group beside one live one — is
/// the first row.
fn shapes() -> Vec<(String, Vec<Slot>)> {
    let mut shapes: Vec<(String, Vec<Slot>)> = vec![
        (
            "the absent ARN first, the live one last".to_string(),
            vec![Slot::GhostTargetGroup, Slot::Live],
        ),
        (
            "the live ARN first, the absent one last".to_string(),
            vec![Slot::Live, Slot::GhostTargetGroup],
        ),
        (
            "an absent ARN between two live ones".to_string(),
            vec![Slot::Live, Slot::GhostTargetGroup, Slot::Live],
        ),
        (
            "two absent ARNs of different kinds among three live ones".to_string(),
            vec![
                Slot::Live,
                Slot::GhostTargetGroup,
                Slot::Live,
                Slot::GhostLoadBalancer,
                Slot::Live,
            ],
        ),
        (
            "every ARN of the batch resolves".to_string(),
            vec![Slot::Live, Slot::Live],
        ),
        (
            "no ARN of the batch resolves".to_string(),
            vec![Slot::GhostTargetGroup, Slot::GhostLoadBalancer],
        ),
        (
            "a single absent ARN, the shortest list the model allows".to_string(),
            vec![Slot::GhostTargetGroup],
        ),
    ];

    // The longest list the model allows, alternating, so the selection is read out of a
    // full batch rather than out of a pair.
    let full: Vec<Slot> = (0..MAX_ARNS_PER_CALL)
        .map(|index| {
            if index % 2 == 0 {
                Slot::Live
            } else {
                Slot::GhostTargetGroup
            }
        })
        .collect();
    shapes.push((
        format!("the longest list the model allows ({MAX_ARNS_PER_CALL} ARNs), half of them absent"),
        full,
    ));
    shapes
}

/// RULE: `TagResources` reports its outcome per resource — HTTP 200 with exactly the
/// ARNs the owning service could not resolve in `FailedResourcesMap`, each under that
/// service's own error code and `StatusCode`, and the tags applied to every ARN of the
/// same request that did resolve.
#[tokio::test]
async fn tag_resources_fails_exactly_the_unresolvable_arns_of_a_batch() {
    for (case, slots) in shapes() {
        // A router per row, so one row's writes are not another row's starting point.
        let router = emulator();
        let batch = batch(&router, &slots).await;

        let (status, body) = tagging(
            &router,
            "TagResources",
            json!({
                "ResourceARNList": &batch.arns,
                "Tags": { WRITTEN.0: WRITTEN.1 },
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: a per-resource failure is still HTTP 200, got {status} with \
             body {body}"
        );

        let want: BTreeMap<String, String> = batch
            .expected_codes()
            .into_iter()
            .map(|(arn, code)| (arn.clone(), code.to_string()))
            .collect();
        let got = failed_codes(&body);
        assert_eq!(
            got, want,
            "case {case:?}: expected FailedResourcesMap to name {want:?}, got {got:?}"
        );
        for arn in batch.ghosts() {
            let failure = &body["FailedResourcesMap"][arn.as_str()];
            assert_eq!(
                failure["StatusCode"],
                json!(400),
                "case {case:?}: expected StatusCode 400 for {arn}, got {failure}"
            );
            assert!(
                failure["ErrorMessage"]
                    .as_str()
                    .is_some_and(|message| !message.is_empty()),
                "case {case:?}: expected a message from the owning service for {arn}, got \
                 {failure}"
            );
        }

        // The write landed on every ARN the map did NOT name, in the owning service's own
        // records and therefore in this API's view of them.
        let tagged = expected(&[BASELINE, WRITTEN]);
        for arn in batch.live() {
            let got = elb_tags(&router, arn).await;
            assert_eq!(
                got, tagged,
                "case {case:?}: expected the owning service to hold {tagged:?} for {arn}, \
                 got {got:?}"
            );
            let page = resources_for(&router, arn).await;
            let got = page_tags(&page, arn);
            assert_eq!(
                got, tagged,
                "case {case:?}: expected GetResources to report {tagged:?} for {arn}, got \
                 {got:?}"
            );
        }
    }
}

/// RULE: an ARN the owning service does not resolve is not tagged at all — a failed
/// entry of the batch leaves no tag anywhere, so nothing reports it afterwards.
#[tokio::test]
async fn an_unresolvable_arn_of_a_batch_is_left_untagged() {
    for (case, slots) in shapes() {
        let router = emulator();
        let batch = batch(&router, &slots).await;
        let (status, body) = tagging(
            &router,
            "TagResources",
            json!({
                "ResourceARNList": &batch.arns,
                "Tags": { WRITTEN.0: WRITTEN.1 },
            }),
        )
        .await;
        assert_eq!(status, 200, "case {case:?}: TagResources body {body}");

        for arn in batch.ghosts() {
            // This API's own index does not report it: a resource nothing holds is not a
            // resource this view can carry tags for.
            let page = resources_for(&router, arn).await;
            let reported = page_arns(&page);
            assert!(
                reported.is_empty(),
                "case {case:?}: expected GetResources to report nothing for {arn}, got \
                 {reported:?}"
            );
            // And neither does the owning service, which refuses the read outright.
            let mut form = action("DescribeTags");
            form.push(p("ResourceArns.member.1", arn));
            let (status, own) = elb(&router, &form).await;
            assert_eq!(
                status, 400,
                "case {case:?}: expected the owning service to refuse DescribeTags({arn}), \
                 got {status} with body {own}"
            );
        }
    }
}

/// RULE: `UntagResources` reports its outcome per resource the same way — HTTP 200,
/// exactly the unresolvable ARNs in `FailedResourcesMap`, and the named keys removed from
/// every ARN of the same request that did resolve.
#[tokio::test]
async fn untag_resources_fails_exactly_the_unresolvable_arns_of_a_batch() {
    for (case, slots) in shapes() {
        let router = emulator();
        let batch = batch(&router, &slots).await;

        // Put the key on every resolvable ARN first, so the untag has something to remove
        // from each of them.
        if !batch.live().is_empty() {
            let live: Vec<&String> = batch.live();
            let (status, body) = tagging(
                &router,
                "TagResources",
                json!({ "ResourceARNList": live, "Tags": { WRITTEN.0: WRITTEN.1 } }),
            )
            .await;
            assert_eq!(status, 200, "case {case:?}: TagResources body {body}");
        }

        let (status, body) = tagging(
            &router,
            "UntagResources",
            json!({ "ResourceARNList": &batch.arns, "TagKeys": [WRITTEN.0] }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: a per-resource failure is still HTTP 200, got {status} with \
             body {body}"
        );
        let want: BTreeMap<String, String> = batch
            .expected_codes()
            .into_iter()
            .map(|(arn, code)| (arn.clone(), code.to_string()))
            .collect();
        let got = failed_codes(&body);
        assert_eq!(
            got, want,
            "case {case:?}: expected FailedResourcesMap to name {want:?}, got {got:?}"
        );

        let kept = expected(&[BASELINE]);
        for arn in batch.live() {
            let got = elb_tags(&router, arn).await;
            assert_eq!(
                got, kept,
                "case {case:?}: expected the untag to leave {kept:?} on {arn}, got {got:?}"
            );
        }
    }
}

/// RULE: the selection is keyed by ARN, so the same batch written twice answers the same
/// `FailedResourcesMap` — a per-resource failure neither creates the resource nor
/// consumes anything.
#[tokio::test]
async fn the_same_batch_answers_the_same_per_resource_selection_twice() {
    let router = emulator();
    let batch = batch(
        &router,
        &[Slot::GhostTargetGroup, Slot::Live, Slot::GhostLoadBalancer],
    )
    .await;
    let mut answers: Vec<BTreeMap<String, String>> = Vec::new();
    for attempt in 1..=2 {
        let (status, body) = tagging(
            &router,
            "TagResources",
            json!({
                "ResourceARNList": &batch.arns,
                "Tags": { WRITTEN.0: WRITTEN.1 },
            }),
        )
        .await;
        assert_eq!(status, 200, "attempt {attempt}: TagResources body {body}");
        answers.push(failed_codes(&body));
    }
    assert_eq!(
        answers[0], answers[1],
        "expected the second write of the same batch to answer {:?}, got {:?}",
        answers[0], answers[1]
    );
    let ghosts: BTreeSet<String> = batch.ghosts().into_iter().cloned().collect();
    let named: BTreeSet<String> = answers[0].keys().cloned().collect();
    assert_eq!(
        named, ghosts,
        "expected the unresolvable ARNs {ghosts:?} to be the whole selection, got {named:?}"
    );
}

// ---------------------------------------------------------------------------
// The same batch contract across OWNING SERVICES: every entry is composed by the
// service that hosts the one resource its key names.
// ---------------------------------------------------------------------------

/// An ARN in a namespace no owning crate is linked against, so it needs no live resource
/// behind it: the tagging store is its tag home, and a batch that names it must tag it.
const UNOWNED: &str = "arn:aws:cloudwatch:us-east-1:000000000000:alarm/batch-owner-control";

/// What Amazon SNS says about a topic ARN it holds no topic for. The WHOLE sentence is
/// fixed text with no variable part, so a test pins it as the wire carries it rather than
/// as the implementation spells it.
const SNS_ABSENT_MESSAGE: &str = "Resource does not exist";

/// What Amazon CloudWatch Logs says about an ARN naming a log group it does not hold: the
/// sentence its model documents for `ResourceNotFoundException` ("The specified resource
/// does not exist.") with the missing resource's KIND named, which is the only variable
/// part of it.
const LOGS_ABSENT_LOG_GROUP_MESSAGE: &str = "The specified log group does not exist.";

/// One row of the owner catalogue: an ARN well formed for this endpoint's partition,
/// Region and account that the service owning its namespace holds no resource for, beside
/// the `StatusCode` and `ErrorCode` THAT service's own refusal carries — and, where the
/// owning crate publishes its wording as a constant, the sentence too.
struct Absent {
    case: &'static str,
    arn: String,
    status_code: i64,
    error_code: &'static str,
    message: Option<&'static str>,
}

/// One unresolvable ARN per owning service this API resolves against, spanning the owners
/// whose refusals differ in STATUS as well as in code, so an entry's fields are shown to
/// come from the ARN's own owner rather than from the call.
///
/// Each pair is the error the owning service's own tagging operation refuses an
/// unresolvable resource with, read from that service's model —
/// `smithy_query(query="shape", shape="<Error>")` on its `smithy-model/` reports the
/// `httpError` and, for a service publishing an awsQuery wire, the `awsQueryError` code
/// the wire carries instead of the shape name:
///
/// * Amazon SNS `ResourceNotFoundException`: 404, awsQuery code `ResourceNotFound`;
/// * AWS Lambda `ResourceNotFoundException`: 404;
/// * Amazon SQS `QueueDoesNotExist`: 400, awsQuery code
///   `AWS.SimpleQueueService.NonExistentQueue`;
/// * Elastic Load Balancing `TargetGroupNotFoundException`: 400, awsQuery code
///   `TargetGroupNotFound`;
/// * Amazon DynamoDB, Amazon EventBridge and AWS Secrets Manager
///   `ResourceNotFoundException`: 400;
/// * Amazon ECS `ClusterNotFoundException`: 400.
/// * Amazon CloudWatch Logs `ResourceNotFoundException`: 400.
///
/// The last row is a second resource of an owner already in the table whose NAME carries
/// upper case and an underscore, which the catalogue's other rows do not: the entry is
/// keyed by the request's ARN as written, so a name no case folding would survive is the
/// row that catches a normalised key.
fn absent_per_owner() -> Vec<Absent> {
    let account = account();
    vec![
        Absent {
            case: "an Amazon SNS topic that was never created",
            arn: format!("arn:aws:sns:{REGION}:{account}:batch-owner-ghost-topic"),
            status_code: 404,
            error_code: "ResourceNotFound",
            message: Some(SNS_ABSENT_MESSAGE),
        },
        Absent {
            case: "an AWS Lambda function that was never created",
            arn: format!("arn:aws:lambda:{REGION}:{account}:function:batch-owner-ghost-function"),
            status_code: 404,
            error_code: "ResourceNotFoundException",
            message: None,
        },
        Absent {
            case: "an Amazon SQS queue that was never created",
            arn: format!("arn:aws:sqs:{REGION}:{account}:batch-owner-ghost-queue"),
            status_code: 400,
            error_code: "AWS.SimpleQueueService.NonExistentQueue",
            message: None,
        },
        Absent {
            case: "an Elastic Load Balancing target group that was never created",
            arn: elb_arn("targetgroup/batch-owner-ghost/0123456789abcdef"),
            status_code: 400,
            error_code: "TargetGroupNotFound",
            message: None,
        },
        Absent {
            case: "an Amazon DynamoDB table that was never created",
            arn: format!("arn:aws:dynamodb:{REGION}:{account}:table/batch-owner-ghost-table"),
            status_code: 400,
            error_code: "ResourceNotFoundException",
            message: None,
        },
        Absent {
            case: "an Amazon EventBridge rule that was never created",
            arn: format!("arn:aws:events:{REGION}:{account}:rule/batch-owner-ghost-rule"),
            status_code: 400,
            error_code: "ResourceNotFoundException",
            message: None,
        },
        Absent {
            case: "an AWS Secrets Manager secret that was never created",
            arn: format!("arn:aws:secretsmanager:{REGION}:{account}:secret:batch-owner-ghost-AbCdEf"),
            status_code: 400,
            error_code: "ResourceNotFoundException",
            message: None,
        },
        Absent {
            case: "an Amazon ECS cluster that was never created",
            arn: format!("arn:aws:ecs:{REGION}:{account}:cluster/batch-owner-ghost"),
            status_code: 400,
            error_code: "ClusterNotFoundException",
            message: None,
        },
        Absent {
            case: "a CloudWatch Logs log group that was never created",
            arn: format!("arn:aws:logs:{REGION}:{account}:log-group:/aws/batch-owner/ghost"),
            status_code: 400,
            error_code: "ResourceNotFoundException",
            message: Some(LOGS_ABSENT_LOG_GROUP_MESSAGE),
        },
        Absent {
            case: "an Amazon SNS topic whose name carries upper case and an underscore",
            arn: format!("arn:aws:sns:{REGION}:{account}:Batch_Owner_GHOST"),
            status_code: 404,
            error_code: "ResourceNotFound",
            message: Some(SNS_ABSENT_MESSAGE),
        },
    ]
}

/// The whole `FailedResourcesMap` of one answer, read the way the sibling suite reads it
/// ([`failures`]), checked against the catalogue: every unresolvable ARN of `rows` is named
/// with the status and code its owner refuses it with, and no other ARN is named at all.
///
/// A reference answer has to be checked before another answer is compared to it: a map that
/// named NOTHING — the symptom this suite exists for — would otherwise agree with every
/// other empty map.
fn checked_failures(
    case: &str,
    body: &Value,
    rows: &[&Absent],
) -> BTreeMap<String, (i64, String, String)> {
    let got = failures(body);
    let named: BTreeSet<&String> = got.keys().collect();
    let want: BTreeSet<&String> = rows.iter().map(|row| &row.arn).collect();
    assert_eq!(
        named, want,
        "case {case:?}: expected exactly the unresolvable ARNs of the batch to be named, got \
         {named:?}"
    );
    for row in rows {
        let (status_code, error_code, message) = got
            .get(&row.arn)
            .unwrap_or_else(|| panic!("case {case:?}: no entry for {}", row.arn));
        assert_eq!(
            (*status_code, error_code.as_str()),
            (row.status_code, row.error_code),
            "case {case:?} / {}: expected ({}, {}) for {}, got ({status_code}, {error_code})",
            row.case,
            row.status_code,
            row.error_code,
            row.arn
        );
        match row.message {
            Some(expected) => assert_eq!(
                message, expected,
                "case {case:?} / {}: expected the owning service's sentence {expected:?} for {}, \
                 got {message:?}",
                row.case, row.arn
            ),
            None => assert!(
                !message.is_empty(),
                "case {case:?} / {}: expected a sentence from the owning service for {}, got \
                 {message:?}",
                row.case,
                row.arn
            ),
        }
    }
    got
}

/// One `TagResources` batch that must answer HTTP 200, and the body it answered.
async fn tag_batch(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    arns: &[String],
) -> Value {
    let (status, body) = tagging(
        router,
        "TagResources",
        json!({ "ResourceARNList": arns, "Tags": { WRITTEN.0: WRITTEN.1 } }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: a per-resource failure is still HTTP 200, got {status} with body {body}"
    );
    body
}

/// RULE: every ARN of a batch that the service owning its namespace cannot resolve earns
/// its OWN `FailedResourcesMap` entry, keyed by that ARN exactly as the request wrote it
/// and carrying that owner's status, error code and sentence; the ARNs that did resolve
/// earn no entry and are tagged by the same call.
#[tokio::test]
async fn a_batch_spanning_owning_services_keys_each_failure_to_its_own_owner() {
    let router = sns_emulator();
    // Two resolvable ARNs of different homes: a topic the owning service really holds, and
    // an ARN whose namespace no owning crate serves, whose tags this API itself keeps.
    let live = topic(&router, "batch-owner-live").await;
    let rows = absent_per_owner();

    let mut arns: Vec<String> = vec![live.clone(), UNOWNED.to_string()];
    arns.extend(rows.iter().map(|row| row.arn.clone()));
    let case = "every owner at once";
    let body = tag_batch(&router, case, &arns).await;

    // Exactly the unresolvable ARNs are named, each with its own owner's status, code and
    // sentence; the resolvable ones are named nowhere.
    checked_failures(case, &body, &rows.iter().collect::<Vec<&Absent>>());

    for row in &rows {
        // A per-resource failure brings nothing into being: the ARN is in no answer of
        // this API afterwards.
        let page = resources_for(&router, &row.arn).await;
        let reported = page_arns(&page);
        assert!(
            reported.is_empty(),
            "case {:?}: expected GetResources to report nothing for {}, got {reported:?}",
            row.case,
            row.arn
        );
    }

    // And the resolvable ARNs of that same call were tagged — the topic in the owning
    // service's own records, read through ITS reader, and the unowned ARN in this API's.
    let tagged = expected(&[WRITTEN]);
    let held = sns_tags(&router, &live).await;
    assert_eq!(
        held, tagged,
        "case the resolvable topic: expected the owning service to hold {tagged:?} for \
         {live}, got {held:?}"
    );
    let page = resources_for(&router, UNOWNED).await;
    let got = page_tags(&page, UNOWNED);
    assert_eq!(
        got, tagged,
        "case the resolvable unowned ARN: expected GetResources to report {tagged:?} for \
         {UNOWNED}, got {got:?}"
    );
}

/// RULE: the entry is composed per RESOURCE, so the failure one ARN earns does not depend
/// on the batch it travelled in — its position, the other owners beside it, or whether any
/// ARN of the call resolved at all.
#[tokio::test]
async fn an_absent_arn_earns_the_same_failure_wherever_it_sits_in_a_batch() {
    let router = sns_emulator();
    let live = topic(&router, "batch-owner-position-live").await;
    let rows = absent_per_owner();
    let every: Vec<&Absent> = rows.iter().collect();
    let all: Vec<String> = rows.iter().map(|row| row.arn.clone()).collect();

    // The reference answer: every unresolvable ARN of the catalogue, behind the resolvable
    // one. It is checked against the catalogue before any other answer is compared to it,
    // so an answer naming nothing cannot pass as agreement.
    let mut first = vec![live.clone()];
    first.extend(all.iter().cloned());
    let case = "the resolvable ARN first";
    let baseline = checked_failures(case, &tag_batch(&router, case, &first).await, &every);

    // The shapes the same ARNs are presented in: the resolvable one last, the
    // unresolvable ones reversed around it, the batch where nothing resolves, and each
    // unresolvable ARN alone beside the resolvable one. Each row carries the rows of the
    // catalogue its call addresses, so the check knows which entries to expect.
    let mut shapes: Vec<(String, Vec<String>, Vec<&Absent>)> = Vec::new();
    let mut last = all.clone();
    last.push(live.clone());
    shapes.push(("the resolvable ARN last".to_string(), last, every.clone()));
    let mut reversed: Vec<String> = all.iter().rev().cloned().collect();
    reversed.insert(all.len() / 2, live.clone());
    shapes.push((
        "the unresolvable ARNs reversed around the resolvable one".to_string(),
        reversed,
        every.clone(),
    ));
    shapes.push((
        "no ARN of the batch resolves".to_string(),
        all.clone(),
        every.clone(),
    ));
    for row in &rows {
        shapes.push((
            format!("{} alone beside the resolvable ARN", row.case),
            vec![row.arn.clone(), live.clone()],
            vec![row],
        ));
    }

    for (case, arns, addressed) in shapes {
        let got = checked_failures(&case, &tag_batch(&router, &case, &arns).await, &addressed);
        let want: BTreeMap<String, (i64, String, String)> = baseline
            .iter()
            .filter(|(arn, _)| arns.contains(arn))
            .map(|(arn, failure)| (arn.clone(), failure.clone()))
            .collect();
        assert_eq!(
            got, want,
            "case {case:?}: expected the same per-resource failures {want:?}, got {got:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// One batch over TWO owners that each keep their resources' own tag sets —
// CloudWatch Logs and AWS Lambda. The call that names the unresolvable ARNs is
// the call that writes the live ones, into the one tag plane both are read from.
// ---------------------------------------------------------------------------

/// A router carrying this API, CloudWatch Logs and AWS Lambda over ONE engine — the
/// arrangement `core/router` builds — so each owner's resources are created and read back
/// through that owner's OWN wire.
fn logs_lambda_emulator() -> smithy_server_runtime::MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = smithy_server_runtime::MultiServiceRouter::new();
    // The tests exercise handler behaviour, not SigV4: the `Authorization` header below is
    // present because it is what routes a request to a signing name, not to be verified.
    router.set_request_authentication(false);
    router.register_service(TaggingService::new(Arc::clone(&engine)));
    router.register_service(owner_logs::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(owner_lambda::EmulatedService::new(engine));
    router
}

/// One CloudWatch Logs operation, over the awsJson wire that model publishes.
async fn logs_call(
    router: &smithy_server_runtime::MultiServiceRouter,
    operation: &str,
    body: Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("Logs_20140328.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/logs/aws4_request, \
                 SignedHeaders=host, Signature=0"
            ),
        )
        .body(bytes::Bytes::from(body.to_string()))
        .expect("build the request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or_else(|error| {
            panic!(
                "{operation} answered a body that is not JSON ({error}): {}",
                String::from_utf8_lossy(response.body())
            )
        })
    };
    (status, parsed)
}

/// One CloudWatch Logs operation that must answer 200.
async fn logs_ok(
    router: &smithy_server_runtime::MultiServiceRouter,
    operation: &str,
    body: Value,
) -> Value {
    let (status, parsed) = logs_call(router, operation, body).await;
    assert_eq!(
        status, 200,
        "{operation} must answer 200, got {status} with body {parsed}"
    );
    parsed
}

/// A tag table as the JSON object a map member carries on the wire.
fn tag_map(tags: &[(&str, &str)]) -> Value {
    Value::Object(
        tags.iter()
            .map(|(key, value)| ((*key).to_string(), json!(value)))
            .collect(),
    )
}

/// Create a log group carrying `tags` and answer the ARN CloudWatch Logs reports for it.
///
/// The ARN is READ BACK from that service's own `DescribeLogGroups` rather than built
/// here, in the `logGroupArn` form — the one without the trailing `:*` of the `arn` form,
/// which names the group together with everything under it.
async fn log_group(
    router: &smithy_server_runtime::MultiServiceRouter,
    name: &str,
    tags: &[(&str, &str)],
) -> String {
    let mut request = json!({ "logGroupName": name });
    if !tags.is_empty() {
        request["tags"] = tag_map(tags);
    }
    logs_ok(router, "CreateLogGroup", request).await;
    let described = logs_ok(
        router,
        "DescribeLogGroups",
        json!({ "logGroupNamePrefix": name }),
    )
    .await;
    let found = described["logGroups"]
        .as_array()
        .unwrap_or_else(|| panic!("a DescribeLogGroups body with no logGroups: {described}"))
        .iter()
        .find(|group| group["logGroupName"] == json!(name))
        .unwrap_or_else(|| panic!("{name} is not in {described}"));
    found["logGroupArn"]
        .as_str()
        .unwrap_or_else(|| panic!("a log group carries a logGroupArn, got {found}"))
        .to_string()
}

/// The tag map CloudWatch Logs' OWN `ListTagsForResource` reports for one ARN.
async fn logs_tags(
    router: &smithy_server_runtime::MultiServiceRouter,
    arn: &str,
) -> BTreeMap<String, String> {
    let body = logs_ok(router, "ListTagsForResource", json!({ "resourceArn": arn })).await;
    body["tags"]
        .as_object()
        .unwrap_or_else(|| panic!("a ListTagsForResource body with no tags: {body}"))
        .iter()
        .map(|(key, value)| (key.clone(), value.as_str().unwrap_or_default().to_string()))
        .collect()
}

/// A log-group ARN in the form CloudWatch Logs reports a group under, for a group this
/// emulator never created.
fn log_group_arn(name: &str) -> String {
    format!("arn:aws:logs:{REGION}:{}:log-group:{name}", account())
}

/// An unqualified AWS Lambda function ARN, for a function this emulator never created.
fn function_arn(name: &str) -> String {
    format!("arn:aws:lambda:{REGION}:{}:function:{name}", account())
}

/// What one slot of a batch addressed at the two owners holds: a resource that owner
/// really has, or a well-formed ARN of one of the kinds it does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Owned {
    /// A log group this emulator really created.
    LiveLogGroup,
    /// A Lambda function this emulator really created.
    LiveFunction,
    /// A well-formed log-group ARN no log group answers to.
    AbsentLogGroup,
    /// A well-formed function ARN no function answers to — the other owner, so the
    /// failure each slot earns is shown to come from the ARN's owner.
    AbsentFunction,
    /// A well-formed CloudWatch Logs ARN naming a resource KIND that service holds no tag
    /// set for at all, which is that owner's other refusal.
    AbsentLogStream,
}

/// One slot of such a batch: the ARN it put in the request, and the entry it must earn in
/// `FailedResourcesMap` — `None` for a slot the owner resolves, which must earn none.
struct OwnedSlot {
    kind: Owned,
    arn: String,
    failure: Option<(i64, &'static str, Option<String>)>,
}

/// Build one batch: create a resource through its owner's own API for every live slot and
/// fabricate an ARN of the right kind for every other, in the order the slots are given.
///
/// Every fabricated ARN is well formed in this endpoint's partition, Region and account —
/// so the call is not refused at request level — and names a resource no create minted.
/// Every live resource carries [`BASELINE`] from its create, so the batch write is shown
/// to merge into the tag set its owner already held.
///
/// The failures are each owner's own, from its model: CloudWatch Logs refuses a missing
/// resource with `ResourceNotFoundException`, 400 (`smithy_query(query="shape",
/// shape="ResourceNotFoundException")` on its model reports the defaulted `httpError`),
/// and an ARN naming a kind it cannot tag with `InvalidParameterException`, 400; AWS
/// Lambda refuses a missing function with its own `ResourceNotFoundException`, 404, whose
/// sentence names the ARN it resolved.
async fn owned_batch(
    router: &smithy_server_runtime::MultiServiceRouter,
    slots: &[Owned],
) -> Vec<OwnedSlot> {
    let mut built: Vec<OwnedSlot> = Vec::with_capacity(slots.len());
    for (index, kind) in slots.iter().enumerate() {
        let (arn, failure) = match kind {
            Owned::LiveLogGroup => (
                log_group(router, &format!("/aws/batch/live-{index}"), &[BASELINE]).await,
                None,
            ),
            Owned::LiveFunction => (
                create_function(router, REGION, &format!("batch-live-{index}"), &[BASELINE]).await,
                None,
            ),
            Owned::AbsentLogGroup => (
                log_group_arn(&format!("/aws/batch/ghost-{index}")),
                Some((
                    400,
                    "ResourceNotFoundException",
                    Some(LOGS_ABSENT_LOG_GROUP_MESSAGE.to_string()),
                )),
            ),
            Owned::AbsentFunction => {
                let arn = function_arn(&format!("batch-ghost-{index}"));
                let message = format!("Function not found: {arn}");
                (
                    arn,
                    Some((404, "ResourceNotFoundException", Some(message))),
                )
            }
            Owned::AbsentLogStream => (
                format!(
                    "{}:log-stream:ghost-{index}",
                    log_group_arn(&format!("/aws/batch/live-stream-{index}"))
                ),
                // The sentence is that owner's own wording for a `resourceArn` naming a
                // kind it cannot tag; the test pins the status and the code it travels
                // under and requires a sentence, as the catalogue above does.
                Some((400, "InvalidParameterException", None)),
            ),
        };
        built.push(OwnedSlot {
            kind: *kind,
            arn,
            failure,
        });
    }
    built
}

/// The batch shapes the mixed-owner rows run: they differ in how many ARNs the call
/// carries, which owner each one belongs to, which of them resolve and where those sit.
/// The recorded instance — one never-created log group beside one live function — is the
/// first row.
fn owned_shapes() -> Vec<(String, Vec<Owned>)> {
    let mut shapes: Vec<(String, Vec<Owned>)> = vec![
        (
            "an absent log group beside a live function".to_string(),
            vec![Owned::AbsentLogGroup, Owned::LiveFunction],
        ),
        (
            "a live function beside an absent log group".to_string(),
            vec![Owned::LiveFunction, Owned::AbsentLogGroup],
        ),
        (
            "an absent function beside a live log group".to_string(),
            vec![Owned::LiveLogGroup, Owned::AbsentFunction],
        ),
        (
            "both owners live, both owners absent, alternating".to_string(),
            vec![
                Owned::LiveLogGroup,
                Owned::AbsentFunction,
                Owned::LiveFunction,
                Owned::AbsentLogGroup,
            ],
        ),
        (
            "a log-group ARN naming an untaggable kind between two live resources".to_string(),
            vec![
                Owned::LiveLogGroup,
                Owned::AbsentLogStream,
                Owned::LiveFunction,
            ],
        ),
        (
            "every ARN of the batch resolves".to_string(),
            vec![Owned::LiveLogGroup, Owned::LiveFunction],
        ),
        (
            "no ARN of the batch resolves".to_string(),
            vec![Owned::AbsentLogGroup, Owned::AbsentFunction],
        ),
        (
            "a single absent log group, the shortest list the model allows".to_string(),
            vec![Owned::AbsentLogGroup],
        ),
    ];

    // The longest list the model allows, cycling through every kind, so the selection is
    // read out of a full batch rather than out of a pair.
    let cycle = [
        Owned::LiveLogGroup,
        Owned::AbsentLogGroup,
        Owned::LiveFunction,
        Owned::AbsentFunction,
        Owned::AbsentLogStream,
    ];
    let full: Vec<Owned> = (0..MAX_ARNS_PER_CALL)
        .map(|index| cycle[index % cycle.len()])
        .collect();
    shapes.push((
        format!("the longest list the model allows ({MAX_ARNS_PER_CALL} ARNs), every kind in turn"),
        full,
    ));
    shapes
}

/// The tag set the owner of `slot` reports for it, read through THAT service's own
/// reader — so an entry absent from `FailedResourcesMap` is checked against the service
/// that hosts the resource rather than against this API's own store.
async fn owner_held_tags(
    router: &smithy_server_runtime::MultiServiceRouter,
    slot: &OwnedSlot,
) -> BTreeMap<String, String> {
    match slot.kind {
        Owned::LiveLogGroup => logs_tags(router, &slot.arn).await,
        Owned::LiveFunction => lambda_tags(router, &slot.arn).await,
        other => panic!("{other:?} names no resource an owner holds tags for"),
    }
}

/// RULE: `TagResources` reports its outcome per resource, so a batch mixing ARNs of
/// several owning services answers HTTP 200 with exactly the ARNs their owners will not
/// tag in `FailedResourcesMap` — each carrying that owner's own `StatusCode`, `ErrorCode`
/// and sentence — and writes the request's tags into the own tag set of every other ARN of
/// the same call.
#[tokio::test]
async fn a_batch_over_two_owners_names_only_the_arns_their_owners_will_not_tag() {
    for (case, slots) in owned_shapes() {
        // A router per row, so one row's writes are not another row's starting point.
        let router = logs_lambda_emulator();
        let batch = owned_batch(&router, &slots).await;
        let arns: Vec<String> = batch.iter().map(|slot| slot.arn.clone()).collect();

        let body = tag_batch(&router, &case, &arns).await;

        let got = failures(&body);
        let named: BTreeSet<&String> = got.keys().collect();
        let want: BTreeSet<&String> = batch
            .iter()
            .filter(|slot| slot.failure.is_some())
            .map(|slot| &slot.arn)
            .collect();
        assert_eq!(
            named, want,
            "case {case:?}: expected FailedResourcesMap to name {want:?}, got {named:?}"
        );

        for slot in &batch {
            let Some((status_code, error_code, message)) = &slot.failure else {
                continue;
            };
            let (reported_status, reported_code, reported_message) = got
                .get(&slot.arn)
                .unwrap_or_else(|| panic!("case {case:?}: no entry for {}", slot.arn));
            assert_eq!(
                (*reported_status, reported_code.as_str()),
                (*status_code, *error_code),
                "case {case:?} / {:?}: expected ({status_code}, {error_code}) for {}, got \
                 ({reported_status}, {reported_code})",
                slot.kind,
                slot.arn
            );
            match message {
                Some(expected) => assert_eq!(
                    reported_message, expected,
                    "case {case:?} / {:?}: expected the owner's sentence {expected:?} for {}, got \
                     {reported_message:?}",
                    slot.kind, slot.arn
                ),
                None => assert!(
                    !reported_message.is_empty(),
                    "case {case:?} / {:?}: expected a sentence from the owner for {}, got \
                     {reported_message:?}",
                    slot.kind,
                    slot.arn
                ),
            }
            // A per-resource failure brings nothing into being: the ARN is in no answer of
            // this API afterwards.
            let page = resources_for(&router, &slot.arn).await;
            let reported = page_arns(&page);
            assert!(
                reported.is_empty(),
                "case {case:?} / {:?}: expected GetResources to report nothing for {}, got \
                 {reported:?}",
                slot.kind,
                slot.arn
            );
        }

        // The write landed on every ARN the map did NOT name: in its OWNER's own tag set,
        // and so in this API's view of that resource.
        let tagged = expected(&[BASELINE, WRITTEN]);
        for slot in batch.iter().filter(|slot| slot.failure.is_none()) {
            let held = owner_held_tags(&router, slot).await;
            assert_eq!(
                held, tagged,
                "case {case:?} / {:?}: expected the owning service to hold {tagged:?} for {}, got \
                 {held:?}",
                slot.kind, slot.arn
            );
            let page = resources_for(&router, &slot.arn).await;
            let seen = page_tags(&page, &slot.arn);
            assert_eq!(
                seen, tagged,
                "case {case:?} / {:?}: expected GetResources to report {tagged:?} for {}, got \
                 {seen:?}",
                slot.kind, slot.arn
            );
        }
    }
}

/// One `TagResources` call writing `tags`, which must answer HTTP 200, and its body.
async fn tag_batch_writing(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    arns: &[String],
    tags: &[(&str, &str)],
) -> Value {
    let (status, body) = tagging(
        router,
        "TagResources",
        json!({ "ResourceARNList": arns, "Tags": tag_map(tags) }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: TagResources must answer 200, got {status} with body {body}"
    );
    body
}

/// The ARNs `GetResources` selects for one `TagFilters` entry, sorted.
async fn selected_by(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    filter: Value,
) -> Vec<String> {
    let (status, body) = tagging(router, "GetResources", json!({ "TagFilters": [filter] })).await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetResources must answer 200, got {status} with body {body}"
    );
    let mut arns = page_arns(&body);
    arns.sort();
    arns
}

/// RULE: the tag sets of the owning services are ONE account-wide tag plane, so a
/// `TagFilters` entry selects every resource of the account carrying that key — and, when
/// the entry names values, that key at one of them — whichever service hosts it.
///
/// The rows span what the rule generalises over in the tag itself: the key and value a
/// filter selects by. Each is a pair whose characters a tag key and value admit
/// (`smithy_query(query="shape", shape="TagMap")`: key 1..128, value 0..256, both
/// `^[\s\S]*$`), including the empty value "The value can be empty or null"
/// (`TagFilter$Values`) and a key whose colon is DATA rather than a namespace separator.
#[tokio::test]
async fn one_key_written_across_two_owners_is_selected_by_one_tag_filter() {
    let rows = [
        ("a plain key and value", "CostCentre", "cc-42"),
        (
            "a key whose data carries a colon",
            "lambda-console:blueprint",
            "blank-python",
        ),
        (
            "a key and a value carrying path characters",
            "team/platform.logs",
            "/estate/live",
        ),
        ("the shortest value the model admits", "Marker", ""),
        (
            "the longest key the model admits",
            // 128 characters, `TagKey`'s own maximum.
            "k12345678901234567890123456789012345678901234567890123456789012345678901234567890123456789012345678901234567890123456789012345",
            "at-the-limit",
        ),
    ];

    for (case, key, value) in rows {
        // A router per row: the selection must come from the tags this row wrote.
        let router = logs_lambda_emulator();
        let group = log_group(&router, "/aws/shared-tag/group", &[]).await;
        let function = create_function(&router, REGION, "shared-tag-function", &[]).await;
        let mut both = vec![group.clone(), function.clone()];
        both.sort();

        // ONE call tags both resources, and neither owner refuses its own.
        let body = tag_batch_writing(&router, case, &both, &[(key, value)]).await;
        let failed = failures(&body);
        assert!(
            failed.is_empty(),
            "case {case:?}: expected no FailedResourcesMap entry for two live resources, got \
             {failed:?}"
        );

        // Each owner holds the pair in its own tag set, read through its own reader.
        let want = expected(&[(key, value)]);
        let held = logs_tags(&router, &group).await;
        assert_eq!(
            held, want,
            "case {case:?}: expected CloudWatch Logs to hold {want:?} for {group}, got {held:?}"
        );
        let held = lambda_tags(&router, &function).await;
        assert_eq!(
            held, want,
            "case {case:?}: expected AWS Lambda to hold {want:?} for {function}, got {held:?}"
        );

        // And one filter over that shared key discovers BOTH, with the key alone and with
        // the key and the value it was written with.
        for filter in [json!({ "Key": key }), json!({ "Key": key, "Values": [value] })] {
            let got = selected_by(&router, case, filter.clone()).await;
            assert_eq!(
                got, both,
                "case {case:?}: expected TagFilters={filter} to select {both:?}, got {got:?}"
            );
        }

        // A filter naming the key with another value selects neither: the plane is
        // selected by what the tag holds, not by the key alone.
        let other = format!("{value}-elsewhere");
        let filter = json!({ "Key": key, "Values": [other] });
        let got = selected_by(&router, case, filter.clone()).await;
        assert!(
            got.is_empty(),
            "case {case:?}: expected TagFilters={filter} to select nothing, got {got:?}"
        );
    }
}
