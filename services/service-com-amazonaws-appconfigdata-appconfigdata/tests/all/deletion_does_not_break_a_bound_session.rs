// SPDX-License-Identifier: CC0-1.0

//! What a control-plane deletion does to a configuration session that is already
//! polling, driven end to end over HTTP.
//!
//! The rule under test is about every level of the AppConfig hierarchy and every poll of
//! a session, not about one deletion: a session's (application, environment,
//! configuration profile) triple is resolved when the session is STARTED, and a poll
//! re-resolves it to pick up a newer version in force — not to re-authorise the session.
//! So a record the control plane stops holding does not refuse the poll that follows the
//! removal, however long the client waited between polls: that poll is answered from the
//! resolution the session carries, which names the version the client already has, so it is
//! the modelled no-change 200 (empty `Configuration`, the configuration's content type, a
//! rotated `Next-Poll-Configuration-Token` and a poll interval). That first withdrawn poll
//! is what makes the refusal DUE, one withdrawal span later — a transition the CLOCK makes,
//! so it arrives whether or not anybody polls again — and from then on every poll of that
//! chain is refused with the modelled `ResourceNotFoundException` (404) naming the resource
//! that is gone. A session bound to no configuration at all has nothing to serve either way
//! and is refused at once, and `StartConfigurationSession` keeps resolving the triple live,
//! so a session started over a removed record is still refused.
//!
//! A poll served from a withdrawn binding carries the deadline it found rather than
//! recording a later one, which is what makes the refusal arrive however often the client
//! polls; a poll whose own resolution held owes no refusal at all.
//!
//! The rows differ along what the rule generalises over: WHICH level the control plane
//! removes (and how many at once), whether the session had already polled when the
//! removal happened, whether the session spelled its triple by name or by id, and the
//! form, content type and version label of the configuration it was serving. Each row
//! also checks the two halves that must NOT change: a session started after the removal is
//! judged against the control plane as it stands, and an untouched hierarchy's session
//! keeps polling throughout, so nothing here is an outage of the endpoint.
//!
//! Every request goes through the router harness the polling suite sets up
//! ([`super::configuration_session_poll::Emulator`]), which mounts AppConfigData and the
//! AppConfig control plane over one shared engine: the hierarchy is created, deployed and
//! deleted through AppConfig's own operations, exactly as an operator would. The one thing
//! no operation exposes is the clock, so the instants a row needs — how long the client has
//! been idle, and the refusal having fallen due — are moved in the store
//! (`backdate_last_poll`, `expire_withdrawal_deadline`) instead of being waited out.

use std::time::Duration;

use bytes::Bytes;
use service_com_amazonaws_appconfigdata_appconfigdata::state::DEFAULT_POLL_INTERVAL_IN_SECONDS;

use super::configuration_session_poll::{Emulator, Hierarchy};

/// How long each row's session has been idle when it polls after the removal: twice the
/// interval the service itself tells a client to poll at
/// (`state::DEFAULT_POLL_INTERVAL_IN_SECONDS`), so the answer below cannot depend on the
/// client having polled in the same instant as the removal. The wait is driven by
/// backdating the stored instant, never by sleeping.
const IDLE_FOR: Duration =
    Duration::from_secs(2 * DEFAULT_POLL_INTERVAL_IN_SECONDS.unsigned_abs() as u64);

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

/// What a poll of the removed triple must answer ONCE the withdrawal has fallen due: the
/// modelled `ResourceType` for the resource the control plane no longer holds, and the
/// input member `ReferencedBy` is keyed by where the refusal is about a resource a request
/// parameter named.
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

/// What a session STARTED after the removal must answer — the half of the rule that shows
/// `StartConfigurationSession` still resolves the triple against the control plane as it
/// stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AfterRemoval {
    /// `StartConfigurationSession` is refused with the modelled
    /// `ResourceNotFoundException` naming this `ResourceType`, and the `Message` is that
    /// level's fixed "<ResourceType> not found" sentence.
    SessionRefused { resource_type: &'static str },
    /// The triple still resolves, so the session is minted — and because it is bound to no
    /// configuration at all, its first poll is refused with this `ResourceType` straight
    /// away.
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

/// A session already bound to a configuration KEEPS POLLING across a control-plane
/// removal: the live, unexpired token it holds is served from the resolution the session
/// carries — the no-change 200 for a client that already has that version — whichever
/// level is removed, however the session spelled its triple, whether or not it had already
/// polled and however long it has been idle. That poll is what makes the refusal due; once
/// it has fallen due, every poll of that chain is refused with the modelled
/// `ResourceNotFoundException` naming the resource that is gone, while
/// `StartConfigurationSession` is judged against the live control plane throughout and an
/// untouched hierarchy's session is undisturbed.
#[tokio::test]
async fn a_removal_leaves_a_bound_session_polling_until_the_refusal_falls_due() {
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
        // yet, so the poll after the removal is its first and carries the content.
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

        // The row's session has been idle for twice the interval the service tells a client
        // to poll at, so what follows is the poll of a client honouring that cadence rather
        // than one that happened to poll in the same instant as the removal.
        emulator.backdate_last_poll(&token, IDLE_FOR);
        assert_eq!(
            emulator.withdrawal_deadline(&token),
            None,
            "case {name}: no poll has found the triple withdrawn yet, so the session can owe \
             no refusal"
        );

        // THE RULE, first half: the poll that follows the removal is SERVED from what the
        // session is bound to — the content it has not been given yet, or nothing at all
        // for a client that already has that version — and it rotates a successor token
        // the chain can go on with.
        let token = assert_served_from_its_binding(
            &emulator,
            &token,
            name,
            "the poll after the removal",
            case,
            !case.polled_before,
        )
        .await;

        // That poll is what made the refusal DUE: the successor carries the deadline...
        let deadline = emulator.withdrawal_deadline(&token).unwrap_or_else(|| {
            panic!(
                "case {name}: the poll that found the triple withdrawn must record when the \
                 refusal falls due"
            )
        });

        // ...and the chain keeps polling until it, carrying the SAME deadline rather than a
        // fresh one, so polling faster cannot postpone the refusal. The client now holds
        // that version, so this poll is the no-change answer whatever the row served above.
        emulator.postpone_withdrawal_deadline(&token);
        let postponed = emulator.withdrawal_deadline(&token);
        let token = assert_served_from_its_binding(
            &emulator,
            &token,
            name,
            "the next poll of the chain",
            case,
            false,
        )
        .await;
        assert_eq!(
            emulator.withdrawal_deadline(&token),
            postponed,
            "case {name}: a poll served from the binding must carry the deadline it found, \
             not record a later one (the deadline the removal set was {deadline:?})"
        );

        // THE RULE, second half: once the withdrawal has fallen due, the poll is refused,
        // naming the resource the control plane no longer holds...
        emulator.expire_withdrawal_deadline(&token);
        let withdrawn = Withdrawal::read(&emulator, &token).await;
        withdrawn.assert_is(
            name,
            "the poll once the withdrawal had fallen due",
            case.withdrawn,
            application,
            environment,
            profile,
        );

        // ...and that refusal is the STABLE answer, not a one-off: the same token
        // presented again is refused the same way, which is what lets a client's poll loop
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
        // refusal above to the removed records rather than to the endpoint — and it owes no
        // refusal, because its own resolution held.
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
        let control_deadline = emulator.withdrawal_deadline(&control_poll.next_token());
        assert_eq!(
            control_deadline, None,
            "case {name}: a poll whose own resolution held owes no refusal, got a deadline of \
             {control_deadline:?}"
        );

        // And a session started AFTER the removal is judged against the control plane as
        // it stands, which is where resolving the triple belongs.
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

/// Poll with `token` after the removal and assert the whole no-change answer the session's
/// binding owes, answering the successor token the poll rotated.
///
/// The payload is the only part that depends on the row: a session that has already been
/// served this version is given no content and no version label ("if the client already
/// has the latest version of the configuration data, this value is empty"), and one that
/// has not is given the deployed bytes and whatever label the version carries. The content
/// type describes the session's configuration either way, so it is always reported.
async fn assert_served_from_its_binding(
    emulator: &Emulator,
    token: &str,
    case_name: &str,
    poll_name: &str,
    case: &Case,
    expect_content: bool,
) -> String {
    let poll = emulator.get_latest_configuration(token).await;
    let text = poll.text();
    assert_eq!(
        poll.status, 200,
        "case {case_name}: {poll_name} expected 200, got {} body {text}",
        poll.status
    );
    let (expected_content, expected_label): (&[u8], Option<&str>) = if expect_content {
        (case.content, case.version_label)
    } else {
        (b"", None)
    };
    assert_eq!(
        poll.body.as_ref(),
        expected_content,
        "case {case_name}: {poll_name} expected the payload {:?}, got {:?}",
        String::from_utf8_lossy(expected_content),
        poll.body.as_ref()
    );
    assert_eq!(
        poll.header("content-type"),
        Some(case.content_type),
        "case {case_name}: {poll_name} expected content type {}, got {:?}",
        case.content_type,
        poll.header("content-type")
    );
    assert_eq!(
        poll.header("version-label"),
        expected_label,
        "case {case_name}: {poll_name} expected version label {expected_label:?}, got {:?}",
        poll.header("version-label")
    );
    let interval = poll.header("next-poll-interval-in-seconds");
    assert!(
        interval.is_some_and(|seconds| seconds.parse::<i64>().is_ok_and(|seconds| seconds > 0)),
        "case {case_name}: {poll_name} expected a positive poll interval, got {interval:?}"
    );
    assert!(
        poll.header("next-poll-configuration-token").is_some(),
        "case {case_name}: {poll_name} must rotate a successor token, got {text}"
    );
    poll.next_token()
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
/// [`AfterRemoval`]: an unresolvable level is refused by `StartConfigurationSession`, and a
/// session the triple still resolves for is minted bound to no configuration, which leaves
/// its first poll nothing to serve at once.
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
