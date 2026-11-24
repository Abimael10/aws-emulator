// SPDX-License-Identifier: CC0-1.0

//! The ONE rule by which Chime SDK Identity decides whether a tagging call may address a
//! `ResourceARN`, asserted across all three tagging routes it serves.
//!
//! The Amazon Chime family binds three generic tagging routes — `GET /tags?arn=…`,
//! `POST /tags?operation=tag-resource` and `POST /tags?operation=untag-resource` — and Chime
//! SDK Identity serves them for every member's resource, because nothing in such a request's
//! `Parts` names a service: the resource ARN travels in the body (or the query), so the
//! family's declared tagging owner answers a call that names no other member's endpoint.
//!
//! RULE: whether a tagging call may address an ARN is one question about two stores — does
//! the ARN name a resource this family's tagging routes serve, and has the member that owns
//! it withdrawn it — so all three routes answer an ARN that may not be addressed with the
//! same HTTP status, the same error shape, the same `Code` and the same `Message`. An ARN
//! whose `AppInstance` this service deleted is refused the modelled `ForbiddenException`
//! (403, `Forbidden`); an ARN that names no resource the routes serve is refused the modelled
//! `BadRequestException` (400, `NotFound`); and an ARN the OWNING member withdrew is refused
//! in that member's own class and wording, mapped onto the shape this model declares.
//!
//! The table spans what the rule generalises over: which of the two stores decides the
//! refusal (this service's own records, and the family's shared tag directory a sibling
//! withdrew into), which refusal class the ARN earns, which resource type the ARN names (an
//! app instance, a user, a bot, a sibling's meeting, another vendor's resource altogether),
//! and which of the three routes is called — plus a live ARN as the control, which all three
//! routes serve.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_chimesdkidentity_chimeidentityservice::state::FORBIDDEN_REQUEST_MESSAGE;
use service_com_amazonaws_chimesdkmeetings_chimemeetingssdkservice::state::INVALID_MEETING_ID_MESSAGE;

type Identity = service_com_amazonaws_chimesdkidentity_chimeidentityservice::EmulatedService<
    engine::InMemoryEngine,
>;
type Meetings = service_com_amazonaws_chimesdkmeetings_chimemeetingssdkservice::EmulatedService<
    engine::InMemoryEngine,
>;

/// A SigV4 `Authorization` header whose credential scope names the signing name every Chime
/// service shares, which is what routes a request to the family.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDTESTCHIME/20260101/us-east-1/chime/aws4_request, \
     SignedHeaders=host;x-amz-date, Signature=0000";

/// The Chime SDK Identity endpoint host: the member that serves the tagging routes below.
const IDENTITY_HOST: &str = "identity-chime.us-east-1.amazonaws.com";

/// The Chime SDK Meetings endpoint host, where the sibling's own resource is created and
/// deleted.
const MEETINGS_HOST: &str = "meetings-chime.us-east-1.amazonaws.com";

/// Chime SDK Identity and Chime SDK Meetings, mounted on ONE engine exactly as
/// `build_router` mounts them, so the tag directory the two share is one directory.
fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(Identity::new(Arc::clone(&engine)));
    router.register_service(Meetings::new(engine));
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

/// One row: the ARN a tagging call addresses and the refusal every one of the three routes
/// owes it.
struct Refused {
    case: &'static str,
    arn: String,
    status: u16,
    error_type: &'static str,
    code: &'static str,
    message: String,
}

/// Create an app instance through Chime SDK Identity and answer its ARN.
async fn app_instance(router: &MultiServiceRouter, name: &str, token: &str) -> String {
    let created = call(
        router,
        "POST",
        IDENTITY_HOST,
        "/app-instances",
        Some(&json!({ "Name": name, "ClientRequestToken": token })),
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

/// Create an app instance user under `instance` and answer its ARN.
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
        .unwrap_or_else(|| panic!("case {user_id:?}: no AppInstanceUserArn in {:?}", created.body))
        .to_string()
}

#[tokio::test]
async fn every_chime_identity_tagging_route_refuses_an_unaddressable_arn_the_same_way() {
    let router = router();

    // A live app instance with a live user: the control rows, which every route serves.
    let live = app_instance(&router, "tagging-live", "tok-tagging-live").await;
    let live_user = app_instance_user(&router, &live, "live-user", "tok-tagging-live-u").await;

    // An instance that is then DELETED, with a user the delete cascades away.
    let doomed = app_instance(&router, "tagging-doomed", "tok-tagging-doomed").await;
    let doomed_user =
        app_instance_user(&router, &doomed, "doomed-user", "tok-tagging-doomed-u").await;
    let deleted = call(
        &router,
        "DELETE",
        IDENTITY_HOST,
        &format!(
            "/app-instances/{}",
            crate::query_wire::form_encode(&doomed)
        ),
        None,
    )
    .await;
    assert_eq!(
        deleted.status, 204,
        "expected DeleteAppInstance to answer 204, got {} {:?}",
        deleted.status, deleted.body
    );

    // A resource of a SIBLING member, withdrawn by that member's own delete: the half of the
    // rule this service reads from the family's shared tag directory rather than from its own
    // records.
    let created = call(
        &router,
        "POST",
        MEETINGS_HOST,
        "/meetings",
        Some(&json!({
            "ClientRequestToken": "tok-tagging-meeting",
            "MediaRegion": "us-east-1",
            "ExternalMeetingId": "ext-tagging-meeting",
        })),
    )
    .await;
    assert_eq!(
        created.status, 200,
        "expected CreateMeeting to answer 200, got {} {:?}",
        created.status, created.body
    );
    let meeting_id = created.body["Meeting"]["MeetingId"]
        .as_str()
        .unwrap_or_else(|| panic!("no MeetingId in {:?}", created.body))
        .to_string();
    let meeting_arn = created.body["Meeting"]["MeetingArn"]
        .as_str()
        .unwrap_or_else(|| panic!("no MeetingArn in {:?}", created.body))
        .to_string();
    let deleted = call(
        &router,
        "DELETE",
        MEETINGS_HOST,
        &format!("/meetings/{meeting_id}"),
        None,
    )
    .await;
    assert_eq!(
        deleted.status, 204,
        "expected DeleteMeeting to answer 204, got {} {:?}",
        deleted.status, deleted.body
    );

    // The control: while an ARN names a resource the routes serve, all three are served.
    // (The meeting ARN is withdrawn by now, so it is a refusal row below rather than a
    // control.)
    for arn in [live.as_str(), live_user.as_str()] {
        for (operation, method, route, body) in tagging_routes(arn) {
            let reply = call(&router, method, IDENTITY_HOST, &route, body.as_ref()).await;
            assert!(
                reply.status < 300,
                "case a live ARN ({operation}, {arn}): expected it served, got {} {:?} {:?}",
                reply.status,
                reply.error_type,
                reply.body
            );
        }
    }

    let not_found = |arn: &str| format!("resource {arn} does not exist");
    let rows: Vec<Refused> = vec![
        Refused {
            case: "the deleted app instance's own ARN",
            arn: doomed.clone(),
            status: 403,
            error_type: "ForbiddenException",
            code: "Forbidden",
            message: FORBIDDEN_REQUEST_MESSAGE.to_string(),
        },
        Refused {
            case: "a user the delete cascaded away with the instance",
            arn: doomed_user.clone(),
            status: 403,
            error_type: "ForbiddenException",
            code: "Forbidden",
            message: FORBIDDEN_REQUEST_MESSAGE.to_string(),
        },
        Refused {
            case: "a bot ARN under the deleted instance",
            arn: format!("{doomed}/bot/b1"),
            status: 403,
            error_type: "ForbiddenException",
            code: "Forbidden",
            message: FORBIDDEN_REQUEST_MESSAGE.to_string(),
        },
        Refused {
            case: "an app instance ARN that never named anything",
            arn: format!("{live}-absent"),
            status: 400,
            error_type: "BadRequestException",
            code: "NotFound",
            message: not_found(&format!("{live}-absent")),
        },
        Refused {
            case: "a user ARN under a live instance that never named anything",
            arn: format!("{live_user}-absent"),
            status: 400,
            error_type: "BadRequestException",
            code: "NotFound",
            message: not_found(&format!("{live_user}-absent")),
        },
        Refused {
            case: "a bot ARN under a live instance that never named anything",
            arn: format!("{live}/bot/b1"),
            status: 400,
            error_type: "BadRequestException",
            code: "NotFound",
            message: not_found(&format!("{live}/bot/b1")),
        },
        Refused {
            case: "another vendor's resource ARN, outside this family's namespace",
            arn: "arn:aws:sqs:us-east-1:000000000000:queue/q1".to_string(),
            status: 400,
            error_type: "BadRequestException",
            code: "NotFound",
            message: not_found("arn:aws:sqs:us-east-1:000000000000:queue/q1"),
        },
        Refused {
            case: "a sibling member's meeting, withdrawn by that member's own delete",
            arn: meeting_arn.clone(),
            status: 400,
            error_type: "BadRequestException",
            code: "BadRequest",
            message: INVALID_MEETING_ID_MESSAGE.to_string(),
        },
    ];

    for row in rows {
        let Refused {
            case,
            arn,
            status,
            error_type,
            code,
            message,
        } = row;
        for (operation, method, route, body) in tagging_routes(&arn) {
            let reply = call(&router, method, IDENTITY_HOST, &route, body.as_ref()).await;
            assert_eq!(
                (
                    reply.status,
                    reply.error_type.as_str(),
                    reply.body["Code"].as_str(),
                    reply.body["Message"].as_str(),
                ),
                (status, error_type, Some(code), Some(message.as_str())),
                "case {case:?} ({operation}, {arn}): expected every tagging route to refuse it \
                 the same way, got {} {:?} {:?}",
                reply.status,
                reply.error_type,
                reply.body
            );
        }
    }

    // The refused calls wrote nothing: the live resources still list exactly the tags the
    // control rows left them with, and a tagging call is still served for them.
    for arn in [live.as_str(), live_user.as_str()] {
        let reply = call(
            &router,
            "GET",
            IDENTITY_HOST,
            &format!("/tags?arn={}", crate::query_wire::form_encode(arn)),
            None,
        )
        .await;
        let listed: Vec<(String, String)> = reply.body["Tags"]
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
        assert_eq!(
            (reply.status, listed.as_slice()),
            (200, [].as_slice()),
            "case a live ARN after the refusals ({arn}): expected the tags the control rows \
             left, got {} {:?}",
            reply.status,
            reply.body
        );
    }
}
