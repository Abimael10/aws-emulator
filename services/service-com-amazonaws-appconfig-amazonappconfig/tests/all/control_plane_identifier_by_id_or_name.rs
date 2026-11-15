// SPDX-License-Identifier: CC0-1.0

//! How every AppConfig control-plane operation ADDRESSES an application, an environment or
//! a configuration profile.
//!
//! RULE: a member that addresses one of those three resources is typed to that resource's
//! own NAME shape — `ApplicationId` and `EnvironmentId` target `Name` (`@length` 1..64),
//! `ConfigurationProfileId` targets `LongName` (1..128) — and none of them carries the `Id`
//! pattern `^[a-z0-9]{4,7}$` that the same operations' OUTPUT members carry
//! (`smithy_query(query="operation",operation="GetApplication,GetEnvironment,GetConfigurationProfile")`).
//! So such a member holds either the logical id the create minted or the name the create
//! supplied: the service resolves it by id first and then by exact name within the scope
//! that holds the resource, acts on the resource it resolved, and reports the resolved
//! logical id in its `Id` members.
//!
//! The suites below span what the rule generalises over rather than one recorded request:
//! the FORM of the name (one character, each member's own `@length` maximum, a value shaped
//! like a minted id, punctuation), WHICH level of a path is spelled by name, and the KIND of
//! operation the identifier reaches (reporting, creating, patching, deleting, validating) —
//! and, at the edges, that a name is matched exactly, only within the application that holds
//! the resource, and only after the id lookup that takes precedence over it.
//!
//! Every case drives real HTTP requests through the router the emulator's front door uses,
//! so what is asserted is what an AppConfig client sees on the wire.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::appconfig_workflow::{
    account, create_application, create_environment, create_profile, host_version,
    instant_deployment_strategy, is_modelled_id, json_call, router, send,
    TERMINAL_DEPLOYMENT_STATES,
};

/// The documented upper bound of `Name` (length 1..64), which `ApplicationId` and
/// `EnvironmentId` target.
const MAX_NAME: usize = 64;

/// The documented upper bound of `LongName` (length 1..128), which `ConfigurationProfileId`
/// targets.
const MAX_LONG_NAME: usize = 128;

/// The bytes every hosted version in this suite holds.
const CONTENT: &[u8] = br#"{"enabled":true}"#;

/// How a row spells the three levels of a path.
#[derive(Debug, Clone)]
struct Spelled {
    application: String,
    environment: String,
    profile: String,
}

/// The ids a fixture minted, beside the names it supplied.
#[derive(Debug, Clone)]
struct Fixture {
    application_id: String,
    application_name: String,
    environment_id: String,
    environment_name: String,
    profile_id: String,
    profile_name: String,
}

impl Fixture {
    /// Every level spelled by the id the create minted.
    fn by_id(&self) -> Spelled {
        Spelled {
            application: self.application_id.clone(),
            environment: self.environment_id.clone(),
            profile: self.profile_id.clone(),
        }
    }

    /// Every level spelled by the name the create supplied.
    fn by_name(&self) -> Spelled {
        Spelled {
            application: self.application_name.clone(),
            environment: self.environment_name.clone(),
            profile: self.profile_name.clone(),
        }
    }
}

/// An application holding one environment, one configuration profile and one hosted
/// version, created under the names the row carries.
async fn fixture(
    router: &MultiServiceRouter,
    case: &str,
    application_name: &str,
    environment_name: &str,
    profile_name: &str,
) -> Fixture {
    let application_id = create_application(router, application_name).await;
    let environment_id = create_environment(router, &application_id, environment_name).await;
    let profile_id = create_profile(router, &application_id, profile_name).await;
    host_version(router, &application_id, &profile_id, case, CONTENT, None).await;
    Fixture {
        application_id,
        application_name: application_name.to_string(),
        environment_id,
        environment_name: environment_name.to_string(),
        profile_id,
        profile_name: profile_name.to_string(),
    }
}

/// Every control-plane READ addressed through one of the three identifiers, in the order
/// they are compared, each beside the operation it is.
fn reads(spelled: &Spelled) -> Vec<(&'static str, String, &'static str)> {
    let Spelled {
        application,
        environment,
        profile,
    } = spelled;
    vec![
        ("GetApplication", format!("/applications/{application}"), "GET"),
        (
            "ListEnvironments",
            format!("/applications/{application}/environments"),
            "GET",
        ),
        (
            "ListConfigurationProfiles",
            format!("/applications/{application}/configurationprofiles"),
            "GET",
        ),
        (
            "GetEnvironment",
            format!("/applications/{application}/environments/{environment}"),
            "GET",
        ),
        (
            "ListDeployments",
            format!("/applications/{application}/environments/{environment}/deployments"),
            "GET",
        ),
        (
            "GetDeployment",
            format!("/applications/{application}/environments/{environment}/deployments/1"),
            "GET",
        ),
        (
            "GetConfigurationProfile",
            format!("/applications/{application}/configurationprofiles/{profile}"),
            "GET",
        ),
        (
            "ListHostedConfigurationVersions",
            format!(
                "/applications/{application}/configurationprofiles/{profile}/hostedconfigurationversions"
            ),
            "GET",
        ),
        (
            "GetHostedConfigurationVersion",
            format!(
                "/applications/{application}/configurationprofiles/{profile}/hostedconfigurationversions/1"
            ),
            "GET",
        ),
        (
            "ValidateConfiguration",
            format!(
                "/applications/{application}/configurationprofiles/{profile}/validators?configuration_version=1"
            ),
            "POST",
        ),
    ]
}

/// RULE: a reporting operation addressed by a resource's NAME answers exactly what the same
/// operation addressed by that resource's ID answers — the same status and the same body,
/// carrying the resolved logical ids.
///
/// The rows span the FORM of the name a create may have supplied: a single character and the
/// member's own `@length` maximum at each level, a name shaped like a minted id (which the
/// id lookup misses, so only the name resolves it), and a name carrying the punctuation
/// AppConfig names allow.
#[tokio::test]
async fn a_control_plane_read_resolves_a_name_exactly_as_it_resolves_an_id() {
    let router = router();
    let strategy = instant_deployment_strategy(&router, "instant-identifier-reads").await;

    // (case, the application's name, the environment's name, the profile's name)
    let rows: [(&str, String, String, String); 4] = [
        (
            "a single-character name at every level",
            "a".to_string(),
            "e".to_string(),
            "p".to_string(),
        ),
        (
            "a name at each member's length maximum",
            "n".repeat(MAX_NAME),
            "v".repeat(MAX_NAME),
            "c".repeat(MAX_LONG_NAME),
        ),
        (
            "names shaped like a minted id",
            "zzzzzzz".to_string(),
            "yyyyyyy".to_string(),
            "xxxxxxx".to_string(),
        ),
        (
            "names carrying the punctuation AppConfig allows",
            "ops.team-east_1".to_string(),
            "pre.prod-eu_2".to_string(),
            "flags.ui-rollout_3".to_string(),
        ),
    ];

    for (case, application_name, environment_name, profile_name) in &rows {
        let built = fixture(
            &router,
            case,
            application_name,
            environment_name,
            profile_name,
        )
        .await;

        // A deployment of that version, so the deployment reads below report a record
        // rather than an empty listing. Started by id and settled before anything is
        // compared, so the two readings of it cannot differ by the clock having moved.
        let started = json_call(
            &router,
            "POST",
            &format!(
                "/applications/{}/environments/{}/deployments",
                built.application_id, built.environment_id
            ),
            &json!({
                "DeploymentStrategyId": strategy,
                "ConfigurationProfileId": built.profile_id,
                "ConfigurationVersion": "1",
            }),
        )
        .await;
        assert_eq!(
            started.status,
            201,
            "case {case:?}: StartDeployment must answer 201, got {} / {}",
            started.status,
            String::from_utf8_lossy(&started.body)
        );
        crate::appconfig_workflow::deployment_in_state(
            &router,
            case,
            &built.application_id,
            &built.environment_id,
            1,
            TERMINAL_DEPLOYMENT_STATES,
        )
        .await;

        let by_id = reads(&built.by_id());
        let by_name = reads(&built.by_name());
        for ((operation, id_uri, method), (_, name_uri, _)) in by_id.iter().zip(by_name.iter()) {
            let addressed_by_id = json_call(&router, method, id_uri, &Value::Null).await;
            let addressed_by_name = json_call(&router, method, name_uri, &Value::Null).await;
            assert!(
                (200..300).contains(&addressed_by_id.status),
                "case {case:?}: {operation} addressed by id must succeed, got {} / {}",
                addressed_by_id.status,
                String::from_utf8_lossy(&addressed_by_id.body)
            );
            assert_eq!(
                addressed_by_name.status,
                addressed_by_id.status,
                "case {case:?}: {operation} addressed by name must answer the status it answers \
                 for the id ({}), got {} / {}",
                addressed_by_id.status,
                addressed_by_name.status,
                String::from_utf8_lossy(&addressed_by_name.body)
            );
            assert_eq!(
                addressed_by_name.body,
                addressed_by_id.body,
                "case {case:?}: {operation} addressed by name must answer the body it answers for \
                 the id, got {:?}",
                String::from_utf8_lossy(&addressed_by_name.body)
            );
        }

        // The ids those answers report are the minted ones, which is what makes the
        // comparison above a statement about resolution rather than about two echoes of the
        // caller's own spelling.
        let read_by_name = json_call(
            &router,
            "GET",
            &format!("/applications/{}", built.application_name),
            &Value::Null,
        )
        .await;
        assert_eq!(
            read_by_name.string("Id").as_deref(),
            Some(built.application_id.as_str()),
            "case {case:?}: GetApplication by name must report the minted Id, got {:?}",
            read_by_name.string("Id")
        );
        assert_eq!(
            read_by_name.string("Name").as_deref(),
            Some(built.application_name.as_str()),
            "case {case:?}: GetApplication by name must report the stored Name, got {:?}",
            read_by_name.string("Name")
        );
        let environment_by_name = json_call(
            &router,
            "GET",
            &format!(
                "/applications/{}/environments/{}",
                built.application_name, built.environment_name
            ),
            &Value::Null,
        )
        .await;
        for (member, expected) in [
            ("ApplicationId", &built.application_id),
            ("Id", &built.environment_id),
        ] {
            assert_eq!(
                environment_by_name.string(member).as_deref(),
                Some(expected.as_str()),
                "case {case:?}: GetEnvironment by name must report {member} {expected:?}, got {:?}",
                environment_by_name.string(member)
            );
        }
        assert!(
            is_modelled_id(&built.profile_id),
            "case {case:?}: the minted profile id {:?} must satisfy the modelled Id shape",
            built.profile_id
        );
        let profile_by_name = json_call(
            &router,
            "GET",
            &format!(
                "/applications/{}/configurationprofiles/{}",
                built.application_name, built.profile_name
            ),
            &Value::Null,
        )
        .await;
        for (member, expected) in [
            ("ApplicationId", built.application_id.as_str()),
            ("Id", built.profile_id.as_str()),
            ("Name", built.profile_name.as_str()),
            ("LocationUri", "hosted"),
            ("Type", "AWS.Freeform"),
        ] {
            assert_eq!(
                profile_by_name.string(member).as_deref(),
                Some(expected),
                "case {case:?}: GetConfigurationProfile by name must report {member} \
                 {expected:?}, got {:?}",
                profile_by_name.string(member)
            );
        }
    }
}

/// One write, spelled from whichever form of the three identifiers the row carries.
struct Write {
    case: &'static str,
    method: &'static str,
    /// The path the write is addressed by, built from the row's spelling.
    path: fn(&Spelled) -> String,
    /// The request body.
    body: fn(&Spelled) -> Value,
    status: u16,
    /// The members whose value must be the application's resolved id.
    application_id_members: &'static [&'static str],
}

/// The creates and patches a control-plane identifier reaches, each of which must act on the
/// resource the identifier resolves and report the resolved ids.
const WRITES: &[Write] = &[
    Write {
        case: "UpdateApplication",
        method: "PATCH",
        path: |spelled| format!("/applications/{}", spelled.application),
        body: |spelled| json!({ "Description": format!("patched via {}", spelled.application) }),
        status: 200,
        application_id_members: &[],
    },
    Write {
        case: "CreateEnvironment",
        method: "POST",
        path: |spelled| format!("/applications/{}/environments", spelled.application),
        body: |spelled| json!({ "Name": format!("made-under-{}", spelled.application) }),
        status: 201,
        application_id_members: &["ApplicationId"],
    },
    Write {
        case: "CreateConfigurationProfile",
        method: "POST",
        path: |spelled| format!("/applications/{}/configurationprofiles", spelled.application),
        body: |spelled| {
            json!({
                "Name": format!("profiled-under-{}", spelled.application),
                "LocationUri": "hosted",
            })
        },
        status: 201,
        application_id_members: &["ApplicationId"],
    },
    Write {
        case: "UpdateEnvironment",
        method: "PATCH",
        path: |spelled| {
            format!(
                "/applications/{}/environments/{}",
                spelled.application, spelled.environment
            )
        },
        body: |spelled| json!({ "Description": format!("patched via {}", spelled.environment) }),
        status: 200,
        application_id_members: &["ApplicationId"],
    },
    Write {
        case: "UpdateConfigurationProfile",
        method: "PATCH",
        path: |spelled| {
            format!(
                "/applications/{}/configurationprofiles/{}",
                spelled.application, spelled.profile
            )
        },
        body: |spelled| json!({ "Description": format!("patched via {}", spelled.profile) }),
        status: 200,
        application_id_members: &["ApplicationId"],
    },
    Write {
        case: "ValidateConfiguration",
        method: "POST",
        path: |spelled| {
            format!(
                "/applications/{}/configurationprofiles/{}/validators?configuration_version=1",
                spelled.application, spelled.profile
            )
        },
        body: |_| Value::Null,
        status: 204,
        application_id_members: &[],
    },
];

/// RULE: a write addressed by a resource's NAME acts on the resource that name resolves to,
/// and reports the resolved logical ids — the same resource, and the same answer, as the
/// write addressed by that resource's id.
#[tokio::test]
async fn a_control_plane_write_resolves_a_name_exactly_as_it_resolves_an_id() {
    let router = router();
    let built = fixture(
        &router,
        "the application every write is addressed into",
        "identifier-writes",
        "staging",
        "hosted-flags",
    )
    .await;

    for (form, spelled) in [
        ("every level by its id", built.by_id()),
        ("every level by its name", built.by_name()),
    ] {
        for write in WRITES {
            let case = write.case;
            let answer = json_call(
                &router,
                write.method,
                &(write.path)(&spelled),
                &(write.body)(&spelled),
            )
            .await;
            assert_eq!(
                answer.status,
                write.status,
                "case {form:?}/{case:?}: expected {}, got {} / {}",
                write.status,
                answer.status,
                String::from_utf8_lossy(&answer.body)
            );
            for member in write.application_id_members {
                assert_eq!(
                    answer.string(member).as_deref(),
                    Some(built.application_id.as_str()),
                    "case {form:?}/{case:?}: {member} must be the resolved application id \
                     {:?}, got {:?}",
                    built.application_id,
                    answer.string(member)
                );
            }
        }

        // A hosted configuration version is written with a raw body, so it is driven on its
        // own: the two bound id headers must carry the resolved ids.
        let hosted = send(
            &router,
            "POST",
            &format!(
                "/applications/{}/configurationprofiles/{}/hostedconfigurationversions",
                spelled.application, spelled.profile
            ),
            "application/json",
            format!(r#"{{"form":"{form}"}}"#).into_bytes(),
            &[],
        )
        .await;
        assert_eq!(
            hosted.status,
            201,
            "case {form:?}: CreateHostedConfigurationVersion must answer 201, got {} / {}",
            hosted.status,
            String::from_utf8_lossy(&hosted.body)
        );
        for (header, expected) in [
            ("Application-Id", &built.application_id),
            ("Configuration-Profile-Id", &built.profile_id),
        ] {
            assert_eq!(
                hosted.header(header).as_deref(),
                Some(expected.as_str()),
                "case {form:?}: {header} must carry the resolved id {expected:?}, got {:?}",
                hosted.header(header)
            );
        }
    }

    // Every write landed on the ONE application the two forms address: the patches are
    // readable through the id-addressed read, and each form's children were created under
    // it rather than under a second record.
    let application = json_call(
        &router,
        "GET",
        &format!("/applications/{}", built.application_id),
        &Value::Null,
    )
    .await;
    assert_eq!(
        application.string("Description").as_deref(),
        Some(format!("patched via {}", built.application_name).as_str()),
        "the by-name patch must be the one the application holds, got {:?}",
        application.string("Description")
    );
    let environments = json_call(
        &router,
        "GET",
        &format!("/applications/{}/environments", built.application_id),
        &Value::Null,
    )
    .await;
    let names: Vec<String> = environments
        .json()
        .get("Items")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("Name").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    for expected in [
        format!("made-under-{}", built.application_id),
        format!("made-under-{}", built.application_name),
    ] {
        assert!(
            names.contains(&expected),
            "the environment created under {expected:?} must be held by the resolved \
             application, got {names:?}"
        );
    }
    // The version each form wrote is a version of the ONE profile both forms addressed:
    // numbered after the version the fixture hosted, and readable through the id path.
    for version_number in [2, 3] {
        let version = json_call(
            &router,
            "GET",
            &format!(
                "/applications/{}/configurationprofiles/{}/hostedconfigurationversions/{version_number}",
                built.application_id, built.profile_id
            ),
            &Value::Null,
        )
        .await;
        assert_eq!(
            version.status,
            200,
            "version {version_number} must be held by the resolved profile, got {} / {}",
            version.status,
            String::from_utf8_lossy(&version.body)
        );
    }
}

/// RULE: a delete addressed by a resource's NAME removes the resource that name resolves to,
/// so the id-addressed read of it misses afterwards.
///
/// The rows walk the tree inwards-out, one kind of resource per step, each delete addressed
/// entirely by names.
#[tokio::test]
async fn a_delete_addressed_by_name_removes_the_resource_the_name_resolves() {
    let router = router();
    let built = fixture(
        &router,
        "the tree the deletes take apart",
        "identifier-deletes",
        "retired",
        "retired-config",
    )
    .await;
    let Fixture {
        application_id,
        application_name,
        environment_id,
        environment_name,
        profile_id,
        profile_name,
    } = built.clone();

    // (case, the DELETE addressed by names, the id-addressed read that must miss afterwards)
    let steps: [(&str, String, String); 4] = [
        (
            "DeleteHostedConfigurationVersion",
            format!(
                "/applications/{application_name}/configurationprofiles/{profile_name}/hostedconfigurationversions/1"
            ),
            format!(
                "/applications/{application_id}/configurationprofiles/{profile_id}/hostedconfigurationversions/1"
            ),
        ),
        (
            "DeleteConfigurationProfile",
            format!("/applications/{application_name}/configurationprofiles/{profile_name}"),
            format!("/applications/{application_id}/configurationprofiles/{profile_id}"),
        ),
        (
            "DeleteEnvironment",
            format!("/applications/{application_name}/environments/{environment_name}"),
            format!("/applications/{application_id}/environments/{environment_id}"),
        ),
        (
            "DeleteApplication",
            format!("/applications/{application_name}"),
            format!("/applications/{application_id}"),
        ),
    ];

    for (case, delete_uri, read_uri) in &steps {
        let read_before = json_call(&router, "GET", read_uri, &Value::Null).await;
        assert_eq!(
            read_before.status,
            200,
            "case {case:?}: the resource must be readable before the delete, got {} / {}",
            read_before.status,
            String::from_utf8_lossy(&read_before.body)
        );
        let deleted = json_call(&router, "DELETE", delete_uri, &Value::Null).await;
        assert_eq!(
            deleted.status,
            204,
            "case {case:?}: the name-addressed delete must answer the modelled 204, got {} / {}",
            deleted.status,
            String::from_utf8_lossy(&deleted.body)
        );
        let read_after = json_call(&router, "GET", read_uri, &Value::Null).await;
        assert_eq!(
            read_after.status,
            404,
            "case {case:?}: the id-addressed read must miss after the by-name delete, got {} / {}",
            read_after.status,
            String::from_utf8_lossy(&read_after.body)
        );
    }
}

/// RULE: only an exact name resolves, and only within the application that holds the
/// resource; an identifier that is neither an id nor such a name is that level's own
/// not-found refusal.
#[tokio::test]
async fn a_name_resolves_exactly_and_only_inside_its_own_application() {
    let router = router();
    let account = account();
    let held = fixture(
        &router,
        "the application holding the names",
        "scoped-names",
        "scoped-environment",
        "scoped-profile",
    )
    .await;
    // A second application with its own children, whose names the first application does
    // not hold: a name resolves inside the application the path scopes the lookup to.
    let elsewhere = fixture(
        &router,
        "the application whose names are out of scope",
        "other-names",
        "other-environment",
        "other-profile",
    )
    .await;

    // (case, the path, the refusal the level that resolved to nothing states)
    let rows: [(&str, String, String); 6] = [
        (
            "an application name no application holds",
            format!("/applications/{}-absent", held.application_name),
            format!(
                "Application with Id {}-absent could not be found for account {account}.",
                held.application_name
            ),
        ),
        (
            "an application name differing only in case",
            format!("/applications/{}", held.application_name.to_uppercase()),
            format!(
                "Application with Id {} could not be found for account {account}.",
                held.application_name.to_uppercase()
            ),
        ),
        (
            "an environment name another application holds",
            format!(
                "/applications/{}/environments/{}",
                held.application_name, elsewhere.environment_name
            ),
            format!(
                "Environment with Application Id {} and Environment Id {} could not be found \
                 for account {account}.",
                held.application_id, elsewhere.environment_name
            ),
        ),
        (
            "an environment name differing only in case",
            format!(
                "/applications/{}/environments/{}",
                held.application_name,
                held.environment_name.to_uppercase()
            ),
            format!(
                "Environment with Application Id {} and Environment Id {} could not be found \
                 for account {account}.",
                held.application_id,
                held.environment_name.to_uppercase()
            ),
        ),
        (
            "a profile name another application holds",
            format!(
                "/applications/{}/configurationprofiles/{}",
                held.application_name, elsewhere.profile_name
            ),
            format!(
                "Configuration Profile with Application Id {} and Configuration Profile Id {} \
                 could not be found for account {account}.",
                held.application_id, elsewhere.profile_name
            ),
        ),
        (
            "a profile name differing only in case",
            format!(
                "/applications/{}/configurationprofiles/{}",
                held.application_name,
                held.profile_name.to_uppercase()
            ),
            format!(
                "Configuration Profile with Application Id {} and Configuration Profile Id {} \
                 could not be found for account {account}.",
                held.application_id,
                held.profile_name.to_uppercase()
            ),
        ),
    ];

    for (case, uri, expected) in &rows {
        let refused = json_call(&router, "GET", uri, &Value::Null).await;
        assert_eq!(
            refused.status,
            404,
            "case {case:?}: expected 404, got {} / {}",
            refused.status,
            String::from_utf8_lossy(&refused.body)
        );
        assert!(
            refused.error_code().contains("ResourceNotFoundException"),
            "case {case:?}: expected ResourceNotFoundException, got {:?}",
            refused.error_code()
        );
        assert_eq!(
            refused.string("Message").unwrap_or_default(),
            *expected,
            "case {case:?}: expected the refusal {expected:?}, got {:?}",
            refused.string("Message")
        );
    }

    // Nothing above disturbed the resources that do exist: each still reads back through its
    // own application, by id and by name.
    for (case, uri) in [
        (
            "the held environment by name",
            format!(
                "/applications/{}/environments/{}",
                held.application_name, held.environment_name
            ),
        ),
        (
            "the environment of the other application by name",
            format!(
                "/applications/{}/environments/{}",
                elsewhere.application_name, elsewhere.environment_name
            ),
        ),
        (
            "the profile of the other application by id",
            format!(
                "/applications/{}/configurationprofiles/{}",
                elsewhere.application_id, elsewhere.profile_id
            ),
        ),
    ] {
        let answer = json_call(&router, "GET", &uri, &Value::Null).await;
        assert_eq!(
            answer.status,
            200,
            "case {case:?}: must still answer 200, got {} / {}",
            answer.status,
            String::from_utf8_lossy(&answer.body)
        );
    }
}

/// RULE: the ID lookup comes FIRST, so a token that is one resource's id and another
/// resource's name addresses the one holding the id.
#[tokio::test]
async fn an_identifier_resolves_as_an_id_before_it_resolves_as_a_name() {
    let router = router();
    let addressed_by_id = create_application(&router, "holds-the-id").await;
    // A second application NAMED after the first one's minted id, which the account allows
    // because an application's name only has to be unique among names.
    let named_after_it = create_application(&router, &addressed_by_id).await;
    assert_ne!(
        named_after_it, addressed_by_id,
        "the two applications must be distinct records"
    );

    let answer = json_call(
        &router,
        "GET",
        &format!("/applications/{addressed_by_id}"),
        &Value::Null,
    )
    .await;
    assert_eq!(
        answer.string("Id").as_deref(),
        Some(addressed_by_id.as_str()),
        "the token must resolve as an id before it resolves as a name, got {:?}",
        answer.json()
    );
    assert_eq!(
        answer.string("Name").as_deref(),
        Some("holds-the-id"),
        "the application reported must be the one holding the id, got {:?}",
        answer.json()
    );

    // The application NAMED after that id is still reachable by its own id, so neither
    // record shadows the other.
    let shadowed = json_call(
        &router,
        "GET",
        &format!("/applications/{named_after_it}"),
        &Value::Null,
    )
    .await;
    assert_eq!(
        shadowed.string("Name").as_deref(),
        Some(addressed_by_id.as_str()),
        "the application named after the id must still read back by its own id, got {:?}",
        shadowed.json()
    );
}
