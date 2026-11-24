// SPDX-License-Identifier: CC0-1.0

//! Whether a build may deliver its output is decided on the GROUP its project configures.
//!
//! RULE under test: CodeBuild provisions a build's log stream under the log group the build's
//! own configuration names, so the delivery actions have to be granted to the build's service
//! role ON THAT GROUP's streams: a role whose grant names another group's streams withholds
//! the delivery, and the build fails at the phase that provisions it with a context naming the
//! refused delivery, creates no stream under the configured prefix in the configured group and
//! delivers no event into it. The same role granted those actions on the configured group's
//! streams runs the identical project's build to success and creates exactly
//! `<streamName>/<build uuid>` in it.
//!
//! The rows differ along what the rule generalises over: which group the grant is scoped to
//! (the configured one, another one, every one), the form the ARN pattern takes (the
//! `log-group:<name>:*` the IAM Service Authorization Reference publishes for the log-group
//! resource type, and the `log-group:<name>:log-stream:*` spelling of one stream of it), and
//! whether the project configures a group of its own at all.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::build_log_delivery::{
    buildspec, codebuild, delivered_messages, json_call, project_body, role_stating, settled_build,
};
use super::project_build_lifecycle::REGION;

/// CloudWatch Logs' `X-Amz-Target` prefix, from that service's own OPERATIONS.md.
const LOGS_TARGET: &str = "Logs_20140328";

/// The group the projects below configure, and which the grants are scoped to or away from.
const CONFIGURED_GROUP: &str = "delivered-into-this-group";

/// A group no project configures and nothing creates: a grant scoped to it decides nothing
/// about a delivery into [`CONFIGURED_GROUP`].
const DECOY_GROUP: &str = "never-delivered-into";

/// The delivery actions CodeBuild performs a build's log delivery under ("Actions defined by
/// Amazon CloudWatch Logs", IAM Service Authorization Reference).
const DELIVERY_ACTIONS: [&str; 2] = ["logs:CreateLogStream", "logs:PutLogEvents"];

/// The line every build below writes, which is what its stream must hold afterwards.
const MARKER: &str = "the delivered marker";

/// One emulator carrying IAM, CloudWatch Logs and CodeBuild over ONE shared engine.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_codebuild_codebuild_20161006::EmulatedService::new(engine),
    );
    router
}

/// The ARN of the streams of the group `group`, as a policy scopes a grant to them.
fn streams_of(group: &str, suffix: &str) -> String {
    format!(
        "arn:aws:logs:{REGION}:{}:log-group:{group}:{suffix}",
        engine::EMULATOR_ACCOUNT_ID
    )
}

/// The names of the streams of `group` that begin with `prefix`, as CloudWatch Logs' own
/// listing reports them — empty for a group this account holds no such stream in, and for a
/// group it holds at all.
async fn streams_named(router: &MultiServiceRouter, group: &str, prefix: &str) -> Vec<String> {
    let (status, listed) = json_call(
        router,
        "logs",
        LOGS_TARGET,
        "DescribeLogStreams",
        &json!({"logGroupName": group, "logStreamNamePrefix": prefix}),
    )
    .await;
    if status != 200 {
        return Vec::new();
    }
    listed["logStreams"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|stream| stream["logStreamName"].as_str().map(str::to_string))
        .collect()
}

/// RULE: the delivery actions are authorized on the streams of the group the build's own
/// configuration names.
#[tokio::test]
async fn a_grant_scoped_to_another_group_withholds_the_delivery() {
    let router = emulator();
    // The group every project below configures really exists, so a refused delivery is the
    // role's doing and not the group's absence.
    let (status, created) = json_call(
        &router,
        "logs",
        LOGS_TARGET,
        "CreateLogGroup",
        &json!({"logGroupName": CONFIGURED_GROUP}),
    )
    .await;
    assert_eq!(
        status, 200,
        "CreateLogGroup {CONFIGURED_GROUP:?}: expected the group to be created, got {status}: \
         {created}"
    );

    /// One row: the case, the resource the role's grant is scoped to, the group the project
    /// configures (`None` for a project configuring none), and whether the build must deliver.
    type Row = (&'static str, String, Option<&'static str>, bool);

    let rows: Vec<Row> = vec![
        (
            "a grant scoped to the streams of the configured group",
            streams_of(CONFIGURED_GROUP, "*"),
            Some(CONFIGURED_GROUP),
            true,
        ),
        (
            "a grant scoped to the streams of another group",
            streams_of(DECOY_GROUP, "*"),
            Some(CONFIGURED_GROUP),
            false,
        ),
        (
            "a grant naming one stream of the configured group",
            streams_of(CONFIGURED_GROUP, "log-stream:*"),
            Some(CONFIGURED_GROUP),
            true,
        ),
        (
            "a grant naming one stream of another group",
            streams_of(DECOY_GROUP, "log-stream:*"),
            Some(CONFIGURED_GROUP),
            false,
        ),
        (
            // The project's default group, which the configuration does not name: a grant on
            // another group's streams withholds the delivery into it just the same.
            "a project configuring no group, with a grant scoped to another group",
            streams_of(DECOY_GROUP, "*"),
            None,
            false,
        ),
        (
            "a grant covering every group's streams",
            streams_of("*", "*"),
            Some(CONFIGURED_GROUP),
            true,
        ),
    ];

    for (index, (case, resource, group, delivers)) in rows.iter().enumerate() {
        let role = role_stating(
            &router,
            &format!("codebuild-scoped-delivery-{index}"),
            json!([{"Effect": "Allow", "Action": DELIVERY_ACTIONS, "Resource": resource}]),
        )
        .await;
        let project = format!("scoped-delivery-proj-{index}");
        // A prefix of this row's own, so what the group holds afterwards is this build's
        // doing and no other row's.
        let prefix = format!("prefix-{index}");
        let mut cloud_watch = json!({"status": "ENABLED", "streamName": prefix});
        if let (Some(group), Value::Object(members)) = (group, &mut cloud_watch) {
            members.insert("groupName".to_string(), json!(group));
        }
        let logs_config = json!({"logsConfig": {"cloudWatchLogs": cloud_watch}});
        codebuild(
            &router,
            case,
            "CreateProject",
            &project_body(
                &project,
                &role,
                &buildspec(&[&format!("echo {MARKER}")]),
                logs_config,
            ),
        )
        .await;
        let build = settled_build(&router, case, &project).await;
        let reported_group = build["logs"]["groupName"].as_str().unwrap_or_default();
        let reported_stream = build["logs"]["streamName"].as_str().unwrap_or_default();
        let messages = delivered_messages(&router, reported_group, reported_stream).await;
        if *delivers {
            assert_eq!(
                build["buildStatus"],
                json!("SUCCEEDED"),
                "case {case:?}: expected the build to end SUCCEEDED, got {build}"
            );
            assert!(
                messages
                    .as_ref()
                    .is_some_and(|held| held.iter().any(|line| line == MARKER)),
                "case {case:?}: expected {MARKER:?} among the events of \
                 {reported_group}/{reported_stream}, got {messages:?}"
            );
            continue;
        }
        assert_eq!(
            build["buildStatus"],
            json!("FAILED"),
            "case {case:?}: expected the build to end FAILED, got {build}"
        );
        assert!(
            messages.as_ref().is_none_or(Vec::is_empty),
            "case {case:?}: expected no delivered events in {reported_group}/{reported_stream}, \
             got {messages:?}"
        );
        // Nor was a stream created under this build's prefix in the group it would have
        // delivered into: nothing of the delivery happened.
        let held = streams_named(&router, reported_group, &prefix).await;
        assert!(
            held.is_empty(),
            "case {case:?}: expected no stream under {prefix:?} in {reported_group}, got {held:?}"
        );
        // The phase that provisions the build is the one that failed, carrying the refused
        // delivery, and no command phase ran at all.
        let phases = build["phases"].as_array().cloned().unwrap_or_default();
        let provisioning = phases
            .iter()
            .find(|phase| phase["phaseType"] == json!("PROVISIONING"))
            .unwrap_or_else(|| {
                panic!("case {case:?}: the build must report its provisioning phase, got {build}")
            });
        assert_eq!(
            provisioning["phaseStatus"],
            json!("FAILED"),
            "case {case:?}: expected the provisioning phase to have failed, got {provisioning}"
        );
        assert!(
            !phases
                .iter()
                .any(|phase| phase["phaseType"] == json!("BUILD")),
            "case {case:?}: a build with no log stream runs no command phase, got {phases:?}"
        );
        let message = provisioning["contexts"][0]["message"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        for named in [
            format!(
                "Service role {role} does not allow AWS CodeBuild to create Amazon CloudWatch \
                 Logs log streams for build "
            ),
            format!("is not authorized to perform: {} on resource: ", DELIVERY_ACTIONS[0]),
            format!(":log-stream:{reported_stream}"),
        ] {
            assert!(
                message.contains(&named),
                "case {case:?}: expected {named:?} in the phase context, got {message:?}"
            );
        }
    }
}

/// RULE: the stream a build creates under its configured prefix is that prefix followed by the
/// build's own uuid, and it holds the build's output — read back through CloudWatch Logs' own
/// listing of the group's streams rather than out of the build record.
#[tokio::test]
async fn the_stream_the_delivery_created_is_the_prefix_and_the_builds_uuid() {
    let router = emulator();
    let group = "stream-named-after-the-build";
    let (status, created) = json_call(
        &router,
        "logs",
        LOGS_TARGET,
        "CreateLogGroup",
        &json!({"logGroupName": group}),
    )
    .await;
    assert_eq!(
        status, 200,
        "CreateLogGroup {group:?}: expected the group to be created, got {status}: {created}"
    );
    let case = "a build delivering under its configured prefix";
    let role = role_stating(
        &router,
        "codebuild-prefix-delivery",
        json!([{"Effect": "Allow", "Action": DELIVERY_ACTIONS, "Resource": streams_of(group, "*")}]),
    )
    .await;
    codebuild(
        &router,
        case,
        "CreateProject",
        &project_body(
            "prefix-delivery-proj",
            &role,
            &buildspec(&[&format!("echo {MARKER}")]),
            json!({"logsConfig": {"cloudWatchLogs": {
                "status": "ENABLED",
                "groupName": group,
                "streamName": "under-this-prefix",
            }}}),
        ),
    )
    .await;
    let build = settled_build(&router, case, "prefix-delivery-proj").await;
    assert_eq!(
        build["buildStatus"],
        json!("SUCCEEDED"),
        "case {case:?}: expected the build to end SUCCEEDED, got {build}"
    );
    // The uuid half of the build's own id is what names the stream.
    let id = build["id"].as_str().unwrap_or_default().to_string();
    let uuid = id.rsplit(':').next().unwrap_or_default().to_string();
    let expected = format!("under-this-prefix/{uuid}");
    let names = streams_named(&router, group, "under-this-prefix").await;
    assert_eq!(
        names,
        vec![expected.clone()],
        "case {case:?}: expected exactly the stream {expected:?} in {group}, got {names:?}"
    );
    let messages = delivered_messages(&router, group, &expected)
        .await
        .unwrap_or_else(|| panic!("case {case:?}: CloudWatch Logs must hold {group}/{expected}"));
    assert!(
        messages.iter().any(|line| line == MARKER),
        "case {case:?}: expected {MARKER:?} among the events of {group}/{expected}, \
         got {messages:?}"
    );
}
