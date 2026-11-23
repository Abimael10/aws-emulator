// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for what `DeleteDomainName` takes WITH the custom domain name record
//! it removes: the `BasePathMapping` records that record holds, read back through
//! `GetDomainName`, `GetBasePathMapping` and `GetBasePathMappings`.
//!
//! Every case drives the real wire path — a `restJson1` HTTP request through the
//! multi-service router into the mounted service — so what is asserted is what a client
//! sees, not what a handler returns.
//!
//! The RULE under test: a base path mapping is a SUBORDINATE record of the custom domain
//! name it was created under, so it never holds that domain name back — `DeleteDomainName`
//! is accepted while mappings stand and removes them with the record, after which both the
//! domain name's own reader and the mapping's reader resolve nothing and answer the domain
//! name's missing-resource `NotFoundException`, and nothing of the removed record comes
//! back when a domain name of the same name is created again. The INDEPENDENT record a
//! mapping merely NAMES whose own delete that reference refuses — the STAGE, which
//! `DeleteStage` refuses over while a mapping names it (the sibling
//! `base_path_mapping_references` suite) — outlives the reference instead, so the cascade is
//! what releases it.
//!
//! The rows span what the rule generalises over rather than repeating one recorded call:
//! how many mappings the record holds (none, one, several), how the base path is spelled
//! (a plain one, one of mixed characters, the `(none)` the member documents, and a request
//! that carries no `basePath` at all), whether the mapping names a stage, and how the
//! record is addressed (a regional record by its name, a private one by its name and
//! `domainNameId` with a same-named sibling standing beside it).
//!
//! [`Gateway`], [`Answer`], [`assert_failure`] and [`string_member`] are this crate's one
//! wire-test harness, reused rather than standing a second emulator up another way;
//! [`DomainRef`], `create_mapping`, `get_mapping`, `created_private_domain`,
//! `created_regional_domain` and `deployed_api` are the `base_path_mapping_private_domain`
//! suite's one spelling of those calls.

use serde_json::{json, Value};
use service_com_amazonaws_apigateway_backplanecontrolservice::state::{
    NONE_BASE_PATH, NO_SUCH_BASE_PATH_MAPPING, NO_SUCH_DOMAIN_NAME,
};

use crate::base_path_mapping_private_domain::{
    create_mapping, created_private_domain, created_regional_domain, deployed_api, get_mapping,
    DomainRef,
};
use crate::rest_api_lifecycle::{assert_failure, page_items, string_member, Answer, Gateway};

const NOT_FOUND: (u16, &str) = (404, "NotFoundException");

/// The status `DeleteDomainName`'s model declares for a success
/// (`smithy_query(query="operation", operation="DeleteDomainName")`:
/// `DELETE /domainnames/{domainName} -> 202`).
const DELETED: u16 = 202;

/// `DeleteDomainName` for the record `domain` addresses.
async fn delete_domain(gateway: &Gateway, domain: &DomainRef) -> Answer {
    let uri = format!("/domainnames/{}{}", domain.name, domain.query());
    gateway.call("DELETE", &uri, None).await
}

/// `GetDomainName` for the record `domain` addresses.
async fn get_domain(gateway: &Gateway, domain: &DomainRef) -> Answer {
    let uri = format!("/domainnames/{}{}", domain.name, domain.query());
    gateway.call("GET", &uri, None).await
}

/// `GetBasePathMappings` for the record `domain` addresses.
async fn list_mappings(gateway: &Gateway, domain: &DomainRef) -> Answer {
    let uri = format!(
        "/domainnames/{}/basepathmappings{}",
        domain.name,
        domain.query()
    );
    gateway.call("GET", &uri, None).await
}

/// `DeleteRestApi` for `api`.
async fn delete_api(gateway: &Gateway, api: &str) -> Answer {
    gateway.call("DELETE", &format!("/restapis/{api}"), None).await
}

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

/// Assert `answer` is the modelled failure `(status, code)` carrying exactly `message`.
fn assert_refusal(answer: &Answer, expected: (u16, &str), message: &str, case: &str) {
    assert_failure(answer, expected.0, expected.1, case);
    assert_eq!(
        answer.body["message"].as_str(),
        Some(message),
        "case {case}: expected the message {message:?}, got {:?}",
        answer.body["message"]
    );
}

/// Assert a delete was accepted with the status its model declares and an empty body.
fn assert_deleted(answer: &Answer, case: &str) {
    assert_eq!(
        answer.status, DELETED,
        "case {case}: expected {DELETED}, got {} with {:?}",
        answer.status, answer.body
    );
    assert!(
        answer.body.is_null(),
        "case {case}: expected an empty body, got {:?}",
        answer.body
    );
}

/// One mapping of a row: the `basePath` the request carries (`None` for a request that
/// carries none), whether it names the stage, and the base path the stored mapping is
/// reported and addressed by — `(none)` for a request with no base path, which is the
/// spelling the `basePath` member documents.
struct MappingSpec {
    requested: Option<&'static str>,
    with_stage: bool,
    stored: &'static str,
}

/// One row of the cascade table: the case, the mappings the record holds, and whether the
/// record is a PRIVATE one (addressed by `domainName` + `domainNameId`).
struct CascadeRow {
    case: &'static str,
    mappings: &'static [MappingSpec],
    private: bool,
}

/// Create `spec` under the record `domain` addresses, assert it was accepted, and assert
/// the stored mapping reports the base path it is addressed by.
async fn create_and_read_back(
    gateway: &Gateway,
    domain: &DomainRef,
    spec: &MappingSpec,
    api: &str,
    stage: &str,
    case: &str,
) {
    let mut request = json!({ "restApiId": api });
    if let Some(requested) = spec.requested {
        request["basePath"] = json!(requested);
    }
    if spec.with_stage {
        request["stage"] = json!(stage);
    }
    let created = create_mapping(gateway, domain, request).await;
    assert_eq!(
        created.status, 201,
        "case {case}: expected 201 from CreateBasePathMapping, got {} with {:?}",
        created.status, created.body
    );
    // The mapping is really there before the delete: the cascade below is a removal of a
    // stored record, not of nothing.
    let read = get_mapping(gateway, domain, spec.stored).await;
    assert_eq!(
        read.status, 200,
        "case {case}: expected the created mapping to read, got {} with {:?}",
        read.status, read.body
    );
    assert_eq!(
        string_member(&read.body, "basePath", case),
        spec.stored,
        "case {case}: expected the mapping to be stored under {:?}, got {:?}",
        spec.stored,
        read.body
    );
}

#[tokio::test]
async fn deleting_a_domain_name_takes_every_base_path_mapping_it_holds_with_it() {
    let gateway = Gateway::new();
    let (api, stage) = deployed_api(&gateway, "cascade-api", "prod").await;

    let rows = [
        CascadeRow {
            case: "a record holding no mapping at all",
            mappings: &[],
            private: false,
        },
        CascadeRow {
            case: "one mapping at a plain base path",
            mappings: &[MappingSpec {
                requested: Some("v1"),
                with_stage: true,
                stored: "v1",
            }],
            private: false,
        },
        CascadeRow {
            case: "one mapping created with no basePath member",
            mappings: &[MappingSpec {
                requested: None,
                with_stage: true,
                stored: NONE_BASE_PATH,
            }],
            private: false,
        },
        CascadeRow {
            case: "one mapping at the documented (none) base path",
            mappings: &[MappingSpec {
                requested: Some(NONE_BASE_PATH),
                with_stage: true,
                stored: NONE_BASE_PATH,
            }],
            private: false,
        },
        CascadeRow {
            case: "one mapping that names no stage",
            mappings: &[MappingSpec {
                requested: Some("stageless"),
                with_stage: false,
                stored: "stageless",
            }],
            private: false,
        },
        CascadeRow {
            case: "several mappings, spelled differently",
            mappings: &[
                MappingSpec {
                    requested: Some("alpha"),
                    with_stage: true,
                    stored: "alpha",
                },
                MappingSpec {
                    requested: Some("Orders-2_beta"),
                    with_stage: false,
                    stored: "Orders-2_beta",
                },
                MappingSpec {
                    requested: None,
                    with_stage: true,
                    stored: NONE_BASE_PATH,
                },
            ],
            private: false,
        },
        CascadeRow {
            case: "a private record addressed by its domainNameId",
            mappings: &[
                MappingSpec {
                    requested: Some("v1"),
                    with_stage: true,
                    stored: "v1",
                },
                MappingSpec {
                    requested: None,
                    with_stage: true,
                    stored: NONE_BASE_PATH,
                },
            ],
            private: true,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let name = format!("cascade-{index}.example.com");
        let domain = if row.private {
            created_private_domain(&gateway, &name, row.case).await
        } else {
            created_regional_domain(&gateway, &name).await
        };
        // A same-named PRIVATE sibling holding its own mapping at the same base paths: the
        // delete addresses ONE record, so the sibling's mappings are not part of the
        // cascade.
        let sibling = if row.private {
            let sibling = created_private_domain(&gateway, &name, row.case).await;
            create_and_read_back(
                &gateway,
                &sibling,
                &MappingSpec {
                    requested: Some("v1"),
                    with_stage: true,
                    stored: "v1",
                },
                &api,
                &stage,
                &format!("{} (the same-named sibling)", row.case),
            )
            .await;
            Some(sibling)
        } else {
            None
        };

        for spec in row.mappings {
            create_and_read_back(&gateway, &domain, spec, &api, &stage, row.case).await;
        }
        let listed = list_mappings(&gateway, &domain).await;
        assert_eq!(
            page_items(&listed.body, row.case).len(),
            row.mappings.len(),
            "case {}: expected GetBasePathMappings to report the created mappings, got {:?}",
            row.case,
            listed.body
        );

        // The delete is ACCEPTED while the mappings stand: nothing about them conditions it.
        assert_deleted(&delete_domain(&gateway, &domain).await, row.case);

        // The record is gone, so its reader answers the missing-resource refusal.
        assert_refusal(
            &get_domain(&gateway, &domain).await,
            NOT_FOUND,
            NO_SUCH_DOMAIN_NAME,
            &format!("GetDomainName after {}", row.case),
        );
        // And every mapping went with it: a mapping is reached THROUGH its domain name, so
        // the read of one resolves no domain name and answers that record's own refusal
        // rather than reporting a dangling mapping.
        for spec in row.mappings {
            assert_refusal(
                &get_mapping(&gateway, &domain, spec.stored).await,
                NOT_FOUND,
                NO_SUCH_DOMAIN_NAME,
                &format!("GetBasePathMapping({}) after {}", spec.stored, row.case),
            );
        }
        assert_refusal(
            &list_mappings(&gateway, &domain).await,
            NOT_FOUND,
            NO_SUCH_DOMAIN_NAME,
            &format!("GetBasePathMappings after {}", row.case),
        );

        // The same-named sibling is untouched, with the mapping it holds.
        if let Some(sibling) = &sibling {
            let read = get_domain(&gateway, sibling).await;
            assert_eq!(
                read.status, 200,
                "case {}: expected the same-named sibling to still read, got {} with {:?}",
                row.case, read.status, read.body
            );
            let kept = get_mapping(&gateway, sibling, "v1").await;
            assert_eq!(
                kept.status, 200,
                "case {}: expected the sibling's mapping to survive, got {} with {:?}",
                row.case, kept.status, kept.body
            );
        }
    }
}

#[tokio::test]
async fn a_domain_name_created_again_holds_none_of_the_removed_mappings() {
    let gateway = Gateway::new();
    let (api, stage) = deployed_api(&gateway, "recreate-api", "prod").await;
    // Both addressing forms: a regional record created again under the same name, and a
    // private one created again (which mints its own `domainNameId`).
    for (case, private) in [
        ("a regional record created again", false),
        ("a private record created again", true),
    ] {
        let name = "recreated.example.com";
        let first = if private {
            created_private_domain(&gateway, name, case).await
        } else {
            created_regional_domain(&gateway, name).await
        };
        let base_paths = ["v1", NONE_BASE_PATH];
        for stored in base_paths {
            create_and_read_back(
                &gateway,
                &first,
                &MappingSpec {
                    requested: Some(stored),
                    with_stage: true,
                    stored,
                },
                &api,
                &stage,
                case,
            )
            .await;
        }
        assert_deleted(&delete_domain(&gateway, &first).await, case);

        let again = if private {
            created_private_domain(&gateway, name, case).await
        } else {
            created_regional_domain(&gateway, name).await
        };
        // The new record carries no mapping: the removed ones are gone, not parked under
        // the name waiting to be adopted. The refusal is the MAPPING's own missing-resource
        // sentence now, because the domain name resolves again.
        for stored in base_paths {
            assert_refusal(
                &get_mapping(&gateway, &again, stored).await,
                NOT_FOUND,
                NO_SUCH_BASE_PATH_MAPPING,
                &format!("GetBasePathMapping({stored}) after {case}"),
            );
        }
        let listed = list_mappings(&gateway, &again).await;
        let items: Vec<Value> = page_items(&listed.body, case);
        assert!(
            items.is_empty(),
            "case {case}: expected the new record to hold no mapping, got {items:?}"
        );
        // Left clean for the next row, which creates a record of the same name.
        assert_deleted(&delete_domain(&gateway, &again).await, case);
    }
}

#[tokio::test]
async fn the_cascade_releases_the_rest_api_and_stage_the_removed_mappings_named() {
    let gateway = Gateway::new();
    let (api, stage) = deployed_api(&gateway, "released-api", "prod").await;
    // Two custom domain names, each holding a mapping of the one API and stage: the API is
    // held while EITHER mapping stands, so deleting one domain name releases only that
    // one's hold.
    let held = created_regional_domain(&gateway, "released-held.example.com").await;
    let other = created_private_domain(&gateway, "released-other.example.com", "the second holder")
        .await;
    for (domain, base_path) in [(&held, "v1"), (&other, NONE_BASE_PATH)] {
        create_and_read_back(
            &gateway,
            domain,
            &MappingSpec {
                requested: Some(base_path),
                with_stage: true,
                stored: base_path,
            },
            &api,
            &stage,
            "two holders",
        )
        .await;
    }

    // While the first domain name's mapping stands the STAGE it names is refused: the
    // reference of an INDEPENDENT record is what refuses a delete, and the cascade below is
    // what takes that reference away. The API ITSELF carries no such precondition — a
    // mapping that names it is left behind by its delete, which the sibling
    // `base_path_mapping_references` suite pins — so the API is deleted here only once
    // every mapping has gone, after the stage.
    assert_failure(
        &delete_stage(&gateway, &api, &stage).await,
        400,
        "BadRequestException",
        "DeleteStage while two mappings name it",
    );

    assert_deleted(&delete_domain(&gateway, &held).await, "the first holder");
    // One holder left, so the stage is still held.
    assert_failure(
        &delete_stage(&gateway, &api, &stage).await,
        400,
        "BadRequestException",
        "DeleteStage while one mapping names it",
    );

    assert_deleted(&delete_domain(&gateway, &other).await, "the second holder");
    // Every mapping went with its domain name, so nothing names the stage or the API any
    // more and both delete.
    assert_deleted(
        &delete_stage(&gateway, &api, &stage).await,
        "DeleteStage once every mapping has gone with its domain name",
    );
    assert_deleted(
        &delete_api(&gateway, &api).await,
        "DeleteRestApi once every mapping has gone with its domain name",
    );
}
