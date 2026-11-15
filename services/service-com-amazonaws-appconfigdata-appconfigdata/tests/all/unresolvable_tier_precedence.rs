// SPDX-License-Identifier: CC0-1.0

//! WHICH tier of a session's triple is reported when more than one of them resolves to
//! nothing, driven end to end over HTTP.
//!
//! The rule under test, which is about the precedence of the three tiers and not about
//! one identifier spelling or one recorded value: `StartConfigurationSession` resolves
//! the application first, because the other two tiers are its children, and of those two
//! the configuration PROFILE — the thing that identifies the configuration a session is
//! over — before the environment. So a request in which several tiers are unresolvable is
//! refused for the OUTERMOST unresolvable tier, profile ahead of environment, with
//! `ResourceType` naming that tier, the fixed message "<ResourceType> not found", and
//! `ReferencedBy` keyed by that tier's own input member and valued with the identifier
//! that member carried.
//!
//! Every case drives real requests through the router harness the polling suite sets up
//! ([`super::configuration_session_poll::Emulator`]), which mounts AppConfigData and the
//! AppConfig control plane over one shared engine: the resolvable identifiers are the
//! control plane's own, created through AppConfig's operations, and the unresolvable ones
//! are values no record of that level answers to.

use super::configuration_session_poll::{is_modelled_token, Emulator};

/// The modelled error a tier that resolves to nothing is refused with, and its
/// `@httpError` status.
const RESOURCE_NOT_FOUND_EXCEPTION: &str = "ResourceNotFoundException";
/// See [`RESOURCE_NOT_FOUND_EXCEPTION`].
const NOT_FOUND_STATUS: u16 = 404;

/// One tier of the triple: the modelled `ResourceType` value it is reported as
/// (`RESOURCE_TYPE_VALUES` in `src/ops/types/shared_types.rs`) and the input member that
/// names it, which `ReferencedBy` is keyed by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tier {
    Application,
    Environment,
    Profile,
}

impl Tier {
    /// The modelled `ResourceType` value for this tier.
    fn resource_type(self) -> &'static str {
        match self {
            Tier::Application => "Application",
            Tier::Environment => "Environment",
            Tier::Profile => "ConfigurationProfile",
        }
    }

    /// The input member that names this tier.
    fn member(self) -> &'static str {
        match self {
            Tier::Application => "ApplicationIdentifier",
            Tier::Environment => "EnvironmentIdentifier",
            Tier::Profile => "ConfigurationProfileIdentifier",
        }
    }
}

/// One session request over a triple some of whose tiers resolve to nothing. The rows
/// differ along what the rule generalises over: which tiers are unresolvable, how many,
/// and the form of the unresolvable identifier (a CloudFormation logical id, an id-shaped
/// token, a path-like value, a name of the wrong level).
struct Case {
    name: &'static str,
    /// The tiers whose identifier no record answers to, each with the value the request
    /// carries for it. Every other tier is spelled with the control plane's own name.
    unresolvable: &'static [(Tier, &'static str)],
    /// The tier AWS reports as the missing resource.
    reported: Tier,
}

const CASES: &[Case] = &[
    Case {
        name: "the environment alone is unresolvable",
        unresolvable: &[(Tier::Environment, "StackEnvironment4F1C0A2B")],
        reported: Tier::Environment,
    },
    Case {
        name: "the configuration profile alone is unresolvable",
        unresolvable: &[(Tier::Profile, "StackConfigurationProfile7AB3584E")],
        reported: Tier::Profile,
    },
    Case {
        name: "the environment and the profile are both unresolvable: the profile is reported",
        unresolvable: &[
            (Tier::Environment, "StackEnvironment4F1C0A2B"),
            (Tier::Profile, "StackConfigurationProfile7AB3584E"),
        ],
        reported: Tier::Profile,
    },
    Case {
        name: "both children unresolvable, the identifiers id-shaped: the profile is reported",
        unresolvable: &[(Tier::Environment, "abcd1234"), (Tier::Profile, "wxyz7890")],
        reported: Tier::Profile,
    },
    Case {
        name: "the application and the environment are unresolvable: the application is reported",
        unresolvable: &[
            (Tier::Application, "StackApplication1D4E6F80"),
            (Tier::Environment, "StackEnvironment4F1C0A2B"),
        ],
        reported: Tier::Application,
    },
    Case {
        name: "the application and the profile are unresolvable: the application is reported",
        unresolvable: &[
            (Tier::Application, "StackApplication1D4E6F80"),
            (Tier::Profile, "some/path/like/value"),
        ],
        reported: Tier::Application,
    },
    Case {
        name: "all three tiers are unresolvable: the application is reported",
        unresolvable: &[
            (Tier::Application, "StackApplication1D4E6F80"),
            (Tier::Environment, "StackEnvironment4F1C0A2B"),
            (Tier::Profile, "StackConfigurationProfile7AB3584E"),
        ],
        reported: Tier::Application,
    },
    Case {
        name: "each child named with the OTHER child's name, so neither resolves",
        unresolvable: &[(Tier::Environment, "Weights"), (Tier::Profile, "Prod")],
        reported: Tier::Profile,
    },
];

/// A triple with more than one unresolvable tier is refused for the outermost one — the
/// application before its children, and the configuration profile before the environment
/// — naming that tier in `ResourceType`, in the fixed message and in the one
/// `ReferencedBy` key, with the identifier the request carried for it. The same triple
/// spelled with the control plane's own names is served, so each row's refusal is about
/// the unresolvable tiers and nothing else.
#[tokio::test]
async fn the_outermost_unresolvable_tier_is_the_one_reported() {
    for case in CASES {
        let name = case.name;
        let emulator = Emulator::new();
        // The environment is named "Prod" and the configuration profile "Weights", so a
        // row may name one child with the other's name and have neither resolve.
        let hierarchy = emulator.create_hierarchy("MyApp", "Prod", "Weights").await;
        emulator
            .deploy(
                &hierarchy,
                &hierarchy.profile_id,
                br#"{"limit":10}"#,
                "application/json",
                None,
            )
            .await;

        // Positive control: spelled with the control plane's own names, the very same
        // triple is served a session.
        let control = emulator
            .session_token(
                &hierarchy.application_name,
                &hierarchy.environment_name,
                &hierarchy.profile_name,
            )
            .await;
        assert!(
            is_modelled_token(&control),
            "case {name}: the resolvable triple must be given a session, got {control:?}"
        );

        let identifier = |tier: Tier| -> String {
            match case
                .unresolvable
                .iter()
                .find(|(unresolvable, _)| *unresolvable == tier)
            {
                Some((_, value)) => (*value).to_string(),
                None => match tier {
                    Tier::Application => hierarchy.application_name.clone(),
                    Tier::Environment => hierarchy.environment_name.clone(),
                    Tier::Profile => hierarchy.profile_name.clone(),
                },
            }
        };
        let application = identifier(Tier::Application);
        let environment = identifier(Tier::Environment);
        let profile = identifier(Tier::Profile);
        let sessions_before = emulator.state().lock().sessions.len();

        let refused = emulator
            .post_json(
                "/configurationsessions",
                &format!(
                    r#"{{"ApplicationIdentifier":"{application}",
                          "EnvironmentIdentifier":"{environment}",
                          "ConfigurationProfileIdentifier":"{profile}"}}"#
                ),
            )
            .await;
        assert_eq!(
            refused.status,
            NOT_FOUND_STATUS,
            "case {name}: expected {NOT_FOUND_STATUS}, got {} body {}",
            refused.status,
            refused.text()
        );
        assert_eq!(
            refused.error_code(),
            RESOURCE_NOT_FOUND_EXCEPTION,
            "case {name}: expected {RESOURCE_NOT_FOUND_EXCEPTION}, got {}",
            refused.text()
        );
        let reported = case.reported;
        let body = refused.json();
        assert_eq!(
            body["ResourceType"].as_str(),
            Some(reported.resource_type()),
            "case {name}: expected the {} tier reported, got {}",
            reported.resource_type(),
            refused.text()
        );
        assert_eq!(
            body["Message"].as_str(),
            Some(format!("{} not found", reported.resource_type()).as_str()),
            "case {name}: expected the tier's whole fixed message, got {}",
            refused.text()
        );
        // `ReferencedBy` is "a map indicating which parameters in the request reference
        // the resource that was not found": the reported tier's own member, carrying the
        // identifier the request gave it, and no other.
        let referenced_by = body["ReferencedBy"]
            .as_object()
            .unwrap_or_else(|| panic!("case {name}: no ReferencedBy in {}", refused.text()));
        let reported_identifier = identifier(reported);
        assert_eq!(
            referenced_by
                .get(reported.member())
                .and_then(serde_json::Value::as_str),
            Some(reported_identifier.as_str()),
            "case {name}: expected ReferencedBy[{}] to carry the identifier the request \
             sent, got {}",
            reported.member(),
            refused.text()
        );
        let mut keys: Vec<&str> = referenced_by.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec![reported.member()],
            "case {name}: expected only the reported tier's member in ReferencedBy, got {}",
            refused.text()
        );
        assert_eq!(
            emulator.state().lock().sessions.len(),
            sessions_before,
            "case {name}: a refused session must persist nothing"
        );
    }
}
