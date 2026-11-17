// SPDX-License-Identifier: CC0-1.0

//! The AWS Global Infrastructure public parameters, end to end over the front door.
//!
//! RULE under test: the `/aws/service/global-infrastructure` hierarchy is AWS's own public
//! REFERENCE DATA, readable by any caller — "you can call the Parameter Store
//! `GetParametersByPath` API operation to view a list of all active Regions", and the long
//! name of a Region is read with `GetParameter` on
//! `/aws/service/global-infrastructure/regions/<region>/longName` (AWS Systems Manager User
//! Guide, "Calling AWS Region, service, and endpoint public parameters") — so a read of it
//! is answered from the published catalogue rather than refused as a hierarchy the account
//! is kept out of. A name inside it that AWS publishes nothing under is still the modelled
//! miss, and a PATH of the reserved hierarchy that is no namespace of it is still denied.
//!
//! Every case goes through the wire a client uses: `POST /` with
//! `X-Amz-Target: AmazonSSM.<Operation>` and an `awsJson1_1` body (`OPERATIONS.md`, "Wire
//! protocol"). Nothing reaches into a state struct.
//!
//! The tables' rows differ along what the rule generalises over: which Region is read
//! (including Regions of the China and AWS GovCloud partitions, whose partition and domain
//! differ), which property of it, which read resolves it (one name, a batch of names, a
//! hierarchy page), and whether the hierarchy is walked recursively. The expected long
//! names below are written out from the AWS General Reference's "Regions and Availability
//! Zones" listing rather than read from the implementation, so a wrong catalogue cannot
//! pass, and they span Regions the Region-discovery workflow does not name.
use serde_json::{json, Value};
use service_com_amazonaws_ssm_amazonssm::state::global_infrastructure::{
    PUBLISHED_REGIONS, REGION_PROPERTIES,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::parameter_store::{call, error_code};
use crate::public_parameter_catalog::{emulator, listing};

/// The level of the catalogue the Regions are published at, as a client writes it.
const REGIONS_PATH: &str = "/aws/service/global-infrastructure/regions";

/// The level above it, which a recursive read reaches the whole Region catalogue through.
const GLOBAL_INFRASTRUCTURE_PATH: &str = "/aws/service/global-infrastructure";

/// The `ParameterType` a published parameter carries, from the modelled enum
/// (`smithy_query(query="shape", shape="ParameterType")`).
const PUBLISHED_TYPE: &str = "String";

/// The modelled class a name nothing publishes is reported as
/// (`smithy_query(query="operation", operation="GetParameter")`).
const NOT_FOUND: &str = "ParameterNotFound";

/// The modelled class a path of the reserved hierarchy that is no namespace of it is
/// refused with.
const DENIED: &str = "AccessDeniedException";

/// One Region a case reads, with the values AWS publishes under it: its long name, the
/// partition it belongs to and the domain its endpoints are served from. Written out from
/// the AWS General Reference's Region listing, not from this emulator's own catalogue.
struct Expected {
    case: &'static str,
    code: &'static str,
    long_name: &'static str,
    partition: &'static str,
    domain: &'static str,
}

/// The Regions the tables below read: the Region a Region-discovery workflow starts from,
/// three commercial Regions in other geographies, and one Region of each partition whose
/// partition and endpoint domain are not the commercial ones.
const EXPECTED: &[Expected] = &[
    Expected {
        case: "the Region a discovery workflow starts from",
        code: "us-east-1",
        long_name: "US East (N. Virginia)",
        partition: "aws",
        domain: "amazonaws.com",
    },
    Expected {
        case: "a European Region",
        code: "eu-west-1",
        long_name: "Europe (Ireland)",
        partition: "aws",
        domain: "amazonaws.com",
    },
    Expected {
        case: "an Asia Pacific Region",
        code: "ap-northeast-1",
        long_name: "Asia Pacific (Tokyo)",
        partition: "aws",
        domain: "amazonaws.com",
    },
    Expected {
        case: "a Canadian Region",
        code: "ca-central-1",
        long_name: "Canada (Central)",
        partition: "aws",
        domain: "amazonaws.com",
    },
    Expected {
        case: "a Region of the China partition, whose endpoint domain differs",
        code: "cn-north-1",
        long_name: "China (Beijing)",
        partition: "aws-cn",
        domain: "amazonaws.com.cn",
    },
    Expected {
        case: "a Region of the AWS GovCloud partition",
        code: "us-gov-west-1",
        long_name: "AWS GovCloud (US-West)",
        partition: "aws-us-gov",
        domain: "amazonaws.com",
    },
];

/// `GetParameter` on `name`, returning `(status, body)`.
async fn get_parameter(router: &MultiServiceRouter, name: &str) -> (u16, Value) {
    call(router, "GetParameter", &json!({ "Name": name })).await
}

/// The `Value` of a parameter that must have resolved, with its `Type` checked: a published
/// parameter is a `String`.
async fn value_of(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let (status, body) = get_parameter(router, name).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected {name} to resolve, got {status} with {body}"
    );
    assert_eq!(
        body["Parameter"]["Type"].as_str(),
        Some(PUBLISHED_TYPE),
        "case {case:?}: a published parameter is a {PUBLISHED_TYPE}, got {body}"
    );
    body["Parameter"]["Value"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: {name} must report a Value, got {body}"))
        .to_string()
}

/// The `Name` of every parameter of a read, in the order reported.
fn names(page: &[Value]) -> Vec<String> {
    page.iter()
        .map(|parameter| {
            parameter["Name"]
                .as_str()
                .unwrap_or("<unset>")
                .to_string()
        })
        .collect()
}

/// A hierarchy read of the Regions level lists one parameter per published Region, whose
/// value is the Region code — the list of Regions a client discovers them through.
#[tokio::test]
async fn the_regions_level_lists_one_parameter_per_region() {
    let router = emulator();
    let case = "the Regions level";
    let page = listing(&router, case, REGIONS_PATH, false).await;
    let listed = names(&page);

    // The whole catalogue, not a sample of it: every Region the endpoint publishes is in
    // the page, and the page holds nothing else.
    assert_eq!(
        listed.len(),
        PUBLISHED_REGIONS.len(),
        "case {case:?}: expected one parameter per published Region, got {listed:?}"
    );
    for region in PUBLISHED_REGIONS {
        let expected = format!("{REGIONS_PATH}/{region}");
        let found = page
            .iter()
            .find(|parameter| parameter["Name"].as_str() == Some(expected.as_str()))
            .unwrap_or_else(|| {
                panic!("case {case:?}: expected {expected} in the page, got {listed:?}")
            });
        assert_eq!(
            found["Value"].as_str(),
            Some(*region),
            "case {case:?}: a Region's own parameter publishes its code, got {found}"
        );
    }

    // A read changes nothing a later caller sees: the same page is served again.
    let again = names(&listing(&router, case, REGIONS_PATH, false).await);
    assert_eq!(
        again, listed,
        "case {case:?}: a read of the catalogue reports the same page again, got {again:?}"
    );
}

/// Each property AWS publishes under a Region resolves by name, for Regions across the
/// partitions — the long name a client reads to label a Region, and the partition and
/// domain it belongs to.
#[tokio::test]
async fn every_published_property_of_a_region_resolves_by_name() {
    let router = emulator();

    for row in EXPECTED {
        assert!(
            PUBLISHED_REGIONS.contains(&row.code),
            "case {:?}: {} must be a Region the endpoint publishes",
            row.case,
            row.code
        );
        let code = value_of(&router, row.case, &format!("{REGIONS_PATH}/{}", row.code)).await;
        assert_eq!(
            code, row.code,
            "case {:?}: the Region's own parameter publishes its code, got {code:?}",
            row.case
        );
        for (property, expected) in [
            ("longName", row.long_name),
            ("partition", row.partition),
            ("domain", row.domain),
        ] {
            let name = format!("{REGIONS_PATH}/{}/{property}", row.code);
            let served = value_of(&router, row.case, &name).await;
            assert_eq!(
                served, expected,
                "case {:?}: expected {name} to publish {expected:?}, got {served:?}",
                row.case
            );
        }
    }

    // A batch read resolves the same names in one call, which is how a client reads a
    // Region's properties together.
    let case = "a batch of published names";
    let wanted: Vec<String> = EXPECTED
        .iter()
        .map(|row| format!("{REGIONS_PATH}/{}/longName", row.code))
        .collect();
    let (status, body) = call(&router, "GetParameters", &json!({ "Names": wanted })).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the batch to be served, got {status} with {body}"
    );
    assert!(
        body["InvalidParameters"]
            .as_array()
            .is_none_or(|invalid| invalid.is_empty()),
        "case {case:?}: every published name resolves, got {body}"
    );
    let mut served: Vec<String> = body["Parameters"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|parameter| parameter["Value"].as_str().unwrap_or("<unset>").to_string())
        .collect();
    served.sort();
    let mut expected: Vec<String> = EXPECTED
        .iter()
        .map(|row| row.long_name.to_string())
        .collect();
    expected.sort();
    assert_eq!(
        served, expected,
        "case {case:?}: the batch reports each Region's long name, got {body}"
    );
}

/// A recursive read of the catalogue reaches every name under it, and a non-recursive read
/// of one Region reaches the properties published immediately under that Region.
#[tokio::test]
async fn the_catalogue_is_walked_recursively_and_level_by_level() {
    let router = emulator();

    let case = "the whole catalogue, recursively";
    let every = names(&listing(&router, case, GLOBAL_INFRASTRUCTURE_PATH, true).await);
    assert_eq!(
        every.len(),
        PUBLISHED_REGIONS.len() * (1 + REGION_PROPERTIES.len()),
        "case {case:?}: expected every Region and every property of it, got {} names",
        every.len()
    );
    let mut sorted = every.clone();
    sorted.sort();
    assert_eq!(
        sorted, every,
        "case {case:?}: a hierarchy read walks the catalogue in name order, got {every:?}"
    );

    // One level below a Region: the properties of that Region and nothing of another's.
    for row in EXPECTED {
        let path = format!("{REGIONS_PATH}/{}", row.code);
        let under = names(&listing(&router, row.case, &path, false).await);
        assert_eq!(
            under.len(),
            REGION_PROPERTIES.len(),
            "case {:?}: expected the properties published under {path}, got {under:?}",
            row.case
        );
        assert!(
            under.iter().all(|name| name.starts_with(&format!("{path}/"))),
            "case {:?}: a level read reaches only that Region's own properties, got {under:?}",
            row.case
        );
    }
}

/// What a read of the reserved hierarchy that names nothing is answered with: a NAME it
/// publishes nothing under is the modelled miss, and a PATH that is no namespace of it is
/// still denied rather than answered with an empty page.
#[tokio::test]
async fn a_name_the_catalogue_does_not_publish_is_still_a_miss() {
    let router = emulator();

    // (case, the name read)
    let missing: &[(&str, String)] = &[
        (
            "a Region code nothing publishes",
            format!("{REGIONS_PATH}/no-such-region-1"),
        ),
        (
            "a property of a Region that nothing publishes",
            format!("{REGIONS_PATH}/no-such-region-1/longName"),
        ),
        (
            "a property name the catalogue does not publish",
            format!("{REGIONS_PATH}/us-east-1/noSuchProperty"),
        ),
    ];
    for (case, name) in missing {
        let (status, body) = get_parameter(&router, name).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected {name} to be a miss, got {status} with {body}"
        );
        assert_eq!(
            error_code(&body),
            NOT_FOUND,
            "case {case:?}: expected {NOT_FOUND} for {name}, got {body}"
        );
    }

    // A hierarchy read of a path inside the reserved hierarchy that no published name lies
    // under is denied, as it was before this catalogue was published there.
    let case = "a path of the reserved hierarchy that is no namespace of it";
    let (status, body) = call(
        &router,
        "GetParametersByPath",
        &json!({ "Path": "/aws/service/global-infrastructure/zones" }),
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected the read to be refused, got {status} with {body}"
    );
    assert_eq!(
        error_code(&body),
        DENIED,
        "case {case:?}: expected {DENIED}, got {body}"
    );
}
