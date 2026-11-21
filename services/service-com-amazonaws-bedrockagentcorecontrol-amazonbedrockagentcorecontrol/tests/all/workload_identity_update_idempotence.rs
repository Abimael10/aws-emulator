// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests pinning two rules of `UpdateWorkloadIdentity` that the
//! replacement table beside this suite does not reach.
//!
//! 1. The operation is `@idempotent` (`smithy_query operation=UpdateWorkloadIdentity`):
//!    replaying the SAME request against the same identity leaves the same record — the
//!    same `allowedResourceOauth2ReturnUrls`, `name`, `workloadIdentityArn` and
//!    `createdTime`, with `lastUpdatedTime` never moving backwards — and the following
//!    `GetWorkloadIdentity` reports exactly that record. A replay is a full replacement
//!    applied again, never an append.
//! 2. A request the boundary constraints refuse writes nothing: the identity keeps the
//!    record the last ACCEPTED mutation left, member for member, and a name no accepted
//!    request ever carried stays absent.
//!
//! The accepted rows span `ResourceOauth2ReturnUrlType`'s own modelled bounds
//! (`len 1..2048`, `pattern ^\w+:(\/?\/?)[^\s]+$`, from
//! `smithy_query shape=ResourceOauth2ReturnUrlListType`): the shortest URL that pattern
//! admits, a URL of exactly the published maximum length, non-HTTP schemes, repeated
//! entries and a list of many entries. The refused rows sit one step past each published
//! bound of that member and of `WorkloadIdentityNameType` (`len 3..255`,
//! `pattern ^[A-Za-z0-9_.-]+$`).
//!
//! Every test drives the service the way the router does: an `http::Request` through
//! `MultiServiceRouter::dispatch` (through [`super::harness`]), so the wire protocol
//! (`restJson1`), the boundary constraint validation and the handlers are all exercised.

use serde_json::json;

use super::harness::{
    create_identity, error_code, expected_arn, get_identity, instant, router, stored_names, text,
    update_identity, NAME_MAX_LENGTH, NAME_MIN_LENGTH,
};

/// The modelled success status of `UpdateWorkloadIdentity` and of the read
/// (`smithy_query operation=UpdateWorkloadIdentity,GetWorkloadIdentity`: `http.code`).
const UPDATE_STATUS: u16 = 200;
const READ_STATUS: u16 = 200;

/// `ResourceOauth2ReturnUrlType`'s published length bounds, from
/// `smithy_query shape=ResourceOauth2ReturnUrlListType` (its `member`).
const URL_MIN_LENGTH: usize = 1;
const URL_MAX_LENGTH: usize = 2048;

/// The members an identity reports that a replayed update may not move at all. `name`,
/// `workloadIdentityArn` and `createdTime` belong to the identity rather than to the
/// request, and the list is what the replayed request itself carries.
const REPLAY_STABLE_MEMBERS: [&str; 4] = [
    "name",
    "workloadIdentityArn",
    "createdTime",
    "allowedResourceOauth2ReturnUrls",
];

/// A URL of exactly `length` characters inside `ResourceOauth2ReturnUrlType`'s pattern:
/// an HTTPS address whose path is padded out to the requested total length.
fn padded_url(length: usize) -> String {
    let prefix = "https://example.com/";
    assert!(
        length > prefix.len(),
        "a padded URL is longer than its {} character prefix, got {length}",
        prefix.len()
    );
    format!("{prefix}{}", "a".repeat(length - prefix.len()))
}

/// The borrowed form [`super::harness::update_identity`] takes.
fn as_refs(urls: &[String]) -> Vec<&str> {
    urls.iter().map(String::as_str).collect()
}

/// One row of the replay table: the identity to store, the list it is created with, and
/// the list the update applies — twice.
struct ReplayCase {
    case: &'static str,
    name: String,
    created_urls: Vec<String>,
    updated_urls: Vec<String>,
}

/// One row of the refusal table: a request that cannot be accepted, and the identity (if
/// any) it names.
struct RefusalCase {
    case: &'static str,
    /// Whether the request names the identity the suite holds; when false the `name` below
    /// is one no accepted request ever carried.
    names_held_identity: bool,
    name: String,
    urls: Vec<String>,
}

/// Rule 1: the same update applied twice leaves the same record, and the read reports it.
#[tokio::test]
async fn replaying_the_same_update_leaves_the_same_record() {
    let router = router();

    let rows: [ReplayCase; 6] = [
        ReplayCase {
            // The shortest value satisfying `^\w+:(\/?\/?)[^\s]+$` is three characters: one
            // word character, the colon, no slash, and one non-space character.
            case: "the shortest URL the member's pattern admits",
            name: "replay-shortest-url".to_string(),
            created_urls: vec!["https://example.com/first".to_string()],
            updated_urls: vec!["a:b".to_string()],
        },
        ReplayCase {
            case: "a URL of exactly the published maximum length",
            name: "replay-longest-url".to_string(),
            created_urls: vec![],
            updated_urls: vec![padded_url(URL_MAX_LENGTH)],
        },
        ReplayCase {
            case: "non-HTTP schemes the pattern admits",
            name: "replay.mixed_schemes-1".to_string(),
            created_urls: vec!["https://example.com/web".to_string()],
            updated_urls: vec![
                "myapp://auth/return".to_string(),
                "urn:ietf:wg:oauth:2.0:oob".to_string(),
                "http://localhost:8080/return".to_string(),
            ],
        },
        ReplayCase {
            case: "the same URL repeated, which is stored as given rather than deduplicated",
            name: "replay-repeated-entries".to_string(),
            created_urls: vec!["https://example.com/one".to_string()],
            updated_urls: vec![
                "https://example.com/same".to_string(),
                "https://example.com/same".to_string(),
                "https://example.com/other".to_string(),
                "https://example.com/same".to_string(),
            ],
        },
        ReplayCase {
            case: "a list of many entries, in an order no sort produces",
            name: "replay-many-entries".to_string(),
            created_urls: vec!["https://example.com/only".to_string()],
            updated_urls: (0..24)
                .map(|index| format!("https://example.com/cb/{}", 24 - index))
                .collect(),
        },
        ReplayCase {
            case: "the empty list, replayed, which revokes every stored URL",
            name: "replay-empty-list".to_string(),
            created_urls: vec!["https://example.com/revoked".to_string()],
            updated_urls: vec![],
        },
    ];

    let mut held: Vec<String> = Vec::new();
    for row in &rows {
        let ReplayCase {
            case,
            name,
            created_urls,
            updated_urls,
        } = row;

        let mut body = json!({"name": name});
        body["allowedResourceOauth2ReturnUrls"] = json!(as_refs(created_urls));
        let (status, created) = create_identity(&router, &body).await;
        assert_eq!(
            status, 201,
            "case {case:?}: expected HTTP 201 from CreateWorkloadIdentity, got {status} ({created})"
        );
        held.push(name.clone());

        let (_, before) = get_identity(&router, name).await;
        let created_time = instant(case, &before, "createdTime");

        let requested = as_refs(updated_urls);
        let (status, first) = update_identity(&router, name, Some(&requested)).await;
        assert_eq!(
            status, UPDATE_STATUS,
            "case {case:?}: expected HTTP {UPDATE_STATUS} from UpdateWorkloadIdentity, got \
             {status} ({first})"
        );
        assert_eq!(
            first["allowedResourceOauth2ReturnUrls"],
            json!(requested),
            "case {case:?}: the update reports exactly the list it was given, got {}",
            first["allowedResourceOauth2ReturnUrls"]
        );
        assert_eq!(
            text(&first, "workloadIdentityArn"),
            expected_arn(name),
            "case {case:?}: an update does not re-mint the ARN, got {}",
            text(&first, "workloadIdentityArn")
        );
        assert_eq!(
            instant(case, &first, "createdTime"),
            created_time,
            "case {case:?}: an update does not move createdTime, got {} for {created_time}",
            instant(case, &first, "createdTime")
        );

        // The SAME request again: a replacement applied a second time, never an append.
        let (status, second) = update_identity(&router, name, Some(&requested)).await;
        assert_eq!(
            status, UPDATE_STATUS,
            "case {case:?}: expected HTTP {UPDATE_STATUS} from the replayed \
             UpdateWorkloadIdentity, got {status} ({second})"
        );
        for member in REPLAY_STABLE_MEMBERS {
            assert_eq!(
                second[member], first[member],
                "case {case:?}: the replayed update reports the same {member}, got {} for {}",
                second[member], first[member]
            );
        }
        assert!(
            instant(case, &second, "lastUpdatedTime") >= instant(case, &first, "lastUpdatedTime"),
            "case {case:?}: lastUpdatedTime never moves backwards, got {} after {}",
            instant(case, &second, "lastUpdatedTime"),
            instant(case, &first, "lastUpdatedTime")
        );

        // The read reports the record the replay left, member for member.
        let (status, after) = get_identity(&router, name).await;
        assert_eq!(
            status, READ_STATUS,
            "case {case:?}: expected HTTP {READ_STATUS} from GetWorkloadIdentity, got {status} \
             ({after})"
        );
        assert_eq!(
            after, second,
            "case {case:?}: the read reports the record the replayed update answered, got \
             {after} for {second}"
        );

        // Every identity stored so far is still stored: an update addresses exactly one.
        // Compared as SETS, so this row fails for its own reason rather than for the order
        // the listing reports.
        let mut observed = stored_names(&router).await;
        observed.sort();
        let mut expected = held.clone();
        expected.sort();
        assert_eq!(
            observed, expected,
            "case {case:?}: expected every identity created so far, got {observed:?}"
        );
    }
}

/// Rule 2: a refused update writes nothing — neither to the identity it names nor under a
/// name no accepted request carried.
#[tokio::test]
async fn a_refused_update_leaves_every_record_untouched() {
    let router = router();

    let held_name = "refusals-are-measured-against-this";
    let held_urls = vec![
        "https://example.com/callback".to_string(),
        "myapp://return".to_string(),
    ];
    let (status, created) = create_identity(
        &router,
        &json!({"name": held_name, "allowedResourceOauth2ReturnUrls": as_refs(&held_urls)}),
    )
    .await;
    assert_eq!(
        status, 201,
        "case \"the identity the refusals are measured against\": expected HTTP 201, got {status} \
         ({created})"
    );
    let requested = as_refs(&held_urls);
    let (status, accepted) = update_identity(&router, held_name, Some(&requested)).await;
    assert_eq!(
        status, UPDATE_STATUS,
        "case \"the last accepted update\": expected HTTP {UPDATE_STATUS}, got {status} \
         ({accepted})"
    );
    let (_, baseline) = get_identity(&router, held_name).await;

    let rows: [RefusalCase; 7] = [
        RefusalCase {
            case: "a URL one character below the member's published minimum length",
            names_held_identity: true,
            name: held_name.to_string(),
            urls: vec!["a".repeat(URL_MIN_LENGTH - 1)],
        },
        RefusalCase {
            case: "a URL one character above the member's published maximum length",
            names_held_identity: true,
            name: held_name.to_string(),
            urls: vec![padded_url(URL_MAX_LENGTH + 1)],
        },
        RefusalCase {
            case: "a URL carrying whitespace, which the member's pattern excludes",
            names_held_identity: true,
            name: held_name.to_string(),
            urls: vec!["https://example.com/a b".to_string()],
        },
        RefusalCase {
            case: "a URL with no scheme, which the member's pattern excludes",
            names_held_identity: true,
            name: held_name.to_string(),
            urls: vec!["example.com/callback".to_string()],
        },
        RefusalCase {
            case: "one good URL beside one past the published maximum length",
            names_held_identity: true,
            name: held_name.to_string(),
            urls: vec![
                "https://example.com/good".to_string(),
                padded_url(URL_MAX_LENGTH + 1),
            ],
        },
        RefusalCase {
            case: "a name one character below the published minimum length",
            names_held_identity: false,
            name: "a".repeat(NAME_MIN_LENGTH - 1),
            urls: vec!["https://example.com/cb".to_string()],
        },
        RefusalCase {
            case: "a name one character above the published maximum length",
            names_held_identity: false,
            name: "z".repeat(NAME_MAX_LENGTH + 1),
            urls: vec!["https://example.com/cb".to_string()],
        },
    ];

    for row in &rows {
        let RefusalCase {
            case,
            names_held_identity,
            name,
            urls,
        } = row;

        let requested = as_refs(urls);
        let (status, refused) = update_identity(&router, name, Some(&requested)).await;
        assert_eq!(
            (status, error_code(&refused)),
            (400, "ValidationException".to_string()),
            "case {case:?}: expected HTTP 400 ValidationException, got {status} {} ({refused})",
            error_code(&refused)
        );

        // The held identity still reports the record the last accepted update left.
        let (status, after) = get_identity(&router, held_name).await;
        assert_eq!(
            (status, after.clone()),
            (READ_STATUS, baseline.clone()),
            "case {case:?}: a refused update leaves the held identity untouched, got {status} \
             ({after}) for ({baseline})"
        );

        // A refusal naming a name nothing holds creates nothing under it. Such a name is
        // outside `WorkloadIdentityNameType`, so the read route refuses it too and the
        // listing is what reports whether anything was stored.
        if !names_held_identity {
            let listed = stored_names(&router).await;
            assert!(
                !listed.contains(name),
                "case {case:?}: a refused update creates no identity, got {listed:?}"
            );
        }
    }

    // One identity, the one the creates made: no refusal added a record.
    assert_eq!(
        stored_names(&router).await,
        vec![held_name.to_string()],
        "case \"after every refusal\": expected only the held identity, got {:?}",
        stored_names(&router).await
    );
    let (_, final_read) = get_identity(&router, held_name).await;
    assert_eq!(
        final_read["allowedResourceOauth2ReturnUrls"],
        json!(as_refs(&held_urls)),
        "case \"the held identity after every refusal\": expected the last accepted list, got {}",
        final_read["allowedResourceOauth2ReturnUrls"]
    );
}
