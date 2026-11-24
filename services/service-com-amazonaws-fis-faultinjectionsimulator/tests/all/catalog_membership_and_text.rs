// SPDX-License-Identifier: CC0-1.0

//! The MEMBERSHIP of FIS's two service-owned catalogs and the TEXT every entry of them
//! carries, end to end over HTTP.
//!
//! Every assertion here goes through the router, so each case exercises routing,
//! deserialization, the handler and serialization — the same path a client takes.
//!
//! The rule under test: a catalog entry is fixed published data keyed by its id. The set the
//! type listing enumerates, the order it enumerates it in and the description each entry of
//! either catalog carries are the service's published values, so a reader reports them as
//! published — character for character, punctuation included — leaves nothing out of the
//! listing, and answers the same text for one id from the listing and from the point read.
//!
//! A second rule the two tables stand in: each is its own published list, and neither
//! contains the other. A resource type a published action targets is one an experiment
//! template may name, whether or not the type listing enumerates it; a type the listing
//! enumerates is published whether or not a catalogued action targets it.
//!
//! What the rows span: the WHOLE published type listing, written out id by id with the
//! description published for it, so the index a client reads any entry at is asserted and
//! the few entries an issue cited are a handful among all of them; action rows that take no
//! parameter, one and several, across five AWS services; page sizes from the published floor
//! through one past the catalog to the published ceiling; ids that depart from a published
//! one by case, by a plural, by a type the listing does not carry and by an unpublished
//! service; and every distinct resource type the action table targets, driven from that
//! table rather than from a list written here.
//!
//! SOURCE for the expected membership and text: the published catalogs themselves — the
//! action catalog the `Action` shape's model documentation points at
//! (<https://docs.aws.amazon.com/fis/latest/userguide/fis-actions-reference.html>) and the
//! published type listing, whose membership, order and description strings the Smithy model
//! does not carry. They are written out here rather than imported from
//! `src/state/catalog.rs`, so a restated or missing row in that table fails this suite
//! instead of agreeing with it.
//!
//! The HTTP harness is the shared one in `experiment_lifecycle.rs`, which this suite reaches
//! for the router, the request helper and the template create; the catalog page walk below is
//! its own.

use std::collections::BTreeSet;

use serde_json::{json, Map, Value};
use service_com_amazonaws_fis_faultinjectionsimulator::state::{
    ACTION_CATALOG, TARGET_RESOURCE_TYPE_CATALOG,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::experiment_lifecycle::{create_template, emulator, send, token, FALLBACK_REGION};

/// `maxResults`' published bounds on both catalog listings (`@range(min: 1, max: 100)`).
const MAX_RESULTS_FLOOR: i32 = 1;
/// See [`MAX_RESULTS_FLOOR`].
const MAX_RESULTS_CEILING: i32 = 100;

/// The published target resource type listing: every id it enumerates, in the order it
/// enumerates them, each with the description published for that id.
///
/// The text is the published value and not a restatement of it: most rows end in a period
/// and four publish none, a row names its service the way the listing names it (spelled out,
/// abbreviated, or not at all), and one row's text is a whole sentence about what the type
/// is for rather than a noun phrase.
const PUBLISHED_TYPES: &[(&str, &str)] = &[
    (
        "aws:arc:zonal-shift-managed-resource",
        "An ARC Zonal Shift Managed Resource.",
    ),
    (
        "aws:directconnect:virtual-interface",
        "A Direct Connect Virtual Interface (VIF).",
    ),
    ("aws:dsql:cluster", "An Amazon Aurora DSQL cluster."),
    ("aws:dynamodb:global-table", "An Amazon DynamoDB global table"),
    (
        "aws:ec2:autoscaling-group",
        "An Amazon EC2 Auto Scaling group",
    ),
    (
        "aws:ec2:ebs-volume",
        "An Amazon Elastic Block Store volume",
    ),
    ("aws:ec2:instance", "An Amazon EC2 instance."),
    ("aws:ec2:spot-instance", "An Amazon EC2 Spot Instance."),
    ("aws:ec2:subnet", "An Amazon EC2 subnet."),
    ("aws:ec2:transit-gateway", "An Amazon EC2 Transit Gateway."),
    (
        "aws:ec2:vpc-endpoint",
        "A Virtual Private Cloud (VPC) endpoint.",
    ),
    ("aws:ecs:cluster", "An Amazon ECS cluster."),
    ("aws:ecs:task", "An Amazon ECS task."),
    ("aws:eks:cluster", "An Amazon EKS cluster."),
    ("aws:eks:nodegroup", "An Amazon EKS node group."),
    ("aws:eks:pod", "A Kubernetes pod."),
    (
        "aws:elasticache:replicationgroup",
        "An ElastiCache Replication Group.",
    ),
    ("aws:iam:role", "An IAM role."),
    (
        "aws:kinesis:stream",
        "AWS Kinesis Data Stream target for the FIS actions.",
    ),
    ("aws:lambda:function", "A Lambda Function."),
    (
        "aws:memorydb:multi-region-cluster",
        "A MemoryDB multi-region cluster.",
    ),
    ("aws:rds:cluster", "An Amazon Aurora DB cluster."),
    ("aws:rds:db", "An Amazon RDS DB instance."),
    ("aws:s3:bucket", "An Amazon S3 bucket"),
];

/// Published ACTION rows: the description published for each of these action ids.
///
/// They differ along what the rule generalises over: how many parameters the action takes
/// (none, one, two or more), whether the published text opens with an assertion, an
/// imperative, an `It ` or a noun, and which AWS service the id belongs to. Only the third
/// row was cited by an issue.
const PUBLISHED_ACTION_TEXT: &[(&str, &str)] = &[
    (
        "aws:cloudwatch:assert-alarm-state",
        "Asserts that the CloudWatch alarms are in the expected states.",
    ),
    (
        "aws:ebs:pause-volume-io",
        "Pauses IO for a set of EBS volumes",
    ),
    (
        "aws:ecs:task-network-latency",
        "It adds latency, with jitter, to outgoing or incoming traffic from a configurable \
         list of sources (Supported: IPv4, IPv4/CIDR, domain name, DYNAMODB|S3).",
    ),
    (
        "aws:eks:pod-memory-stress",
        "Runs memory stress on the target pods.",
    ),
    (
        "aws:rds:reboot-db-instances",
        "Reboot the specified DB instances.",
    ),
    ("aws:ssm:send-command", "Run the specified SSM document."),
];

/// The name every template this suite creates gives its one target.
const TARGET: &str = "underTest";

/// A well-formed execution role ARN: no row here is about the execution role.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/fis-catalog-membership";

/// The value a row supplies for a required target parameter.
/// `ExperimentTemplateTargetParameterValue` is modelled `@length(0..1024)`,
/// `pattern ^[\S]+$`, which this satisfies.
const PARAMETER_VALUE: &str = "placeholder";

/// The `duration` the one action every template here carries: inside the 1 to 720 minutes
/// FIS documents for `aws:fis:wait`.
const WAIT: &str = "PT5M";

/// Every entry of one paginated catalog, walked at `page_size` and concatenated in the order
/// the pages reported them.
async fn walk(
    router: &MultiServiceRouter,
    path: &str,
    member: &str,
    page_size: Option<i32>,
) -> Vec<Value> {
    let case = format!("{path} at maxResults={page_size:?}");
    let mut collected: Vec<Value> = Vec::new();
    let mut next: Option<String> = None;
    for page in 0..=256 {
        let mut query: Vec<String> = Vec::new();
        if let Some(size) = page_size {
            query.push(format!("maxResults={size}"));
        }
        if let Some(token) = &next {
            query.push(format!("nextToken={token}"));
        }
        let uri = if query.is_empty() {
            path.to_string()
        } else {
            format!("{path}?{}", query.join("&"))
        };
        let answer = send(router, "GET", &uri, None, FALLBACK_REGION).await;
        assert_eq!(
            answer.status, 200,
            "case {case:?} page {page}: expected HTTP 200, got {} with {:?}",
            answer.status, answer.body
        );
        let items = answer
            .member(member)
            .as_array()
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?} page {page}: expected a {member} list, got {:?}",
                    answer.body
                )
            })
            .clone();
        collected.extend(items);
        match answer.body["nextToken"].as_str() {
            None => return collected,
            Some(token) => next = Some(token.to_string()),
        }
    }
    panic!("case {case:?}: pagination did not terminate");
}

/// The (id, description) pairs a listing reported, read off the member each summary names
/// its id by.
fn pairs(case: &str, listed: &[Value], id_member: &str) -> Vec<(String, String)> {
    listed
        .iter()
        .map(|entry| {
            let id = entry[id_member]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: no {id_member} in {entry:?}"))
                .to_string();
            let description = entry["description"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: no description in {entry:?}"))
                .to_string();
            (id, description)
        })
        .collect()
}

/// The published rows as the pairs a listing is compared against.
fn expected_pairs(rows: &[(&str, &str)]) -> Vec<(String, String)> {
    rows.iter()
        .map(|(id, description)| ((*id).to_string(), (*description).to_string()))
        .collect()
}

/// An ARN for a resource of `resource_type`, built from the type's own `aws:<service>:<kind>`
/// id so a target names a resource of the type it declares.
fn resource_arn(resource_type: &str) -> String {
    let mut segments = resource_type.split(':').skip(1);
    let service = segments.next().unwrap_or("fis");
    let kind = segments.next().unwrap_or("resource");
    format!("arn:aws:{service}:{FALLBACK_REGION}:000000000000:{kind}/under-test")
}

/// A create body whose one target is of `resource_type`, carrying `parameters` when the row
/// supplies any. The action is the built-in wait, which needs no target, so the call is
/// judged on its target alone.
fn template_body(label: &str, resource_type: &str, parameters: &[&str]) -> Value {
    let mut target = Map::new();
    target.insert("resourceType".to_string(), json!(resource_type));
    target.insert(
        "resourceArns".to_string(),
        json!([resource_arn(resource_type)]),
    );
    target.insert("selectionMode".to_string(), json!("ALL"));
    if !parameters.is_empty() {
        let mut supplied = Map::new();
        for name in parameters {
            supplied.insert((*name).to_string(), json!(PARAMETER_VALUE));
        }
        target.insert("parameters".to_string(), Value::Object(supplied));
    }
    json!({
        "clientToken": token(&label.replace([' ', ':'], "-")),
        "description": format!("catalog membership: {label}"),
        "roleArn": ROLE_ARN,
        "stopConditions": [{ "source": "none" }],
        "targets": { TARGET: Value::Object(target) },
        "actions": { "wait": { "actionId": "aws:fis:wait", "parameters": { "duration": WAIT } } }
    })
}

#[tokio::test]
async fn the_type_listing_publishes_every_entry_with_its_published_text_at_every_page_size() {
    let router = emulator();
    let expected = expected_pairs(PUBLISHED_TYPES);

    // Each row is a page size: the published floor, a size that cuts the catalog into
    // several pages, exactly the catalog, one past it, the published ceiling, and no
    // `maxResults` at all.
    let catalog_size = i32::try_from(PUBLISHED_TYPES.len()).expect("the catalog size fits an i32");
    for page_size in [
        Some(MAX_RESULTS_FLOOR),
        Some(7),
        Some(catalog_size),
        Some(catalog_size + 1),
        Some(MAX_RESULTS_CEILING),
        None,
    ] {
        let case = format!("maxResults={page_size:?}");
        let listed = walk(&router, "/targetResourceTypes", "targetResourceTypes", page_size).await;
        let served = pairs(&case, &listed, "resourceType");
        assert_eq!(
            served, expected,
            "case {case:?}: expected the published listing {expected:?}, got {served:?}"
        );
    }
}

#[tokio::test]
async fn every_published_type_answers_its_published_text_from_the_point_read() {
    let router = emulator();

    for (id, description) in PUBLISHED_TYPES {
        let case = *id;
        let answer = send(
            &router,
            "GET",
            &format!("/targetResourceTypes/{id}"),
            None,
            FALLBACK_REGION,
        )
        .await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected HTTP 200 reading a published type, got {} with {:?}",
            answer.status, answer.body
        );
        let reported = answer.member("targetResourceType");
        assert_eq!(
            reported["resourceType"].as_str(),
            Some(*id),
            "case {case:?}: expected the read to report the id it was asked for, got {reported:?}"
        );
        assert_eq!(
            reported["description"].as_str(),
            Some(*description),
            "case {case:?}: expected the published description {description:?}, got {:?}",
            reported["description"]
        );
    }
}

#[tokio::test]
async fn an_id_the_type_listing_does_not_publish_is_refused() {
    let router = emulator();

    // Each row departs from a published id in one way: a plural, upper case, a published
    // service with a type it publishes no entry for, and a service the listing carries no
    // type for at all.
    for (case, id) in [
        ("a published id pluralised", "aws:kinesis:streams"),
        ("a published id in upper case", "AWS:S3:BUCKET"),
        (
            "a published service with an unpublished type",
            "aws:memorydb:cluster",
        ),
        ("an unpublished service", "aws:timestream:database"),
    ] {
        let answer = send(
            &router,
            "GET",
            &format!("/targetResourceTypes/{id}"),
            None,
            FALLBACK_REGION,
        )
        .await;
        assert_eq!(
            answer.status, 400,
            "case {case:?} ({id:?}): expected HTTP 400, got {} with {:?}",
            answer.status, answer.body
        );
        assert_eq!(
            answer.error_code(),
            "ValidationException",
            "case {case:?} ({id:?}): expected ValidationException, got {:?}",
            answer.body
        );
    }
}

#[tokio::test]
async fn an_action_answers_its_published_description_from_both_readers() {
    let router = emulator();
    let listed = walk(&router, "/actions", "actions", Some(5)).await;
    let published = pairs("actions", &listed, "id");

    for (id, description) in PUBLISHED_ACTION_TEXT {
        let case = *id;
        let listed_text = published
            .iter()
            .find(|(listed_id, _)| listed_id == id)
            .map(|(_, text)| text.as_str());
        assert_eq!(
            listed_text,
            Some(*description),
            "case {case:?}: expected the listing to publish {description:?}, got \
             {listed_text:?}"
        );

        let answer = send(&router, "GET", &format!("/actions/{id}"), None, FALLBACK_REGION).await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected HTTP 200 reading a published action, got {} with {:?}",
            answer.status, answer.body
        );
        let action = answer.member("action");
        assert_eq!(
            action["description"].as_str(),
            Some(*description),
            "case {case:?}: expected the read to report {description:?}, got {:?}",
            action["description"]
        );
    }
}

#[tokio::test]
async fn a_resource_type_a_published_action_targets_can_be_named_by_a_template() {
    // Driven from the action table, so this covers every type a published action targets —
    // including one the type listing does not enumerate, which is targetable all the same
    // because an action published for it names it.
    let router = emulator();
    let targeted: BTreeSet<&str> = ACTION_CATALOG
        .iter()
        .flat_map(|action| action.targets.iter().map(|target| target.resource_type))
        .collect();
    assert!(
        !targeted.is_empty(),
        "case action-targets: the published action table must target some resource type"
    );

    for resource_type in targeted {
        let case = resource_type;
        // A published type's own contract decides which parameters its target has to carry;
        // a type the listing does not publish has no such contract and carries none.
        let required: Vec<&str> = TARGET_RESOURCE_TYPE_CATALOG
            .iter()
            .find(|entry| entry.id == resource_type)
            .map(|entry| {
                entry
                    .parameters
                    .iter()
                    .filter(|parameter| parameter.required)
                    .map(|parameter| parameter.name)
                    .collect()
            })
            .unwrap_or_default();

        let answer = create_template(&router, template_body(case, resource_type, &required)).await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected a template naming a type a published action targets to \
             be created, got {} with {:?}",
            answer.status, answer.body
        );
        let id = answer.member("experimentTemplate")["id"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: no id on {:?}", answer.body))
            .to_string();

        // Read back through the implemented reader: the stored target names the type the
        // create was given.
        let stored = send(
            &router,
            "GET",
            &format!("/experimentTemplates/{id}"),
            None,
            FALLBACK_REGION,
        )
        .await;
        assert_eq!(
            stored.status, 200,
            "case {case:?}: expected HTTP 200 from GetExperimentTemplate, got {} with {:?}",
            stored.status, stored.body
        );
        let target = &stored.member("experimentTemplate")["targets"][TARGET];
        assert_eq!(
            target["resourceType"].as_str(),
            Some(resource_type),
            "case {case:?}: expected the stored target to name the resource type it was \
             created with, got {target:?}"
        );
    }
}
