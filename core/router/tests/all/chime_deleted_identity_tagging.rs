// SPDX-License-Identifier: CC0-1.0

//! The tags of a DELETED Chime SDK Identity resource, across every member that can serve a
//! tagging call naming it.
//!
//! The Amazon Chime family binds the three generic tagging routes (`GET /tags?arn=…`,
//! `POST /tags?operation=tag-resource`, `POST /tags?operation=untag-resource`) in five of its
//! six models, and nothing in such a request's `Parts` names a service: the resource ARN
//! travels in the request BODY (or the query). So a request naming a family endpoint host is
//! served by THAT member whichever member's resource the ARN names, and one naming no family
//! host — an SDK client pointed at the emulator's own address — is served by the family's
//! declared tagging owner.
//!
//! RULE under test: the member that OWNS a Chime resource records, when it deletes it, that
//! the resource's ARN is withdrawn — with the error class and the sentence that member serves
//! for a tagging call naming it — in the family's shared ARN-keyed directory, and every member
//! serving one of those three routes answers a withdrawn ARN with that refusal instead of
//! reporting a deleted resource as a live, untagged one. A member serving the call cannot read
//! the owner's store, so the record is the only thing that can tell it the resource is gone.
//!
//! The table spans what the rule generalises over: which resource type the deleted ARN names
//! (an app instance, an app instance user, an app instance bot), whether the delete was
//! addressed to the resource itself, cascaded to it from its `AppInstance`, or addressed to it
//! and followed by its `AppInstance`'s own delete, whether the resource carried tags when it
//! was deleted, which of the three tagging routes is called, and which member serves that call
//! — each of the five that bind the routes, plus a request that names no family host at all.
//! The status, the error shape and the sentence are the owner's and are the same from every
//! member; the refusal's `Code` is the serving member's own value for that class, which the
//! table carries per member rather than leaving unasserted.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_chimesdkidentity_chimeidentityservice::state::FORBIDDEN_REQUEST_MESSAGE;

type Identity = service_com_amazonaws_chimesdkidentity_chimeidentityservice::EmulatedService<
    engine::InMemoryEngine,
>;
type Messaging = service_com_amazonaws_chimesdkmessaging_chimemessagingservice::EmulatedService<
    engine::InMemoryEngine,
>;
type Meetings = service_com_amazonaws_chimesdkmeetings_chimemeetingssdkservice::EmulatedService<
    engine::InMemoryEngine,
>;
type MediaPipelines =
    service_com_amazonaws_chimesdkmediapipelines_chimesdkmediapipelinesservice::EmulatedService<
        engine::InMemoryEngine,
    >;
type Voice = service_com_amazonaws_chimesdkvoice_chimesdktelephonyservice::EmulatedService<
    engine::InMemoryEngine,
>;

/// A SigV4 `Authorization` header whose credential scope names the signing name every Chime
/// service shares, which is what routes a request to the family.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDTESTCHIME/20260101/us-east-1/chime/aws4_request, \
     SignedHeaders=host;x-amz-date, Signature=0000";

/// The Chime SDK Identity endpoint host: the member that OWNS the resources deleted below,
/// and the family's declared tagging owner.
const IDENTITY_HOST: &str = "identity-chime.us-east-1.amazonaws.com";

/// The Chime SDK Messaging endpoint host, the sibling a tag is also WRITTEN through below.
const MESSAGING_HOST: &str = "messaging-chime.us-east-1.amazonaws.com";

/// The emulator's own address: a request naming it names no family endpoint, so the family's
/// declared tagging owner serves its `/tags` calls.
const EMULATOR_HOST: &str = "localhost:9297";

/// Where a tagging call is addressed, and whether that address is served by the member that
/// OWNS the resources deleted below: its own endpoint, and — because it is the family's
/// declared tagging owner — a request naming no family endpoint at all. Whichever address the
/// client used, the status, the error shape and the sentence for one ARN have to be the same;
/// the refusal's `Code` is the serving member's own enum value for that class, which is why
/// the table carries which member answered.
const SERVING_HOSTS: [(&str, bool); 6] = [
    (IDENTITY_HOST, true),
    (MESSAGING_HOST, false),
    ("meetings-chime.us-east-1.amazonaws.com", false),
    ("voice-chime.us-east-1.amazonaws.com", false),
    ("media-pipelines-chime.us-east-1.amazonaws.com", false),
    (EMULATOR_HOST, true),
];

/// The five members that bind the family's shared tagging routes, mounted on ONE engine
/// exactly as `build_router` mounts them, so the tag directory they share is one directory.
fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(Identity::new(Arc::clone(&engine)));
    router.register_service(Messaging::new(Arc::clone(&engine)));
    router.register_service(Meetings::new(Arc::clone(&engine)));
    router.register_service(MediaPipelines::new(Arc::clone(&engine)));
    router.register_service(Voice::new(engine));
    router
}

/// One answer: the HTTP status, the restJson1 error type a client resolves the shape from,
/// and the parsed body.
struct Reply {
    status: u16,
    error_type: String,
    body: Value,
}

async fn call(
    router: &MultiServiceRouter,
    method: &str,
    host: &str,
    path: &str,
    body: Option<&Value>,
) -> Reply {
    let payload = body.map_or_else(Vec::new, |value| {
        serde_json::to_vec(value).expect("a request body serializes")
    });
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host)
        .header("authorization", AUTHORIZATION)
        .header("content-type", "application/json")
        .body(Bytes::from(payload))
        .expect("the test request is well formed");

    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let header_type = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let bytes = response.into_body();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    let error_type = header_type
        .or_else(|| {
            body.get("__type")
                .and_then(Value::as_str)
                .map(|wire| wire.rsplit('#').next().unwrap_or(wire).to_string())
        })
        .unwrap_or_default();
    Reply {
        status,
        error_type,
        body,
    }
}

/// The three tagging routes, as `(name, method, path, body)` for `resource_arn`.
///
/// The list route carries the ARN in a query parameter, percent-encoded with this test
/// crate's one encoder ([`crate::query_wire::form_encode`]) rather than a copy of it.
fn tagging_routes(resource_arn: &str) -> Vec<(&'static str, &'static str, String, Option<Value>)> {
    vec![
        (
            "ListTagsForResource",
            "GET",
            format!("/tags?arn={}", crate::query_wire::form_encode(resource_arn)),
            None,
        ),
        (
            "TagResource",
            "POST",
            "/tags?operation=tag-resource".to_string(),
            Some(json!({
                "ResourceARN": resource_arn,
                "Tags": [{ "Key": "env", "Value": "dev" }],
            })),
        ),
        (
            "UntagResource",
            "POST",
            "/tags?operation=untag-resource".to_string(),
            Some(json!({ "ResourceARN": resource_arn, "TagKeys": ["env"] })),
        ),
    ]
}

/// Create an app instance through Chime SDK Identity and answer its ARN.
async fn app_instance(router: &MultiServiceRouter, name: &str) -> String {
    let created = call(
        router,
        "POST",
        IDENTITY_HOST,
        "/app-instances",
        Some(&json!({ "Name": name, "ClientRequestToken": format!("tok-{name}") })),
    )
    .await;
    assert_eq!(
        created.status, 201,
        "case {name:?}: expected CreateAppInstance to answer 201, got {} {:?}",
        created.status, created.body
    );
    created.body["AppInstanceArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {name:?}: no AppInstanceArn in {:?}", created.body))
        .to_string()
}

/// Create an app instance user under `instance` and answer its ARN. `token` is varied by the
/// caller because one `ClientRequestToken` mints one resource.
async fn app_instance_user(
    router: &MultiServiceRouter,
    instance: &str,
    user_id: &str,
    token: &str,
) -> String {
    let created = call(
        router,
        "POST",
        IDENTITY_HOST,
        "/app-instance-users",
        Some(&json!({
            "AppInstanceArn": instance,
            "AppInstanceUserId": user_id,
            "Name": user_id,
            "ClientRequestToken": token,
        })),
    )
    .await;
    assert_eq!(
        created.status, 201,
        "case {user_id:?}: expected CreateAppInstanceUser to answer 201, got {} {:?}",
        created.status, created.body
    );
    created.body["AppInstanceUserArn"]
        .as_str()
        .unwrap_or_else(|| {
            panic!(
                "case {user_id:?}: no AppInstanceUserArn in {:?}",
                created.body
            )
        })
        .to_string()
}

/// Create an app instance bot under `instance` and answer its ARN.
///
/// `Configuration` is `@required` and its `Lex` member's `LexBotAliasArn` is pattern-bound
/// (`smithy_query(query="shape",shape="LexConfiguration")`), so the bot carries a well-formed
/// Amazon Lex V2 bot-alias ARN.
async fn app_instance_bot(
    router: &MultiServiceRouter,
    instance: &str,
    bot_id: &str,
    token: &str,
) -> String {
    let created = call(
        router,
        "POST",
        IDENTITY_HOST,
        "/app-instance-bots",
        Some(&json!({
            "AppInstanceArn": instance,
            "Name": bot_id,
            "ClientRequestToken": token,
            "Configuration": {
                "Lex": {
                    "LexBotAliasArn":
                        "arn:aws:lex:us-west-2:123456789012:bot-alias/BOTIDAAAAA/ALIASAAAAA",
                    "LocaleId": "en_US",
                    "InvokedBy": { "StandardMessages": "ALL", "TargetedMessages": "ALL" },
                }
            },
        })),
    )
    .await;
    assert_eq!(
        created.status, 201,
        "case {bot_id:?}: expected CreateAppInstanceBot to answer 201, got {} {:?}",
        created.status, created.body
    );
    created.body["AppInstanceBotArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {bot_id:?}: no AppInstanceBotArn in {:?}", created.body))
        .to_string()
}

/// `DELETE` one identity resource by ARN, asserting the operation's modelled 204.
async fn delete(router: &MultiServiceRouter, case: &str, collection: &str, arn: &str) {
    let deleted = call(
        router,
        "DELETE",
        IDENTITY_HOST,
        &format!("/{collection}/{}", crate::query_wire::form_encode(arn)),
        None,
    )
    .await;
    assert_eq!(
        deleted.status, 204,
        "case {case:?}: expected the delete of {arn} to answer 204, got {} {:?}",
        deleted.status, deleted.body
    );
}

/// Tag `arn` through the shared route, addressed at `host`, asserting it was served.
async fn tag(router: &MultiServiceRouter, case: &str, host: &str, arn: &str, key: &str) {
    let tagged = call(
        router,
        "POST",
        host,
        "/tags?operation=tag-resource",
        Some(&json!({
            "ResourceARN": arn,
            "Tags": [{ "Key": key, "Value": "crew" }],
        })),
    )
    .await;
    assert!(
        tagged.status < 300,
        "case {case:?}: expected TagResource at {host} to be served, got {} {:?} {:?}",
        tagged.status,
        tagged.error_type,
        tagged.body
    );
}

/// The tags `host` reports for `arn`, with the status of that listing.
async fn listed_tags(
    router: &MultiServiceRouter,
    host: &str,
    arn: &str,
) -> (u16, Vec<(String, String)>) {
    let reply = call(
        router,
        "GET",
        host,
        &format!("/tags?arn={}", crate::query_wire::form_encode(arn)),
        None,
    )
    .await;
    let tags = reply.body["Tags"]
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .filter_map(|tag| {
                    Some((
                        tag["Key"].as_str()?.to_string(),
                        tag["Value"].as_str()?.to_string(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default();
    (reply.status, tags)
}

/// Which resource the row's delete removes, and whether it was addressed to that resource or
/// cascaded to it from the `AppInstance` that owned it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Deleted {
    /// `DeleteAppInstance` on the instance itself.
    Instance,
    /// `DeleteAppInstance`, whose cascade takes the user with it.
    CascadedUser,
    /// `DeleteAppInstance`, whose cascade takes the bot with it.
    CascadedBot,
    /// `DeleteAppInstanceUser`, its `AppInstance` left live.
    User,
    /// `DeleteAppInstanceBot`, its `AppInstance` left live.
    Bot,
    /// `DeleteAppInstanceUser` first and `DeleteAppInstance` afterwards: the child was
    /// withdrawn while its instance was still live, and the instance's own delete then
    /// speaks for it too.
    UserThenInstance,
}

/// One row of the table.
struct Case {
    case: &'static str,
    /// A per-row suffix, so each row's resources and idempotency tokens are its own.
    id: &'static str,
    deleted: Deleted,
    /// Whether the resource carried a tag when it was deleted.
    tagged: bool,
}

#[tokio::test]
async fn a_deleted_identity_resource_is_refused_by_every_member_serving_the_shared_tagging_routes()
{
    let rows = [
        Case {
            case: "a tagged app instance deleted by its own delete",
            id: "r1",
            deleted: Deleted::Instance,
            tagged: true,
        },
        Case {
            case: "an untagged app instance deleted by its own delete",
            id: "r2",
            deleted: Deleted::Instance,
            tagged: false,
        },
        Case {
            case: "a tagged user the instance's delete cascaded away",
            id: "r3",
            deleted: Deleted::CascadedUser,
            tagged: true,
        },
        Case {
            case: "an untagged bot the instance's delete cascaded away",
            id: "r4",
            deleted: Deleted::CascadedBot,
            tagged: false,
        },
        Case {
            case: "a tagged user deleted on its own, under a live instance",
            id: "r5",
            deleted: Deleted::User,
            tagged: true,
        },
        Case {
            case: "a tagged bot deleted on its own, under a live instance",
            id: "r6",
            deleted: Deleted::Bot,
            tagged: true,
        },
        Case {
            case: "an untagged user deleted on its own, under a live instance",
            id: "r7",
            deleted: Deleted::User,
            tagged: false,
        },
        Case {
            case: "a tagged user deleted on its own, whose instance is deleted after it",
            id: "r8",
            deleted: Deleted::UserThenInstance,
            tagged: true,
        },
    ];

    for Case {
        case,
        id,
        deleted,
        tagged,
    } in rows
    {
        let router = router();
        let instance = app_instance(&router, &format!("doomed-{id}")).await;
        let user = app_instance_user(&router, &instance, &format!("u-{id}"), &format!("tu-{id}"))
            .await;
        let bot =
            app_instance_bot(&router, &instance, &format!("b-{id}"), &format!("tb-{id}")).await;
        // A second instance, tagged and never deleted: the control that says the refusals
        // below belong to the ARN the delete withdrew and not to the route or the directory.
        let survivor = app_instance(&router, &format!("survivor-{id}")).await;
        tag(&router, case, IDENTITY_HOST, &survivor, "owner").await;

        let target = match deleted {
            Deleted::Instance => instance.clone(),
            Deleted::CascadedUser | Deleted::User | Deleted::UserThenInstance => user.clone(),
            Deleted::CascadedBot | Deleted::Bot => bot.clone(),
        };
        if tagged {
            // Written through a SIBLING member: the tags of one resource are one piece of
            // state whichever member's route wrote them.
            tag(&router, case, MESSAGING_HOST, &target, "owner").await;
        }

        // While the resource is LIVE every member serves every one of the three routes for
        // its ARN, so what changes below is the delete and not the route.
        for (host, _) in SERVING_HOSTS {
            for (operation, method, route, body) in tagging_routes(&target) {
                let reply = call(&router, method, host, &route, body.as_ref()).await;
                assert!(
                    reply.status < 300,
                    "case {case:?} ({operation} at {host}, resource live): expected it served, \
                     got {} {:?} {:?}",
                    reply.status,
                    reply.error_type,
                    reply.body
                );
            }
        }

        match deleted {
            Deleted::Instance | Deleted::CascadedUser | Deleted::CascadedBot => {
                delete(&router, case, "app-instances", &instance).await;
            }
            Deleted::User => delete(&router, case, "app-instance-users", &user).await,
            Deleted::Bot => delete(&router, case, "app-instance-bots", &bot).await,
            Deleted::UserThenInstance => {
                delete(&router, case, "app-instance-users", &user).await;
                delete(&router, case, "app-instances", &instance).await;
            }
        }

        // The refusal the OWNER serves for the deleted ARN: a request addressed to a resource
        // whose `AppInstance` is gone is one this caller may no longer make, while a child
        // deleted under a live instance leaves an ARN that names nothing. `own_code` is the
        // `Code` the owner itself words that refusal with and `sibling_code` the one a member
        // reading the owner's recorded class words it with — the same class, each member's own
        // value for it.
        let (status, error_type, own_code, sibling_code, message) = match deleted {
            Deleted::Instance
            | Deleted::CascadedUser
            | Deleted::CascadedBot
            | Deleted::UserThenInstance => (
                403,
                "ForbiddenException",
                "Forbidden",
                "Forbidden",
                FORBIDDEN_REQUEST_MESSAGE.to_string(),
            ),
            Deleted::User | Deleted::Bot => (
                400,
                "BadRequestException",
                "NotFound",
                "BadRequest",
                format!("resource {target} does not exist"),
            ),
        };

        for (host, served_by_owner) in SERVING_HOSTS {
            let code = if served_by_owner {
                own_code
            } else {
                sibling_code
            };
            for (operation, method, route, body) in tagging_routes(&target) {
                let reply = call(&router, method, host, &route, body.as_ref()).await;
                assert_eq!(
                    (
                        reply.status,
                        reply.error_type.as_str(),
                        reply.body["Code"].as_str(),
                        reply.body["Message"].as_str(),
                    ),
                    (status, error_type, Some(code), Some(message.as_str())),
                    "case {case:?} ({operation} at {host}, resource deleted): expected the \
                     owner's refusal from whichever member serves the call, got {} {:?} {:?}",
                    reply.status,
                    reply.error_type,
                    reply.body
                );
            }
        }

        // The control: the instance nobody deleted still reports the tag it was given, from
        // every member — a withdrawal names ONE ARN.
        for (host, _) in SERVING_HOSTS {
            let (status, tags) = listed_tags(&router, host, &survivor).await;
            assert_eq!(
                (status, tags.as_slice()),
                (200, [("owner".to_string(), "crew".to_string())].as_slice()),
                "case {case:?} (the live instance at {host}): expected its tags still served, \
                 got {status} {tags:?}"
            );
        }
    }
}

#[tokio::test]
async fn an_identity_resource_created_again_under_the_same_id_is_served_by_every_member() {
    // An `AppInstanceUser`'s ARN is its instance's ARN plus the `AppInstanceUserId` the
    // caller chose, so a delete and a re-create under the same id hand out the SAME ARN. The
    // resource that ARN names exists again, so the record of the earlier deletion must not
    // outlive it: every member serves tagging calls for it, and the tags the deleted
    // resource carried are gone. (A bot's ARN carries a freshly minted uuid instead —
    // `CreateAppInstanceBot` takes no caller-chosen id — so no create of one can land on a
    // withdrawn ARN; the bot here is the row that shows a re-created user does not disturb
    // its siblings.)
    let router = router();
    let instance = app_instance(&router, "readmitted").await;
    let user = app_instance_user(&router, &instance, "u-again", "tu-again").await;
    let bot = app_instance_bot(&router, &instance, "b-again", "tb-again").await;

    for (case, arn) in [("a user", user.as_str()), ("a bot", bot.as_str())] {
        tag(&router, case, IDENTITY_HOST, arn, "owner").await;
    }
    delete(&router, "a user", "app-instance-users", &user).await;

    // The same id again, under a fresh token: one `ClientRequestToken` mints one resource.
    let user_again = app_instance_user(&router, &instance, "u-again", "tu-again-2").await;
    assert_eq!(
        user_again, user,
        "expected the re-created user to carry the ARN its id determines"
    );

    // The re-created user reads as live and untagged; the bot nobody deleted still reports
    // the tag it was given. Both answers are the same from every member.
    for (case, arn, expected) in [
        ("the re-created user", user.as_str(), Vec::new()),
        (
            "the untouched bot beside it",
            bot.as_str(),
            vec![("owner".to_string(), "crew".to_string())],
        ),
    ] {
        for (host, _) in SERVING_HOSTS {
            let (status, tags) = listed_tags(&router, host, arn).await;
            assert_eq!(
                (status, tags.as_slice()),
                (200, expected.as_slice()),
                "case {case:?}, listed at {host}: expected a live resource with {expected:?}, \
                 got {status} {tags:?}"
            );
            for (operation, method, route, body) in tagging_routes(arn) {
                let reply = call(&router, method, host, &route, body.as_ref()).await;
                assert!(
                    reply.status < 300,
                    "case {case:?} ({operation} at {host}): expected it served, got {} {:?} {:?}",
                    reply.status,
                    reply.error_type,
                    reply.body
                );
            }
        }
    }
}
