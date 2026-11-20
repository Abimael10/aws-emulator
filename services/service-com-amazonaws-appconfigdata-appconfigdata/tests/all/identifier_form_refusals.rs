// SPDX-License-Identifier: CC0-1.0

//! What FORM `StartConfigurationSession` accepts for the three identifiers of a triple,
//! driven end to end over HTTP.
//!
//! The rule under test, which is about the form of an identifier and not about one
//! member, one vendor or one recorded value: each of `ApplicationIdentifier`,
//! `EnvironmentIdentifier` and `ConfigurationProfileIdentifier` is documented as "the ID
//! or the name" of its level, so a value written as an Amazon Resource Name is neither
//! and is refused on the REQUEST — the modelled `BadRequestException`, HTTP 400, naming
//! that shape in `x-amzn-errortype` and committing to NOTHING in the payload — before any
//! identifier is resolved against the control plane, so a malformed identifier never
//! becomes a `ResourceNotFoundException`. A value that is NOT in that form keeps taking
//! the resolution path: it resolves, or it is the 404 it always was.
//!
//! Every case drives real requests through the router harness the polling suite sets up
//! ([`super::configuration_session_poll::Emulator`]), which mounts AppConfigData and the
//! AppConfig control plane over one shared engine: the application, environment,
//! configuration profile and deployment each case needs are created through AppConfig's
//! own operations, so a case's resolvable identifiers are the control plane's own and the
//! ARNs are built over the ids it minted.

use super::configuration_session_poll::{is_modelled_token, Emulator, Hierarchy};

/// The modelled error this service answers input that fails its constraints with, and its
/// `@httpError` status.
const BAD_REQUEST_EXCEPTION: &str = "BadRequestException";
/// See [`BAD_REQUEST_EXCEPTION`].
const BAD_REQUEST_STATUS: u16 = 400;

/// The modelled error for an identifier that is well formed and resolves to nothing, and
/// its `@httpError` status.
const RESOURCE_NOT_FOUND_EXCEPTION: &str = "ResourceNotFoundException";
/// See [`RESOURCE_NOT_FOUND_EXCEPTION`].
const NOT_FOUND_STATUS: u16 = 404;

/// The three input member names, which `ReferencedBy` is keyed by — and which a refusal of
/// an identifier's FORM must name nowhere in its payload.
const APPLICATION_MEMBER: &str = "ApplicationIdentifier";
/// See [`APPLICATION_MEMBER`].
const ENVIRONMENT_MEMBER: &str = "EnvironmentIdentifier";
/// See [`APPLICATION_MEMBER`].
const PROFILE_MEMBER: &str = "ConfigurationProfileIdentifier";

/// How a case spells one identifier of the triple.
#[derive(Debug, Clone, Copy)]
enum Spelling {
    /// The level's real id, as the control plane minted it.
    Id,
    /// The level's real name.
    Name,
    /// The AppConfig ARN of the level's real record — the form a CloudFormation or CDK
    /// output hands a caller.
    Arn,
    /// Whatever text the case names, resolvable or not.
    Literal(&'static str),
}

/// What the request must be answered with.
#[derive(Debug, Clone, Copy)]
enum Expect {
    /// 201 and a minted token.
    Session,
    /// 400 `BadRequestException` with an empty payload. The members listed are the ones
    /// this request got wrong — which the answer must name NOWHERE, since the refusal
    /// asserts nothing about which parameter the service judged unreadable.
    BadRequest(&'static [&'static str]),
    /// 404 `ResourceNotFoundException` reporting this `ResourceType`.
    NotFound(&'static str),
}

/// One session request. The rows differ along what the rule generalises over: which
/// member carries the malformed value, how many do, the ARN's vendor, partition, Region,
/// account and resource form, and whether a value that is not an ARN still resolves.
struct Case {
    name: &'static str,
    application: Spelling,
    environment: Spelling,
    profile: Spelling,
    expect: Expect,
}

const CASES: &[Case] = &[
    Case {
        name: "ids and names only, so the session is minted",
        application: Spelling::Name,
        environment: Spelling::Name,
        profile: Spelling::Name,
        expect: Expect::Session,
    },
    Case {
        name: "the application named by its own AppConfig ARN",
        application: Spelling::Arn,
        environment: Spelling::Name,
        profile: Spelling::Name,
        expect: Expect::BadRequest(&[APPLICATION_MEMBER]),
    },
    Case {
        name: "the environment named by its own AppConfig ARN",
        application: Spelling::Name,
        environment: Spelling::Arn,
        profile: Spelling::Name,
        expect: Expect::BadRequest(&[ENVIRONMENT_MEMBER]),
    },
    Case {
        name: "the configuration profile named by its own AppConfig ARN, siblings by id",
        application: Spelling::Id,
        environment: Spelling::Id,
        profile: Spelling::Arn,
        expect: Expect::BadRequest(&[PROFILE_MEMBER]),
    },
    Case {
        name: "all three named by their own AppConfig ARNs",
        application: Spelling::Arn,
        environment: Spelling::Arn,
        profile: Spelling::Arn,
        expect: Expect::BadRequest(&[APPLICATION_MEMBER, ENVIRONMENT_MEMBER, PROFILE_MEMBER]),
    },
    Case {
        name: "an ARN of another vendor, partition-global with no Region or account",
        application: Spelling::Literal("arn:aws:s3:::my-configuration-bucket"),
        environment: Spelling::Name,
        profile: Spelling::Name,
        expect: Expect::BadRequest(&[APPLICATION_MEMBER]),
    },
    Case {
        name: "an ARN of another partition whose resource uses the type:id form",
        application: Spelling::Name,
        environment: Spelling::Literal(
            "arn:aws-us-gov:appconfig:us-gov-west-1:123456789012:environment:abc123",
        ),
        profile: Spelling::Name,
        expect: Expect::BadRequest(&[ENVIRONMENT_MEMBER]),
    },
    Case {
        name: "an ARN in one member and unresolvable text in the other two: the form wins",
        application: Spelling::Arn,
        environment: Spelling::Literal("no-such-environment"),
        profile: Spelling::Literal("no-such-profile"),
        expect: Expect::BadRequest(&[APPLICATION_MEMBER]),
    },
    Case {
        name: "two members by ARN, the third resolvable",
        application: Spelling::Name,
        environment: Spelling::Arn,
        profile: Spelling::Arn,
        expect: Expect::BadRequest(&[ENVIRONMENT_MEMBER, PROFILE_MEMBER]),
    },
    Case {
        name: "too few colon-separated fields to be an ARN, so it is resolved and missed",
        application: Spelling::Name,
        environment: Spelling::Name,
        profile: Spelling::Literal("arn:aws:appconfig"),
        expect: Expect::NotFound("ConfigurationProfile"),
    },
    Case {
        name: "six fields but no vendor, so it is resolved and missed",
        application: Spelling::Literal("arn:aws::us-west-2:123456789012:application/abc123"),
        environment: Spelling::Name,
        profile: Spelling::Name,
        expect: Expect::NotFound("Application"),
    },
    Case {
        name: "a value that merely begins like an ARN, so it is resolved and missed",
        application: Spelling::Literal("arnold"),
        environment: Spelling::Name,
        profile: Spelling::Name,
        expect: Expect::NotFound("Application"),
    },
    Case {
        name: "a CloudFormation logical id, so it is resolved and missed",
        application: Spelling::Name,
        environment: Spelling::Name,
        profile: Spelling::Literal("StackConfigurationProfile7AB3584E"),
        expect: Expect::NotFound("ConfigurationProfile"),
    },
];

/// An identifier written as an ARN is refused as input the service cannot use, with the
/// modelled 400 error whose payload asserts nothing about which member was at fault, and
/// nothing is minted — while a value that is not in that form still takes the resolution
/// path, serving the session or answering the 404 for the level that resolves to nothing.
#[tokio::test]
async fn an_identifier_in_arn_form_is_refused_as_input_rather_than_looked_up() {
    for case in CASES {
        let name = case.name;
        let emulator = Emulator::new();
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

        let application = spell(case.application, &hierarchy, Level::Application);
        let environment = spell(case.environment, &hierarchy, Level::Environment);
        let profile = spell(case.profile, &hierarchy, Level::Profile);
        let sessions_before = emulator.state().lock().sessions.len();

        let reply = emulator
            .post_json(
                "/configurationsessions",
                &format!(
                    r#"{{"ApplicationIdentifier":"{application}",
                          "EnvironmentIdentifier":"{environment}",
                          "ConfigurationProfileIdentifier":"{profile}"}}"#
                ),
            )
            .await;

        match case.expect {
            Expect::Session => {
                assert_eq!(
                    reply.status,
                    201,
                    "case {name}: expected 201, got {} body {}",
                    reply.status,
                    reply.text()
                );
                let token = reply.string("InitialConfigurationToken");
                assert!(
                    is_modelled_token(&token),
                    "case {name}: expected a well-formed token, got {token:?}"
                );
            }
            Expect::BadRequest(members) => {
                assert_eq!(
                    reply.status,
                    BAD_REQUEST_STATUS,
                    "case {name}: expected {BAD_REQUEST_STATUS}, got {} body {}",
                    reply.status,
                    reply.text()
                );
                assert_eq!(
                    reply.error_code(),
                    BAD_REQUEST_EXCEPTION,
                    "case {name}: expected {BAD_REQUEST_EXCEPTION}, got {}",
                    reply.text()
                );
                assert_eq!(
                    reply.header("x-amzn-errortype"),
                    Some(BAD_REQUEST_EXCEPTION),
                    "case {name}: expected the modelled error type header, got {:?}",
                    reply.header("x-amzn-errortype")
                );
                // The whole payload: the refusal commits to the status and the shape, and
                // asserts nothing about which parameter it could not read — so the
                // document is empty, whichever member carried the malformed value and
                // however many did.
                assert_eq!(
                    reply.json(),
                    serde_json::json!({}),
                    "case {name}: expected an empty refusal document, got {}",
                    reply.text()
                );
                // Named explicitly, because these are the members a per-member
                // attribution would have reported: no payload names any of them, and a
                // refusal of the INPUT reports no resource either — a client must not read
                // it as a resource that is merely absent.
                let text = reply.text();
                for member in members {
                    assert!(
                        !text.contains(member),
                        "case {name}: the refusal must not name {member}, got {text}"
                    );
                }
                let body = reply.json();
                assert!(
                    body["ResourceType"].is_null() && body["ReferencedBy"].is_null(),
                    "case {name}: an input refusal must report no missing resource, got {text}"
                );
            }
            Expect::NotFound(resource_type) => {
                assert_eq!(
                    reply.status,
                    NOT_FOUND_STATUS,
                    "case {name}: expected {NOT_FOUND_STATUS}, got {} body {}",
                    reply.status,
                    reply.text()
                );
                assert_eq!(
                    reply.error_code(),
                    RESOURCE_NOT_FOUND_EXCEPTION,
                    "case {name}: expected {RESOURCE_NOT_FOUND_EXCEPTION}, got {}",
                    reply.text()
                );
                let body = reply.json();
                assert_eq!(
                    body["ResourceType"].as_str(),
                    Some(resource_type),
                    "case {name}: expected the {resource_type} resource type, got {}",
                    reply.text()
                );
                assert_eq!(
                    body["Message"].as_str(),
                    Some(format!("{resource_type} not found").as_str()),
                    "case {name}: expected the level's fixed message, got {}",
                    reply.text()
                );
            }
        }

        let sessions_after = emulator.state().lock().sessions.len();
        let minted = match case.expect {
            Expect::Session => 1,
            _ => 0,
        };
        assert_eq!(
            sessions_after,
            sessions_before + minted,
            "case {name}: expected {minted} session(s) to be stored, store went from \
             {sessions_before} to {sessions_after}"
        );
    }
}

/// Which level of the hierarchy a spelling is for.
#[derive(Debug, Clone, Copy)]
enum Level {
    Application,
    Environment,
    Profile,
}

/// One identifier as a case spells it: the control plane's own id or name, the AppConfig
/// ARN of that record, or the case's own literal text.
fn spell(spelling: Spelling, hierarchy: &Hierarchy, level: Level) -> String {
    match spelling {
        Spelling::Id => match level {
            Level::Application => hierarchy.application_id.clone(),
            Level::Environment => hierarchy.environment_id.clone(),
            Level::Profile => hierarchy.profile_id.clone(),
        },
        Spelling::Name => match level {
            Level::Application => hierarchy.application_name.clone(),
            Level::Environment => hierarchy.environment_name.clone(),
            Level::Profile => hierarchy.profile_name.clone(),
        },
        // The AppConfig resource ARN forms, as that service's own `arnNamespace`
        // (`appconfig`) spells them: an application, and an environment or configuration
        // profile under it.
        Spelling::Arn => {
            let application = &hierarchy.application_id;
            let resource = match level {
                Level::Application => format!("application/{application}"),
                Level::Environment => format!(
                    "application/{application}/environment/{}",
                    hierarchy.environment_id
                ),
                Level::Profile => format!(
                    "application/{application}/configurationprofile/{}",
                    hierarchy.profile_id
                ),
            };
            engine::emulator_arn("appconfig", "us-west-2", &resource)
        }
        Spelling::Literal(literal) => literal.to_string(),
    }
}

/// The members each modelled error shape of this service declares, as
/// `smithy_query(query="shape",shape="BadRequestException,ResourceNotFoundException",
/// model_path="services/service-com-amazonaws-appconfigdata-appconfigdata/smithy-model/com-amazonaws-appconfigdata-appconfigdata.json")`
/// reports them. A failure's document may carry any subset of its shape's members, and
/// nothing outside it.
const BAD_REQUEST_MEMBERS: &[&str] = &["Message", "Reason", "Details"];
/// See [`BAD_REQUEST_MEMBERS`].
const RESOURCE_NOT_FOUND_MEMBERS: &[&str] = &["Message", "ResourceType", "ReferencedBy"];

/// Which operation a case calls, and with what.
enum Earn {
    /// `StartConfigurationSession` with this request document.
    Session(&'static str),
    /// `GetLatestConfiguration` with this configuration token.
    Poll(&'static str),
}

/// One failure this service answers: the call that earns it, the shape and status it must
/// be answered in, the members that shape declares, and whether the document must be
/// empty.
struct WireCase {
    name: &'static str,
    earn: Earn,
    code: &'static str,
    status: u16,
    members: &'static [&'static str],
    empty: bool,
}

/// An ARN of another vendor: a value in a form no identifier member accepts, written
/// without any id this hierarchy minted, so the row is a static document.
const AN_ARN: &str = "arn:aws:s3:::my-configuration-bucket";

/// An identifier past `Identifier`'s modelled `@length` 1..128 upper bound.
const TOO_LONG: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\
                        aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\
                        aaaaaaaa";

/// RULE: this service speaks `restJson1`, which names a failure's error SHAPE in the
/// `x-amzn-errortype` response header, so a failure's DOCUMENT carries that shape's own
/// modelled members and nothing else — never an in-body discriminator — and a refusal this
/// service attributes to nothing is the empty document exactly.
///
/// The rows differ along what the rule generalises over: which operation answered, which
/// error shape and status, how many members the shape declares, how many the answer fills,
/// and whether the refusal was decided by the handler, by the input schema or by a missing
/// member.
#[tokio::test]
async fn a_failure_names_its_shape_in_the_header_and_its_body_carries_only_modelled_members() {
    let cases: &[WireCase] = &[
        WireCase {
            name: "an identifier whose form is neither an id nor a name",
            earn: Earn::Session(
                r#"{"ApplicationIdentifier":"ARN_HERE",
                     "EnvironmentIdentifier":"Prod",
                     "ConfigurationProfileIdentifier":"Weights"}"#,
            ),
            code: BAD_REQUEST_EXCEPTION,
            status: BAD_REQUEST_STATUS,
            members: BAD_REQUEST_MEMBERS,
            empty: true,
        },
        WireCase {
            name: "an application that resolves to nothing",
            earn: Earn::Session(
                r#"{"ApplicationIdentifier":"NoSuchApplication",
                     "EnvironmentIdentifier":"Prod",
                     "ConfigurationProfileIdentifier":"Weights"}"#,
            ),
            code: RESOURCE_NOT_FOUND_EXCEPTION,
            status: NOT_FOUND_STATUS,
            members: RESOURCE_NOT_FOUND_MEMBERS,
            empty: false,
        },
        WireCase {
            name: "an identifier past the modelled length bound",
            earn: Earn::Session(
                r#"{"ApplicationIdentifier":"TOO_LONG_HERE",
                     "EnvironmentIdentifier":"Prod",
                     "ConfigurationProfileIdentifier":"Weights"}"#,
            ),
            code: BAD_REQUEST_EXCEPTION,
            status: BAD_REQUEST_STATUS,
            members: BAD_REQUEST_MEMBERS,
            empty: false,
        },
        WireCase {
            name: "a request that omits a required identifier",
            earn: Earn::Session(
                r#"{"ApplicationIdentifier":"MyApp","EnvironmentIdentifier":"Prod"}"#,
            ),
            code: BAD_REQUEST_EXCEPTION,
            status: BAD_REQUEST_STATUS,
            members: BAD_REQUEST_MEMBERS,
            empty: false,
        },
        WireCase {
            name: "a poll presenting a token no session answers to",
            earn: Earn::Poll("0000000000000000000000000000000000000000000000000000000000000000"),
            code: BAD_REQUEST_EXCEPTION,
            status: BAD_REQUEST_STATUS,
            members: BAD_REQUEST_MEMBERS,
            empty: false,
        },
    ];

    let emulator = Emulator::new();
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

    for case in cases {
        let name = case.name;
        let reply = match &case.earn {
            Earn::Session(document) => {
                let document = document
                    .replace("ARN_HERE", AN_ARN)
                    .replace("TOO_LONG_HERE", TOO_LONG);
                emulator.post_json("/configurationsessions", &document).await
            }
            Earn::Poll(token) => emulator.get_latest_configuration(token).await,
        };

        assert_eq!(
            reply.status,
            case.status,
            "case {name}: expected {}, got {} body {}",
            case.status,
            reply.status,
            reply.text()
        );
        assert_eq!(
            reply.header("x-amzn-errortype").map(|named| named
                .rsplit('#')
                .next()
                .unwrap_or(named)
                .to_string()),
            Some(case.code.to_string()),
            "case {name}: expected {} in the error type header, got {:?}",
            case.code,
            reply.header("x-amzn-errortype")
        );
        let document = reply.json();
        let object = document.as_object().unwrap_or_else(|| {
            panic!(
                "case {name}: expected a JSON object document, got {}",
                reply.text()
            )
        });
        let unmodelled: Vec<&str> = object
            .keys()
            .map(String::as_str)
            .filter(|member| !case.members.contains(member))
            .collect();
        assert!(
            unmodelled.is_empty(),
            "case {name}: the document carries {unmodelled:?}, which {} does not declare, in {}",
            case.code,
            reply.text()
        );
        if case.empty {
            assert!(
                object.is_empty(),
                "case {name}: expected an empty document, got {}",
                reply.text()
            );
        } else {
            assert!(
                !object.is_empty(),
                "case {name}: expected the shape's own members, got {}",
                reply.text()
            );
        }
    }
}
