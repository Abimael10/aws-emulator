// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for a base path mapping WRITE issued beside the `DeleteRestApi` of the
//! API that mapping names: `CreateBasePathMapping` and `UpdateBasePathMapping` against
//! `DeleteRestApi`, with the records read back through `GetRestApi`, `GetBasePathMapping`,
//! `GetBasePathMappings` and `DeleteBasePathMapping`.
//!
//! Every case drives the real wire path — a `restJson1` HTTP request through the
//! multi-service router into the mounted service — so what is asserted is what a client
//! sees, not what a handler returns. The emulator, the request builder and the
//! `CreateRestApi` / `CreateDeployment` / `CreateDomainName` / `CreateBasePathMapping`
//! helpers are this crate's existing harness.
//!
//! The RULE under test: `DeleteRestApi`'s base-path-mapping precondition is a READ of the
//! stored mappings, and nothing evaluates it against another request's mapping write — the
//! write spends a span before the record is stored
//! (`state::BASE_PATH_MAPPING_WRITE_SPAN`) — so a mapping created or repointed onto an API
//! BESIDE that API's delete is served WITH it: the write answers its own success and the
//! delete answers 202. The mapping was admitted against state that still held the API, so it
//! outlives it: the API's own reads refuse at once, the orphaned mapping still reads back and
//! still OCCUPIES its base path against a replacement API, its own delete is refused for the
//! identifier that now resolves to no API, and what releases it is the record it belongs to,
//! the domain name. A mapping already STORED when the delete arrives is in the state that
//! read resolves, so it refuses the delete exactly as before.
//!
//! The rows differ along what the rule generalises over: which write points the mapping at
//! the API (a create, and a patch that repoints an existing mapping), the spelling of the
//! base path (a plain one, a deeper one and the `(none)` the member documents), whether the
//! mapping names a stage, and which of the two concurrent calls the runtime serves first.
//!
//! The clock is PAUSED where two calls are raced: the span a mapping write spends before the
//! record is stored runs on the runtime's clock, so virtual time carries it while nothing
//! sleeps, and the answers follow from the order the runtime serves the two calls in rather
//! than from how fast or how loaded the host is.

use serde_json::{json, Value};
use service_com_amazonaws_apigateway_backplanecontrolservice::state::{
    BASE_PATH_MAPPING_ALREADY_EXISTS, INVALID_REST_API_IDENTIFIER, NONE_BASE_PATH,
};

use crate::base_path_mapping::{
    assert_refusal, create_mapping, deployed_api, get_mapping, mapping_of,
};
use crate::base_path_mapping_references::api_refusal;
use crate::domain_name_collection::created_domain;
use crate::rest_api_identifier_refusals::no_such_api;
use crate::rest_api_lifecycle::{assert_failure, page_items, string_member, Answer, Gateway};

/// The modelled failures these cases assert, with the status each is declared at
/// (`smithy_query(query="operation",
/// operation="CreateBasePathMapping,DeleteBasePathMapping,GetBasePathMappings")`).
const BAD_REQUEST: (u16, &str) = (400, "BadRequestException");
const NOT_FOUND: (u16, &str) = (404, "NotFoundException");
const CONFLICT: (u16, &str) = (409, "ConflictException");

/// `DeleteRestApi` for `api`, whose accepted removal answers 202.
async fn delete_api(gateway: &Gateway, api: &str) -> Answer {
    gateway
        .call("DELETE", &format!("/restapis/{api}"), None)
        .await
}

/// `GetRestApi` for `api`.
async fn get_api(gateway: &Gateway, api: &str) -> Answer {
    gateway.call("GET", &format!("/restapis/{api}"), None).await
}

/// `DeleteBasePathMapping` for `base_path` of `domain_name`.
async fn delete_mapping(gateway: &Gateway, domain_name: &str, base_path: &str) -> Answer {
    gateway
        .call(
            "DELETE",
            &format!("/domainnames/{domain_name}/basepathmappings/{base_path}"),
            None,
        )
        .await
}

/// `GetBasePathMappings` for `domain_name`.
async fn list_mappings(gateway: &Gateway, domain_name: &str) -> Answer {
    gateway
        .call(
            "GET",
            &format!("/domainnames/{domain_name}/basepathmappings"),
            None,
        )
        .await
}

/// `DeleteDomainName` for `domain_name`.
async fn delete_domain(gateway: &Gateway, domain_name: &str) -> Answer {
    gateway
        .call("DELETE", &format!("/domainnames/{domain_name}"), None)
        .await
}

/// One row of the race table: the write that points a mapping at the API, the base path it
/// is held at, whether the mapping names the API's stage, and which of the two concurrent
/// calls the runtime is given first.
struct RaceRow {
    case: &'static str,
    base_path: &'static str,
    with_stage: bool,
    /// `true` for a patch that repoints an EXISTING mapping onto the API being deleted,
    /// `false` for a create of a new mapping.
    repoint: bool,
    /// `true` when the delete is the call the runtime serves first.
    delete_first: bool,
}

/// The rows the race cases run over.
fn rows() -> [RaceRow; 6] {
    [
        RaceRow {
            case: "a create at a plain base path, the write served first",
            base_path: "v1",
            with_stage: true,
            repoint: false,
            delete_first: false,
        },
        RaceRow {
            case: "a create that names no stage",
            base_path: "stageless",
            with_stage: false,
            repoint: false,
            delete_first: false,
        },
        RaceRow {
            case: "a create at the (none) base path",
            base_path: NONE_BASE_PATH,
            with_stage: true,
            repoint: false,
            delete_first: false,
        },
        RaceRow {
            case: "a create at a deeper base path, the delete served first",
            base_path: "v1-beta",
            with_stage: true,
            repoint: false,
            delete_first: true,
        },
        RaceRow {
            case: "a patch repointing a mapping onto the API, the write served first",
            base_path: "repointed",
            with_stage: true,
            repoint: true,
            delete_first: false,
        },
        RaceRow {
            case: "a patch repointing a mapping onto the API, the delete served first",
            base_path: "repointed-late",
            with_stage: true,
            repoint: true,
            delete_first: true,
        },
    ]
}

/// What the two calls handed to the runtime together answered.
struct Raced {
    write: Answer,
    deleted: Answer,
}

/// The record `BasePathMapping` reports for a row's mapping once the write has been served:
/// the base path it is held at, the API it was pointed at, and the stage it names.
fn expected_mapping(row: &RaceRow, api: &str, stage: &str) -> (String, String, Option<String>) {
    (
        row.base_path.to_string(),
        api.to_string(),
        row.with_stage.then(|| stage.to_string()),
    )
}

/// The success the row's write answers: `CreateBasePathMapping` is declared at 201 and
/// `UpdateBasePathMapping` at 200
/// (`smithy_query(query="operation", operation="CreateBasePathMapping,UpdateBasePathMapping")`).
fn write_success(row: &RaceRow) -> u16 {
    if row.repoint {
        200
    } else {
        201
    }
}

/// A deployed API and a custom domain name for the row, and — for a row that REPOINTS — a
/// mapping already held at the row's base path, pointing at a second API that survives.
///
/// Answers `(restApiId, stageName, domainName)`: the API the race deletes, the stage the
/// mapping names, and the domain name the mapping is held on (one per row, because a base
/// path is unique per domain name).
async fn staged(gateway: &Gateway, row: &RaceRow, index: usize) -> (String, String, String) {
    let (api, stage) = deployed_api(gateway, &format!("raced-api-{index}"), "prod").await;
    let domain_name = format!("raced-{index}.example.com");
    created_domain(gateway, &domain_name, json!({})).await;
    if row.repoint {
        let (surviving, surviving_stage) =
            deployed_api(gateway, &format!("surviving-api-{index}"), "prod").await;
        let created = create_mapping(
            gateway,
            &domain_name,
            json!({
                "basePath": row.base_path,
                "restApiId": surviving,
                "stage": surviving_stage,
            }),
        )
        .await;
        assert_eq!(
            created.status, 201,
            "case {}: the mapping the patch repoints must be stored first, got {} with {:?}",
            row.case, created.status, created.body
        );
    }
    (api, stage, domain_name)
}

/// Point a mapping at `api` under `domain_name` while `api`'s `DeleteRestApi` is issued
/// beside it, and answer both answers.
///
/// The two calls are handed to the runtime TOGETHER, in the order the row asks for; neither
/// waits for the other's answer, which is what makes them concurrent rather than ordered.
async fn race(
    gateway: &Gateway,
    row: &RaceRow,
    domain_name: &str,
    api: &str,
    stage: &str,
) -> Raced {
    let (method, uri, body) = if row.repoint {
        (
            "PATCH",
            format!(
                "/domainnames/{domain_name}/basepathmappings/{}",
                row.base_path
            ),
            // `restapiId` is the spelling API Gateway's own patch surface gives this member.
            json!({ "patchOperations": [
                { "op": "replace", "path": "/restapiId", "value": api },
                { "op": "replace", "path": "/stage", "value": stage },
            ] }),
        )
    } else {
        let mut body = json!({ "basePath": row.base_path, "restApiId": api });
        if row.with_stage {
            body["stage"] = json!(stage);
        }
        (
            "POST",
            format!("/domainnames/{domain_name}/basepathmappings"),
            body,
        )
    };

    let write = gateway.call(method, &uri, Some(body));
    let deleted = delete_api(gateway, api);
    if row.delete_first {
        let (deleted, write) = ::tokio::join!(deleted, write);
        Raced { write, deleted }
    } else {
        let (write, deleted) = ::tokio::join!(write, deleted);
        Raced { write, deleted }
    }
}

/// Assert both calls of a race were served: the write with its own success, the delete with
/// the 202 its accepted removal answers.
fn assert_both_served(raced: &Raced, row: &RaceRow) {
    assert_eq!(
        raced.deleted.status, 202,
        "case {}: a mapping write served beside the delete must leave the delete accepted \
         with 202, got {} with {:?}",
        row.case, raced.deleted.status, raced.deleted.body
    );
    assert_eq!(
        raced.write.status,
        write_success(row),
        "case {}: the mapping write must be served, expected {}, got {} with {:?}",
        row.case,
        write_success(row),
        raced.write.status,
        raced.write.body
    );
}

#[tokio::test(start_paused = true)]
async fn a_mapping_write_beside_the_api_s_delete_leaves_both_calls_served() {
    let gateway = Gateway::new();

    for (index, row) in rows().iter().enumerate() {
        let (api, stage, domain_name) = staged(&gateway, row, index).await;

        let raced = race(&gateway, row, &domain_name, &api, &stage).await;
        assert_both_served(&raced, row);

        // The written record names the API the write pointed it at, and the API's own
        // records went with the accepted delete: every read of them refuses at once, in the
        // sentence that names the identifier the request carried and the account the lookup
        // ran in.
        let expected = expected_mapping(row, &api, &stage);
        assert_eq!(
            mapping_of(&raced.write.body, row.case),
            expected,
            "case {}: the written mapping must name the API it was pointed at, got {:?}",
            row.case,
            raced.write.body
        );
        let read = get_api(&gateway, &api).await;
        assert_refusal(&read, NOT_FOUND, &no_such_api(&api), row.case);

        // The orphan is a record of the domain name, so it is still there to be read, and it
        // still names the API that no longer exists.
        let stored = get_mapping(&gateway, &domain_name, row.base_path).await;
        assert_eq!(
            stored.status, 200,
            "case {}: the orphaned mapping must be readable, got {} with {:?}",
            row.case, stored.status, stored.body
        );
        assert_eq!(
            mapping_of(&stored.body, row.case),
            expected,
            "case {}: the orphaned mapping must report the API it names, got {:?}",
            row.case,
            stored.body
        );
    }
}

#[tokio::test(start_paused = true)]
async fn the_orphaned_mapping_holds_its_base_path_and_goes_with_its_domain_name() {
    let gateway = Gateway::new();

    // The rows that CREATE the orphan carry this half: what the mapping goes on to hold does
    // not depend on which of the two writes wrote it, and each write's own answer is
    // asserted above.
    for (index, row) in rows().iter().filter(|row| !row.repoint).enumerate() {
        let (api, stage, domain_name) = staged(&gateway, row, index).await;
        let raced = race(&gateway, row, &domain_name, &api, &stage).await;
        assert_both_served(&raced, row);

        // NO cascade delete: the domain name's collection still reports the mapping.
        let listed = list_mappings(&gateway, &domain_name).await;
        assert_eq!(
            listed.status, 200,
            "case {}: expected 200 from GetBasePathMappings, got {} with {:?}",
            row.case, listed.status, listed.body
        );
        let base_paths: Vec<String> = page_items(&listed.body, row.case)
            .iter()
            .map(|item| string_member(item, "basePath", row.case))
            .collect();
        assert_eq!(
            base_paths,
            vec![row.base_path.to_string()],
            "case {}: the orphaned mapping must still be listed on the domain name, got \
             {base_paths:?}",
            row.case
        );

        // It still OCCUPIES its (domainName, basePath) slot: a second, LIVE API cannot be
        // mapped onto it.
        let (replacement, replacement_stage) =
            deployed_api(&gateway, &format!("replacement-api-{index}"), "prod").await;
        let refused = create_mapping(
            &gateway,
            &domain_name,
            json!({
                "basePath": row.base_path,
                "restApiId": replacement,
                "stage": replacement_stage,
            }),
        )
        .await;
        assert_refusal(
            &refused,
            CONFLICT,
            BASE_PATH_MAPPING_ALREADY_EXISTS,
            row.case,
        );

        // And its OWN delete is refused: the mapping names an identifier that resolves to no
        // API, which is that operation's bad request, and the refused delete leaves the
        // record where it was.
        let refused_delete = delete_mapping(&gateway, &domain_name, row.base_path).await;
        assert_refusal(
            &refused_delete,
            BAD_REQUEST,
            INVALID_REST_API_IDENTIFIER,
            row.case,
        );
        let survived = get_mapping(&gateway, &domain_name, row.base_path).await;
        assert_eq!(
            survived.status, 200,
            "case {}: the refused delete must leave the orphan readable, got {} with {:?}",
            row.case, survived.status, survived.body
        );

        // What releases it is the record it belongs to: the domain name's delete takes its
        // mappings with it, so the collection answers for no domain name afterwards.
        let deleted_domain = delete_domain(&gateway, &domain_name).await;
        assert_eq!(
            deleted_domain.status, 202,
            "case {}: expected 202 from DeleteDomainName, got {} with {:?}",
            row.case, deleted_domain.status, deleted_domain.body
        );
        let gone = list_mappings(&gateway, &domain_name).await;
        assert_failure(&gone, NOT_FOUND.0, NOT_FOUND.1, row.case);
    }
}

#[tokio::test]
async fn a_mapping_already_stored_when_the_delete_arrives_still_refuses_it() {
    let gateway = Gateway::new();

    // The other side of the same rule: a caller that waits for the mapping's answer before
    // deleting the API is never inside the write's span, so the delete's read resolves state
    // that holds the mapping and the delete is refused — whatever the base path's spelling
    // and whether the mapping names a stage.
    for (index, row) in rows().iter().filter(|row| !row.repoint).enumerate() {
        let (api, stage) = deployed_api(&gateway, &format!("ordered-api-{index}"), "prod").await;
        let domain_name = format!("ordered-{index}.example.com");
        created_domain(&gateway, &domain_name, json!({})).await;

        let mut body: Value = json!({ "basePath": row.base_path, "restApiId": api });
        if row.with_stage {
            body["stage"] = json!(stage);
        }
        let created = create_mapping(&gateway, &domain_name, body).await;
        assert_eq!(
            created.status, 201,
            "case {}: expected 201 from CreateBasePathMapping, got {} with {:?}",
            row.case, created.status, created.body
        );

        let refused = delete_api(&gateway, &api).await;
        assert_refusal(
            &refused,
            BAD_REQUEST,
            &api_refusal(&api, &[&domain_name]),
            row.case,
        );
        // The API survives the refusal, so nothing of it was removed on the way.
        let read = get_api(&gateway, &api).await;
        assert_eq!(
            read.status, 200,
            "case {}: the refused delete must leave the API, got {} with {:?}",
            row.case, read.status, read.body
        );
    }
}
