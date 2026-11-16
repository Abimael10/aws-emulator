// SPDX-License-Identifier: CC0-1.0

//! The AWS-published hierarchies as a client DISCOVERS and then READS them: the hierarchy
//! read enumerates them, the batch read resolves what it enumerated.
//!
//! RULE under test: a read inside the hierarchy AWS publishes its own parameters in resolves
//! against the published catalog rather than against the account's store, so a hierarchy read
//! of a path that catalog publishes names at or under reports one `Parameter` per such name —
//! each a `String` carrying `text`, a version, a last-modified instant and the ARN of a
//! parameter that belongs to no account — and a batch read reports every name that resolves
//! among its `Parameters`, listing under `InvalidParameters` only the names NOTHING resolves,
//! which it reports rather than refuses.
//!
//! That is the discovery sequence every client that cannot hardcode a per-Region AMI id
//! follows: walk a namespace, then read the names it handed back. The two reads are checked
//! against each other here — the names the hierarchy read enumerates are exactly the names
//! the batch read resolves — so neither can be right about a namespace the other is wrong
//! about.
//!
//! Every case goes through the wire a client uses: `POST /` with
//! `X-Amz-Target: AmazonSSM.<Operation>` and an `awsJson1_1` body (`OPERATIONS.md`, "Wire
//! protocol"). Nothing reaches into a state struct.
//!
//! The tables below span the published namespaces this endpoint serves rather than one of
//! them: the AMI alias namespaces of two operating-system families, the metadata hierarchies
//! two services publish a recommended image through (one of them an accelerator build), the
//! namespace a third publishes its recommended node image in, and — for the batch split —
//! every FORM an unresolvable name takes. WHICH names a namespace publishes is
//! service-published data no model describes, so the names are discovered through the
//! hierarchy read itself rather than written out here.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::parameter_store::{call, ACCOUNT, REGION};
use super::public_parameter_catalog::{
    emulator, listing, names, recursive_listing, PUBLISHED_DATA_TYPE, PUBLISHED_TYPE,
};

/// How many names ONE batch read carries (`smithy_query(query="shape",
/// shape="ParameterNameList")`: length 1..10).
const MAX_BATCH_NAMES: usize = 10;

/// One published hierarchy a client discovers names through.
struct Hierarchy {
    /// What it is, for a case label.
    label: &'static str,
    /// The path a client reads to discover the names published at or under it.
    path: &'static str,
}

/// The published hierarchies the suites below span: the AMI alias namespaces of the Linux and
/// the Windows families, the metadata hierarchy of a recommended container-host build of each
/// Amazon Linux family (including an accelerator build), the recommended build of a
/// third-party container operating system, and the namespace AWS PCS publishes its
/// recommended node image in.
const PUBLISHED_HIERARCHIES: &[Hierarchy] = &[
    Hierarchy {
        label: "the Amazon Linux AMI alias namespace",
        path: "/aws/service/ami-amazon-linux-latest",
    },
    Hierarchy {
        label: "the Windows AMI alias namespace",
        path: "/aws/service/ami-windows-latest",
    },
    Hierarchy {
        label: "the recommended ECS-optimized Amazon Linux 2023 build",
        path: "/aws/service/ecs/optimized-ami/amazon-linux-2023/recommended",
    },
    Hierarchy {
        label: "the recommended ECS-optimized Amazon Linux 2 GPU build",
        path: "/aws/service/ecs/optimized-ami/amazon-linux-2/gpu/recommended",
    },
    Hierarchy {
        label: "the recommended Bottlerocket build for 64-bit Arm",
        path: "/aws/service/bottlerocket/aws-ecs-2/arm64/latest",
    },
    Hierarchy {
        label: "the AWS PCS recommended node image namespace",
        path: "/aws/service/pcs",
    },
];

/// The paths whose hierarchy has LEVELS below it, so the one-level read and the recursive read
/// of it report different populations.
const NESTED_HIERARCHIES: &[Hierarchy] = &[
    Hierarchy {
        label: "the ECS-optimized Amazon Linux 2023 family",
        path: "/aws/service/ecs/optimized-ami/amazon-linux-2023",
    },
    Hierarchy {
        label: "the Bottlerocket builds for 64-bit x86",
        path: "/aws/service/bottlerocket/aws-ecs-2/x86_64",
    },
];

/// The ARN a published parameter carries: no account at all, because AWS publishes it in every
/// account alike and no caller can write it.
fn published_arn(name: &str) -> String {
    format!("arn:aws:ssm:{REGION}::parameter{name}")
}

/// Every name a recursive read of `path` enumerates, asserted to be a population and not an
/// empty page.
async fn published_under(router: &MultiServiceRouter, case: &str, path: &str) -> Vec<String> {
    let enumerated = names(&recursive_listing(router, case, path).await);
    assert!(
        !enumerated.is_empty(),
        "case {case:?}: expected the hierarchy read of {path} to enumerate the names published \
         under it, got an empty page"
    );
    enumerated
}

/// One batch read, asserted to have been served: `(Parameters, InvalidParameters)`.
async fn batch(router: &MultiServiceRouter, case: &str, asked: &[String]) -> (Vec<Value>, Vec<String>) {
    assert!(
        !asked.is_empty() && asked.len() <= MAX_BATCH_NAMES,
        "case {case:?}: a batch read carries between 1 and {MAX_BATCH_NAMES} names, got {}",
        asked.len()
    );
    let (status, body) = call(router, "GetParameters", &json!({ "Names": asked })).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from GetParameters for {asked:?}, got {status} with \
         {body}"
    );
    let parameters = body["Parameters"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: GetParameters reports a Parameters list: {body}"))
        .clone();
    let invalid: Vec<String> = body["InvalidParameters"]
        .as_array()
        .unwrap_or_else(|| {
            panic!("case {case:?}: GetParameters reports an InvalidParameters list: {body}")
        })
        .iter()
        .map(|name| name.as_str().unwrap_or_default().to_string())
        .collect();
    (parameters, invalid)
}

/// The same names, sorted — the order a batch read reports its parameters in ("the results for
/// GetParameters requests are listed in alphabetical order in query responses",
/// `GetParametersRequest$Names`).
fn sorted(names: &[String]) -> Vec<String> {
    let mut sorted = names.to_vec();
    sorted.sort();
    sorted
}

/// Assert `parameter` is shaped the way AWS publishes a parameter of its own.
fn assert_published_shape(case: &str, parameter: &Value) {
    let name = parameter["Name"].as_str().unwrap_or_default().to_string();
    assert_eq!(
        parameter["Type"].as_str(),
        Some(PUBLISHED_TYPE),
        "case {case:?}: expected Type {PUBLISHED_TYPE}, got {parameter}"
    );
    assert_eq!(
        parameter["DataType"].as_str(),
        Some(PUBLISHED_DATA_TYPE),
        "case {case:?}: expected DataType {PUBLISHED_DATA_TYPE}, got {parameter}"
    );
    assert!(
        parameter["Version"].as_i64().unwrap_or_default() >= 1,
        "case {case:?}: expected a server-assigned version of at least 1, got {parameter}"
    );
    assert!(
        !parameter["LastModifiedDate"].is_null(),
        "case {case:?}: expected a last-modified instant, got {parameter}"
    );
    assert_eq!(
        parameter["ARN"].as_str(),
        Some(published_arn(&name).as_str()),
        "case {case:?}: expected the account-less published-parameter ARN, got {parameter}"
    );
    assert!(
        !parameter["Value"].as_str().unwrap_or_default().is_empty(),
        "case {case:?}: expected the published parameter to carry a value, got {parameter}"
    );
}

/// RULE: a hierarchy read of a path the published catalog publishes names at or under reports
/// one parameter per such name — never an empty page — each under its own full public path and
/// shaped as a parameter AWS publishes.
#[tokio::test]
async fn every_published_hierarchy_enumerates_the_names_published_under_it() {
    let router = emulator();

    for Hierarchy { label, path } in PUBLISHED_HIERARCHIES {
        let case = format!("{label} at {path}");
        let reported = recursive_listing(&router, &case, path).await;
        assert!(
            !reported.is_empty(),
            "case {case:?}: expected the hierarchy to enumerate its published names, got an \
             empty page"
        );
        for parameter in &reported {
            let name = parameter["Name"].as_str().unwrap_or_default().to_string();
            let case = format!("{case}, entry {name}");
            assert!(
                name == *path || name.starts_with(&format!("{path}/")),
                "case {case:?}: expected every entry to carry its full public path under \
                 {path}, got {name:?}"
            );
            assert_published_shape(&case, parameter);
        }
        let enumerated = names(&reported);
        let mut once = enumerated.clone();
        once.dedup();
        assert_eq!(
            once, enumerated,
            "case {case:?}: expected each published name exactly once, got {enumerated:?}"
        );
        assert_eq!(
            sorted(&enumerated),
            enumerated,
            "case {case:?}: expected ascending name order, got {enumerated:?}"
        );
    }
}

/// RULE: the batch read resolves what the hierarchy read enumerated — every enumerated name
/// lands in `Parameters`, shaped as a published parameter, and only the name nothing publishes
/// lands in `InvalidParameters`.
#[tokio::test]
async fn the_batch_read_resolves_every_name_the_hierarchy_enumerated() {
    let router = emulator();

    for Hierarchy { label, path } in PUBLISHED_HIERARCHIES {
        let case = format!("{label} at {path}");
        let enumerated = published_under(&router, &case, path).await;
        // One name of the same namespace that nothing publishes, carried by every batch: it is
        // the contrast the split is about, so each batch has a resolvable and an unresolvable
        // name in it.
        let unpublished = format!("{path}/no-such-published-leaf");
        for chunk in enumerated.chunks(MAX_BATCH_NAMES - 1) {
            let mut asked = chunk.to_vec();
            asked.push(unpublished.clone());
            let case = format!("{case}, batch {asked:?}");
            let (parameters, invalid) = batch(&router, &case, &asked).await;
            assert_eq!(
                names(&parameters),
                sorted(chunk),
                "case {case:?}: expected every enumerated name among the resolved parameters, \
                 got {parameters:?}"
            );
            assert_eq!(
                invalid,
                vec![unpublished.clone()],
                "case {case:?}: expected only the name nothing publishes to be reported \
                 invalid, got {invalid:?}"
            );
            for parameter in &parameters {
                let name = parameter["Name"].as_str().unwrap_or_default().to_string();
                assert_published_shape(&format!("{case}, parameter {name}"), parameter);
            }
        }
    }
}

/// RULE: a name the batch read cannot resolve is REPORTED, not refused — whatever form it
/// takes — and it is reported beside, not instead of, the names of the same call that do
/// resolve.
#[tokio::test]
async fn an_unresolvable_name_is_reported_rather_than_refusing_the_batch() {
    let router = emulator();
    let namespace = PUBLISHED_HIERARCHIES[0].path;
    let resolvable = published_under(&router, "a name to resolve beside the bad ones", namespace)
        .await
        .first()
        .cloned()
        .expect("a published namespace enumerates at least one name");

    // Every form a name that resolves nothing takes.
    let unresolvable: &[(&str, String)] = &[
        (
            "a leaf a served namespace publishes nothing under",
            format!("{namespace}/no-such-published-leaf"),
        ),
        (
            "a path of the published hierarchy nothing publishes in",
            "/aws/service/no-such-published-namespace/leaf".to_string(),
        ),
        (
            "a level of the published hierarchy that is no namespace of it",
            "/aws/no-such-published-level".to_string(),
        ),
        (
            "a name of the account's own hierarchy nothing wrote",
            "/mine/never-written".to_string(),
        ),
        (
            "an ARN of another account's parameter",
            format!("arn:aws:ssm:{REGION}:111122223333:parameter/mine/elsewhere"),
        ),
    ];

    for (label, name) in unresolvable {
        let case = format!("{label}: {name}");
        let asked = vec![resolvable.clone(), name.clone()];
        let (parameters, invalid) = batch(&router, &case, &asked).await;
        assert_eq!(
            names(&parameters),
            vec![resolvable.clone()],
            "case {case:?}: expected the resolvable name to be served, got {parameters:?}"
        );
        assert_eq!(
            invalid,
            vec![name.clone()],
            "case {case:?}: expected only this name to be reported invalid, got {invalid:?}"
        );
    }
}

/// RULE: the two sources are resolved beside each other — one batch read carrying the
/// account's own parameters, AWS's published ones and names nothing holds reports each from the
/// source that holds it, with the ARN that source's parameters carry, and reports only the rest
/// as invalid.
#[tokio::test]
async fn one_batch_resolves_the_accounts_parameters_beside_the_published_ones() {
    let router = emulator();
    let mine = [("/mine/first", "one"), ("/mine/second", "two")];
    for (name, value) in mine {
        let (status, body) = call(
            &router,
            "PutParameter",
            &json!({ "Name": name, "Value": value, "Type": "String" }),
        )
        .await;
        assert_eq!(
            status, 200,
            "expected PutParameter {name} to be served, got {status} with {body}"
        );
    }

    let published: Vec<String> = {
        let mut collected = Vec::new();
        for Hierarchy { label, path } in PUBLISHED_HIERARCHIES.iter().take(3) {
            let case = format!("{label} at {path}");
            let enumerated = published_under(&router, &case, path).await;
            collected.push(
                enumerated
                    .first()
                    .cloned()
                    .expect("a published namespace enumerates at least one name"),
            );
        }
        collected
    };

    let missing = [
        "/mine/third".to_string(),
        format!("{}/no-such-published-leaf", PUBLISHED_HIERARCHIES[0].path),
    ];
    let case = "the account's parameters beside AWS's published ones";
    let asked: Vec<String> = mine
        .iter()
        .map(|(name, _)| (*name).to_string())
        .chain(published.iter().cloned())
        .chain(missing.iter().cloned())
        .collect();
    let (parameters, invalid) = batch(&router, case, &asked).await;

    let mut resolved: Vec<String> = mine.iter().map(|(name, _)| (*name).to_string()).collect();
    resolved.extend(published.iter().cloned());
    assert_eq!(
        names(&parameters),
        sorted(&resolved),
        "case {case:?}: expected both sources to resolve in one call, got {parameters:?}"
    );
    assert_eq!(
        sorted(&invalid),
        sorted(&missing),
        "case {case:?}: expected only the names neither source holds to be invalid, got \
         {invalid:?}"
    );
    for parameter in &parameters {
        let name = parameter["Name"].as_str().unwrap_or_default().to_string();
        let case = format!("{case}, parameter {name}");
        if published.contains(&name) {
            assert_published_shape(&case, parameter);
        } else {
            assert_eq!(
                parameter["ARN"].as_str(),
                Some(format!("arn:aws:ssm:{REGION}:{ACCOUNT}:parameter{name}").as_str()),
                "case {case:?}: expected the account's own parameter ARN, got {parameter}"
            );
        }
    }
}

/// RULE: `Recursive` decides how deep the hierarchy read reaches — the one-level read reports
/// the names immediately below the path, the recursive read every name under it however deep,
/// and the first is part of the second.
#[tokio::test]
async fn the_recursive_read_reaches_the_levels_the_one_level_read_does_not() {
    let router = emulator();

    for Hierarchy { label, path } in NESTED_HIERARCHIES {
        let case = format!("{label} at {path}");
        let deep = published_under(&router, &case, path).await;
        let shallow = names(&listing(&router, &case, path, false).await);
        let levels_below = |name: &str| match name.strip_prefix(&format!("{path}/")) {
            Some(rest) => rest.split('/').count(),
            None => 0,
        };
        for name in &shallow {
            assert!(
                deep.contains(name),
                "case {case:?}: expected the recursive read to report {name:?} too, got {deep:?}"
            );
            assert_eq!(
                levels_below(name),
                1,
                "case {case:?}: expected a one-level read to report only the level immediately \
                 below {path}, got {name:?}"
            );
        }
        assert!(
            deep.iter().any(|name| levels_below(name) > 1),
            "case {case:?}: expected the recursive read to reach deeper levels than \
             {shallow:?}, got {deep:?}"
        );
        assert!(
            deep.len() > shallow.len(),
            "case {case:?}: expected the recursive read to report more than the one-level \
             read's {shallow:?}, got {deep:?}"
        );
    }
}
