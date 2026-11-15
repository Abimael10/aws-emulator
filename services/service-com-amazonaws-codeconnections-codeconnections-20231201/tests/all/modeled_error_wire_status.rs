// SPDX-License-Identifier: CC0-1.0

//! The HTTP status every MODELLED error of this service arrives with.
//!
//! RULE: this service speaks `awsJson1_0` (`smithy_query(query="service_info")`:
//! protocols `awsJson1_0`, `rpcv2Cbor`), an RPC protocol on which no operation of the
//! model carries an HTTP binding at all — the `@httpError` each error shape records
//! describes a REST shape this service does not serve. So a modelled CLIENT error is
//! answered with that protocol's client status, 400, and is identified only by the error
//! shape's name in the body's `__type`, whatever `@httpError` the shape records. A client
//! that classifies by status therefore never reads a modelled service exception as a
//! missing route (404), a conflict (409) or a rate limit (429).
//!
//! Rows differ along what the rule generalises over: which operation carries the request,
//! which modelled error shape it raises, the `@httpError` that shape records (404, 409,
//! 429 and 400, so the status cannot be a copy of the trait), how the resource is
//! addressed (a connection ARN, a host ARN, a repository-link id, a resource ARN, a
//! `(SyncType, ResourceName)` pair or a blocker id), and why the error is raised (never
//! created, deleted, already taken, still referenced, or a cap exhausted).
//!
//! Every request goes through the real `MultiServiceRouter` dispatch, reusing the
//! `connection_lifecycle` suite's helpers, so these tests exercise the same path a signed
//! SDK request takes.

use serde_json::Value;
use service_com_amazonaws_codeconnections_codeconnections_20231201::scaffolded::wire_status;
use service_com_amazonaws_codeconnections_codeconnections_20231201::state::MAX_TAGS_PER_RESOURCE;
use smithy_server_runtime::MultiServiceRouter;

use super::connection_lifecycle::{REGION, call, create, router};
use super::git_sync::{SYNC_TYPE, error_code, role_arn};
use super::host_association::create_plain_host;

/// The status the `awsJson1_0` client-error channel uses, which is the status under test.
const CLIENT_ERROR_STATUS: u16 = 400;

/// One modelled error shape of this service, with the `@httpError` the model records for
/// it.
///
/// Every value below is from `smithy_query(query="shape", shape=...)` on this service's
/// model: `errorKind client` throughout, with the `httpError` named per row. Those are
/// the statuses the wire must NOT use where they differ from [`CLIENT_ERROR_STATUS`].
#[derive(Debug, Clone, Copy)]
struct ModeledError {
    /// The wire error code: the error shape's name, as `__type` carries it.
    code: &'static str,
    /// The `@httpError` the model records for that shape.
    inert_http_error: u16,
}

/// An identifier that resolves to no resource this account holds (`httpError` 404).
const NOT_FOUND: ModeledError = ModeledError {
    code: "ResourceNotFoundException",
    inert_http_error: 404,
};

/// A resource a create would mint that is already there (`httpError` 409).
const ALREADY_EXISTS: ModeledError = ModeledError {
    code: "ResourceAlreadyExistsException",
    inert_http_error: 409,
};

/// A repository link a sync configuration still references (`httpError` 409).
const STILL_EXISTS: ModeledError = ModeledError {
    code: "SyncConfigurationStillExistsException",
    inert_http_error: 409,
};

/// A request past a cap this service enforces (`httpError` 429).
const LIMIT_EXCEEDED: ModeledError = ModeledError {
    code: "LimitExceededException",
    inert_http_error: 429,
};

/// A sync blocker id that resolves to no blocker of the addressed resource
/// (`httpError` 404).
const BLOCKER_DOES_NOT_EXIST: ModeledError = ModeledError {
    code: "SyncBlockerDoesNotExistException",
    inert_http_error: 404,
};

/// An input this surface cannot resolve (`httpError` 400): the row where the mapping is
/// the identity, so the table also pins what it must leave alone.
const INVALID_INPUT: ModeledError = ModeledError {
    code: "InvalidInputException",
    inert_http_error: 400,
};

/// A resource that IS there but is not usable (`httpError` 400): the second identity row.
const UNAVAILABLE: ModeledError = ModeledError {
    code: "ResourceUnavailableException",
    inert_http_error: 400,
};

/// One row: a request, and the modelled error it must raise.
#[derive(Debug)]
struct Row {
    case: String,
    operation: &'static str,
    request: Value,
    expected: ModeledError,
}

/// `count` distinct tags, as the `Tags` member takes them.
fn tags(count: usize) -> Value {
    Value::Array(
        (0..count)
            .map(|index| serde_json::json!({ "Key": format!("key-{index}"), "Value": "v" }))
            .collect(),
    )
}

/// An ARN of `kind` naming `id`, in the region the suite signs for.
fn arn(kind: &str, id: &str) -> String {
    format!(
        "arn:aws:codeconnections:{REGION}:{}:{kind}/{id}",
        engine::emulator_account_id()
    )
}

/// Create one repository link on `connection_arn` and return its id.
///
/// `git_sync::create_link` mints a connection of its own for each link; the rows below need
/// a second link on the SAME connection, which is why this one takes the connection ARN.
async fn link_on(
    router: &MultiServiceRouter,
    connection_arn: &str,
    owner_id: &str,
    repository_name: &str,
) -> String {
    let (status, body) = call(
        router,
        "CreateRepositoryLink",
        serde_json::json!({
            "ConnectionArn": connection_arn,
            "OwnerId": owner_id,
            "RepositoryName": repository_name,
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup {owner_id}/{repository_name}: CreateRepositoryLink answered {status} {body}"
    );
    body["RepositoryLinkInfo"]["RepositoryLinkId"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: no RepositoryLinkId in {body}"))
        .to_string()
}

/// The resources every row below is read against, so no refusal can come from an empty
/// store.
struct Live {
    connection_name: &'static str,
    connection_arn: String,
    host_arn: String,
    link_id: String,
    link_owner: &'static str,
    link_repository: &'static str,
    resource_name: &'static str,
    tagged_arn: String,
}

/// Stand up the live resources: a connection, a host with a connection on it, a
/// repository link with a sync configuration referencing it, and a resource filled to the
/// per-resource tag cap.
async fn live(router: &MultiServiceRouter) -> Live {
    let connection_name = "wire-status-live";
    let connection_arn = create(router, connection_name, "GitHub").await;

    // A host with a connection on it: deleting it breaks a documented precondition, which
    // is the `ResourceUnavailableException` row.
    let host_arn = create_plain_host(router, "wire-status-host", "GitHubEnterpriseServer").await;
    let (status, body) = call(
        router,
        "CreateConnection",
        serde_json::json!({
            "ConnectionName": "wire-status-on-host",
            "ProviderType": "GitHubEnterpriseServer",
            "HostArn": host_arn,
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateConnection on the live host answered {status} {body}"
    );

    let link_owner = "acme";
    let link_repository = "platform";
    let link_id = link_on(router, &connection_arn, link_owner, link_repository).await;

    let resource_name = "WireStatusStack";
    let (status, body) = call(
        router,
        "CreateSyncConfiguration",
        serde_json::json!({
            "Branch": "main",
            "ConfigFile": "stack.yaml",
            "RepositoryLinkId": link_id,
            "ResourceName": resource_name,
            "RoleArn": role_arn("wire-status"),
            "SyncType": SYNC_TYPE,
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateSyncConfiguration answered {status} {body}"
    );

    // A connection carrying exactly the per-resource tag cap, so one more key is past it.
    let tagged_arn = create(router, "wire-status-tagged", "GitLab").await;
    let (status, body) = call(
        router,
        "TagResource",
        serde_json::json!({ "ResourceArn": tagged_arn, "Tags": tags(MAX_TAGS_PER_RESOURCE) }),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: TagResource up to the cap answered {status} {body}"
    );

    Live {
        connection_name,
        connection_arn,
        host_arn,
        link_id,
        link_owner,
        link_repository,
        resource_name,
        tagged_arn,
    }
}

/// The table: every modelled error this service raises, at the entry points that raise
/// it.
async fn rows(router: &MultiServiceRouter, live: &Live) -> Vec<Row> {
    let mut rows: Vec<Row> = Vec::new();

    // A connection this account held and deleted is in the same class as one it never
    // minted, so both forms of absence are rows.
    let deleted_connection = create(router, "wire-status-doomed", "Bitbucket").await;
    let (status, body) = call(
        router,
        "DeleteConnection",
        serde_json::json!({ "ConnectionArn": deleted_connection }),
    )
    .await;
    assert_eq!(status, 200, "setup: DeleteConnection answered {status} {body}");

    for (why, connection_arn) in [
        (
            "never minted",
            arn("connection", "11111111-2222-3333-4444-555555555555"),
        ),
        ("deleted", deleted_connection),
        (
            "held by another account",
            live.connection_arn.replace(engine::emulator_account_id(), "314159265358"),
        ),
    ] {
        for (operation, member) in [
            ("GetConnection", "ConnectionArn"),
            ("DeleteConnection", "ConnectionArn"),
            ("ListTagsForResource", "ResourceArn"),
        ] {
            rows.push(Row {
                case: format!("{operation} on a connection {why}"),
                operation,
                request: serde_json::json!({ member: connection_arn }),
                expected: NOT_FOUND,
            });
        }
    }

    // A host ARN that resolves to nothing, as the addressed resource, as a reference and
    // as a filter.
    let absent_host = arn("host", "66666666-7777-8888-9999-aaaaaaaaaaaa");
    for (operation, request) in [
        ("GetHost", serde_json::json!({ "HostArn": absent_host })),
        ("DeleteHost", serde_json::json!({ "HostArn": absent_host })),
        ("UpdateHost", serde_json::json!({ "HostArn": absent_host })),
        (
            "ListConnections",
            serde_json::json!({ "HostArnFilter": absent_host }),
        ),
        (
            "CreateConnection",
            serde_json::json!({
                "ConnectionName": "wire-status-refused",
                "ProviderType": "GitHubEnterpriseServer",
                "HostArn": absent_host,
            }),
        ),
    ] {
        rows.push(Row {
            case: format!("{operation} naming a host that resolves to nothing"),
            operation,
            request,
            expected: NOT_FOUND,
        });
    }

    // A repository-link id that resolves to nothing, and a repository-link ARN that does.
    let absent_link_id = "bbbbbbbb-cccc-dddd-eeee-ffffffffffff";
    let absent_link_arn = arn("repository-link", absent_link_id);
    for (operation, request) in [
        (
            "GetRepositoryLink",
            serde_json::json!({ "RepositoryLinkId": absent_link_id }),
        ),
        (
            "UpdateRepositoryLink",
            serde_json::json!({ "RepositoryLinkId": absent_link_id }),
        ),
        (
            "TagResource",
            serde_json::json!({
                "ResourceArn": absent_link_arn,
                "Tags": [{ "Key": "env", "Value": "test" }],
            }),
        ),
        (
            "UntagResource",
            serde_json::json!({ "ResourceArn": absent_link_arn, "TagKeys": ["env"] }),
        ),
    ] {
        rows.push(Row {
            case: format!("{operation} naming a repository link that resolves to nothing"),
            operation,
            request,
            expected: NOT_FOUND,
        });
    }

    // The sync surface: a pair holding no configuration, and a live link no sync has run
    // for.
    rows.push(Row {
        case: "GetSyncConfiguration for a pair holding no configuration".to_string(),
        operation: "GetSyncConfiguration",
        request: serde_json::json!({ "SyncType": SYNC_TYPE, "ResourceName": "NoSuchStack" }),
        expected: NOT_FOUND,
    });
    rows.push(Row {
        case: "GetRepositorySyncStatus on a link no sync has run for".to_string(),
        operation: "GetRepositorySyncStatus",
        request: serde_json::json!({
            "Branch": "main",
            "RepositoryLinkId": live.link_id,
            "SyncType": SYNC_TYPE,
        }),
        expected: NOT_FOUND,
    });

    // The shapes whose `@httpError` is 409: a resource a create would mint twice, and a
    // link something still references.
    rows.push(Row {
        case: "CreateConnection for a name this account already holds".to_string(),
        operation: "CreateConnection",
        request: serde_json::json!({
            "ConnectionName": live.connection_name,
            "ProviderType": "GitLab",
        }),
        expected: ALREADY_EXISTS,
    });
    rows.push(Row {
        case: "CreateRepositoryLink for a triple that is already linked".to_string(),
        operation: "CreateRepositoryLink",
        request: serde_json::json!({
            "ConnectionArn": live.connection_arn,
            "OwnerId": live.link_owner,
            "RepositoryName": live.link_repository,
        }),
        expected: ALREADY_EXISTS,
    });
    rows.push(Row {
        case: "CreateSyncConfiguration for a pair that is already configured".to_string(),
        operation: "CreateSyncConfiguration",
        request: serde_json::json!({
            "Branch": "release",
            "ConfigFile": "stack.yaml",
            "RepositoryLinkId": live.link_id,
            "ResourceName": live.resource_name,
            "RoleArn": role_arn("wire-status"),
            "SyncType": SYNC_TYPE,
        }),
        expected: ALREADY_EXISTS,
    });
    rows.push(Row {
        case: "DeleteRepositoryLink on a link a sync configuration references".to_string(),
        operation: "DeleteRepositoryLink",
        request: serde_json::json!({ "RepositoryLinkId": live.link_id }),
        expected: STILL_EXISTS,
    });

    // The shape whose `@httpError` is 429: a request past the per-resource tag cap.
    rows.push(Row {
        case: format!("TagResource one key past the cap of {MAX_TAGS_PER_RESOURCE}"),
        operation: "TagResource",
        request: serde_json::json!({
            "ResourceArn": live.tagged_arn,
            "Tags": [{ "Key": "one-too-many", "Value": "v" }],
        }),
        expected: LIMIT_EXCEEDED,
    });

    // The second shape whose `@httpError` is 404, raised by a different resolution step.
    for resource_name in [live.resource_name, "UnconfiguredStack"] {
        rows.push(Row {
            case: format!("UpdateSyncBlocker with an unknown blocker id on {resource_name}"),
            operation: "UpdateSyncBlocker",
            request: serde_json::json!({
                "Id": "blocker-1",
                "SyncType": SYNC_TYPE,
                "ResourceName": resource_name,
                "ResolvedReason": "resolved by the test",
            }),
            expected: BLOCKER_DOES_NOT_EXIST,
        });
    }

    // The identity rows: a modelled error whose `@httpError` is already this protocol's
    // client status keeps it, so the mapping cannot be read as "answer 400 for
    // everything the model says 404".
    rows.push(Row {
        case: "GetRepositorySyncStatus naming a link that resolves to nothing".to_string(),
        operation: "GetRepositorySyncStatus",
        request: serde_json::json!({
            "Branch": "main",
            "RepositoryLinkId": absent_link_id,
            "SyncType": SYNC_TYPE,
        }),
        expected: INVALID_INPUT,
    });
    rows.push(Row {
        case: "UpdateSyncConfiguration for a pair holding no configuration".to_string(),
        operation: "UpdateSyncConfiguration",
        request: serde_json::json!({
            "SyncType": SYNC_TYPE,
            "ResourceName": "NoSuchStack",
            "Branch": "main",
        }),
        expected: INVALID_INPUT,
    });
    rows.push(Row {
        case: "DeleteHost on a host a connection still references".to_string(),
        operation: "DeleteHost",
        request: serde_json::json!({ "HostArn": live.host_arn }),
        expected: UNAVAILABLE,
    });

    rows
}

#[tokio::test]
async fn every_modelled_error_arrives_with_this_protocols_client_status() {
    let router = router();
    let live = live(&router).await;
    let table = rows(&router, &live).await;

    // The table must really span the shapes whose modelled status differs, or it could
    // pass while testing one error.
    let covered: std::collections::BTreeSet<u16> = table
        .iter()
        .map(|row| row.expected.inert_http_error)
        .collect();
    assert_eq!(
        covered,
        [400u16, 404, 409, 429].into_iter().collect(),
        "the table must span every modelled client status this service records, got {covered:?}"
    );

    for row in &table {
        let case = &row.case;
        let (status, body) = call(&router, row.operation, row.request.clone()).await;
        assert_eq!(
            status, CLIENT_ERROR_STATUS,
            "case {case:?}: expected HTTP {CLIENT_ERROR_STATUS}, answered {status} {body}"
        );
        assert_eq!(
            error_code(&body),
            row.expected.code,
            "case {case:?}: expected the wire code {:?}, answered {status} {body}",
            row.expected.code
        );
        assert!(
            !body["Message"]
                .as_str()
                .or_else(|| body["message"].as_str())
                .unwrap_or_default()
                .is_empty(),
            "case {case:?}: the error carries a message, answered {status} {body}"
        );
        if row.expected.inert_http_error != CLIENT_ERROR_STATUS {
            assert_ne!(
                status, row.expected.inert_http_error,
                "case {case:?}: the shape's @httpError is inert on this protocol, answered \
                 {status} {body}"
            );
        }
    }
}

#[tokio::test]
async fn a_served_call_is_untouched_by_the_error_status_mapping() {
    // The control for the table above: the mapping is on the error channel only, so every
    // request this service serves still answers 200, and a request it refuses outside its
    // model still answers the client status with the refusal channel's own code.
    let router = router();
    let live = live(&router).await;

    for (operation, request) in [
        (
            "GetConnection",
            serde_json::json!({ "ConnectionArn": live.connection_arn }),
        ),
        ("GetHost", serde_json::json!({ "HostArn": live.host_arn })),
        (
            "GetRepositoryLink",
            serde_json::json!({ "RepositoryLinkId": live.link_id }),
        ),
        (
            "GetSyncConfiguration",
            serde_json::json!({ "SyncType": SYNC_TYPE, "ResourceName": live.resource_name }),
        ),
        ("ListConnections", serde_json::json!({})),
    ] {
        let (status, body) = call(&router, operation, request).await;
        assert_eq!(
            status, 200,
            "case {operation}: a served call answered {status} {body}"
        );
        assert!(
            body.get("__type").is_none(),
            "case {operation}: a served call carries no error code, answered {body}"
        );
    }

    // A value outside a modelled constraint is refused at the boundary, which this
    // service's model declares no error shape for: the same client status, under the
    // refusal channel's own code.
    let (status, refused) = call(
        &router,
        "ListConnections",
        serde_json::json!({ "ProviderTypeFilter": "NotAProvider" }),
    )
    .await;
    assert_eq!(
        (status, error_code(&refused)),
        (CLIENT_ERROR_STATUS, "ValidationException"),
        "an unmodelled refusal answered {status} {refused}"
    );
}

#[test]
fn the_mapping_answers_this_protocols_client_status_for_every_modelled_client_error() {
    // The mapping stated as the statuses this service's own error shapes record, rather
    // than as the branch the implementation uses: each `@httpError` the model declares for
    // a CLIENT error becomes 400, each SERVER one is left alone, and the edges of the
    // client range are pinned so the rule cannot drift into the 3xx or 5xx space.
    //
    // (case, the modelled `@httpError`, the status the wire must carry)
    let rows: [(&str, u16, u16); 10] = [
        ("ResourceNotFoundException / SyncBlockerDoesNotExistException", 404, 400),
        (
            "ResourceAlreadyExistsException / ConcurrentModificationException / \
             ConflictException / ConditionalCheckFailedException / \
             SyncConfigurationStillExistsException / UpdateOutOfSyncException",
            409,
            400,
        ),
        ("LimitExceededException / ThrottlingException", 429, 400),
        ("AccessDeniedException", 403, 400),
        (
            "InvalidInputException / ResourceUnavailableException / \
             UnsupportedProviderTypeException / UnsupportedOperationException",
            400,
            400,
        ),
        ("InternalServerException / RetryLatestCommitFailedException", 503, 503),
        ("the first status of the client range", 400, 400),
        ("the last status of the client range", 499, 400),
        ("the status below the client range", 399, 399),
        ("the first status above the client range", 500, 500),
    ];

    for (case, modeled, expected) in rows {
        let actual = wire_status(modeled);
        assert_eq!(
            actual, expected,
            "case {case:?} (@httpError {modeled}): expected {expected}, got {actual}"
        );
    }
}
