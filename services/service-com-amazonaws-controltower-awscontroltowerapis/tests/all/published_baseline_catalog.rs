// SPDX-License-Identifier: CC0-1.0

//! The baseline catalog's PUBLISHED values, as `ListBaselines` and `GetBaseline` serve
//! them over HTTP.
//!
//! Drives both operations through the same
//! [`MultiServiceRouter`](smithy_server_runtime::MultiServiceRouter) the production
//! emulator dispatches with, so what these tests exercise is the wire behaviour a client
//! sees.
//!
//! The RULE under test: a baseline is AWS-managed catalog data, identical in every
//! partition, region and account, so each entry's name, description and the 16-character
//! id segment of its ARN are AWS's own fixed values, which a client discovers by name or
//! ARN and then hands back to `GetBaseline`/`EnableBaseline` — the emulator reproduces
//! them rather than wording or minting its own. Only the partition and region segments of
//! a served ARN come from the request, and the account segment is empty.
//!
//! The sibling suites (`crate::baseline_catalog`, `crate::list_baselines`) assert the
//! listing and resolution rules against the service's own catalog constant; this suite
//! pins the published VALUES independently of that constant, so a re-worded description or
//! a re-minted id segment fails here even though both copies would still agree with each
//! other. One catalog stands behind both index keys a reader uses, so the listing is
//! checked indexed by `name` and indexed by `arn`, and an ARN taken off the listing is
//! read back through `GetBaseline`.
//!
//! The table below is AWS's published baseline catalog: the entries `ListBaselines`
//! returns in every account, including the ones no issue cites (`BackupAdminBaseline`,
//! `CentralConfigBaseline`, `CentralSecurityRolesBaseline`, `ConfigBaseline`). Each
//! description is AWS's own fixed sentence for that baseline, pinned here independently of
//! `state::BASELINE_CATALOG`, so a re-worded or summarised one fails here. Its member
//! names and types come from `smithy_query(query="shape",
//! shape="BaselineSummary,GetBaselineOutput,BaselineArn")` on this crate's `smithy-model/`:
//! a `BaselineSummary` is `arn` (required), `name` (required) and `description`, and
//! `BaselineArn`'s pattern is
//! `^arn:[a-z-]+:controltower:[a-z0-9-]*:[0-9]{0,12}:baseline/[A-Z0-9]{16}$`.

use std::collections::BTreeMap;

use smithy_server_runtime::MultiServiceRouter;

use crate::landing_zone_lifecycle::{call_in_region, emulator, Answer, SIGNED_REGION};

/// One published catalog entry: the name AWS publishes, the fixed id segment its ARN ends
/// in, and the fixed description text.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Published {
    name: &'static str,
    id: &'static str,
    description: &'static str,
}

/// AWS's published baseline catalog, in the order `ListBaselines` reports it.
const PUBLISHED_CATALOG: &[Published] = &[
    Published {
        name: "AuditBaseline",
        id: "4T4HA1KMO10S6311",
        description: "Sets up resources to monitor security and compliance of accounts \
                      in your organization.",
    },
    Published {
        name: "AWSControlTowerBaseline",
        id: "17BSJV3IGJ2QSGA2",
        description: "Sets up resources and mandatory controls for member accounts \
                      within the target OU, required for AWS Control Tower governance.",
    },
    Published {
        name: "BackupAdminBaseline",
        id: "H6C5JFCJJ3CPU3J5",
        description: "Sets up AWS Backup Audit Manager.",
    },
    Published {
        name: "BackupBaseline",
        id: "APO9ATVPBKFRRGLK",
        description: "Sets up a local AWS Backup vault and attaches multiple AWS Backup \
                      plans.",
    },
    Published {
        name: "BackupCentralVaultBaseline",
        id: "3WPD0NA6TJ9AOMU2",
        description: "Sets up a central AWS Backup vault in your organization.",
    },
    Published {
        name: "CentralConfigBaseline",
        id: "YX7VMZML5IG8EJUD",
        description: "Sets up central resources for compliance monitoring and auditing \
                      within your organization using AWS Config.",
    },
    Published {
        name: "CentralSecurityRolesBaseline",
        id: "A6EEAE10F08193F2",
        description: "Sets up central resources for security monitoring within your \
                      organization.",
    },
    Published {
        name: "ConfigBaseline",
        id: "1QBGH2G48YVGDQ3Y",
        description: "Sets up AWS Config resources for member accounts within the target \
                      OU, required for Detective Controls enablement. The resources set \
                      up are a subset of resources of AWSControlTowerBaseline.",
    },
    Published {
        name: "IdentityCenterBaseline",
        id: "LN25R72TTG6IGPTQ",
        description: "Sets up shared resources for AWS Identity Center, which prepares \
                      the AWSControlTowerBaseline to set up Identity Center access for \
                      accounts.",
    },
    Published {
        name: "LogArchiveBaseline",
        id: "J8HX46AHS5MIKQPD",
        description: "Sets up a central repository for logs of API activities and \
                      resource configurations from accounts in your organization.",
    },
];

/// The regions the rule is checked in, spanning what it generalises over (the request's
/// signed region, which is the only part of a served baseline ARN that moves).
const REQUEST_REGIONS: &[&str] = &[SIGNED_REGION, "us-east-1", "eu-central-1", "ap-south-1"];

/// The ARN `region` serves a published baseline under: that region, and the EMPTY account
/// segment a baseline no account owns carries.
fn published_arn(region: &str, entry: &Published) -> String {
    format!("arn:aws:controltower:{region}::baseline/{}", entry.id)
}

/// One listed or read baseline, as a client reads it off the response body.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Reported {
    arn: String,
    name: String,
    description: String,
}

/// The `arn`/`name`/`description` members of a body, missing ones read as empty.
fn reported(answer: &Answer, entry: &serde_json::Value) -> Reported {
    let member = |name: &str| {
        entry
            .get(name)
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string()
    };
    assert!(
        !member("arn").is_empty() && !member("name").is_empty(),
        "both required members must be reported, got {:?}",
        answer.body
    );
    Reported {
        arn: member("arn"),
        name: member("name"),
        description: member("description"),
    }
}

/// POST `/list-baselines` from `region`, with no pagination members at all.
async fn list_baselines(router: &MultiServiceRouter, region: &str) -> Answer {
    call_in_region(router, region, "/list-baselines", serde_json::json!({})).await
}

/// POST `/get-baseline` from `region` with `identifier`.
async fn get_baseline(router: &MultiServiceRouter, region: &str, identifier: &str) -> Answer {
    call_in_region(
        router,
        region,
        "/get-baseline",
        serde_json::json!({ "baselineIdentifier": identifier }),
    )
    .await
}

/// RULE: `ListBaselines` publishes AWS's catalog — every entry's fixed name, fixed
/// description and fixed ARN id segment — whatever region the request is signed for, the
/// region being the only part of an entry that moves with the request.
#[tokio::test]
async fn the_listing_publishes_the_aws_catalog_in_every_region() {
    let router = emulator();

    for region in REQUEST_REGIONS {
        let case = format!("the listing signed for {region}");
        let answer = list_baselines(&router, region).await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected 200, got {} body {:?}",
            answer.status, answer.body
        );

        let entries = answer
            .get("baselines")
            .as_array()
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: expected a baselines list, got {:?}",
                    answer.body
                )
            })
            .iter()
            .map(|entry| reported(&answer, entry))
            .collect::<Vec<Reported>>();

        let expected: Vec<Reported> = PUBLISHED_CATALOG
            .iter()
            .map(|entry| Reported {
                arn: published_arn(region, entry),
                name: entry.name.to_string(),
                description: entry.description.to_string(),
            })
            .collect();

        assert_eq!(
            entries, expected,
            "case {case:?}: expected AWS's published catalog, got {:?}",
            answer.body
        );
    }
}

/// RULE (the resolution side of it): the ARN a published entry carries resolves, through
/// `GetBaseline`, to THAT entry's name and description — so an ARN a client read out of the
/// listing, or embedded from AWS's documentation, names the baseline AWS names.
#[tokio::test]
async fn every_published_arn_resolves_to_its_own_baseline() {
    let router = emulator();

    for entry in PUBLISHED_CATALOG {
        for region in REQUEST_REGIONS {
            // Asked for in the ARN form AWS documents, from a request signed for the
            // region in the ARN and from one signed for another: the id segment is the
            // identity, so both answer the same baseline.
            for (form, identifier) in [
                ("the ARN of the signed region", published_arn(region, entry)),
                (
                    "an ARN of another region",
                    published_arn("us-gov-west-1", entry),
                ),
            ] {
                let case = format!("{} via {form}, signed for {region}", entry.name);
                let answer = get_baseline(&router, region, &identifier).await;
                assert_eq!(
                    answer.status, 200,
                    "case {case:?}: expected 200 for {identifier:?}, got {} body {:?}",
                    answer.status, answer.body
                );
                assert_eq!(
                    reported(&answer, &answer.body),
                    Reported {
                        arn: published_arn(region, entry),
                        name: entry.name.to_string(),
                        description: entry.description.to_string(),
                    },
                    "case {case:?}: expected the published entry, got {:?}",
                    answer.body
                );
            }
        }
    }
}

/// RULE (the other side of a fixed catalog): an id segment AWS's catalog does not publish
/// names no baseline, so it is `ResourceNotFoundException` (404) — including one that
/// differs from a published id in a single character.
#[tokio::test]
async fn an_id_the_published_catalog_does_not_carry_is_not_found() {
    let mut cases: Vec<(String, String)> = PUBLISHED_CATALOG
        .iter()
        .map(|entry| {
            let (head, last) = entry.id.split_at(15);
            let rotated = if last == "Z" { "Y" } else { "Z" };
            (
                format!("{} with its last character changed", entry.name),
                format!("{head}{rotated}"),
            )
        })
        .collect();
    cases.push((
        "an id of no baseline at all".to_string(),
        "9XQV3TLK8MDR5WZA".to_string(),
    ));

    let router = emulator();
    for (case, id) in cases {
        let identifier = format!("arn:aws:controltower:{SIGNED_REGION}::baseline/{id}");
        let answer = get_baseline(&router, SIGNED_REGION, &identifier).await;
        assert_eq!(
            (answer.status, answer.error_code.as_deref()),
            (404, Some("ResourceNotFoundException")),
            "case {case:?}: expected 404 ResourceNotFoundException for {identifier:?}, \
             got {:?} body {:?}",
            (answer.status, answer.error_code.as_deref()),
            answer.body
        );
    }
}

/// The page cuts the listing is walked with below, spanning what the projection rule
/// generalises over: each published bound of `ListBaselinesMaxResults`
/// (`@range(min: 4, max: 100)`) and a size between them that divides the catalog unevenly.
/// The unpaginated listing is the sibling test above, so it is not repeated here.
const PAGE_CUTS: &[Option<i64>] = &[Some(4), Some(7), Some(100)];

/// The whole listing walked with `max_results`, as a map from each entry's `name` to the
/// `description` it was reported with.
///
/// The walk itself is the sibling listing suite's ([`crate::list_baselines::walk`]), so
/// there is one token loop in this crate's tests rather than a second copy of it here.
async fn walked_descriptions(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    max_results: Option<i64>,
) -> BTreeMap<String, String> {
    let rows = walked_rows(router, case, region, max_results).await;
    indexed_by(IndexKey::Name, case, &rows)
        .into_iter()
        .map(|(name, row)| (name, row.description))
        .collect()
}

/// The whole listing walked with `max_results`, as the [`Reported`] rows a client reads off
/// the responses.
///
/// The walk itself is the sibling listing suite's ([`crate::list_baselines::walk`]), so
/// there is one token loop in this crate's tests rather than a second copy of it here.
async fn walked_rows(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    max_results: Option<i64>,
) -> Vec<Reported> {
    crate::list_baselines::walk(router, case, region, max_results)
        .await
        .0
        .into_iter()
        .map(|entry| Reported {
            arn: entry.arn,
            name: entry.name,
            description: entry.description,
        })
        .collect()
}

/// AWS's published text, as a map from name to description.
fn published_descriptions() -> BTreeMap<String, String> {
    PUBLISHED_CATALOG
        .iter()
        .map(|entry| (entry.name.to_string(), entry.description.to_string()))
        .collect()
}

/// RULE (the description is FIXED text): every projection of the catalog reports AWS's own
/// sentence for a baseline, whatever page cut the listing is walked with and whatever
/// region the request is signed for — a page boundary and a region change move the ARNs,
/// never the published text.
///
/// Each page cut is taken in a different signed region, so the cases span both dimensions
/// without walking the listing once per pair.
#[tokio::test]
async fn every_page_cut_in_every_region_reports_the_published_description() {
    let router = emulator();
    let published = published_descriptions();

    for (index, max_results) in PAGE_CUTS.iter().enumerate() {
        let region = REQUEST_REGIONS[index % REQUEST_REGIONS.len()];
        let case = match max_results {
            Some(max) => format!("the listing in {region} walked with maxResults {max}"),
            None => format!("the listing in {region} with maxResults omitted"),
        };
        let walked = walked_descriptions(&router, &case, region, *max_results).await;
        assert_eq!(
            walked, published,
            "case {case:?}: expected AWS's published description for every baseline, \
             got {walked:?}"
        );
    }
}

/// RULE (what FIXED text means): a published description is the same BYTES for every
/// caller and carries no request-derived and no state-derived part — the text a region's
/// listing reports is the text every other region's reports, and no signed region, account
/// id or ARN id segment appears inside it.
///
/// A GUARD against the text ever becoming templated: the tests above pin the wording
/// itself, and this one fails the day a description is built from the request or the store
/// instead of reproduced.
#[tokio::test]
async fn a_published_description_carries_no_request_or_state_derived_part() {
    let router = emulator();
    let account = ::engine::emulator_account_id();
    let mut first_seen: BTreeMap<String, (String, String)> = BTreeMap::new();

    for region in REQUEST_REGIONS {
        let case = format!("the listing signed for {region}");
        let answer = list_baselines(&router, region).await;
        let entries = answer
            .get("baselines")
            .as_array()
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: expected a baselines list, got {:?}",
                    answer.body
                )
            })
            .clone();
        assert!(
            !entries.is_empty(),
            "case {case:?}: expected the published catalog, got {:?}",
            answer.body
        );

        for entry in &entries {
            let reported = reported(&answer, entry);
            let id = reported
                .arn
                .rsplit_once(":baseline/")
                .map(|(_, id)| id.to_string())
                .unwrap_or_else(|| {
                    panic!(
                        "case {case:?}: expected a baseline ARN, got {:?}",
                        reported.arn
                    )
                });

            for (part, value) in [
                ("the signed region", (*region).to_string()),
                ("the account id", account.to_string()),
                ("the ARN's id segment", id),
            ] {
                assert!(
                    !reported.description.contains(&value),
                    "case {case:?}: {}'s description must carry no {part}, got {:?} \
                     containing {value:?}",
                    reported.name,
                    reported.description
                );
            }

            // The same bytes for every caller: the first region to report this baseline
            // fixes the text every later region must repeat.
            match first_seen.get(&reported.name) {
                Some((seen_in, seen_as)) => assert_eq!(
                    &reported.description,
                    seen_as,
                    "case {case:?}: {}'s description must be the text {seen_in} reported, \
                     got {:?}",
                    reported.name,
                    reported.description
                ),
                None => {
                    first_seen.insert(
                        reported.name.clone(),
                        ((*region).to_string(), reported.description.clone()),
                    );
                }
            }
        }
    }

    assert_eq!(
        first_seen.len(),
        PUBLISHED_CATALOG.len(),
        "every published baseline must have been read at least once, got {first_seen:?}"
    );
}

/// Which required member of a listed entry a reader builds its index on. Both `arn` and
/// `name` are required members of `BaselineSummary`, so a client may key the response on
/// either — and a catalog that is one table answers both keys with the same entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IndexKey {
    Arn,
    Name,
}

impl IndexKey {
    /// The member this index is built on, as the response spells it.
    fn member(self) -> &'static str {
        match self {
            IndexKey::Arn => "arn",
            IndexKey::Name => "name",
        }
    }

    /// The key this index reads off a reported entry.
    fn of(self, entry: &Reported) -> String {
        match self {
            IndexKey::Arn => entry.arn.clone(),
            IndexKey::Name => entry.name.clone(),
        }
    }
}

/// AWS's published catalog as a reader indexing it by `key` sees it in `region`: the whole
/// published row under each key, so a mis-paired or re-worded entry is named in full when a
/// case fails.
fn published_by(key: IndexKey, region: &str) -> BTreeMap<String, Reported> {
    PUBLISHED_CATALOG
        .iter()
        .map(|entry| {
            let row = Reported {
                arn: published_arn(region, entry),
                name: entry.name.to_string(),
                description: entry.description.to_string(),
            };
            (key.of(&row), row)
        })
        .collect()
}

/// `rows` indexed by `key`, refusing a key the listing reported twice — an index a client
/// builds is only a reading of the catalog if each key leads to one entry.
fn indexed_by(key: IndexKey, case: &str, rows: &[Reported]) -> BTreeMap<String, Reported> {
    let mut indexed: BTreeMap<String, Reported> = BTreeMap::new();
    for row in rows {
        assert!(
            indexed.insert(key.of(row), row.clone()).is_none(),
            "case {case:?}: {:?} must be listed once, got it twice",
            key.of(row)
        );
    }
    indexed
}

/// The page cut each region below is walked with: `maxResults` omitted, each published bound
/// of `ListBaselinesMaxResults` (`@range(min: 4, max: 100)`) and a size between them that
/// divides the catalog unevenly — a page boundary must move no entry between the keys.
const INDEX_KEY_PAGE_CUTS: &[Option<i64>] = &[None, Some(4), Some(7), Some(100)];

/// RULE (one catalog behind both index keys): a reader indexes the listing by `name` or by
/// `arn` — both required members — and either key leads to the SAME published entry, so the
/// description reached through an ARN is AWS's sentence for the baseline AWS pairs with that
/// id, not another baseline's and not a re-wording of it; and the ARN so read resolves
/// through `GetBaseline` to that same entry.
///
/// Each region is walked with a different page cut, so the cases span both dimensions the
/// projection could have varied along without walking the listing once per pair.
#[tokio::test]
async fn both_index_keys_of_the_listing_report_the_published_entry() {
    let router = emulator();
    assert_eq!(
        REQUEST_REGIONS.len(),
        INDEX_KEY_PAGE_CUTS.len(),
        "every region must be walked with a page cut of its own, got {} regions and {} cuts",
        REQUEST_REGIONS.len(),
        INDEX_KEY_PAGE_CUTS.len()
    );

    for (index, (region, max_results)) in REQUEST_REGIONS
        .iter()
        .zip(INDEX_KEY_PAGE_CUTS)
        .enumerate()
    {
        let case = match max_results {
            Some(max) => format!("the listing in {region} walked with maxResults {max}"),
            None => format!("the listing in {region} with maxResults omitted"),
        };
        let rows = walked_rows(&router, &case, region, *max_results).await;

        for key in [IndexKey::Arn, IndexKey::Name] {
            let case = format!("{case}, indexed by {}", key.member());
            assert_eq!(
                indexed_by(key, &case, &rows),
                published_by(key, region),
                "case {case:?}: expected AWS's published entry under every key, got {rows:?}"
            );
        }

        // The ARN-keyed path a client walks next: one entry per case, asked for by the ARN
        // as the response reported it rather than one this suite built, must resolve to the
        // very entry the listing showed beside it.
        let row = &rows[index % rows.len()];
        let case = format!("{} read back through its listed ARN in {region}", row.name);
        let read = get_baseline(&router, region, &row.arn).await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: expected 200 for {:?}, got {} body {:?}",
            row.arn, read.status, read.body
        );
        assert_eq!(
            &reported(&read, &read.body),
            row,
            "case {case:?}: expected the entry the listing reported, got {:?}",
            read.body
        );
    }
}

/// RULE (a read changes nothing): reading the catalog — listing it whole, walking it in
/// pages, point-reading each entry — leaves the published text as it was, so a later
/// caller reads exactly what the first one did.
#[tokio::test]
async fn reading_the_catalog_leaves_every_description_as_it_was() {
    let router = emulator();
    let published = published_descriptions();

    let before = walked_descriptions(&router, "the first listing", SIGNED_REGION, None).await;
    assert_eq!(
        before, published,
        "case {:?}: expected AWS's published descriptions, got {before:?}",
        "the first listing"
    );

    // Every other projection of the catalog, exercised between the two listings.
    for entry in PUBLISHED_CATALOG {
        let identifier = published_arn(SIGNED_REGION, entry);
        let answer = get_baseline(&router, SIGNED_REGION, &identifier).await;
        assert_eq!(
            answer.status, 200,
            "case {:?}: expected 200, got {} body {:?}",
            entry.name, answer.status, answer.body
        );
    }
    let paged = walked_descriptions(&router, "a paged walk", SIGNED_REGION, Some(4)).await;
    assert_eq!(
        paged, published,
        "case {:?}: expected AWS's published descriptions, got {paged:?}",
        "a paged walk"
    );

    let after = walked_descriptions(&router, "the listing read back", SIGNED_REGION, None).await;
    assert_eq!(
        after, before,
        "case {:?}: expected the descriptions the first listing reported, got {after:?}",
        "the listing read back"
    );
}
