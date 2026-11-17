// SPDX-License-Identifier: CC0-1.0

//! The deprecated `GetConfiguration` read: WHICH of its two refusals a triple gets, and
//! which identifiers each one names.
//!
//! RULE: this path looks the (Application, Configuration) PAIR up first, and that pair is
//! the only thing it reports as a configuration profile it could not find — naming each of
//! the two levels as the call resolved it, by the id a level that resolved was looked up
//! under and by the label the request spelled for one that resolved to nothing. Once the
//! pair resolves, the `Environment` is the key the DEPLOYMENT serving that profile is looked
//! up by and nothing else, so every triple with no deployed version in force — an
//! environment label nothing resolves as much as a resolvable environment nothing was
//! deployed into — is the deployment-level refusal `No deployed version for configuration
//! profile <Configuration> in environment <Environment> and app <Application>`, built from
//! the three labels the request carried and reporting no account id.
//!
//! Each path member is documented as "the <level> name or the <level> ID"
//! (`smithy_query(query="operation", operation="GetConfiguration")`), so the label a caller
//! sends and the id a lookup is made with are two different strings, and the two refusals
//! differ in which of them they report.
//!
//! The rows span what the rule generalises over: which level is addressed by name and which
//! by id, which level fails to resolve, the form of an environment label nothing holds, and
//! a triple that resolves whole with nothing deployed into it — and, last, the same triple
//! once a version IS deployed, which is what makes the environment a deployment key rather
//! than a resource this refusal reports on. What this path answers a request that omits its
//! `@required` `client_id` is `omitted_required_bindings`, with the other member this
//! service's model binds that way.

use std::time::{Duration, Instant};

use crate::appconfig_workflow::{
    account, create_application, create_environment, create_profile, host_version, json_call,
    router, send, Answer,
};
use smithy_server_runtime::MultiServiceRouter;

/// How long a rollout started here is given to reach its targets before the suite fails: a
/// ceiling on the WAIT, not a statement about the span a growth interval takes.
const REACHED_BY: Duration = Duration::from_secs(120);

/// How often the read under observation is repeated while waiting for that condition.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The predefined strategy that reaches every target in one growth interval.
const ALL_AT_ONCE: &str = "AppConfig.AllAtOnce";

/// `GET` one configuration, naming the three levels as the row spells them.
async fn get_configuration(
    router: &MultiServiceRouter,
    application: &str,
    environment: &str,
    configuration: &str,
) -> Answer {
    send(
        router,
        "GET",
        &format!(
            "/applications/{application}/environments/{environment}/configurations/\
             {configuration}?client_id=test-client"
        ),
        "application/json",
        Vec::new(),
        &[],
    )
    .await
}

/// The two refusals this path states, each with the identifiers it reports.
#[derive(Debug)]
enum Refusal {
    /// The (Application, Configuration) pair resolved no configuration profile.
    ProfileNotFound {
        application: String,
        configuration: String,
    },
    /// The pair resolved and no deployed version is in force for the triple.
    NoDeployedVersion {
        application: String,
        environment: String,
        configuration: String,
    },
}

impl Refusal {
    /// The `Message` the refusal must carry.
    fn message(&self) -> String {
        match self {
            Self::ProfileNotFound {
                application,
                configuration,
            } => format!(
                "Configuration Profile with Application Id {application} and Configuration \
                 Profile Id {configuration} could not be found for account {}.",
                account()
            ),
            Self::NoDeployedVersion {
                application,
                environment,
                configuration,
            } => format!(
                "No deployed version for configuration profile {configuration} in environment \
                 {environment} and app {application}"
            ),
        }
    }
}

/// Assert one refusal on the wire: the status and error code an SDK classifies it from, and
/// the exact `Message` the row expects.
fn assert_refused(case: &str, answer: &Answer, expected: &Refusal) {
    assert_eq!(
        answer.status,
        400,
        "case {case:?}: expected 400, got {} / {}",
        answer.status,
        String::from_utf8_lossy(&answer.body)
    );
    let code = answer.error_code();
    assert!(
        code.contains("BadRequestException"),
        "case {case:?}: expected the modelled BadRequestException, got {code:?}"
    );
    let message = expected.message();
    assert_eq!(
        answer.string("Message").unwrap_or_default(),
        message,
        "case {case:?}: expected the refusal {message:?}, got {:?}",
        answer.string("Message")
    );
}

/// RULE: a refusal of the (Application, Configuration) lookup names the application and the
/// configuration profile as this call resolved them — the resolved id where the identifier
/// resolved, the label the request sent where it did not.
#[tokio::test]
async fn the_profile_refusal_names_each_level_as_the_call_resolved_it() {
    let router = router();
    let application_name = "resolved-identifiers";
    let application = create_application(&router, application_name).await;
    let environment_name = "production";
    create_environment(&router, &application, environment_name).await;
    let profile_name = "config";
    let profile = create_profile(&router, &application, profile_name).await;

    // (case, how the request spells the three levels, the refusal it must get)
    let rows: [(&str, &str, &str, &str, Refusal); 4] = [
        (
            "an application addressed by NAME, with a profile that does not resolve",
            application_name,
            environment_name,
            "nosuchprofile",
            Refusal::ProfileNotFound {
                application: application.clone(),
                configuration: "nosuchprofile".to_string(),
            },
        ),
        (
            "an application addressed by its ID, with a profile that does not resolve",
            application.as_str(),
            environment_name,
            "nosuchprofile",
            Refusal::ProfileNotFound {
                application: application.clone(),
                configuration: "nosuchprofile".to_string(),
            },
        ),
        (
            "an application that does not resolve",
            "nosuchapp",
            environment_name,
            profile_name,
            Refusal::ProfileNotFound {
                application: "nosuchapp".to_string(),
                configuration: profile_name.to_string(),
            },
        ),
        (
            "neither the application nor the profile resolves",
            "nosuchapp",
            "nosuchenv",
            "nosuchprofile",
            Refusal::ProfileNotFound {
                application: "nosuchapp".to_string(),
                configuration: "nosuchprofile".to_string(),
            },
        ),
    ];

    for (case, named_application, named_environment, named_configuration, expected) in &rows {
        let answer = get_configuration(
            &router,
            named_application,
            named_environment,
            named_configuration,
        )
        .await;
        assert_refused(case, &answer, expected);
        // The profile refusal is the one that reports a resolution, so it carries the
        // account the lookup was scoped to; the deployment refusal never does.
        assert!(
            answer
                .string("Message")
                .unwrap_or_default()
                .contains(account()),
            "case {case:?}: the profile refusal names the account the lookup was scoped to, got \
             {:?}",
            answer.string("Message")
        );
    }

    // The profile is still there afterwards: a refusal of this read resolves nothing away.
    let still_there = json_call(
        &router,
        "GET",
        &format!("/applications/{application}/configurationprofiles/{profile}"),
        &serde_json::Value::Null,
    )
    .await;
    assert_eq!(
        still_there.status,
        200,
        "case \"the profile the refusals named\": GetConfigurationProfile must answer 200, got \
         {} / {}",
        still_there.status,
        String::from_utf8_lossy(&still_there.body)
    );
}

/// RULE: once the (Application, Configuration) pair resolves, the environment is a
/// deployment-lookup key — so an environment label nothing resolves and a resolvable
/// environment with nothing deployed are the one `No deployed version …` refusal, built from
/// the three labels the request spelled.
#[tokio::test]
async fn a_triple_with_no_deployed_version_is_refused_as_a_deployment_lookup() {
    let router = router();
    let application_name = "deployment-lookup";
    let application = create_application(&router, application_name).await;
    let environment_name = "production";
    let environment = create_environment(&router, &application, environment_name).await;
    let profile_name = "config";
    let profile = create_profile(&router, &application, profile_name).await;

    // The rows differ along the two things the rule generalises over: whether the
    // environment label resolves at all (and what form an unresolvable one takes), and how
    // the request spells the application and the configuration profile.
    let rows: [(&str, &str, &str, &str); 7] = [
        (
            "an environment label nothing holds, every level by id",
            application.as_str(),
            "sim2real-nonexistent-env-xyz",
            profile.as_str(),
        ),
        (
            "an environment label nothing holds, application and profile by name",
            application_name,
            "sim2real-nonexistent-env-xyz",
            profile_name,
        ),
        (
            "an environment label shaped like a minted id but held by nothing",
            application.as_str(),
            "abcd1234",
            profile.as_str(),
        ),
        (
            "an environment label that is a single character",
            application.as_str(),
            "x",
            profile.as_str(),
        ),
        (
            "an environment label at the modelled 64-character ceiling",
            application.as_str(),
            // `StringWithLengthBetween1And64`, the type of all three labels
            // (`smithy_query(query="operation", operation="GetConfiguration")`).
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            profile.as_str(),
        ),
        (
            "a resolvable environment with nothing deployed, every level by id",
            application.as_str(),
            environment.as_str(),
            profile.as_str(),
        ),
        (
            "a resolvable environment with nothing deployed, every level by name",
            application_name,
            environment_name,
            profile_name,
        ),
    ];

    for (case, named_application, named_environment, named_configuration) in &rows {
        let answer = get_configuration(
            &router,
            named_application,
            named_environment,
            named_configuration,
        )
        .await;
        assert_refused(
            case,
            &answer,
            &Refusal::NoDeployedVersion {
                application: (*named_application).to_string(),
                environment: (*named_environment).to_string(),
                configuration: (*named_configuration).to_string(),
            },
        );
        // The condition is a deployment that is not there, so the refusal must not claim
        // the configuration profile the caller addressed could not be found.
        let message = answer.string("Message").unwrap_or_default();
        assert!(
            !message.contains("could not be found"),
            "case {case:?}: a missing deployment must not be reported as a missing \
             configuration profile, got {message:?}"
        );
        assert!(
            !message.contains(account()),
            "case {case:?}: the deployment refusal reports no account id, got {message:?}"
        );
    }
}

/// RULE (the other half): the environment decides only WHICH deployment is looked up, so a
/// version deployed into one environment is served there while a sibling environment with
/// nothing deployed keeps getting the `No deployed version …` refusal.
#[tokio::test]
async fn a_deployed_version_is_served_while_a_sibling_environment_is_still_refused() {
    let router = router();
    let case = "a version deployed into one of two environments";
    let application = create_application(&router, "deployment-key").await;
    let deployed_into = create_environment(&router, &application, "production").await;
    let untouched = create_environment(&router, &application, "staging").await;
    let profile = create_profile(&router, &application, "config").await;
    let version = host_version(&router, &application, &profile, case, b"served", None).await;

    let answer = json_call(
        &router,
        "POST",
        &format!("/applications/{application}/environments/{deployed_into}/deployments"),
        &serde_json::json!({
            "DeploymentStrategyId": ALL_AT_ONCE,
            "ConfigurationProfileId": profile,
            "ConfigurationVersion": version.to_string(),
        }),
    )
    .await;
    assert_eq!(
        answer.status,
        201,
        "case {case:?}: StartDeployment must answer 201, got {} / {}",
        answer.status,
        String::from_utf8_lossy(&answer.body)
    );

    // A rollout reaches its targets after the call that started it, so the CONDITION is
    // waited for rather than a span of the clock.
    let deadline = Instant::now() + REACHED_BY;
    let served = loop {
        let read = get_configuration(&router, &application, &deployed_into, &profile).await;
        if read.status == 200 {
            break read;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the deployed version was not served within {}s; the read still \
             answers {} / {}",
            REACHED_BY.as_secs(),
            read.status,
            String::from_utf8_lossy(&read.body)
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    };
    assert_eq!(
        served.body.as_ref(),
        b"served",
        "case {case:?}: expected the deployed content, got {:?}",
        String::from_utf8_lossy(&served.body)
    );

    // The sibling environment has no deployment of this profile, which is the refusal and
    // not a claim about the profile that just served a configuration next door.
    assert_refused(
        "the sibling environment with nothing deployed",
        &get_configuration(&router, &application, &untouched, &profile).await,
        &Refusal::NoDeployedVersion {
            application: application.clone(),
            environment: untouched.clone(),
            configuration: profile.clone(),
        },
    );
}
