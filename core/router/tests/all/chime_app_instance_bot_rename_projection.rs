// SPDX-License-Identifier: CC0-1.0

//! Renaming an `AppInstanceBot` through Chime SDK Identity, as Chime SDK Messaging reports
//! it.
//!
//! The rule under test: the `Name` a bot identity carries belongs to the bot, not to the
//! membership that was written earlier, so an `UpdateAppInstanceBot` is what every later
//! Chime SDK Messaging read of that participant reports — exactly as a rename through
//! `UpdateAppInstanceUser` is for a user.
//!
//! This needs BOTH services on one engine: the identity service owns and renames the bot,
//! the messaging service reports its name, and neither crate's own suite can reach the
//! other half. The sibling suite `chime_identity_name_projection` pins the hand-off for a
//! bot's CREATED name and for a USER's rename; the bot rename is the case only this one
//! drives.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// A SigV4 `Authorization` header whose credential scope names the signing name, from the
/// router suite that already states that rule — one `Authorization` builder per test
/// binary. Every Chime SDK service signs as `chime`.
use crate::rds_docdb_neptune_signing_name_routing::authorization;

// Percent-encoding for one URI path segment, as an AWS SDK encodes an `@httpLabel`:
// `all/cloudfront_key_value_store_handshake.rs` states that codec once for this test
// binary, so this suite shares it rather than carrying another copy.
use crate::cloudfront_key_value_store_handshake::segment as encode;

/// The Chime SDK Identity endpoint host.
const IDENTITY_HOST: &str = "identity-chime.us-east-1.amazonaws.com";
/// The Chime SDK Messaging endpoint host.
const MESSAGING_HOST: &str = "messaging-chime.us-east-1.amazonaws.com";

/// Chime SDK Identity and Chime SDK Messaging, mounted on ONE engine, exactly as
/// `core/router/src/lib.rs` mounts them.
fn build_identity_and_messaging_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_chimesdkidentity_chimeidentityservice::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_chimesdkmessaging_chimemessagingservice::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router
}

/// Dispatch one request to `host`, optionally as the `x-amz-chime-bearer` identity, and
/// return the status and the JSON body.
async fn call(
    router: &MultiServiceRouter,
    host: &str,
    bearer: Option<&str>,
    method: &str,
    path_and_query: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let payload = match body {
        Some(value) => Bytes::from(serde_json::to_vec(&value).expect("serialize the body")),
        None => Bytes::new(),
    };
    let mut builder = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path_and_query}"))
        .header("host", host)
        .header("content-type", "application/json")
        .header("authorization", authorization("chime"));
    if let Some(bearer) = bearer {
        builder = builder.header("x-amz-chime-bearer", bearer);
    }
    let request = builder.body(payload).expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let raw = response.into_body();
    let body = if raw.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&raw).unwrap_or(Value::Null)
    };
    (status, body)
}

#[tokio::test]
async fn a_renamed_bot_is_reported_under_its_new_name_by_chime_sdk_messaging() {
    let router = build_identity_and_messaging_router();

    let (status, body) = call(
        &router,
        IDENTITY_HOST,
        None,
        "POST",
        "/app-instances",
        Some(json!({ "Name": "bot-renaming", "ClientRequestToken": "instance-token" })),
    )
    .await;
    assert_eq!(
        status, 201,
        "CreateAppInstance must answer 201, got {status} {body:?}"
    );
    let app_instance = body["AppInstanceArn"]
        .as_str()
        .expect("CreateAppInstance returns an AppInstanceArn")
        .to_string();

    // The user that creates the channel and reads the memberships back.
    let (status, body) = call(
        &router,
        IDENTITY_HOST,
        None,
        "POST",
        "/app-instance-users",
        Some(json!({
            "AppInstanceArn": app_instance,
            "AppInstanceUserId": "creator",
            "Name": "Ada Creator",
            "ClientRequestToken": "user-token",
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "CreateAppInstanceUser must answer 201, got {status} {body:?}"
    );
    let creator = body["AppInstanceUserArn"]
        .as_str()
        .expect("CreateAppInstanceUser returns an AppInstanceUserArn")
        .to_string();

    let (status, body) = call(
        &router,
        IDENTITY_HOST,
        None,
        "POST",
        "/app-instance-bots",
        Some(json!({
            "AppInstanceArn": app_instance,
            "Name": "First Name",
            "Metadata": "created",
            "ClientRequestToken": "bot-token",
            "Configuration": {
                "Lex": {
                    "LexBotAliasArn": "arn:aws:lex:us-east-1:111122223333:bot-alias/AB1CD2EFGH/IJ3KL4MNOP",
                    "LocaleId": "en_US",
                    "RespondsTo": "STANDARD_MESSAGES",
                }
            },
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "CreateAppInstanceBot must answer 201, got {status} {body:?}"
    );
    let bot = body["AppInstanceBotArn"]
        .as_str()
        .expect("CreateAppInstanceBot returns an AppInstanceBotArn")
        .to_string();

    let (status, body) = call(
        &router,
        MESSAGING_HOST,
        Some(&creator),
        "POST",
        "/channels",
        Some(json!({
            "AppInstanceArn": app_instance,
            "Name": "renamed-participants",
            "Privacy": "PUBLIC",
            "ClientRequestToken": "channel-token",
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "CreateChannel must answer 201, got {status} {body:?}"
    );
    let channel = body["ChannelArn"]
        .as_str()
        .expect("CreateChannel returns a ChannelArn")
        .to_string();

    let (status, body) = call(
        &router,
        MESSAGING_HOST,
        Some(&creator),
        "POST",
        &format!("/channels/{}/memberships", encode(&channel)),
        Some(json!({ "MemberArn": bot, "Type": "DEFAULT" })),
    )
    .await;
    assert_eq!(
        status, 201,
        "CreateChannelMembership must answer 201, got {status} {body:?}"
    );
    assert_eq!(
        body["Member"]["Name"].as_str(),
        Some("First Name"),
        "expected the membership to report the name the bot was created with, got {body:?}"
    );

    // Rows differ along what `UpdateAppInstanceBot`'s `Name` generalises over: ordinary
    // text, the empty value its `length 0..256` allows, that 256-character maximum, and
    // non-ASCII text. Source: `smithy_query(query="operation",
    // operation="UpdateAppInstanceBot")` — `Name` is `ResourceName`, len 0..256.
    let max_name = "n".repeat(256);
    let rows: Vec<(&str, String)> = vec![
        ("an ordinary rename", "Second Name".to_string()),
        ("a rename to the empty name the model allows", String::new()),
        ("a rename to the modelled 256-character maximum", max_name),
        ("a rename to non-ASCII text", "Ría Ünicode ⛵".to_string()),
    ];

    for (case, name) in &rows {
        let (status, body) = call(
            &router,
            IDENTITY_HOST,
            None,
            "PUT",
            &format!("/app-instance-bots/{}", encode(&bot)),
            Some(json!({ "Name": name, "Metadata": "renamed" })),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: UpdateAppInstanceBot must answer 200, got {status} {body:?}"
        );
        assert_eq!(
            body["AppInstanceBotArn"].as_str(),
            Some(bot.as_str()),
            "case {case:?}: expected the update to answer with the bot's ARN, got {body:?}"
        );

        // The messaging read AFTER the rename reports the new name, on the membership that
        // was written before any of these renames.
        let (status, body) = call(
            &router,
            MESSAGING_HOST,
            Some(&creator),
            "GET",
            &format!(
                "/channels/{}/memberships/{}",
                encode(&channel),
                encode(&bot)
            ),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeChannelMembership must answer 200, got {status} {body:?}"
        );
        let member = &body["ChannelMembership"]["Member"];
        assert_eq!(
            member["Arn"].as_str(),
            Some(bot.as_str()),
            "case {case:?}: expected the membership to still name the bot, got {body:?}"
        );
        assert_eq!(
            member["Name"].as_str(),
            Some(name.as_str()),
            "case {case:?}: expected the membership to report the renamed bot's Name \
             {name:?}, got {body:?}"
        );
    }

    // The rename is the identity service's own record too: the last name written is what
    // `DescribeAppInstanceBot` reports.
    let (status, body) = call(
        &router,
        IDENTITY_HOST,
        None,
        "GET",
        &format!("/app-instance-bots/{}", encode(&bot)),
        None,
    )
    .await;
    let (last_case, last_name) = rows.last().expect("a non-empty table");
    assert_eq!(
        (status, body["AppInstanceBot"]["Name"].as_str()),
        (200, Some(last_name.as_str())),
        "case {last_case:?}: expected DescribeAppInstanceBot to report the last name \
         written, got {status} {body:?}"
    );
    assert_eq!(
        body["AppInstanceBot"]["Metadata"].as_str(),
        Some("renamed"),
        "expected DescribeAppInstanceBot to report the Metadata the update wrote, got \
         {body:?}"
    );
}
