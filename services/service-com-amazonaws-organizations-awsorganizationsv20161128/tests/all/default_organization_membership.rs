// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the organization membership the emulated account has BY DEFAULT.
//!
//! The rule: AWS Organizations is opt-in per account. An account belongs to an
//! organization only once one is created from it — "the account whose user is calling the
//! `CreateOrganization` operation automatically becomes the management account of the new
//! organization" — or it joins one, so a freshly mounted control plane holds NO
//! organization and every organization-scoped read answers the modelled
//! `AWSOrganizationsNotInUseException` until a request establishes membership. Mounting
//! this service is not such a request: membership is state a caller writes.
//!
//! Past that create, the organization is the CALLER's: it reports the calling account as
//! its management account, so a client that probes its topology (`DescribeOrganization`
//! 404 -> standalone, else compare the caller's account with `MasterAccountId`) reads the
//! topology the store actually holds, and a second create is the refusal an account that
//! is already in one gets.
//!
//! Asserted over sets rather than over one call: every read of the implemented slice,
//! each read taken twice (a read must not change what the next caller sees), over several
//! independent boots of the emulator, and the create driven over the model's whole
//! `OrganizationFeatureSet` catalogue plus the request that omits it.
//!
//! The members of the organization a create does establish are checked against their
//! modelled patterns (`smithy_query(query="shape", shape="Organization")`): `Id`
//! `^o-[a-z0-9]{10,32}$`, `Arn`
//! `^arn:aws:organizations::\d{12}:organization\/o-[a-z0-9]{10,32}$`, `MasterAccountId`
//! `^\d{12}$`, `MasterAccountArn`
//! `^arn:aws:organizations::\d{12}:account\/o-[a-z0-9]{10,32}\/\d{12}$` and
//! `MasterAccountEmail` `^[^\s@]+@[^\s@]+\.[^\s@]+$`.
//!
//! The wire harness is `organization_hierarchy_lifecycle`'s, so these assertions are
//! about the same deserialize → handle → serialize walk a client drives.

use serde_json::{json, Value};
use service_com_amazonaws_organizations_awsorganizationsv20161128::ops::types::ORGANIZATION_FEATURE_SET_VALUES;
use service_com_amazonaws_organizations_awsorganizationsv20161128::state::{
    DEFAULT_FEATURE_SET, NOT_IN_USE_MESSAGE, POLICY_TYPE_SERVICE_CONTROL_POLICY,
    POLICY_TYPE_STATUS_ENABLED, ROOT_NAME,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::organization_hierarchy_lifecycle::{
    account_id, call, matches_id, mounted_router, Answer,
};

/// How many independent mounts of the control plane each rule is asserted over, so a
/// value that happened to be right once cannot carry the assertion.
const BOOTS: usize = 3;

/// An organizational-unit id that is well formed for a root this store does not hold, so
/// the reads below are refused for the MEMBERSHIP they need and not for their input.
const WELL_FORMED_OU_ID: &str = "ou-abcd-11111111";

/// A root id that is well formed for an organization this store does not hold.
const WELL_FORMED_ROOT_ID: &str = "r-abcd";

/// Every organization-scoped request of the implemented slice, with a well-formed body:
/// each one needs the organization the calling account belongs to, so each one is refused
/// while it belongs to none.
fn organization_scoped_calls() -> Vec<(&'static str, Value)> {
    vec![
        ("DescribeOrganization", json!({})),
        ("ListRoots", json!({})),
        (
            "CreateOrganizationalUnit",
            json!({ "ParentId": WELL_FORMED_ROOT_ID, "Name": "Sandbox" }),
        ),
        (
            "DescribeOrganizationalUnit",
            json!({ "OrganizationalUnitId": WELL_FORMED_OU_ID }),
        ),
        (
            "ListOrganizationalUnitsForParent",
            json!({ "ParentId": WELL_FORMED_ROOT_ID }),
        ),
        (
            "DeleteOrganizationalUnit",
            json!({ "OrganizationalUnitId": WELL_FORMED_OU_ID }),
        ),
    ]
}

/// The `Organization` a read reports, asserted to be there at all.
async fn described_organization(router: &MultiServiceRouter, case: &str) -> Value {
    let answer = call(router, "DescribeOrganization", &json!({})).await;
    assert_eq!(
        answer.status, 200,
        "case {case}: expected 200, got {} with {:?}",
        answer.status, answer.body
    );
    answer.body["Organization"].clone()
}

/// Assert `answer` is the modelled refusal of a request made while the calling account
/// belongs to no organization: HTTP 404, `AWSOrganizationsNotInUseException`, and that
/// error's own fixed sentence.
fn assert_not_in_use(case: &str, answer: &Answer) {
    assert_eq!(
        answer.status, 404,
        "case {case}: expected 404, got {} with {:?}",
        answer.status, answer.body
    );
    assert!(
        answer.is("AWSOrganizationsNotInUseException"),
        "case {case}: expected AWSOrganizationsNotInUseException, got {:?} with {:?}",
        answer.error,
        answer.body
    );
    assert_eq!(
        answer.body["Message"].as_str(),
        Some(NOT_IN_USE_MESSAGE),
        "case {case}: expected the error's own sentence, got {:?}",
        answer.body
    );
}

/// Assert `organization` is the organization of THIS account, member by member against
/// the modelled patterns, with `feature_set` the mode it was created in.
fn assert_organization_of_this_account(case: &str, organization: &Value, feature_set: &str) {
    let id = organization["Id"].as_str().unwrap_or_default();
    assert!(
        matches_id(id, "o-", 10, 32),
        "case {case}: expected an Id matching ^o-[a-z0-9]{{10,32}}$, got {organization:?}"
    );
    let account = account_id();
    assert_eq!(
        organization["Arn"].as_str(),
        Some(format!("arn:aws:organizations::{account}:organization/{id}").as_str()),
        "case {case}: expected the organization's modelled ARN, got {organization:?}"
    );
    assert_eq!(
        organization["MasterAccountId"].as_str(),
        Some(account),
        "case {case}: the account the create was made from is the management account, \
         got {organization:?}"
    );
    assert_eq!(
        organization["MasterAccountArn"].as_str(),
        Some(format!("arn:aws:organizations::{account}:account/{id}/{account}").as_str()),
        "case {case}: expected the management account's modelled ARN, got {organization:?}"
    );
    let email = organization["MasterAccountEmail"].as_str().unwrap_or_default();
    assert!(
        email.split('@').count() == 2
            && !email.starts_with('@')
            && email.rsplit('@').next().is_some_and(|domain| {
                domain.contains('.') && !domain.starts_with('.') && !domain.ends_with('.')
            })
            && !email.contains(char::is_whitespace),
        "case {case}: expected an address matching Email's ^[^\\s@]+@[^\\s@]+\\.[^\\s@]+$, \
         got {email:?}"
    );
    assert_eq!(
        organization["FeatureSet"].as_str(),
        Some(feature_set),
        "case {case}: expected the feature set the create asked for, got {organization:?}"
    );
    assert!(
        ORGANIZATION_FEATURE_SET_VALUES
            .contains(&organization["FeatureSet"].as_str().unwrap_or_default()),
        "case {case}: the feature set must be a value the model's enum declares, \
         got {organization:?}"
    );
}

/// Every boot of the emulator presents an account that belongs to NO organization: each
/// organization-scoped call of the implemented slice is refused as "not in use", and a
/// second identical call is refused identically — nothing about a read establishes the
/// membership it needed.
#[tokio::test]
async fn every_booted_emulator_presents_an_account_in_no_organization() {
    for boot in 1..=BOOTS {
        let router = mounted_router();
        for (operation, body) in organization_scoped_calls() {
            for round in 1..=2 {
                let case = format!("{operation} on boot {boot}, call {round}");
                let answer = call(&router, operation, &body).await;
                assert_not_in_use(&case, &answer);
            }
        }
    }
}

/// The create is what establishes membership, and the account it is made from is the
/// organization's management account — whatever feature set it asks for, and whether or
/// not it names one. Past it the hierarchy the organization record names is really there,
/// and a second create is refused.
#[tokio::test]
async fn a_create_establishes_the_organization_this_account_manages() {
    // The model's own `OrganizationFeatureSet` catalogue, plus `None` for a request that
    // omits the optional member.
    let mut cases: Vec<Option<&str>> = vec![None];
    cases.extend(ORGANIZATION_FEATURE_SET_VALUES.iter().copied().map(Some));

    for requested in cases {
        let router = mounted_router();
        let case = format!("CreateOrganization(FeatureSet={requested:?})");
        // Before the create there is no organization to describe.
        assert_not_in_use(
            &format!("{case}, before the create"),
            &call(&router, "DescribeOrganization", &json!({})).await,
        );

        let created = call(
            &router,
            "CreateOrganization",
            &match requested {
                Some(value) => json!({ "FeatureSet": value }),
                None => json!({}),
            },
        )
        .await;
        assert_eq!(
            created.status, 200,
            "case {case}: expected 200, got {} with {:?}",
            created.status, created.body
        );
        // A create that names no feature set asks for the one Organizations defaults to.
        let feature_set = requested.unwrap_or(DEFAULT_FEATURE_SET);
        assert_organization_of_this_account(
            &case,
            &created.body["Organization"],
            feature_set,
        );

        // The read reports the record the create wrote, and reports it identically twice.
        let first = described_organization(&router, &format!("{case}, read 1")).await;
        assert_eq!(
            first, created.body["Organization"],
            "case {case}: the read must report what the create persisted, got {first:?}"
        );
        let second = described_organization(&router, &format!("{case}, read 2")).await;
        assert_eq!(
            first, second,
            "case {case}: two reads must answer identically, got {first:?} then {second:?}"
        );

        // The hierarchy the organization record names is really there: exactly one root,
        // carrying the organization's id in its ARN.
        let roots = call(&router, "ListRoots", &json!({})).await;
        assert_eq!(
            roots.status, 200,
            "case ListRoots after {case}: expected 200, got {} with {:?}",
            roots.status, roots.body
        );
        let listed = roots.body["Roots"].as_array().cloned().unwrap_or_default();
        assert_eq!(
            listed.len(),
            1,
            "case ListRoots after {case}: expected exactly one root, got {listed:?}"
        );
        let organization_id = first["Id"].as_str().unwrap_or_default();
        let root_id = listed[0]["Id"].as_str().unwrap_or_default();
        assert!(
            matches_id(root_id, "r-", 4, 32),
            "case ListRoots after {case}: expected an Id matching ^r-[0-9a-z]{{4,32}}$, \
             got {listed:?}"
        );
        assert_eq!(
            listed[0]["Arn"].as_str(),
            Some(
                format!(
                    "arn:aws:organizations::{}:root/{organization_id}/{root_id}",
                    account_id()
                )
                .as_str()
            ),
            "case ListRoots after {case}: the root's ARN must name this organization, \
             got {listed:?}"
        );
        assert_eq!(
            listed[0]["Name"].as_str(),
            Some(ROOT_NAME),
            "case ListRoots after {case}: expected the root's name, got {listed:?}"
        );
        assert_eq!(
            first["AvailablePolicyTypes"], listed[0]["PolicyTypes"],
            "case {case}: the organization's deprecated policy-type list must report what \
             the root reports, got {first:?} and {listed:?}"
        );

        // A second create is refused, and the refusal leaves the organization as it was.
        let again = call(&router, "CreateOrganization", &json!({})).await;
        assert_eq!(
            again.status, 409,
            "case {case}, second create: expected 409, got {} with {:?}",
            again.status, again.body
        );
        assert!(
            again.is("AlreadyInOrganizationException"),
            "case {case}, second create: expected AlreadyInOrganizationException, got {:?}",
            again.error
        );
        let after = described_organization(&router, &format!("{case}, after the refusal")).await;
        assert_eq!(
            first, after,
            "case {case}: the refusal must change nothing, got {first:?} then {after:?}"
        );
    }
}

/// An all-features organization enables service control policies in its root, and the
/// hierarchy under that root is usable: an OU hangs under it and is read back and listed
/// there, with no read of the slice refusing any more.
#[tokio::test]
async fn the_hierarchy_of_a_created_organization_is_usable() {
    let router = mounted_router();
    let created = call(
        &router,
        "CreateOrganization",
        &json!({ "FeatureSet": DEFAULT_FEATURE_SET }),
    )
    .await;
    assert_eq!(
        created.status, 200,
        "case CreateOrganization: expected 200, got {} with {:?}",
        created.status, created.body
    );
    let organization = described_organization(&router, "DescribeOrganization").await;
    let organization_id = organization["Id"].as_str().unwrap_or_default().to_owned();
    let roots = call(&router, "ListRoots", &json!({})).await;
    let root_id = roots.body["Roots"][0]["Id"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    assert_eq!(
        roots.body["Roots"][0]["PolicyTypes"],
        json!([{ "Type": POLICY_TYPE_SERVICE_CONTROL_POLICY, "Status": POLICY_TYPE_STATUS_ENABLED }]),
        "case ListRoots: an all-features organization enables service control policies in \
         its root, got {:?}",
        roots.body
    );

    let created = call(
        &router,
        "CreateOrganizationalUnit",
        &json!({ "ParentId": root_id, "Name": "Sandbox" }),
    )
    .await;
    assert_eq!(
        created.status, 200,
        "case CreateOrganizationalUnit under the created root: expected 200, got {} with {:?}",
        created.status, created.body
    );
    let ou = created.body["OrganizationalUnit"].clone();
    let ou_id = ou["Id"].as_str().unwrap_or_default().to_owned();
    let root_suffix = root_id.strip_prefix("r-").unwrap_or(&root_id);
    assert!(
        matches_id(&ou_id, &format!("ou-{root_suffix}-"), 8, 32),
        "case CreateOrganizationalUnit under the created root: expected an Id in this root, \
         got {ou:?}"
    );
    assert_eq!(
        ou["Path"].as_str(),
        Some(format!("{organization_id}/{root_id}/{ou_id}/").as_str()),
        "case CreateOrganizationalUnit under the created root: expected the path through \
         this organization, got {ou:?}"
    );

    // Each read of the slice answers from that state, and none of them refuses.
    let reads: Vec<(&str, Value)> = vec![
        ("DescribeOrganization", json!({})),
        ("ListRoots", json!({})),
        (
            "DescribeOrganizationalUnit",
            json!({ "OrganizationalUnitId": ou_id }),
        ),
        (
            "ListOrganizationalUnitsForParent",
            json!({ "ParentId": root_id }),
        ),
    ];
    for (operation, body) in reads {
        for round in 1..=2 {
            let answer: Answer = call(&router, operation, &body).await;
            assert_eq!(
                answer.status, 200,
                "case {operation}, read {round}: expected 200, got {} with {:?}",
                answer.status, answer.body
            );
            assert!(
                !answer.is("AWSOrganizationsNotInUseException"),
                "case {operation}, read {round}: this account is a member of an \
                 organization, got {:?}",
                answer.error
            );
        }
    }
    let listed = call(
        &router,
        "ListOrganizationalUnitsForParent",
        &json!({ "ParentId": root_id }),
    )
    .await;
    assert_eq!(
        listed.body["OrganizationalUnits"][0]["Id"].as_str(),
        Some(ou_id.as_str()),
        "case ListOrganizationalUnitsForParent(created root): expected the created OU, \
         got {:?}",
        listed.body
    );
}
