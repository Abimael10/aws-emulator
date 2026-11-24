// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the MEMBERSHIP of the catalog `ListInstanceTypes` reports,
//! over the wire this service really speaks (awsJson1_0, `POST /`,
//! `X-Amz-Target: EUCMIFrontendAPIService.ListInstanceTypes`).
//!
//! The rule under test: a supported instance family is reported at EVERY size Amazon
//! EC2 publishes that family at, and at no size it does not — so the catalog read
//! answers a usable record for every `<family>.<size>` name a caller can launch, and
//! names nothing a launch would have to refuse. The sibling suites cover the CONTENT of
//! an entry (`instance_type_configuration_completeness`), the filter and the paging
//! (`instance_type_catalog`); this one is about which names are there.
//!
//! The table below differs along what the rule generalises over: generation (fifth
//! through eighth), vendor (Intel, AMD), and the SHAPE of the ladder — burstable
//! (`nano`…`2xlarge`), `-flex` (stops at `16xlarge`), high-frequency
//! (`3xlarge`/`6xlarge`), compute (`9xlarge`/`18xlarge`), dense storage, high memory and
//! graphics — with each family's WHOLE published ladder written out, so a reported
//! family that is short of a size, or carries one EC2 does not publish, fails. Each
//! ladder names its source in the comment above it, and only one of these families is a
//! family the issue behind this suite cited.

use std::collections::BTreeSet;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::workspace_instance_lifecycle::{call, create, error_code, ok, router, TEST_IMAGE_ID};

/// Supported families, each with the COMPLETE set of sizes AWS publishes it at.
///
/// SOURCE: the published Amazon EC2 instance-type names of each family — the same
/// `<family>.<size>` names the EC2 API's own `InstanceType` enumeration carries
/// (`smithy_query(query="shape",shape="InstanceType")` against the EC2 service model in
/// this workspace) — with the bare-metal sizes left out, because a WorkSpace Instance is
/// a managed EC2 instance rather than a dedicated host.
const PUBLISHED_LADDERS: &[(&str, &[&str])] = &[
    // Compute optimised, fifth generation: `c5` steps through 9xlarge/18xlarge where the
    // other fifth-generation families step through 8xlarge/16xlarge.
    (
        "c5",
        &[
            "large", "xlarge", "2xlarge", "4xlarge", "9xlarge", "12xlarge", "18xlarge", "24xlarge",
        ],
    ),
    // The network-optimised compute family stops at 18xlarge.
    (
        "c5n",
        &["large", "xlarge", "2xlarge", "4xlarge", "9xlarge", "18xlarge"],
    ),
    // Fifth-generation AMD compute: the 8xlarge/16xlarge ladder.
    (
        "c5ad",
        &[
            "large", "xlarge", "2xlarge", "4xlarge", "8xlarge", "12xlarge", "16xlarge", "24xlarge",
        ],
    ),
    // Sixth-generation Intel: reaches 32xlarge.
    (
        "c6in",
        &[
            "large", "xlarge", "2xlarge", "4xlarge", "8xlarge", "12xlarge", "16xlarge", "24xlarge",
            "32xlarge",
        ],
    ),
    // Seventh-generation AMD: starts at medium and reaches 48xlarge.
    (
        "c7a",
        &[
            "medium", "large", "xlarge", "2xlarge", "4xlarge", "8xlarge", "12xlarge", "16xlarge",
            "24xlarge", "32xlarge", "48xlarge",
        ],
    ),
    // A `-flex` family stops at 16xlarge.
    (
        "c7i-flex",
        &[
            "large", "xlarge", "2xlarge", "4xlarge", "8xlarge", "12xlarge", "16xlarge",
        ],
    ),
    // Eighth-generation Intel: reaches 96xlarge.
    (
        "c8i",
        &[
            "large", "xlarge", "2xlarge", "4xlarge", "8xlarge", "12xlarge", "16xlarge", "24xlarge",
            "32xlarge", "48xlarge", "96xlarge",
        ],
    ),
    // General purpose, fifth generation.
    (
        "m5",
        &[
            "large", "xlarge", "2xlarge", "4xlarge", "8xlarge", "12xlarge", "16xlarge", "24xlarge",
        ],
    ),
    // High frequency: steps through 3xlarge/6xlarge.
    (
        "m5zn",
        &["large", "xlarge", "2xlarge", "3xlarge", "6xlarge", "12xlarge"],
    ),
    // Sixth-generation AMD: one step further than the Intel ladder, to 48xlarge.
    (
        "m6a",
        &[
            "large", "xlarge", "2xlarge", "4xlarge", "8xlarge", "12xlarge", "16xlarge", "24xlarge",
            "32xlarge", "48xlarge",
        ],
    ),
    // Seventh-generation Intel: no 32xlarge between 24xlarge and 48xlarge.
    (
        "m7i",
        &[
            "large", "xlarge", "2xlarge", "4xlarge", "8xlarge", "12xlarge", "16xlarge", "24xlarge",
            "48xlarge",
        ],
    ),
    (
        "m8i-flex",
        &[
            "large", "xlarge", "2xlarge", "4xlarge", "8xlarge", "12xlarge", "16xlarge",
        ],
    ),
    // Eighth-generation AMD: medium at the bottom, 48xlarge at the top, no 32xlarge.
    (
        "m8a",
        &[
            "medium", "large", "xlarge", "2xlarge", "4xlarge", "8xlarge", "12xlarge", "16xlarge",
            "24xlarge", "48xlarge",
        ],
    ),
    // Memory optimised.
    (
        "r5b",
        &[
            "large", "xlarge", "2xlarge", "4xlarge", "8xlarge", "12xlarge", "16xlarge", "24xlarge",
        ],
    ),
    (
        "r6idn",
        &[
            "large", "xlarge", "2xlarge", "4xlarge", "8xlarge", "12xlarge", "16xlarge", "24xlarge",
            "32xlarge",
        ],
    ),
    // The high-frequency memory-optimised family: 16xlarge then straight to 32xlarge.
    (
        "r7iz",
        &[
            "large", "xlarge", "2xlarge", "4xlarge", "8xlarge", "12xlarge", "16xlarge", "32xlarge",
        ],
    ),
    (
        "r8i-flex",
        &[
            "large", "xlarge", "2xlarge", "4xlarge", "8xlarge", "12xlarge", "16xlarge",
        ],
    ),
    ("z1d", &["large", "xlarge", "2xlarge", "3xlarge", "6xlarge", "12xlarge"]),
    // High memory: offered at a handful of sizes only.
    ("x1", &["16xlarge", "32xlarge"]),
    (
        "x1e",
        &["xlarge", "2xlarge", "4xlarge", "8xlarge", "16xlarge", "32xlarge"],
    ),
    ("x2idn", &["16xlarge", "24xlarge", "32xlarge"]),
    (
        "x2iezn",
        &["2xlarge", "4xlarge", "6xlarge", "8xlarge", "12xlarge"],
    ),
    // Storage optimised.
    ("d2", &["xlarge", "2xlarge", "4xlarge", "8xlarge"]),
    (
        "d3en",
        &["xlarge", "2xlarge", "4xlarge", "6xlarge", "8xlarge", "12xlarge"],
    ),
    ("h1", &["2xlarge", "4xlarge", "8xlarge", "16xlarge"]),
    (
        "i3en",
        &[
            "large", "xlarge", "2xlarge", "3xlarge", "6xlarge", "12xlarge", "24xlarge",
        ],
    ),
    (
        "i4i",
        &[
            "large", "xlarge", "2xlarge", "4xlarge", "8xlarge", "12xlarge", "16xlarge", "24xlarge",
            "32xlarge",
        ],
    ),
    (
        "i7ie",
        &[
            "large", "xlarge", "2xlarge", "3xlarge", "6xlarge", "12xlarge", "18xlarge", "24xlarge",
            "48xlarge",
        ],
    ),
    // Burstable: the whole ladder, from nano up.
    (
        "t3a",
        &[
            "nano", "micro", "small", "medium", "large", "xlarge", "2xlarge",
        ],
    ),
    // Graphics.
    (
        "g4ad",
        &["xlarge", "2xlarge", "4xlarge", "8xlarge", "16xlarge"],
    ),
    (
        "g4dn",
        &["xlarge", "2xlarge", "4xlarge", "8xlarge", "12xlarge", "16xlarge"],
    ),
    (
        "g6e",
        &[
            "xlarge", "2xlarge", "4xlarge", "8xlarge", "12xlarge", "16xlarge", "24xlarge",
            "48xlarge",
        ],
    ),
    ("gr6", &["4xlarge", "8xlarge"]),
];

/// Names a caller may NOT launch, one row per reason the name is outside the catalog.
///
/// SOURCE: the same EC2 instance-type enumeration. A bare-metal size is a dedicated
/// host rather than a managed instance; an AArch64 (Graviton) family runs no WorkSpaces
/// x86-64 platform image; a previous-generation family is no longer offered for this
/// service. The bare-metal rows name families the catalog DOES report at every other
/// size, so each one is the same family at the one size that must be absent.
const UNPUBLISHED_NAMES: &[(&str, &str)] = &[
    ("bare metal", "c5.metal"),
    ("bare metal", "c5n.metal"),
    ("bare metal", "m5zn.metal"),
    ("bare metal", "i3.metal"),
    ("bare metal", "i4i.metal"),
    ("bare metal", "x2iedn.metal"),
    ("bare metal", "g4dn.metal"),
    ("bare metal", "z1d.metal"),
    ("AArch64 (Graviton)", "c7g.large"),
    ("AArch64 (Graviton)", "x2gd.medium"),
    ("previous generation", "c4.large"),
    ("previous generation", "m3.medium"),
];

/// Every `InstanceType` the catalog reports, paging to the end so no prefix is mistaken
/// for the whole answer.
async fn reported_names(router: &MultiServiceRouter) -> Vec<String> {
    let mut names = Vec::new();
    let mut request = json!({});
    loop {
        let page = ok(router, "ListInstanceTypes", request.clone()).await;
        for entry in page["InstanceTypes"]
            .as_array()
            .unwrap_or_else(|| panic!("ListInstanceTypes reported no InstanceTypes: {page}"))
        {
            names.push(
                entry["InstanceType"]
                    .as_str()
                    .unwrap_or_else(|| panic!("an InstanceTypeInfo carries no InstanceType: {entry}"))
                    .to_string(),
            );
        }
        match page.get("NextToken").and_then(Value::as_str) {
            Some(token) => request["NextToken"] = json!(token),
            None => return names,
        }
    }
}

/// The sizes the catalog reports `family` at: the size half of every reported name whose
/// family half is exactly `family`, so `c5` never collects `c5a`'s sizes.
fn reported_sizes(names: &[String], family: &str) -> BTreeSet<String> {
    names
        .iter()
        .filter_map(|name| name.split_once('.'))
        .filter(|(reported_family, _)| *reported_family == family)
        .map(|(_, size)| size.to_string())
        .collect()
}

/// The rule: a supported family is reported at every size EC2 publishes it at, and at no
/// other size.
#[tokio::test]
async fn a_supported_family_is_reported_at_every_published_size() {
    let router = router();
    let names = reported_names(&router).await;

    for (family, ladder) in PUBLISHED_LADDERS {
        let expected: BTreeSet<String> = ladder.iter().map(|size| (*size).to_string()).collect();
        let actual = reported_sizes(&names, family);
        assert_eq!(
            actual, expected,
            "case family {family:?}: expected exactly the published ladder {expected:?}, got \
             {actual:?} out of {} reported instance types",
            names.len()
        );
    }
}

/// The other direction: a name outside the catalog is not reported, and a reported name
/// is always a `<family>.<size>` the modelled `InstanceType` admits.
#[tokio::test]
async fn a_name_the_service_cannot_launch_is_not_reported() {
    let router = router();
    let names = reported_names(&router).await;

    for (reason, name) in UNPUBLISHED_NAMES {
        assert!(
            !names.iter().any(|reported| reported == name),
            "case {name:?} ({reason}): a name outside the service's catalog must not be reported"
        );
    }

    // No bare-metal size at all, whichever family carries it, and every reported name
    // inside the modelled `InstanceType` (`len 1..64`, `^([a-z0-9-]+)\.([a-z0-9]+)$`).
    for name in &names {
        let (family, size) = name
            .split_once('.')
            .unwrap_or_else(|| panic!("case {name:?}: a reported name must be <family>.<size>"));
        assert!(
            size != "metal" && !size.starts_with("metal-"),
            "case {name:?}: a bare-metal size is a dedicated host, not a WorkSpace Instance"
        );
        assert!(
            (1..=64).contains(&name.len())
                && !family.is_empty()
                && !size.is_empty()
                && family
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
                && size
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()),
            "case {name:?}: expected a name inside the modelled InstanceType pattern"
        );
    }
}

/// What the catalog read is FOR: every name it reports is a name a create accepts, and a
/// name it does not report is refused — so a caller that picks a type out of this answer
/// can launch it, and discovery and launch agree on one catalog.
#[tokio::test]
async fn a_reported_instance_type_launches_and_an_unreported_one_is_refused() {
    let router = router();
    let names = reported_names(&router).await;

    // One name per ladder shape, taken from the answer itself: the bottom and the top of
    // a ladder, a burstable size, a `-flex` size, a high-memory size and a graphics size.
    let launchable = [
        "the first reported type",
        "a burstable size",
        "a `-flex` size",
        "a high-memory size",
        "a graphics size",
        "the last reported type",
    ];
    let picks = [
        names.first().cloned(),
        names.iter().find(|name| name.as_str() == "t3a.nano").cloned(),
        names
            .iter()
            .find(|name| name.as_str() == "m8i-flex.16xlarge")
            .cloned(),
        names.iter().find(|name| name.as_str() == "x1.32xlarge").cloned(),
        names.iter().find(|name| name.as_str() == "gr6.8xlarge").cloned(),
        names.last().cloned(),
    ];
    for (case, pick) in launchable.iter().zip(picks) {
        let instance_type = pick.unwrap_or_else(|| {
            panic!("case {case:?}: the catalog must report it, out of {} names", names.len())
        });
        let id = create(
            &router,
            json!({
                "ManagedInstance": {
                    "ImageId": TEST_IMAGE_ID,
                    "InstanceType": instance_type,
                }
            }),
        )
        .await;
        let got = ok(
            &router,
            "GetWorkspaceInstance",
            json!({ "WorkspaceInstanceId": id }),
        )
        .await;
        assert_eq!(
            got["WorkspaceInstanceId"].as_str(),
            Some(id.as_str()),
            "case {case:?} ({instance_type:?}): expected the launch to be stored, got {got}"
        );
    }

    for (reason, name) in UNPUBLISHED_NAMES {
        let (status, payload) = call(
            &router,
            "CreateWorkspaceInstance",
            json!({
                "ManagedInstance": { "ImageId": TEST_IMAGE_ID, "InstanceType": name }
            }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {name:?} ({reason}): a type outside the catalog must be refused, got {status} \
             with {payload}"
        );
        assert_eq!(
            error_code(&payload),
            "ValidationException",
            "case {name:?} ({reason}): expected ValidationException, got {payload}"
        );
    }
}
