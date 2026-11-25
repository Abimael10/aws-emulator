// SPDX-License-Identifier: CC0-1.0

//! The `Tags` a Chime SDK Identity CREATE carries, against the family's shared tag directory.
//!
//! RULE under test: a create that carries `Tags` persists them in the family's shared,
//! ARN-keyed tag directory, which stores a merge of what the resource already carries and
//! what the request sends and refuses the write OUTRIGHT — storing none of the pairs — when
//! that merge would take the resource past the tag count the family's `TagList` is bounded
//! by. The create reports created only when the pairs were stored: a refused write is the
//! `ResourceLimitExceededException` (HTTP 400) each of these creates declares, carrying the
//! count the merge would have reached, and the refused create leaves nothing behind — no
//! resource, no spent `ClientRequestToken`, and the tags the resource already carried exactly
//! as they were.
//!
//! Why this is a CROSS-SERVICE suite: the merge is refused only when the directory already
//! holds tags for the ARN the create determines, and an `AppInstanceUser`'s ARN is its
//! instance's ARN plus the `AppInstanceUserId` the caller chose, so it is known before the
//! user exists. The member that OWNS the user refuses a tagging call for an ARN of its own
//! type it holds no resource for, but a SIBLING binding the same shared `/tags` routes cannot
//! read the owner's store and takes the ARN at its word — so the pre-existing tags below are
//! written through Chime SDK Messaging, as a client whose SDK is pointed at that endpoint
//! does.
//!
//! The table spans what the rule generalises over: how many tags the ARN already carries
//! (none, short of the maximum, one short of it, at it), how many the create sends (one, a
//! few, the whole `TagList`), and whether the keys it sends are new to the resource or ones it
//! already carries — which is the difference between a merge that grows the set and one that
//! only RE-VALUES it, and so between a create that is refused and one that is not.
//! `CreateAppInstance` and `CreateAppInstanceBot` carry the same create-time `Tags` over an
//! ARN that carries a freshly minted uuid, where no earlier tagging call can have addressed
//! it; they are exercised for the stored side of the rule.

use std::collections::BTreeMap;
use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_chimesdkidentity_chimeidentityservice::state::app_instance_user_arn;

type Identity = service_com_amazonaws_chimesdkidentity_chimeidentityservice::EmulatedService<
    engine::InMemoryEngine,
>;
type Messaging = service_com_amazonaws_chimesdkmessaging_chimemessagingservice::EmulatedService<
    engine::InMemoryEngine,
>;

/// A SigV4 `Authorization` header whose credential scope names the signing name every Chime
/// service shares, which is what routes a request to the family.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDTESTCHIME/20260101/us-east-1/chime/aws4_request, \
     SignedHeaders=host;x-amz-date, Signature=0000";

/// The Chime SDK Identity endpoint host: the member that owns the resources created below.
const IDENTITY_HOST: &str = "identity-chime.us-east-1.amazonaws.com";

/// The Chime SDK Messaging endpoint host: the sibling the pre-existing tags are written
/// through, which holds no record of an identity resource and takes its ARN at its word.
const MESSAGING_HOST: &str = "messaging-chime.us-east-1.amazonaws.com";

/// How many tags one Chime resource carries at once, and so the most a create can leave on
/// one: the identity model bounds `TagList` at 50 members
/// (`smithy_query(query="operation",operation="CreateAppInstanceUser")`: `Tags` ->
/// `TagList, len 1..50`), which is the per-resource maximum the whole family shares.
const MAX_TAGS_PER_RESOURCE: usize = 50;

/// The two services that answer below, mounted on ONE engine exactly as `build_router` mounts
/// them, so the tag directory they share is one directory.
fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(Identity::new(Arc::clone(&engine)));
    router.register_service(Messaging::new(engine));
    router
}

/// One answer: the HTTP status, the restJson1 error type a client resolves the shape from,
/// and the parsed body.
struct Reply {
    status: u16,
    error_type: String,
    body: Value,
}

impl Reply {
    /// The `Code` and `Message` of a refusal, as a client reads them.
    fn refusal(&self) -> (u16, String, String, String) {
        (
            self.status,
            self.error_type.clone(),
            self.body["Code"].as_str().unwrap_or_default().to_string(),
            self.body["Message"].as_str().unwrap_or_default().to_string(),
        )
    }
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

/// The `Tags` member of a request, from key/value pairs.
fn tag_list(pairs: &[(String, String)]) -> Value {
    Value::Array(
        pairs
            .iter()
            .map(|(key, value)| json!({ "Key": key, "Value": value }))
            .collect(),
    )
}

/// Create an app instance through Chime SDK Identity and answer its ARN.
async fn app_instance(router: &MultiServiceRouter, case: &str, name: &str) -> String {
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
        "case {case:?}: expected CreateAppInstance to answer 201, got {} {:?}",
        created.status, created.body
    );
    created.body["AppInstanceArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no AppInstanceArn in {:?}", created.body))
        .to_string()
}

/// `CreateAppInstanceUser` with `tags` (sent only when non-empty, `TagList` being
/// `@length(min: 1)`), under the caller's token.
async fn create_user(
    router: &MultiServiceRouter,
    instance: &str,
    user_id: &str,
    token: &str,
    tags: &[(String, String)],
) -> Reply {
    let mut request = json!({
        "AppInstanceArn": instance,
        "AppInstanceUserId": user_id,
        "Name": user_id,
        "ClientRequestToken": token,
    });
    if !tags.is_empty() {
        request["Tags"] = tag_list(tags);
    }
    call(
        router,
        "POST",
        IDENTITY_HOST,
        "/app-instance-users",
        Some(&request),
    )
    .await
}

/// `TagResource` over the shared route, addressed at `host`.
async fn tag_resource(
    router: &MultiServiceRouter,
    host: &str,
    arn: &str,
    pairs: &[(String, String)],
) -> Reply {
    call(
        router,
        "POST",
        host,
        "/tags?operation=tag-resource",
        Some(&json!({ "ResourceARN": arn, "Tags": tag_list(pairs) })),
    )
    .await
}

/// The tags the directory reports for `arn`, read through the member addressed at `host`.
async fn listed_tags(
    router: &MultiServiceRouter,
    host: &str,
    arn: &str,
) -> (u16, BTreeMap<String, String>) {
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

/// `DescribeAppInstanceUser` by ARN.
async fn describe_user(router: &MultiServiceRouter, arn: &str) -> Reply {
    call(
        router,
        "GET",
        IDENTITY_HOST,
        &format!(
            "/app-instance-users/{}",
            crate::query_wire::form_encode(arn)
        ),
        None,
    )
    .await
}

/// `count` pairs under distinct keys, named by `prefix` so a row can say which of them the
/// resource already carried.
fn pairs(prefix: &str, count: usize, value: &str) -> Vec<(String, String)> {
    (0..count)
        .map(|index| (format!("{prefix}-{index:02}"), value.to_string()))
        .collect()
}

/// One row of the table: the tags the ARN already carries and the tags the create sends.
struct Case {
    case: &'static str,
    /// A per-row suffix, so each row's resources and tokens are its own.
    id: &'static str,
    /// How many tags the sibling member writes for the user's ARN before it exists.
    held: usize,
    /// How many of the create's own `Tags` repeat a key the resource already carries, and so
    /// only re-value it.
    revalued: usize,
    /// How many of the create's own `Tags` carry a key new to the resource.
    added: usize,
    /// The tag count the merge reaches, which decides the answer and is what a refusal
    /// reports.
    merged: usize,
}

#[tokio::test]
async fn a_create_stores_its_tags_or_is_refused_with_none_of_them_stored() {
    let rows = [
        Case {
            case: "nothing held, one tag created",
            id: "r1",
            held: 0,
            revalued: 0,
            added: 1,
            merged: 1,
        },
        Case {
            case: "nothing held, the whole TagList created",
            id: "r2",
            held: 0,
            revalued: 0,
            added: MAX_TAGS_PER_RESOURCE,
            merged: MAX_TAGS_PER_RESOURCE,
        },
        Case {
            case: "one short of the maximum held, one new key created",
            id: "r3",
            held: MAX_TAGS_PER_RESOURCE - 1,
            revalued: 0,
            added: 1,
            merged: MAX_TAGS_PER_RESOURCE,
        },
        Case {
            case: "at the maximum, a key it already carries re-valued",
            id: "r4",
            held: MAX_TAGS_PER_RESOURCE,
            revalued: 1,
            added: 0,
            merged: MAX_TAGS_PER_RESOURCE,
        },
        Case {
            case: "at the maximum, three keys it already carries re-valued",
            id: "r5",
            held: MAX_TAGS_PER_RESOURCE,
            revalued: 3,
            added: 0,
            merged: MAX_TAGS_PER_RESOURCE,
        },
        Case {
            case: "at the maximum, one new key created",
            id: "r6",
            held: MAX_TAGS_PER_RESOURCE,
            revalued: 0,
            added: 1,
            merged: MAX_TAGS_PER_RESOURCE + 1,
        },
        Case {
            case: "two short of the maximum, three new keys created",
            id: "r7",
            held: MAX_TAGS_PER_RESOURCE - 2,
            revalued: 0,
            added: 3,
            merged: MAX_TAGS_PER_RESOURCE + 1,
        },
        Case {
            case: "at the maximum, one key re-valued and one new key created",
            id: "r8",
            held: MAX_TAGS_PER_RESOURCE,
            revalued: 1,
            added: 1,
            merged: MAX_TAGS_PER_RESOURCE + 1,
        },
        Case {
            case: "well short of the maximum, the whole TagList created",
            id: "r9",
            held: 10,
            revalued: 0,
            added: MAX_TAGS_PER_RESOURCE,
            merged: 10 + MAX_TAGS_PER_RESOURCE,
        },
    ];

    for Case {
        case,
        id,
        held,
        revalued,
        added,
        merged,
    } in rows
    {
        let router = router();
        let instance = app_instance(&router, case, &format!("instance-{id}")).await;
        let user_id = format!("alice-{id}");
        let arn = app_instance_user_arn(&instance, &user_id);

        // What the ARN already carries, written through the sibling member: it holds no
        // record of an identity resource, so it takes the ARN at its word.
        let before = pairs("held", held, "before");
        if !before.is_empty() {
            let tagged = tag_resource(&router, MESSAGING_HOST, &arn, &before).await;
            assert!(
                tagged.status < 300,
                "case {case:?}: expected the sibling's TagResource to store the {held} tags \
                 the ARN starts with, got {} {:?} {:?}",
                tagged.status,
                tagged.error_type,
                tagged.body
            );
        }

        // The create's own `Tags`: keys the resource already carries (re-valued) and keys new
        // to it (which are what can grow the set past its maximum).
        let mut sent = pairs("held", revalued, "after");
        sent.extend(pairs("added", added, "created"));

        let created = create_user(&router, &instance, &user_id, &format!("tok-{id}"), &sent).await;

        if merged <= MAX_TAGS_PER_RESOURCE {
            assert_eq!(
                (created.status, created.body["AppInstanceUserArn"].as_str()),
                (201, Some(arn.as_str())),
                "case {case:?}: expected the create to answer 201 with {arn}, got {} {:?}",
                created.status,
                created.body
            );

            // The merge: every key the resource held, re-valued where the create sent that
            // key, plus every key the create added.
            let mut expected: BTreeMap<String, String> = before.iter().cloned().collect();
            expected.extend(sent.iter().cloned());
            let (status, listed) = listed_tags(&router, IDENTITY_HOST, &arn).await;
            assert_eq!(
                (status, &listed),
                (200, &expected),
                "case {case:?}: expected the directory to report the merged {merged} tags, \
                 got {status} {listed:?}"
            );

            let described = describe_user(&router, &arn).await;
            assert_eq!(
                (
                    described.status,
                    described.body["AppInstanceUser"]["AppInstanceUserArn"].as_str()
                ),
                (200, Some(arn.as_str())),
                "case {case:?}: expected the created user to read back, got {} {:?}",
                described.status,
                described.body
            );
            continue;
        }

        // Past the maximum the directory stores NONE of the pairs, so the create is the
        // modelled resource-limit refusal carrying the count the merge would have reached.
        let expected_refusal = (
            400,
            "ResourceLimitExceededException".to_string(),
            "ResourceLimitExceeded".to_string(),
            format!(
                "{merged} tags were requested for {arn}, which is more than the maximum of \
                 {MAX_TAGS_PER_RESOURCE} tags per resource"
            ),
        );
        assert_eq!(
            created.refusal(),
            expected_refusal,
            "case {case:?}: expected the create to answer the modelled resource-limit \
             refusal, got {} {:?} {:?}",
            created.status,
            created.error_type,
            created.body
        );

        // The identical write over the tagging route earns the identical refusal: one
        // refused write, one answer, whichever route asked for it. Addressed at the sibling,
        // which takes the ARN of a resource it holds no record of at its word — the owner
        // refuses a tagging call for an id of its own it holds nothing under, which is the
        // other refusal and not this one.
        let tagged = tag_resource(&router, MESSAGING_HOST, &arn, &sent).await;
        assert_eq!(
            tagged.refusal(),
            expected_refusal,
            "case {case:?}: expected TagResource to answer the create's refusal for the same \
             write, got {} {:?} {:?}",
            tagged.status,
            tagged.error_type,
            tagged.body
        );

        // Nothing of the refused create is left: the tags the ARN carried are exactly as they
        // were, no user was stored under the id, and the token it was presented with was not
        // spent — the same token creates the user once the request fits. The listing goes
        // through the sibling for the same reason the write did: the owner holds no resource
        // under the ARN, so its own answer for it is the absent-resource refusal.
        let expected_before: BTreeMap<String, String> = before.iter().cloned().collect();
        let (status, listed) = listed_tags(&router, MESSAGING_HOST, &arn).await;
        assert_eq!(
            (status, &listed),
            (200, &expected_before),
            "case {case:?}: expected the {held} tags the ARN carried to be untouched, got \
             {status} {listed:?}"
        );

        let described = describe_user(&router, &arn).await;
        assert_eq!(
            (
                described.status,
                described.body["Code"].as_str(),
                described.body.get("AppInstanceUser").is_some()
            ),
            (400, Some("NotFound"), false),
            "case {case:?}: expected no user stored under the refused create's id, got {} {:?}",
            described.status,
            described.body
        );

        let again = create_user(&router, &instance, &user_id, &format!("tok-{id}"), &[]).await;
        assert_eq!(
            (again.status, again.body["AppInstanceUserArn"].as_str()),
            (201, Some(arn.as_str())),
            "case {case:?}: expected the refused create's token and id to be free, got {} {:?}",
            again.status,
            again.body
        );

        // With the user live, the OWNER's own tagging route answers that same refused write
        // the same way: the create and the tagging call are one write against one directory.
        let owner_tagged = tag_resource(&router, IDENTITY_HOST, &arn, &sent).await;
        assert_eq!(
            owner_tagged.refusal(),
            expected_refusal,
            "case {case:?}: expected the owner's TagResource to answer the create's refusal \
             for the same write, got {} {:?} {:?}",
            owner_tagged.status,
            owner_tagged.error_type,
            owner_tagged.body
        );
    }
}

#[tokio::test]
async fn an_instance_and_a_bot_store_the_tags_their_creates_carry() {
    // `CreateAppInstance` and `CreateAppInstanceBot` mint an ARN carrying a fresh uuid, so no
    // earlier tagging call can have addressed it and their merge is the request itself: the
    // whole `TagList` is stored and read back through the shared route.
    let router = router();
    let instance_tags = pairs("instance", MAX_TAGS_PER_RESOURCE, "created");
    let created = call(
        &router,
        "POST",
        IDENTITY_HOST,
        "/app-instances",
        Some(&json!({
            "Name": "tagged-instance",
            "ClientRequestToken": "tok-instance",
            "Tags": tag_list(&instance_tags),
        })),
    )
    .await;
    assert_eq!(
        created.status, 201,
        "case \"a tagged app instance\": expected 201, got {} {:?}",
        created.status, created.body
    );
    let instance = created.body["AppInstanceArn"]
        .as_str()
        .unwrap_or_else(|| {
            panic!(
                "case \"a tagged app instance\": no AppInstanceArn in {:?}",
                created.body
            )
        })
        .to_string();

    let bot_tags = pairs("bot", 3, "created");
    let created_bot = call(
        &router,
        "POST",
        IDENTITY_HOST,
        "/app-instance-bots",
        Some(&json!({
            "AppInstanceArn": instance,
            "Name": "tagged-bot",
            "ClientRequestToken": "tok-bot",
            "Tags": tag_list(&bot_tags),
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
        created_bot.status, 201,
        "case \"a tagged app instance bot\": expected 201, got {} {:?}",
        created_bot.status, created_bot.body
    );
    let bot = created_bot.body["AppInstanceBotArn"]
        .as_str()
        .unwrap_or_else(|| {
            panic!(
                "case \"a tagged app instance bot\": no AppInstanceBotArn in {:?}",
                created_bot.body
            )
        })
        .to_string();

    for (case, arn, expected) in [
        ("a tagged app instance", instance.as_str(), &instance_tags),
        ("a tagged app instance bot", bot.as_str(), &bot_tags),
    ] {
        let expected: BTreeMap<String, String> = expected.iter().cloned().collect();
        let (status, listed) = listed_tags(&router, IDENTITY_HOST, arn).await;
        assert_eq!(
            (status, &listed),
            (200, &expected),
            "case {case:?}: expected its create-time tags to be stored, got {status} {listed:?}"
        );
    }
}
