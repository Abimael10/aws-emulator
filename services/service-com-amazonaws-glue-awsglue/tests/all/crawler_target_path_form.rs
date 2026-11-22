// SPDX-License-Identifier: CC0-1.0

//! What a crawler's TABLE-FORMAT targets record of the Amazon S3 paths they name:
//! `DeltaTarget$DeltaTables`, `IcebergTarget$Paths` and `HudiTarget$Paths`.
//!
//! Every call goes over the wire through the shared [`crate::wire`] harness, so what is
//! asserted is the HTTP behaviour and not a direct call into a handler.
//!
//! RULE under test: every target member documented as an Amazon S3 path — "a list of the
//! Amazon S3 paths to the Delta tables" (`DeltaTarget$DeltaTables`), "one or more Amazon S3
//! paths that contains Iceberg metadata folders as `s3://bucket/prefix`"
//! (`IcebergTarget$Paths`), "an array of Amazon S3 location strings" (`HudiTarget$Paths`) —
//! names data held by ANOTHER service rather than a resource the write brings into being, and
//! neither `CreateCrawler` nor `UpdateCrawler` models a not-found error for one. So the value
//! is stored exactly as the request wrote it and read back unchanged, and nothing is resolved
//! against the object store: a crawler is routinely registered before the bucket or the data
//! behind its target exists. It is the rule [`crate::crawler_s3_target_resolution`] pins for
//! `S3Target$Path`, read over the three members that carry a LIST of paths instead of one.
//!
//! The rows differ along what the rule generalises over: WHICH member carries the paths, HOW
//! MANY of them one target names, and the FORM of each value — a prefix under a bucket, a
//! bucket on its own, and the `s3a` and `s3n` spellings a crawl also resolves — read through
//! both writes that take `Targets`.

use serde_json::{json, Value};

use crate::wire::ok;

/// The role every crawler below runs as. No IAM is mounted in this suite, so nothing can be
/// shown about the role and it refuses no write — which leaves the TARGETS as the one thing
/// each row varies.
const ROLE: &str = "arn:aws:iam::000000000000:role/sim2real-crawler-role";

/// One target member the rule covers: the list it carries its paths in, and the target list
/// it hangs off.
struct Member {
    /// What every assertion of the row reports.
    case: &'static str,
    /// The `CrawlerTargets` member holding the targets of this kind.
    targets: &'static str,
    /// The member of one such target holding its Amazon S3 paths.
    paths: &'static str,
}

const MEMBERS: [Member; 3] = [
    Member {
        case: "a Delta target",
        targets: "DeltaTargets",
        paths: "DeltaTables",
    },
    Member {
        case: "an Iceberg target",
        targets: "IcebergTargets",
        paths: "Paths",
    },
    Member {
        case: "a Hudi target",
        targets: "HudiTargets",
        paths: "Paths",
    },
];

/// The path sets a row sends, each with the case it stands for: the forms a path can take,
/// and how many of them one target names.
const PATH_SETS: [(&str, &[&str]); 4] = [
    ("one prefix under a bucket", &["s3://crawler-lake/events/"]),
    ("a bucket on its own", &["s3://crawler-lake"]),
    (
        "three prefixes under one bucket",
        &[
            "s3://crawler-lake/events/",
            "s3://crawler-lake/orders/",
            "s3://crawler-lake/returns/",
        ],
    ),
    (
        "the s3a and s3n spellings beside the s3 one",
        &[
            "s3://crawler-lake/events/",
            "s3a://crawler-lake/events/",
            "s3n://crawler-lake/events/",
        ],
    ),
];

/// The `CrawlerTargets` one row sends.
fn targets_of(member: &Member, paths: &[&str]) -> Value {
    let mut target = serde_json::Map::new();
    target.insert(member.paths.to_string(), json!(paths));
    let mut targets = serde_json::Map::new();
    targets.insert(
        member.targets.to_string(),
        Value::Array(vec![Value::Object(target)]),
    );
    Value::Object(targets)
}

#[tokio::test]
async fn a_table_format_target_is_stored_with_every_path_it_names() {
    let router = crate::wire::emulator();

    for member in MEMBERS {
        for (index, (why, paths)) in PATH_SETS.iter().enumerate() {
            let case = format!("{}: {why}", member.case);
            let name = format!("{}-{index}", member.targets.to_lowercase());
            let sent = targets_of(&member, paths);
            ok(
                &router,
                "CreateCrawler",
                &json!({ "Name": name, "Role": ROLE, "Targets": sent.clone() }),
            )
            .await;

            let read = ok(&router, "GetCrawler", &json!({ "Name": name })).await;
            assert_eq!(
                read["Crawler"]["Targets"][member.targets][0][member.paths],
                json!(paths),
                "case {case:?}: the stored target must carry every path as written, got {read}"
            );
            assert_eq!(
                read["Crawler"]["State"],
                json!("READY"),
                "case {case:?}: a created crawler is READY, got {read}"
            );

            // And an UPDATE of the same crawler replaces that list with the one it carries,
            // leaving the paths exactly as the update wrote them.
            let replaced = targets_of(&member, &["s3://crawler-lake/replaced/"]);
            ok(
                &router,
                "UpdateCrawler",
                &json!({ "Name": name, "Targets": replaced }),
            )
            .await;
            let after = ok(&router, "GetCrawler", &json!({ "Name": name })).await;
            assert_eq!(
                after["Crawler"]["Targets"][member.targets][0][member.paths],
                json!(["s3://crawler-lake/replaced/"]),
                "case {case:?}: the update's own paths must be the stored ones, got {after}"
            );
        }
    }
}
