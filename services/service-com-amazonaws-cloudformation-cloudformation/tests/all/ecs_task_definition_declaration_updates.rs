// SPDX-License-Identifier: CC0-1.0

//! What `UpdateStack` does to an `AWS::ECS::TaskDefinition` the stack already has, end to end
//! over the wire each service speaks.
//!
//! RULE under test: **a changed declaration of a resource that exists is applied through the
//! calls the owning service publishes for the resource that exists, and a property no such
//! call carries is create-only and REPLACES the resource instead.** A task-definition revision
//! is immutable — Amazon ECS publishes no call that edits one, and `RegisterTaskDefinition`
//! mints a new revision of the family on every call — so the only part of such a declaration
//! that reaches the revision in force is the resource's `Tags`, through that service's own
//! `TagResource`/`UntagResource` on the revision's ARN: the revision keeps its identity, and
//! every reference the stack published goes on resolving to it. A declaration that states any
//! other property differently asks for a new revision, which the deployment carries out as a
//! replacement: the next revision is registered, the logical resource is pointed at it, and the
//! superseded revision is taken down — deregistered, which leaves it `INACTIVE` and still
//! discoverable rather than gone.
//!
//! Why it matters: a stack that cannot apply the change at all fails the resource and rolls
//! back, so a client revising a task definition through IaC — a new tag, a new task size, a new
//! image — has no path to deploy it; and a stack that reported the tag change applied while
//! Amazon ECS still held the old set would hand the client a revision whose tags are not what
//! the template says.
//!
//! The rows differ along what the rule generalises over: WHICH part of the declaration changed
//! (a tag's value, a tag added, a tag removed, the task size, a container definition, and a tag
//! together with the task size), whether that part is create-only, and the template spelling
//! (YAML and JSON).

use std::time::{Duration, Instant};

use serde_json::json;
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudwatch_alarm_and_ecs_service_resources::ecs;
use crate::harness::{
    call, create_stack, member, physical_resource_id, router, settled_stack_status,
    stack_event_reasons, REGION,
};

/// How a read waits for a clock-owed step of the deployment — the cleanup that takes down the
/// revision a replacement superseded — to come due: look again, under a deadline generous
/// enough that a loaded host still reaches it. What is asserted is the CONDITION, never how
/// long it took.
const LOOK_AGAIN_AFTER: Duration = Duration::from_millis(100);
const SETTLE_DEADLINE: Duration = Duration::from_secs(90);

/// The statuses a stack and a revision settle in.
const UPDATE_COMPLETE: &str = "UPDATE_COMPLETE";
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const ACTIVE: &str = "ACTIVE";
const INACTIVE: &str = "INACTIVE";

/// The image every row's container runs, and the one an image change moves to.
const IMAGE: &str = "public.ecr.aws/docker/library/busybox:latest";
const OTHER_IMAGE: &str = "public.ecr.aws/docker/library/busybox:1.36";

/// What the update does to the identity of the revision the stack published.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Identity {
    /// The revision in force takes the change: the ARN the stack publishes does not move.
    Kept,
    /// The change is create-only, so the next revision of the family is registered and the
    /// superseded one is deregistered.
    NextRevision,
}

/// One row: the stack it deploys, the declaration it then states, and what Amazon ECS must
/// report afterwards.
struct Row {
    case: &'static str,
    stack: &'static str,
    /// The template the stack is created with, and the one the update declares.
    created: String,
    updated: String,
    /// Whether the update keeps the revision or registers the next one.
    identity: Identity,
    /// The tags Amazon ECS must report for the revision the stack publishes after the update,
    /// as `(key, value)` pairs.
    tags: &'static [(&'static str, &'static str)],
}

/// The declaration every row starts from, with the row's own family, task size, image and tag
/// lines filled in: a Fargate revision with one container, which is the shape a client deploys.
fn yaml(family: &str, cpu: &str, memory: &str, image: &str, tags: &str) -> String {
    format!(
        "Resources:\n  TaskDef:\n    Type: AWS::ECS::TaskDefinition\n    Properties:\n      \
         Family: {family}\n      RequiresCompatibilities:\n        - FARGATE\n      \
         NetworkMode: awsvpc\n      Cpu: '{cpu}'\n      Memory: '{memory}'\n      \
         ContainerDefinitions:\n        - Name: app\n          Image: {image}\n          \
         Essential: 'true'\n      Tags:\n{tags}"
    )
}

/// One `Tags` entry as the `[{Key, Value}]` list the type publishes, indented for [`yaml`].
fn tag_line(key: &str, value: &str) -> String {
    format!("        - Key: {key}\n          Value: {value}\n")
}

/// The same declaration in the other template spelling, for the row that deploys JSON.
fn json_template(family: &str, image: &str, owner: &str) -> String {
    json!({
        "Resources": {
            "TaskDef": {
                "Type": "AWS::ECS::TaskDefinition",
                "Properties": {
                    "Family": family,
                    "RequiresCompatibilities": ["FARGATE"],
                    "NetworkMode": "awsvpc",
                    "Cpu": "256",
                    "Memory": "512",
                    "ContainerDefinitions": [
                        {"Name": "app", "Image": image, "Essential": true}
                    ],
                    "Tags": [{"Key": "Owner", "Value": owner}],
                },
            }
        }
    })
    .to_string()
}

/// The table: each row's two templates differ only in the part that row is about.
fn rows() -> Vec<Row> {
    let owner = tag_line("Owner", "platform");
    let tier = tag_line("Tier", "backend");
    vec![
        Row {
            case: "a declared tag's value restated",
            stack: "ecs-td-tag-restated",
            created: yaml(
                "ecs-td-tag-restated-family",
                "256",
                "512",
                IMAGE,
                &tag_line("Owner", "ops"),
            ),
            updated: yaml("ecs-td-tag-restated-family", "256", "512", IMAGE, &owner),
            identity: Identity::Kept,
            tags: &[("Owner", "platform")],
        },
        Row {
            case: "a tag added beside one the update leaves alone",
            stack: "ecs-td-tag-added",
            created: yaml("ecs-td-tag-added-family", "256", "512", IMAGE, &owner),
            updated: yaml(
                "ecs-td-tag-added-family",
                "256",
                "512",
                IMAGE,
                &format!("{owner}{tier}"),
            ),
            identity: Identity::Kept,
            tags: &[("Owner", "platform"), ("Tier", "backend")],
        },
        Row {
            case: "a declared tag removed",
            stack: "ecs-td-tag-removed",
            created: yaml(
                "ecs-td-tag-removed-family",
                "256",
                "512",
                IMAGE,
                &format!("{owner}{tier}"),
            ),
            updated: yaml("ecs-td-tag-removed-family", "256", "512", IMAGE, &owner),
            identity: Identity::Kept,
            tags: &[("Owner", "platform")],
        },
        Row {
            case: "the task size restated, which no call changes on a revision",
            stack: "ecs-td-size-restated",
            created: yaml("ecs-td-size-restated-family", "256", "512", IMAGE, &owner),
            updated: yaml("ecs-td-size-restated-family", "512", "1024", IMAGE, &owner),
            identity: Identity::NextRevision,
            tags: &[("Owner", "platform")],
        },
        Row {
            case: "a container definition's image restated, in JSON",
            stack: "ecs-td-image-restated",
            created: json_template("ecs-td-image-restated-family", IMAGE, "platform"),
            updated: json_template("ecs-td-image-restated-family", OTHER_IMAGE, "platform"),
            identity: Identity::NextRevision,
            tags: &[("Owner", "platform")],
        },
        Row {
            case: "a tag restated together with the task size",
            stack: "ecs-td-size-and-tag",
            created: yaml("ecs-td-size-and-tag-family", "256", "512", IMAGE, &owner),
            updated: yaml(
                "ecs-td-size-and-tag-family",
                "512",
                "1024",
                IMAGE,
                &tag_line("Owner", "payments"),
            ),
            identity: Identity::NextRevision,
            tags: &[("Owner", "payments")],
        },
    ]
}

/// `UpdateStack` with a new template body.
async fn update_stack(router: &MultiServiceRouter, stack: &str, template: &str) -> (u16, String) {
    call(
        router,
        "UpdateStack",
        &[member("StackName", stack), member("TemplateBody", template)],
    )
    .await
}

/// The tags Amazon ECS itself reports for `arn`, as `(key, value)` pairs in key order — read
/// through that service's own `ListTagsForResource`, so what is asserted is the set a client
/// reading the revision the stack published sees.
async fn ecs_tags(router: &MultiServiceRouter, case: &str, arn: &str) -> Vec<(String, String)> {
    let (code, answered) = ecs(router, "ListTagsForResource", &json!({"resourceArn": arn})).await;
    assert_eq!(
        code, 200,
        "case {case:?}: expected HTTP 200 from ListTagsForResource({arn}), got {code} with \
         {answered}"
    );
    let mut held: Vec<(String, String)> = answered["tags"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|tag| {
            Some((
                tag.get("key")?.as_str()?.to_string(),
                tag.get("value")?.as_str()?.to_string(),
            ))
        })
        .collect();
    held.sort();
    held
}

/// The row's expected tag set as the pairs [`ecs_tags`] answers.
fn expected_tags(tags: &[(&str, &str)]) -> Vec<(String, String)> {
    let mut expected: Vec<(String, String)> = tags
        .iter()
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect();
    expected.sort();
    expected
}

/// The `status` Amazon ECS reports for the revision `arn` names, or the empty string where that
/// service answers no revision for it at all.
async fn revision_status(router: &MultiServiceRouter, arn: &str) -> String {
    let (code, answered) = ecs(
        router,
        "DescribeTaskDefinition",
        &json!({"taskDefinition": arn}),
    )
    .await;
    if code != 200 {
        return String::new();
    }
    answered["taskDefinition"]["status"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// Wait for Amazon ECS to report `arn` in `wanted`, which is what the deployment's own cleanup
/// of a superseded revision makes true: the condition is waited FOR, under a deadline that only
/// fails a case whose revision never reaches it.
async fn settled_revision_status(
    router: &MultiServiceRouter,
    case: &str,
    arn: &str,
    wanted: &str,
) {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let reported = revision_status(router, arn).await;
        if reported == wanted {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected Amazon ECS to report {arn} as {wanted}, got {reported:?}"
        );
        tokio::time::sleep(LOOK_AGAIN_AFTER).await;
    }
}

/// The revision number an ARN carries (`…:task-definition/<family>:<revision>`).
fn revision_of(case: &str, arn: &str) -> u64 {
    arn.rsplit_once(':')
        .and_then(|(_, revision)| revision.parse().ok())
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected a task-definition ARN carrying its revision, got \
                 {arn:?}"
            )
        })
}

/// The family segment of such an ARN.
fn family_of(case: &str, arn: &str) -> String {
    let (head, _) = arn
        .rsplit_once(':')
        .unwrap_or_else(|| panic!("case {case:?}: expected a task-definition ARN, got {arn:?}"));
    head.rsplit_once('/')
        .map(|(_, family)| family.to_string())
        .unwrap_or_else(|| panic!("case {case:?}: expected a family in {arn:?}"))
}

#[tokio::test]
async fn an_updated_task_definition_declaration_reaches_amazon_ecs() {
    for row in rows() {
        let case = row.case;
        let router = router();

        let (status, body) = create_stack(&router, row.stack, &row.created, &[]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {status} with {body}"
        );
        let settled = settled_stack_status(case, &router, REGION, row.stack).await;
        assert_eq!(
            settled, CREATE_COMPLETE,
            "case {case:?}: expected the stack to create, got {settled:?} because {:?}",
            stack_event_reasons(&router, REGION, row.stack).await
        );
        let first = physical_resource_id(&router, case, row.stack, "TaskDef").await;
        assert_eq!(
            revision_status(&router, &first).await,
            ACTIVE,
            "case {case:?}: expected the registered revision {first} to be {ACTIVE}"
        );

        let (status, body) = update_stack(&router, row.stack, &row.updated).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from UpdateStack, got {status} with {body}"
        );
        let settled = settled_stack_status(case, &router, REGION, row.stack).await;
        assert_eq!(
            settled, UPDATE_COMPLETE,
            "case {case:?}: expected the stack to update, got {settled:?} because {:?}",
            stack_event_reasons(&router, REGION, row.stack).await
        );

        // WHICH revision the stack now publishes: the one it had, or the next one of the same
        // family.
        let second = physical_resource_id(&router, case, row.stack, "TaskDef").await;
        match row.identity {
            Identity::Kept => assert_eq!(
                second, first,
                "case {case:?}: expected the update to keep the revision {first}, got {second}"
            ),
            Identity::NextRevision => {
                assert_eq!(
                    family_of(case, &second),
                    family_of(case, &first),
                    "case {case:?}: expected the next revision of the same family, got {second}"
                );
                assert_eq!(
                    revision_of(case, &second),
                    revision_of(case, &first) + 1,
                    "case {case:?}: expected the revision after {first}, got {second}"
                );
            }
        }

        // ...and what Amazon ECS itself holds for it: the revision the stack publishes is the
        // one a client can run, carrying the tag set the declaration now asks for.
        assert_eq!(
            revision_status(&router, &second).await,
            ACTIVE,
            "case {case:?}: expected the revision {second} the stack publishes to be {ACTIVE}"
        );
        assert_eq!(
            ecs_tags(&router, case, &second).await,
            expected_tags(row.tags),
            "case {case:?}: expected Amazon ECS to report the declared tags for {second}"
        );

        // The revision a replacement superseded is RETAINED — a deregistered revision "remains
        // discoverable in your account indefinitely" — and out of ACTIVE, which is what the
        // deployment's cleanup leaves behind.
        if row.identity == Identity::NextRevision {
            settled_revision_status(&router, case, &first, INACTIVE).await;
        }
    }
}
