// SPDX-License-Identifier: CC0-1.0

//! The instance log STREAMING an environment's `aws:elasticbeanstalk:cloudwatch:logs`
//! configuration turns on: the CloudWatch Logs groups it publishes, the retention it keeps
//! them for, and the application output it delivers into them.
//!
//! "StreamLogs: whether to create groups in CloudWatch Logs for proxy and deployment logs,
//! and stream logs from each instance in your environment" — so an environment whose
//! configuration sets it is one whose instance log files really are in CloudWatch Logs, under
//! the name Elastic Beanstalk derives for each of them:
//! `/aws/elasticbeanstalk/<environment name>` followed by the file's own path on the instance
//! ([`log_group_name`]). "RetentionInDays: the number of days to keep log events before they
//! expire" is kept on each of those GROUPS, which is where a client reads it back
//! (`DescribeLogGroups` reports it as `retentionInDays`).
//!
//! Three things follow from that, and all three are what this module does:
//!
//! * the groups are PUBLISHED by the configuration, so they exist from the moment it is
//!   accepted rather than from the first record — a client turns streaming on and reads the
//!   groups back, with their retention, before anything has been logged;
//! * only the files the environment's PLATFORM streams get a group ([`streamed_log_paths`]):
//!   a file another platform's log task publishes (a Ruby platform's Puma log on a Python
//!   platform) is one this environment never writes, so it has no group at all;
//! * the application's own standard output is DELIVERED as log events into the group of the
//!   file it goes to on the instance ([`APPLICATION_STDOUT_LOG`]), in a stream per instance
//!   the environment runs, so each event is attributable to one of the instance ids
//!   `DescribeEnvironmentResources` reports.
//!
//! Nothing here is synthesised. The groups are created and the events are ingested through
//! CloudWatch Logs' OWN API over this emulator's in-process callback
//! ([`::service_common::cloudwatch_logs`], the one path every service delivering records on
//! the account's behalf writes through), so a group this module publishes is a group that
//! service holds and an event it delivers is one `GetLogEvents` reads. What is delivered is
//! what the deployed application WROTE when it ran ([`super::deployed_application`]) — an
//! environment running no application of its own delivers no event, rather than output
//! nobody produced.

use super::*;

/// The configuration-option namespace of an environment's instance log streaming.
pub const NAMESPACE_CLOUDWATCH_LOGS: &str = "aws:elasticbeanstalk:cloudwatch:logs";

/// "StreamLogs: whether to create groups in CloudWatch Logs for proxy and deployment logs,
/// and stream logs from each instance in your environment."
pub const OPTION_STREAM_LOGS: &str = "StreamLogs";

/// "RetentionInDays: the number of days to keep log events before they expire."
pub const OPTION_RETENTION_IN_DAYS: &str = "RetentionInDays";

/// The prefix Elastic Beanstalk derives every instance log group's name under: the published
/// `/aws/elasticbeanstalk/<environment name>`, which the file's own path on the instance then
/// completes.
const LOG_GROUP_NAME_PREFIX: &str = "/aws/elasticbeanstalk";

/// The on-instance log files EVERY Elastic Beanstalk platform streams: the two its own
/// deployment engine writes, and the file the platform captures the application's standard
/// output into ([`APPLICATION_STDOUT_LOG`], named once in this crate so the file the
/// compiled environment information reports and the file streamed here are the same one).
///
/// SOURCE: the instance log files Elastic Beanstalk's CloudWatch Logs documentation lists for
/// an environment with log streaming turned on.
const PLATFORM_STREAMED_LOGS: &[&str] = &[
    "/var/log/eb-engine.log",
    "/var/log/eb-hooks.log",
    APPLICATION_STDOUT_LOG,
];

/// The web proxy's own two log files, streamed by every platform that proxies requests to the
/// application through nginx.
const NGINX_STREAMED_LOGS: &[&str] = &["/var/log/nginx/access.log", "/var/log/nginx/error.log"];

/// What ONE platform category streams BESIDE [`PLATFORM_STREAMED_LOGS`]: its proxy's logs and
/// the log file its own application server writes.
///
/// SOURCE: the same documentation's per-platform rows. A category this table has no row for
/// streams what every platform streams and nothing of another platform's: the files a
/// platform does not write are files it has no group for.
const PLATFORM_CATEGORY_STREAMED_LOGS: &[(&str, &[&str])] = &[
    (".NET Core", NGINX_STREAMED_LOGS),
    ("Docker", DOCKER_STREAMED_LOGS),
    ("Go", NGINX_STREAMED_LOGS),
    ("Java", NGINX_STREAMED_LOGS),
    ("Node.js", NGINX_STREAMED_LOGS),
    ("PHP", PHP_STREAMED_LOGS),
    ("Python", NGINX_STREAMED_LOGS),
    ("Ruby", RUBY_STREAMED_LOGS),
    ("Tomcat", TOMCAT_STREAMED_LOGS),
];

/// A Docker platform's own: the output of the container the environment runs, beside the
/// proxy's logs.
const DOCKER_STREAMED_LOGS: &[&str] = &[
    "/var/log/nginx/access.log",
    "/var/log/nginx/error.log",
    "/var/log/eb-docker/containers/eb-current-app/stdouterr.log",
];

/// A PHP platform's own: the process manager's error log, beside the proxy's logs.
const PHP_STREAMED_LOGS: &[&str] = &[
    "/var/log/nginx/access.log",
    "/var/log/nginx/error.log",
    "/var/log/php-fpm/www-error.log",
];

/// A Ruby platform's own: the Puma server's log, beside the proxy's logs.
const RUBY_STREAMED_LOGS: &[&str] = &[
    "/var/log/nginx/access.log",
    "/var/log/nginx/error.log",
    "/var/log/puma/puma.log",
];

/// A Tomcat platform's own: the servlet container's own output, beside the proxy's logs.
const TOMCAT_STREAMED_LOGS: &[&str] = &[
    "/var/log/nginx/access.log",
    "/var/log/nginx/error.log",
    "/var/log/tomcat/catalina.out",
];

/// The log group the on-instance file `path` of the environment `environment_name` is
/// streamed to: the published prefix, the environment's name, and the file's own absolute
/// path on the instance.
#[must_use]
pub fn log_group_name(environment_name: &str, path: &str) -> String {
    format!(
        "{LOG_GROUP_NAME_PREFIX}/{environment_name}/{}",
        path.trim_start_matches('/')
    )
}

/// Every on-instance log file the platform of category `platform_category` streams, in the
/// order they are published.
///
/// A platform streams what every platform streams plus its own category's files
/// ([`PLATFORM_CATEGORY_STREAMED_LOGS`]); a category this service has no row for — a custom
/// platform version's — streams the first set alone, because nothing is known about log files
/// of its own.
#[must_use]
pub fn streamed_log_paths(platform_category: Option<&str>) -> Vec<&'static str> {
    let category = platform_category
        .and_then(|category| {
            PLATFORM_CATEGORY_STREAMED_LOGS
                .iter()
                .find(|(named, _)| *named == category)
        })
        .map(|(_, paths)| *paths)
        .unwrap_or_default();
    let mut paths: Vec<&'static str> = PLATFORM_STREAMED_LOGS.to_vec();
    for path in category {
        if !paths.contains(path) {
            paths.push(path);
        }
    }
    paths
}

/// What one environment's instance log streaming is: the groups it publishes, the retention
/// they are kept for, and the instances whose logs are streamed into them.
#[derive(Debug, Clone)]
pub struct InstanceLogStreaming {
    /// The Region the groups are held in, which is the environment's own.
    pub region: String,
    pub environment_name: String,
    /// "The number of days to keep log events before they expire", as the configuration
    /// states it for these groups.
    pub retention_in_days: i32,
    /// The on-instance files the environment's platform streams
    /// ([`streamed_log_paths`]).
    pub log_paths: Vec<&'static str>,
    /// The Amazon EC2 instances the environment runs, which is what the streams inside the
    /// groups are named after — the same ids `DescribeEnvironmentResources` reports.
    pub instance_ids: Vec<String>,
}

impl InstanceLogStreaming {
    /// The log group one of this environment's streamed files is published under.
    #[must_use]
    pub fn log_group(&self, path: &str) -> String {
        log_group_name(&self.environment_name, path)
    }
}

impl ComAmazonawsElasticbeanstalkAwselasticbeanstalkserviceState {
    /// The instance log streaming the environment `environment_id` has turned on — and `None`
    /// when it has not, which is the default (`StreamLogs` is published with `false` as its
    /// value).
    ///
    /// The configuration read is the one the environment is RUNNING OR DEPLOYING: its draft
    /// where an update created one, and otherwise the settings it runs. That is what makes
    /// the groups exist from the moment the configuration turning streaming on is accepted,
    /// rather than only once the clock has finished deploying it. Everything else is read off
    /// the stored environment, so the instances the streams are named after are the ones this
    /// service reports for it.
    pub fn instance_log_streaming(
        &self,
        region: &str,
        environment_id: &str,
    ) -> Option<InstanceLogStreaming> {
        let environment = self.environments.get(environment_id)?;
        let option_settings = environment
            .draft_configuration
            .as_ref()
            .map(|draft| draft.option_settings.as_slice())
            .unwrap_or(&environment.option_settings);
        if !option_is_on(option_settings, OPTION_STREAM_LOGS) {
            return None;
        }
        let platform_category = named_platform_version(
            region,
            environment.solution_stack_name.as_deref(),
            environment.platform_arn.as_deref(),
        )
        .map(|version| version.stack.platform_category);
        Some(InstanceLogStreaming {
            region: region.to_string(),
            environment_name: environment.environment_name.clone(),
            retention_in_days: retention_in_days(option_settings),
            log_paths: streamed_log_paths(platform_category),
            instance_ids: environment
                .resources
                .as_ref()
                .map(|resources| resources.instances.clone())
                .unwrap_or_default(),
        })
    }
}

/// Whether the boolean option `option_name` of the log-streaming namespace is ON in
/// `settings`: the value the configuration set, and otherwise the value the option catalog
/// publishes as that option's default.
fn option_is_on(settings: &[ConfigurationOptionSetting], option_name: &str) -> bool {
    let value: &str = match option_value(settings, NAMESPACE_CLOUDWATCH_LOGS, option_name) {
        Some(value) => value,
        None => published_default(option_name),
    };
    value.trim().eq_ignore_ascii_case(BOOLEAN_TRUE)
}

/// "The number of days to keep log events before they expire", as the configuration states it
/// — and the value the option catalog publishes as that option's default where it states
/// none.
///
/// Only a value the option's own published set admits is honoured
/// ([`LOG_RETENTION_IN_DAYS`], CloudWatch Logs' own retention periods): a value outside it is
/// one no log group could be kept for, so such a group is kept for the option's default
/// instead, exactly as one whose configuration states no retention at all. Validating a
/// configuration's syntax belongs to the operations that TAKE option settings, not to the
/// delivery that reads the configuration they stored.
fn retention_in_days(settings: &[ConfigurationOptionSetting]) -> i32 {
    option_value(settings, NAMESPACE_CLOUDWATCH_LOGS, OPTION_RETENTION_IN_DAYS)
        .map(str::trim)
        .filter(|stated| LOG_RETENTION_IN_DAYS.contains(stated))
        .or_else(|| Some(published_default(OPTION_RETENTION_IN_DAYS)))
        .and_then(|value| value.parse::<i32>().ok())
        .unwrap_or_default()
}

/// The value the published option catalog states for the log-streaming option `option_name`
/// when a configuration sets none.
///
/// Read from that catalog rather than written here a second time, so the value honoured by
/// the delivery and the `DefaultValue` `DescribeConfigurationOptions` publishes for the same
/// option cannot differ.
fn published_default(option_name: &str) -> &'static str {
    GENERAL_CONFIGURATION_OPTIONS
        .iter()
        .find(|option| option.key() == (NAMESPACE_CLOUDWATCH_LOGS, option_name))
        .map(|option| option.default_value)
        .unwrap_or_default()
}

/// PUBLISH this environment's instance log groups and deliver `application_output` into the
/// group of the file the application's standard output goes to.
///
/// Every group the platform streams is created with the retention the configuration states on
/// it, whether or not anything has been written into it yet: that is what makes the groups
/// readable as soon as the configuration turning streaming on is accepted. The output is then
/// delivered as one event per line the application wrote, in a stream named for each instance
/// the environment runs, so a consumer of the group can tell which instance an event came
/// from.
///
/// Best effort, as every vended-logs write is: a group CloudWatch Logs would not create, or a
/// write it refuses, is reported in tracing rather than failing the call the client has
/// already been answered. Call it with NO state guard held — it awaits.
pub async fn stream_instance_logs(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    streaming: &InstanceLogStreaming,
    application_output: &[String],
) {
    for path in &streaming.log_paths {
        let log_group = streaming.log_group(path);
        // A name CloudWatch Logs would not hold names a group nothing could ever be read
        // from, so it is reported rather than attempted: streaming that silently delivers
        // nothing is what this avoids.
        if !::service_common::cloudwatch_logs::is_log_group_name(&log_group) {
            ::tracing::warn!(
                log_group = %log_group,
                "the log group this environment's instance log streaming derives is not a name \
                 CloudWatch Logs holds, so that file is not streamed"
            );
            continue;
        }
        if !::service_common::cloudwatch_logs::provisioned(ctx, &streaming.region, &log_group).await
        {
            continue;
        }
        ::service_common::cloudwatch_logs::retention_set(
            ctx,
            &streaming.region,
            &log_group,
            streaming.retention_in_days,
        )
        .await;
    }
    if application_output.is_empty() {
        return;
    }
    let log_group = streaming.log_group(APPLICATION_STDOUT_LOG);
    for instance_id in &streaming.instance_ids {
        ::service_common::cloudwatch_logs::delivered(
            ctx,
            &streaming.region,
            &log_group,
            instance_id,
            application_output,
        )
        .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: a group is derived for every file the environment's platform streams and for no
    /// other — a file only another platform's log task writes has none — and each group's
    /// name is the published prefix, the environment's name and the file's own path.
    #[test]
    fn only_the_files_the_platform_streams_have_a_group() {
        // (case, the platform category, a file it streams, a file it does not)
        let cases: &[(&str, Option<&str>, &str, &str)] = &[
            (
                "a Python platform",
                Some("Python"),
                APPLICATION_STDOUT_LOG,
                "/var/log/puma/puma.log",
            ),
            (
                "a Ruby platform",
                Some("Ruby"),
                "/var/log/puma/puma.log",
                "/var/log/tomcat/catalina.out",
            ),
            (
                "a Docker platform",
                Some("Docker"),
                "/var/log/eb-docker/containers/eb-current-app/stdouterr.log",
                "/var/log/puma/puma.log",
            ),
            (
                "a Tomcat platform",
                Some("Tomcat"),
                "/var/log/tomcat/catalina.out",
                "/var/log/php-fpm/www-error.log",
            ),
            (
                "a PHP platform",
                Some("PHP"),
                "/var/log/php-fpm/www-error.log",
                "/var/log/puma/puma.log",
            ),
            (
                "a platform whose category this service names no files for",
                None,
                "/var/log/eb-engine.log",
                "/var/log/nginx/access.log",
            ),
        ];
        for (case, category, streamed, not_streamed) in cases {
            let paths = streamed_log_paths(*category);
            assert!(
                paths.contains(streamed),
                "case {case:?}: expected {streamed:?} among the streamed files, got {paths:?}"
            );
            assert!(
                !paths.contains(not_streamed),
                "case {case:?}: expected no group for {not_streamed:?}, got {paths:?}"
            );
            // Every platform streams its deployment engine's log and the file the
            // application's own output goes to.
            for always in PLATFORM_STREAMED_LOGS {
                assert!(
                    paths.contains(always),
                    "case {case:?}: expected {always:?} among the streamed files, got {paths:?}"
                );
            }
            let mut unique = paths.clone();
            unique.sort_unstable();
            unique.dedup();
            assert_eq!(
                unique.len(),
                paths.len(),
                "case {case:?}: expected each file once, got {paths:?}"
            );
        }
    }

    /// RULE: the group a streamed file is published under is the documented prefix, the
    /// environment's name and the file's own path on the instance.
    #[test]
    fn the_group_name_is_the_derived_one() {
        // (case, the environment's name, the on-instance path, the group name)
        let cases: &[(&str, &str, &str, &str)] = &[
            (
                "the application's standard output",
                "logs-env",
                APPLICATION_STDOUT_LOG,
                "/aws/elasticbeanstalk/logs-env/var/log/web.stdout.log",
            ),
            (
                "a file in a directory of its own",
                "my-env-1",
                "/var/log/nginx/access.log",
                "/aws/elasticbeanstalk/my-env-1/var/log/nginx/access.log",
            ),
            (
                "a path given without its leading separator",
                "my-env-1",
                "var/log/eb-engine.log",
                "/aws/elasticbeanstalk/my-env-1/var/log/eb-engine.log",
            ),
        ];
        for (case, environment_name, path, expected) in cases {
            let actual = log_group_name(environment_name, path);
            assert_eq!(
                actual, *expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
            // A derived name has to be one CloudWatch Logs can hold, or no group could ever
            // be created for it.
            assert!(
                ::service_common::cloudwatch_logs::is_log_group_name(&actual),
                "case {case:?}: expected a name CloudWatch Logs holds, got {actual:?}"
            );
        }
    }

    /// One configuration the streaming options are read off, and what they say: a row of
    /// [`the_configuration_decides_whether_streaming_is_on_and_for_how_long`].
    struct ConfigurationRow {
        case: &'static str,
        /// The settings the configuration carries in the log-streaming namespace, as
        /// `(option, value)`.
        written: &'static [(&'static str, &'static str)],
        /// Whether streaming is then on.
        streaming: bool,
        /// How long the groups are then kept for.
        retention: i32,
    }

    /// RULE: whether streaming is on, and how long the groups are kept, is what the
    /// configuration states — and otherwise what the option catalog publishes as the
    /// option's default.
    #[test]
    fn the_configuration_decides_whether_streaming_is_on_and_for_how_long() {
        let cases: &[ConfigurationRow] = &[
            ConfigurationRow {
                case: "a configuration setting neither option",
                written: &[],
                streaming: false,
                retention: 7,
            },
            ConfigurationRow {
                case: "streaming turned on, with no retention stated",
                written: &[(OPTION_STREAM_LOGS, "true")],
                streaming: true,
                retention: 7,
            },
            ConfigurationRow {
                case: "streaming turned on with a retention of its own",
                written: &[(OPTION_STREAM_LOGS, "true"), (OPTION_RETENTION_IN_DAYS, "30")],
                streaming: true,
                retention: 30,
            },
            ConfigurationRow {
                case: "the longest published retention",
                written: &[
                    (OPTION_STREAM_LOGS, "true"),
                    (OPTION_RETENTION_IN_DAYS, "3653"),
                ],
                streaming: true,
                retention: 3653,
            },
            ConfigurationRow {
                case: "streaming turned off explicitly",
                written: &[(OPTION_STREAM_LOGS, "false"), (OPTION_RETENTION_IN_DAYS, "90")],
                streaming: false,
                retention: 90,
            },
            ConfigurationRow {
                case: "a retention outside the option's published set",
                written: &[(OPTION_STREAM_LOGS, "true"), (OPTION_RETENTION_IN_DAYS, "2")],
                streaming: true,
                retention: 7,
            },
            ConfigurationRow {
                case: "a retention that is no count of days at all",
                written: &[
                    (OPTION_STREAM_LOGS, "true"),
                    (OPTION_RETENTION_IN_DAYS, "soon"),
                ],
                streaming: true,
                retention: 7,
            },
        ];
        for row in cases {
            let case = row.case;
            let settings: Vec<ConfigurationOptionSetting> = row
                .written
                .iter()
                .map(|(option_name, value)| ConfigurationOptionSetting {
                    namespace: Some(NAMESPACE_CLOUDWATCH_LOGS.to_string()),
                    option_name: Some((*option_name).to_string()),
                    value: Some((*value).to_string()),
                    ..ConfigurationOptionSetting::default()
                })
                .collect();
            let actual_on = option_is_on(&settings, OPTION_STREAM_LOGS);
            assert_eq!(
                actual_on, row.streaming,
                "case {case:?}: expected streaming on = {}, got {actual_on}",
                row.streaming
            );
            let actual_retention = retention_in_days(&settings);
            assert_eq!(
                actual_retention, row.retention,
                "case {case:?}: expected {} days, got {actual_retention}",
                row.retention
            );
        }
    }

    /// RULE: both options this module reads are ones the service PUBLISHES, so a client can
    /// read the same (namespace, option) out of `DescribeConfigurationOptions` and set it —
    /// and the defaults honoured here are the ones that catalog publishes.
    #[test]
    fn the_streaming_options_are_published_ones() {
        for option_name in [OPTION_STREAM_LOGS, OPTION_RETENTION_IN_DAYS] {
            let published = GENERAL_CONFIGURATION_OPTIONS
                .iter()
                .find(|option| option.key() == (NAMESPACE_CLOUDWATCH_LOGS, option_name));
            let published = published.unwrap_or_else(|| {
                panic!("expected the catalog to publish {NAMESPACE_CLOUDWATCH_LOGS} / {option_name}")
            });
            assert_eq!(
                published_default(option_name),
                published.default_value,
                "case {option_name:?}: expected the published default to be honoured"
            );
        }
    }
}
