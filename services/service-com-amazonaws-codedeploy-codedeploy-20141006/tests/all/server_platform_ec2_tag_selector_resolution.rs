// SPDX-License-Identifier: CC0-1.0

//! WHICH Amazon EC2 instances an EC2/on-premises (`Server`) deployment group's EC2 tag
//! selectors select, over the whole `EC2TagFilterType` catalogue, and WHEN that selection is
//! made.
//!
//! RULE under test: a `Server`-platform deployment resolves its fleet when it is created, by
//! evaluating its deployment group's EC2 tag selectors against the tags Amazon EC2 holds for
//! the account's running instances at that instant — each filter matched as its `Type` states
//! (KEY_ONLY: the key whatever the value, VALUE_ONLY: the value whatever the key,
//! KEY_AND_VALUE: both, and a filter stating no type by the members it does name), and a
//! filter LIST taking in an instance that satisfies ANY of its filters — and it deploys to
//! every instance so identified, reporting each as an instance target taken through the
//! in-place lifecycle events. The `NO_INSTANCES` outcome belongs
//! ONLY to a deployment whose selectors identify no instance.
//!
//! The filter types are the model's own
//! (`smithy_query(query="shape",shape="EC2TagFilterType")`: KEY_ONLY | VALUE_ONLY |
//! KEY_AND_VALUE), and the list's own semantics are stated on the member: `ec2TagFilters` is
//! "the Amazon EC2 tags on which to filter. The deployment group includes Amazon EC2
//! instances with any of the specified tags"
//! (`smithy_query(query="doc",shape="CreateDeploymentGroupInput",member="ec2TagFilters")`).
//!
//! What this adds to the sibling suite `server_platform_instance_deployments.rs`, whose
//! harness it drives rather than writing a second one: the first table below spans the filter
//! type catalogue — VALUE_ONLY and a two-filter list included — while the second case moves
//! the TAGS instead of the filters, through Amazon EC2's own `CreateTags` and `DeleteTags`
//! after the host was launched, which is what shows a deployment's fleet is read out of that
//! service's tag state at the instant the deployment is created rather than cached from the
//! launch, and that the fleet one deployment resolved stays the fleet it reports afterwards.
//!
//! Nothing here waits a fixed time: every span is a stored due instant, so each step LOOKS
//! AGAIN until the condition holds, and nothing asserts how long anything took.

use serde_json::{json, Value};

use crate::server_platform_instance_deployments::{
    app_spec, bundle, events_of, param, Fleet, LIFECYCLE_EVENTS,
};

/// The tag key and value the second case puts on a host AFTER it was launched, and takes off
/// it again.
const LATE_KEY: &str = "Fleet";
/// See [`LATE_KEY`].
const LATE_VALUE: &str = "deployable";

/// The `ErrorCode` a deployment that identified no instance reports
/// (`smithy_query(query="shape",shape="ErrorCode")` lists `NO_INSTANCES`; its fixed sentence
/// is pinned by `fixed_error_sentences.rs`, which is the one place that wording lives).
const NO_INSTANCES: &str = "NO_INSTANCES";

/// The ids the harness reads off `ListDeploymentTargets` for `deployment`, SORTED so a row
/// can state the fleet it expects as a set.
async fn resolved_targets(fleet: &Fleet, case: &str, deployment: &str) -> Vec<String> {
    let mut ids = fleet.target_ids(case, deployment).await;
    ids.sort();
    ids
}

/// Upload a revision of its own for `name` — one file installed under a destination nothing
/// else deploys to — and answer the key it was uploaded at.
///
/// A destination another deployment group's deployment put there is a foreign file on the
/// host, which is what `fileExistsBehavior` decides; each row installing into its own
/// directory keeps these cases about the SELECTION.
async fn given_revision(fleet: &Fleet, case: &str, name: &str) -> String {
    let key = format!("{name}.zip");
    let destination = format!("/var/www/{name}");
    let document = app_spec(&[("site/index.html", &destination)], &[]);
    fleet
        .upload(
            case,
            &key,
            &bundle(&[
                ("appspec.yml", &document),
                ("site/index.html", &format!("<p>{name}</p>")),
            ]),
        )
        .await;
    key
}

/// Assert that `deployment` deployed to exactly `expected`, succeeding on each of those hosts
/// and reporting the in-place lifecycle events in order on every one of them; answers the
/// fleet it read, so a caller asking anything else of that listing does not read it twice.
async fn assert_deployed_to(
    fleet: &Fleet,
    case: &str,
    deployment: &str,
    expected: &[String],
) -> Vec<String> {
    let info = fleet.terminal(case, deployment).await;
    assert_eq!(
        info["status"].as_str(),
        Some("Succeeded"),
        "case {case:?}: expected the deployment over the fleet its selectors identified to \
         succeed, got {info:?}"
    );
    assert_eq!(
        info["errorInformation"],
        Value::Null,
        "case {case:?}: expected a deployment that identified {} host(s) to report no error \
         information, got {:?}",
        expected.len(),
        info["errorInformation"]
    );
    let reported = resolved_targets(fleet, case, deployment).await;
    assert_eq!(
        reported, expected,
        "case {case:?}: expected the targets {expected:?}, got {reported:?}"
    );
    for instance in expected {
        let target = fleet.instance_target(case, deployment, instance).await;
        assert_eq!(
            target["status"].as_str(),
            Some("Succeeded"),
            "case {case:?}: expected {instance} to have taken the revision, got {target:?}"
        );
        let events = events_of(&target);
        let names: Vec<&str> = events.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(
            names, LIFECYCLE_EVENTS,
            "case {case:?}: expected the in-place lifecycle events in order on {instance}, got \
             {names:?}"
        );
        for (name, status) in &events {
            assert_eq!(
                status, "Succeeded",
                "case {case:?}: expected {name} to have completed on {instance}, got {status}"
            );
        }
    }
    reported
}

/// Assert that `deployment` identified no instance: the one outcome `NO_INSTANCES` belongs to.
async fn assert_no_instances(fleet: &Fleet, case: &str, deployment: &str) {
    let info = fleet.terminal(case, deployment).await;
    assert_eq!(
        (
            info["status"].as_str(),
            info["errorInformation"]["code"].as_str(),
        ),
        (Some("Failed"), Some(NO_INSTANCES)),
        "case {case:?}: expected a deployment whose selectors identify no instance to fail \
         with {NO_INSTANCES}, got {info:?}"
    );
}

/// RULE: see this module's own documentation — every `EC2TagFilterType` is matched as that
/// type states, a filter list takes in an instance satisfying ANY of its filters, and a
/// selector that identifies no instance is the only one that ends `NO_INSTANCES`.
///
/// The rows differ along what the rule generalises over: the filter TYPE (all three the model
/// states, including the VALUE_ONLY one no end-to-end case covered, and a filter stating
/// none), how many filters the list states, how many hosts the selector identifies (one, two,
/// none), and which of the fleet's tags carry the key and which the value.
#[tokio::test]
async fn every_ec2_tag_filter_type_selects_the_hosts_amazon_ec2_holds_those_tags_for() {
    let fleet = Fleet::new();
    // Three tagged hosts and one carrying no tag at all: the tags are laid out so that a
    // key-only, a value-only and a key-and-value filter each name a DIFFERENT pair of them —
    // the same value under two keys, the same key with two values.
    let web = fleet
        .launch("setup", &[("Name", "web"), ("Stage", "prod")], None)
        .await;
    let api = fleet.launch("setup", &[("Name", "api")], None).await;
    let worker = fleet.launch("setup", &[("Service", "web")], None).await;
    let untagged = fleet.launch("setup", &[], None).await;
    for host in [&web, &api, &worker, &untagged] {
        fleet.wait_until_running("setup", host).await;
    }

    /// One row: how the group names its fleet, and which hosts that names.
    struct Row {
        case: &'static str,
        selectors: Value,
        /// The hosts the deployment must go to, sorted, or none for a row whose selectors
        /// identify nothing.
        expected: Vec<String>,
    }
    /// `expected`, sorted, which is the order the listing is compared in.
    fn hosts(mut ids: Vec<String>) -> Vec<String> {
        ids.sort();
        ids
    }
    let rows: [Row; 7] = [
        Row {
            case: "a KEY_ONLY filter, which names both hosts carrying that key",
            selectors: json!({"ec2TagFilters": [{"Key": "Name", "Type": "KEY_ONLY"}]}),
            expected: hosts(vec![web.clone(), api.clone()]),
        },
        Row {
            case: "a VALUE_ONLY filter, which names both hosts carrying that value under \
                   different keys",
            selectors: json!({"ec2TagFilters": [{"Value": "web", "Type": "VALUE_ONLY"}]}),
            expected: hosts(vec![web.clone(), worker.clone()]),
        },
        Row {
            case: "a VALUE_ONLY filter on a value one host alone carries",
            selectors: json!({"ec2TagFilters": [{"Value": "prod", "Type": "VALUE_ONLY"}]}),
            expected: vec![web.clone()],
        },
        Row {
            case: "a KEY_AND_VALUE filter, which names the host carrying that exact pair",
            selectors: json!({
                "ec2TagFilters": [{"Key": "Name", "Value": "web", "Type": "KEY_AND_VALUE"}],
            }),
            expected: vec![web.clone()],
        },
        Row {
            case: "two filters, each naming another host, which the list takes in together",
            selectors: json!({
                "ec2TagFilters": [
                    {"Key": "Name", "Value": "api", "Type": "KEY_AND_VALUE"},
                    {"Key": "Service", "Value": "web", "Type": "KEY_AND_VALUE"},
                ],
            }),
            expected: hosts(vec![api.clone(), worker.clone()]),
        },
        Row {
            case: "a filter stating no type at all, which can only mean the members it names",
            selectors: json!({"ec2TagFilters": [{"Key": "Name", "Value": "api"}]}),
            expected: vec![api.clone()],
        },
        Row {
            case: "a KEY_ONLY filter on a key no host of the account carries",
            selectors: json!({"ec2TagFilters": [{"Key": "Cohort", "Type": "KEY_ONLY"}]}),
            expected: Vec::new(),
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let name = format!("filter-type-{index}");
        let key = given_revision(&fleet, case, &name).await;
        let (application, group) = fleet.group(case, &name, row.selectors.clone()).await;
        let deployment = fleet
            .deploy(case, &application, &group, &key, json!({}))
            .await;

        if row.expected.is_empty() {
            assert_no_instances(&fleet, case, &deployment).await;
            continue;
        }
        // The host carrying no tag is named by no selector above, and neither is a host whose
        // tags the row's own filters do not satisfy.
        let reported = assert_deployed_to(&fleet, case, &deployment, &row.expected).await;
        assert!(
            !reported.contains(&untagged),
            "case {case:?}: expected the untagged host not to be a target, got {reported:?}"
        );
    }
}

/// RULE: the fleet is resolved from Amazon EC2's tag state at the instant the deployment is
/// created — a tag put on a host after it was launched brings that host in, and taking the tag
/// off keeps the next deployment out — while the fleet a deployment already resolved stays the
/// fleet it reports.
#[tokio::test]
async fn the_fleet_is_resolved_from_ec2s_tag_state_when_the_deployment_is_created() {
    let fleet = Fleet::new();
    let case = "a host tagged, then untagged, after it was launched";
    let host = fleet.launch(case, &[("Name", "late")], None).await;
    fleet.wait_until_running(case, &host).await;
    let key = given_revision(&fleet, case, "late-tagged").await;
    let (application, group) = fleet
        .group(
            case,
            "late-tagged",
            json!({
                "ec2TagFilters": [
                    {"Key": LATE_KEY, "Value": LATE_VALUE, "Type": "KEY_AND_VALUE"},
                ],
            }),
        )
        .await;

    // Before the tag exists the selector identifies nothing, whatever else the host carries.
    let before = fleet
        .deploy(case, &application, &group, &key, json!({}))
        .await;
    assert_no_instances(&fleet, case, &before).await;

    // The tag put on the running host through Amazon EC2's own `CreateTags` brings it in.
    fleet
        .ec2(
            case,
            "CreateTags",
            &[
                param("ResourceId.1", &host),
                param("Tag.1.Key", LATE_KEY),
                param("Tag.1.Value", LATE_VALUE),
            ],
        )
        .await;
    let tagged = fleet
        .deploy(case, &application, &group, &key, json!({}))
        .await;
    assert_deployed_to(&fleet, case, &tagged, std::slice::from_ref(&host)).await;

    // Taking it off again keeps the NEXT deployment out.
    fleet
        .ec2(
            case,
            "DeleteTags",
            &[param("ResourceId.1", &host), param("Tag.1.Key", LATE_KEY)],
        )
        .await;
    let untagged = fleet
        .deploy(case, &application, &group, &key, json!({}))
        .await;
    assert_no_instances(&fleet, case, &untagged).await;

    // And the deployment that DID resolve the host still reports it: what a deployment
    // resolved is not re-evaluated against a fleet that has since changed.
    let still = resolved_targets(&fleet, case, &tagged).await;
    assert_eq!(
        still,
        vec![host.clone()],
        "case {case:?}: expected the finished deployment to still report the host it resolved, \
         got {still:?}"
    );
    let info = fleet.terminal(case, &tagged).await;
    assert_eq!(
        info["status"].as_str(),
        Some("Succeeded"),
        "case {case:?}: expected the finished deployment's outcome to be unchanged, got {info:?}"
    );
}
