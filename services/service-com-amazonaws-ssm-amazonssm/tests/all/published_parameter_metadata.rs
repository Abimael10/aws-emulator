// SPDX-License-Identifier: CC0-1.0

//! The metadata read of AWS's own published parameters, end to end over the front door.
//!
//! RULE under test: `DescribeParameters` resolves the same two catalogues every other read of
//! Parameter Store resolves — the account's own parameters and the ones AWS publishes in the
//! reserved `/aws` hierarchy — so a published parameter that is readable by name is
//! describable too. Which catalogue a listing reports follows from what its filters ADDRESS:
//! one confined to AWS's hierarchy (a `Path` filter inside it, or a `Name` filter anchored at
//! a name in it) resolves the published catalogue beside the store, and one that is not
//! confined to it reports the account's own parameters alone, no account owning any of what
//! AWS publishes. A published entry carries the projection the whole service gives it: the
//! `String` type, the standard tier, `text` data, no policy, a version, and an ARN whose
//! ACCOUNT field is empty, because no account owns it.
//!
//! Every call goes through the wire a client uses: `POST /` with
//! `X-Amz-Target: AmazonSSM.<Operation>` and an `awsJson1_1` body (`OPERATIONS.md`, "Wire
//! protocol"). Nothing reaches into a state struct.
//!
//! The rows differ along what the rule generalises over: which published namespace is
//! addressed (the Amazon Linux AMI aliases the issue's client reads, the Windows ones, and
//! the Global Infrastructure reference data, a second catalogue in the same hierarchy), which
//! filter addresses it (`Path` recursively, `Path` one level, `Name` by equality, `Name` by
//! prefix), and which does NOT address it (`Name` by fragment, and no filter at all). What
//! the catalogue holds is DISCOVERED through the hierarchy read rather than written here, so
//! no row restates a published name.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::parameter_store::{call, ACCOUNT, REGION};
use crate::public_parameter_catalog::emulator;

/// The namespace the published Amazon Linux AMI aliases live in — the one the issue's client
/// asks for explicitly.
const AMAZON_LINUX_NAMESPACE: &str = "/aws/service/ami-amazon-linux-latest";

/// The published Windows AMI aliases, a second namespace of the same catalogue.
const WINDOWS_NAMESPACE: &str = "/aws/service/ami-windows-latest";

/// The Global Infrastructure reference data AWS publishes in every Region, which lives in the
/// same hierarchy and is published by something else entirely — so the rule is exercised on a
/// catalogue the issue did not cite.
const GLOBAL_INFRASTRUCTURE_REGIONS: &str = "/aws/service/global-infrastructure/regions";

/// The `ParameterType` a published alias carries, from the modelled enum
/// (`smithy_query(query="shape",shape="ParameterType")`).
const PUBLISHED_TYPE: &str = "String";

/// The tier it carries, from the modelled `ParameterTier` enum.
const PUBLISHED_TIER: &str = "Standard";

/// The data type it carries
/// (`smithy_query(query="doc",shape="PutParameterRequest",member="DataType")`).
const PUBLISHED_DATA_TYPE: &str = "text";

/// The published bound of `MaxResults` for this read
/// (`smithy_query(query="shape",shape="MaxResults")`: range 1..50).
const MAX_PAGE_SIZE: i64 = 50;

/// The published bound of `GetParametersByPathMaxResults` (range 1..10).
const MAX_HIERARCHY_PAGE_SIZE: i64 = 10;

/// Every parameter `DescribeParameters` reports under `filters`, following the continuation
/// token to the end, as `(Name, entry)` pairs in the order they were served.
async fn describe_all(
    router: &MultiServiceRouter,
    case: &str,
    filters: &Value,
) -> Vec<(String, Value)> {
    let mut reported = Vec::new();
    let mut next: Option<String> = None;
    loop {
        let mut body = json!({ "MaxResults": MAX_PAGE_SIZE });
        if let Value::Array(entries) = filters {
            if !entries.is_empty() {
                body["ParameterFilters"] = filters.clone();
            }
        }
        if let Some(token) = &next {
            body["NextToken"] = json!(token);
        }
        let (status, answered) = call(router, "DescribeParameters", &body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeParameters must answer, got {status} with {answered}"
        );
        for entry in answered["Parameters"].as_array().into_iter().flatten() {
            let name = entry["Name"]
                .as_str()
                .unwrap_or_else(|| {
                    panic!("case {case:?}: every described parameter names itself: {entry}")
                })
                .to_string();
            reported.push((name, entry.clone()));
        }
        match answered["NextToken"].as_str() {
            Some(token) if !token.is_empty() => next = Some(token.to_string()),
            _ => return reported,
        }
    }
}

/// Every parameter `GetParametersByPath` reads under `path`, following the continuation token
/// to the end, as the names it served — the catalogue as the read that already resolves it
/// reports it.
async fn read_all(router: &MultiServiceRouter, case: &str, path: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut next: Option<String> = None;
    loop {
        let mut body = json!({
            "Path": path,
            "Recursive": true,
            "MaxResults": MAX_HIERARCHY_PAGE_SIZE,
        });
        if let Some(token) = &next {
            body["NextToken"] = json!(token);
        }
        let (status, answered) = call(router, "GetParametersByPath", &body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetParametersByPath must answer, got {status} with {answered}"
        );
        for entry in answered["Parameters"].as_array().into_iter().flatten() {
            names.push(
                entry["Name"]
                    .as_str()
                    .unwrap_or_else(|| {
                        panic!("case {case:?}: every read parameter names itself: {entry}")
                    })
                    .to_string(),
            );
        }
        match answered["NextToken"].as_str() {
            Some(token) if !token.is_empty() => next = Some(token.to_string()),
            _ => return names,
        }
    }
}

/// Everything a published entry reports beside its name, asserted for one entry.
fn published_projection(case: &str, name: &str, entry: &Value) {
    assert_eq!(
        entry["Type"].as_str(),
        Some(PUBLISHED_TYPE),
        "case {case:?}: {name} is a {PUBLISHED_TYPE} parameter, got {entry}"
    );
    assert_eq!(
        entry["Tier"].as_str(),
        Some(PUBLISHED_TIER),
        "case {case:?}: {name} is of the {PUBLISHED_TIER} tier, got {entry}"
    );
    assert_eq!(
        entry["DataType"].as_str(),
        Some(PUBLISHED_DATA_TYPE),
        "case {case:?}: {name} carries {PUBLISHED_DATA_TYPE} data, got {entry}"
    );
    assert_eq!(
        entry["Policies"].as_array().map(Vec::len),
        Some(0),
        "case {case:?}: {name} carries no parameter policy, got {entry}"
    );
    assert!(
        entry["Version"].as_i64().is_some_and(|version| version >= 1),
        "case {case:?}: {name} carries a version of at least 1, got {entry}"
    );
    // No account owns a published parameter, so the ARN's account field is EMPTY where an
    // account's own parameter carries this endpoint's account id.
    assert_eq!(
        entry["ARN"].as_str(),
        Some(format!("arn:aws:ssm:{REGION}::parameter{name}").as_str()),
        "case {case:?}: {name} carries an account-less ARN, got {entry}"
    );
    assert!(
        !entry["ARN"]
            .as_str()
            .unwrap_or_default()
            .contains(ACCOUNT),
        "case {case:?}: {name}'s ARN names no account, got {entry}"
    );
}

/// A `Path` filter inside AWS's hierarchy describes exactly the published parameters the
/// hierarchy read of that same path serves, each with the published projection.
#[tokio::test]
async fn a_path_filter_inside_the_published_hierarchy_describes_its_parameters() {
    let router = emulator();
    for namespace in [
        AMAZON_LINUX_NAMESPACE,
        WINDOWS_NAMESPACE,
        GLOBAL_INFRASTRUCTURE_REGIONS,
    ] {
        let case = namespace;
        let described = describe_all(
            &router,
            case,
            &json!([{ "Key": "Path", "Option": "Recursive", "Values": [namespace] }]),
        )
        .await;
        let mut names: Vec<String> = described.iter().map(|(name, _)| name.clone()).collect();
        assert!(
            !names.is_empty(),
            "case {case:?}: expected the published parameters under {namespace} to be \
             described, got none"
        );
        for (name, entry) in &described {
            assert!(
                name.starts_with(&format!("{namespace}/")),
                "case {case:?}: expected every described parameter to be confined to \
                 {namespace}, got {name}"
            );
            published_projection(case, name, entry);
        }
        // The two reads of one catalogue agree: a name readable through the hierarchy read
        // is described, and nothing else is.
        let mut read = read_all(&router, case, namespace).await;
        names.sort();
        read.sort();
        assert_eq!(
            names, read,
            "case {case:?}: expected the described set to be the set the hierarchy read \
             serves, got {names:?} against {read:?}"
        );
    }
}

/// A `Path` filter's reach is the one its option names, for a published path as for the
/// account's own: `OneLevel` describes the level immediately below it and `Recursive` the
/// whole subtree.
#[tokio::test]
async fn a_published_path_filter_reaches_as_far_as_its_option_says() {
    let router = emulator();
    let case = "one level below the published service root";
    // `/aws/service` has every published namespace under it, but no published name sits
    // directly at that level, so the narrower reach describes none of them while the wider
    // one describes them all.
    let one_level = describe_all(
        &router,
        case,
        &json!([{ "Key": "Path", "Option": "OneLevel", "Values": ["/aws/service"] }]),
    )
    .await;
    assert!(
        one_level.is_empty(),
        "case {case:?}: expected no published name directly below /aws/service, got {:?}",
        one_level.iter().map(|(name, _)| name).collect::<Vec<_>>()
    );
    let recursive = describe_all(
        &router,
        case,
        &json!([{ "Key": "Path", "Option": "Recursive", "Values": ["/aws/service"] }]),
    )
    .await;
    assert!(
        recursive.len() > one_level.len(),
        "case {case:?}: expected the recursive reach to describe the whole published \
         subtree, got {} entries",
        recursive.len()
    );

    let case = "one level below a published namespace";
    let leaves = describe_all(
        &router,
        case,
        &json!([{ "Key": "Path", "Option": "OneLevel", "Values": [AMAZON_LINUX_NAMESPACE] }]),
    )
    .await;
    assert!(
        !leaves.is_empty(),
        "case {case:?}: expected the aliases directly below {AMAZON_LINUX_NAMESPACE} to be \
         described, got none"
    );
    for (name, _) in &leaves {
        let under = name
            .strip_prefix(&format!("{AMAZON_LINUX_NAMESPACE}/"))
            .unwrap_or_else(|| {
                panic!("case {case:?}: expected {name} to be under {AMAZON_LINUX_NAMESPACE}")
            });
        assert!(
            !under.contains('/'),
            "case {case:?}: expected only the level immediately below the path, got {name}"
        );
    }
}

/// A `Name` filter anchored at a published name describes it, by equality and by prefix; one
/// that only searches for a fragment of a name addresses no hierarchy, and a listing carrying
/// no filter at all reports the account's own parameters alone.
#[tokio::test]
async fn a_name_filter_describes_a_published_parameter_only_when_it_anchors_one() {
    let router = emulator();
    let published = read_all(&router, "the catalogue", AMAZON_LINUX_NAMESPACE).await;
    let anchor = published
        .first()
        .expect("the published catalogue holds an Amazon Linux alias")
        .clone();

    // The account's own parameter, so that "the account's own alone" is an assertion with
    // something in it. Its name carries the fragment the Contains row searches for.
    let own = "/own/ami-amazon-linux-latest/pin";
    let (status, body) = call(
        &router,
        "PutParameter",
        &json!({ "Name": own, "Type": "String", "Value": "pinned" }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the account's own parameter must be written, got {status} with {body}"
    );

    let case = "a Name filter equal to a published name";
    let described = describe_all(
        &router,
        case,
        &json!([{ "Key": "Name", "Option": "Equals", "Values": [anchor.clone()] }]),
    )
    .await;
    let names: Vec<&str> = described.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(
        names,
        vec![anchor.as_str()],
        "case {case:?}: expected exactly the published parameter asked for, got {names:?}"
    );
    published_projection(case, &anchor, &described[0].1);

    let case = "a Name filter prefixed with a published namespace";
    let described = describe_all(
        &router,
        case,
        &json!([{
            "Key": "Name",
            "Option": "BeginsWith",
            "Values": [AMAZON_LINUX_NAMESPACE],
        }]),
    )
    .await;
    let mut names: Vec<String> = described.iter().map(|(name, _)| name.clone()).collect();
    let mut expected = published.clone();
    names.sort();
    expected.sort();
    assert_eq!(
        names, expected,
        "case {case:?}: expected every published alias of the namespace, got {names:?}"
    );

    let case = "a Name filter searching for a fragment";
    let described = describe_all(
        &router,
        case,
        &json!([{
            "Key": "Name",
            "Option": "Contains",
            "Values": ["ami-amazon-linux-latest"],
        }]),
    )
    .await;
    let names: Vec<&str> = described.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(
        names,
        vec![own],
        "case {case:?}: expected the account's own parameter alone, a fragment search \
         addressing no hierarchy, got {names:?}"
    );

    let case = "a listing carrying no filter";
    let described = describe_all(&router, case, &json!([])).await;
    let names: Vec<&str> = described.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(
        names,
        vec![own],
        "case {case:?}: expected the account's own parameters alone, got {names:?}"
    );
}

/// A published entry is described with the same identity it is READ with: the ARN and the
/// version `DescribeParameters` reports for a published name are the ones `GetParameter`
/// reports for it, so describing the catalogue and reading it cannot disagree.
#[tokio::test]
async fn a_described_published_parameter_is_the_one_a_read_of_its_name_answers() {
    let router = emulator();
    let case = "describe then read";
    let described = describe_all(
        &router,
        case,
        &json!([{
            "Key": "Path",
            "Option": "Recursive",
            "Values": [AMAZON_LINUX_NAMESPACE],
        }]),
    )
    .await;
    assert!(
        !described.is_empty(),
        "case {case:?}: expected the published aliases to be described, got none"
    );
    for (name, entry) in &described {
        let (status, read) = call(&router, "GetParameter", &json!({ "Name": name })).await;
        assert_eq!(
            status, 200,
            "case {case:?}: {name} must be readable, got {status} with {read}"
        );
        assert_eq!(
            read["Parameter"]["ARN"], entry["ARN"],
            "case {case:?}: {name} is described and read under one ARN, got {read} against \
             {entry}"
        );
        assert_eq!(
            read["Parameter"]["Version"], entry["Version"],
            "case {case:?}: {name} is described and read at one version, got {read} against \
             {entry}"
        );
        assert_eq!(
            read["Parameter"]["DataType"], entry["DataType"],
            "case {case:?}: {name} is described and read with one data type, got {read} \
             against {entry}"
        );
    }
}
