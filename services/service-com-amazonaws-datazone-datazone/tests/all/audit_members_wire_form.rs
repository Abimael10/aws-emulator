// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for the AUDIT members every DataZone response reports: the
//! instants (`createdAt`, `updatedAt`, `lastUpdatedAt`, `firstRevisionCreatedAt`) and the
//! users (`createdBy`, `updatedBy`, `firstRevisionCreatedBy`).
//!
//! Every case drives the service the way a client does — a real `restJson1` request
//! through the router, with only the response body as evidence — so a handler answering
//! from a hardcoded value instead of from the store would fail here. The rules are
//! asserted over a TABLE of responses rather than one recorded request: the rows span the
//! operation (a create, a read, an update, a list, a nested projection), the resource
//! (domain, asset type, form type, asset and its revisions, domain unit, glossary,
//! glossary term, catalog listing) and the member itself.
//!
//! The rules under test:
//!
//! 1. Every member this service's model targets at a plain `timestamp` — `CreatedAt` /
//!    `UpdatedAt`, neither of which carries a `@timestampFormat`, so `restJson1` puts it
//!    on the wire as epoch SECONDS — is reported as a whole number of seconds: a JSON
//!    integer, never a fractional epoch reading off the host clock. The instant it names
//!    is the one the resource was created or last changed at, so it falls inside the
//!    window the fixture ran in, and an `updatedAt` is never earlier than the `createdAt`
//!    beside it.
//! 2. Every "who did this" member names the DataZone USER the caller is: one
//!    `UserProfileId`-shaped identifier, the same on every member of every response, which
//!    `GetUserProfile` resolves — addressed either by that id or by the IAM ARN behind it,
//!    which the profile reports as its `iam` details. It is never an IAM principal ARN and
//!    never carries the caller's access key id, neither of which names a DataZone user.
//! 3. The owners a domain unit is created with name that same user, so the creator of a
//!    resource and the owner of what it created are one identity.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Value};

use crate::harness::Emulator;
// `UserProfileId`'s modelled pattern, checked independently of the service's own helper —
// one definition for this test target, owned by the suite about user profiles.
use crate::user_profiles::is_user_profile_id;

/// The members a response reports an audit INSTANT in. Each targets `CreatedAt` or
/// `UpdatedAt`, plain `timestamp` shapes with no `@timestampFormat`
/// (`smithy_query(query="operation", operation="CreateAsset,GetListing,CreateAssetType,
/// GetFormType,GetDomain,GetDomainUnit,GetGlossary,GetGlossaryTerm")`), so every one of
/// them travels as an epoch-seconds number.
///
/// Deliberately NOT the project and account-pool members of the same NAME: those target
/// `Timestamp` with `@timestampFormat("date-time")`, a different wire form, so no response
/// carrying one is a row here.
const TIMESTAMP_MEMBERS: &[&str] = &[
    "createdAt",
    "firstRevisionCreatedAt",
    "lastUpdatedAt",
    "updatedAt",
];

/// The members a response names the USER behind a change in: `CreatedBy` / `UpdatedBy`,
/// plain strings documented as "The Amazon DataZone user that created this asset in the
/// catalog" and the like (`smithy_query(query="doc", shape="CreateAssetOutput",
/// member="createdBy")`).
const PRINCIPAL_MEMBERS: &[&str] = &["createdBy", "firstRevisionCreatedBy", "updatedBy"];

/// The access key id [`crate::harness`] signs every request with. An audit member must not
/// be spelled out of it: an access key id is not a user any identity service knows.
const SIGNING_ACCESS_KEY_ID: &str = "AKIAEXAMPLETESTKEY";

/// Seconds since the Unix epoch, now — the resolution rule 1 says an audit instant is
/// reported at.
fn unix_seconds() -> i64 {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("the host clock is at or after the Unix epoch")
            .as_secs(),
    )
    .expect("the epoch second fits an i64")
}

/// One response the audit rules are asserted over.
struct Reported {
    /// The call that answered it, named in every assertion message.
    label: &'static str,
    body: Value,
}

/// Every audit member of `body`, as (path, value) pairs — the walk descends into nested
/// structures and list elements, so a listing's `item.assetListing.createdAt` and a
/// revision listing's `items[0].createdBy` are covered like a top-level member.
fn audit_members(body: &Value) -> Vec<(String, Value)> {
    fn walk(value: &Value, path: &str, out: &mut Vec<(String, Value)>) {
        match value {
            Value::Object(members) => {
                for (name, child) in members {
                    let child_path = format!("{path}.{name}");
                    if TIMESTAMP_MEMBERS.contains(&name.as_str())
                        || PRINCIPAL_MEMBERS.contains(&name.as_str())
                    {
                        out.push((child_path.clone(), child.clone()));
                    }
                    walk(child, &child_path, out);
                }
            }
            Value::Array(items) => {
                for (index, child) in items.iter().enumerate() {
                    walk(child, &format!("{path}[{index}]"), out);
                }
            }
            _ => {}
        }
    }
    let mut found = Vec::new();
    walk(body, "$", &mut found);
    found
}

/// The member name a path ends in.
fn member_name(path: &str) -> &str {
    path.rsplit('.').next().unwrap_or(path)
}

/// `member` of `body` as a string, or a panic naming what was missing: the fixture cannot
/// go on without the identifier a later call addresses.
fn id_of(label: &str, body: &Value, member: &str) -> String {
    body.get(member)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("{label} must report {member}, got {body:?}"))
        .to_string()
}

/// Every response the fixture gathered, and the window of epoch seconds it ran in: every
/// audit instant it reports was taken inside that window.
struct Fixture {
    reported: Vec<Reported>,
    domain: String,
    before: i64,
    after: i64,
}

/// Send one request, assert it answered `status`, and answer the body.
async fn answered(
    dz: &Emulator,
    label: &str,
    method: &str,
    uri: &str,
    body: Option<Value>,
    status: u16,
) -> Value {
    let answer = dz.call(method, uri, body).await;
    assert_eq!(
        answer.status, status,
        "{label}: expected HTTP {status}, got {} {:?}",
        answer.status, answer.body
    );
    answer.body
}

/// Drive one of every kind of path that reports an audit member — a create, a read, an
/// update, a list and a nested projection, across eight resources — and answer the
/// responses to assert the rules over.
async fn fixture(dz: &Emulator, domain_name: &str) -> Fixture {
    let before = unix_seconds();
    let domain = dz.available_domain(domain_name).await;
    let project = dz.project(&domain, "audit owner").await;
    let mut reported: Vec<Reported> = Vec::new();

    // The domain itself: `createdAt` and `lastUpdatedAt`.
    let read_domain = answered(
        dz,
        "GetDomain",
        "GET",
        &format!("/v2/domains/{domain}"),
        None,
        200,
    )
    .await;
    let root_unit = id_of("GetDomain", &read_domain, "rootDomainUnitId");
    reported.push(Reported {
        label: "GetDomain",
        body: read_domain,
    });

    // A custom asset type, which is what the asset below is an instance of: `createdAt`,
    // `updatedAt`, `createdBy`, `updatedBy`.
    let created_type = answered(
        dz,
        "CreateAssetType",
        "POST",
        &format!("/v2/domains/{domain}/asset-types"),
        Some(json!({
            "name": "AuditTable",
            "formsInput": {},
            "owningProjectIdentifier": project,
        })),
        201,
    )
    .await;
    reported.push(Reported {
        label: "CreateAssetType",
        body: created_type,
    });
    let read_type = answered(
        dz,
        "GetAssetType",
        "GET",
        &format!("/v2/domains/{domain}/asset-types/AuditTable"),
        None,
        200,
    )
    .await;
    reported.push(Reported {
        label: "GetAssetType",
        body: read_type,
    });

    // A metadata form type: `createdAt` and `createdBy` on the read.
    answered(
        dz,
        "CreateFormType",
        "POST",
        &format!("/v2/domains/{domain}/form-types"),
        Some(json!({
            "name": "AuditForm",
            "model": { "smithy": "structure AuditForm {\n  id: String\n}" },
            "owningProjectIdentifier": project,
        })),
        201,
    )
    .await;
    let read_form_type = answered(
        dz,
        "GetFormType",
        "GET",
        &format!("/v2/domains/{domain}/form-types/AuditForm"),
        None,
        200,
    )
    .await;
    reported.push(Reported {
        label: "GetFormType",
        body: read_form_type,
    });

    // The catalog asset the issue's recorded step creates: `createdAt`, `createdBy`,
    // `firstRevisionCreatedAt`, `firstRevisionCreatedBy`.
    let created_asset = answered(
        dz,
        "CreateAsset",
        "POST",
        &format!("/v2/domains/{domain}/assets"),
        Some(json!({
            "name": "audit asset",
            "typeIdentifier": "AuditTable",
            "owningProjectIdentifier": project,
            "formsInput": [{
                "formName": "AuditForm",
                "typeIdentifier": "AuditForm",
                "content": "{\"id\":\"1\"}",
            }],
        })),
        201,
    )
    .await;
    let asset_id = id_of("CreateAsset", &created_asset, "id");
    reported.push(Reported {
        label: "CreateAsset",
        body: created_asset,
    });
    let read_asset = answered(
        dz,
        "GetAsset",
        "GET",
        &format!("/v2/domains/{domain}/assets/{asset_id}"),
        None,
        200,
    )
    .await;
    reported.push(Reported {
        label: "GetAsset",
        body: read_asset,
    });
    // A second revision: the asset's own `createdAt` moves with the new revision while the
    // FIRST revision's instant and user stay what they were.
    let revision = answered(
        dz,
        "CreateAssetRevision",
        "POST",
        &format!("/v2/domains/{domain}/assets/{asset_id}/revisions"),
        Some(json!({ "name": "audit asset v2" })),
        200,
    )
    .await;
    reported.push(Reported {
        label: "CreateAssetRevision",
        body: revision,
    });
    // A LIST: every element carries the audit members of the revision it reports.
    let revisions = answered(
        dz,
        "ListAssetRevisions",
        "GET",
        &format!("/v2/domains/{domain}/assets/{asset_id}/revisions"),
        None,
        200,
    )
    .await;
    reported.push(Reported {
        label: "ListAssetRevisions",
        body: revisions,
    });

    // A domain unit: `createdAt`, `createdBy`, and the `owners` rule 3 is about.
    let created_unit = answered(
        dz,
        "CreateDomainUnit",
        "POST",
        &format!("/v2/domains/{domain}/domain-units"),
        Some(json!({ "name": "audit unit", "parentDomainUnitIdentifier": root_unit })),
        201,
    )
    .await;
    let unit_id = id_of("CreateDomainUnit", &created_unit, "id");
    reported.push(Reported {
        label: "CreateDomainUnit",
        body: created_unit,
    });
    let read_unit = answered(
        dz,
        "GetDomainUnit",
        "GET",
        &format!("/v2/domains/{domain}/domain-units/{unit_id}"),
        None,
        200,
    )
    .await;
    reported.push(Reported {
        label: "GetDomainUnit",
        body: read_unit,
    });

    // A glossary, then a PATCH of it: the read after the update is the row that carries
    // `updatedAt`/`updatedBy` beside the create's own `createdAt`/`createdBy`.
    let glossary = answered(
        dz,
        "CreateGlossary",
        "POST",
        &format!("/v2/domains/{domain}/glossaries"),
        Some(json!({ "name": "audit glossary", "owningProjectIdentifier": project })),
        201,
    )
    .await;
    let glossary_id = id_of("CreateGlossary", &glossary, "id");
    answered(
        dz,
        "UpdateGlossary",
        "PATCH",
        &format!("/v2/domains/{domain}/glossaries/{glossary_id}"),
        Some(json!({ "name": "audit glossary, renamed" })),
        200,
    )
    .await;
    let read_glossary = answered(
        dz,
        "GetGlossary",
        "GET",
        &format!("/v2/domains/{domain}/glossaries/{glossary_id}"),
        None,
        200,
    )
    .await;
    reported.push(Reported {
        label: "GetGlossary",
        body: read_glossary,
    });

    // A glossary term under it.
    let term = answered(
        dz,
        "CreateGlossaryTerm",
        "POST",
        &format!("/v2/domains/{domain}/glossary-terms"),
        Some(json!({ "glossaryIdentifier": glossary_id, "name": "audit term" })),
        201,
    )
    .await;
    let term_id = id_of("CreateGlossaryTerm", &term, "id");
    let read_term = answered(
        dz,
        "GetGlossaryTerm",
        "GET",
        &format!("/v2/domains/{domain}/glossary-terms/{term_id}"),
        None,
        200,
    )
    .await;
    reported.push(Reported {
        label: "GetGlossaryTerm",
        body: read_term,
    });

    // The catalog listing of that asset, the issue's other recorded step: `createdAt`,
    // `updatedAt`, `createdBy`, `updatedBy`, plus the nested asset listing's own
    // `createdAt`. Read once the publish the clock owes has landed, so `updatedAt` names a
    // later instant than the listing's own `createdAt`.
    let change_set = answered(
        dz,
        "CreateListingChangeSet",
        "POST",
        &format!("/v2/domains/{domain}/listings/change-set"),
        Some(json!({
            "entityIdentifier": asset_id,
            "entityType": "ASSET",
            "action": "PUBLISH",
        })),
        200,
    )
    .await;
    let listing_id = id_of("CreateListingChangeSet", &change_set, "listingId");
    let active = dz.await_listing_status(&domain, &listing_id, "ACTIVE").await;
    reported.push(Reported {
        label: "GetListing",
        body: active.body,
    });

    let after = unix_seconds();
    Fixture {
        reported,
        domain,
        before,
        after,
    }
}

/// The members named in `all`, sorted, as the coverage assertions compare against.
fn sorted(all: &[&'static str]) -> Vec<&'static str> {
    let mut names = all.to_vec();
    names.sort_unstable();
    names
}

#[tokio::test]
async fn every_audit_timestamp_is_reported_as_whole_epoch_seconds() {
    let dz = Emulator::new();
    let fixture = fixture(&dz, "audit-instants").await;

    // Which members were actually exercised, so the rule cannot pass vacuously.
    let mut seen: BTreeSet<&'static str> = BTreeSet::new();
    for response in &fixture.reported {
        for (path, value) in audit_members(&response.body) {
            let name = member_name(&path);
            let Some(member) = TIMESTAMP_MEMBERS.iter().find(|known| **known == name) else {
                continue;
            };
            seen.insert(member);
            // The JSON TYPE first and on its own: a fractional epoch reading is the exact
            // defect this pins, and a reader that accepted either form would not see it.
            let number = value.as_number().cloned().unwrap_or_else(|| {
                panic!(
                    "case {}{path}: an epoch-seconds timestamp is a JSON number, got {value:?}",
                    response.label
                )
            });
            assert!(
                number.is_i64() || number.is_u64(),
                "case {}{path}: expected a whole number of epoch seconds, got {number}",
                response.label
            );
            let seconds = number.as_i64().unwrap_or_default();
            assert!(
                (fixture.before..=fixture.after).contains(&seconds),
                "case {}{path}: expected an instant inside the fixture's window [{}, {}], \
                 got {seconds}",
                response.label,
                fixture.before,
                fixture.after
            );
        }
    }
    assert_eq!(
        seen.iter().copied().collect::<Vec<&str>>(),
        sorted(TIMESTAMP_MEMBERS),
        "every timestamp member has to be exercised by the fixture, saw {seen:?}"
    );

    // Within one response the two instants cannot disagree about order: what was updated
    // was updated no earlier than it was created.
    for response in &fixture.reported {
        let members: BTreeMap<&str, Option<i64>> = audit_members(&response.body)
            .iter()
            .map(|(path, value)| (member_name(path).to_string(), value.as_i64()))
            .filter(|(name, _)| TIMESTAMP_MEMBERS.contains(&name.as_str()))
            .map(|(name, seconds)| {
                (
                    *TIMESTAMP_MEMBERS
                        .iter()
                        .find(|known| **known == name)
                        .expect("the name came from the list"),
                    seconds,
                )
            })
            .collect();
        let created = members.get("createdAt").copied().flatten();
        for updated_member in ["updatedAt", "lastUpdatedAt"] {
            let updated = members.get(updated_member).copied().flatten();
            if let (Some(created), Some(updated)) = (created, updated) {
                assert!(
                    updated >= created,
                    "case {}: {updated_member} {updated} is earlier than createdAt {created}",
                    response.label
                );
            }
        }
    }
}

#[tokio::test]
async fn every_audit_principal_names_the_callers_datazone_user() {
    let dz = Emulator::new();
    let fixture = fixture(&dz, "audit-users").await;

    let mut seen: BTreeSet<&'static str> = BTreeSet::new();
    let mut users: BTreeSet<String> = BTreeSet::new();
    for response in &fixture.reported {
        for (path, value) in audit_members(&response.body) {
            let name = member_name(&path);
            let Some(member) = PRINCIPAL_MEMBERS.iter().find(|known| **known == name) else {
                continue;
            };
            seen.insert(member);
            let user = value.as_str().unwrap_or_else(|| {
                panic!(
                    "case {}{path}: a DataZone user identifier is a JSON string, got {value:?}",
                    response.label
                )
            });
            assert!(
                is_user_profile_id(user),
                "case {}{path}: expected a UserProfileId-shaped DataZone user, got {user:?}",
                response.label
            );
            assert!(
                !user.starts_with("arn:") && !user.contains(SIGNING_ACCESS_KEY_ID),
                "case {}{path}: an audit member names a DataZone user, not an IAM ARN or an \
                 access key id, got {user:?}",
                response.label
            );
            users.insert(user.to_string());
        }
    }
    assert_eq!(
        seen.iter().copied().collect::<Vec<&str>>(),
        sorted(PRINCIPAL_MEMBERS),
        "every principal member has to be exercised by the fixture, saw {seen:?}"
    );
    // One caller, one user: every member of every response names the same identity.
    assert_eq!(
        users.len(),
        1,
        "every audit member names the one calling user, got {users:?}"
    );
    let user = users
        .iter()
        .next()
        .expect("one user was collected")
        .to_string();

    // Rule 2's round trip: the id a response reports is one `GetUserProfile` resolves, and
    // the profile it answers with reports the IAM principal behind that user.
    // The tree's single definition of the IAM principal this emulator's own credential
    // speaks for, which is the identity behind the DataZone user every audit member names.
    let iam_arn = ::service_common::caller_principal::ambient_user_arn();
    let domain = &fixture.domain;
    for (addressed_by, identifier) in [
        ("its own id", user.clone()),
        ("its IAM ARN", iam_arn.clone()),
    ] {
        let encoded = identifier.replace('/', "%2F");
        let profile = answered(
            &dz,
            &format!("GetUserProfile by {addressed_by}"),
            "GET",
            &format!("/v2/domains/{domain}/user-profiles/{encoded}"),
            None,
            200,
        )
        .await;
        assert_eq!(
            profile.get("id").and_then(Value::as_str),
            Some(user.as_str()),
            "GetUserProfile by {addressed_by}: expected the profile of {user:?}, got {profile:?}"
        );
        assert_eq!(
            profile.get("type").and_then(Value::as_str),
            Some("IAM"),
            "GetUserProfile by {addressed_by}: the caller is an IAM principal, got {profile:?}"
        );
        assert_eq!(
            profile
                .get("details")
                .and_then(|details| details.get("iam"))
                .and_then(|iam| iam.get("arn"))
                .and_then(Value::as_str),
            Some(iam_arn.as_str()),
            "GetUserProfile by {addressed_by}: the profile reports the IAM principal behind \
             the user, got {profile:?}"
        );
    }

    // Rule 3: the owners a domain unit was created with name that same user.
    let owners: Vec<String> = fixture
        .reported
        .iter()
        .filter(|response| response.label == "CreateDomainUnit")
        .flat_map(|response| {
            response
                .body
                .get("owners")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
        })
        .map(|owner| {
            owner
                .get("user")
                .and_then(|user| user.get("userId"))
                .and_then(Value::as_str)
                .unwrap_or("<absent>")
                .to_string()
        })
        .collect();
    assert_eq!(
        owners,
        vec![user.clone()],
        "a created domain unit is owned by the calling user, got {owners:?}"
    );
}
