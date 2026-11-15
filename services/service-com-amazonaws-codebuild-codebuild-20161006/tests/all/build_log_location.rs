// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for WHERE a build's output went, driven over the wire the router
//! serves: `POST /` with `X-Amz-Target: CodeBuild_20161006.<Operation>` on awsJson1_1.
//!
//! RULE under test: a build carries a `logs` location that says where its output was
//! delivered — the CloudWatch Logs GROUP and the STREAM its own output went to, and the ARN
//! of that stream — so a client can find one build's log events from the build record
//! alone. The group is the one the project's log configuration names, and otherwise the
//! group CodeBuild creates for the project; `CloudWatchLogsConfig$streamName` is "the
//! PREFIX of the stream name", so the stream one build wrote is that prefix followed by the
//! build's own id, and the id alone when no prefix is configured — two builds of one project
//! therefore never report the same stream. A configuration that DISABLES CloudWatch
//! delivery reports no group, stream or stream ARN, and every build still reports the two
//! delivery configurations it runs with.
//!
//! The rows differ along what the rule generalises over: which of the two names the
//! configuration states, whether delivery is enabled at all, and whether the build was
//! started with an override of the project's configuration.

use serde_json::{json, Value};

use super::project_build_lifecycle::{
    create_project, emulator, merged, settled_build, start_build, REGION,
};

/// The log configuration `CreateProject` members carrying `cloudWatchLogs`.
fn logs_config(status: &str, group: Option<&str>, stream: Option<&str>) -> Value {
    let mut cloud_watch = json!({"status": status});
    if let (Value::Object(members), Some(group)) = (&mut cloud_watch, group) {
        members.insert("groupName".to_string(), json!(group));
    }
    if let (Value::Object(members), Some(stream)) = (&mut cloud_watch, stream) {
        members.insert("streamName".to_string(), json!(stream));
    }
    json!({"logsConfig": {"cloudWatchLogs": cloud_watch}})
}

/// The uuid half of a build's id (`<projectName>:<uuid>`), which is what names its stream.
fn build_uuid(build: &Value) -> String {
    let id = build["id"].as_str().unwrap_or_default();
    id.rsplit(':').next().unwrap_or(id).to_string()
}

/// The ARN of the stream `stream` of the group `group`, in the form
/// `LogsLocation$cloudWatchLogsArn` documents.
fn expected_stream_arn(group: &str, stream: &str) -> String {
    format!(
        "arn:aws:logs:{REGION}:{}:log-group:{group}:log-stream:{stream}",
        engine::EMULATOR_ACCOUNT_ID
    )
}

/// RULE: every build reports the group and the stream its own output went to.
#[tokio::test]
async fn a_build_reports_the_log_group_and_stream_its_output_went_to() {
    let router = emulator();
    /// One row: the case, the project's `CreateProject` members, the group the build
    /// reports, and the stream prefix the build's own id is written after — `None` for a
    /// build whose delivery is off, which reports neither.
    type Row = (&'static str, Value, Option<&'static str>, Option<&'static str>);

    let rows: Vec<Row> = vec![
        (
            "a project that configures no logs at all",
            json!({}),
            Some("/aws/codebuild/{project}"),
            Some(""),
        ),
        (
            "a configuration naming neither name",
            logs_config("ENABLED", None, None),
            Some("/aws/codebuild/{project}"),
            Some(""),
        ),
        (
            "a configured group and no stream prefix",
            logs_config("ENABLED", Some("own-group"), None),
            Some("own-group"),
            Some(""),
        ),
        (
            "a configured group and stream prefix",
            logs_config("ENABLED", Some("own-group"), Some("own-stream")),
            Some("own-group"),
            Some("own-stream/"),
        ),
        (
            "a configured stream prefix and the project's own group",
            logs_config("ENABLED", None, Some("own-stream")),
            Some("/aws/codebuild/{project}"),
            Some("own-stream/"),
        ),
        (
            "a configuration that disables CloudWatch delivery",
            logs_config("DISABLED", Some("own-group"), Some("own-stream")),
            None,
            None,
        ),
    ];

    for (index, (case, extra, group, prefix)) in rows.iter().enumerate() {
        let project = format!("logs-proj-{index}");
        create_project(&router, &project, extra.clone()).await;
        let started = start_build(&router, json!({"projectName": project})).await;
        let settled = settled_build(&router, &project, json!({})).await;
        // The location is reported by the start and by the read alike: it is the build's
        // own record rather than something one operation computes.
        for (where_from, build) in [("StartBuild", &started), ("BatchGetBuilds", &settled)] {
            let logs = build["logs"].clone();
            assert!(
                logs.is_object(),
                "case {case:?}: {where_from} must report a logs location, got {build}"
            );
            let expected_group = group.map(|group| group.replace("{project}", &project));
            assert_eq!(
                logs["groupName"].as_str().map(str::to_string),
                expected_group,
                "case {case:?}: {where_from} expected the group {expected_group:?}, got {logs}"
            );
            let expected_stream =
                prefix.map(|prefix| format!("{prefix}{}", build_uuid(build)));
            assert_eq!(
                logs["streamName"].as_str().map(str::to_string),
                expected_stream,
                "case {case:?}: {where_from} expected the stream {expected_stream:?}, got {logs}"
            );
            let expected_arn = expected_group
                .as_deref()
                .zip(expected_stream.as_deref())
                .map(|(group, stream)| expected_stream_arn(group, stream));
            assert_eq!(
                logs["cloudWatchLogsArn"].as_str().map(str::to_string),
                expected_arn,
                "case {case:?}: {where_from} expected the stream ARN {expected_arn:?}, got {logs}"
            );
            // The delivery the build RUNS with is always reported, whichever way it is
            // configured.
            assert!(
                logs["cloudWatchLogs"].is_object() && logs["s3Logs"].is_object(),
                "case {case:?}: {where_from} must report both log configurations, got {logs}"
            );
            assert_eq!(
                logs["s3Logs"]["status"], json!("DISABLED"),
                "case {case:?}: {where_from} a project configuring no S3 logs delivers none, \
                 got {logs}"
            );
        }
    }
}

/// RULE: the stream a build wrote is named after that BUILD, so two builds of one project
/// report different streams in the same group — and a `logsConfigOverride` is what the
/// build it was started for runs with.
#[tokio::test]
async fn two_builds_of_one_project_report_streams_of_their_own() {
    let router = emulator();
    create_project(
        &router,
        "logs-two-proj",
        logs_config("ENABLED", Some("shared-group"), Some("shared-prefix")),
    )
    .await;
    let first = settled_build(&router, "logs-two-proj", json!({})).await;
    let second = settled_build(&router, "logs-two-proj", json!({})).await;
    for (case, build) in [("the first build", &first), ("the second build", &second)] {
        assert_eq!(
            build["logs"]["groupName"], json!("shared-group"),
            "case {case:?}: both builds deliver to the configured group, got {build}"
        );
        assert_eq!(
            build["logs"]["streamName"].as_str().map(str::to_string),
            Some(format!("shared-prefix/{}", build_uuid(build))),
            "case {case:?}: the stream is the prefix and the build's own id, got {build}"
        );
    }
    assert_ne!(
        first["logs"]["streamName"], second["logs"]["streamName"],
        "two builds of one project must not report the same stream, got {} and {}",
        first["logs"], second["logs"]
    );

    // A build started with an override runs with THAT configuration, and reports where its
    // own output went under it.
    let overridden = settled_build(
        &router,
        "logs-two-proj",
        merged(
            json!({}),
            json!({"logsConfigOverride": {
                "cloudWatchLogs": {"status": "ENABLED", "groupName": "override-group"}
            }}),
        ),
    )
    .await;
    assert_eq!(
        overridden["logs"]["groupName"], json!("override-group"),
        "case \"an overridden configuration\": expected the override's group, got {overridden}"
    );
    assert_eq!(
        overridden["logs"]["streamName"].as_str().map(str::to_string),
        Some(build_uuid(&overridden)),
        "case \"an overridden configuration\": the override names no prefix, so the stream is \
         the build's own id, got {overridden}"
    );
}
