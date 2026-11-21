// SPDX-License-Identifier: CC0-1.0

//! `CreateOrUpdateTags` upserts the tags of an Auto Scaling group, and every later reader
//! of that group uses what the call wrote.
//!
//! The RULE under test: each tag a `CreateOrUpdateTags` request carries is written onto the
//! group its `ResourceId` names — a key the group does not carry is ADDED, and a key it
//! already carries has its whole definition OVERWRITTEN ("when you specify a tag with a key
//! that already exists, the operation overwrites the previous tag definition, and you do not
//! get an error message"), its value and its propagation decision included, so a tag that
//! states no `PropagateAtLaunch` leaves the group carrying the decision the service applies
//! when a request states none rather than the one an earlier call stated. The call writes the
//! group's one tag collection, so the group's own read, the account-wide tag read and the
//! tags the group's LATER launches carry all report what it wrote.
//!
//! The rows span what that rule generalises over: whether the key is new or already carried,
//! the value written (another one, the same one, an empty one), the propagation decision
//! (stated `true`, stated `false`, reversed either way, stated not at all), how many tags one
//! call carries and how many groups it names at once.
//!
//! The crate's `describe_tags` suite reads the same writes back through every filter of the
//! account-wide read, and its `group_tag_propagate_at_launch` suite pins the decision a
//! CREATE stores; this suite is about what a later upsert does to a group that already
//! carries tags, down to the instances it launches afterwards.

use std::time::{Duration, Instant};

use smithy_server_runtime::MultiServiceRouter;

use crate::support::{
    borrowed, ec2_query, emulator, list_members, p, query, seed_subnets, tag_value, tag_values,
};

/// The `ResourceType` every tag of an Auto Scaling group is written and reported under, from
/// the model's own documentation for `Tag$ResourceType`: "The only supported value is
/// auto-scaling-group". Spelled out here rather than imported, so a wrong constant in the
/// implementation cannot make a test pass.
const GROUP_RESOURCE_TYPE: &str = "auto-scaling-group";

/// Where an expectation stands for the decision the service applies to a tag whose request
/// states no `PropagateAtLaunch`.
///
/// The model documents no default for the member, so WHICH decision that is belongs to the
/// service, not to this suite: it is read off the CREATE path in the test itself
/// ([`unstated_decision`]) and substituted here. What the rows then pin is the invariant —
/// an upsert that states no decision leaves the tag carrying the same one a create that
/// stated none would, because the upsert overwrites the whole tag definition — which fails
/// if the two paths ever disagree, whichever decision the service applies.
const UNSTATED: &str = "<unstated>";

/// The two groups the rows write to, the group whose create states no decision at all, and
/// the launch template each is created from.
const ALPHA: &str = "upsert-alpha";
const BETA: &str = "upsert-beta";
const UNSTATED_GROUP: &str = "upsert-unstated";
const TEMPLATE: &str = "tags-template";

/// One tag as a request carries it: the group it names, its key, its value, and the
/// propagation decision when the request states one at all.
struct WrittenTag {
    group: &'static str,
    key: &'static str,
    value: &'static str,
    propagate: Option<bool>,
}

/// Those tags as the `Tags.member.N.*` parameters a client sends.
fn tag_params(tags: &[WrittenTag]) -> Vec<(String, String)> {
    let mut params: Vec<(String, String)> = Vec::new();
    for (position, tag) in tags.iter().enumerate() {
        let member = position + 1;
        params.push(p(&format!("Tags.member.{member}.ResourceId"), tag.group));
        params.push(p(
            &format!("Tags.member.{member}.ResourceType"),
            GROUP_RESOURCE_TYPE,
        ));
        params.push(p(&format!("Tags.member.{member}.Key"), tag.key));
        params.push(p(&format!("Tags.member.{member}.Value"), tag.value));
        // A row that states no decision sends NO `PropagateAtLaunch` parameter at all.
        if let Some(propagate) = tag.propagate {
            params.push(p(
                &format!("Tags.member.{member}.PropagateAtLaunch"),
                if propagate { "true" } else { "false" },
            ));
        }
    }
    params
}

/// Every tag one answered body reports for `group`, as `key=value|propagate`, ordered by
/// key — the form the expectations below are written in.
///
/// Read per `TagDescription` rather than per member type, so a value belongs to the tag it
/// was reported inside; a tag whose value is empty carries an empty one here.
fn reported_tags(case: &str, body: &str, group: &str) -> Vec<String> {
    let mut out: Vec<String> = list_members(body, "Tags")
        .into_iter()
        .filter(|member| tag_value(member, "ResourceId") == group)
        .map(|member| {
            let resource_type = tag_value(member, "ResourceType");
            assert_eq!(
                resource_type, GROUP_RESOURCE_TYPE,
                "case {case:?}: expected ResourceType {GROUP_RESOURCE_TYPE}, got \
                 {resource_type:?} in {body}"
            );
            let key = tag_value(member, "Key");
            let propagate = tag_value(member, "PropagateAtLaunch");
            assert!(
                !key.is_empty() && !propagate.is_empty(),
                "case {case:?}: every reported tag carries a Key and a PropagateAtLaunch, \
                 got {member:?}"
            );
            format!("{key}={}|{propagate}", tag_value(member, "Value"))
        })
        .collect();
    out.sort();
    out
}

/// The tags `group` carries as the ACCOUNT-WIDE read reports them, narrowed to that group.
async fn account_tags_of(case: &str, router: &MultiServiceRouter, group: &str) -> Vec<String> {
    let (status, body) = query(
        router,
        "DescribeTags",
        &[
            ("Filters.member.1.Name", GROUP_RESOURCE_TYPE),
            ("Filters.member.1.Values.member.1", group),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeTags expected 200, got {status}: {body}"
    );
    reported_tags(case, &body, group)
}

/// The tags `group` carries as the GROUP's own read reports them: the same stored tags seen
/// through the other reader.
async fn group_tags_of(case: &str, router: &MultiServiceRouter, group: &str) -> Vec<String> {
    let (status, body) = query(
        router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", group)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeAutoScalingGroups expected 200, got {status}: {body}"
    );
    reported_tags(case, &body, group)
}

/// Create one group of `desired_capacity` instances carrying `tags`, over the wire, so every
/// assertion below follows state the service stored from a request a client could send.
async fn given_group(
    router: &MultiServiceRouter,
    name: &str,
    subnet: &str,
    desired_capacity: i32,
    tags: &[WrittenTag],
) {
    let mut params = vec![
        p("AutoScalingGroupName", name),
        p("MinSize", "0"),
        p("MaxSize", desired_capacity.max(1).to_string()),
        p("DesiredCapacity", desired_capacity.to_string()),
        p("LaunchTemplate.LaunchTemplateName", TEMPLATE),
        p("VPCZoneIdentifier", subnet),
    ];
    for (position, tag) in tags.iter().enumerate() {
        let member = position + 1;
        params.push(p(&format!("Tags.member.{member}.Key"), tag.key));
        params.push(p(&format!("Tags.member.{member}.Value"), tag.value));
        if let Some(propagate) = tag.propagate {
            params.push(p(
                &format!("Tags.member.{member}.PropagateAtLaunch"),
                if propagate { "true" } else { "false" },
            ));
        }
    }
    let (status, body) = query(router, "CreateAutoScalingGroup", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "CreateAutoScalingGroup for {name:?} expected 200, got {status}: {body}"
    );
}

/// One row of the upsert table: the call it makes, and the whole tag set each group carries
/// afterwards.
struct Row {
    case: &'static str,
    tags: &'static [WrittenTag],
    alpha_after: &'static [&'static str],
    beta_after: &'static [&'static str],
}

/// The rows are applied IN ORDER against one account, each over the state the previous one
/// left: both groups start from the tags their create carried — `ALPHA` with `env=prod`
/// propagating and `team=platform` not, `BETA` with `env=staging` propagating.
const ROWS: &[Row] = &[
    Row {
        case: "a key the group does not carry, stating PropagateAtLaunch=true",
        tags: &[WrittenTag {
            group: ALPHA,
            key: "owner",
            value: "payments",
            propagate: Some(true),
        }],
        alpha_after: &["env=prod|true", "owner=payments|true", "team=platform|false"],
        beta_after: &["env=staging|true"],
    },
    Row {
        case: "a key the group does not carry, stating PropagateAtLaunch=false",
        tags: &[WrittenTag {
            group: ALPHA,
            key: "tier",
            value: "batch",
            propagate: Some(false),
        }],
        alpha_after: &[
            "env=prod|true",
            "owner=payments|true",
            "team=platform|false",
            "tier=batch|false",
        ],
        beta_after: &["env=staging|true"],
    },
    Row {
        case: "a key the group does not carry, stating no PropagateAtLaunch at all",
        tags: &[WrittenTag {
            group: ALPHA,
            key: "stack",
            value: "blue",
            propagate: None,
        }],
        alpha_after: &[
            "env=prod|true",
            "owner=payments|true",
            "stack=blue|<unstated>",
            "team=platform|false",
            "tier=batch|false",
        ],
        beta_after: &["env=staging|true"],
    },
    Row {
        case: "a key the create of the group wrote, overwritten with another value",
        tags: &[WrittenTag {
            group: ALPHA,
            key: "env",
            value: "canary",
            propagate: Some(true),
        }],
        alpha_after: &[
            "env=canary|true",
            "owner=payments|true",
            "stack=blue|<unstated>",
            "team=platform|false",
            "tier=batch|false",
        ],
        beta_after: &["env=staging|true"],
    },
    Row {
        case: "the same key and value again, with its propagation decision reversed to false",
        tags: &[WrittenTag {
            group: ALPHA,
            key: "env",
            value: "canary",
            propagate: Some(false),
        }],
        alpha_after: &[
            "env=canary|false",
            "owner=payments|true",
            "stack=blue|<unstated>",
            "team=platform|false",
            "tier=batch|false",
        ],
        beta_after: &["env=staging|true"],
    },
    Row {
        case: "a non-propagating key, with its decision reversed the other way",
        tags: &[WrittenTag {
            group: ALPHA,
            key: "team",
            value: "platform",
            propagate: Some(true),
        }],
        alpha_after: &[
            "env=canary|false",
            "owner=payments|true",
            "stack=blue|<unstated>",
            "team=platform|true",
            "tier=batch|false",
        ],
        beta_after: &["env=staging|true"],
    },
    Row {
        case: "a key the group carries, overwritten with an empty value",
        tags: &[WrittenTag {
            group: ALPHA,
            key: "stack",
            value: "",
            propagate: Some(false),
        }],
        alpha_after: &[
            "env=canary|false",
            "owner=payments|true",
            "stack=|false",
            "team=platform|true",
            "tier=batch|false",
        ],
        beta_after: &["env=staging|true"],
    },
    Row {
        case: "a key the group carries, overwritten by a tag stating no PropagateAtLaunch, \
               which takes the decision the service applies",
        tags: &[WrittenTag {
            group: ALPHA,
            key: "tier",
            value: "batch",
            propagate: None,
        }],
        alpha_after: &[
            "env=canary|false",
            "owner=payments|true",
            "stack=|false",
            "team=platform|true",
            "tier=batch|<unstated>",
        ],
        beta_after: &["env=staging|true"],
    },
    Row {
        case: "one call naming BOTH groups: a new key on one, an overwrite on the other",
        tags: &[
            WrittenTag {
                group: ALPHA,
                key: "cost-centre",
                value: "1234",
                propagate: Some(false),
            },
            WrittenTag {
                group: BETA,
                key: "env",
                value: "qa",
                propagate: Some(false),
            },
            WrittenTag {
                group: BETA,
                key: "cost-centre",
                value: "5678",
                propagate: Some(true),
            },
        ],
        alpha_after: &[
            "cost-centre=1234|false",
            "env=canary|false",
            "owner=payments|true",
            "stack=|false",
            "team=platform|true",
            "tier=batch|<unstated>",
        ],
        beta_after: &["cost-centre=5678|true", "env=qa|false"],
    },
];

/// Expectations as [`reported_tags`] answers them: sorted by key, with [`UNSTATED`] standing
/// for the decision `unstated` says the service applies to a tag that states none.
fn expected(tags: &[&str], unstated: &str) -> Vec<String> {
    let mut expected: Vec<String> = tags.iter().map(|tag| tag.replace(UNSTATED, unstated)).collect();
    expected.sort();
    expected
}

/// The decision the SERVICE applies to a tag whose request states no `PropagateAtLaunch`, as
/// the wire spells it — read off a group whose create stated none, which is the one path that
/// says what the service's own decision is.
async fn unstated_decision(router: &MultiServiceRouter, subnet: &str) -> String {
    given_group(
        router,
        UNSTATED_GROUP,
        subnet,
        0,
        &[WrittenTag {
            group: UNSTATED_GROUP,
            key: "unflagged",
            value: "by-the-create",
            propagate: None,
        }],
    )
    .await;
    let reported = group_tags_of("the decision a create states none for", router, UNSTATED_GROUP)
        .await;
    let decision = reported
        .first()
        .and_then(|tag| tag.rsplit('|').next())
        .unwrap_or_default()
        .to_string();
    assert!(
        decision == "true" || decision == "false",
        "a tag whose create states no PropagateAtLaunch must still report a decision, got \
         {reported:?}"
    );
    decision
}

#[tokio::test]
async fn an_upsert_adds_a_new_key_and_overwrites_a_carried_one() {
    let (router, engine) = emulator();
    let subnets = seed_subnets(&engine, 1);
    let subnet = subnets.first().expect("one seeded subnet").id.clone();
    // What the SERVICE applies to a tag that states no decision, read off the create path, so
    // the rows below pin the two paths against each other rather than against a literal.
    let unstated = unstated_decision(&router, &subnet).await;
    given_group(
        &router,
        ALPHA,
        &subnet,
        0,
        &[
            WrittenTag {
                group: ALPHA,
                key: "env",
                value: "prod",
                propagate: Some(true),
            },
            WrittenTag {
                group: ALPHA,
                key: "team",
                value: "platform",
                propagate: Some(false),
            },
        ],
    )
    .await;
    given_group(
        &router,
        BETA,
        &subnet,
        0,
        &[WrittenTag {
            group: BETA,
            key: "env",
            value: "staging",
            propagate: Some(true),
        }],
    )
    .await;

    for row in ROWS {
        let case = row.case;
        let params = tag_params(row.tags);
        let (status, body) = query(&router, "CreateOrUpdateTags", &borrowed(&params)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateOrUpdateTags expected 200, got {status}: {body}"
        );

        for (group, after) in [(ALPHA, row.alpha_after), (BETA, row.beta_after)] {
            let wanted = expected(after, &unstated);
            let account = account_tags_of(case, &router, group).await;
            assert_eq!(
                account, wanted,
                "case {case:?}: expected {group:?} to carry {wanted:?} through the \
                 account-wide read, got {account:?}"
            );
            // One tag, one stored record, two readers of it.
            let own = group_tags_of(case, &router, group).await;
            assert_eq!(
                own, wanted,
                "case {case:?}: expected {group:?} to carry {wanted:?} through its own read, \
                 got {own:?}"
            );
        }
    }
}

/// One tag of the group the launch test upserts, and whether the instances the group
/// launches afterwards must carry it.
struct LaunchRow {
    case: &'static str,
    key: &'static str,
    value: &'static str,
    propagate: Option<bool>,
    /// Whether the tag the upsert left behind reaches an instance launched after it. `None`
    /// where the row states no decision and the answer is therefore the one the SERVICE
    /// applies, read off the create path in the test.
    reaches_a_launch: Option<bool>,
}

/// The upserts the launch test makes on a group created with `env=prod` propagating and
/// `team=platform` not: a new propagating key, a new non-propagating key, a new key stating
/// no decision, a carried key whose decision is turned OFF and a carried key whose decision
/// is turned ON — so what reaches the launch is the decision the UPSERT left, not the one the
/// create stated.
const LAUNCH_ROWS: &[LaunchRow] = &[
    LaunchRow {
        case: "a new key the upsert states as propagating",
        key: "owner",
        value: "payments",
        propagate: Some(true),
        reaches_a_launch: Some(true),
    },
    LaunchRow {
        case: "a new key the upsert states as not propagating",
        key: "tier",
        value: "batch",
        propagate: Some(false),
        reaches_a_launch: Some(false),
    },
    LaunchRow {
        case: "a new key the upsert states no decision for",
        key: "stack",
        value: "blue",
        propagate: None,
        reaches_a_launch: None,
    },
    LaunchRow {
        case: "a carried key whose propagation the upsert turns off",
        key: "env",
        value: "prod",
        propagate: Some(false),
        reaches_a_launch: Some(false),
    },
    LaunchRow {
        case: "a carried key whose propagation the upsert turns on, with another value",
        key: "team",
        value: "core",
        propagate: Some(true),
        reaches_a_launch: Some(true),
    },
];

/// Poll `read` until it answers a value, or the generous deadline passes: the test waits for
/// the CONDITION the emulator's clock owes rather than for a fixed time.
async fn poll_until<T, F, Fut>(what: &str, mut read: F) -> T
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if let Some(value) = read().await {
            return value;
        }
        assert!(
            Instant::now() < deadline,
            "the emulator never answered {what}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

#[tokio::test]
async fn the_tags_an_upsert_left_are_the_tags_a_later_launch_carries() {
    let (router, engine) = emulator();
    let subnets = seed_subnets(&engine, 1);
    let subnet = subnets.first().expect("one seeded subnet").id.clone();
    let group = "upsert-launching";
    // The decision the SERVICE applies to a tag that states none, read off the create path:
    // the row that states none expects that same decision at the launch.
    let unstated = unstated_decision(&router, &subnet).await == "true";
    given_group(
        &router,
        group,
        &subnet,
        0,
        &[
            WrittenTag {
                group,
                key: "env",
                value: "prod",
                propagate: Some(true),
            },
            WrittenTag {
                group,
                key: "team",
                value: "platform",
                propagate: Some(false),
            },
        ],
    )
    .await;

    // Every upsert in ONE call, the way a client sends a batch of them.
    let mut params: Vec<(String, String)> = Vec::new();
    for (position, row) in LAUNCH_ROWS.iter().enumerate() {
        let member = position + 1;
        params.push(p(&format!("Tags.member.{member}.ResourceId"), group));
        params.push(p(
            &format!("Tags.member.{member}.ResourceType"),
            GROUP_RESOURCE_TYPE,
        ));
        params.push(p(&format!("Tags.member.{member}.Key"), row.key));
        params.push(p(&format!("Tags.member.{member}.Value"), row.value));
        if let Some(propagate) = row.propagate {
            params.push(p(
                &format!("Tags.member.{member}.PropagateAtLaunch"),
                if propagate { "true" } else { "false" },
            ));
        }
    }
    let (status, body) = query(&router, "CreateOrUpdateTags", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "CreateOrUpdateTags expected 200, got {status}: {body}"
    );

    // Capacity asked for AFTER the upsert, so the instance it launches is tagged from what
    // the upsert left behind.
    let (status, body) = query(
        &router,
        "SetDesiredCapacity",
        &[("AutoScalingGroupName", group), ("DesiredCapacity", "1")],
    )
    .await;
    assert_eq!(
        status, 200,
        "SetDesiredCapacity expected 200, got {status}: {body}"
    );

    let instance_id = poll_until("the instance the group's capacity launched", || async {
        let (status, body) = query(
            &router,
            "DescribeAutoScalingGroups",
            &[("AutoScalingGroupNames.member.1", group)],
        )
        .await;
        if status != 200 {
            return None;
        }
        tag_values(&body, "InstanceId").into_iter().next()
    })
    .await;

    // Amazon EC2's own view of that instance: the launch is the account's, so the tags are
    // read where a client reads them.
    let instance_tags = poll_until("Amazon EC2's own view of that instance", || async {
        let (status, body) =
            ec2_query(&router, "DescribeInstances", &[p("InstanceId.1", &instance_id)]).await;
        if status != 200 || tag_value(&body, "instanceId").is_empty() {
            return None;
        }
        let keys = tag_values(&body, "key");
        let values = tag_values(&body, "value");
        // Every tag Amazon EC2 reports carries both members, so the two lists line up; a
        // body where they do not would pair a key with another tag's value, and is caught
        // here rather than asserted on.
        assert_eq!(
            keys.len(),
            values.len(),
            "every tag Amazon EC2 reports carries a key and a value, got {keys:?} and \
             {values:?} in {body}"
        );
        Some(
            keys.into_iter()
                .zip(values)
                .map(|(key, value)| format!("{key}={value}"))
                .collect::<Vec<String>>(),
        )
    })
    .await;

    for row in LAUNCH_ROWS {
        let case = row.case;
        let pair = format!("{}={}", row.key, row.value);
        let reaches = row.reaches_a_launch.unwrap_or(unstated);
        assert_eq!(
            instance_tags.contains(&pair),
            reaches,
            "case {case:?}: the tag {pair:?} reaches an instance launched after the upsert \
             only when the decision the upsert left is true (expected {reaches}), got \
             {instance_tags:?}"
        );
        // A key whose propagation the upsert turned off reaches the instance under NO value:
        // what the create stated does not survive the overwrite either.
        if !reaches {
            assert!(
                !instance_tags
                    .iter()
                    .any(|tag| tag.starts_with(&format!("{}=", row.key))),
                "case {case:?}: no value of the key {:?} reaches the instance, got \
                 {instance_tags:?}",
                row.key
            );
        }
    }
}
