// SPDX-License-Identifier: CC0-1.0

//! The instance classes the shared `rds` front door publishes for an engine ANOTHER control
//! plane owns: that control plane's own class list, whichever view of the door answers.
//!
//! The rule under test: `DescribeOrderableDBInstanceOptions` reports, for an engine of a
//! control plane that publishes its own instance-class table, the classes THAT table names —
//! crossed with the storage types the engine is ordered with — and no class of a generation
//! the engine is not offered in. A door serves several control planes, and each of them
//! publishes its own table: the Amazon RDS view and the Amazon DocumentDB view of this one
//! door therefore answer the same classes for the document engine (they read the one catalog
//! in `service_common::docdb_engine_catalog`), while that engine's list and the door's own
//! Aurora list stay different lists.
//!
//! An SDK DocumentDB client signs `rds`, so its catalog reads land on the Amazon RDS view;
//! a request addressed at the DocumentDB view by its own routing key lands on that one. The
//! rows below drive BOTH, and they span what the rule generalises over: the engine whose
//! control plane publishes the table (the document engine), the door's own cluster engine,
//! and the graph engine of the third control plane — each asserted on a class the other two
//! do not publish, including `db.r5.24xlarge` (a memory-optimised class the document engine
//! is offered in and the door's own Aurora list stops short of) and `db.serverless` (a
//! capacity class the document engine publishes no serverless capacity for).
//!
//! Assembling the router costs stack (see `boot_serve.rs`), so it is built — and the requests
//! driven — on an explicitly sized thread.

use std::sync::Arc;

use aws_emulator_router::{build_router, InMemoryEngine};

/// Keep this at or above `boot_serve.rs`'s stack: the same assembly runs here.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The awsQuery API version every view of this front door shares.
const API_VERSION: &str = "2014-10-31";

/// The region every request below is signed for.
const REGION: &str = "us-west-2";

/// The engine of the control plane whose own instance-class table this suite is about, as its
/// `Engine` member publishes it ("Valid values: docdb").
const DOCUMENT_ENGINE: &str = "docdb";

/// The serverless capacity class, which a cluster member of an engine whose control plane
/// offers serverless capacity is ordered with — and which the document engine's own table
/// names nowhere.
const SERVERLESS_CLASS: &str = "db.serverless";

/// The classes the document engine's own instance-class table publishes, which is the one
/// table both views of this door read for it.
///
/// Source: the "Amazon DocumentDB instance classes" section of the Amazon DocumentDB
/// Developer Guide, which the emulator holds in
/// `service_common::docdb_engine_catalog::INSTANCE_FAMILIES` — the two burstable `medium`
/// classes, the memory-optimised `r5` line up to `24xlarge` and the Graviton `r6g` and `r7g`
/// lines up to `16xlarge`. They are written out here rather than read back from that table,
/// so a change to it has to be made here too.
const DOCUMENT_CLASSES: &[&str] = &[
    "db.t3.medium",
    "db.t4g.medium",
    "db.r5.large",
    "db.r5.xlarge",
    "db.r5.2xlarge",
    "db.r5.4xlarge",
    "db.r5.8xlarge",
    "db.r5.12xlarge",
    "db.r5.16xlarge",
    "db.r5.24xlarge",
    "db.r6g.large",
    "db.r6g.xlarge",
    "db.r6g.2xlarge",
    "db.r6g.4xlarge",
    "db.r6g.8xlarge",
    "db.r6g.12xlarge",
    "db.r6g.16xlarge",
    "db.r7g.large",
    "db.r7g.xlarge",
    "db.r7g.2xlarge",
    "db.r7g.4xlarge",
    "db.r7g.8xlarge",
    "db.r7g.12xlarge",
    "db.r7g.16xlarge",
];

/// The storage types an option of that engine is offered with, in the order its own
/// `CreateDBCluster$StorageType` publishes them ("standard | iopt1"): a class is offered once
/// per type, because the type is part of what an order picks.
const DOCUMENT_STORAGE_TYPES: [&str; 2] = ["standard", "iopt1"];

/// The release of that engine the rows below read the options of: the version a cluster
/// created without an `EngineVersion` runs, which its catalog designates as the engine's
/// default.
const DOCUMENT_DEFAULT_VERSION: &str = "5.0.0";

/// One engine the door serves, and the classes its report must and must not name.
struct Row {
    /// The `Engine` the report is read for.
    engine: &'static str,
    /// A class the engine's own published table names.
    offered: &'static str,
    /// A class of a generation this engine is NOT offered in, which another engine of the same
    /// door IS: a report that named it would tell a caller to order what the engine cannot be.
    not_offered: &'static str,
}

/// The engines under test. `db.r6i.*` is in the door's own Aurora list and in the graph
/// engine's; `db.r7g.*` is in the document engine's table and the graph engine's.
const ROWS: &[Row] = &[
    Row {
        engine: DOCUMENT_ENGINE,
        offered: "db.r5.24xlarge",
        not_offered: "db.r6i.large",
    },
    Row {
        engine: "aurora-postgresql",
        offered: "db.r6i.large",
        not_offered: "db.r5.24xlarge",
    },
    Row {
        engine: "neptune",
        offered: "db.r5.24xlarge",
        not_offered: "db.t3.large",
    },
];

/// POST one awsQuery action to the router under `signing_name`'s routing key and hand back
/// the body.
fn call(
    router: &smithy_server_runtime::MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    signing_name: &str,
    action: &str,
    params: &[(&str, &str)],
) -> String {
    let mut body = format!("Action={action}&Version={API_VERSION}");
    for (name, value) in params {
        body.push_str(&format!("&{name}={value}"));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("http://localhost/")
        .header("content-type", "application/x-www-form-urlencoded")
        .header("host", "localhost")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 \
                 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
                 SignedHeaders=host, Signature=not-verified"
            ),
        )
        .body(bytes::Bytes::from(body))
        .expect("a well-formed request");
    let response = runtime.block_on(router.dispatch(request));
    let status = response.status().as_u16();
    let body = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "case {signing_name:?} {action}: a catalog read is answered 200, got {status} with \
         {body}"
    );
    body
}

/// Every text carried by `<tag>` elements of `xml`, in document order.
fn texts(xml: &str, tag: &str) -> Vec<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let mut found = Vec::new();
    let mut rest = xml;
    while let Some(start) = rest.find(&open) {
        let after = &rest[start + open.len()..];
        let Some(end) = after.find(&close) else { break };
        found.push(after[..end].to_string());
        rest = &after[end + close.len()..];
    }
    found
}

/// The classes an option report NAMES, without repeats and in the order it reports them.
fn reported_classes(xml: &str) -> Vec<String> {
    let mut classes: Vec<String> = Vec::new();
    for class in texts(xml, "DBInstanceClass") {
        if !classes.contains(&class) {
            classes.push(class);
        }
    }
    classes
}

#[test]
fn the_document_engines_own_instance_classes_are_published_by_every_view() {
    std::thread::Builder::new()
        .name("rds-document-classes".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
            let mut router = build_router(engine);
            router.set_request_authentication(false);
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("a current-thread runtime");

            // The classes the document engine's own catalog publishes, which is the ONE
            // table both views of this door read ([`DOCUMENT_CLASSES`]).
            let published: Vec<String> = DOCUMENT_CLASSES
                .iter()
                .map(|class| (*class).to_string())
                .collect();
            let version = DOCUMENT_DEFAULT_VERSION;
            let storage_types = DOCUMENT_STORAGE_TYPES;

            // Both views of the door answer the same class list for that engine, and it is
            // the catalog's own: a client that enumerates options to pick a class reads the
            // same answer however it addressed the door.
            for key in ["rds", DOCUMENT_ENGINE] {
                let body = call(
                    &router,
                    &runtime,
                    key,
                    "DescribeOrderableDBInstanceOptions",
                    &[
                        ("Engine", DOCUMENT_ENGINE),
                        ("EngineVersion", version),
                        ("MaxRecords", "100"),
                    ],
                );
                let mut reported = reported_classes(&body);
                reported.sort_unstable();
                let mut expected = published.clone();
                expected.sort_unstable();
                assert_eq!(
                    reported, expected,
                    "case {key:?}: every view publishes the engine's own class list, got \
                     {reported:?}"
                );
                // Each class is offered once per storage type the engine is ordered with,
                // so the option count is the whole matrix of one release.
                let options = texts(&body, "DBInstanceClass").len();
                assert_eq!(
                    options,
                    published.len() * storage_types.len(),
                    "case {key:?}: expected {} class(es) crossed with {} storage type(s), got \
                     {options}",
                    published.len(),
                    storage_types.len()
                );
                // The storage types reported for ONE class are the engine's own, in the order
                // its own value list publishes them.
                let one = call(
                    &router,
                    &runtime,
                    key,
                    "DescribeOrderableDBInstanceOptions",
                    &[
                        ("Engine", DOCUMENT_ENGINE),
                        ("EngineVersion", version),
                        ("DBInstanceClass", "db.r5.24xlarge"),
                    ],
                );
                let reported_types = texts(&one, "StorageType");
                assert_eq!(
                    reported_types,
                    storage_types
                        .iter()
                        .map(|storage_type| (*storage_type).to_string())
                        .collect::<Vec<_>>(),
                    "case {key:?}: a class is offered once per published storage type, got \
                     {reported_types:?}"
                );
                // The serverless capacity class is not one of this engine's: its control
                // plane offers no serverless capacity for a cluster member.
                let serverless = call(
                    &router,
                    &runtime,
                    key,
                    "DescribeOrderableDBInstanceOptions",
                    &[
                        ("Engine", DOCUMENT_ENGINE),
                        ("DBInstanceClass", SERVERLESS_CLASS),
                    ],
                );
                assert!(
                    texts(&serverless, "DBInstanceClass").is_empty(),
                    "case {key:?}: the engine publishes no serverless capacity class, got \
                     {serverless}"
                );
            }

            // Each control plane the door serves publishes its OWN table, so a class one
            // engine is offered in is not reported for an engine that is not.
            for row in ROWS {
                for (class, expected) in [(row.offered, 1_usize), (row.not_offered, 0)] {
                    let body = call(
                        &router,
                        &runtime,
                        "rds",
                        "DescribeOrderableDBInstanceOptions",
                        &[
                            ("Engine", row.engine),
                            ("DBInstanceClass", class),
                            ("MaxRecords", "100"),
                        ],
                    );
                    let named = reported_classes(&body);
                    assert_eq!(
                        named.len(),
                        expected,
                        "case {:?} {class:?}: expected {} class(es) named, got {named:?}",
                        row.engine,
                        expected
                    );
                }
            }
        })
        .expect("the catalog thread starts")
        .join()
        .expect("the catalog thread finishes");
}
