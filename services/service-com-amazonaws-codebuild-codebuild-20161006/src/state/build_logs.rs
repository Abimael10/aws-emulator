// SPDX-License-Identifier: CC0-1.0

//! WHERE one build's output went: the log group and stream a build reports.
//!
//! # The rule this module implements
//!
//! A build carries a `logs` ([`LogsLocation`]) that says where its output was delivered, and
//! CodeBuild delivers to CloudWatch Logs unless a project turns that off: a build whose
//! CloudWatch delivery is on reports the GROUP and the STREAM its own output went to, so a
//! client can find one build's log events from the build record alone. Both names follow from
//! the configuration and the build:
//!
//! * the group is `CloudWatchLogsConfig$groupName` when the configuration names one, and
//!   otherwise the group CodeBuild creates for the project it belongs to
//!   ([`DEFAULT_GROUP_PREFIX`]);
//! * `CloudWatchLogsConfig$streamName` is "the PREFIX of the stream name", not the stream, so
//!   the stream one build wrote is that prefix followed by the build's own half of its id
//!   ([`super::uuid_half`]) — and that half alone when the configuration names no prefix. Two
//!   builds of one project therefore never report the same stream.
//! * `LogsLocation$cloudWatchLogsArn` states its own format
//!   (`arn:${Partition}:logs:${Region}:${Account}:log-group:${LogGroupName}:log-stream:${LogStreamName}`),
//!   so it is composed from the two names above rather than stated separately.
//! * `deepLink` is "the URL to an individual build log in CloudWatch Logs", so it is the
//!   CloudWatch Logs console address of that same group and stream
//!   ([`CLOUD_WATCH_CONSOLE`]) — composed from the two names for the same reason.
//!
//! A configuration that DISABLES CloudWatch delivery leaves all four absent: there is no
//! group, no stream, no stream ARN and no log to link to for output that was not delivered
//! there. The configurations themselves (`cloudWatchLogs`, `s3Logs`) are reported as the
//! build RUNS with them, which for a build whose project configures neither is CloudWatch
//! `ENABLED` and Amazon S3 `DISABLED` — the delivery CodeBuild performs for a project that
//! configures no logs at all.
//!
//! `s3DeepLink` and `s3LogsArn` are left absent: both address the S3 object an upload of the
//! build's log file created, which happens only for a build whose output was delivered to a
//! bucket.

use crate::ops::types::{CloudWatchLogsConfig, LogsConfig, LogsLocation, S3LogsConfig};

/// The group CodeBuild delivers a project's build logs to when the project names none:
/// `/aws/codebuild/<projectName>`, the group its default log configuration creates.
pub const DEFAULT_GROUP_PREFIX: &str = "/aws/codebuild/";

/// `LogsConfigStatusType` `ENABLED`, the delivery a build whose project configures no logs
/// still gets in CloudWatch Logs.
pub const LOGS_ENABLED: &str = "ENABLED";

/// `LogsConfigStatusType` `DISABLED`.
pub const LOGS_DISABLED: &str = "DISABLED";

/// The ARN namespace the CloudWatch Logs group and stream of a build live under.
const LOGS_ARN_NAMESPACE: &str = "logs";

/// The CloudWatch Logs console view of ONE log stream, which is what `deepLink` addresses:
/// the console's log-event view takes the group and the stream in its fragment
/// (`#logEvent:group=<group>;stream=<stream>`).
const CLOUD_WATCH_CONSOLE: &str = "https://console.aws.amazon.com/cloudwatch/home";

/// The console address of the log events one build wrote — `LogsLocation$deepLink`.
fn deep_link(region: &str, group: &str, stream: &str) -> String {
    format!("{CLOUD_WATCH_CONSOLE}?region={region}#logEvent:group={group};stream={stream}")
}

/// Whether `config` leaves CloudWatch delivery on: a configuration that states no
/// `cloudWatchLogs` at all, or one whose status is not `DISABLED`.
fn delivers_to_cloud_watch(config: Option<&LogsConfig>) -> bool {
    config
        .and_then(|config| config.cloud_watch_logs.as_ref())
        .is_none_or(|logs| logs.status != LOGS_DISABLED)
}

/// The CloudWatch configuration a build RUNS with: the project's own, and the default
/// delivery (`ENABLED`, with no group or prefix named) for a build whose project configures
/// none.
fn effective_cloud_watch_logs(config: Option<&LogsConfig>) -> CloudWatchLogsConfig {
    config
        .and_then(|config| config.cloud_watch_logs.clone())
        .unwrap_or_else(|| CloudWatchLogsConfig {
            status: LOGS_ENABLED.to_string(),
            group_name: None,
            stream_name: None,
        })
}

/// The S3 configuration a build RUNS with: the project's own, and `DISABLED` for a build
/// whose project configures none — CodeBuild uploads no log file for such a build.
fn effective_s3_logs(config: Option<&LogsConfig>) -> S3LogsConfig {
    config
        .and_then(|config| config.s3_logs.clone())
        .unwrap_or_else(|| S3LogsConfig {
            status: LOGS_DISABLED.to_string(),
            location: None,
            encryption_disabled: None,
            bucket_owner_access: None,
        })
}

/// The STREAM one build's output went to: the configured prefix followed by the build's own
/// id, and the id alone when no prefix is configured.
///
/// `build_id` is the build's id as this service mints it (`<projectName>:<uuid>`), and the
/// uuid half is what names the stream — the same half the session a build assumes its role
/// in is named after.
fn stream_name(configured_prefix: Option<&str>, build_id: &str) -> String {
    let uuid = super::uuid_half(build_id);
    match configured_prefix.map(str::trim).filter(|p| !p.is_empty()) {
        Some(prefix) => format!("{}/{uuid}", prefix.trim_end_matches('/')),
        None => uuid.to_string(),
    }
}

/// Where the build `build_id` of the project `project_name` delivered its output, under the
/// log configuration `config` it runs with.
#[must_use]
pub fn build_logs_location(
    region: &str,
    project_name: &str,
    build_id: &str,
    config: Option<&LogsConfig>,
) -> LogsLocation {
    let cloud_watch_logs = effective_cloud_watch_logs(config);
    let s3_logs = effective_s3_logs(config);
    let (group_name, stream_name) = if delivers_to_cloud_watch(config) {
        let group = cloud_watch_logs
            .group_name
            .clone()
            .map(|group| group.trim().to_string())
            .filter(|group| !group.is_empty())
            .unwrap_or_else(|| format!("{DEFAULT_GROUP_PREFIX}{project_name}"));
        let stream = stream_name(cloud_watch_logs.stream_name.as_deref(), build_id);
        (Some(group), Some(stream))
    } else {
        (None, None)
    };
    LogsLocation {
        cloud_watch_logs_arn: group_name
            .as_deref()
            .zip(stream_name.as_deref())
            .map(|(group, stream)| {
                ::emulator_server::emulator_arn(
                    LOGS_ARN_NAMESPACE,
                    region,
                    &format!("log-group:{group}:log-stream:{stream}"),
                )
            }),
        deep_link: group_name
            .as_deref()
            .zip(stream_name.as_deref())
            .map(|(group, stream)| deep_link(region, group, stream)),
        group_name,
        stream_name,
        // The object an S3 log upload created; see the module docs.
        s3_deep_link: None,
        s3_logs_arn: None,
        cloud_watch_logs: Some(cloud_watch_logs),
        s3_logs: Some(s3_logs),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One row of the table below: the case, the log configuration the build runs with, the
    /// build's id, and the group and stream it reports.
    type Row = (&'static str, Option<LogsConfig>, &'static str, Option<&'static str>, Option<&'static str>);

    /// RULE: a build reports the group and the stream its own output went to — the
    /// configured group or the project's default one, and the configured prefix followed by
    /// the build's own id.
    ///
    /// The rows differ along what the rule generalises over: which of the two names the
    /// configuration states, whether delivery is enabled at all, and the id of the build.
    #[test]
    fn a_build_reports_the_group_and_stream_its_output_went_to() {
        let configured = |status: &str, group: Option<&str>, stream: Option<&str>| {
            Some(LogsConfig {
                cloud_watch_logs: Some(CloudWatchLogsConfig {
                    status: status.to_string(),
                    group_name: group.map(str::to_string),
                    stream_name: stream.map(str::to_string),
                }),
                s3_logs: None,
            })
        };
        // (case, the configuration, the build id, the group, the stream)
        let rows: Vec<Row> = vec![
            (
                "no configuration at all",
                None,
                "pipe:1111-2222",
                Some("/aws/codebuild/pipe"),
                Some("1111-2222"),
            ),
            (
                "a configuration naming neither name",
                configured(LOGS_ENABLED, None, None),
                "pipe:1111-2222",
                Some("/aws/codebuild/pipe"),
                Some("1111-2222"),
            ),
            (
                "a configured group and no prefix",
                configured(LOGS_ENABLED, Some("own-group"), None),
                "pipe:3333",
                Some("own-group"),
                Some("3333"),
            ),
            (
                "a configured prefix",
                configured(LOGS_ENABLED, Some("own-group"), Some("own-stream")),
                "pipe:4444",
                Some("own-group"),
                Some("own-stream/4444"),
            ),
            (
                "a configured prefix and the default group",
                configured(LOGS_ENABLED, None, Some("own-stream")),
                "pipe:5555",
                Some("/aws/codebuild/pipe"),
                Some("own-stream/5555"),
            ),
            (
                "delivery disabled",
                configured(LOGS_DISABLED, Some("own-group"), Some("own-stream")),
                "pipe:6666",
                None,
                None,
            ),
        ];
        for (case, config, build_id, group, stream) in rows {
            let location = build_logs_location("us-west-2", "pipe", build_id, config.as_ref());
            assert_eq!(
                location.group_name.as_deref(),
                group,
                "case {case:?}: expected the group {group:?}, got {:?}",
                location.group_name
            );
            assert_eq!(
                location.stream_name.as_deref(),
                stream,
                "case {case:?}: expected the stream {stream:?}, got {:?}",
                location.stream_name
            );
            // The stream ARN and the console link are composed from the two names, so they
            // are present exactly when they are.
            let arn = location.cloud_watch_logs_arn.clone();
            let link = location.deep_link.clone();
            match (group, stream) {
                (Some(group), Some(stream)) => {
                    let expected = format!(":log-group:{group}:log-stream:{stream}");
                    assert!(
                        arn.as_deref().is_some_and(|arn| arn.ends_with(&expected)),
                        "case {case:?}: expected a stream ARN ending {expected:?}, got {arn:?}"
                    );
                    let expected = format!("#logEvent:group={group};stream={stream}");
                    assert!(
                        link.as_deref().is_some_and(|link| link.ends_with(&expected)),
                        "case {case:?}: expected a deep link ending {expected:?}, got {link:?}"
                    );
                }
                _ => {
                    assert!(
                        arn.is_none(),
                        "case {case:?}: expected no stream ARN, got {arn:?}"
                    );
                    assert!(
                        link.is_none(),
                        "case {case:?}: expected no deep link, got {link:?}"
                    );
                }
            }
            // The configurations the build runs with are always reported.
            assert!(
                location.cloud_watch_logs.is_some() && location.s3_logs.is_some(),
                "case {case:?}: expected both log configurations, got {:?} and {:?}",
                location.cloud_watch_logs,
                location.s3_logs
            );
        }
    }

    /// RULE: a build whose project configures no logs at all still reports the delivery it
    /// runs with — CloudWatch `ENABLED` and Amazon S3 `DISABLED`.
    #[test]
    fn the_default_delivery_is_cloud_watch_enabled_and_s3_disabled() {
        let location = build_logs_location("us-west-2", "pipe", "pipe:7777", None);
        let cloud_watch = location.cloud_watch_logs.expect("a CloudWatch configuration");
        let s3 = location.s3_logs.expect("an S3 configuration");
        assert_eq!(
            cloud_watch.status, LOGS_ENABLED,
            "case \"no configuration\": expected CloudWatch {LOGS_ENABLED}, got {:?}",
            cloud_watch.status
        );
        assert_eq!(
            s3.status, LOGS_DISABLED,
            "case \"no configuration\": expected S3 {LOGS_DISABLED}, got {:?}",
            s3.status
        );
    }
}
