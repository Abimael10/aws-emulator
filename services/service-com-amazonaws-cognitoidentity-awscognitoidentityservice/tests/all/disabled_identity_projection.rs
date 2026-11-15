// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the `Logins` list a read projects for an identity nothing reaches
//! any more — the sentinel that stands in place of the providers a call took off it.
//!
//! RULE: an identity a MERGE merged away projects its `Logins` as exactly two elements, the
//! fixed disabled marker followed by the REGION of the pool that holds the identity (read
//! off the pool's own id, `<region>:<guid>`, and so off state rather than off the request);
//! an identity an UNLINK left linked to no provider projects the marker alone; and an
//! identity that never carried a login projects an empty list. Every read of the identity
//! reports the same projection — `DescribeIdentity` and the descriptions `ListIdentities`
//! pages — and it is permanent: a second read, and a read after another merge in the same
//! pool, report it unchanged.
//!
//! Each case asserts that rule over a TABLE rather than one recorded call: the rows differ
//! in the REGION the pool lives in (which is what the second element generalises over), in
//! how many developer user identifiers the merged-away user carried, in whether it was also
//! reached through federated logins, and in how many users are merged into one.
//!
//! The harness is `identity_pool_flow`'s: one shared engine behind the same
//! `MultiServiceRouter` the production router builds, with every call signed for the row's
//! own region — the SigV4 credential scope's Region, which is the region a pool's id is
//! minted in.

use serde_json::{json, Value};

use crate::identity_pool_flow::Emulator;
// The developer-user registration every merge suite of this target drives, signed for the
// row's own region.
use crate::merge_developer_identities::user_in;

/// The marker the `Logins` list of a DISABLED identity opens with. Spelled out here as the
/// literal, so the wire-visible projection is pinned by the test rather than by the code
/// under test.
const DISABLED: &str = "DISABLED";

/// A `Logins` entry: the provider name and the value presented under it.
type Login<'a> = (&'a str, &'a str);

/// One row of the merge table: the region the pool is created in, the two developer users of
/// that pool as the identifiers each is registered under, and the federated logins the
/// merged-away one is also reached through.
struct Row<'a> {
    case: &'a str,
    region: &'a str,
    survivor_identifiers: Vec<&'a str>,
    merged_away_identifiers: Vec<&'a str>,
    merged_away_federated: Vec<Login<'a>>,
}

/// The developer provider every row's pool is configured with.
const DEVELOPER_PROVIDER: &str = "login.example.app";

/// The whole `Logins` list a read owes for an identity a MERGE merged away: the marker and
/// the region of the pool holding it, which that pool's own id names (`<region>:<guid>`).
/// `pub(crate)` so the sibling suite that reads a merged-away identity back asserts the same
/// projection rather than keeping a second copy of this rule.
pub(crate) fn merged_away_projection(pool_id: &str) -> Vec<String> {
    let (region, _) = pool_id
        .split_once(':')
        .unwrap_or_else(|| panic!("an identity pool id is <region>:<guid>, got {pool_id:?}"));
    vec![DISABLED.to_string(), region.to_string()]
}

/// A pool of `region` configured with [`DEVELOPER_PROVIDER`], and its minted id.
async fn pool_in(emulator: &Emulator, region: &str, name: &str) -> String {
    let created = emulator
        .ok_in(
            region,
            "CreateIdentityPool",
            &json!({
                "IdentityPoolName": name,
                "AllowUnauthenticatedIdentities": true,
                "DeveloperProviderName": DEVELOPER_PROVIDER,
                "SupportedLoginProviders": { "graph.facebook.com": "declared-app-id" },
            }),
        )
        .await;
    created["IdentityPoolId"]
        .as_str()
        .expect("CreateIdentityPool returns an IdentityPoolId")
        .to_string()
}

/// The `Logins` list `DescribeIdentity` reports for one identity.
async fn described_logins(emulator: &Emulator, region: &str, identity_id: &str) -> Vec<String> {
    let described = emulator
        .ok_in(region, "DescribeIdentity", &json!({ "IdentityId": identity_id }))
        .await;
    logins_of(identity_id, &described)
}

/// The `Logins` list a LISTING of `pool_id` reports for one identity, or `None` when the
/// listing does not report it at all.
async fn listed_logins(
    emulator: &Emulator,
    region: &str,
    pool_id: &str,
    identity_id: &str,
) -> Option<Vec<String>> {
    let page = emulator
        .ok_in(
            region,
            "ListIdentities",
            &json!({ "IdentityPoolId": pool_id, "MaxResults": 60, "HideDisabled": false }),
        )
        .await;
    page["Identities"]
        .as_array()
        .unwrap_or_else(|| panic!("ListIdentities answers an Identities list: {page}"))
        .iter()
        .find(|described| described["IdentityId"] == json!(identity_id))
        .map(|described| logins_of(identity_id, described))
}

/// The `Logins` list one identity description carries.
fn logins_of(identity_id: &str, described: &Value) -> Vec<String> {
    described["Logins"]
        .as_array()
        .unwrap_or_else(|| panic!("the description of {identity_id} carries Logins: {described}"))
        .iter()
        .map(|login| {
            login
                .as_str()
                .unwrap_or_else(|| panic!("a login is a string: {described}"))
                .to_string()
        })
        .collect()
}

/// RULE: a merge leaves the identity it merged away projecting the disabled marker followed
/// by the region of its pool, through every read of it, whatever region the pool is in and
/// whatever logins the merge moved.
#[tokio::test]
async fn a_merge_leaves_the_merged_away_identity_projecting_the_marker_and_its_pools_region() {
    let emulator = Emulator::new();

    // The rows differ along what the rule generalises over: the REGION the pool lives in,
    // how many developer user identifiers each user carries, and whether the merged-away
    // user is also reached through federated logins.
    let rows = [
        Row {
            case: "one identifier each, in us-west-2",
            region: "us-west-2",
            survivor_identifiers: vec!["survivor-user"],
            merged_away_identifiers: vec!["merged-away-user"],
            merged_away_federated: Vec::new(),
        },
        Row {
            case: "several identifiers, in eu-central-1",
            region: "eu-central-1",
            survivor_identifiers: vec!["kept-1", "kept-2"],
            merged_away_identifiers: vec!["gone-1", "gone-2", "gone-3"],
            merged_away_federated: Vec::new(),
        },
        Row {
            case: "a merged-away user also reached through a federated login, in ap-southeast-1",
            region: "ap-southeast-1",
            survivor_identifiers: vec!["survivor-user"],
            merged_away_identifiers: vec!["merged-away-user"],
            merged_away_federated: vec![("graph.facebook.com", "a-facebook-access-token")],
        },
        Row {
            case: "a region spelled with digits and dashes alone, in us-gov-west-1",
            region: "us-gov-west-1",
            survivor_identifiers: vec!["survivor-user"],
            merged_away_identifiers: vec!["merged-away-user"],
            merged_away_federated: Vec::new(),
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let pool_id = pool_in(&emulator, row.region, &format!("merge-projection-{index}")).await;
        assert!(
            pool_id.starts_with(&format!("{}:", row.region)),
            "case {case:?}: expected a pool of {}, got {pool_id}",
            row.region
        );
        let survivor = user_in(
            &emulator,
            row.region,
            &pool_id,
            DEVELOPER_PROVIDER,
            &row.survivor_identifiers,
            &[],
        )
        .await;
        let merged_away = user_in(
            &emulator,
            row.region,
            &pool_id,
            DEVELOPER_PROVIDER,
            &row.merged_away_identifiers,
            &row.merged_away_federated,
        )
        .await;

        emulator
            .ok_in(
                row.region,
                "MergeDeveloperIdentities",
                &json!({
                    "IdentityPoolId": &pool_id,
                    "DeveloperProviderName": DEVELOPER_PROVIDER,
                    "SourceUserIdentifier": row.survivor_identifiers[0],
                    "DestinationUserIdentifier": row.merged_away_identifiers[0],
                }),
            )
            .await;

        let expected = merged_away_projection(&pool_id);
        assert_eq!(
            described_logins(&emulator, row.region, &merged_away).await,
            expected,
            "case {case:?}: expected {merged_away} to project {expected:?}"
        );
        // The projection is a property of the RECORD, so the listing reports the same one.
        assert_eq!(
            listed_logins(&emulator, row.region, &pool_id, &merged_away).await,
            Some(expected.clone()),
            "case {case:?}: expected the listing to report {expected:?} for {merged_away}"
        );
        // And it is permanent: reading it again does not change it.
        assert_eq!(
            described_logins(&emulator, row.region, &merged_away).await,
            expected,
            "case {case:?}: expected {merged_away} to keep projecting {expected:?}"
        );
        // The SURVIVOR is reached through its logins, so it projects provider names and not
        // the sentinel — which is what makes the sentinel the mark of the merged-away one.
        let kept = described_logins(&emulator, row.region, &survivor).await;
        assert!(
            !kept.contains(&DISABLED.to_string()) && kept.contains(&DEVELOPER_PROVIDER.to_string()),
            "case {case:?}: expected {survivor} to project its own providers, got {kept:?}"
        );
    }
}

/// RULE: every merge of one pool leaves its own merged-away identity projecting that same
/// pair, so a chain of merges leaves one such identity per merge.
#[tokio::test]
async fn every_merge_of_a_pool_leaves_its_own_identity_projecting_the_pair() {
    let emulator = Emulator::new();
    let region = "eu-west-3";

    // The rows differ in HOW MANY users are merged into one, one merge after another: the
    // rule holds per merge, so each row leaves one disabled identity per merged-away user.
    for users in [2usize, 3, 4] {
        let case = format!("{users} users merged into one");
        let pool_id = pool_in(&emulator, region, &format!("merge-chain-{users}")).await;
        let identifiers: Vec<String> = (0..users).map(|index| format!("user-{index}")).collect();
        let mut minted: Vec<String> = Vec::new();
        for identifier in &identifiers {
            minted.push(
                user_in(
                    &emulator,
                    region,
                    &pool_id,
                    DEVELOPER_PROVIDER,
                    &[identifier.as_str()],
                    &[],
                )
                .await,
            );
        }

        for merged_away in &identifiers[1..] {
            emulator
                .ok_in(
                    region,
                    "MergeDeveloperIdentities",
                    &json!({
                        "IdentityPoolId": &pool_id,
                        "DeveloperProviderName": DEVELOPER_PROVIDER,
                        "SourceUserIdentifier": &identifiers[0],
                        "DestinationUserIdentifier": merged_away,
                    }),
                )
                .await;
        }

        let expected = merged_away_projection(&pool_id);
        for merged_away in &minted[1..] {
            assert_eq!(
                described_logins(&emulator, region, merged_away).await,
                expected,
                "case {case:?}: expected {merged_away} to project {expected:?}"
            );
        }
        // The SURVIVOR carries the pool's developer provider once for itself and once for
        // each user merged into it: the merge appends the merged-away list rather than
        // collapsing a name the survivor already carries.
        assert_eq!(
            described_logins(&emulator, region, &minted[0]).await,
            vec![DEVELOPER_PROVIDER.to_string(); users],
            "case {case:?}: expected the survivor {} to carry the developer provider once per \
             merged user",
            minted[0]
        );
        // The repeated entries are the record's, not the identity's provider SET: the
        // ceiling on the providers one identity may be linked to counts a repeated name
        // once, so a federated login still links to a survivor of several merges.
        let federated = emulator
            .ok_in(
                region,
                "GetOpenIdTokenForDeveloperIdentity",
                &json!({
                    "IdentityPoolId": &pool_id,
                    "IdentityId": &minted[0],
                    "Logins": {
                        DEVELOPER_PROVIDER: &identifiers[0],
                        "graph.facebook.com": "a-facebook-access-token",
                    },
                }),
            )
            .await;
        assert_eq!(
            federated["IdentityId"],
            json!(minted[0]),
            "case {case:?}: expected a federated login to link to the survivor {}, got \
             {federated}",
            minted[0]
        );
        let linked = described_logins(&emulator, region, &minted[0]).await;
        assert!(
            linked.contains(&"graph.facebook.com".to_string()),
            "case {case:?}: expected the survivor to carry the federated provider too, got \
             {linked:?}"
        );
    }
}

/// RULE: the pair is the MERGE's own end state — an identity an unlink left linked to no
/// provider projects the marker ALONE, and one that never carried a login projects an empty
/// list, so the three states a read can report are told apart by the projection.
#[tokio::test]
async fn an_unlink_and_a_guest_identity_project_their_own_lists() {
    let emulator = Emulator::new();
    let region = "us-west-2";
    let pool_id = pool_in(&emulator, region, "projection-contrast").await;

    // An identity reached through one federated login, which an unlink then takes off it.
    let federated = emulator
        .ok_in(
            region,
            "GetId",
            &json!({
                "IdentityPoolId": &pool_id,
                "Logins": { "graph.facebook.com": "a-facebook-access-token" },
            }),
        )
        .await["IdentityId"]
        .as_str()
        .expect("GetId returns an IdentityId")
        .to_string();
    emulator
        .ok_in(
            region,
            "UnlinkIdentity",
            &json!({
                "IdentityId": &federated,
                "Logins": { "graph.facebook.com": "a-facebook-access-token" },
                "LoginsToRemove": ["graph.facebook.com"],
            }),
        )
        .await;
    assert_eq!(
        described_logins(&emulator, region, &federated).await,
        vec![DISABLED.to_string()],
        "case \"an identity an unlink left linked to nothing\": expected the marker alone"
    );

    // And an unauthenticated identity, which never carried a login at all.
    let guest = emulator
        .ok_in(region, "GetId", &json!({ "IdentityPoolId": &pool_id }))
        .await["IdentityId"]
        .as_str()
        .expect("GetId returns an IdentityId")
        .to_string();
    let projected: Vec<String> = described_logins(&emulator, region, &guest).await;
    assert!(
        projected.is_empty(),
        "case \"an identity that never carried a login\": expected an empty list, got \
         {projected:?}"
    );
}
