// SPDX-License-Identifier: CC0-1.0

//! Deleting an app instance in Chime SDK Identity REVOKES the `AppInstanceAdmin` role Chime
//! SDK Messaging honours for it.
//!
//! The rule under test: a promotion is a grant OVER one app instance, so it lasts exactly as
//! long as that instance does. `DeleteAppInstance` takes everything the instance owned with
//! it, the promotions of its administrators included, so the identity that administered it
//! holds no administrator action afterwards — and an administrator of ANOTHER app instance
//! keeps every action its own grant carries.
//!
//! Only a cross-service suite can pin it: the promotion is written and deleted by one crate
//! and the role is judged by another, and what a leftover promotion would buy is exactly the
//! two actions the messaging service reserves for an administrator (listing an account's
//! PRIVATE channels, deleting a channel message). The identity crate's own suites can see the
//! record go, not what the sibling makes of it.

use serde_json::json;

// The two services on one engine, the bearer-carrying wire call, the hosts they answer on
// and the error-code reader are stated once by the sibling suite about this same hand-off,
// so this one shares them rather than building a second harness.
use crate::chime_app_instance_admin_authorization::{
    build_identity_and_messaging_router, call, IDENTITY_HOST, MESSAGING_HOST,
};
// Percent-encoding for one URI path segment, as an AWS SDK encodes an `@httpLabel`.
use crate::cloudfront_key_value_store_handshake::segment as encode;

/// One app instance of the test world: its ARN, the identity promoted over it, the identity
/// that created its channels, its private channel and a message in its public channel.
struct World {
    name: &'static str,
    app_instance: String,
    admin: String,
    creator: String,
    private_channel: String,
    public_channel: String,
    message_id: String,
}

/// Create an app instance with two identities, a PRIVATE channel, a PUBLIC channel holding
/// one message, and a promotion of the first identity over the instance.
async fn world(router: &smithy_server_runtime::MultiServiceRouter, name: &'static str) -> World {
    let created = call(
        router,
        IDENTITY_HOST,
        None,
        "POST",
        "/app-instances",
        Some(json!({ "Name": name, "ClientRequestToken": format!("instance-{name}") })),
    )
    .await;
    assert_eq!(
        created.status, 201,
        "CreateAppInstance({name}) must answer 201, got {} {:?}",
        created.status, created.body
    );
    let app_instance = created.body["AppInstanceArn"]
        .as_str()
        .expect("CreateAppInstance returns an AppInstanceArn")
        .to_string();

    let mut identities = Vec::new();
    for user_id in ["admin", "creator"] {
        let user = call(
            router,
            IDENTITY_HOST,
            None,
            "POST",
            "/app-instance-users",
            Some(json!({
                "AppInstanceArn": app_instance,
                "AppInstanceUserId": user_id,
                "Name": user_id,
                "ClientRequestToken": format!("user-{name}-{user_id}"),
            })),
        )
        .await;
        assert_eq!(
            user.status, 201,
            "CreateAppInstanceUser({name}/{user_id}) must answer 201, got {} {:?}",
            user.status, user.body
        );
        identities.push(
            user.body["AppInstanceUserArn"]
                .as_str()
                .expect("CreateAppInstanceUser returns an AppInstanceUserArn")
                .to_string(),
        );
    }
    let (admin, creator) = (identities[0].clone(), identities[1].clone());

    let mut channels = Vec::new();
    for (channel_name, privacy) in [("secret", "PRIVATE"), ("open", "PUBLIC")] {
        let channel = call(
            router,
            MESSAGING_HOST,
            Some(&creator),
            "POST",
            "/channels",
            Some(json!({
                "AppInstanceArn": app_instance,
                "Name": channel_name,
                "Privacy": privacy,
                "ClientRequestToken": format!("channel-{name}-{channel_name}"),
            })),
        )
        .await;
        assert_eq!(
            channel.status, 201,
            "CreateChannel({name}/{channel_name}) must answer 201, got {} {:?}",
            channel.status, channel.body
        );
        channels.push(
            channel.body["ChannelArn"]
                .as_str()
                .expect("CreateChannel returns a ChannelArn")
                .to_string(),
        );
    }
    let (private_channel, public_channel) = (channels[0].clone(), channels[1].clone());

    let sent = call(
        router,
        MESSAGING_HOST,
        Some(&creator),
        "POST",
        &format!("/channels/{}/messages", encode(&public_channel)),
        Some(json!({
            "Content": "hello",
            "Type": "STANDARD",
            "Persistence": "PERSISTENT",
            "ClientRequestToken": format!("message-{name}"),
        })),
    )
    .await;
    assert_eq!(
        sent.status, 201,
        "SendChannelMessage({name}) must answer 201, got {} {:?}",
        sent.status, sent.body
    );
    let message_id = sent.body["MessageId"]
        .as_str()
        .expect("SendChannelMessage returns a MessageId")
        .to_string();

    let promoted = call(
        router,
        IDENTITY_HOST,
        None,
        "POST",
        &format!("/app-instances/{}/admins", encode(&app_instance)),
        Some(json!({ "AppInstanceAdminArn": admin })),
    )
    .await;
    assert_eq!(
        promoted.status, 201,
        "CreateAppInstanceAdmin({name}) must answer 201, got {} {:?}",
        promoted.status, promoted.body
    );

    World {
        name,
        app_instance,
        admin,
        creator,
        private_channel,
        public_channel,
        message_id,
    }
}

/// `ListChannels` with `privacy=PRIVATE` as `bearer` — one of the two actions the messaging
/// service reserves for an administrator of the app instance.
async fn list_private_channels(
    router: &smithy_server_runtime::MultiServiceRouter,
    world: &World,
    bearer: &str,
) -> crate::chime_app_instance_admin_authorization::Reply {
    call(
        router,
        MESSAGING_HOST,
        Some(bearer),
        "GET",
        &format!(
            "/channels?app-instance-arn={}&privacy=PRIVATE",
            encode(&world.app_instance)
        ),
        None,
    )
    .await
}

/// `DeleteChannelMessage` as `bearer` — the other administrator-only action.
async fn delete_message(
    router: &smithy_server_runtime::MultiServiceRouter,
    world: &World,
    bearer: &str,
) -> crate::chime_app_instance_admin_authorization::Reply {
    call(
        router,
        MESSAGING_HOST,
        Some(bearer),
        "DELETE",
        &format!(
            "/channels/{}/messages/{}",
            encode(&world.public_channel),
            encode(&world.message_id)
        ),
        None,
    )
    .await
}

#[tokio::test]
async fn deleting_an_app_instance_revokes_the_admin_role_its_promotion_granted() {
    let router = build_identity_and_messaging_router();
    // Two app instances, each with an administrator of its own: the one whose instance is
    // deleted, and the control whose grant every assertion below must leave standing.
    let revoked = world(&router, "grant-revoked").await;
    let kept = world(&router, "grant-kept").await;

    // Both grants are honoured while both app instances exist.
    for world in [&revoked, &kept] {
        let listed = list_private_channels(&router, world, &world.admin).await;
        assert_eq!(
            listed.status, 200,
            "case {:?}: the promoted identity must be served the PRIVATE listing, got {} {:?}",
            world.name, listed.status, listed.body
        );
        let listed_arns: Vec<&str> = listed.body["Channels"]
            .as_array()
            .map(|channels| {
                channels
                    .iter()
                    .filter_map(|entry| entry["ChannelArn"].as_str())
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(
            listed_arns,
            vec![world.private_channel.as_str()],
            "case {:?}: the PRIVATE listing must report that instance's private channel, body {:?}",
            world.name,
            listed.body
        );
    }

    // The app instance that granted one of the two roles is deleted.
    let deleted = call(
        &router,
        IDENTITY_HOST,
        None,
        "DELETE",
        &format!("/app-instances/{}", encode(&revoked.app_instance)),
        None,
    )
    .await;
    assert_eq!(
        deleted.status, 204,
        "DeleteAppInstance must answer 204, got {} {:?}",
        deleted.status, deleted.body
    );

    // The role went with it: neither administrator-only action is served to the identity
    // that held it, and the message it was refused permission to delete is still there.
    let listed = list_private_channels(&router, &revoked, &revoked.admin).await;
    assert_eq!(
        listed.status, 403,
        "case {:?}: the PRIVATE listing must be refused once the granting app instance is \
         deleted, got {} {:?}",
        revoked.name, listed.status, listed.body
    );
    assert_eq!(
        listed.error_code(),
        "ForbiddenException",
        "case {:?}: wrong error code for the revoked listing, body {:?}",
        revoked.name,
        listed.body
    );
    let message_deletion = delete_message(&router, &revoked, &revoked.admin).await;
    assert_eq!(
        message_deletion.status, 403,
        "case {:?}: deleting a channel message must be refused once the granting app instance \
         is deleted, got {} {:?}",
        revoked.name, message_deletion.status, message_deletion.body
    );
    let read_back = call(
        &router,
        MESSAGING_HOST,
        Some(&revoked.creator),
        "GET",
        &format!(
            "/channels/{}/messages/{}",
            encode(&revoked.public_channel),
            encode(&revoked.message_id)
        ),
        None,
    )
    .await;
    assert_eq!(
        read_back.status, 200,
        "case {:?}: the refused deletion must have deleted nothing, got {} {:?}",
        revoked.name, read_back.status, read_back.body
    );

    // The identity service agrees about the same record: the promotion is gone, so both of
    // its reads answer the refusal a request over a deleted app instance earns.
    for (what, path) in [
        (
            "DescribeAppInstanceAdmin",
            format!(
                "/app-instances/{}/admins/{}",
                encode(&revoked.app_instance),
                encode(&revoked.admin)
            ),
        ),
        (
            "ListAppInstanceAdmins",
            format!("/app-instances/{}/admins", encode(&revoked.app_instance)),
        ),
    ] {
        let read = call(&router, IDENTITY_HOST, None, "GET", &path, None).await;
        assert_eq!(
            (read.status, read.body["Code"].as_str()),
            (403, Some("Forbidden")),
            "case {what:?}: a read of the deleted app instance's administrators must be \
             refused, got {} {:?}",
            read.status,
            read.body
        );
    }

    // The control: the other app instance's administrator still holds both actions, so what
    // the delete revoked is ONE instance's grant and not the role itself.
    let listed = list_private_channels(&router, &kept, &kept.admin).await;
    assert_eq!(
        listed.status, 200,
        "case {:?}: another app instance's administrator must still be served the PRIVATE \
         listing, got {} {:?}",
        kept.name, listed.status, listed.body
    );
    let kept_deletion = delete_message(&router, &kept, &kept.admin).await;
    assert_eq!(
        kept_deletion.status, 204,
        "case {:?}: another app instance's administrator must still delete a channel message, \
         got {} {:?}",
        kept.name, kept_deletion.status, kept_deletion.body
    );
}
