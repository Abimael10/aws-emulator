// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for `CreateIpamResourceDiscovery`, driven over the wire the service
//! actually speaks: `ec2Query` — an `application/x-www-form-urlencoded` POST whose body
//! carries `Action=…`, answered with XML (README.md / OPERATIONS.md "Wire protocol").
//!
//! The rules under test:
//!
//! * the create STORES one resource discovery carrying exactly what the request described —
//!   its description, its tags and the operating Regions it names together with the Region the
//!   call was signed for, kept as a SET — and derives every member the service owns: the id,
//!   the ARN that id gives, the owning account, the Region the discovery lives in, the fact
//!   that a discovery a caller asked for is NOT an IPAM's default one ("The default resource
//!   discovery is the resource discovery automatically created when you create an IPAM",
//!   `IpamResourceDiscovery$IsDefault`), the empty organizational-unit exclusion set and the
//!   non-terminal lifecycle state; and the stored record is what
//!   `DescribeIpamResourceDiscoveries` then reports;
//! * the discovery is answered in `create-in-progress` and the clock owes it `create-complete`
//!   whether or not anybody calls again;
//! * a request the documented contract does not admit is refused with the code naming what was
//!   wrong, and NOTHING is stored — which a `DryRun` ("If you have the required permissions,
//!   the error response is DryRunOperation") shares;
//! * one `ClientToken` names the one discovery it minted: a replay answers that discovery and
//!   stores nothing further, and a replay asking for anything else is refused.
//!
//! Each table row is a distinct input along what those generalise over: whether the request
//! carries a description, how many operating Regions it names and whether they repeat or
//! include the signing Region, how many tags it carries, which Region the call is signed for,
//! and which member of a refused request is at fault and in what way.

use std::collections::BTreeSet;
use std::time::Duration;

use ec2::state::{
    DRY_RUN_OPERATION, IDEMPOTENT_PARAMETER_MISMATCH, INVALID_PARAMETER_VALUE,
    IPAM_RESOURCE_DISCOVERY_ID_PREFIX, IPAM_STATE_CREATE_COMPLETE, IPAM_STATE_CREATE_IN_PROGRESS,
    MAX_CLIENT_TOKEN_CHARS, MISSING_PARAMETER, RESOURCE_TYPE_IPAM_RESOURCE_DISCOVERY,
    TRANSITION_DELAY,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, error_code};
use super::wire::{child, child_text, document, is_long_id, param, set_items, Param};

/// The Region most calls here are signed for, which is therefore the Region the discovery they
/// create lives in.
const REGION: &str = "us-east-1";

/// The action under test.
const ACTION: &str = "CreateIpamResourceDiscovery";

/// The emulator the tests drive, as [`super::ec2_query::emulator`] builds it: the router and
/// the engine its store lives in.
fn emulator() -> (MultiServiceRouter, std::sync::Arc<engine::InMemoryEngine>) {
    super::ec2_query::emulator()
}

/// One call of `action` signed for `region`.
async fn invoke_in(
    router: &MultiServiceRouter,
    region: &str,
    action: &str,
    params: &[Param],
) -> (u16, String) {
    call(router, region, action, params).await
}

/// One call of `action` requiring success, and the body it answered.
async fn ok_in(
    case: &str,
    router: &MultiServiceRouter,
    region: &str,
    action: &str,
    params: &[Param],
) -> String {
    let (status, body) = invoke_in(router, region, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    body
}

/// The form fields for an `OperatingRegions` list of `regions` — `OperatingRegion` is the name
/// the member is flattened under on the wire
/// (`CreateIpamResourceDiscoveryRequest$OperatingRegions` carries `xmlName: OperatingRegion`).
fn operating_region_params(regions: &[&str]) -> Vec<Param> {
    regions
        .iter()
        .enumerate()
        .map(|(index, region)| param(&format!("OperatingRegion.{}.RegionName", index + 1), region))
        .collect()
}

/// The form fields for one `ipam-resource-discovery` tag specification.
fn tag_params(tags: &[(&str, &str)]) -> Vec<Param> {
    if tags.is_empty() {
        return Vec::new();
    }
    let mut params = vec![param(
        "TagSpecification.1.ResourceType",
        RESOURCE_TYPE_IPAM_RESOURCE_DISCOVERY,
    )];
    for (index, (key, value)) in tags.iter().enumerate() {
        params.push(param(
            &format!("TagSpecification.1.Tag.{}.Key", index + 1),
            key,
        ));
        params.push(param(
            &format!("TagSpecification.1.Tag.{}.Value", index + 1),
            value,
        ));
    }
    params
}

/// One resource discovery as a response reports it: every member read off the wire, with the
/// nested lists flattened into comparable values.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ReportedDiscovery {
    owner_id: Option<String>,
    id: Option<String>,
    arn: Option<String>,
    region: Option<String>,
    description: Option<String>,
    /// The `regionName`s of `operatingRegionSet`, sorted: the Regions are a SET, so the order
    /// they are reported in is not part of the rule, but how many times each appears is.
    operating_regions: Vec<String>,
    is_default: Option<String>,
    state: Option<String>,
    tags: Vec<(String, String)>,
    organizational_unit_exclusions: Vec<String>,
}

/// One discovery element, read off the wire.
fn read_discovery(node: roxmltree::Node<'_, '_>) -> ReportedDiscovery {
    let mut operating_regions: Vec<String> = set_items(node, "operatingRegionSet")
        .into_iter()
        .filter_map(|region| child_text(region, "regionName"))
        .collect();
    operating_regions.sort();
    ReportedDiscovery {
        owner_id: child_text(node, "ownerId"),
        id: child_text(node, "ipamResourceDiscoveryId"),
        arn: child_text(node, "ipamResourceDiscoveryArn"),
        region: child_text(node, "ipamResourceDiscoveryRegion"),
        description: child_text(node, "description"),
        operating_regions,
        is_default: child_text(node, "isDefault"),
        state: child_text(node, "state"),
        tags: set_items(node, "tagSet")
            .into_iter()
            .map(|tag| {
                (
                    child_text(tag, "key").unwrap_or_default(),
                    child_text(tag, "value").unwrap_or_default(),
                )
            })
            .collect(),
        organizational_unit_exclusions: set_items(node, "organizationalUnitExclusionSet")
            .into_iter()
            .filter_map(|exclusion| child_text(exclusion, "organizationsEntityPath"))
            .collect(),
    }
}

/// The one discovery a `CreateIpamResourceDiscovery` response carries.
fn created(case: &str, body: &str) -> ReportedDiscovery {
    let parsed = document(case, body);
    let node = parsed
        .descendants()
        .find(|node| node.is_element() && child(*node, "ipamResourceDiscoveryId").is_some())
        .unwrap_or_else(|| {
            panic!("case {case:?}: no discovery (no <ipamResourceDiscoveryId>) in {body}")
        });
    read_discovery(node)
}

/// The one discovery a `DescribeIpamResourceDiscoveries` narrowed to `discovery_id` reports.
async fn described(
    case: &str,
    router: &MultiServiceRouter,
    region: &str,
    discovery_id: &str,
) -> ReportedDiscovery {
    let body = ok_in(
        case,
        router,
        region,
        "DescribeIpamResourceDiscoveries",
        &[param("IpamResourceDiscoveryId.1", discovery_id)],
    )
    .await;
    let parsed = document(case, &body);
    let items = set_items(parsed.root_element(), "ipamResourceDiscoverySet");
    assert_eq!(
        items.len(),
        1,
        "case {case:?}: expected exactly one resource discovery for {discovery_id}, got {body}"
    );
    read_discovery(items[0])
}

/// The state `DescribeIpamResourceDiscoveries` reports for `discovery_id` once it is `wanted`:
/// the state is looked at AGAIN in a loop until it has moved or a deadline far past
/// [`TRANSITION_DELAY`] passes, so the wait is for the CONDITION rather than for a span of the
/// clock and a loaded host cannot turn "not yet" into a failure.
async fn described_state_once(
    case: &str,
    router: &MultiServiceRouter,
    discovery_id: &str,
    wanted: &str,
) -> String {
    let deadline = std::time::Instant::now() + TRANSITION_DELAY * 40;
    loop {
        let state = described(case, router, REGION, discovery_id)
            .await
            .state
            .unwrap_or_default();
        if state == wanted || std::time::Instant::now() >= deadline {
            break state;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// The ids of every resource discovery the store holds, which is what proves a refusal wrote
/// nothing and that a create wrote exactly one record.
fn stored_ids(engine: &engine::InMemoryEngine) -> BTreeSet<String> {
    let state = ec2::state::service_state(engine).expect("the EC2 store");
    let store = state.lock();
    store.ipam_resource_discoveries.keys().cloned().collect()
}

/// The ARN the service derives for the discovery `discovery_id` names.
fn expected_arn(discovery_id: &str) -> String {
    engine::emulator_arn(
        "ec2",
        "",
        &format!("{RESOURCE_TYPE_IPAM_RESOURCE_DISCOVERY}/{discovery_id}"),
    )
}

// ---------------------------------------------------------------------------
// What the create stores
// ---------------------------------------------------------------------------

/// One row of the create table.
struct CreateCase {
    case: &'static str,
    /// The Region the call is signed for, which is the Region the discovery lives in.
    region: &'static str,
    description: Option<&'static str>,
    /// The Region names the request asks the discovery to operate in.
    requested_regions: Vec<&'static str>,
    tags: Vec<(&'static str, &'static str)>,
}

/// The rule: `CreateIpamResourceDiscovery` stores one resource discovery carrying exactly what
/// the request described — its description, its tags, and the operating Regions it names as a
/// SET together with the Region the call was signed for — with every member the service owns
/// derived rather than echoed, and the stored record is what the describe then reports.
///
/// The rows differ along what that generalises over: whether a description is given, how many
/// operating Regions the request names and whether they repeat or already include the signing
/// Region, how many tags it carries, and which Region the call is signed for.
#[tokio::test]
async fn create_ipam_resource_discovery_stores_what_the_request_described() {
    let (router, engine) = emulator();
    let account = engine::emulator_account_id().to_string();

    let cases = [
        CreateCase {
            case: "nothing but the action",
            region: REGION,
            description: None,
            requested_regions: Vec::new(),
            tags: Vec::new(),
        },
        CreateCase {
            case: "a description and no operating Region",
            region: REGION,
            description: Some("corp discovery"),
            requested_regions: Vec::new(),
            tags: Vec::new(),
        },
        CreateCase {
            case: "one operating Region beyond the signing Region",
            region: REGION,
            description: None,
            requested_regions: vec!["eu-west-1"],
            tags: Vec::new(),
        },
        CreateCase {
            case: "the signing Region named explicitly",
            region: REGION,
            description: None,
            requested_regions: vec![REGION],
            tags: Vec::new(),
        },
        CreateCase {
            case: "one operating Region named twice",
            region: REGION,
            description: None,
            requested_regions: vec!["eu-west-1", "eu-west-1"],
            tags: vec![("Name", "repeated-region")],
        },
        CreateCase {
            case: "a description, three operating Regions and three tags",
            region: REGION,
            description: Some("global discovery"),
            requested_regions: vec!["eu-west-1", "ap-south-1", "us-west-2"],
            tags: vec![("Name", "wide"), ("Team", "network"), ("Env", "prod")],
        },
        CreateCase {
            case: "a call signed for another Region",
            region: "eu-central-1",
            description: Some("frankfurt discovery"),
            requested_regions: vec!["us-west-2"],
            tags: Vec::new(),
        },
    ];

    let mut minted: BTreeSet<String> = BTreeSet::new();
    for row in &cases {
        let case = row.case;
        let mut params: Vec<Param> = Vec::new();
        if let Some(description) = row.description {
            params.push(param("Description", description));
        }
        params.extend(operating_region_params(&row.requested_regions));
        params.extend(tag_params(&row.tags));

        let body = ok_in(case, &router, row.region, ACTION, &params).await;
        let discovery = created(case, &body);
        let discovery_id = discovery
            .id
            .clone()
            .unwrap_or_else(|| panic!("case {case:?}: the discovery has no id: {body}"));

        // What the service owns, derived rather than echoed.
        assert!(
            is_long_id(IPAM_RESOURCE_DISCOVERY_ID_PREFIX, &discovery_id),
            "case {case:?}: expected a minted {IPAM_RESOURCE_DISCOVERY_ID_PREFIX} id, \
             got {discovery_id:?}"
        );
        assert!(
            minted.insert(discovery_id.clone()),
            "case {case:?}: expected an id no earlier call was given, got {discovery_id:?}"
        );
        assert_eq!(
            discovery.arn.as_deref(),
            Some(expected_arn(&discovery_id).as_str()),
            "case {case:?}: the discovery reports the ARN its own id gives, got {discovery:?}"
        );
        assert_eq!(
            discovery.owner_id.as_deref(),
            Some(account.as_str()),
            "case {case:?}: the discovery is owned by this account, got {discovery:?}"
        );
        assert_eq!(
            discovery.region.as_deref(),
            Some(row.region),
            "case {case:?}: the discovery lives in the Region the call was signed for, \
             got {discovery:?}"
        );
        assert_eq!(
            discovery.is_default.as_deref(),
            Some("false"),
            "case {case:?}: a discovery a caller asked for is not an IPAM's default one, \
             got {discovery:?}"
        );
        assert_eq!(
            discovery.state.as_deref(),
            Some(IPAM_STATE_CREATE_IN_PROGRESS),
            "case {case:?}: the discovery is answered while it is still being created, \
             got {discovery:?}"
        );
        assert!(
            discovery.organizational_unit_exclusions.is_empty(),
            "case {case:?}: a create excludes no organizational unit, got {discovery:?}"
        );

        // What the request decided, carried through.
        assert_eq!(
            discovery.description.as_deref(),
            row.description,
            "case {case:?}: the discovery carries the description the request named, \
             got {discovery:?}"
        );
        let mut wanted_regions: Vec<String> = vec![row.region.to_string()];
        for region in &row.requested_regions {
            if !wanted_regions.iter().any(|held| held == region) {
                wanted_regions.push((*region).to_string());
            }
        }
        wanted_regions.sort();
        assert_eq!(
            discovery.operating_regions, wanted_regions,
            "case {case:?}: the discovery operates in the Regions the request named together \
             with its own, each once, got {discovery:?}"
        );
        let wanted_tags: Vec<(String, String)> = row
            .tags
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect();
        assert_eq!(
            discovery.tags, wanted_tags,
            "case {case:?}: the discovery carries the tags the request asked for, \
             got {discovery:?}"
        );

        // The round trip: the describe reports the record the create stored. Only the
        // lifecycle state may differ, because the clock may already have moved it.
        let mut reported = described(case, &router, row.region, &discovery_id).await;
        let settled = reported.state.take();
        let mut answered = discovery.clone();
        answered.state = None;
        assert_eq!(
            reported, answered,
            "case {case:?}: the describe reports the discovery the create stored, \
             got {reported:?}"
        );
        assert!(
            settled.as_deref() == Some(IPAM_STATE_CREATE_IN_PROGRESS)
                || settled.as_deref() == Some(IPAM_STATE_CREATE_COMPLETE),
            "case {case:?}: the described discovery is being created or is created, \
             got {settled:?}"
        );
    }

    // Exactly one record per row, and nothing besides: no IPAM was created here, so every
    // discovery the store holds is one a row asked for.
    assert_eq!(
        stored_ids(&engine),
        minted,
        "the store holds exactly the discoveries the rows created"
    );
}

/// The rule: a discovery `CreateIpamResourceDiscovery` answers is in the non-terminal
/// `create-in-progress` state, and the clock owes it `create-complete` whether or not anybody
/// calls again — so a caller acting straight after the call still sees the transient state.
#[tokio::test]
async fn a_created_discovery_completes_on_its_own() {
    let case = "the clock completes the discovery";
    let (router, _engine) = emulator();

    let body = ok_in(case, &router, REGION, ACTION, &[]).await;
    let discovery = created(case, &body);
    let discovery_id = discovery.id.clone().expect("the discovery has an id");
    assert_eq!(
        discovery.state.as_deref(),
        Some(IPAM_STATE_CREATE_IN_PROGRESS),
        "case {case:?}: a caller acting straight after the call sees the transient state, \
         got {discovery:?}"
    );

    let settled = described_state_once(case, &router, &discovery_id, IPAM_STATE_CREATE_COMPLETE)
        .await;
    assert_eq!(
        settled, IPAM_STATE_CREATE_COMPLETE,
        "case {case:?}: the discovery completes once the transition is due, got {settled:?}"
    );
}

// ---------------------------------------------------------------------------
// What the create refuses
// ---------------------------------------------------------------------------

/// The rule: a request the documented contract does not admit is refused with the code naming
/// what was wrong — `InvalidParameterValue` for a value outside what the member admits,
/// `MissingParameter` for a member given without the one value it needs, and
/// `DryRunOperation` for a permission check — and NOTHING is stored.
#[tokio::test]
async fn create_ipam_resource_discovery_refuses_what_the_contract_does_not_admit() {
    let (router, engine) = emulator();

    // (case, form fields, expected error code, expected HTTP status)
    let cases: Vec<(&str, Vec<Param>, &str, u16)> = vec![
        (
            "an operating Region AWS does not publish",
            operating_region_params(&["mars-north-1"]),
            INVALID_PARAMETER_VALUE,
            400,
        ),
        (
            "an operating Region AWS publishes beside one it does not",
            operating_region_params(&["eu-west-1", "atlantis-1"]),
            INVALID_PARAMETER_VALUE,
            400,
        ),
        (
            "an operating Region entry with no Region name",
            vec![param("OperatingRegion.1.RegionName", "")],
            MISSING_PARAMETER,
            400,
        ),
        (
            "a tag specification naming another resource type",
            vec![
                param("TagSpecification.1.ResourceType", "ipam"),
                param("TagSpecification.1.Tag.1.Key", "Name"),
                param("TagSpecification.1.Tag.1.Value", "wrong-type"),
            ],
            INVALID_PARAMETER_VALUE,
            400,
        ),
        (
            "a tag specification with no resource type",
            vec![
                param("TagSpecification.1.Tag.1.Key", "Name"),
                param("TagSpecification.1.Tag.1.Value", "no-type"),
            ],
            MISSING_PARAMETER,
            400,
        ),
        (
            "a tag key in the reserved namespace",
            tag_params(&[("aws:owner", "reserved")]),
            INVALID_PARAMETER_VALUE,
            400,
        ),
        (
            "one tag key given twice",
            {
                let mut params = tag_params(&[("Name", "first")]);
                params.push(param("TagSpecification.1.Tag.2.Key", "Name"));
                params.push(param("TagSpecification.1.Tag.2.Value", "second"));
                params
            },
            INVALID_PARAMETER_VALUE,
            400,
        ),
        (
            "a client token one character past the bound",
            vec![param("ClientToken", &"t".repeat(MAX_CLIENT_TOKEN_CHARS + 1))],
            INVALID_PARAMETER_VALUE,
            400,
        ),
        (
            "a permission check for a request that would have succeeded",
            vec![param("DryRun", "true")],
            DRY_RUN_OPERATION,
            412,
        ),
        (
            "a permission check carrying everything a create takes",
            {
                let mut params = vec![
                    param("DryRun", "true"),
                    param("Description", "never stored"),
                    param("ClientToken", "dry-run-token"),
                ];
                params.extend(operating_region_params(&["eu-west-1"]));
                params.extend(tag_params(&[("Name", "never-stored")]));
                params
            },
            DRY_RUN_OPERATION,
            412,
        ),
    ];

    for (case, params, code, status) in &cases {
        let (answered, body) = invoke_in(&router, REGION, ACTION, params).await;
        assert_eq!(
            answered, *status,
            "case {case:?}: expected HTTP {status}, got {answered} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            *code,
            "case {case:?}: expected the error code {code}, got body {body}"
        );
        assert!(
            stored_ids(&engine).is_empty(),
            "case {case:?}: a refused create stores nothing, got {:?}",
            stored_ids(&engine)
        );
    }

    // A good request after every refusal still works, so none of them left the store in a
    // state that blocks one.
    let body = ok_in("the good create after the refusals", &router, REGION, ACTION, &[]).await;
    let discovery_id = created("the good create after the refusals", &body)
        .id
        .expect("the discovery has an id");
    assert_eq!(
        stored_ids(&engine),
        BTreeSet::from([discovery_id.clone()]),
        "the one good create is the only discovery the store holds"
    );
}

// ---------------------------------------------------------------------------
// What one client token names
// ---------------------------------------------------------------------------

/// The rule: one `ClientToken` names the one resource discovery it minted — a replay of the
/// same request answers that discovery and stores nothing further, a replay asking for
/// anything else is refused with `IdempotentParameterMismatch`, and a different token mints a
/// discovery of its own.
#[tokio::test]
async fn one_client_token_names_one_discovery() {
    let case = "the first create";
    let (router, engine) = emulator();

    let first_request = |token: &str| -> Vec<Param> {
        let mut params = vec![
            param("ClientToken", token),
            param("Description", "token discovery"),
        ];
        params.extend(operating_region_params(&["eu-west-1"]));
        params.extend(tag_params(&[("Name", "token-discovery")]));
        params
    };

    let body = ok_in(case, &router, REGION, ACTION, &first_request("token-one")).await;
    let first = created(case, &body);
    let discovery_id = first.id.clone().expect("the discovery has an id");

    // A replay answers the discovery that token made, exactly as it stands now.
    let replay_case = "the same request replayed";
    let replayed_body =
        ok_in(replay_case, &router, REGION, ACTION, &first_request("token-one")).await;
    let replayed = created(replay_case, &replayed_body);
    assert_eq!(
        replayed.id.as_deref(),
        Some(discovery_id.as_str()),
        "case {replay_case:?}: the replay answers the discovery the token made, \
         got {replayed:?}"
    );
    assert_eq!(
        stored_ids(&engine),
        BTreeSet::from([discovery_id.clone()]),
        "case {replay_case:?}: the replay stored nothing further"
    );

    // A replay asking for anything else names a discovery the caller did not ask for.
    let mismatches: Vec<(&str, Vec<Param>)> = vec![
        (
            "the same token with another description",
            {
                let mut params = first_request("token-one");
                params[1] = param("Description", "another description");
                params
            },
        ),
        (
            "the same token with no description at all",
            {
                let mut params = first_request("token-one");
                params.remove(1);
                params
            },
        ),
        (
            "the same token with another operating Region",
            {
                let mut params = vec![
                    param("ClientToken", "token-one"),
                    param("Description", "token discovery"),
                ];
                params.extend(operating_region_params(&["ap-south-1"]));
                params.extend(tag_params(&[("Name", "token-discovery")]));
                params
            },
        ),
        (
            "the same token with another tag value",
            {
                let mut params = vec![
                    param("ClientToken", "token-one"),
                    param("Description", "token discovery"),
                ];
                params.extend(operating_region_params(&["eu-west-1"]));
                params.extend(tag_params(&[("Name", "something-else")]));
                params
            },
        ),
        (
            "the same token with no tag at all",
            {
                let mut params = vec![
                    param("ClientToken", "token-one"),
                    param("Description", "token discovery"),
                ];
                params.extend(operating_region_params(&["eu-west-1"]));
                params
            },
        ),
    ];
    for (mismatch_case, params) in &mismatches {
        let (status, body) = invoke_in(&router, REGION, ACTION, params).await;
        assert_eq!(
            status, 400,
            "case {mismatch_case:?}: expected HTTP 400, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            IDEMPOTENT_PARAMETER_MISMATCH,
            "case {mismatch_case:?}: expected {IDEMPOTENT_PARAMETER_MISMATCH}, got body {body}"
        );
        assert_eq!(
            stored_ids(&engine),
            BTreeSet::from([discovery_id.clone()]),
            "case {mismatch_case:?}: a refused replay stores nothing"
        );
    }

    // The same body under a different token is a different request, so it mints its own
    // discovery.
    let second_case = "another token, the same body";
    let second_body =
        ok_in(second_case, &router, REGION, ACTION, &first_request("token-two")).await;
    let second = created(second_case, &second_body)
        .id
        .expect("the second discovery has an id");
    assert_ne!(
        second, discovery_id,
        "case {second_case:?}: a different token mints a discovery of its own"
    );
    assert_eq!(
        stored_ids(&engine),
        BTreeSet::from([discovery_id, second]),
        "case {second_case:?}: the store holds one discovery per token"
    );
}
