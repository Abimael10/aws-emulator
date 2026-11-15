// SPDX-License-Identifier: CC0-1.0

//! The MEMBERS a `GetDirectoryLimits` answer carries, driven end to end over HTTP.
//!
//! Every case goes through the same front door a client uses: `POST /` with
//! `X-Amz-Target: DirectoryService_20150416.<Operation>` and an `awsJson1_1` body,
//! dispatched by the multi-service router (`crate::harness`), and builds the population it
//! reports on through `CreateDirectory` and `CreateMicrosoftAD`. Nothing reaches into the
//! state struct, so a flag that is not computed from persisted state cannot pass.
//!
//! The rule under test: a limits answer reports EVERY limit flag Directory Service carries
//! as an explicit boolean — the three per-category `...LimitReached` flags the model
//! declares and the `IdentityPoolDirectoriesLimitReached` flag it does not
//! (`crate::ops::types::directory_limits_wire` in the crate is where that member is
//! declared, and why) — so a client that reads a flag off the answer to decide whether
//! another directory can be created finds a boolean there rather than an absent member. A
//! per-category flag is on exactly when that category's own count has reached its own
//! quota; the identity-pool flag is off, because an identity-pool directory is created
//! through the identity service and none of the directory types this account can hold here
//! counts towards that allowance.
//!
//! The sibling suite `directory_limits` is what asserts the three counters themselves —
//! each category's count, its quota and the comparison between them — over populations,
//! deletes and each category's own bound. This suite is about the member SET of the answer
//! and about the flag that suite's counters do not cover.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::directory_lifecycle::{
    create_directory, minimal_create, CLOUD_ONLY_QUOTA, CONNECTED_QUOTA, MICROSOFT_AD_QUOTA,
};
use crate::harness::{ok, service};
use crate::microsoft_ad::{create_microsoft_ad, minimal_microsoft_ad};

/// The flag a limits answer carries for IDENTITY-POOL directories: a member the on-disk
/// model does not declare and a Directory Service answer does.
const IDENTITY_POOL_FLAG: &str = "IdentityPoolDirectoriesLimitReached";

/// Every member a limits answer carries, as `(count, limit, flag, that category's own
/// published quota)` — the three the model declares
/// (`smithy_query(query="shape",shape="DirectoryLimits")`), in its own member order.
const COUNTERS: [(&str, &str, &str, usize); 3] = [
    (
        "CloudOnlyDirectoriesCurrentCount",
        "CloudOnlyDirectoriesLimit",
        "CloudOnlyDirectoriesLimitReached",
        CLOUD_ONLY_QUOTA,
    ),
    (
        "CloudOnlyMicrosoftADCurrentCount",
        "CloudOnlyMicrosoftADLimit",
        "CloudOnlyMicrosoftADLimitReached",
        MICROSOFT_AD_QUOTA,
    ),
    (
        "ConnectedDirectoriesCurrentCount",
        "ConnectedDirectoriesLimit",
        "ConnectedDirectoriesLimitReached",
        CONNECTED_QUOTA,
    ),
];

/// One row: the population to build, as `(Simple AD directories, Managed Microsoft AD
/// directories)`.
struct Population {
    case: &'static str,
    simple_ad: usize,
    microsoft_ad: usize,
}

/// The `DirectoryLimits` structure of a limits answer, which the request takes no members
/// to narrow.
async fn limits(router: &MultiServiceRouter, case: &str) -> Value {
    let answer = ok(router, "GetDirectoryLimits", json!({}), case).await;
    let limits = answer["DirectoryLimits"].clone();
    assert!(
        limits.is_object(),
        "case {case:?}: expected a DirectoryLimits object, got {answer}"
    );
    limits
}

/// A router holding the row's population, built through the public creates.
async fn populated(row: &Population) -> MultiServiceRouter {
    let router = service();
    for index in 0..row.simple_ad {
        create_directory(
            &router,
            minimal_create(&format!("flag-simple{index}.example.com")),
            row.case,
        )
        .await;
    }
    for index in 0..row.microsoft_ad {
        create_microsoft_ad(
            &router,
            minimal_microsoft_ad(&format!("flag-managed{index}.example.com")),
            row.case,
        )
        .await;
    }
    router
}

/// The populations the rules below are asserted over: they differ in how many directories
/// are stored, in the MIX of types that makes up the population, and in whether a
/// category's own quota has been reached — so a flag read from the wrong category, or one
/// omitted on some populations and not others, fails here.
fn populations() -> [Population; 5] {
    [
        Population {
            case: "a fresh Region",
            simple_ad: 0,
            microsoft_ad: 0,
        },
        Population {
            case: "one Simple AD directory",
            simple_ad: 1,
            microsoft_ad: 0,
        },
        Population {
            case: "one Managed Microsoft AD directory",
            simple_ad: 0,
            microsoft_ad: 1,
        },
        Population {
            case: "a mix of both types",
            simple_ad: 3,
            microsoft_ad: 2,
        },
        Population {
            case: "the cloud-only quota used up",
            simple_ad: CLOUD_ONLY_QUOTA,
            microsoft_ad: 1,
        },
    ]
}

#[tokio::test]
async fn every_limit_flag_the_answer_carries_is_an_explicit_boolean() {
    for row in populations() {
        let router = populated(&row).await;
        let limits = limits(&router, row.case).await;

        // The three modelled flags and the identity-pool one are all present, and all
        // BOOLEANS — not absent, and not some other JSON type a client would have to
        // coerce.
        let flags: Vec<&str> = COUNTERS
            .iter()
            .map(|(_, _, flag, _)| *flag)
            .chain(std::iter::once(IDENTITY_POOL_FLAG))
            .collect();
        for flag in &flags {
            assert!(
                limits[flag].is_boolean(),
                "case {:?}: expected {flag} to be reported as a boolean, got {limits}",
                row.case
            );
        }

        // Each category's own flag follows its own count against its own quota...
        for (count, _, flag, quota) in COUNTERS {
            let stored = limits[count]
                .as_u64()
                .unwrap_or_else(|| panic!("case {:?}: expected {count}, got {limits}", row.case));
            let allowance = u64::try_from(quota).expect("the quota fits a u64");
            assert_eq!(
                limits[flag],
                json!(stored >= allowance),
                "case {:?}: expected {flag} on only once that category's own quota is used \
                 up ({stored} of {allowance}), got {limits}",
                row.case
            );
        }

        // ...and the identity-pool flag is OFF whatever the population: an identity-pool
        // directory is created through the identity service, and nothing the account holds
        // here counts towards that allowance.
        assert_eq!(
            limits[IDENTITY_POOL_FLAG],
            json!(false),
            "case {:?}: expected {IDENTITY_POOL_FLAG} off for an account holding no \
             identity-pool directory, got {limits}",
            row.case
        );

        // And nothing else is reported: the answer carries exactly the members Directory
        // Service carries, so a client iterating the object sees no member it cannot place.
        let reported: Vec<String> = limits
            .as_object()
            .map(|object| object.keys().cloned().collect())
            .unwrap_or_default();
        let mut expected: Vec<String> = COUNTERS
            .iter()
            .flat_map(|(count, limit, flag, _)| [count.to_string(), limit.to_string(), flag.to_string()])
            .collect();
        expected.push(IDENTITY_POOL_FLAG.to_string());
        expected.sort();
        let mut found = reported.clone();
        found.sort();
        assert_eq!(
            found, expected,
            "case {:?}: expected exactly the members of a limits answer, got {reported:?}",
            row.case
        );
    }
}

#[tokio::test]
async fn the_flag_set_is_the_same_on_every_read_and_a_read_changes_nothing() {
    let row = Population {
        case: "one of each type, read repeatedly",
        simple_ad: 1,
        microsoft_ad: 1,
    };
    let router = populated(&row).await;

    // A reporting operation: every read answers the same structure, flags included, and a
    // create after them is still served — so no read consumed the allowance it reports on.
    let first = limits(&router, row.case).await;
    for repeat in 1..=3 {
        let case = format!("{} (read {repeat})", row.case);
        assert_eq!(
            limits(&router, &case).await,
            first,
            "case {case:?}: expected every read to report the same limits as {first}"
        );
    }
    create_directory(
        &router,
        minimal_create("flag-after-reads.example.com"),
        row.case,
    )
    .await;
    let after = limits(&router, row.case).await;
    assert!(
        after[IDENTITY_POOL_FLAG].is_boolean(),
        "case {:?}: expected {IDENTITY_POOL_FLAG} to still be reported after a create, got \
         {after}",
        row.case
    );
    assert_eq!(
        after["CloudOnlyDirectoriesCurrentCount"],
        json!(2),
        "case {:?}: expected the create to be counted, got {after}",
        row.case
    );
}
