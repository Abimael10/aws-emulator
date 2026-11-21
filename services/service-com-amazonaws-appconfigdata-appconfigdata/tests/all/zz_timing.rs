// SPDX-License-Identifier: CC0-1.0

//! The TIMING of the rollout gate, read from the data plane: which version a session is
//! served as the CLOCK carries a rollout through its growth intervals.
//!
//! RULE: how far a deployment's rollout has got is owed by the clock, not by anybody
//! calling — a strategy that declares a rollout window reaches its targets one growth
//! interval at a time — and the configuration in force for a session is the one the
//! rollout has reached that session with (`PercentageComplete` is "the percentage of
//! targets for which the deployment is available"). So a session opened before the first
//! interval has elapsed is served the version that was in force BEFORE the rollout
//! started, and a session opened once the whole rollout window has passed is served the
//! new version — with nothing about the deployment written by hand.
//!
//! The companion suite `rollout_percentage_splits_sessions` asserts the gate itself over
//! a table of percentages; this one asserts that the clock is what moves a deployment
//! through them, which is why it waits rather than writing a percentage into the store.
//!
//! The second test here is the other transition the clock owes the data plane: the refusal
//! a withdrawn triple owes a session, which a client converges on by looking again rather
//! than by following the token rotation.

use std::time::{Duration, Instant};

use bytes::Bytes;

use super::configuration_session_poll::{Emulator, Hierarchy};

/// The two versions deployed, told apart by their bytes.
const PREVIOUS: &[u8] = br#"{"tier":"one"}"#;
/// See [`PREVIOUS`].
const NEW: &[u8] = br#"{"tier":"two"}"#;

/// RULE: the rollout gate moves with the clock — before the rollout's first growth
/// interval a session is served the previous version, and once the whole rollout window
/// has passed it is served the new one.
#[tokio::test]
async fn the_version_a_session_is_served_follows_the_clock_through_the_rollout() {
    let emulator = Emulator::new();
    let hierarchy = emulator
        .create_hierarchy("paced-app", "production", "flags")
        .await;

    // The previous version goes out with a strategy that has nothing to wait for, and is
    // left to settle, so it is in force for every session when the rollout starts.
    emulator
        .deploy(
            &hierarchy,
            &hierarchy.profile_id,
            PREVIOUS,
            "application/json",
            None,
        )
        .await;
    emulator.settle_deployments(&hierarchy);

    // A strategy that declares a rollout window and grows in TWO intervals, so the
    // rollout has reached nobody when the deployment starts and every target once its
    // window has passed.
    let version = host_version(&emulator, &hierarchy, NEW).await;
    let created = emulator
        .post_json(
            "/deploymentstrategies",
            r#"{"Name":"two-intervals","DeploymentDurationInMinutes":1,"GrowthFactor":50.0,
                "GrowthType":"LINEAR","FinalBakeTimeInMinutes":1440,"ReplicateTo":"NONE"}"#,
        )
        .await;
    assert_eq!(
        created.status,
        201,
        "CreateDeploymentStrategy: expected 201, got {} body {}",
        created.status,
        created.text()
    );
    let strategy = created.string("Id");
    let application_id = &hierarchy.application_id;
    let environment_id = &hierarchy.environment_id;
    let profile_id = &hierarchy.profile_id;
    let started = emulator
        .post_json(
            &format!("/applications/{application_id}/environments/{environment_id}/deployments"),
            &format!(
                r#"{{"DeploymentStrategyId":"{strategy}",
                      "ConfigurationProfileId":"{profile_id}",
                      "ConfigurationVersion":"{version}"}}"#
            ),
        )
        .await;
    assert_eq!(
        started.status,
        201,
        "StartDeployment: expected 201, got {} body {}",
        started.status,
        started.text()
    );

    // Before the first growth interval has elapsed the rollout has reached nobody.
    let served = poll_one_session(&emulator, &hierarchy).await;
    assert_eq!(
        served.as_slice(),
        PREVIOUS,
        "case a rollout whose first interval has not elapsed: expected the previous \
         version, got {:?}",
        String::from_utf8_lossy(&served)
    );

    // Once the whole rollout window has passed, every target has been reached. The wait
    // is the rollout's own span — two growth intervals of
    // `service_com_amazonaws_appconfig_amazonappconfig::state::STAGE_DURATION` — plus a
    // margin, and nothing is written into either store.
    let rollout = service_com_amazonaws_appconfig_amazonappconfig::state::STAGE_DURATION * 2;
    tokio::time::sleep(rollout + Duration::from_millis(500)).await;
    let served = poll_one_session(&emulator, &hierarchy).await;
    assert_eq!(
        served.as_slice(),
        NEW,
        "case a rollout whose window has passed: expected the new version, got {:?}",
        String::from_utf8_lossy(&served)
    );
}

/// How long a client's poll loop is given to converge on the refusal a withdrawn triple
/// owes: far more than the span the service records the deadline one of
/// (`state::WITHDRAWAL_SPAN`), so the test waits for the CONDITION on a host of any speed
/// and fails only on a chain that never converges at all.
const CONVERGENCE_DEADLINE: Duration = Duration::from_secs(120);

/// How long the poll loop below pauses between looks. A client polls at the cadence the
/// response tells it to; this one only has to look often enough to notice the refusal
/// shortly after it falls due.
const LOOK_AGAIN_AFTER: Duration = Duration::from_millis(250);

/// RULE: the refusal a withdrawn triple owes a session falls due on the CLOCK, and it is
/// owed by the whole poll chain — so a client that keeps re-presenting the token it minted
/// before the deletion, without ever following the rotation, converges on the modelled
/// `ResourceNotFoundException` naming the resource that is gone. Nothing is written into
/// either store here: the deletion goes through AppConfig's own operation and the deadline
/// is reached by waiting for it.
#[tokio::test]
async fn a_token_minted_before_a_deletion_converges_on_the_refusal() {
    let emulator = Emulator::new();
    let hierarchy = emulator
        .create_hierarchy("withdrawn-app", "production", "flags")
        .await;
    emulator
        .deploy(
            &hierarchy,
            &hierarchy.profile_id,
            PREVIOUS,
            "application/json",
            None,
        )
        .await;

    // The token the client holds BEFORE the deletion, and the configuration it is serving.
    let token = emulator
        .session_token(
            &hierarchy.application_id,
            &hierarchy.environment_id,
            &hierarchy.profile_id,
        )
        .await;
    let served = emulator.get_latest_configuration(&token).await;
    assert_eq!(
        served.status,
        200,
        "the poll before the deletion expected 200, got {} body {}",
        served.status,
        served.text()
    );
    assert_eq!(
        served.body.as_ref(),
        PREVIOUS,
        "the poll before the deletion must serve the deployed bytes, got {:?}",
        String::from_utf8_lossy(&served.body)
    );

    let application_id = &hierarchy.application_id;
    let environment_id = &hierarchy.environment_id;
    let deleted = emulator
        .send(
            "DELETE",
            &format!("/applications/{application_id}/environments/{environment_id}"),
            "application/json",
            &[],
            Bytes::new(),
        )
        .await;
    assert_eq!(
        deleted.status,
        204,
        "DeleteEnvironment expected 204, got {} body {}",
        deleted.status,
        deleted.text()
    );

    // The client re-presents the SAME token, looking again until the refusal has fallen
    // due rather than waiting a fixed time for it.
    let give_up_at = Instant::now() + CONVERGENCE_DEADLINE;
    let refusal = loop {
        let poll = emulator.get_latest_configuration(&token).await;
        if poll.status != 200 {
            break poll;
        }
        assert!(
            Instant::now() < give_up_at,
            "a configuration token minted before its environment was deleted must stop \
             serving: the re-presented token was still answered 200 with {:?} for the whole \
             poll window",
            String::from_utf8_lossy(&poll.body)
        );
        tokio::time::sleep(LOOK_AGAIN_AFTER).await;
    };

    assert_eq!(
        refusal.status,
        404,
        "the re-presented token expected 404 once the withdrawal had fallen due, got {} \
         body {}",
        refusal.status,
        refusal.text()
    );
    assert_eq!(
        refusal.error_code(),
        "ResourceNotFoundException",
        "expected ResourceNotFoundException, got {}",
        refusal.text()
    );
    let body = refusal.json();
    assert_eq!(
        body["ResourceType"].as_str(),
        Some("Environment"),
        "expected the ResourceType of the level that is gone, got {}",
        refusal.text()
    );
    assert_eq!(
        body["Message"].as_str(),
        Some("Environment not found"),
        "expected the level's fixed not-found sentence, got {}",
        refusal.text()
    );
    assert_eq!(
        body["ReferencedBy"]["EnvironmentIdentifier"].as_str(),
        Some(hierarchy.environment_id.as_str()),
        "expected ReferencedBy to name the environment the session referenced, got {}",
        refusal.text()
    );
    assert!(
        refusal.header("next-poll-configuration-token").is_none(),
        "a refused poll serves nothing, so it must rotate no token, got {}",
        refusal.text()
    );
}

/// Open one session over the triple, poll it once, and answer the bytes it was served.
async fn poll_one_session(emulator: &Emulator, hierarchy: &Hierarchy) -> Vec<u8> {
    let token = emulator
        .session_token(
            &hierarchy.application_id,
            &hierarchy.environment_id,
            &hierarchy.profile_id,
        )
        .await;
    let reply = emulator.get_latest_configuration(&token).await;
    assert_eq!(
        reply.status,
        200,
        "GetLatestConfiguration must answer 200, got {} body {}",
        reply.status,
        reply.text()
    );
    reply.body.to_vec()
}

/// Write one hosted configuration version, answering the version number AppConfig minted.
async fn host_version(emulator: &Emulator, hierarchy: &Hierarchy, content: &[u8]) -> String {
    let application_id = &hierarchy.application_id;
    let profile_id = &hierarchy.profile_id;
    let written = emulator
        .send(
            "POST",
            &format!(
                "/applications/{application_id}/configurationprofiles/{profile_id}\
                 /hostedconfigurationversions"
            ),
            "application/json",
            &[],
            Bytes::copy_from_slice(content),
        )
        .await;
    assert_eq!(
        written.status,
        201,
        "CreateHostedConfigurationVersion: expected 201, got {} body {}",
        written.status,
        written.text()
    );
    written
        .header("version-number")
        .unwrap_or_else(|| panic!("no Version-Number header; headers {:?}", written.headers))
        .to_string()
}
