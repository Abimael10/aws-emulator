// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the REFERENCE a record outside a REST API's own tree may still be
//! pointed AT while that API's accepted removal is in flight — the `BasePathMapping` of a
//! custom domain name, created (`CreateBasePathMapping`) or repointed
//! (`UpdateBasePathMapping`), read back through `GetBasePathMapping` and `GetRestApi`.
//!
//! Every case drives the real wire path — a `restJson1` HTTP request through the
//! multi-service router into the mounted service — so what is asserted is what a client
//! sees, not what a handler returns. The emulator, the request builder and the
//! `CreateRestApi` / `CreateDeployment` / `CreateDomainName` / `CreateBasePathMapping`
//! helpers are this crate's existing harness.
//!
//! The RULE under test: `DeleteRestApi` answers 202 — the removal is ACCEPTED, not
//! finished — so the API's own records are gone the moment it answers (`GetRestApi` refuses
//! at once) while the removal is still travelling through the control plane; for that span
//! the API id still RESOLVES for a call that points a record of another resource at it, so
//! a base path mapping created or repointed in the window is stored and outlives the API it
//! names. An id no API of the account ever answered to is refused as it always was — 400
//! `BadRequestException`, `Invalid REST API identifier specified` — and so is one whose
//! removal has gone through.
//!
//! The rows differ along what the rule generalises over: the spelling of the base path (a
//! plain one, a deeper one and the `(none)` the member documents), whether the mapping names
//! a stage of the API at all, and whether the reference is CREATED inside the window or an
//! existing mapping is REPOINTED into it.

use serde_json::{json, Value};
use service_com_amazonaws_apigateway_backplanecontrolservice::state::{
    NONE_BASE_PATH, NO_SUCH_STAGE,
};

use crate::base_path_mapping::{
    assert_refusal, create_mapping, deployed_api, get_mapping, mapping_of,
};
use crate::domain_name_collection::created_domain;
use crate::rest_api_lifecycle::{Answer, Gateway};

/// The modelled refusal of a `restApiId` that resolves to nothing: `CreateBasePathMapping`
/// declares `BadRequestException` at HTTP 400
/// (`smithy_query(query="operation", operation="CreateBasePathMapping")`).
const BAD_REQUEST: (u16, &str) = (400, "BadRequestException");

/// The sentence API Gateway answers a `restApiId` that resolves to no API with, written out
/// here rather than taken from the handler's own constant: entirely fixed text, quoting
/// neither the value nor the member that carried it.
const INVALID_REST_API_IDENTIFIER: &str = "Invalid REST API identifier specified";

/// How long this suite gives the accepted removal to go through before giving up on it: far
/// longer than the span itself, so a loaded host cannot make the wait the thing that fails.
const CLOSING_DEADLINE: ::std::time::Duration = ::std::time::Duration::from_secs(60);

/// How often the closing window is looked at again; see [`CLOSING_DEADLINE`].
const POLL_INTERVAL: ::std::time::Duration = ::std::time::Duration::from_millis(50);

/// `DeleteRestApi` for `api`.
async fn delete_api(gateway: &Gateway, api: &str) -> Answer {
    gateway.call("DELETE", &format!("/restapis/{api}"), None).await
}

/// `GetRestApi` for `api`.
async fn get_api(gateway: &Gateway, api: &str) -> Answer {
    gateway.call("GET", &format!("/restapis/{api}"), None).await
}

/// `UpdateBasePathMapping` for `base_path` of `domain_name`, with `patches` sent as given.
async fn patch_mapping(
    gateway: &Gateway,
    domain_name: &str,
    base_path: &str,
    patches: Value,
) -> Answer {
    gateway
        .call(
            "PATCH",
            &format!("/domainnames/{domain_name}/basepathmappings/{base_path}"),
            Some(json!({ "patchOperations": patches })),
        )
        .await
}

/// One row of the table: the case, the base path the reference is held at, and whether the
/// mapping names the API's stage.
struct ReferenceRow {
    case: &'static str,
    base_path: &'static str,
    with_stage: bool,
}

/// The rows both assertions below run over: what the rule generalises over is the base
/// path's spelling and whether the reference names a stage, because a mapping references its
/// API either way.
fn rows() -> [ReferenceRow; 4] {
    [
        ReferenceRow {
            case: "a mapping that names the API's stage",
            base_path: "v1",
            with_stage: true,
        },
        ReferenceRow {
            case: "a mapping that names no stage",
            base_path: "stageless",
            with_stage: false,
        },
        ReferenceRow {
            case: "a mapping at the (none) base path",
            base_path: NONE_BASE_PATH,
            with_stage: true,
        },
        ReferenceRow {
            case: "a mapping at a deeper base path",
            base_path: "v1-beta",
            with_stage: true,
        },
    ]
}

#[tokio::test]
async fn a_mapping_created_while_the_api_s_accepted_delete_is_in_flight_outlives_the_api() {
    let gateway = Gateway::new();

    for (index, row) in rows().iter().enumerate() {
        let (api, stage) = deployed_api(&gateway, &format!("accepted-delete-{index}"), "prod").await;
        let domain_name = format!("accepted-delete-{index}.example.com");
        created_domain(&gateway, &domain_name, json!({})).await;

        // The delete is ACCEPTED: 202, and the API's own records are gone at once.
        let deleted = delete_api(&gateway, &api).await;
        assert_eq!(
            deleted.status, 202,
            "case {}: DeleteRestApi must accept the delete with 202, got {} with {:?}",
            row.case, deleted.status, deleted.body
        );
        // While that removal is in flight the id still resolves for a record pointed at
        // it, so the mapping is CREATED — naming an API that is already gone. The create is
        // the first call after the delete, so nothing but it can run out the window.
        let mut request = json!({ "basePath": row.base_path, "restApiId": api });
        if row.with_stage {
            request["stage"] = json!(stage);
        }
        let created = create_mapping(&gateway, &domain_name, request).await;
        assert_eq!(
            created.status, 201,
            "case {}: a mapping created while the accepted delete is in flight must be \
             stored, got {} with {:?}",
            row.case, created.status, created.body
        );
        // The API's own records went with the acceptance: the delete answered 202 and
        // nothing of the API reads back, mapping or no mapping.
        let read = get_api(&gateway, &api).await;
        assert_eq!(
            read.status, 404,
            "case {}: the accepted delete must leave GetRestApi refusing, got {} with {:?}",
            row.case, read.status, read.body
        );
        let expected = (
            row.base_path.to_string(),
            api.clone(),
            row.with_stage.then(|| stage.clone()),
        );
        assert_eq!(
            mapping_of(&created.body, row.case),
            expected,
            "case {}: the stored mapping must name the API it was created against, got {:?}",
            row.case,
            created.body
        );

        // The orphan is a record of the domain name, so it is still there to be read, and
        // it still names the API that no longer exists.
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
        assert_eq!(
            get_api(&gateway, &api).await.status,
            404,
            "case {}: the orphaned mapping must not bring the API back",
            row.case
        );
    }
}

#[tokio::test]
async fn a_mapping_repointed_while_the_api_s_accepted_delete_is_in_flight_names_the_deleted_api() {
    let gateway = Gateway::new();

    for (index, row) in rows().iter().enumerate() {
        // The mapping starts out on an API that survives and is repointed at the one whose
        // removal is in flight: the patch resolves the reference by the same rule the
        // create does.
        let (surviving, surviving_stage) =
            deployed_api(&gateway, &format!("repoint-from-{index}"), "prod").await;
        let (doomed, doomed_stage) =
            deployed_api(&gateway, &format!("repoint-to-{index}"), "prod").await;
        let domain_name = format!("repoint-{index}.example.com");
        created_domain(&gateway, &domain_name, json!({})).await;
        let created = create_mapping(
            &gateway,
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
            "case {}: expected 201 from CreateBasePathMapping, got {} with {:?}",
            row.case, created.status, created.body
        );

        let deleted = delete_api(&gateway, &doomed).await;
        assert_eq!(
            deleted.status, 202,
            "case {}: DeleteRestApi must accept the delete with 202, got {} with {:?}",
            row.case, deleted.status, deleted.body
        );

        // `restapiId` is the spelling API Gateway's own patch surface gives this member.
        let stage = if row.with_stage {
            doomed_stage.clone()
        } else {
            NONE_BASE_PATH.to_string()
        };
        let patched = patch_mapping(
            &gateway,
            &domain_name,
            row.base_path,
            json!([
                { "op": "replace", "path": "/restapiId", "value": doomed },
                { "op": "replace", "path": "/stage", "value": stage },
            ]),
        )
        .await;
        assert_eq!(
            patched.status, 200,
            "case {}: a mapping repointed while the accepted delete is in flight must be \
             stored, got {} with {:?}",
            row.case, patched.status, patched.body
        );
        assert_eq!(
            mapping_of(&patched.body, row.case).1,
            doomed,
            "case {}: the repointed mapping must name the API the patch moved it to, got \
             {:?}",
            row.case,
            patched.body
        );
    }
}

#[tokio::test]
async fn a_stage_the_deleted_api_never_carried_is_refused_inside_the_window_too() {
    let gateway = Gateway::new();
    // The window changes WHICH state a `stage` is resolved against, never whether it is
    // resolved: the racing call reached state that held the API's stages, so a name no stage
    // of it answered to is refused there exactly as it is for a stored API. The rows differ
    // along the spelling of the unknown stage, including one that differs from the API's own
    // only in case.
    let (api, stage) = deployed_api(&gateway, "window-stage-guard", "prod").await;
    let domain_name = "window-stage-guard.example.com";
    created_domain(&gateway, domain_name, json!({})).await;

    let deleted = delete_api(&gateway, &api).await;
    assert_eq!(
        deleted.status, 202,
        "DeleteRestApi must accept the delete with 202, got {} with {:?}",
        deleted.status, deleted.body
    );

    for (case, requested_stage) in [
        ("a stage the API never carried", "absent"),
        ("the API's own stage in another case", "PROD"),
        ("an empty-looking stage name", "-"),
    ] {
        let refused = create_mapping(
            &gateway,
            domain_name,
            json!({
                "basePath": format!("stage-{}", requested_stage.to_ascii_lowercase()),
                "restApiId": api,
                "stage": requested_stage,
            }),
        )
        .await;
        assert_refusal(&refused, BAD_REQUEST, NO_SUCH_STAGE, case);
    }

    // The stage the API DID carry is still admitted in the same window, which is what makes
    // the refusals above the stage guard's doing and not the window's.
    let created = create_mapping(
        &gateway,
        domain_name,
        json!({ "basePath": "carried", "restApiId": api, "stage": stage }),
    )
    .await;
    assert_eq!(
        created.status, 201,
        "case \"the stage the API carried\": expected 201, got {} with {:?}",
        created.status, created.body
    );
}

#[tokio::test]
async fn a_rest_api_id_that_resolves_to_nothing_is_still_refused() {
    let gateway = Gateway::new();
    let domain_name = "unresolvable-reference.example.com";
    created_domain(&gateway, domain_name, json!({})).await;

    // An id no API of the account ever answered to is refused whatever its spelling: the
    // window only ever covers an id this account HELD and whose delete it accepted.
    for (case, rest_api_id) in [
        ("an unknown restApiId", "nonexist12"),
        ("a restApiId of the wrong shape", "not an api"),
        ("a restApiId carrying an account prefix", "000000000000:api"),
    ] {
        let refused = create_mapping(
            &gateway,
            domain_name,
            json!({
                "basePath": format!("fresh-{}", rest_api_id.len()),
                "restApiId": rest_api_id,
            }),
        )
        .await;
        assert_refusal(&refused, BAD_REQUEST, INVALID_REST_API_IDENTIFIER, case);
    }
}

#[tokio::test]
async fn once_the_accepted_delete_has_gone_through_the_api_id_resolves_to_nothing() {
    let gateway = Gateway::new();
    let (api, _stage) = deployed_api(&gateway, "window-closes", "prod").await;
    let domain_name = "window-closes.example.com";
    created_domain(&gateway, domain_name, json!({})).await;

    let deleted = delete_api(&gateway, &api).await;
    assert_eq!(
        deleted.status, 202,
        "DeleteRestApi must accept the delete with 202, got {} with {:?}",
        deleted.status, deleted.body
    );

    // PAST the window the id resolves to nothing, like any other id the account does not
    // hold. The wait is on that CONDITION — looked for again until it holds or a generous
    // deadline passes — rather than on a duration, and each attempt asks under a base path
    // of its own so an admitted one is never a second create of the same mapping.
    let giving_up_at = ::std::time::Instant::now() + CLOSING_DEADLINE;
    let mut attempt = 0_u32;
    let refused = loop {
        let answer = create_mapping(
            &gateway,
            domain_name,
            json!({ "basePath": format!("late-{attempt}"), "restApiId": api }),
        )
        .await;
        if answer.status == BAD_REQUEST.0 {
            break answer;
        }
        assert!(
            ::std::time::Instant::now() < giving_up_at,
            "case \"past the window\": expected CreateBasePathMapping to stop resolving \
             {api} once the accepted delete has gone through, still got {} with {:?} after \
             {attempt} attempts",
            answer.status,
            answer.body
        );
        attempt = attempt.saturating_add(1);
        ::tokio::time::sleep(POLL_INTERVAL).await;
    };
    assert_refusal(
        &refused,
        BAD_REQUEST,
        INVALID_REST_API_IDENTIFIER,
        "past the window",
    );
}
