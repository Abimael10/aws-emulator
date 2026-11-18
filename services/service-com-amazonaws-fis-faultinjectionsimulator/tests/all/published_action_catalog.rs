// SPDX-License-Identifier: CC0-1.0

//! The published ACTION catalog: which actions `ListActions` and `GetAction` report, and the
//! description each of them carries, end to end over HTTP.
//!
//! Every assertion here goes through the router, so each case exercises routing,
//! deserialization, the handler and serialization — the same path a client takes.
//!
//! The rule under test is one rule over both readers of the one table:
//!
//! * an `ActionSummary` is published catalog DATA keyed by the action id. Its `description`
//!   is the string the service publishes for that id — the action's own summary, reported
//!   character for character, punctuation included, and never restated in this emulator's
//!   words — and the only per-environment parts of the record are the region and the account
//!   in its `arn`;
//! * the catalog's MEMBERSHIP is published data too: every action the service publishes is
//!   enumerated, so an id it publishes resolves from `GetAction` instead of answering
//!   `ResourceNotFoundException`, and it can be named by an experiment template;
//! * one table is behind both readers, so the listing and the point read answer the same
//!   text for one id, at any page size and however often it is read.
//!
//! SOURCE for the expected text below: the published catalog itself, which the `Action`
//! shape's own model documentation points at — the Smithy model carries neither the
//! catalog's text nor its membership, so the served responses are the authority for both.
//! The rows are written out here rather than imported from `src/state/catalog.rs`, so a
//! restated or missing entry in that table fails this suite instead of agreeing with it.
//! Rows whose text is pinned here are the ones whose published string is known verbatim;
//! the whole-catalog cases below hold over every entry whether or not a row pins its text.
//!
//! What the rows span: text that closes with a period and text that publishes none, a noun
//! phrase and a multi-clause sentence, text naming a wire exception class, one entry with no
//! target and entries with one, ids from ten AWS services, and entries no issue cited
//! (`aws:ec2:reboot-instances`, `aws:ssm:start-automation-execution`). The page sizes run
//! from the published floor past the catalog to the published ceiling.
//!
//! The HTTP harness is the shared one in `experiment_lifecycle.rs`, which this suite reaches
//! for the router, the request helper and the template create; the catalog page walk is its
//! own.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Value};
use service_com_amazonaws_fis_faultinjectionsimulator::state::ACTION_CATALOG;
use smithy_server_runtime::MultiServiceRouter;

use crate::experiment_lifecycle::{
    create_template, emulator, send, token, Answer, FALLBACK_REGION,
};

/// `maxResults`' published bounds on the action listing (`@range(min: 1, max: 100)`).
const MAX_RESULTS_FLOOR: i32 = 1;
/// See [`MAX_RESULTS_FLOOR`].
const MAX_RESULTS_CEILING: i32 = 100;

/// The account every ARN this emulator reports names, spelled out so a changed account
/// field fails here.
const ACCOUNT_ID: &str = "000000000000";

/// A well-formed execution role ARN: no row here is about the execution role.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/fis-published-action-catalog";

/// The name every template this suite creates gives its one target and its one action.
const UNDER_TEST: &str = "underTest";

/// The value a row supplies for a required action parameter.
///
/// `ExperimentTemplateActionParameter` is modelled `@length(0..1024)`, which this satisfies;
/// no row here is about a parameter's own value.
const PARAMETER_VALUE: &str = "PT5M";

/// One published action whose text this suite pins: its id, the description published for
/// it, and the targets it publishes as (key, resource type) pairs.
struct ActionRow {
    id: &'static str,
    description: &'static str,
    targets: &'static [(&'static str, &'static str)],
}

/// The rows whose published description is known verbatim.
///
/// They differ along what the rule generalises over: whether the published text closes with
/// a period (`aws:cloudwatch:assert-alarm-state`) or publishes none
/// (`aws:ebs:pause-volume-io`), whether it is a noun phrase
/// (`aws:directconnect:virtual-interface-disconnect`), one plain sentence
/// (`aws:ec2:reboot-instances`) or a multi-clause one
/// (`aws:dynamodb:global-table-pause-replication`), whether it spells the resource's I/O
/// (`aws:ebs:volume-io-latency`) or its service name, how many targets the action publishes
/// (none for `aws:ssm:start-automation-execution`, one for the rest), and which AWS service
/// the id belongs to. `aws:ec2:reboot-instances` and `aws:ssm:start-automation-execution`
/// are rows no issue cited.
fn pinned_rows() -> Vec<ActionRow> {
    vec![
        ActionRow {
            id: "aws:cloudwatch:assert-alarm-state",
            description: "Asserts that the CloudWatch alarms are in the expected states.",
            targets: &[],
        },
        ActionRow {
            id: "aws:directconnect:virtual-interface-disconnect",
            description: "Direct Connect Failover Test.",
            targets: &[(
                "VirtualInterfaces",
                "aws:directconnect:virtual-interface",
            )],
        },
        ActionRow {
            id: "aws:dsql:cluster-connection-failure",
            description: "Simulates connection failures to a DSQL cluster for the specified \
                          duration.",
            targets: &[("Clusters", "aws:dsql:cluster")],
        },
        ActionRow {
            id: "aws:dynamodb:global-table-pause-replication",
            description: "Disrupts DynamoDB Global Table regional connectivity by pausing \
                          replication and blocking table operations in the affected region.",
            targets: &[("Tables", "aws:dynamodb:global-table")],
        },
        ActionRow {
            id: "aws:ebs:pause-volume-io",
            description: "Pauses IO for a set of EBS volumes",
            targets: &[("Volumes", "aws:ec2:ebs-volume")],
        },
        ActionRow {
            id: "aws:ebs:volume-io-latency",
            description: "Sets latency to EBS volume I/O operations.",
            targets: &[("Volumes", "aws:ec2:ebs-volume")],
        },
        ActionRow {
            id: "aws:ec2:reboot-instances",
            description: "Reboot the specified EC2 instances.",
            targets: &[("Instances", "aws:ec2:instance")],
        },
        ActionRow {
            id: "aws:ssm:start-automation-execution",
            description: "Run the specified SSM automation document.",
            targets: &[],
        },
    ]
}

/// The action ids the catalog published no record for while it was a subset of the published
/// one, each with the resource type its target names.
///
/// They span seven AWS services and four families of fault — a connectivity failover, a
/// connection failure, a storage-latency fault, two wire exceptions injected into a stream, a
/// paused replication, a disrupted VPC endpoint and a cross-Region routing disruption — so
/// the membership rule is asserted over the class rather than over one id.
const REGAINED_ACTIONS: &[(&str, &str)] = &[
    (
        "aws:directconnect:virtual-interface-disconnect",
        "aws:directconnect:virtual-interface",
    ),
    ("aws:dsql:cluster-connection-failure", "aws:dsql:cluster"),
    ("aws:ebs:volume-io-latency", "aws:ec2:ebs-volume"),
    (
        "aws:kinesis:stream-expired-iterator-exception",
        "aws:kinesis:stream",
    ),
    (
        "aws:kinesis:stream-provisioned-throughput-exception",
        "aws:kinesis:stream",
    ),
    (
        "aws:memorydb:multi-region-cluster-pause-replication",
        "aws:memorydb:multi-region-cluster",
    ),
    ("aws:network:disrupt-vpc-endpoint", "aws:ec2:vpc-endpoint"),
    (
        "aws:network:route-table-disrupt-cross-region-connectivity",
        "aws:ec2:subnet",
    ),
];

/// Every action the listing publishes, walked at `page_size` and concatenated in the order
/// the pages reported them, signed for `region`.
async fn walk(router: &MultiServiceRouter, page_size: Option<i32>, region: &str) -> Vec<Value> {
    let case = format!("maxResults={page_size:?} in {region}");
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
            "/actions".to_string()
        } else {
            format!("/actions?{}", query.join("&"))
        };
        let answer = send(router, "GET", &uri, None, region).await;
        assert_eq!(
            answer.status, 200,
            "case {case:?} page {page}: expected HTTP 200 from ListActions, got {} with {:?}",
            answer.status, answer.body
        );
        let items = answer
            .member("actions")
            .as_array()
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?} page {page}: expected an actions list, got {:?}",
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

/// The listed summary of `id`.
fn listed<'a>(case: &str, listing: &'a [Value], id: &str) -> &'a Value {
    listing
        .iter()
        .find(|entry| entry["id"].as_str() == Some(id))
        .unwrap_or_else(|| {
            panic!("case {case:?}: the listing does not publish {id:?}: {listing:?}")
        })
}

/// What `GetAction` answers for `id`, signed for `region`.
async fn get_action(router: &MultiServiceRouter, id: &str, region: &str) -> Answer {
    send(router, "GET", &format!("/actions/{id}"), None, region).await
}

/// The (key, resource type) pairs a reported `targets` map carries, sorted by key.
fn target_pairs(case: &str, reported: &Value) -> Vec<(String, String)> {
    reported
        .as_object()
        .unwrap_or_else(|| panic!("case {case:?}: expected a targets map, got {reported:?}"))
        .iter()
        .map(|(key, target)| {
            let resource_type = target["resourceType"]
                .as_str()
                .unwrap_or_else(|| {
                    panic!("case {case:?}: target {key:?} names no resourceType: {target:?}")
                })
                .to_string();
            (key.clone(), resource_type)
        })
        .collect()
}

/// The pairs a row expects, in the same form [`target_pairs`] reports.
fn expected_pairs(targets: &[(&str, &str)]) -> Vec<(String, String)> {
    let mut pairs: Vec<(String, String)> = targets
        .iter()
        .map(|(key, resource_type)| ((*key).to_string(), (*resource_type).to_string()))
        .collect();
    pairs.sort();
    pairs
}

/// A create body whose one target is of `resource_type` and whose one action is `action_id`
/// with `parameters` supplied.
fn template_body(label: &str, resource_type: &str, action_id: &str, parameters: Value) -> Value {
    let mut segments = resource_type.split(':').skip(1);
    let service = segments.next().unwrap_or("fis");
    let kind = segments.next().unwrap_or("resource");
    json!({
        "clientToken": token(&label.replace([' ', ':'], "-")),
        "description": format!("published action catalog: {label}"),
        "roleArn": ROLE_ARN,
        "stopConditions": [{ "source": "none" }],
        "targets": { UNDER_TEST: {
            "resourceType": resource_type,
            "resourceArns": [format!(
                "arn:aws:{service}:{FALLBACK_REGION}:{ACCOUNT_ID}:{kind}/under-test"
            )],
            "selectionMode": "ALL",
        } },
        "actions": { UNDER_TEST: { "actionId": action_id, "parameters": parameters } },
    })
}

#[tokio::test]
async fn a_published_action_reports_its_published_text_from_both_readers() {
    let router = emulator();
    // Walked at a page size that cuts the catalog into several pages, so a row's text is the
    // text the paginated path serves too.
    let listing = walk(&router, Some(6), FALLBACK_REGION).await;

    for row in pinned_rows() {
        let case = row.id;
        let summary = listed(case, &listing, row.id);
        assert_eq!(
            summary["description"].as_str(),
            Some(row.description),
            "case {case:?}: expected the listing to publish the description {:?}, got {:?}",
            row.description,
            summary["description"]
        );
        assert_eq!(
            target_pairs(case, &summary["targets"]),
            expected_pairs(row.targets),
            "case {case:?}: expected the listing to publish the targets {:?}, got {:?}",
            row.targets,
            summary["targets"]
        );

        let answer = get_action(&router, row.id, FALLBACK_REGION).await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected HTTP 200 from GetAction, got {} with {:?}",
            answer.status, answer.body
        );
        let action = answer.member("action");
        assert_eq!(
            action["description"].as_str(),
            Some(row.description),
            "case {case:?}: expected the point read to report the description {:?}, got {:?}",
            row.description,
            action["description"]
        );
        assert_eq!(
            target_pairs(case, &action["targets"]),
            expected_pairs(row.targets),
            "case {case:?}: expected the point read to report the targets {:?}, got {:?}",
            row.targets,
            action["targets"]
        );
    }
}

#[tokio::test]
async fn the_catalog_publishes_every_action_it_once_left_out() {
    let router = emulator();
    // Each row is a page size: the published floor, one that cuts the catalog into several
    // pages, one past the whole catalog, the published ceiling, and no `maxResults` at all.
    // Membership is published data, so it does not depend on how the client pages it.
    let catalog_size = i32::try_from(ACTION_CATALOG.len()).expect("the catalog size fits an i32");
    for page_size in [
        Some(MAX_RESULTS_FLOOR),
        Some(9),
        Some(catalog_size + 1),
        Some(MAX_RESULTS_CEILING),
        None,
    ] {
        let listing = walk(&router, page_size, FALLBACK_REGION).await;
        for (id, resource_type) in REGAINED_ACTIONS {
            let case = format!("{id} at maxResults={page_size:?}");
            let summary = listed(&case, &listing, id);
            assert!(
                summary["description"]
                    .as_str()
                    .is_some_and(|text| !text.is_empty()),
                "case {case:?}: expected the published entry to carry a description, got {summary:?}"
            );
            let pairs = target_pairs(&case, &summary["targets"]);
            assert!(
                pairs
                    .iter()
                    .any(|(_, published)| published == resource_type),
                "case {case:?}: expected a target of resource type {resource_type:?}, got \
                 {pairs:?}"
            );

            // The same id resolves from the point read, which is what an absent entry could
            // not do: it answered ResourceNotFoundException for an action the service
            // publishes.
            let answer = get_action(&router, id, FALLBACK_REGION).await;
            assert_eq!(
                answer.status, 200,
                "case {case:?}: expected HTTP 200 from GetAction, got {} with {:?}",
                answer.status, answer.body
            );
            let action = answer.member("action");
            assert_eq!(
                action["description"], summary["description"],
                "case {case:?}: expected the point read to report the listed description {:?}, \
                 got {:?}",
                summary["description"], action["description"]
            );
            assert_eq!(
                action["arn"].as_str().unwrap_or_default(),
                format!("arn:aws:fis:{FALLBACK_REGION}:{ACCOUNT_ID}:action/{id}"),
                "case {case:?}: expected the arn to name the signed region and the serving \
                 account, got {:?}",
                action["arn"]
            );
            assert!(
                action["tags"]
                    .as_object()
                    .is_some_and(serde_json::Map::is_empty),
                "case {case:?}: expected no tags on a service-owned action, got {:?}",
                action["tags"]
            );
        }
    }
}

#[tokio::test]
async fn every_published_action_carries_its_own_text_from_both_readers() {
    // The class the rule holds over is the whole catalog, so this case is driven from the
    // listing itself rather than from a table: whatever the listing publishes, the point read
    // answers the same record for it, each entry's text is its own, and reading it again
    // changes nothing.
    let router = emulator();
    let listing = walk(&router, Some(7), FALLBACK_REGION).await;
    assert_eq!(
        listing.len(),
        ACTION_CATALOG.len(),
        "case membership: expected the listing to publish every catalogued action, got {}",
        listing.len()
    );

    let mut by_description: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for entry in &listing {
        let id = entry["id"]
            .as_str()
            .unwrap_or_else(|| panic!("a listed action carries no id: {entry:?}"))
            .to_string();
        let description = entry["description"]
            .as_str()
            .unwrap_or_else(|| panic!("case {id:?}: the listed action carries no description"))
            .to_string();
        assert!(
            !description.is_empty() && description.trim() == description,
            "case {id:?}: expected published text with no padding, got {description:?}"
        );
        by_description
            .entry(description)
            .or_default()
            .push(id.clone());

        let first = get_action(&router, &id, FALLBACK_REGION).await;
        assert_eq!(
            first.status, 200,
            "case {id:?}: expected HTTP 200 from GetAction for a listed action, got {} with {:?}",
            first.status, first.body
        );
        assert_eq!(
            first.member("action")["description"],
            entry["description"],
            "case {id:?}: expected the point read to report the listed description {:?}, got {:?}",
            entry["description"],
            first.member("action")["description"]
        );
        let second = get_action(&router, &id, FALLBACK_REGION).await;
        assert_eq!(
            second.body, first.body,
            "case {id:?}: expected a repeated read to report the same action, got {:?} then {:?}",
            first.body, second.body
        );
    }

    // Published text is keyed by the action id: two entries sharing one string would be a
    // restatement of one of them rather than the text published for it.
    let shared: Vec<(&String, &Vec<String>)> = by_description
        .iter()
        .filter(|(_, ids)| ids.len() > 1)
        .collect();
    assert!(
        shared.is_empty(),
        "case distinct-text: expected each published action to carry its own description, got \
         {shared:?}"
    );

    // Reading the catalog changes nothing a later caller sees.
    let again = walk(&router, Some(7), FALLBACK_REGION).await;
    assert_eq!(
        again, listing,
        "case repeat: expected the same listing on a second read, got {again:?} after {listing:?}"
    );
}

#[tokio::test]
async fn a_published_action_is_reported_for_the_region_the_request_names() {
    // The only per-environment parts of a published record are the region and the account in
    // its arn: the text and the membership are the same whatever the request is signed for.
    let router = emulator();
    let reference: BTreeSet<(String, String)> = walk(&router, None, FALLBACK_REGION)
        .await
        .iter()
        .map(|entry| {
            (
                entry["id"].as_str().unwrap_or_default().to_string(),
                entry["description"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect();

    for (signed, expected) in [
        ("eu-central-1", "eu-central-1"),
        ("ap-south-2", "ap-south-2"),
        ("us-gov-east-1", "us-gov-east-1"),
        ("not a region", FALLBACK_REGION),
    ] {
        let listing = walk(&router, Some(11), signed).await;
        let served: BTreeSet<(String, String)> = listing
            .iter()
            .map(|entry| {
                (
                    entry["id"].as_str().unwrap_or_default().to_string(),
                    entry["description"].as_str().unwrap_or_default().to_string(),
                )
            })
            .collect();
        assert_eq!(
            served, reference,
            "case {signed:?}: expected the same published text and membership, got {served:?}"
        );
        for entry in &listing {
            let id = entry["id"].as_str().unwrap_or_default();
            assert_eq!(
                entry["arn"].as_str().unwrap_or_default(),
                format!("arn:aws:fis:{expected}:{ACCOUNT_ID}:action/{id}"),
                "case {signed:?} ({id:?}): expected the arn to name {expected}, got {:?}",
                entry["arn"]
            );
        }
    }
}

#[tokio::test]
async fn an_action_the_catalog_gained_can_be_named_by_a_template() {
    // Membership is backed, not just listed: an id the catalog publishes is one
    // CreateExperimentTemplate admits, and the stored template names it. The rows are the
    // regained entries, each with the parameters its published contract requires and a target
    // of the type it publishes.
    for (id, resource_type) in REGAINED_ACTIONS {
        let router = emulator();
        let entry = ACTION_CATALOG
            .iter()
            .find(|entry| entry.id == *id)
            .unwrap_or_else(|| panic!("case {id:?}: the catalog publishes this action"));
        let mut parameters = serde_json::Map::new();
        for parameter in entry.parameters.iter().filter(|parameter| parameter.required) {
            parameters.insert(parameter.name.to_string(), json!(PARAMETER_VALUE));
        }

        let answer = create_template(
            &router,
            template_body(id, resource_type, id, Value::Object(parameters)),
        )
        .await;
        assert_eq!(
            answer.status, 200,
            "case {id:?}: expected a template naming a published action to be created, got {} \
             with {:?}",
            answer.status, answer.body
        );
        let template_id = answer.member("experimentTemplate")["id"]
            .as_str()
            .unwrap_or_else(|| panic!("case {id:?}: no template id on {:?}", answer.body))
            .to_string();

        let stored = send(
            &router,
            "GET",
            &format!("/experimentTemplates/{template_id}"),
            None,
            FALLBACK_REGION,
        )
        .await;
        assert_eq!(
            stored.status, 200,
            "case {id:?}: expected HTTP 200 from GetExperimentTemplate, got {} with {:?}",
            stored.status, stored.body
        );
        assert_eq!(
            stored.member("experimentTemplate")["actions"][UNDER_TEST]["actionId"].as_str(),
            Some(*id),
            "case {id:?}: expected the stored action to name the published id, got {:?}",
            stored.member("experimentTemplate")["actions"]
        );
    }
}
