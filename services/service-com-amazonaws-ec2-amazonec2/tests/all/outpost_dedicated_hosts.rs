// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for Dedicated Hosts allocated ON AN OUTPOST, driven over the wire the
//! service actually speaks — `ec2Query`, through the crate's shared [`super::ec2_query`]
//! harness.
//!
//! The rules under test, each stated once and then exercised over a SET of inputs:
//!
//! 1. A host is allocated either IN THE SIGNED REGION or ON AN OUTPOST of it: `OutpostArn`
//!    names that Outpost and `AssetIds` the hardware assets of it to allocate on, so an
//!    allocation naming assets mints ONE host per asset — "a Dedicated Host on each
//!    specified hardware asset" — each reporting the Outpost it was allocated on
//!    (`Host$OutpostArn`) and the one asset it sits on (`Host$AssetId`), while a host
//!    allocated in the Region reports neither.
//! 2. `Quantity` may be omitted when `AssetIds` names the hardware, and when both are sent
//!    the count must equal the number of asset ids.
//! 3. A value that could name no Outpost of this Region and account is refused rather than
//!    answered with a host in the Region: an ARN of another Region, of another account, of
//!    another service, one whose Outpost id is not of the published form, an empty one, one
//!    longer than this service resolves, and a bare id that is no ARN at all. `AssetIds`
//!    without `OutpostArn` is refused the same way, since it names hardware of no Outpost,
//!    and so is an asset id that is empty or named twice — one host is allocated on each
//!    asset named, so neither reduces the batch the caller asked for silently.
//!
//! Whether that Outpost EXISTS is not EC2's answer to give: an Outpost is hardware Amazon
//! Web Services installs in the customer's facility and no operation of this service
//! creates one, so this service holds no Outpost inventory. What runs on an Outpost's
//! hardware is answered by the service that owns the hardware, from these hosts — see that
//! service's own suite over `ListAssetInstances`.

use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_ec2_amazonec2::state::{
    availability_zones_in_region, MAX_OUTPOST_ARN_CHARS,
};

use super::ec2_query::{call, element, elements, error_code, indexed};
use super::wire::{child_text, document, set_items};

/// The Region every call here is signed for: a host sits in one zone of one Region, and an
/// Outpost belongs to one Region's inventory, so both are read against this one.
const REGION: &str = "us-west-2";

/// The account id every request this emulator serves arrives as, which is therefore the
/// only account an Outpost ARN this service resolves can name. Read from the engine itself,
/// the same place the handler reads it.
fn own_account() -> &'static str {
    ::engine::emulator_account_id()
}

/// The instance type every host below supports.
const INSTANCE_TYPE: &str = "m5.4xlarge";

/// Two hardware asset ids of the ten-digit form an Outpost's units are identified by.
const ASSETS: [&str; 2] = ["0000000105", "0000000106"];

/// A `(key, value)` request parameter.
fn p(key: &str, value: impl Into<String>) -> (String, String) {
    (key.to_string(), value.into())
}

/// One request's form parameters, as the ec2Query harness takes them.
type Params = Vec<(String, String)>;

/// Where one host sits, as it reports it: the `outpostArn` of the Outpost it was allocated
/// on and its `assetId`, each `None` for a member the host reports none of.
type Placement = (Option<String>, Option<&'static str>);

/// One row of the allocation table: the case, the parameters beside the fixed ones, and the
/// placement each host the allocation mints must report.
type AllocationRow = (&'static str, Params, Vec<Placement>);

/// One row of the refusal table: the case, the parameters beside the fixed ones, and the
/// error code the refusal must carry.
type RefusalRow = (&'static str, Params, &'static str);

fn emulator() -> MultiServiceRouter {
    super::ec2_query::emulator().0
}

/// The name of the first Availability Zone of the signed Region.
fn zone_name() -> String {
    availability_zones_in_region(REGION)[0].zone_name.clone()
}

/// The ARN of an Outpost of `region` in `account`, as the one `OutpostArn` pattern this
/// model carries spells it out:
/// `arn:aws:outposts:<region>:<account>:outpost/op-<17 hex digits>`.
fn outpost_arn(region: &str, account: &str, outpost_id: &str) -> String {
    format!("arn:aws:outposts:{region}:{account}:outpost/{outpost_id}")
}

/// An Outpost id of the published form, whose ARN every allocation that succeeds here
/// names.
const OUTPOST_ID: &str = "op-0123456789abcdef0";

/// The ARN of the Outpost every succeeding allocation below is made on.
fn an_outpost() -> String {
    outpost_arn(REGION, own_account(), OUTPOST_ID)
}

/// An Outpost ARN of exactly `chars` characters, padded in the PARTITION field — the one
/// field of the published form whose length is the partition's own name, and the one this
/// service reads nothing out of.
fn padded_outpost_arn(chars: usize) -> String {
    let tail = format!(
        ":outposts:{REGION}:{}:outpost/{OUTPOST_ID}",
        own_account()
    );
    let scheme = "arn:";
    let partition = chars.saturating_sub(scheme.len() + tail.len());
    let arn = format!("{scheme}{}{tail}", "a".repeat(partition));
    assert_eq!(
        arn.len(),
        chars,
        "the padded ARN must be exactly {chars} characters long"
    );
    arn
}

async fn allocate(router: &MultiServiceRouter, params: &[(String, String)]) -> (u16, String) {
    call(router, REGION, "AllocateHosts", params).await
}

/// The ids in an `AllocateHosts` answer: the `<item>`s of its `hostIdSet`.
fn allocated_ids(body: &str) -> Vec<String> {
    element(body, "hostIdSet")
        .map(|set| elements(&set, "item"))
        .unwrap_or_default()
}

/// What `DescribeHosts` reports for `host_id`: its `(outpostArn, assetId)`, each `None`
/// when the host reports no such member.
async fn described_placement(
    router: &MultiServiceRouter,
    case: &str,
    host_id: &str,
) -> (Option<String>, Option<String>) {
    let (status, body) = call(
        router,
        REGION,
        "DescribeHosts",
        &indexed("HostId", &[host_id]),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeHosts must report {host_id}, got {status} with body {body}"
    );
    let parsed = document(case, &body);
    let items = set_items(parsed.root_element(), "hostSet");
    let host = items
        .iter()
        .find(|item| child_text(**item, "hostId").as_deref() == Some(host_id))
        .copied()
        .unwrap_or_else(|| panic!("case {case:?}: no host {host_id} in {body}"));
    (
        child_text(host, "outpostArn"),
        child_text(host, "assetId"),
    )
}

/// Rules 1 and 2: where an allocation puts its hosts, and what each one reports.
///
/// The rows differ along what the rule generalises over: how the allocation names its
/// hardware (two assets, one asset, no asset at all, no Outpost at all), whether it carries
/// a `Quantity` beside the assets, how long the ARN naming the Outpost is (the published
/// form, and one of exactly [`MAX_OUTPOST_ARN_CHARS`] characters, the longest this service
/// resolves), and how many hosts it therefore mints.
#[tokio::test]
async fn an_allocation_puts_one_host_on_each_outpost_asset_it_names() {
    let router = emulator();
    let outpost = an_outpost();
    let longest = padded_outpost_arn(MAX_OUTPOST_ARN_CHARS);

    // (case, the extra parameters, the (outpostArn, assetId) each minted host must report)
    let rows: Vec<AllocationRow> = vec![
        (
            "two assets of the Outpost and no quantity, so one host on each",
            vec![
                p("OutpostArn", outpost.clone()),
                p("AssetId.1", ASSETS[0]),
                p("AssetId.2", ASSETS[1]),
            ],
            vec![
                (Some(outpost.clone()), Some(ASSETS[0])),
                (Some(outpost.clone()), Some(ASSETS[1])),
            ],
        ),
        (
            "one asset of the Outpost beside the quantity that matches it",
            vec![
                p("OutpostArn", outpost.clone()),
                p("AssetId.1", ASSETS[0]),
                p("Quantity", "1"),
            ],
            vec![(Some(outpost.clone()), Some(ASSETS[0]))],
        ),
        (
            "two assets of the Outpost beside the quantity that matches them",
            vec![
                p("OutpostArn", outpost.clone()),
                p("AssetId.1", ASSETS[0]),
                p("AssetId.2", ASSETS[1]),
                p("Quantity", "2"),
            ],
            vec![
                (Some(outpost.clone()), Some(ASSETS[0])),
                (Some(outpost.clone()), Some(ASSETS[1])),
            ],
        ),
        (
            "the Outpost but no asset at all, so two hosts on no named hardware",
            vec![p("OutpostArn", outpost.clone()), p("Quantity", "2")],
            vec![
                (Some(outpost.clone()), None),
                (Some(outpost.clone()), None),
            ],
        ),
        (
            "an Outpost ARN of exactly the longest this service resolves",
            vec![p("OutpostArn", longest.clone()), p("Quantity", "1")],
            vec![(Some(longest.clone()), None)],
        ),
        (
            "no Outpost at all, so a host in the Region",
            vec![p("Quantity", "1")],
            vec![(None, None)],
        ),
    ];

    for (case, extra, expected) in &rows {
        let mut params = vec![
            p("InstanceType", INSTANCE_TYPE),
            p("AvailabilityZone", zone_name()),
        ];
        params.extend(extra.clone());
        let (status, body) = allocate(&router, &params).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the allocation must succeed, got {status} with body {body}"
        );
        let host_ids = allocated_ids(&body);
        assert_eq!(
            host_ids.len(),
            expected.len(),
            "case {case:?}: expected {} host(s), got {host_ids:?} in {body}",
            expected.len()
        );
        for (host_id, (expected_arn, asset_of)) in host_ids.iter().zip(expected) {
            let reported = described_placement(&router, case, host_id).await;
            assert_eq!(
                (reported.0.as_deref(), reported.1.as_deref()),
                (expected_arn.as_deref(), *asset_of),
                "case {case:?}: {host_id} must report the Outpost and the asset it was \
                 allocated on, got {reported:?}"
            );
        }
    }
}

/// Rule 3: every value that could name no Outpost of this Region and account, and every
/// request whose `Quantity` cannot hold beside the assets it names, is refused — with the
/// code EC2 answers for the member that cannot hold.
#[tokio::test]
async fn a_value_that_names_no_outpost_of_this_region_is_refused() {
    let router = emulator();

    // (case, the extra parameters, the expected error code)
    let rows: Vec<RefusalRow> = vec![
        (
            "an Outpost ARN of another Region",
            vec![
                p("OutpostArn", outpost_arn("eu-west-1", own_account(), OUTPOST_ID)),
                p("Quantity", "1"),
            ],
            "InvalidParameterValue",
        ),
        (
            "an Outpost ARN of another account",
            vec![
                p("OutpostArn", outpost_arn(REGION, "111122223333", OUTPOST_ID)),
                p("Quantity", "1"),
            ],
            "InvalidParameterValue",
        ),
        (
            "an ARN of another service altogether",
            vec![
                p(
                    "OutpostArn",
                    format!("arn:aws:ec2:{REGION}:{}:outpost/{OUTPOST_ID}", own_account()),
                ),
                p("Quantity", "1"),
            ],
            "InvalidParameterValue",
        ),
        (
            "an Outpost ARN whose id is not of the published form",
            vec![
                p("OutpostArn", outpost_arn(REGION, own_account(), "op-nothex")),
                p("Quantity", "1"),
            ],
            "InvalidParameterValue",
        ),
        (
            "an Outpost ARN naming another resource type",
            vec![
                p(
                    "OutpostArn",
                    format!(
                        "arn:aws:outposts:{REGION}:{}:site/os-0123456789abcdef0",
                        own_account()
                    ),
                ),
                p("Quantity", "1"),
            ],
            "InvalidParameterValue",
        ),
        (
            "the bare Outpost id instead of its ARN",
            vec![p("OutpostArn", OUTPOST_ID), p("Quantity", "1")],
            "InvalidParameterValue",
        ),
        (
            "hardware assets without the Outpost they are installed in",
            vec![p("AssetId.1", ASSETS[0])],
            "InvalidParameterValue",
        ),
        (
            "an empty Outpost ARN, which names no Outpost either",
            vec![p("OutpostArn", ""), p("Quantity", "1")],
            "InvalidParameterValue",
        ),
        (
            "an empty hardware asset id beside a real one",
            vec![
                p("OutpostArn", an_outpost()),
                p("AssetId.1", ASSETS[0]),
                p("AssetId.2", ""),
            ],
            "InvalidParameterValue",
        ),
        (
            "the same hardware asset named twice",
            vec![
                p("OutpostArn", an_outpost()),
                p("AssetId.1", ASSETS[0]),
                p("AssetId.2", ASSETS[0]),
            ],
            "InvalidParameterValue",
        ),
        (
            "an Outpost ARN one character longer than this service resolves",
            vec![
                p("OutpostArn", padded_outpost_arn(MAX_OUTPOST_ARN_CHARS + 1)),
                p("Quantity", "1"),
            ],
            "InvalidParameterValue",
        ),
        (
            "a quantity above the number of assets named",
            vec![
                p("OutpostArn", an_outpost()),
                p("AssetId.1", ASSETS[0]),
                p("Quantity", "2"),
            ],
            "InvalidParameterCombination",
        ),
        (
            "a quantity below the number of assets named",
            vec![
                p("OutpostArn", an_outpost()),
                p("AssetId.1", ASSETS[0]),
                p("AssetId.2", ASSETS[1]),
                p("Quantity", "1"),
            ],
            "InvalidParameterCombination",
        ),
    ];

    for (case, extra, expected) in &rows {
        let mut params = vec![
            p("InstanceType", INSTANCE_TYPE),
            p("AvailabilityZone", zone_name()),
        ];
        params.extend(extra.clone());
        let (status, body) = allocate(&router, &params).await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, *expected),
            "case {case:?}: expected HTTP 400 {expected}, got {status} with body {body}"
        );
    }

    // Nothing was allocated by any of those refusals: a request EC2 refused leaves no host
    // behind for a later describe to find.
    let (status, body) = call(&router, REGION, "DescribeHosts", &[]).await;
    assert_eq!(
        status, 200,
        "DescribeHosts must answer, got {status} with body {body}"
    );
    assert!(
        allocated_ids(&body).is_empty() && elements(&body, "hostId").is_empty(),
        "a refused allocation must store no host, got {body}"
    );
}
