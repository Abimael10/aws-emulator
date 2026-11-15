// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for `ModifyIpamPool`, driven over the wire the service actually speaks:
//! `ec2Query` — an `application/x-www-form-urlencoded` POST whose body carries `Action=…`,
//! answered with XML (README.md / OPERATIONS.md "Wire protocol").
//!
//! The rules under test:
//!
//! * a modification WRITES the members the request carries onto the stored pool and leaves
//!   every other member exactly as it stood — nothing the pool's creation decided moves —
//!   and the pool it answers is the pool a later `DescribeIpamPools` reports;
//! * the allocation resource tags are a SET keyed by the tag key: an added key the pool
//!   already requires replaces that rule's value, a removed key is dropped, and a key the
//!   pool does not require is removed without effect;
//! * the pool the modification LEAVES BEHIND is held to the same documented allocation rules
//!   a create holds a new pool to — the netmask range of the pool's own address family, a
//!   minimum below the maximum, a default inside both — read against the values the pool
//!   already holds for whatever the request leaves out;
//! * a pool id that is not one of this resource's at all is refused with its `Malformed`
//!   code and a well-formed one the account holds no pool for with its `NotFound` code, two
//!   members asking incompatible things of one rule with `InvalidParameterCombination`, a
//!   tag set past the documented ceiling with `TagLimitExceeded`, a pool a delete has
//!   already acted on with `IncorrectState`, and a `DryRun` reports the permission outcome
//!   without writing;
//! * every refusal leaves the stored pool untouched.
//!
//! Each table row is a distinct input along what those generalise over: which member the
//! request carries, how many it carries at once, which address family the pool holds, which
//! of the two tag lists is used, and which member is at fault.

use std::collections::BTreeMap;

use ec2::state::{
    ADDRESS_FAMILY_IPV4, ADDRESS_FAMILY_IPV6, DRY_RUN_OPERATION, DRY_RUN_OPERATION_STATUS,
    INCORRECT_STATE, INVALID_IPAM_POOL_ID_MALFORMED, INVALID_IPAM_POOL_ID_NOT_FOUND,
    INVALID_PARAMETER_COMBINATION, INVALID_PARAMETER_VALUE, IPAM_POOL_STATE_CREATE_COMPLETE,
    IPAM_POOL_STATE_CREATE_IN_PROGRESS, MAX_TAGS_PER_RESOURCE,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, error_code, error_message};
use super::wire::{Param, child, child_text, document, param, set_items};

/// The Region every call here is signed for, which is therefore each IPAM's home Region.
const REGION: &str = "us-east-1";

/// One call of `action` signed for the Region above.
async fn invoke(router: &MultiServiceRouter, action: &str, params: &[Param]) -> (u16, String) {
    call(router, REGION, action, params).await
}

/// One call of `action` requiring success, and the body it answered.
async fn ok(case: &str, router: &MultiServiceRouter, action: &str, params: &[Param]) -> String {
    let (status, body) = invoke(router, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    body
}

/// The two default scopes of the IPAM a suite creates — "an IPAM contains two default
/// scopes" (`IpamScope`) — which is what a pool is created in.
struct CreatedIpam {
    /// The IPAM's default private scope, where every pool of this suite lives.
    private_scope: String,
}

/// Create one IPAM through the operation that owns it and answer what its pools are created
/// in.
async fn create_ipam(case: &str, router: &MultiServiceRouter) -> CreatedIpam {
    let body = ok(case, router, "CreateIpam", &[]).await;
    let parsed = document(case, &body);
    let ipam = parsed
        .descendants()
        .find(|node| node.is_element() && child(*node, "ipamId").is_some())
        .unwrap_or_else(|| panic!("case {case:?}: no IPAM (no <ipamId>) in {body}"));
    CreatedIpam {
        private_scope: child_text(ipam, "privateDefaultScopeId").unwrap_or_else(|| {
            panic!("case {case:?}: no <privateDefaultScopeId> on the IPAM in {body}")
        }),
    }
}

/// One IPAM pool as a response reports it, read generically so a member this suite never
/// names is still compared: every SCALAR member of the pool element, plus its two tag sets.
///
/// `state` is deliberately not one of the members: the clock owes a freshly created pool its
/// move to [`IPAM_POOL_STATE_CREATE_COMPLETE`], so it is asserted on its own
/// ([`Pool::assert_creating`]) rather than compared between two reads.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Pool {
    /// Every scalar child of the pool element except `state`, by element name.
    members: BTreeMap<String, String>,
    /// The pool's own `state`, whichever of the two create states it is in.
    state: String,
    /// `(key, value)` per entry of `allocationResourceTagSet`, sorted so two reports compare
    /// by content.
    allocation_resource_tags: Vec<(String, String)>,
    /// The same for the pool's own `tagSet`.
    tags: Vec<(String, String)>,
}

impl Pool {
    /// A modification is not a transition, so the pool is still in one of the two states a
    /// CREATE leaves it in: the `create-in-progress` it started in, or the `create-complete`
    /// the clock owed it by the time this read happened.
    fn assert_creating(&self, case: &str) {
        assert!(
            self.state == IPAM_POOL_STATE_CREATE_IN_PROGRESS
                || self.state == IPAM_POOL_STATE_CREATE_COMPLETE,
            "case {case:?}: expected the pool still in one of the create states, got {:?}",
            self.state
        );
    }
}

/// The `(key, value)` pairs of the `name` tag set of `node`, sorted.
fn tag_pairs(node: roxmltree::Node<'_, '_>, name: &str) -> Vec<(String, String)> {
    let mut pairs: Vec<(String, String)> = set_items(node, name)
        .into_iter()
        .map(|item| {
            (
                child_text(item, "key").unwrap_or_default(),
                child_text(item, "value").unwrap_or_default(),
            )
        })
        .collect();
    pairs.sort();
    pairs
}

/// The one IPAM pool a response carries — the element that has an `ipamPoolId` child.
fn pool(case: &str, body: &str) -> Pool {
    let parsed = document(case, body);
    let node = parsed
        .descendants()
        .find(|node| node.is_element() && child(*node, "ipamPoolId").is_some())
        .unwrap_or_else(|| panic!("case {case:?}: no IPAM pool (no <ipamPoolId>) in {body}"));
    let members = node
        .children()
        .filter(|member| {
            member.is_element() && !member.children().any(|inner| inner.is_element())
        })
        .filter(|member| member.tag_name().name() != "state")
        .map(|member| {
            (
                member.tag_name().name().to_string(),
                member.text().unwrap_or_default().to_string(),
            )
        })
        .collect();
    Pool {
        members,
        state: child_text(node, "state").unwrap_or_default(),
        allocation_resource_tags: tag_pairs(node, "allocationResourceTagSet"),
        tags: tag_pairs(node, "tagSet"),
    }
}

/// The form fields for an allocation resource tag list, under the member `prefix` the
/// operation names it by (`AllocationResourceTag`, `AddAllocationResourceTag`,
/// `RemoveAllocationResourceTag` — the `xmlName` of each list member).
fn allocation_tags(prefix: &str, tags: &[(&str, &str)]) -> Vec<Param> {
    let mut params = Vec::new();
    for (index, (key, value)) in tags.iter().enumerate() {
        params.push(param(&format!("{prefix}.{}.Key", index + 1), key));
        params.push(param(&format!("{prefix}.{}.Value", index + 1), value));
    }
    params
}

/// The allocation resource tags every baseline pool of this suite requires, so a row that
/// adds or removes one acts on a set that already holds rules.
const BASELINE_TAGS: [(&str, &str); 2] = [("Owner", "TeamA"), ("Stage", "prod")];

/// Create one pool of `family` carrying every member a modification can change, and answer
/// the `IpamPoolId` the service minted for it.
///
/// Every member is set at creation so a row that names one is CHANGING a value rather than
/// setting it for the first time, and a row that names none has something to leave alone.
async fn create_baseline_pool(
    case: &str,
    router: &MultiServiceRouter,
    ipam: &CreatedIpam,
    family: &str,
) -> String {
    let mut params = vec![
        param("IpamScopeId", &ipam.private_scope),
        param("AddressFamily", family),
        param("Description", "the baseline pool"),
        param("AutoImport", "false"),
        param("AllocationMinNetmaskLength", "16"),
        param("AllocationMaxNetmaskLength", "28"),
        param("AllocationDefaultNetmaskLength", "24"),
    ];
    params.extend(allocation_tags("AllocationResourceTag", &BASELINE_TAGS));
    let body = ok(case, router, "CreateIpamPool", &params).await;
    pool(case, &body)
        .members
        .get("ipamPoolId")
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: the created pool reports no id: {body}"))
}

/// Create a pool with no allocation resource tags at all, for the rows that drive the tag
/// ceiling from an empty set.
async fn create_untagged_pool(case: &str, router: &MultiServiceRouter, ipam: &CreatedIpam) -> String {
    let body = ok(
        case,
        router,
        "CreateIpamPool",
        &[
            param("IpamScopeId", &ipam.private_scope),
            param("AddressFamily", ADDRESS_FAMILY_IPV4),
        ],
    )
    .await;
    pool(case, &body)
        .members
        .get("ipamPoolId")
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: the created pool reports no id: {body}"))
}

/// The pool `pool_id` as `DescribeIpamPools` reports it — the reader that says what a LATER
/// caller sees.
async fn described(case: &str, router: &MultiServiceRouter, pool_id: &str) -> Pool {
    let body = ok(
        case,
        router,
        "DescribeIpamPools",
        &[param("IpamPoolId.1", pool_id)],
    )
    .await;
    pool(case, &body)
}

/// `n` distinct allocation resource tag rules, so a row can drive the documented ceiling
/// from the constant rather than from a number written here.
fn many_tags(n: usize) -> Vec<(String, String)> {
    (0..n)
        .map(|nth| (format!("key-{nth}"), format!("value-{nth}")))
        .collect()
}

/// The form fields for [`many_tags`] under `prefix`.
fn many_tag_params(prefix: &str, tags: &[(String, String)]) -> Vec<Param> {
    let borrowed: Vec<(&str, &str)> = tags
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    allocation_tags(prefix, &borrowed)
}

// ---------------------------------------------------------------------------
// What a modification writes
// ---------------------------------------------------------------------------

/// One row of the modification table: a request, and what the pool must report afterwards.
struct Applied {
    /// What makes this row different from the ones beside it.
    case: &'static str,
    /// The address family the pool being modified holds.
    family: &'static str,
    /// The request members beyond the pool id.
    params: Vec<Param>,
    /// `(element, text)` the pool must report afterwards — `None` for a member it must not
    /// report at all any more. Every member not named here must be exactly as it was.
    changed: Vec<(&'static str, Option<&'static str>)>,
    /// The allocation resource tags the pool must require afterwards, or `None` when the row
    /// leaves them as the baseline set.
    tags_after: Option<Vec<(&'static str, &'static str)>>,
}

/// The rule: a modification writes the members the request carries onto the stored pool,
/// leaves every other member exactly as it stood, and answers the pool a later
/// `DescribeIpamPools` reports.
#[tokio::test]
async fn a_modification_writes_the_members_it_carries_and_leaves_the_rest() {
    let rows: Vec<Applied> = vec![
        Applied {
            case: "a description on its own",
            family: ADDRESS_FAMILY_IPV4,
            params: vec![param("Description", "the production network")],
            changed: vec![("description", Some("the production network"))],
            tags_after: None,
        },
        Applied {
            case: "auto-import turned on",
            family: ADDRESS_FAMILY_IPV4,
            params: vec![param("AutoImport", "true")],
            changed: vec![("autoImport", Some("true"))],
            tags_after: None,
        },
        Applied {
            case: "both netmask bounds widened at once",
            family: ADDRESS_FAMILY_IPV4,
            params: vec![
                param("AllocationMinNetmaskLength", "12"),
                param("AllocationMaxNetmaskLength", "30"),
            ],
            changed: vec![
                ("allocationMinNetmaskLength", Some("12")),
                ("allocationMaxNetmaskLength", Some("30")),
            ],
            tags_after: None,
        },
        Applied {
            case: "only the minimum, read against the maximum the pool already holds",
            family: ADDRESS_FAMILY_IPV4,
            params: vec![param("AllocationMinNetmaskLength", "20")],
            changed: vec![("allocationMinNetmaskLength", Some("20"))],
            tags_after: None,
        },
        Applied {
            case: "the default netmask rule moved inside the bounds the pool holds",
            family: ADDRESS_FAMILY_IPV4,
            params: vec![param("AllocationDefaultNetmaskLength", "26")],
            changed: vec![("allocationDefaultNetmaskLength", Some("26"))],
            tags_after: None,
        },
        Applied {
            case: "the default netmask rule cleared",
            family: ADDRESS_FAMILY_IPV4,
            params: vec![param("ClearAllocationDefaultNetmaskLength", "true")],
            changed: vec![("allocationDefaultNetmaskLength", None)],
            tags_after: None,
        },
        Applied {
            case: "netmask rules an IPv6 pool admits and an IPv4 pool does not",
            family: ADDRESS_FAMILY_IPV6,
            params: vec![
                param("AllocationMinNetmaskLength", "48"),
                param("AllocationMaxNetmaskLength", "64"),
                param("AllocationDefaultNetmaskLength", "56"),
            ],
            changed: vec![
                ("allocationMinNetmaskLength", Some("48")),
                ("allocationMaxNetmaskLength", Some("64")),
                ("allocationDefaultNetmaskLength", Some("56")),
            ],
            tags_after: None,
        },
        Applied {
            case: "one tag rule added and one the pool already requires replaced",
            family: ADDRESS_FAMILY_IPV4,
            params: allocation_tags(
                "AddAllocationResourceTag",
                &[("Owner", "TeamB"), ("Cost", "center")],
            ),
            changed: Vec::new(),
            tags_after: Some(vec![
                ("Cost", "center"),
                ("Owner", "TeamB"),
                ("Stage", "prod"),
            ]),
        },
        Applied {
            case: "one tag rule removed by its key",
            family: ADDRESS_FAMILY_IPV4,
            params: allocation_tags("RemoveAllocationResourceTag", &[("Stage", "prod")]),
            changed: Vec::new(),
            tags_after: Some(vec![("Owner", "TeamA")]),
        },
        Applied {
            case: "a tag rule the pool does not require, removed without effect",
            family: ADDRESS_FAMILY_IPV4,
            params: allocation_tags("RemoveAllocationResourceTag", &[("Absent", "whatever")]),
            changed: Vec::new(),
            tags_after: Some(vec![("Owner", "TeamA"), ("Stage", "prod")]),
        },
        Applied {
            case: "one tag rule added and another removed by the same request",
            family: ADDRESS_FAMILY_IPV4,
            params: {
                let mut params =
                    allocation_tags("AddAllocationResourceTag", &[("Env", "staging")]);
                params.extend(allocation_tags(
                    "RemoveAllocationResourceTag",
                    &[("Owner", "TeamA")],
                ));
                params
            },
            changed: Vec::new(),
            tags_after: Some(vec![("Env", "staging"), ("Stage", "prod")]),
        },
        Applied {
            case: "every member a modification can change, at once",
            family: ADDRESS_FAMILY_IPV4,
            params: {
                let mut params = vec![
                    param("Description", "everything at once"),
                    param("AutoImport", "true"),
                    param("AllocationMinNetmaskLength", "8"),
                    param("AllocationMaxNetmaskLength", "24"),
                    param("AllocationDefaultNetmaskLength", "20"),
                ];
                params.extend(allocation_tags(
                    "AddAllocationResourceTag",
                    &[("Cost", "center")],
                ));
                params.extend(allocation_tags(
                    "RemoveAllocationResourceTag",
                    &[("Stage", "prod")],
                ));
                params
            },
            changed: vec![
                ("description", Some("everything at once")),
                ("autoImport", Some("true")),
                ("allocationMinNetmaskLength", Some("8")),
                ("allocationMaxNetmaskLength", Some("24")),
                ("allocationDefaultNetmaskLength", Some("20")),
            ],
            tags_after: Some(vec![("Cost", "center"), ("Owner", "TeamA")]),
        },
        Applied {
            case: "a request naming nothing but the pool",
            family: ADDRESS_FAMILY_IPV4,
            params: Vec::new(),
            changed: Vec::new(),
            tags_after: None,
        },
    ];

    let (router, _engine) = super::ec2_query::emulator();
    let ipam = create_ipam("the IPAM the pools belong to", &router).await;

    for row in rows {
        let case = row.case;
        let pool_id = create_baseline_pool(case, &router, &ipam, row.family).await;
        let before = described(case, &router, &pool_id).await;
        before.assert_creating(case);

        let mut params = vec![param("IpamPoolId", &pool_id)];
        params.extend(row.params.clone());
        let body = ok(case, &router, "ModifyIpamPool", &params).await;
        let answered = pool(case, &body);

        // What the pool must report afterwards: the pool as it stood, with the row's members
        // written over it — so a member no row names is asserted unchanged without this
        // table having to list every one of them.
        let mut expected = before.members.clone();
        for (name, value) in &row.changed {
            match value {
                Some(value) => expected.insert((*name).to_string(), (*value).to_string()),
                None => expected.remove(*name),
            };
        }
        let expected_tags: Vec<(String, String)> = match &row.tags_after {
            Some(tags) => {
                let mut pairs: Vec<(String, String)> = tags
                    .iter()
                    .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
                    .collect();
                pairs.sort();
                pairs
            }
            None => before.allocation_resource_tags.clone(),
        };

        assert_eq!(
            answered.members, expected,
            "case {case:?}: the modification must answer the pool with the request's members \
             written over it, got {:?} in {body}",
            answered.members
        );
        assert_eq!(
            answered.allocation_resource_tags, expected_tags,
            "case {case:?}: expected the allocation resource tags {expected_tags:?}, got {:?} \
             in {body}",
            answered.allocation_resource_tags
        );
        assert_eq!(
            answered.tags, before.tags,
            "case {case:?}: a modification changes none of the pool's own tags, got {:?}",
            answered.tags
        );
        answered.assert_creating(case);

        // The round trip: what the modification answered is what the store holds, so the
        // reader every later caller uses reports exactly it.
        let after = described(case, &router, &pool_id).await;
        assert_eq!(
            after.members, expected,
            "case {case:?}: DescribeIpamPools must report the modified pool, got {:?}",
            after.members
        );
        assert_eq!(
            after.allocation_resource_tags, expected_tags,
            "case {case:?}: DescribeIpamPools must report the modified tag rules, got {:?}",
            after.allocation_resource_tags
        );
        after.assert_creating(case);
    }
}

/// The rule, applied twice over: a modification reads the values the pool already holds, so
/// two modifications in a row leave the pool holding what the SECOND one asked and what the
/// first one left behind.
#[tokio::test]
async fn a_second_modification_reads_what_the_first_one_left() {
    let case = "two modifications in a row";
    let (router, _engine) = super::ec2_query::emulator();
    let ipam = create_ipam("the IPAM the pool belongs to", &router).await;
    let pool_id = create_baseline_pool(case, &router, &ipam, ADDRESS_FAMILY_IPV4).await;

    ok(
        case,
        &router,
        "ModifyIpamPool",
        &[
            param("IpamPoolId", &pool_id),
            param("Description", "after the first modification"),
            param("AllocationMaxNetmaskLength", "30"),
        ],
    )
    .await;
    // Admissible only against the maximum the FIRST modification left: 29 is at the baseline
    // maximum of 28 and would have been refused before it.
    ok(
        case,
        &router,
        "ModifyIpamPool",
        &[
            param("IpamPoolId", &pool_id),
            param("AllocationDefaultNetmaskLength", "29"),
        ],
    )
    .await;

    let after = described(case, &router, &pool_id).await;
    let member = |name: &str| after.members.get(name).cloned().unwrap_or_default();
    assert_eq!(
        member("description"),
        "after the first modification",
        "case {case:?}: the first modification's description must survive the second, got {:?}",
        after.members
    );
    assert_eq!(
        member("allocationMaxNetmaskLength"),
        "30",
        "case {case:?}: the first modification's maximum must survive the second, got {:?}",
        after.members
    );
    assert_eq!(
        member("allocationDefaultNetmaskLength"),
        "29",
        "case {case:?}: the second modification's default must be stored, got {:?}",
        after.members
    );
}

/// The documented ceiling on how many tag rules one pool requires
/// ([`MAX_TAGS_PER_RESOURCE`], "Each resource can have a maximum of 50 tags"): as many as
/// that is accepted, and one more — whether in one list or added to the rules the pool
/// already requires — is refused and nothing is stored.
#[tokio::test]
async fn the_tag_rules_a_pool_requires_are_bounded() {
    let (router, _engine) = super::ec2_query::emulator();
    let ipam = create_ipam("the IPAM the pools belong to", &router).await;

    let case = "as many tag rules as one resource holds";
    let at_the_ceiling = create_untagged_pool(case, &router, &ipam).await;
    let full = many_tags(MAX_TAGS_PER_RESOURCE);
    let mut params = vec![param("IpamPoolId", &at_the_ceiling)];
    params.extend(many_tag_params("AddAllocationResourceTag", &full));
    let body = ok(case, &router, "ModifyIpamPool", &params).await;
    assert_eq!(
        pool(case, &body).allocation_resource_tags.len(),
        MAX_TAGS_PER_RESOURCE,
        "case {case:?}: expected {MAX_TAGS_PER_RESOURCE} tag rules on the pool, got {body}"
    );

    let case = "one tag rule past the ceiling, added to a pool already at it";
    let mut params = vec![param("IpamPoolId", &at_the_ceiling)];
    params.extend(allocation_tags(
        "AddAllocationResourceTag",
        &[("one-too-many", "value")],
    ));
    let (status, body) = invoke(&router, "ModifyIpamPool", &params).await;
    assert_eq!(
        (status, error_code(&body).as_str()),
        (400, "TagLimitExceeded"),
        "case {case:?}: expected 400 TagLimitExceeded, got {status} with body {body}"
    );
    assert_eq!(
        described(case, &router, &at_the_ceiling)
            .await
            .allocation_resource_tags
            .len(),
        MAX_TAGS_PER_RESOURCE,
        "case {case:?}: the refused modification must leave the pool's tag rules alone"
    );

    let case = "one list carrying one tag rule past the ceiling";
    let untouched = create_untagged_pool(case, &router, &ipam).await;
    let over = many_tags(MAX_TAGS_PER_RESOURCE + 1);
    let mut params = vec![param("IpamPoolId", &untouched)];
    params.extend(many_tag_params("AddAllocationResourceTag", &over));
    let (status, body) = invoke(&router, "ModifyIpamPool", &params).await;
    assert_eq!(
        (status, error_code(&body).as_str()),
        (400, "TagLimitExceeded"),
        "case {case:?}: expected 400 TagLimitExceeded, got {status} with body {body}"
    );
    assert!(
        described(case, &router, &untouched)
            .await
            .allocation_resource_tags
            .is_empty(),
        "case {case:?}: the refused modification must store no tag rule at all"
    );
}

// ---------------------------------------------------------------------------
// What a modification refuses
// ---------------------------------------------------------------------------

/// One row of the refusal table: a request, and the status and code it must be refused with.
struct Refused {
    /// What makes this row different from the ones beside it.
    case: &'static str,
    /// The address family the pool being modified holds.
    family: &'static str,
    /// Which pool the request names: `None` for the row's own baseline pool, `Some(id)` for
    /// an id of its own.
    pool_id: Option<&'static str>,
    /// The request members beyond the pool id.
    params: Vec<Param>,
    /// The HTTP status and EC2 error code the request must be refused with.
    expected: (u16, &'static str),
    /// The message the refusal must carry, for a row whose rule AWS words for itself;
    /// `None` where only the code is under test.
    message: Option<&'static str>,
}

/// The rule: a request the pool's documented rules do not admit is refused with EC2's own
/// code for what is at fault, and NOTHING about the stored pool changes.
#[tokio::test]
async fn a_modification_the_pools_rules_do_not_admit_is_refused() {
    let rows: Vec<Refused> = vec![
        Refused {
            case: "an id that is not an IPAM pool id at all",
            family: ADDRESS_FAMILY_IPV4,
            pool_id: Some("not-a-pool"),
            params: vec![param("Description", "nope")],
            expected: (400, INVALID_IPAM_POOL_ID_MALFORMED),
            message: None,
        },
        Refused {
            case: "a well-formed pool id the account holds no pool for",
            family: ADDRESS_FAMILY_IPV4,
            pool_id: Some("ipam-pool-0000000000000000f"),
            params: vec![param("Description", "nope")],
            expected: (400, INVALID_IPAM_POOL_ID_NOT_FOUND),
            message: None,
        },
        Refused {
            case: "a minimum at the maximum the pool already holds",
            family: ADDRESS_FAMILY_IPV4,
            pool_id: None,
            // The pool's default rule is cleared by the same request, so the range itself is
            // the only rule left at fault.
            params: vec![
                param("AllocationMinNetmaskLength", "28"),
                param("ClearAllocationDefaultNetmaskLength", "true"),
            ],
            expected: (400, INVALID_PARAMETER_VALUE),
            message: None,
        },
        Refused {
            case: "a maximum at the minimum the pool already holds",
            family: ADDRESS_FAMILY_IPV4,
            pool_id: None,
            params: vec![
                param("AllocationMaxNetmaskLength", "16"),
                param("ClearAllocationDefaultNetmaskLength", "true"),
            ],
            expected: (400, INVALID_PARAMETER_VALUE),
            message: None,
        },
        Refused {
            case: "a minimum above the maximum, both named by the same request",
            family: ADDRESS_FAMILY_IPV4,
            pool_id: None,
            params: vec![
                param("AllocationMinNetmaskLength", "24"),
                param("AllocationMaxNetmaskLength", "20"),
                param("ClearAllocationDefaultNetmaskLength", "true"),
            ],
            expected: (400, INVALID_PARAMETER_VALUE),
            message: None,
        },
        Refused {
            case: "a netmask length past the range of the pool's address family",
            family: ADDRESS_FAMILY_IPV4,
            pool_id: None,
            // Inside the modelled `IpamNetmaskLength` range of 0..128, outside the 0 - 32 an
            // IPv4 address admits.
            params: vec![param("AllocationMinNetmaskLength", "48")],
            expected: (400, INVALID_PARAMETER_VALUE),
            message: None,
        },
        Refused {
            case: "a default netmask length above the maximum the pool already holds",
            family: ADDRESS_FAMILY_IPV4,
            pool_id: None,
            params: vec![param("AllocationDefaultNetmaskLength", "30")],
            expected: (400, INVALID_PARAMETER_VALUE),
            message: Some("The default netmask length cannot be larger than the max."),
        },
        Refused {
            case: "a default netmask length below the minimum the pool already holds",
            family: ADDRESS_FAMILY_IPV4,
            pool_id: None,
            params: vec![param("AllocationDefaultNetmaskLength", "12")],
            expected: (400, INVALID_PARAMETER_VALUE),
            message: Some("The default netmask length cannot be smaller than the min."),
        },
        Refused {
            case: "a new minimum that the default the pool already holds falls below",
            family: ADDRESS_FAMILY_IPV4,
            pool_id: None,
            params: vec![param("AllocationMinNetmaskLength", "26")],
            expected: (400, INVALID_PARAMETER_VALUE),
            message: Some("The default netmask length cannot be smaller than the min."),
        },
        Refused {
            case: "the default netmask rule both set and cleared",
            family: ADDRESS_FAMILY_IPV4,
            pool_id: None,
            params: vec![
                param("AllocationDefaultNetmaskLength", "26"),
                param("ClearAllocationDefaultNetmaskLength", "true"),
            ],
            expected: (400, INVALID_PARAMETER_COMBINATION),
            message: None,
        },
        Refused {
            case: "one tag rule key both added and removed",
            family: ADDRESS_FAMILY_IPV4,
            pool_id: None,
            params: {
                let mut params =
                    allocation_tags("AddAllocationResourceTag", &[("Owner", "TeamB")]);
                params.extend(allocation_tags(
                    "RemoveAllocationResourceTag",
                    &[("Owner", "TeamA")],
                ));
                params
            },
            expected: (400, INVALID_PARAMETER_COMBINATION),
            message: None,
        },
        Refused {
            case: "one tag rule key given twice in the added list",
            family: ADDRESS_FAMILY_IPV4,
            pool_id: None,
            params: allocation_tags(
                "AddAllocationResourceTag",
                &[("Owner", "TeamB"), ("Owner", "TeamC")],
            ),
            expected: (400, INVALID_PARAMETER_VALUE),
            message: None,
        },
        Refused {
            case: "an added tag rule with no key at all",
            family: ADDRESS_FAMILY_IPV4,
            pool_id: None,
            params: vec![param("AddAllocationResourceTag.1.Value", "no key")],
            expected: (400, INVALID_PARAMETER_VALUE),
            message: None,
        },
        Refused {
            case: "a dry run of a modification that would have succeeded",
            family: ADDRESS_FAMILY_IPV4,
            pool_id: None,
            params: vec![
                param("Description", "never stored"),
                param("DryRun", "true"),
            ],
            expected: (DRY_RUN_OPERATION_STATUS, DRY_RUN_OPERATION),
            message: None,
        },
    ];

    let (router, _engine) = super::ec2_query::emulator();
    let ipam = create_ipam("the IPAM the pools belong to", &router).await;

    for row in rows {
        let case = row.case;
        let own_pool = create_baseline_pool(case, &router, &ipam, row.family).await;
        let before = described(case, &router, &own_pool).await;

        let mut params = vec![param("IpamPoolId", row.pool_id.unwrap_or(&own_pool))];
        params.extend(row.params.clone());
        let (status, body) = invoke(&router, "ModifyIpamPool", &params).await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            row.expected,
            "case {case:?}: expected {:?}, got {status} with body {body}",
            row.expected
        );
        if let Some(expected) = row.message {
            assert_eq!(
                error_message(&body),
                expected,
                "case {case:?}: expected the refusal {expected:?}, got {:?}",
                error_message(&body)
            );
        }

        let after = described(case, &router, &own_pool).await;
        assert_eq!(
            after.members, before.members,
            "case {case:?}: a refused modification must leave the pool's members alone, got \
             {:?}",
            after.members
        );
        assert_eq!(
            after.allocation_resource_tags, before.allocation_resource_tags,
            "case {case:?}: a refused modification must leave the pool's tag rules alone, got \
             {:?}",
            after.allocation_resource_tags
        );
    }
}

/// The rule: a pool a delete has already acted on has nothing left to configure, so a
/// modification of it is refused for the state it is in rather than written.
#[tokio::test]
async fn a_pool_on_its_way_out_cannot_be_modified() {
    let case = "a pool whose deletion has been asked for";
    let (router, _engine) = super::ec2_query::emulator();
    let ipam = create_ipam("the IPAM the pool belongs to", &router).await;
    let pool_id = create_baseline_pool(case, &router, &ipam, ADDRESS_FAMILY_IPV4).await;

    ok(
        case,
        &router,
        "DeleteIpamPool",
        &[param("IpamPoolId", &pool_id)],
    )
    .await;

    let (status, body) = invoke(
        &router,
        "ModifyIpamPool",
        &[
            param("IpamPoolId", &pool_id),
            param("Description", "too late"),
        ],
    )
    .await;
    assert_eq!(
        (status, error_code(&body).as_str()),
        (400, INCORRECT_STATE),
        "case {case:?}: expected 400 {INCORRECT_STATE}, got {status} with body {body}"
    );
    let after = described(case, &router, &pool_id).await;
    assert_eq!(
        after.members.get("description").map(String::as_str),
        Some("the baseline pool"),
        "case {case:?}: the refused modification must leave the pool's description alone, got \
         {:?}",
        after.members
    );
}
