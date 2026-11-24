// SPDX-License-Identifier: CC0-1.0

//! What this service refuses about a REQUEST, before it reads or writes
//! anything: the idempotency token and the name a create needs, the grammar of
//! the resource ids a path carries, how a value outside a modelled enum is
//! worded, the page size and continuation token a listing admits, and the IAM
//! role a channel may run as.
//!
//! Every suite is a TABLE spanning the class the rule generalises over rather
//! than the one recorded request: the operation, the member, the spelling the
//! value arrives in, and the position in the request the value sits at. Every
//! test drives the service the way a client does, through
//! `MultiServiceRouter::dispatch` with real HTTP requests, so what is asserted
//! is the emulator's wire behaviour rather than an internal function's return
//! value.
//!
//! The role suite mounts IAM beside MediaLive on ONE engine, because resolving a
//! channel's `RoleArn` is a call into this emulator's own IAM.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_medialive_medialive::state::{
    invalid_role_arn_message, max_results_above_ceiling_message, non_empty_name_message,
    non_empty_request_id_message, request_family, INVALID_NEXT_TOKEN_MESSAGE,
    MAX_LIST_PAGE_SIZE, MAX_RESULTS_BELOW_FLOOR_MESSAGE, NON_NUMERIC_ID_MESSAGE,
};
use smithy_server_runtime::{EmbeddedEmulator, MultiServiceRouter};

use crate::support::channel_encoder_settings;
use crate::wire::{call, call_raw, error_code, error_message, medialive, DEFAULT_REGION};

/// The service principal a channel's role has to let assume it.
pub(crate) const MEDIALIVE_PRINCIPAL: &str = "medialive.amazonaws.com";

/// A service principal that is NOT this service, for a role that exists and
/// trusts somebody else.
pub(crate) const OTHER_PRINCIPAL: &str = "lambda.amazonaws.com";

/// A channel create body that carries everything EXCEPT the member a row is
/// about, so a refusal is about that member and nothing else.
fn channel_body(name: &str) -> Value {
    json!({ "name": name, "encoderSettings": channel_encoder_settings() })
}

/// Dispatch one request whose body is sent EXACTLY as written — the harness fills
/// no idempotency token for it — so a row about an absent or empty `RequestId`
/// says so itself.
async fn call_verbatim(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let mut body = body;
    if let Some(Value::Object(members)) = body.as_mut() {
        // An explicit null is how the harness is told to send no member at all;
        // a row that wants a token names it.
        members.entry("requestId").or_insert(Value::Null);
    }
    let (status, bytes, _headers) =
        call_raw(router, DEFAULT_REGION, method, uri, body, &[]).await;
    let parsed = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// Rule: a create that mints a resource of a family carries the idempotency
/// token a retry of it is keyed on, and one that names none is refused with the
/// operation's modelled 400 naming that family — whatever else about the request
/// is wrong.
///
/// The rows span the OPERATION (every create of this service that takes a
/// `RequestId` for a channel or an input), the family the message names, and the
/// spellings of "no token" a client can send (the member absent, and the member
/// present but empty).
#[tokio::test]
async fn a_create_with_no_idempotency_token_is_refused_naming_its_family() {
    // (case, method, uri, the rest of the body, the family the message names)
    let rows: [(&str, &str, Value, &str); 2] = [
        (
            "CreateChannel",
            "/prod/channels",
            channel_body("tokenless"),
            request_family::CHANNEL,
        ),
        (
            "CreateInput",
            "/prod/inputs",
            json!({ "name": "tokenless", "type": "URL_PULL" }),
            request_family::INPUT,
        ),
    ];

    for (case, uri, body, family) in rows {
        for (token_case, token) in [("absent", None), ("empty", Some("")), ("blank", Some("  "))] {
            let router = medialive();
            let mut sent = body.clone();
            sent["requestId"] = match token {
                Some(value) => json!(value),
                None => Value::Null,
            };
            let (status, refused) = call_verbatim(&router, "POST", uri, Some(sent)).await;
            let case = format!("{case} with a {token_case} requestId");
            assert_eq!(
                status, 400,
                "case {case:?}: expected 400, got {status}: {refused}"
            );
            assert_eq!(
                error_code(status, &refused),
                "BadRequestException",
                "case {case:?}: expected the modelled BadRequestException, got {refused}"
            );
            assert_eq!(
                error_message(&refused),
                non_empty_request_id_message(family),
                "case {case:?}: expected the token refusal of the {family} family, got {:?}",
                error_message(&refused)
            );
        }
    }

    // The same rule for the create that mints an input FOR another input: the
    // family is the input's, so the wording is the input family's too.
    let router = medialive();
    let (status, refused) =
        call_verbatim(&router, "POST", "/prod/inputs/1000000/partners", Some(json!({}))).await;
    assert_eq!(
        status, 400,
        "case \"CreatePartnerInput with no requestId\": expected 400, got {status}: {refused}"
    );
    assert_eq!(
        error_message(&refused),
        non_empty_request_id_message(request_family::INPUT),
        "case \"CreatePartnerInput with no requestId\": expected the input family's token \
         refusal, got {:?}",
        error_message(&refused)
    );
}

/// Rule: a channel create names the channel, and one that does not is refused
/// with the modelled 400 naming the channel family — ahead of the token, so a
/// request missing both is told about the name.
#[tokio::test]
async fn a_channel_create_with_no_name_is_refused_naming_the_channel_family() {
    // (case, the name the body carries, whether the body also carries a token)
    let rows: [(&str, Option<&str>, bool); 4] = [
        ("no name member", None, true),
        ("an empty name", Some(""), true),
        ("no name and no token", None, false),
        ("an empty name and no token", Some(""), false),
    ];

    for (case, name, tokened) in rows {
        let router = medialive();
        let mut body = json!({ "encoderSettings": channel_encoder_settings() });
        if let Some(value) = name {
            body["name"] = json!(value);
        }
        if tokened {
            body["requestId"] = json!(format!("token-{case}"));
        }
        let (status, refused) = call_verbatim(&router, "POST", "/prod/channels", Some(body)).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected 400, got {status}: {refused}"
        );
        assert_eq!(
            error_message(&refused),
            non_empty_name_message(request_family::CHANNEL),
            "case {case:?}: expected the channel name refusal, got {:?}",
            error_message(&refused)
        );
    }
}

/// Rule: a request value outside a modelled enum is refused with MediaLive's own
/// `BadRequestException` (400) worded for the request FAMILY the operation
/// belongs to — a channel request quotes the request it could not read and the
/// value it could not read, an input request states its fixed sentence — rather
/// than with the shared constraint validator's `ValidationException`, a code
/// this service's model declares on no operation.
///
/// The rows span the family, the OPERATION inside it, the MEMBER the bad value
/// sits at (a member of the request itself and one nested inside another shape,
/// so the rule is not keyed to one member) and the spelling (a value differing
/// from a declared one only in case, a value of another enum, and an empty one).
#[tokio::test]
async fn a_value_outside_a_modelled_enum_is_refused_in_medialives_own_wording() {
    /// The fixed sentence an input or input-security-group request's enum
    /// refusal carries.
    const INPUT_ENUM_REFUSAL: &str = "Invalid input or ISG request Invalid enum value";

    // (case, method, uri, a body builder taking the offending value, the member
    //  the value sits at, whether the refusal quotes the request)
    struct Row {
        case: &'static str,
        method: &'static str,
        uri: &'static str,
        body: fn(&str) -> Value,
        quotes_the_request: bool,
    }

    let rows: [Row; 4] = [
        Row {
            case: "CreateChannel channelClass",
            method: "POST",
            uri: "/prod/channels",
            body: |value| json!({ "name": "enum", "channelClass": value }),
            quotes_the_request: true,
        },
        Row {
            // A member of the SAME request, at another enum: the wording follows
            // the request family, not the member.
            case: "CreateChannel logLevel",
            method: "POST",
            uri: "/prod/channels",
            body: |value| json!({ "name": "enum", "logLevel": value }),
            quotes_the_request: true,
        },
        Row {
            case: "CreateInput type",
            method: "POST",
            uri: "/prod/inputs",
            body: |value| json!({ "name": "enum", "type": value }),
            quotes_the_request: false,
        },
        Row {
            // NESTED inside another shape, so the rule reaches every enum the
            // request carries rather than the members of its top level.
            case: "CreateInput srtSettings caller decryption algorithm",
            method: "POST",
            uri: "/prod/inputs",
            body: |value| {
                json!({
                    "name": "enum",
                    "type": "SRT_CALLER",
                    "srtSettings": {
                        "srtCallerSources": [{
                            "decryption": { "algorithm": value, "passphraseSecretArn": "arn" },
                        }],
                    },
                })
            },
            quotes_the_request: false,
        },
    ];

    for row in rows {
        for value in ["standard", "NOT_A_VALUE", ""] {
            let router = medialive();
            let body = (row.body)(value);
            let case = format!("{} = {value:?}", row.case);
            let (status, refused) =
                call(&router, row.method, row.uri, Some(body.clone())).await;
            assert_eq!(
                status, 400,
                "case {case:?}: expected 400, got {status}: {refused}"
            );
            assert_eq!(
                error_code(status, &refused),
                "BadRequestException",
                "case {case:?}: expected MediaLive's own BadRequestException, got {refused}"
            );
            let message = error_message(&refused);
            if row.quotes_the_request {
                assert!(
                    message.starts_with("Invalid channel request '")
                        && message.ends_with(&format!("Invalid enum value'{value}'")),
                    "case {case:?}: expected the channel-family enum wording, got {message:?}"
                );
                assert!(
                    message.contains(value) || value.is_empty(),
                    "case {case:?}: expected the refusal to quote the request, got {message:?}"
                );
            } else {
                assert_eq!(
                    message, INPUT_ENUM_REFUSAL,
                    "case {case:?}: expected the input-family enum wording, got {message:?}"
                );
            }
        }
    }

    // The quoted request is HELD to the clamp this tree states for a refusal
    // echo, so neither the message nor the work behind it is sized by the
    // request: a body far longer than that clamp comes back as a prefix naming
    // how many bytes were sent, and the refusal is still the enum one.
    let router = medialive();
    let filler = "f".repeat(200_000);
    let body = json!({ "name": filler, "channelClass": "standard" });
    let submitted = serde_json::to_vec(&body).expect("the oversized body serializes").len();
    let (status, refused) = call(&router, "POST", "/prod/channels", Some(body)).await;
    assert_eq!(
        status, 400,
        "case \"an oversized body\": expected 400, got {status}: {refused}"
    );
    let message = error_message(&refused);
    assert!(
        message.len() < submitted / 2,
        "case \"an oversized body\": expected the quoted request to be held well under the \
         {submitted} bytes submitted, got a {} byte message",
        message.len()
    );
    assert!(
        message.contains("... [") && message.contains("bytes]"),
        "case \"an oversized body\": expected the elision marker naming what was sent, got a \
         {} byte message",
        message.len()
    );
    assert!(
        message.starts_with("Invalid channel request '")
            && message.ends_with("Invalid enum value'standard'"),
        "case \"an oversized body\": expected the channel-family enum wording, got a {} byte \
         message",
        message.len()
    );
}

/// Rule: a channel id and an input id are decimal strings, so an id a path label
/// carries that is outside that grammar names no resource and could never come
/// to name one: it is refused with the modelled 400 and the fixed sentence,
/// ahead of the lookup — while an id of that grammar that names nothing reaches
/// the lookup and is reported absent.
///
/// The rows span the OPERATION (every channel-id and input-id path of this
/// service, read and write alike) and the spelling of the id (names, ids with a
/// trailing letter, signed and separated numbers, and — on the other side of the
/// rule — unminted decimal ids).
#[tokio::test]
async fn an_id_outside_the_minted_grammar_is_refused_before_the_lookup() {
    // (case, method, the uri with {id} standing for the label, a body)
    let paths: [(&str, &str, &str, Option<Value>); 12] = [
        ("DescribeChannel", "GET", "/prod/channels/{id}", None),
        ("DeleteChannel", "DELETE", "/prod/channels/{id}", None),
        ("StartChannel", "POST", "/prod/channels/{id}/start", None),
        ("StopChannel", "POST", "/prod/channels/{id}/stop", None),
        (
            "RestartChannelPipelines",
            "POST",
            "/prod/channels/{id}/restartChannelPipelines",
            None,
        ),
        (
            "UpdateChannel",
            "PUT",
            "/prod/channels/{id}",
            Some(json!({ "name": "renamed" })),
        ),
        (
            "UpdateChannelClass",
            "PUT",
            "/prod/channels/{id}/channelClass",
            Some(json!({ "channelClass": "SINGLE_PIPELINE" })),
        ),
        ("DescribeSchedule", "GET", "/prod/channels/{id}/schedule", None),
        ("DeleteSchedule", "DELETE", "/prod/channels/{id}/schedule", None),
        ("ListAlerts", "GET", "/prod/channels/{id}/alerts", None),
        ("DescribeInput", "GET", "/prod/inputs/{id}", None),
        ("DeleteInput", "DELETE", "/prod/inputs/{id}", None),
    ];
    let malformed = [
        "MyMediaLiveChannel",
        "1000000a",
        "-1",
        "+1",
        "1_000",
        "10 00",
        "",
    ];

    let router = medialive();
    for (case, method, uri, body) in paths {
        for id in malformed {
            // An empty label leaves the collection URI, which is a different
            // route: the rule is about a label that IS there.
            if id.is_empty() {
                continue;
            }
            let case = format!("{case} on {id:?}");
            let (status, refused) = call(
                &router,
                method,
                &uri.replace("{id}", &crate::support::percent_encoded(id)),
                body.clone(),
            )
            .await;
            assert_eq!(
                status, 400,
                "case {case:?}: expected a malformed id to be refused with 400, got {status}: \
                 {refused}"
            );
            assert_eq!(
                error_code(status, &refused),
                "BadRequestException",
                "case {case:?}: expected the modelled BadRequestException, got {refused}"
            );
            assert_eq!(
                error_message(&refused),
                NON_NUMERIC_ID_MESSAGE,
                "case {case:?}: expected the fixed id-grammar sentence, got {:?}",
                error_message(&refused)
            );
        }

        // The other side of the rule: an id OF that grammar reaches the lookup,
        // so it is never refused for its spelling.
        for id in ["0", "9999999", "1234567890123"] {
            let case = format!("{case} on the unminted {id:?}");
            let (status, answered) =
                call(&router, method, &uri.replace("{id}", id), body.clone()).await;
            assert_ne!(
                error_message(&answered),
                NON_NUMERIC_ID_MESSAGE,
                "case {case:?}: a decimal id must reach the lookup, got {status}: {answered}"
            );
        }
    }
}

/// Rule: a paginated request's `MaxResults` is the page size verbatim inside the
/// published range (1 to [`MAX_LIST_PAGE_SIZE`]); outside it the request is
/// refused with the modelled 400 and that end's own fixed sentence; and a
/// `NextToken` spelled any other way than the listing mints an id continues no
/// walk, so it is refused with its own fixed sentence.
///
/// The rows span both bounds and each side of them, plus the token spellings a
/// client can write a number in that this service never hands out.
#[tokio::test]
async fn a_paging_parameter_outside_what_the_listing_serves_is_refused() {
    let ceiling = MAX_LIST_PAGE_SIZE;
    let router = medialive();

    // (case, the query, the sentence the refusal carries)
    let refused: [(String, String, String); 6] = [
        (
            "a page of none".to_string(),
            "?maxResults=0".to_string(),
            MAX_RESULTS_BELOW_FLOOR_MESSAGE.to_string(),
        ),
        (
            "a negative page".to_string(),
            "?maxResults=-7".to_string(),
            MAX_RESULTS_BELOW_FLOOR_MESSAGE.to_string(),
        ),
        (
            "one past the ceiling".to_string(),
            format!("?maxResults={}", ceiling + 1),
            max_results_above_ceiling_message(ceiling),
        ),
        (
            "far past the ceiling".to_string(),
            format!("?maxResults={}", ceiling * 9),
            max_results_above_ceiling_message(ceiling),
        ),
        (
            "a token that is not a number".to_string(),
            "?nextToken=later".to_string(),
            INVALID_NEXT_TOKEN_MESSAGE.to_string(),
        ),
        (
            "a padded spelling of a number".to_string(),
            "?nextToken=01000000".to_string(),
            INVALID_NEXT_TOKEN_MESSAGE.to_string(),
        ),
    ];

    for (case, query, expected) in refused {
        let (status, body) = call(&router, "GET", &format!("/prod/channels{query}"), None).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected 400, got {status}: {body}"
        );
        assert_eq!(
            error_code(status, &body),
            "BadRequestException",
            "case {case:?}: expected the modelled BadRequestException, got {body}"
        );
        assert_eq!(
            error_message(&body),
            expected,
            "case {case:?}: expected {expected:?}, got {:?}",
            error_message(&body)
        );
    }

    // Inside the range, and naming no size at all: served.
    for (case, query) in [
        ("the floor", "?maxResults=1".to_string()),
        ("the ceiling", format!("?maxResults={ceiling}")),
        ("no size at all", String::new()),
    ] {
        let (status, body) = call(&router, "GET", &format!("/prod/channels{query}"), None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected a page size inside the range to be served, got {status}: \
             {body}"
        );
    }
}

/// MediaLive and IAM on ONE engine, plus an IAM client over the router's own
/// in-process callback — the seam a channel's `RoleArn` resolution reaches IAM
/// through.
pub(crate) fn medialive_with_iam() -> (MultiServiceRouter, ::aws_sdk_iam::Client) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_medialive_medialive::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    let callback = EmbeddedEmulator::from_router(router.clone());
    let iam = ::aws_sdk_iam::Client::from_conf(
        ::aws_sdk_iam::Config::builder()
            .behavior_version(::aws_sdk_iam::config::BehaviorVersion::latest())
            .region(::aws_sdk_iam::config::Region::new(DEFAULT_REGION.to_string()))
            .endpoint_url(format!("https://iam.{DEFAULT_REGION}.amazonaws.com"))
            .credentials_provider(callback.aws_credentials_provider())
            .http_client(callback.http_client())
            .retry_config(::aws_sdk_iam::config::retry::RetryConfig::disabled())
            .timeout_config(::aws_sdk_iam::config::timeout::TimeoutConfig::disabled())
            .identity_cache(::aws_sdk_iam::config::IdentityCache::no_cache())
            .stalled_stream_protection(
                ::aws_sdk_iam::config::StalledStreamProtectionConfig::disabled(),
            )
            .build(),
    );
    (router, iam)
}

/// Create a role trusting `principal` through IAM itself, and answer the ARN IAM
/// minted for it.
pub(crate) async fn role_trusting(
    iam: &::aws_sdk_iam::Client,
    role_name: &str,
    principal: &str,
) -> String {
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": principal },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    iam.create_role()
        .role_name(role_name)
        .assume_role_policy_document(trust)
        .send()
        .await
        .unwrap_or_else(|error| panic!("setup: IAM must create {role_name}: {error:?}"))
        .role()
        .map(|role| role.arn().to_string())
        .unwrap_or_else(|| panic!("setup: IAM must report an ARN for {role_name}"))
}

/// Rule: `RoleArn` is the ARN of the role a channel runs as, so a value that is
/// not the ARN of an IAM role is a malformed member — the modelled 400 echoing
/// it — and an ARN that IAM answers does not name a role this account holds and
/// this service may assume is unprocessable — the modelled 422, carrying one
/// `ValidationErrors` entry at the member the request spelled it in — while a
/// role IAM holds and that trusts this service is accepted and read back.
///
/// The rows span the spellings a non-ARN can take, the kinds of well-formed ARN
/// that name no role of this account, and the two IAM answers that decide the
/// 422.
#[tokio::test]
async fn a_channels_role_is_resolved_against_iam_before_the_channel_is_stored() {
    let (router, iam) = medialive_with_iam();
    let assumable = role_trusting(&iam, "medialive-runner", MEDIALIVE_PRINCIPAL).await;
    let somebody_elses = role_trusting(&iam, "lambda-runner", OTHER_PRINCIPAL).await;

    // Not an IAM role ARN at all: the modelled 400, echoing the value.
    for (case, value) in [
        ("a bare role name", "MediaLiveAccessRole"),
        ("a role path with no ARN", "service-role/MediaLiveAccessRole"),
        (
            "another vendor's ARN",
            "arn:aws:s3:::a-bucket",
        ),
        (
            "an IAM ARN naming a user",
            "arn:aws:iam::000000000000:user/somebody",
        ),
        (
            "an IAM role ARN with no name",
            "arn:aws:iam::000000000000:role/",
        ),
        ("a sentence", "the role please"),
    ] {
        let mut body = channel_body("bad-role");
        body["roleArn"] = json!(value);
        let (status, refused) = call(&router, "POST", "/prod/channels", Some(body)).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected 400, got {status}: {refused}"
        );
        assert_eq!(
            error_code(status, &refused),
            "BadRequestException",
            "case {case:?}: expected the modelled BadRequestException, got {refused}"
        );
        assert_eq!(
            error_message(&refused),
            invalid_role_arn_message(value),
            "case {case:?}: expected the malformed-ARN sentence, got {:?}",
            error_message(&refused)
        );
    }

    // A well-formed role ARN IAM answers about: absent, or held but trusting
    // somebody else. Both are the modelled 422 at `roleArn`.
    for (case, value) in [
        (
            "a role IAM does not hold",
            "arn:aws:iam::000000000000:role/not-provisioned".to_string(),
        ),
        (
            "a role name IAM has never minted",
            "arn:aws:iam::000000000000:role/MediaLiveAccessRole".to_string(),
        ),
        ("a role trusting somebody else", somebody_elses.clone()),
    ] {
        let mut body = channel_body("unassumable-role");
        body["roleArn"] = json!(value);
        let (status, refused) = call(&router, "POST", "/prod/channels", Some(body)).await;
        assert_eq!(
            status, 422,
            "case {case:?}: expected 422, got {status}: {refused}"
        );
        assert_eq!(
            error_code(status, &refused),
            "UnprocessableEntityException",
            "case {case:?}: expected the modelled UnprocessableEntityException, got {refused}"
        );
        let problems = refused["validationErrors"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            problems.len(),
            1,
            "case {case:?}: expected one validationErrors entry, got {refused}"
        );
        assert_eq!(
            problems[0]["elementPath"],
            json!("roleArn"),
            "case {case:?}: expected the entry at roleArn, got {:?}",
            problems[0]
        );
        let stated = problems[0]["errorMessage"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(
            stated.contains(&value) && stated.contains(MEDIALIVE_PRINCIPAL),
            "case {case:?}: expected the entry to name the role and the principal, got {stated:?}"
        );
        assert_eq!(
            error_message(&refused),
            format!("roleArn {stated}"),
            "case {case:?}: expected the message to read the entry out, got {:?}",
            error_message(&refused)
        );
        // Nothing was stored for the refusal.
        let (status, listed) = call(&router, "GET", "/prod/channels", None).await;
        assert_eq!(status, 200, "case {case:?}: ListChannels: got {status}: {listed}");
        assert_eq!(
            listed["channels"].as_array().map(Vec::len).unwrap_or(0),
            0,
            "case {case:?}: expected the refused create to store nothing, got {listed}"
        );
    }

    // A role IAM holds that trusts this service: accepted, stored, and read back
    // through `DescribeChannel`.
    let mut body = channel_body("good-role");
    body["roleArn"] = json!(assumable.clone());
    let (status, created) = call(&router, "POST", "/prod/channels", Some(body)).await;
    assert_eq!(
        status, 201,
        "case \"an assumable role\": expected 201, got {status}: {created}"
    );
    let id = created["channel"]["id"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let (status, described) = call(&router, "GET", &format!("/prod/channels/{id}"), None).await;
    assert_eq!(
        status, 200,
        "case \"an assumable role\": DescribeChannel: got {status}: {described}"
    );
    assert_eq!(
        described["roleArn"],
        json!(assumable),
        "case \"an assumable role\": expected the stored role to read back, got {:?}",
        described["roleArn"]
    );
}
