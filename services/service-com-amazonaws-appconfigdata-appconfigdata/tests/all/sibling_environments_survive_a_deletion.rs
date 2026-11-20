// SPDX-License-Identifier: CC0-1.0

//! Deleting ONE environment of an application withdraws exactly the sessions that poll
//! THAT environment, driven end to end over HTTP.
//!
//! The rule under test: a session's (application, environment, configuration profile)
//! triple is resolved when the session is STARTED, and a poll re-resolves it to pick up a
//! newer version in force rather than to re-authorise the session — so deleting one
//! environment does not refuse the poll that follows the deletion for a session already
//! bound to a configuration: that poll is answered from the resolution the session carries,
//! and is what makes the refusal DUE, one withdrawal span later. From that instant on every
//! poll of that chain is refused with the modelled `ResourceNotFoundException` (404) naming
//! the level that is gone. The withdrawal is SCOPED to the deleted record, so every sibling
//! record still held keeps serving, including the siblings that share the removed record's
//! parents and the very configuration version it was serving.
//!
//! That the withdrawal must be SCOPED is the half this suite adds to
//! [`super::deletion_does_not_break_a_bound_session`], whose control hierarchy is a
//! separate application: here the two environments are siblings under ONE application,
//! polling ONE configuration profile, served from ONE hosted configuration version
//! deployed into both — so a withdrawal that reached further than the deleted environment
//! (to the application, the profile or the version) would stop the surviving session too,
//! and one that never fell due would keep serving the deleted environment for ever.
//!
//! The rows differ along what the rule generalises over: WHICH of the two sibling
//! environments is deleted (the first created or the second), how the sessions spelled
//! their triple (by name or by id — each identifier is "the ID or the name" of its
//! level), whether the withdrawn session had already been served when the deletion
//! happened, the `DeletionProtectionCheck` the delete request carried, and the form,
//! content type and version label of the configuration both environments were serving.
//!
//! Every request goes through the router harness the polling suite sets up
//! ([`super::configuration_session_poll::Emulator`]), which mounts AppConfigData and the
//! AppConfig control plane over one shared engine: the hierarchy is created, deployed to
//! and deleted through AppConfig's own operations, exactly as an operator would. The one
//! thing no operation exposes is the clock, so the refusal is made to have fallen due by
//! bringing the session's withdrawal deadline forward (`expire_withdrawal_deadline`)
//! instead of sleeping for the span.

use bytes::Bytes;

use super::configuration_session_poll::{Emulator, Hierarchy};

/// The predefined deployment strategy that reaches every target at once
/// (`service_com_amazonaws_appconfig_amazonappconfig::state::PREDEFINED_DEPLOYMENT_STRATEGIES`),
/// so a deployment's rollout has begun the instant `StartDeployment` answers.
const ALL_AT_ONCE: &str = "AppConfig.AllAtOnce";

/// The header `DeleteEnvironment`'s `DeletionProtectionCheck` member travels in:
/// `smithy_query(query="operation",operation="DeleteEnvironment",model_path="services/service-com-amazonaws-appconfig-amazonappconfig/smithy-model/com-amazonaws-appconfig-amazonappconfig.json")`
/// reports `httpHeader: x-amzn-deletion-protection-check`.
const DELETION_PROTECTION_CHECK_HEADER: &str = "x-amzn-deletion-protection-check";

/// The modelled `ResourceType` a deleted environment is reported as, from the model's
/// `ResourceType` enum (`RESOURCE_TYPE_VALUES` in `src/ops/types/shared_types.rs`).
const RESOURCE_TYPE_ENVIRONMENT: &str = "Environment";

/// The `StartConfigurationSession` input member that names an environment, which the
/// modelled `ReferencedBy` map is keyed by for a refusal about one.
const ENVIRONMENT_IDENTIFIER_MEMBER: &str = "EnvironmentIdentifier";

/// One deletion of one of two sibling environments, and the configuration both of them
/// were being served when it happened.
struct Case {
    name: &'static str,
    /// Whether the deleted environment is the first of the two created, so neither
    /// creation order nor "the one the application was created with" decides the verdict.
    doomed_is_first: bool,
    /// Whether the sessions spelled their triple by id rather than by name — each
    /// identifier is "the ID or the name" of its level, and `ReferencedBy` echoes the
    /// spelling the session used.
    by_id: bool,
    /// Whether the withdrawn session's own token had already been served a configuration
    /// when the deletion happened.
    polled_before: bool,
    /// The `DeletionProtectionCheck` the delete request carries — `None` for a request
    /// that names none. The values are the model's `DeletionProtectionCheck` enum
    /// (`DELETION_PROTECTION_CHECK_VALUES` in the AppConfig crate's
    /// `src/ops/types/shared_types.rs`: ACCOUNT_DEFAULT, APPLY, BYPASS); `APPLY` is not a
    /// row here because it asks for the protection check to RUN even where the account
    /// has protection disabled, which is a rule about whether the delete happens at all
    /// rather than about what the data plane does once it has.
    deletion_protection_check: Option<&'static str>,
    content: &'static [u8],
    content_type: &'static str,
    version_label: Option<&'static str>,
}

const CASES: &[Case] = &[
    Case {
        name: "the first environment is deleted, spelled by name, after the session was served",
        doomed_is_first: true,
        by_id: false,
        polled_before: true,
        deletion_protection_check: None,
        content: br#"{"shared":"between-both-environments"}"#,
        content_type: "application/json",
        version_label: Some("shared-1"),
    },
    Case {
        name: "the second environment is deleted with BYPASS, spelled by id, before the \
               session's first poll",
        doomed_is_first: false,
        by_id: true,
        polled_before: false,
        deletion_protection_check: Some("BYPASS"),
        content: b"shared: between-both-environments\n",
        content_type: "application/x-yaml",
        version_label: None,
    },
    Case {
        name: "the first environment is deleted with ACCOUNT_DEFAULT while serving opaque \
               bytes, spelled by name",
        doomed_is_first: true,
        by_id: false,
        polled_before: false,
        deletion_protection_check: Some("ACCOUNT_DEFAULT"),
        content: &[0x00, 0x9f, 0x92, 0x96, 0xff, 0x01],
        content_type: "application/octet-stream",
        version_label: Some("binary-3"),
    },
    Case {
        name: "the second environment is deleted with BYPASS, spelled by id, after the \
               session was served plain text",
        doomed_is_first: false,
        by_id: true,
        polled_before: true,
        deletion_protection_check: Some("BYPASS"),
        content: b"on",
        content_type: "text/plain",
        version_label: None,
    },
];

/// Deleting one environment withdraws every session polling it once the withdrawal falls
/// due — the live, unexpired token such a session holds keeps polling until then, and is
/// refused with the modelled `ResourceNotFoundException` naming the environment
/// afterwards, as is a session started over the deleted triple — while its sibling
/// environment under the same application, polling the same configuration profile and
/// served from the same configuration version, keeps serving that version's exact bytes.
#[tokio::test]
async fn deleting_one_environment_withdraws_only_its_own_sessions() {
    for case in CASES {
        let name = case.name;
        let emulator = Emulator::new();

        // ONE application, ONE configuration profile, TWO environments: the siblings the
        // rule has to tell apart.
        let first = emulator
            .create_hierarchy("SharedApp", "FirstEnvironment", "SharedProfile")
            .await;
        let second_environment_id = emulator
            .create_environment(&first.application_id, "SecondEnvironment")
            .await;
        let second = Hierarchy {
            environment_id: second_environment_id,
            environment_name: "SecondEnvironment".to_string(),
            ..first.clone()
        };

        // ONE hosted configuration version, deployed into BOTH environments, so the
        // surviving session's answer below is read out of the very record the deleted
        // environment was being served from.
        let version = write_hosted_version(&emulator, &first, case).await;
        deploy_version_into(&emulator, &first, &version).await;
        deploy_version_into(&emulator, &second, &version).await;

        let (doomed, surviving) = if case.doomed_is_first {
            (&first, &second)
        } else {
            (&second, &first)
        };
        let (application, doomed_environment, profile) = spelled(doomed, case.by_id);
        let (_, surviving_environment, _) = spelled(surviving, case.by_id);

        // The barrier the refusal is measured against: both environments serve the
        // deployed bytes through a session of their own before anything is deleted.
        let witness = emulator
            .session_token(application, doomed_environment, profile)
            .await;
        let witness = assert_serves(&emulator, &witness, name, "the doomed environment", case).await;
        let surviving_token = emulator
            .session_token(application, surviving_environment, profile)
            .await;
        let surviving_token =
            assert_serves(&emulator, &surviving_token, name, "the sibling environment", case).await;

        // The token the rule is about: minted BEFORE the deletion, and for the rows that
        // ask for it already served once.
        let pre_token = emulator
            .session_token(application, doomed_environment, profile)
            .await;
        let pre_token = if case.polled_before {
            assert_serves(&emulator, &pre_token, name, "the pre-deletion token", case).await
        } else {
            pre_token
        };

        delete_environment(&emulator, doomed, case.deletion_protection_check).await;
        assert_control_plane_withdrew(&emulator, doomed, surviving, name).await;

        // THE RULE: every live, unexpired token of the deleted environment keeps polling
        // while the resolution that bound its session stands, and is refused once the
        // withdrawal has fallen due — the one minted before the deletion and the witness
        // chain alike — after which the refusal is the STABLE answer a client's poll loop
        // converges on rather than a one-off.
        for (label, token, expect_content) in [
            // The witness chain has already been served this version; the pre-deletion
            // token has only for the rows that polled it.
            ("the pre-deletion token", &pre_token, !case.polled_before),
            ("the witness chain", &witness, false),
        ] {
            assert_withdrawal_falls_due(
                &emulator,
                token,
                case,
                label,
                expect_content,
                doomed_environment,
            )
            .await;
        }

        // A session STARTED over the deleted triple is refused at once: resolving the
        // triple against the live control plane is what starting a session does.
        let started = emulator
            .post_json(
                "/configurationsessions",
                &format!(
                    r#"{{"ApplicationIdentifier":"{application}",
                          "EnvironmentIdentifier":"{doomed_environment}",
                          "ConfigurationProfileIdentifier":"{profile}"}}"#
                ),
            )
            .await;
        assert_eq!(
            started.status,
            404,
            "case {name}: a session over the deleted environment expected 404, got {} body {}",
            started.status,
            started.text()
        );
        assert_eq!(
            started.json()["ResourceType"].as_str(),
            Some(RESOURCE_TYPE_ENVIRONMENT),
            "case {name}: a session over the deleted environment expected ResourceType \
             {RESOURCE_TYPE_ENVIRONMENT}, got {}",
            started.text()
        );

        // And the sibling is untouched: the session that was already polling it holds the
        // latest version, so its poll serves no content...
        let sibling_poll = emulator.get_latest_configuration(&surviving_token).await;
        assert_eq!(
            sibling_poll.status,
            200,
            "case {name}: the sibling environment's poll expected 200, got {} body {}",
            sibling_poll.status,
            sibling_poll.text()
        );
        assert!(
            sibling_poll.body.is_empty(),
            "case {name}: the sibling session holds the latest version, so its poll must \
             serve no content, got {:?}",
            sibling_poll.body.as_ref()
        );

        // ...and a brand-new session over the sibling triple is still served the shared
        // version's exact bytes, which is what shows the deletion took away the
        // environment and nothing else.
        let fresh = emulator
            .session_token(application, surviving_environment, profile)
            .await;
        assert_serves(&emulator, &fresh, name, "a fresh sibling session", case).await;
    }
}

/// The hierarchy's triple as a row's sessions spell it: every identifier is "the ID or
/// the name" of its level.
fn spelled(hierarchy: &Hierarchy, by_id: bool) -> (&str, &str, &str) {
    if by_id {
        (
            &hierarchy.application_id,
            &hierarchy.environment_id,
            &hierarchy.profile_id,
        )
    } else {
        (
            &hierarchy.application_name,
            &hierarchy.environment_name,
            &hierarchy.profile_name,
        )
    }
}

/// Write the row's configuration into AppConfig's hosted store, answered by the version
/// number AppConfig minted — the one both deployments below name.
async fn write_hosted_version(emulator: &Emulator, hierarchy: &Hierarchy, case: &Case) -> String {
    let application_id = &hierarchy.application_id;
    let profile_id = &hierarchy.profile_id;
    let headers: Vec<(&str, &str)> = case
        .version_label
        .map(|label| vec![("VersionLabel", label)])
        .unwrap_or_default();
    let written = emulator
        .send(
            "POST",
            &format!(
                "/applications/{application_id}/configurationprofiles/{profile_id}\
                 /hostedconfigurationversions"
            ),
            case.content_type,
            &headers,
            Bytes::copy_from_slice(case.content),
        )
        .await;
    assert_eq!(
        written.status,
        201,
        "case {}: CreateHostedConfigurationVersion expected 201, got {} body {}",
        case.name,
        written.status,
        written.text()
    );
    written
        .header("version-number")
        .unwrap_or_else(|| {
            panic!(
                "case {}: no Version-Number header; headers {:?}",
                case.name, written.headers
            )
        })
        .to_string()
}

/// Deploy one hosted configuration version into one environment, and let the stages the
/// CLOCK owes the deployment (the rollout reaching its targets, the bake, the completion)
/// fall due rather than sleeping for them — see `Emulator::settle_deployments`.
async fn deploy_version_into(emulator: &Emulator, hierarchy: &Hierarchy, version: &str) {
    let application_id = &hierarchy.application_id;
    let environment_id = &hierarchy.environment_id;
    let profile_id = &hierarchy.profile_id;
    let started = emulator
        .post_json(
            &format!("/applications/{application_id}/environments/{environment_id}/deployments"),
            &format!(
                r#"{{"DeploymentStrategyId":"{ALL_AT_ONCE}",
                      "ConfigurationProfileId":"{profile_id}",
                      "ConfigurationVersion":"{version}"}}"#
            ),
        )
        .await;
    assert_eq!(
        started.status,
        201,
        "StartDeployment of version {version} into {environment_id}: expected 201, got {} body {}",
        started.status,
        started.text()
    );
    emulator.settle_deployments(hierarchy);
}

/// Poll with `token`, assert the row's configuration is what comes back, and answer the
/// successor token the poll rotated.
async fn assert_serves(
    emulator: &Emulator,
    token: &str,
    case_name: &str,
    whose: &str,
    case: &Case,
) -> String {
    let poll = emulator.get_latest_configuration(token).await;
    assert_eq!(
        poll.status,
        200,
        "case {case_name}: the first poll of {whose} expected 200, got {} body {}",
        poll.status,
        poll.text()
    );
    assert_eq!(
        poll.body.as_ref(),
        case.content,
        "case {case_name}: {whose} must be served the deployed content {:?}, got {:?}",
        case.content,
        poll.body.as_ref()
    );
    assert_eq!(
        poll.header("content-type"),
        Some(case.content_type),
        "case {case_name}: {whose} expected content type {}, got {:?}",
        case.content_type,
        poll.header("content-type")
    );
    poll.next_token()
}

/// Delete one environment through AppConfig's own operation, carrying the row's
/// `DeletionProtectionCheck` when it names one.
async fn delete_environment(emulator: &Emulator, doomed: &Hierarchy, check: Option<&str>) {
    let application_id = &doomed.application_id;
    let environment_id = &doomed.environment_id;
    let headers: Vec<(&str, &str)> = check
        .map(|check| vec![(DELETION_PROTECTION_CHECK_HEADER, check)])
        .unwrap_or_default();
    let path = format!("/applications/{application_id}/environments/{environment_id}");
    let deleted = emulator
        .send("DELETE", &path, "application/json", &headers, Bytes::new())
        .await;
    assert_eq!(
        deleted.status,
        204,
        "DELETE {path} (DeletionProtectionCheck {check:?}): expected 204, got {} body {}",
        deleted.status,
        deleted.text()
    );
}

/// The deletion as the CONTROL plane reports it, so what the data plane answers is
/// asserted against a removal that really happened: the deleted environment is gone from
/// its read and from its application's listing, and the sibling is still there.
async fn assert_control_plane_withdrew(
    emulator: &Emulator,
    doomed: &Hierarchy,
    surviving: &Hierarchy,
    case_name: &str,
) {
    let application_id = &doomed.application_id;
    for (label, environment_id, expected) in [
        ("the deleted environment", &doomed.environment_id, 404),
        ("the sibling environment", &surviving.environment_id, 200),
    ] {
        let read = emulator
            .send(
                "GET",
                &format!("/applications/{application_id}/environments/{environment_id}"),
                "application/json",
                &[],
                Bytes::new(),
            )
            .await;
        assert_eq!(
            read.status,
            expected,
            "case {case_name}: GetEnvironment on {label} expected {expected}, got {} body {}",
            read.status,
            read.text()
        );
    }

    let listed = list_environment_ids(emulator, application_id, case_name).await;
    assert!(
        !listed.contains(&doomed.environment_id),
        "case {case_name}: ListEnvironments must not report the deleted environment {}, got {listed:?}",
        doomed.environment_id
    );
    assert!(
        listed.contains(&surviving.environment_id),
        "case {case_name}: ListEnvironments must still report the sibling environment {}, got {listed:?}",
        surviving.environment_id
    );
}

/// Every environment id the application holds, across however many pages
/// `ListEnvironments` answers in.
async fn list_environment_ids(
    emulator: &Emulator,
    application_id: &str,
    case_name: &str,
) -> Vec<String> {
    let mut ids = Vec::new();
    let mut next: Option<String> = None;
    // One page per environment at worst, and the application holds two: a bound that
    // cannot spin even if a page ever repeated a cursor.
    for _ in 0..8 {
        let query = match &next {
            Some(cursor) => format!("?next_token={cursor}"),
            None => String::new(),
        };
        let listed = emulator
            .send(
                "GET",
                &format!("/applications/{application_id}/environments{query}"),
                "application/json",
                &[],
                Bytes::new(),
            )
            .await;
        assert_eq!(
            listed.status,
            200,
            "case {case_name}: ListEnvironments expected 200, got {} body {}",
            listed.status,
            listed.text()
        );
        let page = listed.json();
        for item in page["Items"]
            .as_array()
            .unwrap_or_else(|| panic!("case {case_name}: no Items in {}", listed.text()))
        {
            ids.push(
                item["Id"]
                    .as_str()
                    .unwrap_or_else(|| panic!("case {case_name}: no Id in {item}"))
                    .to_string(),
            );
        }
        match page["NextToken"].as_str() {
            Some(cursor) => next = Some(cursor.to_string()),
            None => return ids,
        }
    }
    panic!("case {case_name}: ListEnvironments never ran out of pages, ids so far {ids:?}");
}

/// One poll chain of the deleted environment: the poll that follows the deletion is still
/// served from what the session is bound to — the deployed bytes for a session that has not
/// been given this version yet, nothing for one that has — and once the withdrawal has
/// fallen due every poll of the chain is refused the same way.
async fn assert_withdrawal_falls_due(
    emulator: &Emulator,
    token: &str,
    case: &Case,
    label: &str,
    expect_content: bool,
    environment_identifier: &str,
) {
    let case_name = case.name;
    let served = emulator.get_latest_configuration(token).await;
    assert_eq!(
        served.status,
        200,
        "case {case_name}: {label} polled after the deletion expected 200, got {} body {}",
        served.status,
        served.text()
    );
    let expected: &[u8] = if expect_content { case.content } else { b"" };
    assert_eq!(
        served.body.as_ref(),
        expected,
        "case {case_name}: {label} polled after the deletion expected the payload {:?}, got {:?}",
        String::from_utf8_lossy(expected),
        served.body.as_ref()
    );
    let next = served.next_token();

    // The refusal falls due one withdrawal span after the poll that found the environment
    // gone, and reaching that instant is the CLOCK's doing, which no operation exposes.
    emulator.expire_withdrawal_deadline(&next);
    for poll in [
        "the poll once the withdrawal had fallen due",
        "the next poll of the chain",
    ] {
        let refused = emulator.get_latest_configuration(&next).await;
        assert_withdrawn(
            &refused,
            case_name,
            &format!("{label}: {poll}"),
            environment_identifier,
        );
    }
}

/// One refusal a withdrawn poll must answer with: 404 `ResourceNotFoundException` naming
/// the environment that is gone and the session identifier that referenced it, with no
/// configuration served and no successor token minted.
fn assert_withdrawn(
    reply: &super::configuration_session_poll::Reply,
    case_name: &str,
    poll: &str,
    environment_identifier: &str,
) {
    let text = reply.text();
    assert_eq!(
        reply.status, 404,
        "case {case_name}: {poll} expected 404, got {} body {text}",
        reply.status
    );
    assert_eq!(
        reply.header("x-amzn-errortype"),
        Some("ResourceNotFoundException"),
        "case {case_name}: {poll} expected the error type header, got {:?}",
        reply.header("x-amzn-errortype")
    );
    assert_eq!(
        reply.json()["ResourceType"].as_str(),
        Some(RESOURCE_TYPE_ENVIRONMENT),
        "case {case_name}: {poll} expected ResourceType {RESOURCE_TYPE_ENVIRONMENT}, got {text}"
    );
    assert_eq!(
        reply.json()["ReferencedBy"][ENVIRONMENT_IDENTIFIER_MEMBER].as_str(),
        Some(environment_identifier),
        "case {case_name}: {poll} expected ReferencedBy[{ENVIRONMENT_IDENTIFIER_MEMBER}] to name \
         {environment_identifier}, got {text}"
    );
    assert_eq!(
        reply.json()["Message"].as_str(),
        Some(format!("{RESOURCE_TYPE_ENVIRONMENT} not found").as_str()),
        "case {case_name}: {poll} expected the environment's fixed not-found sentence, got {text}"
    );
    assert!(
        reply.header("next-poll-configuration-token").is_none(),
        "case {case_name}: {poll} serves nothing, so it must rotate no token, got {text}"
    );
    assert!(
        reply.body.is_empty() || reply.json()["Configuration"].is_null(),
        "case {case_name}: {poll} must serve no configuration, got {text}"
    );
}
