// SPDX-License-Identifier: CC0-1.0

//! An A/B test is built on configuration ANOTHER AgentCore plane minted.
//!
//! RULE: the identifiers an A/B test is created with come from the calls that mint them —
//! the AgentCore control plane mints the online evaluation configuration an
//! `evaluationConfig` names and the gateway a `gatewayArn` names — and with those in hand
//! the whole modelled A/B-test lifecycle on the AgentCore data plane runs end to end:
//! `CreateABTest` is accepted 202 with the name it was asked for and the `executionStatus`
//! `enableOnCreate` chose, the test settles from `CREATING` to `ACTIVE` on the clock with
//! that `executionStatus` untouched, `UpdateABTest`'s change reads back through
//! `GetABTest`, and after `DeleteABTest` every read of that `abTestId` is refused 404
//! `ResourceNotFoundException`.
//!
//! The two planes are separate services over one shared engine and only the CONTROL plane
//! can mint either prerequisite, so this suite lives here rather than in either crate: a
//! single-service router has no plane to mint them. It is the client's own order of calls —
//! mint the evaluation configuration, then create the test on the ARN it answered — so a
//! prerequisite that is not served makes the whole lifecycle unreachable rather than
//! visibly wrong. The emulator and the request sender are the sibling cross-plane suite's
//! (`bedrock_agentcore_data_plane_parent_resources`), which exports them for exactly this.
//!
//! The rows span what the rule generalises over: how `evaluationConfig` names its
//! configurations (one shared ARN, two distinct per-variant ARNs, one ARN used for both
//! variants), whether `enableOnCreate` asks for traffic splitting, the traffic split and
//! the order the variants are listed in, and whether the create carried a description, a
//! `gatewayFilter` and tags.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The ONE cross-plane AgentCore harness this test target keeps: the sibling suite mounts both
// planes over one shared engine and sends one restJson1 call under the signing name they
// share, and exports them (`pub(crate)`) so no suite writes a second copy and the suites
// cannot drift in how they mount the two planes.
use crate::bedrock_agentcore_data_plane_parent_resources::{
    call, emulator, error_code, message, REGION,
};

/// The role the evaluation configuration and the A/B test run under, in the form `RoleArn`
/// publishes as its pattern (source: smithy_query query="operation" operation="CreateABTest").
const ROLE_ARN: &str = "arn:aws:iam::123456789012:role/AgentCoreABTest";

/// The CloudWatch log group an online evaluation configuration samples traces from, in the
/// form `LogGroupName` publishes as its pattern.
const LOG_GROUP: &str = "/aws/bedrock-agentcore/probe";

/// `ABTestStatus` members this suite waits for and reports (source: smithy_query
/// query="shape" shape="ABTestStatus", values CREATING | ACTIVE | CREATE_FAILED | UPDATING
/// | UPDATE_FAILED | DELETING | DELETE_FAILED | FAILED).
const AB_TEST_CREATING: &str = "CREATING";
const AB_TEST_ACTIVE: &str = "ACTIVE";
const AB_TEST_UPDATING: &str = "UPDATING";
const AB_TEST_DELETING: &str = "DELETING";

/// `OnlineEvaluationExecutionStatus` for a configuration that is not evaluating (source:
/// smithy_query query="shape" shape="OnlineEvaluationExecutionStatus", values ENABLED |
/// DISABLED).
const CONFIG_DISABLED: &str = "DISABLED";

/// How long a wait for a transition the clock owes may take before this suite calls it a
/// failure. GENEROUS on purpose: what is waited for is a condition, never an elapsed time,
/// so a loaded host only makes the wait longer and never changes the verdict.
const SETTLE_DEADLINE: Duration = Duration::from_secs(60);

/// How often a wait looks again.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// One member of a JSON object, or `Value::Null` when it is absent.
fn member<'a>(body: &'a Value, name: &str) -> &'a Value {
    body.get(name).unwrap_or(&Value::Null)
}

/// One member of a JSON object as a string, or `""`.
fn text(body: &Value, name: &str) -> String {
    member(body, name).as_str().unwrap_or_default().to_string()
}

/// `CreateOnlineEvaluationConfig` on the CONTROL plane:
/// `POST /online-evaluation-configs/create`. The body is the one a client building an A/B
/// test sends: sample every session, read traces from one log group, score them with a
/// built-in evaluator, and do not start evaluating on creation.
fn create_config_body(name: &str) -> Value {
    json!({
        "onlineEvaluationConfigName": name,
        "rule": {"samplingConfig": {"samplingPercentage": 100.0}},
        "dataSourceConfig": {"cloudWatchLogs": {
            "serviceNames": ["probe-service"],
            "logGroupNames": [LOG_GROUP],
        }},
        "evaluators": [{"evaluatorId": "Builtin.Helpfulness"}],
        "evaluationExecutionRoleArn": ROLE_ARN,
        "enableOnCreate": false,
    })
}

/// Mint one online evaluation configuration and answer `(onlineEvaluationConfigId,
/// onlineEvaluationConfigArn)`, having checked every member
/// `CreateOnlineEvaluationConfigResponse` declares required.
async fn mint_config(router: &MultiServiceRouter, case: &str, name: &str) -> (String, String) {
    let (status, created) = call(
        router,
        "POST",
        "/online-evaluation-configs/create",
        Some(create_config_body(name)),
    )
    .await;
    assert_eq!(
        status, 202,
        "case {case:?}: expected 202 from CreateOnlineEvaluationConfig, got {status} with \
         {created:?}"
    );
    let id = text(&created, "onlineEvaluationConfigId");
    let arn = text(&created, "onlineEvaluationConfigArn");
    for required in ["onlineEvaluationConfigId", "onlineEvaluationConfigArn", "status"] {
        assert!(
            !text(&created, required).is_empty(),
            "case {case:?}: CreateOnlineEvaluationConfig must report {required}, got {created:?}"
        );
    }
    assert!(
        member(&created, "createdAt").is_number(),
        "case {case:?}: CreateOnlineEvaluationConfig must report createdAt, got {created:?}"
    );
    assert_eq!(
        text(&created, "executionStatus"),
        CONFIG_DISABLED,
        "case {case:?}: a configuration created with enableOnCreate false must not be \
         evaluating, got {created:?}"
    );
    assert_eq!(
        arn,
        format!(
            "arn:aws:bedrock-agentcore:{REGION}:{}:online-evaluation-config/{id}",
            engine::emulator_account_id()
        ),
        "case {case:?}: expected the ARN of the minted configuration, got {arn:?}"
    );
    (id, arn)
}

/// `CreateGateway` on the CONTROL plane: `POST /gateways/`, answering the `gatewayArn` it
/// minted — the gateway whose traffic an A/B test splits.
async fn mint_gateway(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let (status, created) = call(
        router,
        "POST",
        "/gateways/",
        Some(json!({
            "name": name,
            "roleArn": ROLE_ARN,
            "protocolType": "MCP",
            "authorizerType": "AWS_IAM",
        })),
    )
    .await;
    assert_eq!(
        status, 202,
        "case {case:?}: expected 202 from CreateGateway, got {status} with {created:?}"
    );
    let gateway_arn = text(&created, "gatewayArn");
    let expected_prefix = format!(
        "arn:aws:bedrock-agentcore:{REGION}:{}:gateway/",
        engine::emulator_account_id()
    );
    assert!(
        gateway_arn.starts_with(&expected_prefix),
        "case {case:?}: expected a minted GatewayArn beginning {expected_prefix:?}, got \
         {gateway_arn:?}"
    );
    gateway_arn
}

/// `GetABTest` on the DATA plane: `GET /ab-tests/{abTestId}`. `ABTestId` admits only
/// letters, digits, `-` and `_`, so the `@httpLabel` needs no escaping.
async fn get_ab_test(router: &MultiServiceRouter, ab_test_id: &str) -> (u16, Value) {
    call(router, "GET", &format!("/ab-tests/{ab_test_id}"), None).await
}

/// Read the A/B test again until it reports `ACTIVE`, and answer that body.
///
/// The wait is for the CONDITION the clock makes true, with a generous deadline; nothing
/// here sleeps for a span and then asserts, and nothing asserts on elapsed time.
async fn settled(router: &MultiServiceRouter, case: &str, ab_test_id: &str) -> Value {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, read) = get_ab_test(router, ab_test_id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 from GetABTest, got {status} with {read:?}"
        );
        if text(&read, "status") == AB_TEST_ACTIVE {
            return read;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: GetABTest never reported {AB_TEST_ACTIVE}, last saw {read:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// One row of the lifecycle table: the A/B test a client asks for, built on the
/// configurations this row mints.
struct Row {
    case: &'static str,
    /// The `ABTestName` the create asks for, unique within the row's account.
    ab_test: &'static str,
    /// The gateway this row mints, named in lowercase so the minted `GatewayArn` matches
    /// the pattern `CreateABTest` holds `gatewayArn` to.
    gateway: &'static str,
    /// The online evaluation configurations this row mints: one, or one per variant.
    configs: &'static [&'static str],
    /// Whether `evaluationConfig` names a configuration PER VARIANT rather than one shared
    /// one — the two members of the `ABTestEvaluationConfig` union.
    per_variant: bool,
    /// `enableOnCreate` as the request carries it: absent, `false` or `true`.
    enable_on_create: Option<bool>,
    /// The control and the treatment weight.
    weights: (i64, i64),
    /// Whether the treatment variant is listed first, so the stored order is exercised.
    treatment_first: bool,
    description: Option<&'static str>,
    /// The description `UpdateABTest` replaces it with.
    updated_description: &'static str,
    /// The one target path a `gatewayFilter` restricts the test to, or none.
    target_path: Option<&'static str>,
    tagged: bool,
    /// The `ABTestExecutionStatus` the create and every later read must report.
    expected_execution_status: &'static str,
}

/// The lifecycle table.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "one shared evaluation config, not enabled on create",
            ab_test: "checkout_flow",
            gateway: "abtestgateway",
            configs: &["checkout_quality"],
            per_variant: false,
            enable_on_create: Some(false),
            weights: (50, 50),
            treatment_first: false,
            description: None,
            updated_description: "updated description",
            target_path: None,
            tagged: false,
            expected_execution_status: "NOT_STARTED",
        },
        Row {
            case: "one evaluation config per variant, enabled on create",
            ab_test: "search_ranking",
            gateway: "rankinggateway",
            configs: &["ranking_quality", "ranking_latency"],
            per_variant: true,
            enable_on_create: Some(true),
            weights: (90, 10),
            treatment_first: true,
            description: Some("compare the reranked prompt against the current one"),
            updated_description: "compare the reranked prompt against the previous one",
            target_path: Some("/search/*"),
            tagged: true,
            expected_execution_status: "RUNNING",
        },
        Row {
            case: "both variants on one minted config, enableOnCreate absent",
            ab_test: "summary_tone",
            gateway: "tonegateway",
            configs: &["tone_quality"],
            per_variant: true,
            enable_on_create: None,
            weights: (1, 99),
            treatment_first: false,
            description: Some("tone experiment"),
            updated_description: "tone experiment, second round",
            target_path: None,
            tagged: true,
            expected_execution_status: "NOT_STARTED",
        },
    ]
}

impl Row {
    /// This row's `evaluationConfig`, over the ARNs the control plane minted for it. A row
    /// that asks for per-variant configurations but minted ONE names that one for both
    /// variants, which is what a client with a single quality bar sends.
    fn evaluation_config(&self, arns: &[String]) -> Value {
        let control = &arns[0];
        if !self.per_variant {
            return json!({"onlineEvaluationConfigArn": control});
        }
        let treatment = arns.get(1).unwrap_or(control);
        json!({"perVariantOnlineEvaluationConfig": [
            {"name": "C", "onlineEvaluationConfigArn": control},
            {"name": "T1", "onlineEvaluationConfigArn": treatment},
        ]})
    }

    /// This row's `variants`: one control and one treatment, each routed at a gateway
    /// target, in the order the row lists them.
    fn variants(&self) -> Value {
        let control = json!({
            "name": "C",
            "weight": self.weights.0,
            "variantConfiguration": {"target": {"name": "control-target"}},
        });
        let treatment = json!({
            "name": "T1",
            "weight": self.weights.1,
            "variantConfiguration": {"target": {"name": "treatment-target"}},
        });
        if self.treatment_first {
            json!([treatment, control])
        } else {
            json!([control, treatment])
        }
    }

    /// This row's `CreateABTest` body, over the identifiers the control plane minted.
    fn create_body(&self, gateway_arn: &str, arns: &[String]) -> Value {
        let mut body = json!({
            "name": self.ab_test,
            "gatewayArn": gateway_arn,
            "roleArn": ROLE_ARN,
            "variants": self.variants(),
            "evaluationConfig": self.evaluation_config(arns),
        });
        let object = body.as_object_mut().expect("a JSON object");
        if let Some(description) = self.description {
            object.insert("description".to_string(), json!(description));
        }
        if let Some(enabled) = self.enable_on_create {
            object.insert("enableOnCreate".to_string(), json!(enabled));
        }
        if let Some(path) = self.target_path {
            object.insert("gatewayFilter".to_string(), json!({"targetPaths": [path]}));
        }
        if self.tagged {
            object.insert("tags".to_string(), json!({"owner": "probe"}));
        }
        body
    }
}

/// The whole A/B-test lifecycle, on prerequisites the control plane minted: the create is
/// accepted, the test settles to `ACTIVE` without starting traffic splitting, an update
/// reads back, and the delete makes the test gone.
#[tokio::test]
async fn the_ab_test_lifecycle_runs_on_what_the_control_plane_minted() {
    let router = emulator();

    for row in rows() {
        let case = row.case;

        // The prerequisite a client mints FIRST, and the control plane's own read is what
        // says the configuration it reported really exists.
        let mut arns = Vec::new();
        for name in row.configs {
            let (config_id, config_arn) = mint_config(&router, case, name).await;
            let (status, read) = call(
                &router,
                "GET",
                &format!("/online-evaluation-configs/{config_id}"),
                None,
            )
            .await;
            assert_eq!(
                (status, text(&read, "onlineEvaluationConfigArn")),
                (200, config_arn.clone()),
                "case {case:?}: the minted configuration must read back from the control \
                 plane, got {status} with {read:?}"
            );
            arns.push(config_arn);
        }
        let gateway_arn = mint_gateway(&router, case, row.gateway).await;

        // CreateABTest, on nothing hand-written: every identifier it names was minted above.
        let body = row.create_body(&gateway_arn, &arns);
        let (status, created) = call(&router, "POST", "/ab-tests", Some(body.clone())).await;
        assert_eq!(
            status, 202,
            "case {case:?}: expected 202 from CreateABTest, got {status} with {created:?}"
        );
        let ab_test_id = text(&created, "abTestId");
        let ab_test_arn = text(&created, "abTestArn");
        assert_eq!(
            text(&created, "name"),
            row.ab_test,
            "case {case:?}: CreateABTest must echo the name it created, got {created:?}"
        );
        assert_eq!(
            text(&created, "status"),
            AB_TEST_CREATING,
            "case {case:?}: an accepted create reports the transient status, got {created:?}"
        );
        assert_eq!(
            text(&created, "executionStatus"),
            row.expected_execution_status,
            "case {case:?}: expected executionStatus {:?}, got {created:?}",
            row.expected_execution_status
        );
        assert!(
            member(&created, "createdAt").is_number(),
            "case {case:?}: CreateABTest must report createdAt, got {created:?}"
        );
        assert_eq!(
            ab_test_arn,
            format!(
                "arn:aws:bedrock-agentcore:{REGION}:{}:ab-test/{ab_test_id}",
                engine::emulator_account_id()
            ),
            "case {case:?}: expected the ARN of the created test, got {ab_test_arn:?}"
        );

        // The test settles to ACTIVE on the clock, and settling starts no traffic splitting:
        // what the create persisted is what the read reports.
        let read = settled(&router, case, &ab_test_id).await;
        assert_eq!(
            text(&read, "executionStatus"),
            row.expected_execution_status,
            "case {case:?}: the lifecycle settling must not move executionStatus, got {read:?}"
        );
        // Only the members the request carried are compared here: a member it did not send has
        // its own assertion below, so no comparison here is a default against a default.
        for reported in [
            "gatewayArn",
            "roleArn",
            "variants",
            "evaluationConfig",
            "gatewayFilter",
            "description",
        ]
        .into_iter()
        .filter(|name| !member(&body, name).is_null())
        {
            assert_eq!(
                member(&read, reported),
                member(&body, reported),
                "case {case:?}: expected {reported} to read back as it was created, got {:?}",
                member(&read, reported)
            );
        }
        assert_eq!(
            text(&read, "abTestId"),
            ab_test_id,
            "case {case:?}: GetABTest must report the created abTestId, got {read:?}"
        );
        // A create that carried neither member reports neither: an absent optional member is
        // not a value this service invents.
        if row.description.is_none() {
            assert!(
                member(&read, "description").is_null(),
                "case {case:?}: a create that carried no description must report none, got \
                 {read:?}"
            );
        }
        if row.target_path.is_none() {
            assert!(
                member(&read, "gatewayFilter").is_null(),
                "case {case:?}: a create that carried no gatewayFilter must report none, got \
                 {read:?}"
            );
        }

        // UpdateABTest's change reads back, and the test returns to ACTIVE.
        let (status, updated) = call(
            &router,
            "PUT",
            &format!("/ab-tests/{ab_test_id}"),
            Some(json!({"description": row.updated_description})),
        )
        .await;
        assert_eq!(
            status, 202,
            "case {case:?}: expected 202 from UpdateABTest, got {status} with {updated:?}"
        );
        assert_eq!(
            (
                text(&updated, "abTestId"),
                text(&updated, "abTestArn"),
                text(&updated, "status"),
                text(&updated, "executionStatus"),
            ),
            (
                ab_test_id.clone(),
                ab_test_arn.clone(),
                AB_TEST_UPDATING.to_string(),
                row.expected_execution_status.to_string(),
            ),
            "case {case:?}: expected the update to report the test it changed in \
             {AB_TEST_UPDATING}, got {updated:?}"
        );
        let after = settled(&router, case, &ab_test_id).await;
        assert_eq!(
            text(&after, "description"),
            row.updated_description,
            "case {case:?}: expected the updated description to read back, got {after:?}"
        );
        assert_eq!(
            text(&after, "executionStatus"),
            row.expected_execution_status,
            "case {case:?}: an update that carried no executionStatus must not move it, got \
             {after:?}"
        );
        assert_eq!(
            member(&after, "variants"),
            member(&body, "variants"),
            "case {case:?}: an update that carried no variants must leave the split alone, \
             got {after:?}"
        );

        // The delete removes the test, so every later read of that abTestId is refused.
        let (status, deleted) = call(
            &router,
            "DELETE",
            &format!("/ab-tests/{ab_test_id}"),
            None,
        )
        .await;
        assert_eq!(
            status, 202,
            "case {case:?}: expected 202 from DeleteABTest, got {status} with {deleted:?}"
        );
        assert_eq!(
            (
                text(&deleted, "abTestId"),
                text(&deleted, "abTestArn"),
                text(&deleted, "status"),
            ),
            (
                ab_test_id.clone(),
                ab_test_arn.clone(),
                AB_TEST_DELETING.to_string(),
            ),
            "case {case:?}: expected the delete to report the test it removed, got {deleted:?}"
        );

        let (status, refused) = get_ab_test(&router, &ab_test_id).await;
        assert_eq!(
            (status, error_code(&refused), message(&refused)),
            (
                404,
                "ResourceNotFoundException".to_string(),
                format!("AB test not found: abTestId={ab_test_id}"),
            ),
            "case {case:?}: expected the deleted test to be refused 404 by id, got {status} \
             with {refused:?}"
        );

        // Deleting the test takes none of its prerequisites with it: the configurations it
        // was built on are the control plane's resources, not the test's.
        for arn in &arns {
            let config_id = arn
                .rsplit('/')
                .next()
                .expect("the minted ARN ends in its resource id");
            let (status, read) = call(
                &router,
                "GET",
                &format!("/online-evaluation-configs/{config_id}"),
                None,
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: the configuration the deleted test named must still exist, got \
                 {status} with {read:?}"
            );
        }
    }
}
