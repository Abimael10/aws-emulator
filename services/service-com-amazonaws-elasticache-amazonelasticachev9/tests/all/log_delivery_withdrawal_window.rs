// SPDX-License-Identifier: CC0-1.0

//! What a change to a cache's LOG-DELIVERY configurations is reported as while the service is
//! carrying it out, and what the clock then settles it at.
//!
//! RULE: a change a request makes to a log-delivery configuration is reported in the transient
//! `LogDeliveryConfiguration$Status` that change puts the configuration in — `enabling` for a
//! log type newly asked for, `modifying` for one already configured, `disabling` for one a
//! request has asked for no longer, each carrying the destination that configuration holds —
//! and the clock then owes it its settled outcome: a configuration being set up or changed
//! settles against the destination it names, and one being withdrawn is taken out of the
//! cache's list, which is the only way it leaves. A log type no configuration is held for has
//! nothing to withdraw, so asking for it no longer stores none.
//!
//! So `disabling` is observable: a client that turns a delivery off is told it is going before
//! it is gone, and can tell a delivery it has just turned off from a log type that was never
//! configured — the distinction the enabled and disabled arms of a delivery flow rest on.
//!
//! The rows span what the rule generalises over: whether a configuration for the log type was
//! held at all, whether the one held had already settled, which direction the change goes (ask
//! for, re-point, ask for no longer, ask for again while it is being withdrawn), which log
//! type the change names, and which destination the configuration carries — a Kinesis Data
//! Firehose stream, which this service resolves nothing against, and a CloudWatch Logs group.
//! A test of its own takes the other operation that re-writes these configurations,
//! `ModifyReplicationGroup`, where the withdrawal reaches the group's own list and each member
//! cluster's — and with it what a member then reports beside that list
//! (`CacheCluster$ReplicationGroupLogDeliveryEnabled`).
//!
//! Every assertion goes over the wire through [`crate::harness`]; the records a cache
//! DELIVERS into a CloudWatch Logs group belong to that service, so what landed in one is
//! asserted in `core/router/tests/all/elasticache_slow_log_delivery_stops_and_resumes.rs`.

use crate::cache_cluster_lifecycle::create_available_cluster;
use crate::harness::{params, poll_until, Emulator, Reply, REGION};

/// A `DestinationType` this service holds no view of, so a configuration naming one settles
/// `active` with no destination to resolve — the rows that need a SETTLED configuration to
/// change use it, rather than standing a log group up in another service.
const FIREHOSE: &str = "kinesis-firehose";

/// A `DestinationType` whose destination is a CloudWatch Logs log group.
const CLOUDWATCH: &str = "cloudwatch-logs";

/// The log types `LogDeliveryConfigurationRequest$LogType` takes
/// (`smithy_query(query="shape", shape="LogType")`: `slow-log | engine-log`).
const SLOW_LOG: &str = "slow-log";
/// See [`SLOW_LOG`].
const ENGINE_LOG: &str = "engine-log";

/// The form pairs one `LogDeliveryConfigurationRequest` is sent as.
fn delivery(log_type: &str, destination: Option<&str>) -> Vec<(String, String)> {
    let prefix = "LogDeliveryConfigurations.LogDeliveryConfigurationRequest.1";
    let mut pairs = vec![
        (format!("{prefix}.LogType"), log_type.to_string()),
        (
            format!("{prefix}.Enabled"),
            destination.is_some().to_string(),
        ),
    ];
    if let Some(destination) = destination {
        pairs.push((
            format!("{prefix}.DestinationType"),
            destination.to_string(),
        ));
        if destination == CLOUDWATCH {
            pairs.push((
                format!("{prefix}.DestinationDetails.CloudWatchLogsDetails.LogGroup"),
                "/aws/elasticache/withdrawal-window".to_string(),
            ));
        }
        pairs.push((format!("{prefix}.LogFormat"), "json".to_string()));
    }
    pairs
}

/// The `(LogType, DestinationType, Status)` of every configuration `reply` reports, in the
/// order they are reported.
fn configurations(reply: &Reply) -> Vec<(String, String, String)> {
    let types = reply.all("LogDeliveryConfiguration/LogType");
    let destinations = reply.all("LogDeliveryConfiguration/DestinationType");
    let statuses = reply.all("LogDeliveryConfiguration/Status");
    assert_eq!(
        (types.len(), destinations.len()),
        (statuses.len(), statuses.len()),
        "every configuration reports its log type, its destination and its status, got {}",
        reply.body
    );
    (0..statuses.len())
        .map(|at| {
            (
                types[at].to_string(),
                destinations[at].to_string(),
                statuses[at].to_string(),
            )
        })
        .collect()
}

/// Send one `ModifyCacheCluster` that changes the log delivery of `cluster`, applied at once,
/// and answer what it reported.
async fn change(
    emulator: &Emulator,
    case: &str,
    cluster: &str,
    pairs: Vec<(String, String)>,
) -> Reply {
    let mut request = params(&[("CacheClusterId", cluster), ("ApplyImmediately", "true")]);
    request.extend(pairs);
    let answered = emulator.call("ModifyCacheCluster", &request).await;
    assert_eq!(
        answered.status, 200,
        "case {case:?}: the change must be served, got {} {}",
        answered.status, answered.body
    );
    answered
}

/// Read `cluster` back until every configuration it reports has left its transient status, and
/// answer with that reply.
async fn settled(emulator: &Emulator, case: &str, cluster: &str) -> Reply {
    poll_until(
        emulator,
        REGION,
        case,
        "every log-delivery configuration leaving the status its change put it in",
        "DescribeCacheClusters",
        &params(&[("CacheClusterId", cluster)]),
        |reply| {
            configurations(reply)
                .iter()
                .all(|(_type, _destination, status)| matches!(status.as_str(), "active" | "error"))
        },
    )
    .await
}

/// One row: the cache a change is made to, the change, and what the call that made it reports.
struct Row {
    case: &'static str,
    /// The cluster this row stands up, which also names it.
    cluster: &'static str,
    /// The destination the cluster is CREATED delivering its slow log to, and `None` for a
    /// cache created with no log-delivery configuration at all.
    created_with: Option<&'static str>,
    /// Whether the clock is let settle that configuration before the change is made.
    settle_first: bool,
    /// The log type the change names, and the destination it asks for — `None` to ask for that
    /// log type to be delivered no longer.
    changes: (&'static str, Option<&'static str>),
    /// The `(LogType, DestinationType, Status)` the call that made the change reports.
    reports: &'static [(&'static str, &'static str, &'static str)],
}

const ROWS: &[Row] = &[
    Row {
        case: "a log type newly asked for",
        cluster: "ldw-new",
        created_with: None,
        settle_first: false,
        changes: (SLOW_LOG, Some(FIREHOSE)),
        reports: &[(SLOW_LOG, FIREHOSE, "enabling")],
    },
    Row {
        case: "a log type already configured, re-pointed while it is still being set up",
        cluster: "ldw-repointed",
        created_with: Some(FIREHOSE),
        settle_first: false,
        changes: (SLOW_LOG, Some(CLOUDWATCH)),
        reports: &[(SLOW_LOG, CLOUDWATCH, "modifying")],
    },
    Row {
        case: "a settled delivery re-pointed",
        cluster: "ldw-settled-repointed",
        created_with: Some(FIREHOSE),
        settle_first: true,
        changes: (SLOW_LOG, Some(CLOUDWATCH)),
        reports: &[(SLOW_LOG, CLOUDWATCH, "modifying")],
    },
    Row {
        case: "a settled delivery asked for no longer",
        cluster: "ldw-withdrawn",
        created_with: Some(FIREHOSE),
        settle_first: true,
        changes: (SLOW_LOG, None),
        reports: &[(SLOW_LOG, FIREHOSE, "disabling")],
    },
    Row {
        case: "a delivery still being set up, asked for no longer",
        cluster: "ldw-withdrawn-early",
        created_with: Some(CLOUDWATCH),
        settle_first: false,
        changes: (SLOW_LOG, None),
        reports: &[(SLOW_LOG, CLOUDWATCH, "disabling")],
    },
    Row {
        case: "a log type no configuration is held for, asked for no longer",
        cluster: "ldw-nothing-to-withdraw",
        created_with: None,
        settle_first: false,
        changes: (SLOW_LOG, None),
        reports: &[],
    },
    Row {
        case: "another log type asked for no longer, beside a configured one",
        cluster: "ldw-other-type",
        created_with: Some(FIREHOSE),
        settle_first: true,
        changes: (ENGINE_LOG, None),
        reports: &[(SLOW_LOG, FIREHOSE, "active")],
    },
];

/// Stand the row's cluster up, settled and `available`, with the log delivery it was created
/// with.
async fn planted(emulator: &Emulator, row: &Row) {
    let mut extra: Vec<(String, String)> = params(&[("Engine", "redis"), ("NumCacheNodes", "1")]);
    if let Some(destination) = row.created_with {
        extra.extend(delivery(SLOW_LOG, Some(destination)));
    }
    let borrowed: Vec<(&str, &str)> = extra
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();
    create_available_cluster(emulator, row.cluster, &borrowed).await;
    if row.settle_first {
        let reply = settled(emulator, row.case, row.cluster).await;
        assert_eq!(
            configurations(&reply)
                .iter()
                .map(|(_type, _destination, status)| status.clone())
                .collect::<Vec<String>>(),
            vec!["active".to_string()],
            "case {:?}: the configuration must settle before the change, got {}",
            row.case,
            reply.body
        );
    }
}

#[tokio::test]
async fn a_change_to_a_log_delivery_is_reported_in_the_status_that_change_puts_it_in() {
    let emulator = Emulator::new();
    for row in ROWS {
        let case = row.case;
        planted(&emulator, row).await;
        let (log_type, destination) = row.changes;
        let answered = change(
            &emulator,
            case,
            row.cluster,
            delivery(log_type, destination),
        )
        .await;
        let expected: Vec<(String, String, String)> = row
            .reports
            .iter()
            .map(|(log_type, destination, status)| {
                (
                    (*log_type).to_string(),
                    (*destination).to_string(),
                    (*status).to_string(),
                )
            })
            .collect();
        assert_eq!(
            configurations(&answered),
            expected,
            "case {case:?}: expected the change to report {expected:?}, got {}",
            answered.body
        );
    }
}

#[tokio::test]
async fn the_clock_takes_a_withdrawn_delivery_out_of_the_cache_s_list() {
    let emulator = Emulator::new();
    let case = "a withdrawn delivery beside one that stays";
    let cluster = "ldw-two-types";
    let mut extra: Vec<(String, String)> = params(&[("Engine", "redis"), ("NumCacheNodes", "1")]);
    extra.extend(delivery(SLOW_LOG, Some(FIREHOSE)));
    let borrowed: Vec<(&str, &str)> = extra
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();
    create_available_cluster(&emulator, cluster, &borrowed).await;
    // The second log type, so the withdrawal of one is shown to leave the other alone.
    change(&emulator, case, cluster, delivery(ENGINE_LOG, Some(FIREHOSE))).await;
    let both = settled(&emulator, case, cluster).await;
    assert_eq!(
        configurations(&both),
        vec![
            (SLOW_LOG.to_string(), FIREHOSE.to_string(), "active".to_string()),
            (ENGINE_LOG.to_string(), FIREHOSE.to_string(), "active".to_string()),
        ],
        "case {case:?}: both deliveries must settle active, got {}",
        both.body
    );

    // Asked for no longer: reported being withdrawn, and the other log type untouched.
    let withdrawn = change(&emulator, case, cluster, delivery(SLOW_LOG, None)).await;
    assert_eq!(
        configurations(&withdrawn),
        vec![
            (
                SLOW_LOG.to_string(),
                FIREHOSE.to_string(),
                "disabling".to_string()
            ),
            (ENGINE_LOG.to_string(), FIREHOSE.to_string(), "active".to_string()),
        ],
        "case {case:?}: the withdrawal must be reported on the log type it names alone, got {}",
        withdrawn.body
    );

    // And the clock takes it out of the list, which is the only way it leaves.
    let after = poll_until(
        &emulator,
        REGION,
        case,
        "the withdrawn configuration being taken out of the list",
        "DescribeCacheClusters",
        &params(&[("CacheClusterId", cluster)]),
        |reply| {
            !configurations(reply)
                .iter()
                .any(|(log_type, _destination, _status)| log_type == SLOW_LOG)
        },
    )
    .await;
    assert_eq!(
        configurations(&after),
        vec![(ENGINE_LOG.to_string(), FIREHOSE.to_string(), "active".to_string())],
        "case {case:?}: the cache must keep the delivery nobody withdrew, got {}",
        after.body
    );
}

#[tokio::test]
async fn a_withdrawn_group_delivery_leaves_its_members_reporting_none() {
    let emulator = Emulator::new();
    let case = "a replication group's delivery asked for no longer";
    let group = "ldw-group";
    let mut request = params(&[
        ("ReplicationGroupId", group),
        ("ReplicationGroupDescription", "the delivery under test"),
        ("Engine", "redis"),
        ("NumCacheClusters", "1"),
    ]);
    request.extend(delivery(SLOW_LOG, Some(FIREHOSE)));
    let created = emulator.call("CreateReplicationGroup", &request).await;
    assert_eq!(
        created.status, 200,
        "case {case:?}: the group must be created, got {} {}",
        created.status, created.body
    );
    let available = poll_until(
        &emulator,
        REGION,
        case,
        "the group reaching available with its delivery settled",
        "DescribeReplicationGroups",
        &params(&[("ReplicationGroupId", group)]),
        |reply| {
            reply.text("Status") == Some("available")
                && configurations(reply)
                    .iter()
                    .all(|(_type, _destination, status)| status == "active")
        },
    )
    .await;
    let members = available.all("NodeGroupMember/CacheClusterId");
    assert_eq!(
        members.len(),
        1,
        "case {case:?}: the group must report its one member, got {}",
        available.body
    );
    let member = members[0].to_string();

    // The member carries the group's delivery, and says so beside it.
    let before = emulator
        .call("DescribeCacheClusters", &params(&[("CacheClusterId", &member)]))
        .await;
    assert_eq!(
        (
            configurations(&before).len(),
            before.text("ReplicationGroupLogDeliveryEnabled"),
        ),
        (1, Some("true")),
        "case {case:?}: the member must carry the group's delivery, got {}",
        before.body
    );

    // Asked for no longer through the group: reported being withdrawn on the group itself.
    let mut withdraw = params(&[("ReplicationGroupId", group), ("ApplyImmediately", "true")]);
    withdraw.extend(delivery(SLOW_LOG, None));
    let withdrawn = emulator.call("ModifyReplicationGroup", &withdraw).await;
    assert_eq!(
        withdrawn.status, 200,
        "case {case:?}: the change must be served, got {} {}",
        withdrawn.status, withdrawn.body
    );
    assert_eq!(
        configurations(&withdrawn),
        vec![(
            SLOW_LOG.to_string(),
            FIREHOSE.to_string(),
            "disabling".to_string()
        )],
        "case {case:?}: the group must report its delivery going, got {}",
        withdrawn.body
    );

    // And the clock takes it out of the group's list and the member's — which is when the
    // member stops saying it delivers any of the group's logs.
    let after = poll_until(
        &emulator,
        REGION,
        case,
        "the withdrawn configuration being taken out of the group's list",
        "DescribeReplicationGroups",
        &params(&[("ReplicationGroupId", group)]),
        |reply| configurations(reply).is_empty(),
    )
    .await;
    assert!(
        configurations(&after).is_empty(),
        "case {case:?}: the group must carry no configuration once the withdrawal is \
         through, got {}",
        after.body
    );
    let member_after = poll_until(
        &emulator,
        REGION,
        case,
        "the member reporting no delivery of the group's logs",
        "DescribeCacheClusters",
        &params(&[("CacheClusterId", &member)]),
        |reply| configurations(reply).is_empty(),
    )
    .await;
    assert_eq!(
        member_after.text("ReplicationGroupLogDeliveryEnabled"),
        Some("false"),
        "case {case:?}: a member carrying no configuration must not report delivering the \
         group's logs, got {}",
        member_after.body
    );
}

#[tokio::test]
async fn a_delivery_asked_for_again_while_it_is_being_withdrawn_is_set_up_instead() {
    let emulator = Emulator::new();
    let case = "a withdrawal a client changed its mind about";
    let cluster = "ldw-re-asked";
    let mut extra: Vec<(String, String)> = params(&[("Engine", "redis"), ("NumCacheNodes", "1")]);
    extra.extend(delivery(SLOW_LOG, Some(FIREHOSE)));
    let borrowed: Vec<(&str, &str)> = extra
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();
    create_available_cluster(&emulator, cluster, &borrowed).await;
    settled(&emulator, case, cluster).await;

    let withdrawn = change(&emulator, case, cluster, delivery(SLOW_LOG, None)).await;
    assert_eq!(
        configurations(&withdrawn),
        vec![(
            SLOW_LOG.to_string(),
            FIREHOSE.to_string(),
            "disabling".to_string()
        )],
        "case {case:?}: the withdrawal must be reported, got {}",
        withdrawn.body
    );

    // Asked for again before the withdrawal is through: a delivery being set up, not a change
    // to one that is carrying.
    let again = change(
        &emulator,
        case,
        cluster,
        delivery(SLOW_LOG, Some(CLOUDWATCH)),
    )
    .await;
    assert_eq!(
        configurations(&again),
        vec![(
            SLOW_LOG.to_string(),
            CLOUDWATCH.to_string(),
            "enabling".to_string()
        )],
        "case {case:?}: asking for the log type again must set a delivery up, got {}",
        again.body
    );

    // And it settles rather than being taken out of the list with the withdrawal it replaced.
    let after = settled(&emulator, case, cluster).await;
    let statuses: Vec<String> = configurations(&after)
        .into_iter()
        .map(|(_type, _destination, status)| status)
        .collect();
    assert_eq!(
        statuses.len(),
        1,
        "case {case:?}: the configuration asked for again must still be held, got {}",
        after.body
    );
    assert_ne!(
        statuses[0], "disabling",
        "case {case:?}: it must settle rather than stay a withdrawal, got {}",
        after.body
    );
}
