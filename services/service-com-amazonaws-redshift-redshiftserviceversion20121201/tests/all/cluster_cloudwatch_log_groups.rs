// SPDX-License-Identifier: CC0-1.0

//! What BACKS a cluster's `cloudwatch` audit-log delivery, end to end: the log
//! groups Amazon CloudWatch Logs holds once `EnableLogging` has been accepted.
//!
//! The RULE every case here asserts: enabling a cluster's audit logging with
//! `LogDestinationType=cloudwatch` makes the service itself create, in Amazon
//! CloudWatch Logs and with NO further client action, one log group per log type
//! the request exports, named `/aws/redshift/cluster/<ClusterIdentifier>/<log
//! type>` — the cluster identifier from the request and the log type from
//! `LogExports` being the variable parts. A delivery to another destination
//! creates none: an `s3` delivery writes objects into the bucket it names, and
//! `s3table` publishes system tables, so neither is backed by a log group.
//!
//! Amazon CloudWatch Logs is mounted beside Redshift on ONE engine (the shared
//! harness) and every assertion reads the groups back through THAT service's own
//! `DescribeLogGroups`, listed under the cluster's prefix the way a client lists
//! them — nothing here reaches into either store, so a group exists only because
//! CloudWatch Logs reports holding it.
//!
//! The rows differ along what the rule generalises over: how many log types the
//! delivery exports and which ones, the identifier of the cluster they are the
//! logs of, a log type named twice, and a second `EnableLogging` over the same
//! cluster.

use service_com_amazonaws_redshift_redshiftserviceversion20121201::state;

use crate::cluster_lifecycle::Emulator;

/// The log types the operation's own documentation publishes for an `s3` or
/// `cloudwatch` delivery — `connectionlog`, `useractivitylog` and `userlog`
/// (`smithy_query(query="doc", shape="EnableLoggingMessage",
/// member="LogExports")`). The member itself is modelled as a list of bare
/// strings, so the catalogue is documentation rather than a constraint.
const PUBLISHED_LOG_TYPES: [&str; 3] = ["connectionlog", "useractivitylog", "userlog"];

/// Create `identifier` and leave it SERVING: a cluster the service is still
/// working on takes no configuration change.
///
/// The provisioning span is crossed by moving the emulator's own clock, and the
/// cluster is then read until it reports itself available — the CONDITION the
/// case waits on — so nothing here waits for a fixed time.
async fn serving_cluster(emulator: &Emulator, case: &str, identifier: &str) {
    let created = emulator.create_minimal(identifier).await;
    assert_eq!(
        created.status, 200,
        "case {case:?}: expected HTTP 200 from CreateCluster {identifier}, got {} with body {}",
        created.status, created.body
    );
    emulator.advance_clock(state::PROVISIONING_SPAN);
    emulator
        .poll_until(
            case,
            "DescribeClusters",
            &[("ClusterIdentifier", identifier)],
            |answer| answer.text("ClusterStatus").as_deref() == Some(state::STATUS_AVAILABLE),
        )
        .await;
}

/// `EnableLogging` for `cluster`, exporting `log_types` to `destination`.
async fn enable_logging(
    emulator: &Emulator,
    cluster: &str,
    destination: &str,
    log_types: &[&str],
) -> crate::cluster_lifecycle::Answer {
    let members: Vec<(String, String)> = log_types
        .iter()
        .enumerate()
        .map(|(index, log_type)| {
            (
                format!("LogExports.member.{}", index + 1),
                (*log_type).to_string(),
            )
        })
        .collect();
    let mut params: Vec<(&str, &str)> = vec![
        ("ClusterIdentifier", cluster),
        ("LogDestinationType", destination),
    ];
    // An S3 delivery is refused without a bucket, so a row asking for one names
    // the bucket Amazon S3 would be asked about; this suite mounts no S3, which
    // establishes nothing about the bucket and so refuses nothing.
    if destination == state::LOG_DESTINATION_S3 {
        params.push(("BucketName", "redshift-cloudwatch-suite-bucket"));
    }
    params.extend(
        members
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str())),
    );
    emulator.call("EnableLogging", &params).await
}

/// The log group names Amazon CloudWatch Logs holds under `cluster`'s own
/// prefix, sorted, as a client reading them back sees them.
async fn held_log_groups(emulator: &Emulator, case: &str, cluster: &str) -> Vec<String> {
    held_under_prefix(emulator, case, &state::cloudwatch_log_group_prefix(cluster)).await
}

/// The log group names Amazon CloudWatch Logs holds under `prefix`, sorted —
/// the read a client makes, so a case may name the documented prefix itself
/// rather than build it from the crate under test.
async fn held_under_prefix(emulator: &Emulator, case: &str, prefix: &str) -> Vec<String> {
    let (status, body) = emulator
        .call_logs(
            "DescribeLogGroups",
            &serde_json::json!({ "logGroupNamePrefix": prefix }),
        )
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeLogGroups under {prefix:?}, got {status} \
         with body {body}"
    );
    let mut held: Vec<String> = match body.get("logGroups").and_then(serde_json::Value::as_array) {
        Some(groups) => groups
            .iter()
            .filter_map(|group| {
                group
                    .get("logGroupName")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
            })
            .collect(),
        None => Vec::new(),
    };
    held.sort();
    held
}

/// The group names a delivery exporting `log_types` for `cluster` must leave
/// behind, sorted and deduplicated.
fn expected_log_groups(cluster: &str, log_types: &[&str]) -> Vec<String> {
    let mut expected: Vec<String> = log_types
        .iter()
        .map(|log_type| state::cloudwatch_log_group(cluster, log_type))
        .collect();
    expected.sort();
    expected.dedup();
    expected
}

/// One row of the table: the cluster the delivery is enabled on and the log
/// types it exports.
struct Row {
    case: &'static str,
    cluster: &'static str,
    log_types: &'static [&'static str],
}

/// The rows the table spans: one exported type, two, all three the operation
/// publishes, a type named twice, and a cluster whose identifier is of another
/// shape — the identifier and the log types being what the name is built from.
const ROWS: &[Row] = &[
    Row {
        case: "one exported log type",
        cluster: "cwlogs-one-type",
        log_types: &["userlog"],
    },
    Row {
        case: "two exported log types",
        cluster: "cwlogs-two-types",
        log_types: &["connectionlog", "userlog"],
    },
    Row {
        case: "every log type the operation publishes",
        cluster: "cwlogs-every-type",
        log_types: &PUBLISHED_LOG_TYPES,
    },
    Row {
        case: "one log type named twice",
        cluster: "cwlogs-repeated-type",
        log_types: &["connectionlog", "connectionlog"],
    },
    Row {
        case: "a cluster under a long hyphenated identifier",
        cluster: "c-1-cwlogs-delivery-target-cluster-with-a-long-name-000000000",
        log_types: &["useractivitylog", "connectionlog"],
    },
];

#[tokio::test]
async fn a_cloudwatch_delivery_is_backed_by_a_log_group_per_exported_log_type() {
    let emulator = Emulator::with_logs();

    for row in ROWS {
        let case = row.case;
        serving_cluster(&emulator, case, row.cluster).await;

        // Before the delivery is enabled there is nothing under the cluster's
        // prefix, so what the assertion below reads is this call's own effect.
        assert_eq!(
            held_log_groups(&emulator, case, row.cluster).await,
            Vec::<String>::new(),
            "case {case:?}: expected no log group under the cluster's prefix before its \
             delivery is enabled"
        );

        let answered = enable_logging(
            &emulator,
            row.cluster,
            state::LOG_DESTINATION_CLOUDWATCH,
            row.log_types,
        )
        .await;
        assert_eq!(
            answered.status, 200,
            "case {case:?}: expected HTTP 200 from EnableLogging, got {} with body {}",
            answered.status, answered.body
        );

        // ONE call, no further client action: the groups are there as soon as
        // the delivery is accepted.
        let held = held_log_groups(&emulator, case, row.cluster).await;
        let expected = expected_log_groups(row.cluster, row.log_types);
        assert_eq!(
            held, expected,
            "case {case:?}: expected the log groups {expected:?}, got {held:?}"
        );

        // Enabling the same delivery again leaves the same groups: a group
        // already there is the one this cluster's logs go to.
        let again = enable_logging(
            &emulator,
            row.cluster,
            state::LOG_DESTINATION_CLOUDWATCH,
            row.log_types,
        )
        .await;
        assert_eq!(
            again.status, 200,
            "case {case:?}: expected HTTP 200 from the second EnableLogging, got {} with body {}",
            again.status, again.body
        );
        let held_again = held_log_groups(&emulator, case, row.cluster).await;
        assert_eq!(
            held_again, expected,
            "case {case:?}: expected the second call to leave the log groups {expected:?}, got \
             {held_again:?}"
        );
    }
}

#[tokio::test]
async fn a_delivery_to_another_destination_is_backed_by_no_log_group() {
    let emulator = Emulator::with_logs();

    // The destinations the enum publishes beside `cloudwatch`
    // (`smithy_query(query="shape", shape="LogDestinationType")`), neither of
    // which is delivered into a log group.
    for (index, destination) in [state::LOG_DESTINATION_S3, state::LOG_DESTINATION_S3_TABLE]
        .into_iter()
        .enumerate()
    {
        let case = format!("the {destination} destination");
        let cluster = format!("cwlogs-other-destination-{index}");
        serving_cluster(&emulator, &case, &cluster).await;

        let answered = enable_logging(
            &emulator,
            &cluster,
            destination,
            &["connectionlog", "userlog"],
        )
        .await;
        assert_eq!(
            answered.status, 200,
            "case {case:?}: expected HTTP 200 from EnableLogging, got {} with body {}",
            answered.status, answered.body
        );
        let held = held_log_groups(&emulator, &case, &cluster).await;
        assert_eq!(
            held,
            Vec::<String>::new(),
            "case {case:?}: expected no log group under the cluster's prefix, got {held:?}"
        );
    }
}

#[tokio::test]
async fn a_value_that_is_no_exported_log_type_is_backed_by_no_log_group() {
    let case = "a delivery naming one published log type and two values that are not log types";
    let emulator = Emulator::with_logs();
    let cluster = "cwlogs-unknown-type";
    serving_cluster(&emulator, case, cluster).await;

    // The published catalogue is the three types above; a value outside it is no
    // log type Amazon Redshift exports, so it is backed by no group — only the
    // one published type the same request names is.
    let answered = enable_logging(
        &emulator,
        cluster,
        state::LOG_DESTINATION_CLOUDWATCH,
        &["userlog", "auditlog", "all"],
    )
    .await;
    assert_eq!(
        answered.status, 200,
        "case {case:?}: expected HTTP 200 from EnableLogging, got {} with body {}",
        answered.status, answered.body
    );

    let held = held_log_groups(&emulator, case, cluster).await;
    let expected = expected_log_groups(cluster, &["userlog"]);
    assert_eq!(
        held, expected,
        "case {case:?}: expected the log groups {expected:?}, got {held:?}"
    );
}

#[tokio::test]
async fn the_log_groups_carry_the_documented_names() {
    let case = "the documented group name of each exported log type";
    let emulator = Emulator::with_logs();
    // The name rule and the prefix are spelled OUT here, so the suite pins the
    // client-visible names (`/aws/redshift/cluster/<ClusterIdentifier>/<log
    // type>`, which is what a client lists and attaches to) independently of
    // what the crate under test builds.
    let cluster = "cwlogs-documented-name";
    serving_cluster(&emulator, case, cluster).await;

    let answered = enable_logging(
        &emulator,
        cluster,
        "cloudwatch",
        &["connectionlog", "useractivitylog", "userlog"],
    )
    .await;
    assert_eq!(
        answered.status, 200,
        "case {case:?}: expected HTTP 200 from EnableLogging, got {} with body {}",
        answered.status, answered.body
    );

    let held = held_under_prefix(
        &emulator,
        case,
        "/aws/redshift/cluster/cwlogs-documented-name/",
    )
    .await;
    assert_eq!(
        held,
        vec![
            "/aws/redshift/cluster/cwlogs-documented-name/connectionlog".to_string(),
            "/aws/redshift/cluster/cwlogs-documented-name/useractivitylog".to_string(),
            "/aws/redshift/cluster/cwlogs-documented-name/userlog".to_string(),
        ],
        "case {case:?}: expected the documented log group names, got {held:?}"
    );
}

#[tokio::test]
async fn the_log_group_of_one_cluster_names_that_cluster_alone() {
    let case = "two clusters exporting the same log type";
    let emulator = Emulator::with_logs();
    let first = "cwlogs-tenant-one";
    let second = "cwlogs-tenant-two";
    serving_cluster(&emulator, case, first).await;
    serving_cluster(&emulator, case, second).await;

    for cluster in [first, second] {
        let answered = enable_logging(
            &emulator,
            cluster,
            state::LOG_DESTINATION_CLOUDWATCH,
            &["connectionlog"],
        )
        .await;
        assert_eq!(
            answered.status, 200,
            "case {case:?}: expected HTTP 200 from EnableLogging for {cluster}, got {} with \
             body {}",
            answered.status, answered.body
        );
    }

    // Each cluster's delivery is backed by ITS OWN group, under its own prefix:
    // the groups of one cluster are not the groups of another.
    for cluster in [first, second] {
        let held = held_log_groups(&emulator, case, cluster).await;
        let expected = expected_log_groups(cluster, &["connectionlog"]);
        assert_eq!(
            held, expected,
            "case {case:?}: expected {cluster} to hold the log groups {expected:?}, got {held:?}"
        );
    }
}
