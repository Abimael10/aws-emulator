// SPDX-License-Identifier: CC0-1.0

//! WHERE a GraphQL API's real-time endpoint judges a user-pool identity: the CONNECTION
//! against the API's authentication mode, and each SUBSCRIPTION against the subscription
//! field's own group guard.
//!
//! The rule these rows assert, one level above any one API, identity or field:
//!
//! > A real-time connection is admitted by the API's AUTHENTICATION mode alone: every
//! > identity that mode admits — for `AMAZON_COGNITO_USER_POOLS`, a token the configured pool
//! > signed, inside its own window — completes the opening handshake and is answered
//! > `connection_ack`, whatever groups it claims. A field's GROUP GUARD
//! > (`@aws_auth(cognito_groups: […])`) is judged per SUBSCRIPTION, at the `start` that asks
//! > for it: a caller claiming none of the groups it names is answered an `error` message
//! > addressed by that subscription's own id, and nothing of the connection is lost — every
//! > other subscription on it is started and delivered to as usual. Only an identity the mode
//! > itself refuses is turned away, and that at its `connection_init`, which ends the
//! > connection. What a guard is judged against is what the PRESENTED token claims, so a
//! > caller the pool later puts in the group is started on the identical subscription once it
//! > has signed in again.
//!
//! The rows span what that rule generalises over: how many groups a guard names (none, one,
//! two) and whether the caller's own group is among them (yes, no, last), the identity
//! presented (a caller in the group, a caller in none, a caller whose token the pool did not
//! sign, the same caller before and after the pool put it in the group), and which of a
//! connection's subscriptions was refused (the first, with a second started after it).
//!
//! Every address dialled here is the one the API PUBLISHED: each case parses `uris.REALTIME`
//! and `uris.GRAPHQL` — scheme, authority and path — and dials exactly that, so a published
//! host no client on this machine resolves fails here rather than silently going unused.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::event_api_data_plane::{appsync, loopback_port};
use crate::graphql_api_mapping_template_resolvers::{install_schema, try_post_answer};
use crate::graphql_api_realtime_endpoint::{
    exchange, handshake, next_frame, started_message, ClientSocket,
};
use crate::graphql_api_user_pool_authorization::{
    cognito, emulator, id_token, pool_with_client, sign_in, user_in_groups, with_rewritten_claims,
};

/// The Region the pool is created in and the API's configuration names.
const REGION: &str = "us-west-2";

/// The path both of a GraphQL API's endpoints serve; the service's own copies are
/// `service::graphql_api::query::GRAPHQL_PATH` and `service::graphql_api::realtime::\
/// REALTIME_PATH`.
const GRAPHQL_PATH: &str = "/graphql";

/// The scheme each published endpoint is addressed over: the GraphQL one takes HTTP requests
/// and the real-time one a WebSocket handshake.
const GRAPHQL_SCHEME: &str = "https";
const REALTIME_SCHEME: &str = "wss";

/// The group one subscriber of every case is in, which is the group [`SCHEMA`]'s guards name
/// beside one (`Auditors`) no subscriber of this suite is ever in.
const EDITORS: &str = "Editors";

/// The refusal a subscription nobody is authorized for carries. The service's own copies are
/// `service::event_api::UNAUTHORIZED_ERROR_TYPE` and `…_MESSAGE`, and the `errorCode` is that
/// error's modelled `httpError`
/// (`smithy_query(query="shape",shape="UnauthorizedException")`).
const UNAUTHORIZED_ERROR_TYPE: &str = "UnauthorizedException";
const UNAUTHORIZED_MESSAGE: &str = "You are not authorized to make this call.";
const UNAUTHORIZED_STATUS: u16 = 401;

/// The `runtime` the one resolver of this suite is defined under. Source: `AppSyncRuntime`'s
/// own values (`smithy_query(query="shape",shape="AppSyncRuntime,RuntimeName")`).
const RUNTIME: (&str, &str) = ("APPSYNC_JS", "1.0.0");

/// The name of the local data source the mutation resolves against: `NONE` is "There is no
/// data source", so the request function's own payload is the result.
const DATA_SOURCE: &str = "local";

/// The subscription fields of [`SCHEMA`]: one under no guard, one whose guard names a single
/// group, and one whose guard names two with the subscriber's group LAST.
const UNGUARDED_FIELD: &str = "onCreatePost";
const EDITORS_FIELD: &str = "onCreatePostForEditors";
const STAFF_FIELD: &str = "onCreatePostForStaff";

/// The ids the cases start their subscriptions under: the guarded one each row is about, and
/// the unguarded one started after it on the same connection.
const GUARDED_ID: &str = "guarded";
const OPEN_ID: &str = "open";

/// What the mutation of each case writes, which is what a live subscription receives.
const CONTENT: &str = "a post";

/// The two users of every case: one the pool puts in [`EDITORS`], and one in no group at all.
const USERNAME_IN_GROUP: &str = "editor";
const USERNAME_IN_NO_GROUP: &str = "outsider";

/// The schema every API of this suite is created with: one mutation, and three subscriptions
/// linked to it — one under no guard, one guarded by a single group and one guarded by two,
/// so a subscriber's membership is judged against a list of either length. The group a
/// subscriber of this suite is in is [`EDITORS`].
const SCHEMA: &str = "type Post {\n  id: ID!\n  content: String\n}\n\
     \n\
     type Query {\n  post(id: ID!): Post\n}\n\
     \n\
     type Mutation {\n  createPost(id: ID!, content: String!): Post\n}\n\
     \n\
     type Subscription {\n  \
     onCreatePost: Post @aws_subscribe(mutations: [\"createPost\"])\n  \
     onCreatePostForEditors: Post @aws_subscribe(mutations: [\"createPost\"]) \
     @aws_auth(cognito_groups: [\"Editors\"])\n  \
     onCreatePostForStaff: Post @aws_subscribe(mutations: [\"createPost\"]) \
     @aws_auth(cognito_groups: [\"Auditors\", \"Editors\"])\n}\n";

/// One created GraphQL API, as a client reads it: the authorities its two endpoints are
/// PUBLISHED on, parsed out of `uris`.
struct Published {
    graphql: String,
    realtime: String,
}

#[tokio::test]
async fn the_connection_is_admitted_by_the_mode_and_each_subscription_by_the_fields_guard() {
    let (router, _engine) = emulator();
    let (pool_id, client_id) = pool_with_client(&router, "guard-deferral-pool").await;
    user_in_groups(&router, &pool_id, USERNAME_IN_GROUP, &[EDITORS]).await;
    user_in_groups(&router, &pool_id, USERNAME_IN_NO_GROUP, &[]).await;
    let editor = id_token(&sign_in(&router, &pool_id, &client_id, USERNAME_IN_GROUP).await);
    let outsider = id_token(&sign_in(&router, &pool_id, &client_id, USERNAME_IN_NO_GROUP).await);
    let api = create_api(&router, "guard-deferral", &pool_id).await;

    /// One row: the token the subscriber presents, the subscription field it starts on, and
    /// whether that field's guard admits it.
    struct Row {
        case: &'static str,
        presented: String,
        field: &'static str,
        started: bool,
    }
    // The rows differ along the guard the field carries (none, one group, two groups) and
    // along the groups the subscriber's own token claims.
    let rows = [
        Row {
            case: "an unguarded field, for a caller in no group",
            presented: outsider.clone(),
            field: UNGUARDED_FIELD,
            started: true,
        },
        Row {
            case: "a guard naming one group, for a caller in it",
            presented: editor.clone(),
            field: EDITORS_FIELD,
            started: true,
        },
        Row {
            case: "a guard naming two groups, for a caller in the one named last",
            presented: editor.clone(),
            field: STAFF_FIELD,
            started: true,
        },
        Row {
            case: "a guard naming one group, for a caller in none",
            presented: outsider.clone(),
            field: EDITORS_FIELD,
            started: false,
        },
        Row {
            case: "a guard naming two groups, for a caller in neither",
            presented: outsider.clone(),
            field: STAFF_FIELD,
            started: false,
        },
    ];


    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        // Admission is the MODE's decision: every identity the pool signed a live token for
        // completes the upgrade and is acked, whatever groups the token claims.
        let mut socket = connected(case, &api, &row.presented).await;

        let answered = exchange(
            case,
            &mut socket,
            &started_message(GUARDED_ID, &document_for(row.field), &json!({})),
        )
        .await;
        let kind = answered.as_ref().and_then(|frame| frame["type"].as_str());
        match row.started {
            true => assert_eq!(
                kind,
                Some("start_ack"),
                "case {case:?}: a start on a field whose guard admits the caller must be \
                 acked, got {answered:?}"
            ),
            false => assert_refused_subscription(case, GUARDED_ID, answered.as_ref()),
        }

        // Nothing of the connection was lost by that decision: a further start, on the same
        // socket, is served as usual.
        let further = exchange(
            case,
            &mut socket,
            &started_message(OPEN_ID, &document_for(UNGUARDED_FIELD), &json!({})),
        )
        .await;
        assert_eq!(
            further.as_ref().and_then(|frame| frame["type"].as_str()),
            Some("start_ack"),
            "case {case:?}: the connection stays usable, so a later start on it must be \
             acked, got {further:?}"
        );

        // And a mutation the schema links to every subscription field says which
        // subscriptions this connection really holds.
        let post_id = format!("P-{index}");
        create_post(case, &api, &editor, &post_id, CONTENT).await;
        let mut owed: Vec<(&str, &str)> = vec![(OPEN_ID, UNGUARDED_FIELD)];
        if row.started {
            owed.push((GUARDED_ID, row.field));
        }
        let delivered = deliveries(case, &mut socket, &owed, &post_id).await;
        let mut expected: Vec<String> = owed.iter().map(|(id, _)| (*id).to_string()).collect();
        expected.sort();
        assert_eq!(
            delivered, expected,
            "case {case:?}: expected a delivery to every subscription this connection holds \
             and to no other"
        );
        if !row.started {
            // The refused subscription delivers nothing: the mutation reached the live
            // subscription of this connection and no second frame followed it.
            let extra = next_frame(&mut socket).await;
            assert!(
                extra.is_none(),
                "case {case:?}: a subscription this endpoint refused receives nothing, got \
                 {extra:?}"
            );
        }
    }
}

#[tokio::test]
async fn an_identity_the_mode_refuses_is_turned_away_at_its_connection_init() {
    let (router, _engine) = emulator();
    let (pool_id, client_id) = pool_with_client(&router, "mode-refusal-pool").await;
    user_in_groups(&router, &pool_id, USERNAME_IN_GROUP, &[EDITORS]).await;
    user_in_groups(&router, &pool_id, USERNAME_IN_NO_GROUP, &[]).await;
    let outsider = id_token(&sign_in(&router, &pool_id, &client_id, USERNAME_IN_NO_GROUP).await);
    let api = create_api(&router, "mode-refusal", &pool_id).await;

    // A holder of a token writing the group into its OWN claim set: a value the pool never
    // signed, so the MODE refuses it — which is the one refusal this endpoint reports about
    // the connection rather than about a subscription.
    let case = "a token whose claim set was rewritten after the pool signed it";
    let forged = with_rewritten_claims(&outsider, |claims| {
        claims["cognito:groups"] = json!([EDITORS]);
    });
    let mut socket = handshake(
        case,
        &api.realtime,
        GRAPHQL_PATH,
        true,
        Some(&json!({"host": api.realtime, "Authorization": forged})),
        &[],
    )
    .await
    .unwrap_or_else(|status| {
        panic!(
            "case {case:?}: the handshake carries no decision about the material, so it must \
             complete, got {status}"
        )
    });
    let answered = exchange(case, &mut socket, &json!({"type": "connection_init"})).await;
    assert_eq!(
        answered.as_ref().and_then(|frame| frame["type"].as_str()),
        Some("connection_error"),
        "case {case:?}: a connection the mode refuses must be refused at its init, got \
         {answered:?}"
    );
    assert_eq!(
        answered
            .as_ref()
            .and_then(|frame| frame.pointer("/payload/errors/0/errorType"))
            .and_then(Value::as_str),
        Some(UNAUTHORIZED_ERROR_TYPE),
        "case {case:?}: expected the refusal of an unauthorized call, got {answered:?}"
    );
    // The refusal ends the connection: nothing more is served over it.
    let further = next_frame(&mut socket).await;
    assert!(
        further.is_none(),
        "case {case:?}: the connection ends with that refusal, got {further:?}"
    );
}

#[tokio::test]
async fn a_caller_the_pool_puts_in_the_group_is_started_on_the_identical_subscription() {
    let (router, _engine) = emulator();
    let (pool_id, client_id) = pool_with_client(&router, "restore-pool").await;
    // The group the guard names is created with the user that is in it from the start.
    user_in_groups(&router, &pool_id, USERNAME_IN_GROUP, &[EDITORS]).await;
    user_in_groups(&router, &pool_id, USERNAME_IN_NO_GROUP, &[]).await;
    let before = id_token(&sign_in(&router, &pool_id, &client_id, USERNAME_IN_NO_GROUP).await);
    let api = create_api(&router, "restore", &pool_id).await;

    let case = "the caller before the pool put it in the group";
    let mut refused_socket = connected(case, &api, &before).await;
    let refused = exchange(
        case,
        &mut refused_socket,
        &started_message(GUARDED_ID, &document_for(EDITORS_FIELD), &json!({})),
    )
    .await;
    assert_refused_subscription(case, GUARDED_ID, refused.as_ref());

    // The pool puts that same user in the group and it signs in again, so the token it now
    // presents claims the group.
    cognito(
        &router,
        "AdminAddUserToGroup",
        json!({
            "UserPoolId": pool_id,
            "Username": USERNAME_IN_NO_GROUP,
            "GroupName": EDITORS,
        }),
    )
    .await;
    let after = id_token(&sign_in(&router, &pool_id, &client_id, USERNAME_IN_NO_GROUP).await);

    let case = "the same caller after the pool put it in the group";
    let mut socket = connected(case, &api, &after).await;
    let started = exchange(
        case,
        &mut socket,
        &started_message(GUARDED_ID, &document_for(EDITORS_FIELD), &json!({})),
    )
    .await;
    assert_eq!(
        started.as_ref().and_then(|frame| frame["type"].as_str()),
        Some("start_ack"),
        "case {case:?}: the identical start must now be acked, got {started:?}"
    );
    let post_id = "P-restored";
    create_post(case, &api, &after, post_id, CONTENT).await;
    let delivered = deliveries(case, &mut socket, &[(GUARDED_ID, EDITORS_FIELD)], post_id).await;
    assert_eq!(
        delivered,
        vec![GUARDED_ID.to_string()],
        "case {case:?}: the admitted subscription must receive the mutation"
    );

    // The guard is judged against what the PRESENTED token claims: the token minted before
    // the group claims none of it, so a connection carrying it is still refused that field.
    let case = "the token minted before the pool put the caller in the group";
    let mut stale = connected(case, &api, &before).await;
    let still_refused = exchange(
        case,
        &mut stale,
        &started_message(GUARDED_ID, &document_for(EDITORS_FIELD), &json!({})),
    )
    .await;
    assert_refused_subscription(case, GUARDED_ID, still_refused.as_ref());
}

/// The GraphQL document one case subscribes with: a subscription operation on `field`,
/// selecting the published post's own members.
fn document_for(field: &str) -> String {
    format!("subscription Watch {{ {field} {{ id content }} }}")
}

/// Open one real-time connection to the API's PUBLISHED address, presenting `token` as a
/// client's `Authorization` field, and ack it.
async fn connected(case: &str, api: &Published, token: &str) -> ClientSocket {
    // The material travels in the handshake's base64url `header` object, which is where a
    // real-time client of a GraphQL API puts a request's own fields.
    let mut socket = handshake(
        case,
        &api.realtime,
        GRAPHQL_PATH,
        true,
        Some(&json!({"host": api.realtime, "Authorization": token})),
        &[],
    )
    .await
    .unwrap_or_else(|status| {
        panic!(
            "case {case:?}: an identity the API's mode admits must complete the upgrade, got \
             {status}"
        )
    });
    let acked = exchange(case, &mut socket, &json!({"type": "connection_init"})).await;
    assert_eq!(
        acked.as_ref().and_then(|frame| frame["type"].as_str()),
        Some("connection_ack"),
        "case {case:?}: connection_init must be acked for an identity the mode admits, got \
         {acked:?}"
    );
    socket
}

/// What an `error` frame about ONE subscription says: it is addressed by that subscription's
/// own id and carries the refusal of an unauthorized call.
fn assert_refused_subscription(case: &str, subscription_id: &str, answered: Option<&Value>) {
    assert_eq!(
        answered.map(|frame| frame["type"].clone()),
        Some(json!("error")),
        "case {case:?}: a start on a field whose guard the caller does not satisfy is \
         answered an error message, got {answered:?}"
    );
    assert_eq!(
        answered.and_then(|frame| frame["id"].as_str()),
        Some(subscription_id),
        "case {case:?}: the error must be addressed by the subscription's own id, got \
         {answered:?}"
    );
    assert_eq!(
        answered.and_then(|frame| frame.pointer("/payload/errors/0").cloned()),
        Some(json!({
            "errorType": UNAUTHORIZED_ERROR_TYPE,
            "errorCode": UNAUTHORIZED_STATUS,
            "message": UNAUTHORIZED_MESSAGE,
        })),
        "case {case:?}: expected the refusal of an unauthorized call, got {answered:?}"
    );
}

/// Read one delivery per subscription of `owed` off `socket`, assert each carries the
/// published post under its own field, and answer the subscription ids delivered to, sorted.
async fn deliveries(
    case: &str,
    socket: &mut ClientSocket,
    owed: &[(&str, &str)],
    post_id: &str,
) -> Vec<String> {
    let mut delivered: Vec<String> = Vec::new();
    for _ in owed {
        let frame = next_frame(socket).await.unwrap_or_else(|| {
            panic!("case {case:?}: every live subscription of this connection receives the mutation")
        });
        assert_eq!(
            frame["type"].as_str(),
            Some("data"),
            "case {case:?}: a delivery arrives as a data frame, got {frame}"
        );
        let id = frame["id"].as_str().unwrap_or_default().to_string();
        let field = owed
            .iter()
            .find(|(known, _)| *known == id)
            .map(|(_, field)| *field)
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: a delivery is addressed to a subscription this connection \
                     started, got {frame}"
                )
            });
        assert_eq!(
            frame.pointer(&format!("/payload/data/{field}")),
            Some(&json!({"id": post_id, "content": CONTENT})),
            "case {case:?}: expected the published post under {field}, got {frame}"
        );
        delivered.push(id);
    }
    delivered.sort();
    delivered
}

/// Create one GraphQL API in the `AMAZON_COGNITO_USER_POOLS` mode over `user_pool_id`, with
/// [`SCHEMA`], a `NONE` data source and the mutation's resolver, and answer the addresses it
/// PUBLISHED — parsed out of `uris`, which is what a client dials.
async fn create_api(router: &MultiServiceRouter, name: &str, user_pool_id: &str) -> Published {
    let (status, created) = appsync(
        router,
        "POST",
        "/v1/apis",
        Some(json!({
            "name": format!("group-guard-{name}"),
            "authenticationType": "AMAZON_COGNITO_USER_POOLS",
            "userPoolConfig": {
                "userPoolId": user_pool_id,
                "awsRegion": REGION,
                "defaultAction": "ALLOW",
            },
        })),
    )
    .await;
    assert_eq!(status, 200, "CreateGraphqlApi must answer 200, got {created}");
    let api = &created["graphqlApi"];
    let api_id = api["apiId"].as_str().unwrap_or_default().to_string();
    let published = Published {
        graphql: published_authority(name, api, "GRAPHQL", GRAPHQL_SCHEME),
        realtime: published_authority(name, api, "REALTIME", REALTIME_SCHEME),
    };
    // A later read publishes the same addresses, so the case dials what any read of the API
    // hands a client.
    let (status, fetched) = appsync(router, "GET", &format!("/v1/apis/{api_id}"), None).await;
    assert_eq!(
        (status, &fetched["graphqlApi"]["uris"]),
        (200, &api["uris"]),
        "GetGraphqlApi must publish the same uris, got {fetched}"
    );
    install_schema(router, &api_id, SCHEMA).await;
    let (status, source) = appsync(
        router,
        "POST",
        &format!("/v1/apis/{api_id}/datasources"),
        Some(json!({"name": DATA_SOURCE, "type": "NONE"})),
    )
    .await;
    assert_eq!(status, 200, "CreateDataSource must answer 200, got {source}");
    let code = "export function request(ctx) {\n\
           return {payload: {id: ctx.args.id, content: ctx.args.content}};\n\
         }\n\
         export function response(ctx) { return ctx.result; }";
    let (status, resolver) = appsync(
        router,
        "POST",
        &format!("/v1/apis/{api_id}/types/Mutation/resolvers"),
        Some(json!({
            "fieldName": "createPost",
            "kind": "UNIT",
            "dataSourceName": DATA_SOURCE,
            "code": code,
            "runtime": {"name": RUNTIME.0, "runtimeVersion": RUNTIME.1},
        })),
    )
    .await;
    assert_eq!(
        status, 200,
        "CreateResolver for createPost must answer 200, got {resolver}"
    );
    published
}

/// The authority one published endpoint is reached at, read out of the URI the API published
/// for it: the scheme and path have to be the ones that endpoint is served over, and the host
/// has to be this emulator's own loopback listener rather than a host belonging to another
/// server.
fn published_authority(case: &str, api: &Value, endpoint: &str, scheme: &str) -> String {
    let published = api["uris"][endpoint].as_str().unwrap_or_else(|| {
        panic!("case {case:?}: the api must publish its {endpoint} endpoint, got {api}")
    });
    let rest = published
        .strip_prefix(&format!("{scheme}://"))
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected a {scheme} address, got {published:?}")
        });
    let (authority, path) = rest
        .split_once('/')
        .unwrap_or_else(|| panic!("case {case:?}: expected a path, got {published:?}"));
    assert_eq!(
        format!("/{path}"),
        GRAPHQL_PATH,
        "case {case:?}: expected the path this endpoint serves, got {published:?}"
    );
    assert!(
        !published.contains("amazonaws.com"),
        "case {case:?}: a published address is this emulator's own, never a host of another \
         server, got {published:?}"
    );
    // The reading of an announced address is the data-plane suites' own (`loopback_port`): it
    // is the host the core binds on, and nothing else.
    let port = loopback_port(case, authority);
    assert!(
        port > 0,
        "case {case:?}: expected the port the endpoint is served on, got {published:?}"
    );
    authority.to_string()
}

/// POST one `createPost` mutation to the API's published GraphQL endpoint, presenting
/// `token`, and assert it was served.
async fn create_post(case: &str, api: &Published, token: &str, id: &str, content: &str) {
    let document = json!({
        "query": "mutation Write($id: ID!, $content: String!) { \
                  createPost(id: $id, content: $content) { id content } }",
        "variables": {"id": id, "content": content},
    });
    let (status, _, answered) = try_post_answer(
        case,
        &api.graphql,
        GRAPHQL_PATH,
        "POST",
        &document,
        &[("authorization", token)],
    )
    .await
    .unwrap_or_else(|| {
        panic!(
            "case {case:?}: the published GraphQL endpoint {:?} must answer this request",
            api.graphql
        )
    });
    assert_eq!(
        (status, answered["data"]["createPost"]["id"].as_str()),
        (200, Some(id)),
        "case {case:?}: the mutation must be served by the API's own resolver, got {status} \
         and {answered}"
    );
}
