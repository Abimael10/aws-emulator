// SPDX-License-Identifier: CC0-1.0

//! Whether the LIST member a configuration item is MODELLED with is there to be read.
//!
//! The rule under test: **a modelled list member of a configuration item is reported as the
//! list the item holds — every entry of it, or the EMPTY list where there are none — never
//! as an absent member and never as one entry of it. So `relationships`
//! (`ConfigurationItem$relationships`, target `RelationshipList`, "a list of related Amazon
//! Web Services resources", from
//! `smithy_query(query="shape", shape="BaseConfigurationItem,ConfigurationItem")`) is
//! present on EVERY configuration item this emulator reports — every item of a resource's
//! history, the single aggregate read's item, and the row a query that SELECTS the member
//! answers — so a client may read it without first asking whether it is there. Only a read
//! whose output shape declares no such member leaves it out: `BaseConfigurationItem`, which
//! is what the batched read answers, has no `relationships` member at all.**
//!
//! The empty list is what the first test's rows are about, because an empty list is the
//! value a member is easiest to drop: each row's resource is one the recorder observed no
//! related resource for. The second test is the counter-case — a resource the recorder DID
//! observe links for — which is what keeps the member from being a list that is always
//! empty, and what tells a list reported WHOLE from one reported as its first entry.
//!
//! The rows differ along what the rule generalises over: the resource type the item is
//! about, the form of the resource within that type, which STATE of the resource the item
//! records (its discovery, a state it changed into, and its deletion), how many links the
//! recorder observed for it, and which read reports it. Every item asserted on is one a
//! RUNNING configuration recorder observed through the owning service's own API, through
//! the harnesses `config_recorded_resource_compliance` (Config, Amazon SQS and S3 on one
//! engine) and `conformance_pack_rule_evaluation` (Config and IAM).

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::config_recorded_resource_compliance::{Emulator, ACCOUNT_ID, REGION};
use crate::conformance_pack_rule_evaluation::Emulator as ConfigOverIam;

/// The resource types the rows below record, entries of
/// `com.amazonaws.configservice#ResourceType`.
const QUEUE_RESOURCE_TYPE: &str = "AWS::SQS::Queue";
/// See [`QUEUE_RESOURCE_TYPE`].
const BUCKET_RESOURCE_TYPE: &str = "AWS::S3::Bucket";
/// See [`QUEUE_RESOURCE_TYPE`]: the type whose items carry the relationships the
/// non-empty case below is about.
const POLICY_RESOURCE_TYPE: &str = "AWS::IAM::Policy";

/// The `ConfigurationItemStatus` of the item that records a resource is gone
/// (`com.amazonaws.configservice#ConfigurationItemStatus`).
const RESOURCE_DELETED: &str = "ResourceDeleted";

/// The aggregator every row reads the single aggregate item through.
const AGGREGATOR: &str = "every-recorded-resource";

/// How long a poll waits for the recording pass the clock owes. A pass falls due a
/// fraction of a second after the call that makes it due, so this is hundreds of passes'
/// worth of slack and not a span the behaviour depends on: the wait ends on the CONDITION,
/// and the budget is only there so a wedged emulator fails instead of hanging.
const CONVERGENCE_BUDGET: Duration = Duration::from_secs(180);

/// How long a poll waits between two reads.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// Which resource a row records, and in which form.
#[derive(Debug, Clone, Copy)]
enum Recorded {
    /// A standard Amazon SQS queue.
    Queue,
    /// A FIFO queue, which carries the attributes only a FIFO queue has.
    FifoQueue,
    /// An Amazon S3 bucket, a resource of another service and another type.
    Bucket,
}

/// Create the row's resource through its OWN service's API and answer the
/// `(resourceType, resourceId)` Config records it under.
async fn create_resource(
    case: &str,
    emulator: &Emulator,
    recorded: Recorded,
    name: &str,
) -> (String, String) {
    match recorded {
        Recorded::Queue | Recorded::FifoQueue => {
            let fifo = matches!(recorded, Recorded::FifoQueue);
            let queue_name = if fifo {
                format!("{name}.fifo")
            } else {
                name.to_string()
            };
            let mut request = json!({ "QueueName": queue_name });
            if fifo {
                request["Attributes"] = json!({ "FifoQueue": "true" });
            }
            let created = emulator.sqs("CreateQueue", &request).await;
            let url = created["QueueUrl"]
                .as_str()
                .unwrap_or_else(|| {
                    panic!("case {case:?}: CreateQueue must answer a QueueUrl, got {created}")
                })
                .to_string();
            (QUEUE_RESOURCE_TYPE.to_string(), url)
        }
        Recorded::Bucket => {
            emulator.create_bucket(name, &[]).await;
            (BUCKET_RESOURCE_TYPE.to_string(), name.to_string())
        }
    }
}

/// Move the row's resource into another state through its own service's API, which is what
/// makes the recorder record a FURTHER configuration item for it.
async fn change_resource(emulator: &Emulator, recorded: Recorded, resource_id: &str) {
    match recorded {
        Recorded::Queue | Recorded::FifoQueue => {
            emulator
                .sqs(
                    "SetQueueAttributes",
                    &json!({
                        "QueueUrl": resource_id,
                        "Attributes": { "VisibilityTimeout": "41" },
                    }),
                )
                .await;
        }
        Recorded::Bucket => emulator.set_versioning(resource_id, "Enabled").await,
    }
}

/// Take the row's resource away in its own service, which is what makes the recorder
/// record the item that says it is gone.
async fn remove_resource(case: &str, emulator: &Emulator, recorded: Recorded, resource_id: &str) {
    match recorded {
        Recorded::Queue | Recorded::FifoQueue => {
            emulator
                .sqs("DeleteQueue", &json!({ "QueueUrl": resource_id }))
                .await;
        }
        Recorded::Bucket => {
            emulator
                .s3()
                .delete_bucket()
                .bucket(resource_id)
                .send()
                .await
                .unwrap_or_else(|error| {
                    panic!("case {case:?}: deleting {resource_id} must succeed, got {error:?}")
                });
        }
    }
}

/// The configuration items `GetResourceConfigHistory` reports for one resource key, newest
/// first — empty while Config has recorded no item for the key, which that read answers by
/// refusing.
async fn history(
    case: &str,
    emulator: &Emulator,
    resource_type: &str,
    resource_id: &str,
) -> Vec<Value> {
    let (status, body) = emulator
        .call(
            "config",
            "StarlingDoveService",
            "1.1",
            "GetResourceConfigHistory",
            &json!({ "resourceType": resource_type, "resourceId": resource_id }),
        )
        .await;
    if status == 400 {
        return Vec::new();
    }
    assert_eq!(
        status, 200,
        "case {case:?}: GetResourceConfigHistory must answer 200, got {status} with {body}"
    );
    body["configurationItems"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// Read the resource's history until `settled` holds of the items it reports, and answer
/// them. What is waited for is the pass the recorder owes, never a span.
async fn await_history<P>(
    case: &str,
    emulator: &Emulator,
    resource_type: &str,
    resource_id: &str,
    wanted: &str,
    settled: P,
) -> Vec<Value>
where
    P: Fn(&[Value]) -> bool,
{
    let deadline = Instant::now() + CONVERGENCE_BUDGET;
    loop {
        let items = history(case, emulator, resource_type, resource_id).await;
        if settled(&items) {
            return items;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {wanted} for {resource_id}, got {items:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The `configurationItemStatus` of each item, newest first.
fn statuses(items: &[Value]) -> Vec<String> {
    items
        .iter()
        .map(|item| {
            item["configurationItemStatus"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}

/// Every item in `items` carries `relationships` as a LIST — present, whatever it holds —
/// and every entry in it names the related resource.
fn assert_relationships_are_a_list(case: &str, read: &str, items: &[Value]) {
    assert!(
        !items.is_empty(),
        "case {case:?}: {read}: expected at least one configuration item to read"
    );
    for item in items {
        let member = item.get("relationships");
        let links = member.unwrap_or_else(|| {
            panic!(
                "case {case:?}: {read}: expected the modelled relationships member on every \
                 configuration item, got {item}"
            )
        });
        let links = links.as_array().unwrap_or_else(|| {
            panic!("case {case:?}: {read}: expected relationships to be a list, got {links}")
        });
        for link in links {
            assert!(
                link.get("resourceType").is_some() && link.get("resourceId").is_some(),
                "case {case:?}: {read}: a reported relationship names the related resource, \
                 got {link}"
            );
        }
    }
}

/// Every item in `items` reports the links the recorder observed for the resource, which
/// for a resource it observed none for is the EMPTY list.
fn assert_no_links_observed(case: &str, read: &str, items: &[Value]) {
    for item in items {
        assert_eq!(
            item.get("relationships"),
            Some(&json!([])),
            "case {case:?}: {read}: a resource the recorder observed no related resource for \
             reports the empty list, got {item}"
        );
    }
}

#[tokio::test]
async fn every_configuration_item_read_reports_relationships_as_a_list() {
    let cases: [(&str, Recorded, &str); 3] = [
        (
            "a queue the recorder observed no related resource for",
            Recorded::Queue,
            "relationships-plain",
        ),
        (
            "a FIFO queue the recorder observed no related resource for",
            Recorded::FifoQueue,
            "relationships-ordered",
        ),
        (
            "a bucket of another service the recorder observed no related resource for",
            Recorded::Bucket,
            "relationships-bucket",
        ),
    ];

    for (case, recorded, name) in cases {
        let emulator = Emulator::new();
        emulator.recording().await;
        emulator
            .config(
                "PutConfigurationAggregator",
                &json!({
                    "ConfigurationAggregatorName": AGGREGATOR,
                    "AccountAggregationSources": [{
                        "AccountIds": [ACCOUNT_ID],
                        "AwsRegions": [REGION],
                    }],
                }),
            )
            .await;
        let (resource_type, resource_id) = create_resource(case, &emulator, recorded, name).await;

        // The item that DISCOVERED the resource carries the member, through the read the
        // whole history comes from.
        let discovered = await_history(
            case,
            &emulator,
            &resource_type,
            &resource_id,
            "the discovery item",
            |items| !items.is_empty(),
        )
        .await;
        assert_relationships_are_a_list(case, "GetResourceConfigHistory", &discovered);
        assert_no_links_observed(case, "GetResourceConfigHistory", &discovered);

        // The single AGGREGATE read reports a full configuration item too, so it carries
        // the member on exactly the same terms.
        let aggregated = emulator
            .config(
                "GetAggregateResourceConfig",
                &json!({
                    "ConfigurationAggregatorName": AGGREGATOR,
                    "ResourceIdentifier": {
                        "SourceAccountId": ACCOUNT_ID,
                        "SourceRegion": REGION,
                        "ResourceType": resource_type,
                        "ResourceId": resource_id,
                    },
                }),
            )
            .await;
        let single = vec![aggregated["ConfigurationItem"].clone()];
        assert_relationships_are_a_list(case, "GetAggregateResourceConfig", &single);
        assert_no_links_observed(case, "GetAggregateResourceConfig", &single);

        // The query surface projects the same items, so a query that SELECTS the member
        // answers it on the same terms: `relationships` is one of the members a Config
        // query selects (`state::select_query::CONFIGURATION_ITEM_MEMBERS`, from
        // `smithy_query(query="shape", shape="ConfigurationItem")`).
        let selected = emulator
            .config(
                "SelectResourceConfig",
                &json!({
                    "Expression": format!(
                        "SELECT resourceId, relationships WHERE resourceType = \
                         '{resource_type}' AND resourceId = '{resource_id}'"
                    ),
                }),
            )
            .await;
        let rows: Vec<Value> = selected["Results"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|row| {
                let text = row.as_str().unwrap_or_else(|| {
                    panic!("case {case:?}: a result is a JSON document, got {selected}")
                });
                serde_json::from_str::<Value>(text).unwrap_or_else(|error| {
                    panic!("case {case:?}: a result parses as JSON, got {error} for {text:?}")
                })
            })
            .collect();
        assert_eq!(
            rows,
            vec![json!({ "resourceId": resource_id, "relationships": [] })],
            "case {case:?}: SelectResourceConfig: expected the selected member to be \
             answered as the empty list, got {selected}"
        );

        // The counter-case: the batched read answers `BaseConfigurationItem`s, the shape
        // that declares no `relationships` member at all, so it reports none.
        let batched = emulator
            .config(
                "BatchGetResourceConfig",
                &json!({
                    "resourceKeys": [{
                        "resourceType": resource_type,
                        "resourceId": resource_id,
                    }],
                }),
            )
            .await;
        let base_items = batched["baseConfigurationItems"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            base_items.len(),
            1,
            "case {case:?}: the batched read answers the recorded key, got {batched}"
        );
        for base in &base_items {
            assert!(
                base.get("relationships").is_none(),
                "case {case:?}: a base configuration item declares no relationships member, \
                 got {base}"
            );
        }

        // A state the resource CHANGES into is recorded as a further item, and every item
        // the history then reports carries the member — the past ones included.
        change_resource(&emulator, recorded, &resource_id).await;
        let changed = await_history(
            case,
            &emulator,
            &resource_type,
            &resource_id,
            "a second item",
            |items| items.len() > 1,
        )
        .await;
        assert_relationships_are_a_list(case, "GetResourceConfigHistory after a change", &changed);
        assert_no_links_observed(case, "GetResourceConfigHistory after a change", &changed);

        // And so does the item that records the resource is GONE.
        remove_resource(case, &emulator, recorded, &resource_id).await;
        let deleted = await_history(
            case,
            &emulator,
            &resource_type,
            &resource_id,
            "the deletion item",
            |items| statuses(items).first().map(String::as_str) == Some(RESOURCE_DELETED),
        )
        .await;
        assert_relationships_are_a_list(
            case,
            "GetResourceConfigHistory after the deletion",
            &deleted,
        );
        assert_no_links_observed(case, "GetResourceConfigHistory after the deletion", &deleted);
    }
}

/// The wording Config reports a managed policy's attachment to each kind of identity by
/// (`Relationship$relationshipName`), as the recorder observes them from IAM.
const ATTACHED_TO_ROLE: &str = "Is attached to Role";
/// See [`ATTACHED_TO_ROLE`].
const ATTACHED_TO_USER: &str = "Is attached to User";

/// One reported relationship as a comparable value: every member it names.
fn comparable(links: &Value) -> BTreeSet<String> {
    links
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|link| {
            format!(
                "{}|{}|{}|{}",
                link["resourceType"].as_str().unwrap_or_default(),
                link["resourceId"].as_str().unwrap_or_default(),
                link["resourceName"].as_str().unwrap_or_default(),
                link["relationshipName"].as_str().unwrap_or_default(),
            )
        })
        .collect()
}

#[tokio::test]
async fn a_selected_relationships_member_reports_every_link_the_recorder_observed() {
    // The counter-case to the rows above, and what keeps the member from being an empty
    // list the reads could just as well have invented: a resource the recorder DID observe
    // links for reports them all, through the same two reads. An IAM customer managed
    // policy is such a resource — its item's relationships name the identities IAM reports
    // it is attached to — so the row attaches it to TWO identities of different kinds,
    // which is what tells a list reported whole from one reported as its first element.
    let case = "a managed policy attached to a role and a user";
    let emulator = ConfigOverIam::new();
    emulator.recording().await;
    let iam = emulator.iam();
    let created = iam
        .create_policy()
        .policy_name("linked-policy")
        .policy_document(
            json!({
                "Version": "2012-10-17",
                "Statement": [{ "Effect": "Allow", "Action": "s3:GetObject", "Resource": "*" }],
            })
            .to_string(),
        )
        .send()
        .await
        .unwrap_or_else(|error| panic!("case {case:?}: creating the policy must succeed: {error:?}"))
        .policy
        .unwrap_or_else(|| panic!("case {case:?}: CreatePolicy answers the policy"));
    let policy_id = created
        .policy_id
        .unwrap_or_else(|| panic!("case {case:?}: CreatePolicy answers a PolicyId"));
    let policy_arn = created
        .arn
        .unwrap_or_else(|| panic!("case {case:?}: CreatePolicy answers an ARN"));

    let role = iam
        .create_role()
        .role_name("linked-role")
        .assume_role_policy_document(
            json!({
                "Version": "2012-10-17",
                "Statement": [{
                    "Effect": "Allow",
                    "Principal": { "Service": "ec2.amazonaws.com" },
                    "Action": "sts:AssumeRole",
                }],
            })
            .to_string(),
        )
        .send()
        .await
        .unwrap_or_else(|error| panic!("case {case:?}: creating the role must succeed: {error:?}"))
        .role
        .unwrap_or_else(|| panic!("case {case:?}: CreateRole answers the role"));
    iam.attach_role_policy()
        .role_name("linked-role")
        .policy_arn(&policy_arn)
        .send()
        .await
        .unwrap_or_else(|error| panic!("case {case:?}: attaching to the role must succeed: {error:?}"));
    let user = iam
        .create_user()
        .user_name("linked-user")
        .send()
        .await
        .unwrap_or_else(|error| panic!("case {case:?}: creating the user must succeed: {error:?}"))
        .user
        .unwrap_or_else(|| panic!("case {case:?}: CreateUser answers the user"));
    iam.attach_user_policy()
        .user_name("linked-user")
        .policy_arn(&policy_arn)
        .send()
        .await
        .unwrap_or_else(|error| panic!("case {case:?}: attaching to the user must succeed: {error:?}"));

    let expected: BTreeSet<String> = [
        format!(
            "{}|{}|{}|{}",
            "AWS::IAM::Role",
            role.role_id,
            role.role_name,
            ATTACHED_TO_ROLE
        ),
        format!(
            "{}|{}|{}|{}",
            "AWS::IAM::User",
            user.user_id,
            user.user_name,
            ATTACHED_TO_USER
        ),
    ]
    .into_iter()
    .collect();

    // The history's newest item, once the pass the recorder owes has observed both
    // attachments.
    let deadline = Instant::now() + CONVERGENCE_BUDGET;
    let item = loop {
        let (status, body) = emulator
            .call(
                "GetResourceConfigHistory",
                &json!({ "resourceType": POLICY_RESOURCE_TYPE, "resourceId": policy_id }),
            )
            .await;
        let newest = if status == 200 {
            body["configurationItems"][0].clone()
        } else {
            Value::Null
        };
        if comparable(&newest["relationships"]) == expected {
            break newest;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the item to report both attachments, got {status} with \
             {body}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    };
    assert_relationships_are_a_list(case, "GetResourceConfigHistory", std::slice::from_ref(&item));

    // And a query SELECTING the member answers the same list, every link in it — not the
    // first one, and not nothing.
    let selected = emulator
        .ok(
            "SelectResourceConfig",
            &json!({
                "Expression": format!(
                    "SELECT resourceId, relationships WHERE resourceType = \
                     '{POLICY_RESOURCE_TYPE}' AND resourceId = '{policy_id}'"
                ),
            }),
        )
        .await;
    let rows: Vec<Value> = selected["Results"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|row| row.as_str().and_then(|text| serde_json::from_str(text).ok()))
        .collect();
    assert_eq!(
        rows.len(),
        1,
        "case {case:?}: expected one row for the recorded policy, got {selected}"
    );
    assert_eq!(
        comparable(&rows[0]["relationships"]),
        expected,
        "case {case:?}: SelectResourceConfig: expected every observed link, got {selected}"
    );
}
