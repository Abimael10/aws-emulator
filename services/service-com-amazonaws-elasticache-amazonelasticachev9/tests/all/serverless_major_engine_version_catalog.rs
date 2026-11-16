// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the `MajorEngineVersion` a serverless cache may run.
//!
//! The rule under test: `MajorEngineVersion` carries no modelled enum or pattern, so WHICH
//! versions a serverless cache may be created on or moved to is enforced server-side, and
//! the one answer to it is the engine-version catalog `DescribeCacheEngineVersions`
//! publishes. A request naming a major line that catalog publishes a version of is served;
//! one naming a line it publishes nothing for asks for an `Engine` and a
//! `MajorEngineVersion` that do not go together and is refused HTTP 400
//! `InvalidParameterCombination`, worded `Specified engine does not support major engine
//! version (<the requested version>).`, with nothing stored. A value that names no version
//! at all is a different refusal — a bad value of that one member, HTTP 400
//! `InvalidParameterValue` — so the two classes stay apart.
//!
//! The tests are tables whose rows differ along what the rule generalises over: the ENGINE
//! (every one the catalog publishes), the FORM of the version (a line, a line with a minor
//! component, an exact published release), which side of the catalog the version falls on,
//! and the operation that takes the member (`CreateServerlessCache` and
//! `ModifyServerlessCache`). The accepted lines are not written out here: they are READ from
//! `DescribeCacheEngineVersions` over the wire, and every refused row asserts first that the
//! live catalog really offers nothing for it — so a row cannot pass by agreeing with a
//! literal in this file.
//!
//! Every assertion goes over the wire through [`crate::harness`], and every accepted create
//! is read back through `DescribeServerlessCaches`, so a version that was not persisted
//! cannot pass.

use service_com_amazonaws_elasticache_amazonelasticachev9::state;

use crate::harness::{params, poll_until, Emulator, Reply, REGION};

/// The sentence ElastiCache refuses an engine/major-version pair it does not offer with.
///
/// Written out here on purpose: a test that built it from the helper the handlers call
/// would pass whatever that helper said.
fn unsupported(version: &str) -> String {
    format!("Specified engine does not support major engine version ({version}).")
}

/// Every `(engine, engine_version)` the published catalog reports, read over the wire.
async fn published_catalog(emulator: &Emulator) -> Vec<(String, String)> {
    let reply = emulator
        .call("DescribeCacheEngineVersions", &params(&[("MaxRecords", "100")]))
        .await;
    assert_eq!(
        reply.status, 200,
        "DescribeCacheEngineVersions must answer 200, got {} {}",
        reply.status, reply.body
    );
    let engines = reply.all("Engine");
    let versions = reply.all("EngineVersion");
    assert_eq!(
        engines.len(),
        versions.len(),
        "every published version must name its engine, got {:?} / {:?}",
        engines,
        versions
    );
    assert!(
        !engines.is_empty(),
        "this suite needs a published engine-version catalog, got {}",
        reply.body
    );
    engines
        .into_iter()
        .map(str::to_string)
        .zip(versions.into_iter().map(str::to_string))
        .collect()
}

/// True when `catalog` publishes `version`, or a version of the line `version` names, for
/// `engine` — the same reading a filter of `DescribeCacheEngineVersions` gives a value that
/// names only the leading components of a version.
fn catalog_offers(catalog: &[(String, String)], engine: &str, version: &str) -> bool {
    catalog.iter().any(|(published_engine, published)| {
        published_engine == engine
            && (published == version
                || published
                    .strip_prefix(version)
                    .is_some_and(|rest| rest.starts_with('.')))
    })
}

/// `CreateServerlessCache` for one cache on one engine and major version.
async fn create_cache(emulator: &Emulator, name: &str, engine: &str, version: &str) -> Reply {
    emulator
        .call(
            "CreateServerlessCache",
            &params(&[
                ("ServerlessCacheName", name),
                ("Engine", engine),
                ("MajorEngineVersion", version),
            ]),
        )
        .await
}

/// `DescribeServerlessCaches` for one cache by name.
async fn describe_cache(emulator: &Emulator, name: &str) -> Reply {
    emulator
        .call(
            "DescribeServerlessCaches",
            &params(&[("ServerlessCacheName", name)]),
        )
        .await
}

/// Assert `reply` is the refusal `status` / `code` worded exactly `message`.
fn assert_refusal(case: &str, reply: &Reply, status: u16, code: &str, message: &str) {
    assert_eq!(
        (reply.status, reply.error_code()),
        (status, code),
        "case {case}: expected {status} {code}, got {} {}",
        reply.status,
        reply.body
    );
    assert_eq!(
        reply.error_message(),
        message,
        "case {case}: expected message {message:?}, got {:?}",
        reply.error_message()
    );
}

#[tokio::test]
async fn every_major_line_the_published_catalog_offers_is_created_and_read_back() {
    let emulator = Emulator::new();
    let catalog = published_catalog(&emulator).await;

    // The axis: every engine the catalog publishes, and for each the FORM of the version a
    // request may name — one exact published version of it, and the major LINE another of its
    // versions belongs to. The rows are derived from the catalog itself, so an engine the
    // service starts offering is covered without this file changing; two rows per engine keep
    // both forms of the member under test without creating a cache per published release,
    // which the sibling rows below and the lifecycle suite already do.
    let mut rows: Vec<(String, String)> = Vec::new();
    for (engine, version) in &catalog {
        match rows.iter().filter(|(named, _)| named == engine).count() {
            // The engine's first published version stands for the EXACT form.
            0 => rows.push((engine.clone(), version.clone())),
            // Another of its versions stands for the LINE form, as long as that line is not
            // the exact value already named for this engine.
            1 => {
                let line = version
                    .split('.')
                    .next()
                    .unwrap_or(version.as_str())
                    .to_string();
                if !rows
                    .iter()
                    .any(|(named_engine, named)| named_engine == engine && *named == line)
                {
                    rows.push((engine.clone(), line));
                }
            }
            _ => {}
        }
    }
    assert!(
        rows.len() > 1,
        "this suite needs several published versions to range over, got {rows:?}"
    );

    for (index, (engine, version)) in rows.iter().enumerate() {
        let case = format!("{engine} {version}");
        let name = format!("offered-{index}");
        let created = create_cache(&emulator, &name, engine, version).await;
        assert_eq!(
            created.status, 200,
            "case {case}: a version the catalog offers must be served, got {} {}",
            created.status, created.body
        );
        assert_eq!(
            created.text("MajorEngineVersion"),
            Some(version.as_str()),
            "case {case}: the create must report the version it was asked for, got {:?}",
            created.text("MajorEngineVersion")
        );
        // The round trip: what the cache runs is read back from the stored record.
        let described = describe_cache(&emulator, &name).await;
        assert_eq!(
            described.text("MajorEngineVersion"),
            Some(version.as_str()),
            "case {case}: the stored cache must report that version, got {:?} ({})",
            described.text("MajorEngineVersion"),
            described.body
        );
        assert_eq!(
            described.text("Engine"),
            Some(engine.as_str()),
            "case {case}: the stored cache must report that engine, got {:?}",
            described.text("Engine")
        );
    }
}

/// The versions NO engine of the catalog offers, as `(engine, version)`.
///
/// The axis: how far past the catalog the version is — one major line above what the engine
/// publishes, one below it, a minor line the engine never had, and a version far beyond
/// every engine. Each row is checked against the live catalog before it is sent.
const UNOFFERED: &[(&str, &str)] = &[
    ("redis", "999.999"),
    ("redis", "8"),
    ("redis", "3"),
    ("redis", "6.9"),
    ("valkey", "6"),
    ("valkey", "9.9"),
    ("memcached", "2"),
    ("memcached", "1.9"),
];

#[tokio::test]
async fn a_major_engine_version_the_catalog_does_not_offer_is_refused_as_an_incompatible_pair() {
    let emulator = Emulator::new();
    let catalog = published_catalog(&emulator).await;

    for (index, (engine, version)) in UNOFFERED.iter().enumerate() {
        let case = format!("{engine} {version}");
        assert!(
            !catalog_offers(&catalog, engine, version),
            "case {case}: this row needs a version the published catalog does NOT offer, and \
             the catalog reports {catalog:?}"
        );
        let name = format!("unoffered-{index}");
        let refused = create_cache(&emulator, &name, engine, version).await;
        assert_refusal(
            &case,
            &refused,
            400,
            "InvalidParameterCombination",
            &unsupported(version),
        );
        // A refused create stores nothing, so the name resolves to no cache at all.
        let described = describe_cache(&emulator, &name).await;
        assert_eq!(
            (described.status, described.error_code()),
            (404, "ServerlessCacheNotFoundFault"),
            "case {case}: a refused create must leave no cache behind, got {} {}",
            described.status,
            described.body
        );
    }
}

#[tokio::test]
async fn a_modify_takes_a_version_the_catalog_offers_and_refuses_one_it_does_not() {
    let emulator = Emulator::new();
    let catalog = published_catalog(&emulator).await;
    let name = "modified-version-cache";
    let created = create_cache(&emulator, name, "redis", "7").await;
    assert_eq!(
        created.status, 200,
        "the baseline create must answer 200, got {} {}",
        created.status, created.body
    );
    // A cache is reconfigured from its settled status, so the create is followed by looking
    // again until the clock has settled it.
    poll_until(
        &emulator,
        REGION,
        "the baseline cache",
        "the serverless cache reporting available",
        "DescribeServerlessCaches",
        &params(&[("ServerlessCacheName", name)]),
        |reply| reply.text("Status") == Some(state::SERVERLESS_STATUS_AVAILABLE),
    )
    .await;

    // The same rule at the other operation that takes the member: a version the catalog
    // publishes nothing for is the incompatible pair, and the live cache keeps the version
    // it is running.
    for (engine, version) in UNOFFERED {
        let case = format!("modify to {engine} {version}");
        assert!(
            !catalog_offers(&catalog, engine, version),
            "case {case}: this row needs a version the published catalog does NOT offer"
        );
        let refused = emulator
            .call(
                "ModifyServerlessCache",
                &params(&[
                    ("ServerlessCacheName", name),
                    ("Engine", engine),
                    ("MajorEngineVersion", version),
                ]),
            )
            .await;
        assert_refusal(
            &case,
            &refused,
            400,
            "InvalidParameterCombination",
            &unsupported(version),
        );
        let described = describe_cache(&emulator, name).await;
        assert_eq!(
            described.text("MajorEngineVersion"),
            Some("7"),
            "case {case}: a refused modify must leave the cache on its own version, got {:?}",
            described.text("MajorEngineVersion")
        );
    }

    // And a line the catalog DOES offer for the engine the cache moves onto is applied.
    let moved = emulator
        .call(
            "ModifyServerlessCache",
            &params(&[
                ("ServerlessCacheName", name),
                ("Engine", "valkey"),
                ("MajorEngineVersion", "8"),
            ]),
        )
        .await;
    assert_eq!(
        moved.status, 200,
        "a version the catalog offers must be applied, got {} {}",
        moved.status, moved.body
    );
    assert!(
        catalog_offers(&catalog, "valkey", "8"),
        "this row needs the catalog to offer valkey 8, and it reports {catalog:?}"
    );
    let described = describe_cache(&emulator, name).await;
    for (member, expected) in [("Engine", "valkey"), ("MajorEngineVersion", "8")] {
        assert_eq!(
            described.text(member),
            Some(expected),
            "the stored cache must report {member} {expected:?}, got {:?} ({})",
            described.text(member),
            described.body
        );
    }
}

#[tokio::test]
async fn a_value_that_names_no_version_at_all_stays_a_bad_parameter_value() {
    let emulator = Emulator::new();
    let name = "typed-version-cache";
    let created = create_cache(&emulator, name, "redis", "7").await;
    assert_eq!(
        created.status, 200,
        "the baseline create must answer 200, got {} {}",
        created.status, created.body
    );

    // The counterpart of the rule above: a value that is no version cannot be an engine's
    // UNSUPPORTED version, so it keeps the bad-value refusal and its own sentence. The axis
    // is the way the value misses the form — a word, a version line with a wildcard
    // component, a negative number, and a dotted value with an empty component.
    for (index, value) in ["seven", "7.x", "-1", "7."].iter().enumerate() {
        let case = format!("MajorEngineVersion {value:?}");
        let refused = create_cache(&emulator, &format!("malformed-{index}"), "redis", value).await;
        assert_eq!(
            (refused.status, refused.error_code()),
            (400, "InvalidParameterValue"),
            "case {case}: expected 400 InvalidParameterValue, got {} {}",
            refused.status,
            refused.body
        );
        assert!(
            refused.error_message().contains("MajorEngineVersion")
                && refused.error_message().contains(value),
            "case {case}: the refusal must name the member and the value it got, got {:?}",
            refused.error_message()
        );

        let modified = emulator
            .call(
                "ModifyServerlessCache",
                &params(&[
                    ("ServerlessCacheName", name),
                    ("MajorEngineVersion", value),
                ]),
            )
            .await;
        assert_eq!(
            (modified.status, modified.error_code()),
            (400, "InvalidParameterValue"),
            "case {case} through ModifyServerlessCache: expected 400 InvalidParameterValue, \
             got {} {}",
            modified.status,
            modified.body
        );
    }
}
