// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for `DeleteAssetType`.
//!
//! Every case drives the service the way a client does — a real `restJson1` request
//! through the router — and uses only the responses as evidence, so a handler that
//! answered from a hardcoded value instead of from the store fails here.
//!
//! The rules under test, each asserted over a TABLE of inputs rather than one request:
//!
//! 1. Deleting a stored asset type that nothing uses removes the record: the call
//!    answers 204 and the type stops being readable, so a second delete of the same
//!    identifier is a miss and the name is creatable again.
//! 2. "The asset type must not be in use (e.g., assigned to any asset). If used,
//!    deletion will fail." (`smithy_query(query="doc",shape="DeleteAssetType")`): an
//!    asset type a stored asset is OF is refused with `ConflictException` naming the
//!    asset type and the asset that prevented the delete, and the refusal stores
//!    nothing; once that asset is gone the delete goes through. The reference is matched
//!    in whole, the way a read addresses an asset type, so an asset whose type merely
//!    shares a segment or a prefix of the name does not block it.
//! 3. A delete is scoped to the domain in the request path and addresses an asset type
//!    by the identifier that reads it, so a name stored in another domain, or in no
//!    domain, is a `ResourceNotFoundException` naming the KIND and never the caller's
//!    identifier.

use crate::harness::Emulator;
use serde_json::{json, Map, Value};

/// `TypeName`'s modelled length, 1..256 (smithy_query shape="TypeName") — the name an
/// asset type is stored and addressed under.
const NAME_MAX: usize = 256;

/// The `ResourceNotFoundException` message the service answers for an asset type it
/// cannot find: a fixed sentence naming the resource KIND rather than the caller's
/// identifier (`crate::state::ResourceKind::AssetType`).
const ASSET_TYPE_NOT_FOUND: &str = "Asset type not found";

/// The `AccessDeniedException` message a request inside a domain the caller holds
/// nothing on is denied with: the operation name only.
const NOT_PERMITTED: &str = "User is not permitted to perform operation: DeleteAssetType";

/// One asset-domain object — an asset type, or an asset of one — as a deletion refusal
/// names it: `type <tag>#<domainId>#<identifier>`. SOURCE: the recorded
/// `ConflictException` wording, which names an asset-domain object
/// `type asset#<domainId>#<identifier>`.
fn asset_object(domain: &str, identifier: &str) -> String {
    format!("type asset#{domain}#{identifier}")
}

/// The `ConflictException` message a delete a stored dependency prevents is refused
/// with: recorded wording, built from the objects the call found, ending in a period and
/// a space.
fn deletion_prevented_message(object: &str, blocker: &str) -> String {
    format!("The following objects could not be deleted: {object} deletion prevented by {blocker}. ")
}

/// The calls these cases drive, on the shared [`Emulator`] harness.
impl Emulator {
    async fn create_asset_type_named(&self, domain: &str, name: &str, project: &str, forms: usize) {
        let forms: Map<String, Value> = (0..forms)
            .map(|index| {
                (
                    format!("Form{index}"),
                    json!({ "typeIdentifier": format!("FormType{index}"), "typeRevision": "1" }),
                )
            })
            .collect();
        let created = self
            .call(
                "POST",
                &format!("/v2/domains/{domain}/asset-types"),
                Some(json!({
                    "name": name,
                    "owningProjectIdentifier": project,
                    "formsInput": Value::Object(forms),
                })),
            )
            .await;
        assert_eq!(
            created.status, 201,
            "creating asset type {name:?}: expected 201, got {} {:?}",
            created.status, created.body
        );
    }

    async fn get_asset_type_named(&self, domain: &str, identifier: &str) -> crate::harness::Answer {
        self.call(
            "GET",
            &format!("/v2/domains/{domain}/asset-types/{identifier}"),
            None,
        )
        .await
    }

    async fn delete_asset_type_named(
        &self,
        domain: &str,
        identifier: &str,
    ) -> crate::harness::Answer {
        self.call(
            "DELETE",
            &format!("/v2/domains/{domain}/asset-types/{identifier}"),
            None,
        )
        .await
    }

    /// Create one asset of the type `type_identifier` and answer its minted id.
    async fn asset_of_type(
        &self,
        domain: &str,
        project: &str,
        name: &str,
        type_identifier: &str,
    ) -> String {
        let created = self
            .call(
                "POST",
                &format!("/v2/domains/{domain}/assets"),
                Some(json!({
                    "name": name,
                    "typeIdentifier": type_identifier,
                    "owningProjectIdentifier": project,
                    "formsInput": [],
                })),
            )
            .await;
        assert_eq!(
            created.status, 201,
            "creating an asset of type {type_identifier:?}: expected 201, got {} {:?}",
            created.status, created.body
        );
        created.s("id")
    }

    /// Record one further revision of an asset: the asset keeps the type it was created
    /// with, so a revised asset is still of that type.
    async fn revise_asset(&self, domain: &str, asset: &str, name: &str) {
        let revised = self
            .call(
                "POST",
                &format!("/v2/domains/{domain}/assets/{asset}/revisions"),
                Some(json!({ "name": name, "formsInput": [] })),
            )
            .await;
        assert_eq!(
            revised.status, 200,
            "revising asset {asset:?}: expected 200, got {} {:?}",
            revised.status, revised.body
        );
    }

    async fn delete_asset_by_id(&self, domain: &str, asset: &str) {
        let deleted = self
            .call("DELETE", &format!("/v2/domains/{domain}/assets/{asset}"), None)
            .await;
        assert_eq!(
            deleted.status, 204,
            "deleting asset {asset:?}: expected 204, got {} {:?}",
            deleted.status, deleted.body
        );
    }
}

/// One asset type to delete when nothing uses it. The rows differ along the form of the
/// name (plain, underscore-led, digits and mixed case, dotted, the modelled bounds) and
/// along how many metadata forms the type carries.
struct UnusedCase {
    label: &'static str,
    name: String,
    forms: usize,
}

fn unused_cases() -> Vec<UnusedCase> {
    vec![
        UnusedCase {
            label: "a plain name carrying no forms",
            name: "OrdersType".to_string(),
            forms: 0,
        },
        UnusedCase {
            label: "an underscore-led name carrying two forms",
            name: "_private_type1".to_string(),
            forms: 2,
        },
        UnusedCase {
            label: "digits and mixed case",
            name: "Three_Form_Type3".to_string(),
            forms: 3,
        },
        UnusedCase {
            // `TypeName` admits the dots a qualified name carries, so the whole dotted
            // string is the name the type is stored and addressed under.
            label: "a dotted, qualified name",
            name: "mydomain.sub.LeafType".to_string(),
            forms: 1,
        },
        UnusedCase {
            label: "a name at the modelled one-character minimum",
            name: "A".to_string(),
            forms: 0,
        },
        UnusedCase {
            label: "a name at the modelled 256-character maximum",
            name: "n".repeat(NAME_MAX),
            // `FormsInputMap` is len 0..10 (smithy_query shape="FormsInputMap").
            forms: 10,
        },
    ]
}

#[tokio::test]
async fn deleting_an_unused_asset_type_removes_the_record() {
    let dz = Emulator::new();
    let domain = dz.available_domain("asset-type-delete-domain").await;
    let project = dz.project(&domain, "delete owner").await;

    for case in unused_cases() {
        let label = case.label;
        dz.create_asset_type_named(&domain, &case.name, &project, case.forms)
            .await;

        // It is readable before the delete, so the 404 afterwards is evidence of the
        // delete rather than of a create that never landed.
        let before = dz.get_asset_type_named(&domain, &case.name).await;
        assert_eq!(
            (before.status, before.s("name").as_str()),
            (200, case.name.as_str()),
            "case {label:?}: expected the stored asset type, got {} {:?}",
            before.status,
            before.body
        );

        let deleted = dz.delete_asset_type_named(&domain, &case.name).await;
        assert_eq!(
            deleted.status, 204,
            "case {label:?}: expected 204 from DeleteAssetType, got {} {:?}",
            deleted.status, deleted.body
        );
        assert_eq!(
            deleted.body.as_object().map(Map::len),
            Some(0),
            "case {label:?}: DeleteAssetType models no output, so the answer carries no \
             members, got {:?}",
            deleted.body
        );

        let after = dz.get_asset_type_named(&domain, &case.name).await;
        assert_eq!(
            (after.status, after.error_code.as_str(), after.s("message")),
            (
                404,
                "ResourceNotFoundException",
                ASSET_TYPE_NOT_FOUND.to_string()
            ),
            "case {label:?}: a deleted asset type must not be readable, got {} {:?}",
            after.status,
            after.body
        );

        // A second delete of the same identifier is a miss, naming the KIND only.
        let again = dz.delete_asset_type_named(&domain, &case.name).await;
        assert_eq!(
            (again.status, again.error_code.as_str(), again.s("message")),
            (
                404,
                "ResourceNotFoundException",
                ASSET_TYPE_NOT_FOUND.to_string()
            ),
            "case {label:?}: expected a second delete to miss, got {} {:?}",
            again.status,
            again.body
        );

        // Nothing of the deleted record is left: the name is creatable again, at the
        // first revision.
        dz.create_asset_type_named(&domain, &case.name, &project, 0)
            .await;
        let remade = dz.get_asset_type_named(&domain, &case.name).await;
        assert_eq!(
            (remade.status, remade.s("revision").as_str()),
            (200, "1"),
            "case {label:?}: a deleted name must be creatable again at revision 1, got {} {:?}",
            remade.status,
            remade.body
        );
        assert_eq!(
            remade.body.get("formsOutput"),
            Some(&json!({})),
            "case {label:?}: the re-created type must carry only its own forms, got {:?}",
            remade.body.get("formsOutput")
        );
        let cleanup = dz.delete_asset_type_named(&domain, &case.name).await;
        assert_eq!(
            cleanup.status, 204,
            "case {label:?}: expected 204 deleting the re-created type, got {} {:?}",
            cleanup.status, cleanup.body
        );
    }
}

/// One asset type, and the assets stored beside it. The rows differ along how the type's
/// name is spelled, how many assets are stored and which of them is of the type, how
/// many revisions that asset has — and along whether any asset is of the type at all,
/// which is what decides between the refusal and the 204.
struct InUseCase {
    label: &'static str,
    /// The asset type that is created and then deleted.
    asset_type: &'static str,
    /// The `typeIdentifier` each stored asset is created with, in order.
    assets: Vec<&'static str>,
    /// How many revisions the FIRST stored asset ends up with: an asset keeps the type
    /// it was created with, so a revised asset is still of that type.
    revisions: usize,
    /// Whether some stored asset is of THIS asset type, which is what decides between
    /// the refusal and the 204.
    expect_blocked: bool,
}

fn in_use_cases() -> Vec<InUseCase> {
    vec![
        InUseCase {
            label: "one asset of the type",
            asset_type: "OrdersType",
            assets: vec!["OrdersType"],
            revisions: 1,
            expect_blocked: true,
        },
        InUseCase {
            label: "the third of three assets is of the type",
            asset_type: "ThirdUsedType",
            assets: vec![
                "amazon.datazone.RelationalTable",
                "SomeOtherType",
                "ThirdUsedType",
            ],
            revisions: 1,
            expect_blocked: true,
        },
        InUseCase {
            label: "a dotted type name an asset names in full",
            asset_type: "ns.sub.LeafType",
            assets: vec!["ns.sub.LeafType"],
            revisions: 1,
            expect_blocked: true,
        },
        InUseCase {
            label: "an asset of the type that carries three revisions",
            asset_type: "RevisedType",
            assets: vec!["RevisedType"],
            revisions: 3,
            expect_blocked: true,
        },
        InUseCase {
            label: "assets of other types only",
            asset_type: "UnusedType",
            assets: vec!["amazon.datazone.RelationalTable", "SomeOtherType"],
            revisions: 1,
            expect_blocked: false,
        },
        InUseCase {
            // An asset type is addressed by the WHOLE name it is stored under, so a
            // reference that only ends in that name is a reference to another type.
            label: "an asset whose type merely ends in the name",
            asset_type: "LeafType",
            assets: vec!["ns.sub.LeafType"],
            revisions: 1,
            expect_blocked: false,
        },
        InUseCase {
            label: "an asset whose type only shares a prefix of the name",
            asset_type: "Orders",
            assets: vec!["OrdersExtended"],
            revisions: 1,
            expect_blocked: false,
        },
        InUseCase {
            label: "no assets stored at all",
            asset_type: "LonelyType",
            assets: Vec::new(),
            revisions: 1,
            expect_blocked: false,
        },
    ]
}

#[tokio::test]
async fn an_asset_type_in_use_is_refused_and_an_unused_one_is_deleted() {
    let dz = Emulator::new();
    let domain = dz.available_domain("asset-type-in-use-domain").await;
    let project = dz.project(&domain, "in-use owner").await;

    for case in in_use_cases() {
        let label = case.label;
        dz.create_asset_type_named(&domain, case.asset_type, &project, 1)
            .await;
        let mut asset_ids = Vec::new();
        for (index, type_identifier) in case.assets.iter().enumerate() {
            let id = dz
                .asset_of_type(
                    &domain,
                    &project,
                    &format!("asset {index} of {type_identifier}"),
                    type_identifier,
                )
                .await;
            asset_ids.push(id);
        }
        if let Some(first) = asset_ids.first() {
            for revision in 2..=case.revisions {
                dz.revise_asset(&domain, first, &format!("revision {revision}"))
                    .await;
            }
        }

        // The type is readable before the delete, whatever the answer turns out to be.
        let before = dz.get_asset_type_named(&domain, case.asset_type).await;
        assert_eq!(
            before.status, 200,
            "case {label:?}: expected the stored asset type, got {} {:?}",
            before.status, before.body
        );

        if case.expect_blocked {
            // Exactly one stored asset is of the type in every blocked row, so the
            // object the refusal names is the one this row created.
            let blocking = asset_ids
                .iter()
                .zip(case.assets.iter())
                .find(|(_, type_identifier)| **type_identifier == case.asset_type)
                .map(|(id, _)| id.clone())
                .unwrap_or_else(|| panic!("case {label:?}: a blocked row stores an asset of it"));
            let refused = dz.delete_asset_type_named(&domain, case.asset_type).await;
            assert_eq!(
                (refused.status, refused.error_code.as_str()),
                (409, "ConflictException"),
                "case {label:?}: expected 409 ConflictException for a type in use, got {} {:?}",
                refused.status,
                refused.body
            );
            assert_eq!(
                refused.s("message"),
                deletion_prevented_message(
                    &asset_object(&domain, case.asset_type),
                    &asset_object(&domain, &blocking),
                ),
                "case {label:?}: the refusal must name the type and the asset that \
                 prevented the delete, got {:?}",
                refused.body
            );

            // A refused delete stores nothing: the type reads back exactly as before.
            let still_there = dz.get_asset_type_named(&domain, case.asset_type).await;
            assert_eq!(
                still_there.body, before.body,
                "case {label:?}: a refused delete must leave the stored type alone"
            );

            // Once the asset that was of it is gone, the delete goes through.
            dz.delete_asset_by_id(&domain, &blocking).await;
        }

        let deleted = dz.delete_asset_type_named(&domain, case.asset_type).await;
        assert_eq!(
            deleted.status, 204,
            "case {label:?}: expected 204 once nothing is of the type, got {} {:?}",
            deleted.status, deleted.body
        );
        let gone = dz.get_asset_type_named(&domain, case.asset_type).await;
        assert_eq!(
            (gone.status, gone.error_code.as_str()),
            (404, "ResourceNotFoundException"),
            "case {label:?}: the deleted type must not be readable, got {} {:?}",
            gone.status,
            gone.body
        );

        // Leave the domain as the row found it, so the next row stands on its own.
        for (id, type_identifier) in asset_ids.iter().zip(case.assets.iter()) {
            if *type_identifier != case.asset_type {
                dz.delete_asset_by_id(&domain, id).await;
            }
        }
    }
}

#[tokio::test]
async fn delete_asset_type_is_scoped_to_the_domain_that_stores_it() {
    let dz = Emulator::new();
    let domain = dz.available_domain("asset-type-scope-domain").await;
    let other_domain = dz.available_domain("asset-type-other-domain").await;
    let project = dz.project(&domain, "scope owner").await;
    let other_project = dz.project(&other_domain, "other owner").await;
    dz.create_asset_type_named(&domain, "StoredType", &project, 0)
        .await;
    dz.create_asset_type_named(&other_domain, "ElsewhereType", &other_project, 0)
        .await;

    // Each row names something this domain does not store; the stored type is the
    // control. An unknown DOMAIN is not an existence answer at all: the delete is scoped
    // to a domain the caller holds nothing on, so authorization decides it first.
    let cases: [(&str, &str, &str, u16, &str); 5] = [
        (
            "a well-formed domain id that was never created",
            // ^dzd[-_][a-zA-Z0-9_-]{1,36}$, so the refusal comes from the handler and
            // not from the router.
            "dzd_nosuchdomain",
            "StoredType",
            403,
            "AccessDeniedException",
        ),
        (
            "a name stored only in another domain",
            domain.as_str(),
            "ElsewhereType",
            404,
            "ResourceNotFoundException",
        ),
        (
            "a name no domain stores",
            domain.as_str(),
            "NoSuchAssetTypeHere",
            404,
            "ResourceNotFoundException",
        ),
        (
            "the stored name in another case",
            domain.as_str(),
            "storedtype",
            404,
            "ResourceNotFoundException",
        ),
        ("the stored name", domain.as_str(), "StoredType", 204, ""),
    ];
    for (label, target_domain, identifier, status, code) in cases {
        let answer = dz.delete_asset_type_named(target_domain, identifier).await;
        assert_eq!(
            (answer.status, answer.error_code.as_str()),
            (status, code),
            "case {label:?}: expected {status} {code:?}, got {} {:?}",
            answer.status,
            answer.body
        );
        match code {
            "AccessDeniedException" => assert_eq!(
                answer.s("message"),
                NOT_PERMITTED,
                "case {label:?}: the denial must name the operation only, got {:?}",
                answer.body
            ),
            "ResourceNotFoundException" => {
                assert_eq!(
                    answer.s("message"),
                    ASSET_TYPE_NOT_FOUND,
                    "case {label:?}: the miss must name the KIND only, got {:?}",
                    answer.body
                );
                assert!(
                    !answer.s("message").contains(identifier),
                    "case {label:?}: a not-found message must not echo the caller's \
                     identifier, got {:?}",
                    answer.s("message")
                );
            }
            _ => {}
        }
    }

    // The other domain kept its own type: a delete reaches only the domain in the path.
    let elsewhere = dz.get_asset_type_named(&other_domain, "ElsewhereType").await;
    assert_eq!(
        (elsewhere.status, elsewhere.s("domainId").as_str()),
        (200, other_domain.as_str()),
        "the other domain's type must be untouched, got {} {:?}",
        elsewhere.status,
        elsewhere.body
    );

    // An asset type is stored under its domain, so it goes when the domain goes — and a
    // delete addressed to a domain that is no longer there is denied, not reported as a
    // miss.
    let removed = dz
        .call("DELETE", &format!("/v2/domains/{other_domain}"), None)
        .await;
    assert_eq!(
        removed.status, 202,
        "DeleteDomain: expected 202, got {} {:?}",
        removed.status, removed.body
    );
    dz.await_domain_gone(&other_domain).await;
    let orphaned = dz
        .delete_asset_type_named(&other_domain, "ElsewhereType")
        .await;
    assert_eq!(
        (orphaned.status, orphaned.error_code.as_str()),
        (403, "AccessDeniedException"),
        "an asset type must not outlive its domain, got {} {:?}",
        orphaned.status,
        orphaned.body
    );
}
