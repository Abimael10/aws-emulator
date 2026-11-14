// SPDX-License-Identifier: CC0-1.0

//! What a control-plane deletion does to a configuration session that is already
//! polling, driven end to end over HTTP.
//!
//! The rule under test is about every level of the AppConfig hierarchy and every poll of
//! a session, not about one deletion: a session is over LIVE control-plane state, so
//! every poll resolves the session's (application, environment, configuration profile)
//! triple against that store as it stands at the instant of the poll. Whatever the
//! control plane no longer holds — a level of the hierarchy, or the configuration version
//! the in-force deployment names — leaves the poll nothing to serve, and the poll is
//! refused with the modelled `ResourceNotFoundException` (404) naming the resource that
//! is gone. That is the same answer `StartConfigurationSession` gives for the same
//! triple, so what a session is served never depends on when it was started: a token
//! bounds a poll in TIME, it is not a standing grant over withdrawn state.
//!
//! The rows differ along what the rule generalises over: WHICH level the control plane
//! removes (and how many at once), whether the session had already polled when the
//! removal happened, whether the session spelled its triple by name or by id, and the
//! form, content type and version label of the configuration it was serving. Each row
//! also checks the two halves that must NOT change: a session started after the removal
//! is judged the same way (that is what keeps the refusal a property of the state rather
//! than of the token), and an untouched hierarchy's session keeps polling throughout, so
//! the refusal is scoped to the removed records rather than being an outage of the
//! endpoint.
//!
//! Every request goes through the router harness the polling suite sets up
//! ([`super::configuration_session_poll::Emulator`]), which mounts AppConfigData and the
//! AppConfig control plane over one shared engine: the hierarchy is created, deployed and
//! deleted through AppConfig's own operations, exactly as an operator would.

use bytes::Bytes;

use super::configuration_session_poll::{Emulator, Hierarchy};

/// One level of the hierarchy the control plane removes. AppConfig does not cascade the
/// resources a container owns, so a row that reaches a container lists the children it
/// has to remove first, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Removal {
    /// Every hosted configuration version the configuration profile holds.
    HostedVersions,
    ConfigurationProfile,
    Environment,
    Application,
}

/// What a poll of the removed triple must answer: the modelled `ResourceType` for the
/// resource the control plane no longer holds, and the input member `ReferencedBy` is
/// keyed by where the refusal is about a resource a request parameter named.
///
/// The `ResourceType` values are the five the model's `ResourceType` enum declares
/// (Application, ConfigurationProfile, Deployment, Environment, Configuration), listed in
/// `src/ops/types/shared_types.rs` as `RESOURCE_TYPE_VALUES`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Withdrawn {
    resource_type: &'static str,
    /// `None` where the missing resource is the CONFIGURATION itself, which no
    /// identifier of the session names.
    referenced_by: Option<&'static str>,
}

/// What a session STARTED after the removal must answer — the half of the rule that
/// shows the refusal belongs to the state and not to the age of a token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AfterRemoval {
    /// `StartConfigurationSession` is refused with the modelled
    /// `ResourceNotFoundException` naming this `ResourceType`, and the `Message` is that
    /// level's fixed "<ResourceType> not found" sentence.
    SessionRefused { resource_type: &'static str },
    /// The triple still resolves, so the session is minted — and its first poll is
    /// refused with this `ResourceType`, exactly as the withdrawn session's poll is.
    PollRefused { resource_type: &'static str },
}

/// One removal, and the configuration the victim session was being served when it
/// happened.
struct Case {
    name: &'static str,
    /// What the control plane removes, in the order it has to.
    removed: &'static [Removal],
    /// Whether the session had already polled once (so it holds the latest version) when
    /// the removal happened.
    polled_before: bool,
    /// Whether the session spelled its triple by id rather than by name — each
    /// identifier is "the ID or the name" of its level, and `ReferencedBy` echoes the
    /// spelling the session used.
    by_id: bool,
    content: &'static [u8],
    content_type: &'static str,
    version_label: Option<&'static str>,
    withdrawn: Withdrawn,
    after: AfterRemoval,
}

const CASES: &[Case] = &[
    Case {
        name: "the hosted configuration version is deleted before the session's first poll",
        removed: &[Removal::HostedVersions],
        polled_before: false,
        by_id: false,
        content: br#"{"feature":"on"}"#,
        content_type: "application/json",
        version_label: Some("v-one"),
        withdrawn: Withdrawn {
            resource_type: "Configuration",
            referenced_by: None,
        },
        after: AfterRemoval::PollRefused {
            resource_type: "Configuration",
        },
    },
    Case {
        name: "the hosted configuration version is deleted after a poll",
        removed: &[Removal::HostedVersions],
        polled_before: true,
        by_id: false,
        content: b"feature: on\n",
        content_type: "application/x-yaml",
        version_label: None,
        withdrawn: Withdrawn {
            resource_type: "Configuration",
            referenced_by: None,
        },
        after: AfterRemoval::PollRefused {
            resource_type: "Configuration",
        },
    },
    Case {
        name: "the configuration profile is deleted after a poll",
        removed: &[Removal::HostedVersions, Removal::ConfigurationProfile],
        polled_before: true,
        by_id: false,
        content: br#"{"victim":true}"#,
        content_type: "application/json",
        version_label: Some("release-1"),
        withdrawn: Withdrawn {
            resource_type: "ConfigurationProfile",
            referenced_by: Some("ConfigurationProfileIdentifier"),
        },
        after: AfterRemoval::SessionRefused {
            resource_type: "ConfigurationProfile",
        },
    },
    Case {
        name: "the configuration profile is deleted before the session's first poll",
        removed: &[Removal::HostedVersions, Removal::ConfigurationProfile],
        polled_before: false,
        by_id: true,
        content: &[0x00, 0x9f, 0x92, 0x96, 0xff],
        content_type: "application/octet-stream",
        version_label: None,
        withdrawn: Withdrawn {
            resource_type: "ConfigurationProfile",
            referenced_by: Some("ConfigurationProfileIdentifier"),
        },
        after: AfterRemoval::SessionRefused {
            resource_type: "ConfigurationProfile",
        },
    },
    Case {
        name: "the environment is deleted after a poll",
        removed: &[Removal::Environment],
        polled_before: true,
        by_id: false,
        content: b"on",
        content_type: "text/plain",
        version_label: Some("flag-2"),
        withdrawn: Withdrawn {
            resource_type: "Environment",
            referenced_by: Some("EnvironmentIdentifier"),
        },
        after: AfterRemoval::SessionRefused {
            resource_type: "Environment",
        },
    },
    Case {
        name: "the environment is deleted before the session's first poll, named by id",
        removed: &[Removal::Environment],
        polled_before: false,
        by_id: true,
        content: b"feature: off\nextra: 2\n",
        content_type: "application/x-yaml",
        version_label: None,
        withdrawn: Withdrawn {
            resource_type: "Environment",
            referenced_by: Some("EnvironmentIdentifier"),
        },
        after: AfterRemoval::SessionRefused {
            resource_type: "Environment",
        },
    },
    Case {
        name: "the whole hierarchy is deleted after a poll",
        removed: &[
            Removal::HostedVersions,
            Removal::ConfigurationProfile,
            Removal::Environment,
            Removal::Application,
        ],
        polled_before: true,
        by_id: false,
        content: &[0x01, 0x02, 0x03, 0x04, 0x05, 0x06],
        content_type: "application/octet-stream",
        version_label: Some("binary-2"),
        withdrawn: Withdrawn {
            // Resolved outermost first, so the application is the level reported.
            resource_type: "Application",
            referenced_by: Some("ApplicationIdentifier"),
        },
        after: AfterRemoval::SessionRefused {
            resource_type: "Application",
        },
    },
];

/// The configuration of the hierarchy no row touches, whose session is the positive
/// control: different bytes from every victim row, so serving the wrong session's
/// configuration is visible.
const CONTROL_CONTENT: &[u8] = br#"{"control":true}"#;

/// A session polling a configuration is WITHDRAWN as soon as the control plane stops
/// holding what it polls: the live, unexpired token it already holds is refused with the
/// modelled `ResourceNotFoundException` naming the resource that is gone, whichever level
/// is removed, however the session spelled its triple and whether or not it had already
/// polled — while a session started afterwards is judged exactly the same way, and an
/// untouched hierarchy's session is undisturbed.
#[tokio::test]
async fn a_removal_withdraws_a_session_already_polling_a_configuration() {
    for case in CASES {
        let name = case.name;
        let emulator = Emulator::new();

        // The positive control: a second hierarchy, polling throughout, that no removal
        // in this row touches.
        let control = emulator
            .create_hierarchy("ControlApp", "ControlEnvironment", "ControlProfile")
            .await;
        emulator
            .deploy(
                &control,
                &control.profile_id,
                CONTROL_CONTENT,
                "application/json",
                None,
            )
            .await;
        let control_token = emulator
            .session_token(
                &control.application_name,
                &control.environment_name,
                &control.profile_name,
            )
            .await;
        let control_poll = emulator.get_latest_configuration(&control_token).await;
        assert_eq!(
            control_poll.status,
            200,
            "case {name}: the control session's first poll expected 200, got {} body {}",
            control_poll.status,
            control_poll.text()
        );
        assert_eq!(
            control_poll.body.as_ref(),
            CONTROL_CONTENT,
            "case {name}: the control session must be served its own configuration, got {:?}",
            control_poll.body.as_ref()
        );
        let control_token = control_poll.next_token();

        // The victim: deployed, then a session started over it with the spelling the row
        // asks for, so the removal below takes away the records that spelling resolves
        // through.
        let victim = emulator
            .create_hierarchy("VictimApp", "VictimEnvironment", "VictimProfile")
            .await;
        emulator
            .deploy(
                &victim,
                &victim.profile_id,
                case.content,
                case.content_type,
                case.version_label,
            )
            .await;
        let (application, environment, profile) = spelled(&victim, case.by_id);
        let token = emulator
            .session_token(application, environment, profile)
            .await;

        // A row that polls first is served the deployed content and holds the latest
        // version when the removal happens; a row that does not has been served nothing
        // yet.
        let token = if case.polled_before {
            let poll = emulator.get_latest_configuration(&token).await;
            assert_eq!(
                poll.status,
                200,
                "case {name}: the poll before the removal expected 200, got {} body {}",
                poll.status,
                poll.text()
            );
            assert_eq!(
                poll.body.as_ref(),
                case.content,
                "case {name}: the poll before the removal must serve the deployed \
                 content, got {:?}",
                poll.body.as_ref()
            );
            poll.next_token()
        } else {
            token
        };

        remove(&emulator, &victim, case.removed).await;

        // THE RULE: the token is live and unexpired, and the poll is refused anyway —
        // what it polls is no longer there to serve.
        let withdrawn = Withdrawal::read(&emulator, &token).await;
        withdrawn.assert_is(
            name,
            "the poll after the removal",
            case.withdrawn,
            application,
            environment,
            profile,
        );

        // The refusal is the STABLE answer, not a one-off: the same token presented
        // again is refused the same way, which is what lets a client's poll loop
        // converge on it.
        let again = Withdrawal::read(&emulator, &token).await;
        again.assert_is(
            name,
            "the next poll of the chain",
            case.withdrawn,
            application,
            environment,
            profile,
        );

        // The untouched hierarchy's session is undisturbed, which is what scopes the
        // refusal above to the removed records rather than to the endpoint.
        let control_poll = emulator.get_latest_configuration(&control_token).await;
        assert_eq!(
            control_poll.status,
            200,
            "case {name}: the control session's poll expected 200, got {} body {}",
            control_poll.status,
            control_poll.text()
        );
        assert!(
            control_poll.body.is_empty(),
            "case {name}: the control session holds the latest version, so its poll must \
             serve no content, got {:?}",
            control_poll.body.as_ref()
        );

        // And a session started AFTER the removal is judged the same way the withdrawn
        // one is: the refusal is a property of the state, not of how old a token is.
        assert_after_removal(&emulator, &victim, case).await;
    }
}

/// The victim's triple as the row's session spells it: every identifier is "the ID or the
/// name" of its level.
fn spelled(victim: &Hierarchy, by_id: bool) -> (&str, &str, &str) {
    if by_id {
        (
            &victim.application_id,
            &victim.environment_id,
            &victim.profile_id,
        )
    } else {
        (
            &victim.application_name,
            &victim.environment_name,
            &victim.profile_name,
        )
    }
}

/// One refusal a withdrawn poll answered with, reduced to what the rule is about.
struct Withdrawal {
    status: u16,
    error_type_header: Option<String>,
    rotated: bool,
    body: serde_json::Value,
    text: String,
}

impl Withdrawal {
    async fn read(emulator: &Emulator, token: &str) -> Self {
        let reply = emulator.get_latest_configuration(token).await;
        Self {
            status: reply.status,
            error_type_header: reply.header("x-amzn-errortype").map(str::to_string),
            rotated: reply.header("next-poll-configuration-token").is_some(),
            body: reply.json(),
            text: reply.text(),
        }
    }

    /// The whole refusal: 404 `ResourceNotFoundException` naming the resource that is
    /// gone, with no configuration served and no successor token minted.
    fn assert_is(
        &self,
        case: &str,
        poll: &str,
        expected: Withdrawn,
        application: &str,
        environment: &str,
        profile: &str,
    ) {
        assert_eq!(
            self.status, 404,
            "case {case}: {poll} expected 404, got {} body {}",
            self.status, self.text
        );
        assert_eq!(
            self.body["__type"].as_str(),
            Some("ResourceNotFoundException"),
            "case {case}: {poll} expected ResourceNotFoundException, got {}",
            self.text
        );
        assert_eq!(
            self.error_type_header.as_deref(),
            Some("ResourceNotFoundException"),
            "case {case}: {poll} expected the error type header, got {:?}",
            self.error_type_header
        );
        assert_eq!(
            self.body["ResourceType"].as_str(),
            Some(expected.resource_type),
            "case {case}: {poll} expected ResourceType {}, got {}",
            expected.resource_type,
            self.text
        );
        assert!(
            !self.rotated,
            "case {case}: {poll} serves nothing, so it must rotate no token, got {}",
            self.text
        );
        match expected.referenced_by {
            // A missing level of the hierarchy is named by the identifier the session
            // referenced it with, in the spelling the session used.
            Some(member) => {
                let identifier = match member {
                    "ApplicationIdentifier" => application,
                    "EnvironmentIdentifier" => environment,
                    _ => profile,
                };
                assert_eq!(
                    self.body["ReferencedBy"][member].as_str(),
                    Some(identifier),
                    "case {case}: {poll} expected ReferencedBy[{member}] to name {identifier}, \
                     got {}",
                    self.text
                );
                assert_eq!(
                    self.body["Message"].as_str(),
                    Some(format!("{} not found", expected.resource_type).as_str()),
                    "case {case}: {poll} expected the level's fixed not-found sentence, got {}",
                    self.text
                );
            }
            // The CONFIGURATION itself is what is missing, and no identifier of the
            // session names it: the message says which configuration profile has
            // nothing to serve.
            None => {
                assert!(
                    self.body["ReferencedBy"].is_null(),
                    "case {case}: {poll} names no resource parameter, so ReferencedBy must \
                     be absent, got {}",
                    self.text
                );
                assert!(
                    self.body["Message"]
                        .as_str()
                        .is_some_and(|message| message.contains(profile)),
                    "case {case}: {poll} expected a Message naming {profile}, got {}",
                    self.text
                );
            }
        }
    }
}

/// Remove each level the row names, through AppConfig's own delete operations, and read
/// the removal back so what follows is asserted against a deletion the control plane
/// really performed.
async fn remove(emulator: &Emulator, victim: &Hierarchy, removed: &[Removal]) {
    let application_id = &victim.application_id;
    let environment_id = &victim.environment_id;
    let profile_id = &victim.profile_id;
    for removal in removed {
        let paths = match removal {
            // A profile OWNS its hosted configuration versions and AppConfig does not
            // cascade them away, so each version the control plane reports is deleted by
            // the number IT reports.
            Removal::HostedVersions => hosted_version_numbers(emulator, victim)
                .await
                .into_iter()
                .map(|number| {
                    format!(
                        "/applications/{application_id}/configurationprofiles/{profile_id}\
                         /hostedconfigurationversions/{number}"
                    )
                })
                .collect(),
            Removal::ConfigurationProfile => vec![format!(
                "/applications/{application_id}/configurationprofiles/{profile_id}"
            )],
            Removal::Environment => vec![format!(
                "/applications/{application_id}/environments/{environment_id}"
            )],
            Removal::Application => vec![format!("/applications/{application_id}")],
        };
        for path in paths {
            let deleted = emulator
                .send("DELETE", &path, "application/json", &[], Bytes::new())
                .await;
            assert_eq!(
                deleted.status,
                204,
                "DELETE {path}: expected 204, got {} body {}",
                deleted.status,
                deleted.text()
            );
            let read_back = emulator
                .send("GET", &path, "application/json", &[], Bytes::new())
                .await;
            assert_eq!(
                read_back.status,
                404,
                "GET {path} after its delete: expected 404, got {} body {}",
                read_back.status,
                read_back.text()
            );
        }
    }
}

/// The hosted configuration version numbers the control plane reports for the victim's
/// configuration profile.
async fn hosted_version_numbers(emulator: &Emulator, victim: &Hierarchy) -> Vec<i64> {
    let application_id = &victim.application_id;
    let profile_id = &victim.profile_id;
    let listed = emulator
        .send(
            "GET",
            &format!(
                "/applications/{application_id}/configurationprofiles/{profile_id}\
                 /hostedconfigurationversions"
            ),
            "application/json",
            &[],
            Bytes::new(),
        )
        .await;
    assert_eq!(
        listed.status,
        200,
        "ListHostedConfigurationVersions: expected 200, got {} body {}",
        listed.status,
        listed.text()
    );
    listed.json()["Items"]
        .as_array()
        .unwrap_or_else(|| panic!("no Items in {}", listed.text()))
        .iter()
        .map(|item| {
            item["VersionNumber"]
                .as_i64()
                .unwrap_or_else(|| panic!("no VersionNumber in {item}"))
        })
        .collect()
}

/// Start a fresh session over the victim's triple after the removal and assert the row's
/// [`AfterRemoval`]: an unresolvable level is refused by `StartConfigurationSession`, and
/// a session the triple still resolves for is minted and then has nothing to serve —
/// which is the same verdict the already-polling session's token now gets.
async fn assert_after_removal(emulator: &Emulator, victim: &Hierarchy, case: &Case) {
    let name = case.name;
    let (application, environment, profile) = spelled(victim, case.by_id);
    let started = emulator
        .post_json(
            "/configurationsessions",
            &format!(
                r#"{{"ApplicationIdentifier":"{application}",
                      "EnvironmentIdentifier":"{environment}",
                      "ConfigurationProfileIdentifier":"{profile}"}}"#
            ),
        )
        .await;
    match case.after {
        AfterRemoval::SessionRefused { resource_type } => {
            assert_eq!(
                started.status,
                404,
                "case {name}: a session over the removed triple expected 404, got {} body {}",
                started.status,
                started.text()
            );
            assert_eq!(
                started.error_code(),
                "ResourceNotFoundException",
                "case {name}: expected ResourceNotFoundException, got {}",
                started.text()
            );
            assert_eq!(
                started.json()["ResourceType"].as_str(),
                Some(resource_type),
                "case {name}: expected ResourceType {resource_type}, got {}",
                started.text()
            );
            assert_eq!(
                started.json()["Message"].as_str(),
                Some(format!("{resource_type} not found").as_str()),
                "case {name}: expected the level's fixed not-found sentence, got {}",
                started.text()
            );
        }
        AfterRemoval::PollRefused { resource_type } => {
            assert_eq!(
                started.status,
                201,
                "case {name}: the triple still resolves, so the session expected 201, got {} \
                 body {}",
                started.status,
                started.text()
            );
            let token = started.string("InitialConfigurationToken");
            let poll = emulator.get_latest_configuration(&token).await;
            assert_eq!(
                poll.status,
                404,
                "case {name}: a session with nothing to serve expected 404, got {} body {}",
                poll.status,
                poll.text()
            );
            assert_eq!(
                poll.error_code(),
                "ResourceNotFoundException",
                "case {name}: expected ResourceNotFoundException, got {}",
                poll.text()
            );
            assert_eq!(
                poll.json()["ResourceType"].as_str(),
                Some(resource_type),
                "case {name}: expected ResourceType {resource_type}, got {}",
                poll.text()
            );
        }
    }
}
