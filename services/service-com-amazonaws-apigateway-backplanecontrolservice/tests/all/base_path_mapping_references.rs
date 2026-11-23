// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the INDEPENDENT records a `BasePathMapping` holds a REFERENCE to,
//! and what that reference refuses: `DeleteRestApi` and `DeleteStage`, read back through
//! `GetRestApi` and `GetStage`.
//!
//! Every case drives the real wire path — a `restJson1` HTTP request through the
//! multi-service router into the mounted service — so what is asserted is what a client
//! sees, not what a handler returns. The emulator, the request builder and the
//! `CreateRestApi` / `CreateDeployment` / `CreateDomainName` / `CreateBasePathMapping`
//! helpers are this crate's existing harness.
//!
//! The RULE under test: a base path mapping routes the calls arriving on its base path of
//! a custom domain name to a STAGE of a REST API, so while it stands that stage may not be
//! deleted — `DeleteStage` is refused with the modelled `BadRequestException`, naming the
//! domain names whose mappings stand in the way, and every record survives. The API ITSELF
//! carries no such precondition: `DeleteRestApi` removes the API while a mapping names it
//! and the mapping is LEFT BEHIND, naming an id that now resolves to nothing. Deleting the
//! mapping releases the stage. Nothing else is held: a stage no mapping points at deletes
//! as it always did. The custom domain name that HOLDS the mapping is NOT one of those
//! records — a mapping is a subordinate record of it and is deleted with it — which
//! `domain_name_delete_cascade` is the suite for.
//!
//! The rows differ along what the rule generalises over: the spelling of the base path
//! (a plain one and the `(none)` the member documents), whether the mapping names a stage
//! at all, how many mappings the domain name holds, and which of several stages and APIs
//! the mapping points at.

use serde_json::json;
use service_com_amazonaws_apigateway_backplanecontrolservice::state::NONE_BASE_PATH;

use crate::base_path_mapping::{create_mapping, deployed_api, get_mapping};
use crate::domain_name_collection::created_domain;
use crate::rest_api_lifecycle::{assert_failure, Answer, Gateway};
use crate::stage_lifecycle::{create_deployed_stage, get_stage};

const BAD_REQUEST: (u16, &str) = (400, "BadRequestException");

/// `DeleteStage` for `stage_name` of `api`.
async fn delete_stage(gateway: &Gateway, api: &str, stage_name: &str) -> Answer {
    gateway
        .call(
            "DELETE",
            &format!("/restapis/{api}/stages/{stage_name}"),
            None,
        )
        .await
}

/// `DeleteRestApi` for `api`.
async fn delete_api(gateway: &Gateway, api: &str) -> Answer {
    gateway
        .call("DELETE", &format!("/restapis/{api}"), None)
        .await
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

/// Assert `answer` is the modelled `BadRequestException` carrying exactly `message`.
fn assert_bad_request(answer: &Answer, message: &str, case: &str) {
    assert_failure(answer, BAD_REQUEST.0, BAD_REQUEST.1, case);
    assert_eq!(
        answer.body["message"].as_str(),
        Some(message),
        "case {case}: expected the message {message:?}, got {:?}",
        answer.body["message"]
    );
}

/// The refusal AWS states for the STAGE a base path mapping of `domain_names` still names,
/// which is the record `DeleteStage` addresses, written out here rather than taken from the
/// handler's own renderer: the FIXED text is what this test pins, and the VARIABLE parts
/// are the identifier the request addressed and the domain names the mappings belong to.
///
/// `pub(crate)` so the sibling `refused_control_plane_writes` suite, which drives this same
/// refusal as one row of its table, states the sentence the one way rather than keeping a
/// second copy of it.
pub(crate) fn stage_refusal(stage_name: &str, domain_names: &[&str]) -> String {
    format!(
        "Deleting stage {stage_name} failed. Please remove all base path mappings related \
         to the stage in your domains: {}",
        domain_names.join(", ")
    )
}

/// One row: the case, the base path the mapping is created under, and whether the mapping
/// names the stage.
struct MappingRow {
    case: &'static str,
    base_path: &'static str,
    with_stage: bool,
}

#[tokio::test]
async fn a_stage_a_base_path_mapping_still_names_cannot_be_deleted() {
    let gateway = Gateway::new();
    let (api, stage) = deployed_api(&gateway, "held-stage-api", "prod").await;
    // A second stage of the same API and a second API with a same-named stage: neither is
    // the stage the mapping names, so neither is held.
    create_deployed_stage(&gateway, &api, "beta").await;
    let (other_api, other_stage) = deployed_api(&gateway, "free-stage-api", "prod").await;

    let rows = [
        MappingRow {
            case: "a plain base path",
            base_path: "v1",
            with_stage: true,
        },
        MappingRow {
            case: "the (none) base path",
            base_path: NONE_BASE_PATH,
            with_stage: true,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let domain_name = format!("held-stage-{index}.example.com");
        created_domain(&gateway, &domain_name, json!({})).await;
        create_mapping(
            &gateway,
            &domain_name,
            json!({ "basePath": row.base_path, "restApiId": api, "stage": stage }),
        )
        .await;

        let refused = delete_stage(&gateway, &api, &stage).await;
        assert_bad_request(&refused, &stage_refusal(&stage, &[&domain_name]), row.case);

        // The stage survives the refusal.
        let read = get_stage(&gateway, &api, &stage).await;
        assert_eq!(
            read.status, 200,
            "case {}: the refused delete must leave the stage, got {} with {:?}",
            row.case, read.status, read.body
        );

        // Nothing else is held: the API's other stage, and the same-named stage of
        // another API, are deletable while this mapping stands — and are restored for the
        // next row.
        for (free_api, free_stage) in [(&api, "beta"), (&other_api, other_stage.as_str())] {
            let deleted = delete_stage(&gateway, free_api, free_stage).await;
            assert_eq!(
                deleted.status, 202,
                "case {}: a stage no mapping names must delete, got {} with {:?}",
                row.case, deleted.status, deleted.body
            );
            create_deployed_stage(&gateway, free_api, free_stage).await;
        }

        // Releasing the mapping releases the stage.
        delete_mapping(&gateway, &domain_name, row.base_path).await;
        let deleted = delete_stage(&gateway, &api, &stage).await;
        assert_eq!(
            deleted.status, 202,
            "case {}: expected 202 from DeleteStage once the mapping is gone, got {} with {:?}",
            row.case, deleted.status, deleted.body
        );
        let gone = get_stage(&gateway, &api, &stage).await;
        assert_eq!(
            gone.status, 404,
            "case {}: expected the deleted stage to be gone, got {} with {:?}",
            row.case, gone.status, gone.body
        );
        // Restored for the next row, which needs the same stage back.
        create_deployed_stage(&gateway, &api, &stage).await;
    }
}

#[tokio::test]
async fn a_rest_api_a_base_path_mapping_names_deletes_and_leaves_the_mapping_behind() {
    let gateway = Gateway::new();
    // The rows differ along what the rule generalises over: whether the mapping names a
    // stage of the API at all, and the spelling of its base path. A mapping names its API
    // either way, and either way it is no precondition of the API's delete.
    let rows = [
        MappingRow {
            case: "a mapping that names a stage",
            base_path: "v1",
            with_stage: true,
        },
        MappingRow {
            case: "a mapping that names no stage",
            base_path: "stageless",
            with_stage: false,
        },
        MappingRow {
            case: "a mapping at the (none) base path",
            base_path: NONE_BASE_PATH,
            with_stage: true,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let (api, stage) = deployed_api(&gateway, &format!("mapped-api-{index}"), "prod").await;
        let domain_name = format!("mapped-api-{index}.example.com");
        created_domain(&gateway, &domain_name, json!({})).await;
        let mut request = json!({ "basePath": row.base_path, "restApiId": api });
        if row.with_stage {
            request["stage"] = json!(stage);
        }
        create_mapping(&gateway, &domain_name, request).await;

        let deleted = delete_api(&gateway, &api).await;
        assert_eq!(
            deleted.status, 202,
            "case {}: a mapping naming the API is no precondition of its delete, got {} \
             with {:?}",
            row.case, deleted.status, deleted.body
        );
        let gone = gateway.call("GET", &format!("/restapis/{api}"), None).await;
        assert_eq!(
            gone.status, 404,
            "case {}: expected the deleted API to be gone, got {} with {:?}",
            row.case, gone.status, gone.body
        );

        // And the mapping is LEFT BEHIND, still naming the id the delete took away: it is
        // the domain name's record, not the API's, so nothing of it went with the API.
        let orphaned = get_mapping(&gateway, &domain_name, row.base_path).await;
        assert_eq!(
            orphaned.status, 200,
            "case {}: expected the mapping to outlive the API it named, got {} with {:?}",
            row.case, orphaned.status, orphaned.body
        );
        assert_eq!(
            orphaned.body["restApiId"].as_str(),
            Some(api.as_str()),
            "case {}: expected the orphaned mapping to still name {api}, got {:?}",
            row.case,
            orphaned.body
        );

        // Deleting the orphan is the caller's own call, and it goes through.
        let released = delete_mapping(&gateway, &domain_name, row.base_path).await;
        assert_eq!(
            released.status, 202,
            "case {}: expected 202 from DeleteBasePathMapping for the orphan, got {} with {:?}",
            row.case, released.status, released.body
        );
    }
}

#[tokio::test]
async fn a_mapping_that_names_no_stage_holds_no_stage() {
    let gateway = Gateway::new();
    let (api, stage) = deployed_api(&gateway, "stageless-mapping-api", "prod").await;
    let domain_name = "stageless-mapping.example.com";
    created_domain(&gateway, domain_name, json!({})).await;

    // "Specify '(none)' if you want callers to explicitly specify the stage name after any
    // base path name": a mapping that names no stage, and one that names `(none)`, name no
    // stage of the API at all, so neither holds one.
    for (case, request) in [
        (
            "a mapping created without a stage",
            json!({ "basePath": "no-stage", "restApiId": api }),
        ),
        (
            "a mapping whose stage is (none)",
            json!({ "basePath": "none-stage", "restApiId": api, "stage": NONE_BASE_PATH }),
        ),
    ] {
        let created = create_mapping(&gateway, domain_name, request).await;
        assert_eq!(
            created.status, 201,
            "case {case}: expected 201 from CreateBasePathMapping, got {} with {:?}",
            created.status, created.body
        );

        let deleted = delete_stage(&gateway, &api, &stage).await;
        assert_eq!(
            deleted.status, 202,
            "case {case}: a stage no mapping names must delete, got {} with {:?}",
            deleted.status, deleted.body
        );
        // Restored for the next row.
        create_deployed_stage(&gateway, &api, &stage).await;
    }
}

/// One row of the sentence table: how many custom domain names hold a mapping of the API,
/// how many mappings each of them holds, and the domain names the refusal must list.
struct DomainsRow {
    case: &'static str,
    /// The base paths to map, per domain name: one entry per domain name, each listing the
    /// base paths of that domain name's mappings of the API under test.
    mappings: &'static [&'static [&'static str]],
}

#[tokio::test]
async fn the_refusal_lists_the_domain_names_whose_mappings_hold_the_record() {
    // The rule: a delete of the STAGE a base path mapping names is refused with AWS's
    // sentence, which asks the caller to remove ALL base path mappings of the record from
    // the DOMAIN NAMES that hold them — so what the message carries is the list of those
    // domain names, in creation order and each named once, and never a base path.
    //
    // The rows differ along what the rule generalises over: how many domain names hold a
    // mapping of the record (one, two, three), how many mappings each of them holds (one,
    // several), and the spelling of those base paths (a plain one and the `(none)` the
    // member documents).
    //
    // Each plain base path is spelled long enough that it cannot occur BY CHANCE inside the
    // identifier the refusal names: an id is ten characters drawn from the lowercase
    // alphanumerics, so a two-character base path lands inside one about once in a hundred
    // runs and the assertion below — that the sentence names no base path — would read that
    // coincidence as a failure. The rule under test is unchanged: what differs between rows
    // is still the count of domain names, the count of mappings and the spelling.
    let rows = [
        DomainsRow {
            case: "one domain name holding one mapping",
            mappings: &[&["releaseone"]],
        },
        DomainsRow {
            case: "one domain name holding several mappings of the same record",
            mappings: &[&["releaseone", "releasetwo", NONE_BASE_PATH]],
        },
        DomainsRow {
            case: "two domain names",
            mappings: &[&["releaseone"], &[NONE_BASE_PATH]],
        },
        DomainsRow {
            case: "three domain names, one of them holding two mappings",
            mappings: &[&["alpha"], &["beta", "gamma"], &["delta"]],
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let gateway = Gateway::new();
        let (api, stage) = deployed_api(&gateway, &format!("listed-api-{index}"), "prod").await;
        // An API of its own behind a mapping of every one of these domain names: its
        // mappings must not put its domain names in the refusal of the API under test.
        let (other_api, other_stage) =
            deployed_api(&gateway, &format!("listed-other-{index}"), "prod").await;

        let mut domain_names: Vec<String> = Vec::new();
        for (held, base_paths) in row.mappings.iter().enumerate() {
            let domain_name = format!("listed-{index}-{held}.example.com");
            created_domain(&gateway, &domain_name, json!({})).await;
            for base_path in *base_paths {
                let created = create_mapping(
                    &gateway,
                    &domain_name,
                    json!({ "basePath": base_path, "restApiId": api, "stage": stage }),
                )
                .await;
                assert_eq!(
                    created.status, 201,
                    "case {}: expected 201 from CreateBasePathMapping, got {} with {:?}",
                    row.case, created.status, created.body
                );
            }
            let other = create_mapping(
                &gateway,
                &domain_name,
                json!({ "basePath": "other", "restApiId": other_api, "stage": other_stage }),
            )
            .await;
            assert_eq!(
                other.status, 201,
                "case {}: expected 201 from CreateBasePathMapping, got {} with {:?}",
                row.case, other.status, other.body
            );
            domain_names.push(domain_name);
        }
        let listed: Vec<&str> = domain_names.iter().map(String::as_str).collect();

        let refused_stage = delete_stage(&gateway, &api, &stage).await;
        assert_bad_request(&refused_stage, &stage_refusal(&stage, &listed), row.case);

        // The sentence names the domain names and nothing else: no base path of any
        // mapping rides on it, which is what tells the caller to remove ALL of them.
        //
        // Searched with the values the sentence is REQUIRED to carry masked out first — the
        // record's identifier and the domain names — because a minted `restApiId` is ten
        // characters of lowercase alphanumerics and can CONTAIN a short base path
        // (`v137i7gk2u` holds `v1`), which would fail this row by chance rather than for
        // the behaviour under test.
        {
            let stated = refused_stage.body["message"].as_str().unwrap_or_default();
            let mut searched = stated.replace(&api, "<restApiId>").replace(&stage, "<stage>");
            for domain_name in &listed {
                searched = searched.replace(domain_name, "<domainName>");
            }
            for base_paths in row.mappings {
                for base_path in *base_paths {
                    assert!(
                        !searched.contains(base_path),
                        "case {}: the refusal must name no base path, got {stated:?} naming \
                         {base_path:?}",
                        row.case
                    );
                }
            }
        }

        // The other API's own stage, whose mappings live on every one of those domain
        // names, is held by its own mappings and by nothing of the record above: the
        // refusal it earns lists the same domain names, under its own stage name.
        let refused_other = delete_stage(&gateway, &other_api, &other_stage).await;
        assert_bad_request(
            &refused_other,
            &stage_refusal(&other_stage, &listed),
            row.case,
        );

        // Releasing every mapping of the record releases it: a domain name whose last
        // mapping of it has gone drops out of the sentence, and the delete goes through
        // when the list is empty.
        let mut left = listed.clone();
        for base_paths in row.mappings {
            for base_path in *base_paths {
                delete_mapping(&gateway, left[0], base_path).await;
            }
            left.remove(0);
            if left.is_empty() {
                break;
            }
            let still = delete_stage(&gateway, &api, &stage).await;
            assert_bad_request(&still, &stage_refusal(&stage, &left), row.case);
        }
        let deleted = delete_stage(&gateway, &api, &stage).await;
        assert_eq!(
            deleted.status, 202,
            "case {}: expected 202 from DeleteStage once every mapping is gone, got {} \
             with {:?}",
            row.case, deleted.status, deleted.body
        );
    }
}
