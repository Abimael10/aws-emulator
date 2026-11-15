// SPDX-License-Identifier: CC0-1.0

//! The NAME an Amazon Web Services-managed prefix list is published under, driven over
//! `ec2Query` (README.md / OPERATIONS.md "Wire protocol").
//!
//! The rule under test: an AWS-managed prefix list is named
//! `com.amazonaws.<scope>.<service label>`, where the scope is the REGION for a list whose
//! ranges belong to that Region and the literal `global` for a list AWS publishes once for
//! every Region. `DescribeManagedPrefixLists` reports every one of them ("describes your
//! managed prefix lists and any Amazon Web Services-managed prefix lists"), selectable by
//! its documented `prefix-list-name` filter
//! (`DescribeManagedPrefixListsRequest$Filters`), so a globally published list answers
//! under its one global name from EVERY Region's endpoint under one id, while a
//! Region-scoped list answers only under its own Region's name and carries a different id
//! in each Region. Whichever scope a list sits under, the id the report carries is the id
//! every other read of a prefix list accepts: the entry report and the prefix list a
//! security group rule grants (`IpPermission$PrefixListIds`).
//!
//! The rows differ along what the rule generalises over: the SCOPE (global and
//! Region-scoped), the service the list belongs to, the ADDRESS FAMILY of its ranges (IPv4
//! and IPv6), and the Region the request is signed for — Regions in three partitions'
//! worth of geographies, none of which the lookup's name depends on for a global list.

use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use ec2::state::{
    AWS_MANAGED_PREFIX_LIST_OWNER, GLOBAL_PREFIX_LIST_SCOPE,
    PREFIX_LIST_ADDRESS_FAMILY_IPV4, PREFIX_LIST_ADDRESS_FAMILY_IPV6,
    PREFIX_LIST_STATE_CREATE_COMPLETE,
};

use super::ec2_query::{element, elements, emulator, filters};
use super::wire::{param, Param};

/// The Regions every row is asked in: more than one, because the scope of a list's name is
/// exactly what decides whether the answer travels between them.
const REGIONS: [&str; 3] = ["us-east-1", "eu-west-1", "ap-southeast-2"];

/// One prefix list AWS publishes: the service label its name carries, whether AWS publishes
/// it under the global scope rather than per Region, and whether its ranges are IPv6.
///
/// The labels and the scope of each come from the Amazon VPC User Guide's "AWS-managed
/// prefix lists" page, which names Amazon CloudFront's origin-facing list
/// `com.amazonaws.global.cloudfront.origin-facing` and AWS Ground Station's
/// `com.amazonaws.global.groundstation` while naming every other service's list for the
/// Region (`com.amazonaws.<region>.s3`). Written out here rather than read off the state
/// catalog so the catalog cannot drift from the guide unnoticed, and carrying services the
/// report that prompted this suite never cited (Ground Station, the health checkers, Amazon
/// VPC Lattice, EC2 Instance Connect).
struct ListCase {
    label: &'static str,
    global: bool,
    ipv6: bool,
}

/// The rows: both scopes, both address families, and a service family each.
const LISTS: [ListCase; 8] = [
    ListCase { label: "cloudfront.origin-facing", global: true, ipv6: false },
    ListCase { label: "ipv6.cloudfront.origin-facing", global: true, ipv6: true },
    ListCase { label: "groundstation", global: true, ipv6: false },
    ListCase { label: "ipv6.groundstation", global: true, ipv6: true },
    ListCase { label: "s3", global: false, ipv6: false },
    ListCase { label: "ipv6.route53-healthchecks", global: false, ipv6: true },
    ListCase { label: "vpc-lattice", global: false, ipv6: false },
    ListCase { label: "ec2-instance-connect", global: false, ipv6: false },
];

impl ListCase {
    /// The name this list is published under, as seen from `region`.
    fn name(&self, region: &str) -> String {
        let scope = if self.global {
            GLOBAL_PREFIX_LIST_SCOPE
        } else {
            region
        };
        format!("com.amazonaws.{scope}.{label}", label = self.label)
    }

    /// The name this list is NOT published under: the other scope, which must select
    /// nothing.
    fn name_under_the_other_scope(&self, region: &str) -> String {
        let scope = if self.global {
            region
        } else {
            GLOBAL_PREFIX_LIST_SCOPE
        };
        format!("com.amazonaws.{scope}.{label}", label = self.label)
    }

    /// The `AddressFamily` the list's ranges give it ("The IP address type. Valid Values:
    /// IPv4 | IPv6" — `CreateManagedPrefixListRequest$AddressFamily`).
    fn address_family(&self) -> &'static str {
        if self.ipv6 {
            PREFIX_LIST_ADDRESS_FAMILY_IPV6
        } else {
            PREFIX_LIST_ADDRESS_FAMILY_IPV4
        }
    }
}

/// One call of `action` signed for `region`, as its HTTP status and XML body.
async fn call(
    router: &MultiServiceRouter,
    region: &str,
    action: &str,
    params: &[Param],
) -> (u16, String) {
    super::ec2_query::call(router, region, action, params).await
}

/// One call of `action` that has to succeed, and the body it answered.
async fn ok(
    case: &str,
    router: &MultiServiceRouter,
    region: &str,
    action: &str,
    params: &[Param],
) -> String {
    let (status, body) = call(router, region, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    body
}

/// Is `id` shaped like a prefix list id — `pl-` followed by lowercase hexadecimal digits,
/// the form AWS mints (`PrefixListResourceId`)?
fn id_is_well_formed(id: &str) -> bool {
    id.strip_prefix("pl-").is_some_and(|digits| {
        !digits.is_empty()
            && digits
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

/// The one prefix list `DescribeManagedPrefixLists` reports in `region` for the
/// `prefix-list-name` filter value `name`, as `(id, addressFamily)` — panicking with the
/// case and the body when the answer does not carry exactly one.
async fn one_list_named(
    case: &str,
    router: &MultiServiceRouter,
    region: &str,
    name: &str,
) -> (String, String) {
    let body = ok(
        case,
        router,
        region,
        "DescribeManagedPrefixLists",
        &filters(&[("prefix-list-name", &[name])]),
    )
    .await;
    let reported = elements(&body, "prefixListName");
    assert_eq!(
        reported,
        vec![name.to_string()],
        "case {case:?}: expected exactly the list {name}, got {reported:?} in {body}"
    );
    let ids = elements(&body, "prefixListId");
    assert_eq!(
        ids.len(),
        1,
        "case {case:?}: expected one prefix list id, got {ids:?} in {body}"
    );
    assert!(
        id_is_well_formed(&ids[0]),
        "case {case:?}: expected a pl- prefix list id, got {:?} in {body}",
        ids[0]
    );
    assert_eq!(
        elements(&body, "ownerId"),
        vec![AWS_MANAGED_PREFIX_LIST_OWNER.to_string()],
        "case {case:?}: a list AWS publishes is owned by the service, got {body}"
    );
    assert_eq!(
        elements(&body, "state"),
        vec![PREFIX_LIST_STATE_CREATE_COMPLETE.to_string()],
        "case {case:?}: a list AWS publishes is settled, got {body}"
    );
    let families = elements(&body, "addressFamily");
    assert_eq!(
        families.len(),
        1,
        "case {case:?}: expected one addressFamily, got {families:?} in {body}"
    );
    (ids[0].clone(), families[0].clone())
}

/// Every published list answers under the name its scope gives it, in every Region — and
/// under no other scope's name. A globally published list carries ONE id across Regions; a
/// Region-scoped one carries a different id in each.
#[tokio::test]
async fn a_published_list_answers_under_the_name_its_scope_gives_it() {
    let (router, _engine) = emulator();

    for list in &LISTS {
        let mut ids: Vec<(&str, String)> = Vec::new();
        for region in REGIONS {
            let name = list.name(region);
            let case = format!("{name} in {region}");
            let (id, family) = one_list_named(&case, &router, region, &name).await;
            assert_eq!(
                family,
                list.address_family(),
                "case {case:?}: expected the address family of its ranges, got {family:?}"
            );

            // The other scope's name is not this list's name, so it selects nothing rather
            // than the list: a name is matched, never guessed at.
            let other = list.name_under_the_other_scope(region);
            let case = format!("{other} in {region}");
            let body = ok(
                &case,
                &router,
                region,
                "DescribeManagedPrefixLists",
                &filters(&[("prefix-list-name", &[other.as_str()])]),
            )
            .await;
            assert_eq!(
                elements(&body, "prefixListName"),
                Vec::<String>::new(),
                "case {case:?}: no list is published under the other scope, got {body}"
            );

            ids.push((region, id));
        }

        let first = ids[0].1.clone();
        for (region, id) in &ids[1..] {
            let case = format!("{} in {region}", list.label);
            if list.global {
                assert_eq!(
                    *id, first,
                    "case {case:?}: a globally published list is one list, so every Region \
                     reports the id {first:?}, got {id:?}"
                );
            } else {
                assert_ne!(
                    *id, first,
                    "case {case:?}: a Region-scoped list belongs to its own Region, so its \
                     id must differ from {first:?}, got {id:?}"
                );
            }
        }
    }
}

/// The id a lookup by name reports is the id every other read of a prefix list accepts: the
/// report narrowed by `PrefixListIds`, the entry report, and the prefix list a security
/// group rule grants on either side of the group.
///
/// This is the documented pattern for locking an origin to one Amazon Web Services service —
/// look the managed prefix list up by name, then authorize from the id it reports — so the
/// rows drive it over both scopes, both address families and more than one Region, with the
/// two ports such a grant is written for.
#[tokio::test]
async fn the_id_a_lookup_by_name_reports_resolves_wherever_a_prefix_list_id_is_read() {
    // The ports an origin-facing grant is written for: "tcp/443" and "tcp/80".
    const PORTS: [&str; 2] = ["443", "80"];

    for region in REGIONS {
        let (router, _engine) = emulator();
        for (index, list) in LISTS.iter().enumerate() {
            let name = list.name(region);
            let case = format!("{name} in {region}");
            let (prefix_list_id, _) = one_list_named(&case, &router, region, &name).await;

            // Narrowed by the id instead of the name: the same one list, under the same
            // name, so the two ways of reaching it cannot disagree.
            let body = ok(
                &case,
                &router,
                region,
                "DescribeManagedPrefixLists",
                &[param("PrefixListId.1", &prefix_list_id)],
            )
            .await;
            assert_eq!(
                elements(&body, "prefixListName"),
                vec![name.clone()],
                "case {case:?}: the id {prefix_list_id} must report that one list, got {body}"
            );

            // Its ranges are readable: a prefix list a rule grants grants exactly the ranges
            // the list holds, so an empty list would grant nothing.
            let body = ok(
                &case,
                &router,
                region,
                "GetManagedPrefixListEntries",
                &[param("PrefixListId", &prefix_list_id)],
            )
            .await;
            let cidrs = elements(&body, "cidr");
            assert!(
                !cidrs.is_empty(),
                "case {case:?}: the published list {prefix_list_id} must hold at least one \
                 range, got {body}"
            );

            // And the id is accepted as a rule's source and its destination, on the ports a
            // caller locking an origin to this service writes.
            for (action, is_egress) in [
                ("AuthorizeSecurityGroupIngress", "false"),
                ("AuthorizeSecurityGroupEgress", "true"),
            ] {
                for port in PORTS {
                    let case = format!("{case}: {action} tcp/{port}");
                    let created = ok(
                        &case,
                        &router,
                        region,
                        "CreateSecurityGroup",
                        &[
                            param(
                                "GroupName",
                                &format!("origin-{index}-{action}-{port}"),
                            ),
                            param("GroupDescription", "locked to one published prefix list"),
                        ],
                    )
                    .await;
                    let group_id = element(&created, "groupId").unwrap_or_else(|| {
                        panic!("case {case:?}: no <groupId> in {created}")
                    });
                    let body = ok(
                        &case,
                        &router,
                        region,
                        action,
                        &[
                            param("GroupId", &group_id),
                            param("IpPermissions.1.IpProtocol", "tcp"),
                            param("IpPermissions.1.FromPort", port),
                            param("IpPermissions.1.ToPort", port),
                            param(
                                "IpPermissions.1.PrefixListIds.1.PrefixListId",
                                &prefix_list_id,
                            ),
                        ],
                    )
                    .await;
                    assert_eq!(
                        elements(&body, "prefixListId"),
                        vec![prefix_list_id.clone()],
                        "case {case:?}: expected the rule to grant {prefix_list_id:?}, got \
                         {body}"
                    );
                    assert_eq!(
                        elements(&body, "isEgress"),
                        vec![is_egress.to_string()],
                        "case {case:?}: expected the rule on the {action} side, got {body}"
                    );

                    // The stored rule is what the group reports back, so the grant is
                    // persisted rather than assembled for the reply.
                    let described = ok(
                        &case,
                        &router,
                        region,
                        "DescribeSecurityGroupRules",
                        &[
                            param("Filter.1.Name", "group-id"),
                            param("Filter.1.Value.1", &group_id),
                        ],
                    )
                    .await;
                    assert!(
                        elements(&described, "prefixListId").contains(&prefix_list_id),
                        "case {case:?}: expected the stored rule to carry \
                         {prefix_list_id:?}, got {described}"
                    );
                }
            }
        }
    }
}
