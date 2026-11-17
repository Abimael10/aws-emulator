// SPDX-License-Identifier: CC0-1.0

//! The PREFIX LIST a security group rule grants, driven over `ec2Query` (README.md /
//! OPERATIONS.md "Wire protocol").
//!
//! The rule under test: "the prefix list IDs" (`IpPermission$PrefixListIds`) a rule grants
//! are resolved against the prefix lists the account can see — the lists it created and the
//! ones the Region publishes for its services, which is what `DescribeManagedPrefixLists`
//! reports ("describes your managed prefix lists and any Amazon Web Services-managed prefix
//! lists") — so a well-formed id that names none of them is refused with that resource's own
//! not-found class and no rule is stored, while an id that names one of them is granted. A
//! rule can only ever grant the ranges its prefix list holds, so an id resolving to nothing
//! grants nothing.
//!
//! The rows differ along what the rule generalises over: the DIRECTION the rule is authorized
//! in (both `AuthorizeSecurityGroupIngress` and `AuthorizeSecurityGroupEgress` resolve one),
//! which kind of visible list is granted (one this account created, of either address family,
//! and each list the Region publishes), and which kind of unknown id is refused (one of the
//! length the service mints, one of the length a published list carries, and one whose
//! Region publishes it to no account at all).
//!
//! The suites after those two pin the two rules a rule sourced from a published list is read
//! back by:
//!
//! * "Describes your managed prefix lists and any Amazon Web Services-managed prefix lists"
//!   (`DescribeManagedPrefixLists`): a list Amazon Web Services publishes is a fact of a
//!   Region, so it is reported by name whether or not the account created anything, owned by
//!   Amazon Web Services rather than by the caller, settled, and carrying the ARN whose
//!   account segment is the service's own — and a rule that names one reads back through
//!   `DescribeSecurityGroupRules` as a rule of the group with its own id and ARN.
//! * "Specifying `-1` or a protocol number other than `tcp`, `udp`, `icmp`, or `icmpv6`
//!   allows traffic on all ports, regardless of any port range you specify"
//!   (`IpPermission$IpProtocol`): such a rule is on EVERY port, so the range a request named
//!   is no part of it — the rule reports the all-values sentinel at both ends, and the
//!   aggregate permission a group reports carries neither end.

use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use ec2::state::{PREFIX_LIST_ADDRESS_FAMILY_IPV4, PREFIX_LIST_ADDRESS_FAMILY_IPV6};

use super::ec2_query::{element, elements, emulator, error_code, error_message, filters};
use super::wire::{child, child_text, document, param, Param};

/// The Region every call here is signed for.
const REGION: &str = "us-east-1";

/// One `action` call carrying `params`, as its HTTP status and XML body.
async fn call(router: &MultiServiceRouter, action: &str, params: &[Param]) -> (u16, String) {
    super::ec2_query::call(router, REGION, action, params).await
}

/// One `action` call that has to succeed, and the body it answered.
async fn ok(case: &str, router: &MultiServiceRouter, action: &str, params: &[Param]) -> String {
    let (status, body) = call(router, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    body
}

/// One element of a response body, or a panic naming the case and showing the body.
fn member(case: &str, body: &str, name: &str) -> String {
    element(body, name).unwrap_or_else(|| panic!("case {case:?}: no <{name}> in {body}"))
}

/// The two operations that authorize a rule, and the member each reports a granted prefix
/// list on. "Adds the specified inbound (ingress) rules" / "outbound (egress) rules to a
/// security group", and a rule grants "exactly one of the following sources" /
/// "destinations".
const DIRECTIONS: [(&str, &str); 2] = [
    ("AuthorizeSecurityGroupIngress", "false"),
    ("AuthorizeSecurityGroupEgress", "true"),
];

/// A security group of the account's default VPC, by its minted id.
async fn create_group(case: &str, router: &MultiServiceRouter, group_name: &str) -> String {
    let body = ok(
        case,
        router,
        "CreateSecurityGroup",
        &[
            param("GroupName", group_name),
            param("GroupDescription", "the group whose prefix lists are read"),
        ],
    )
    .await;
    member(case, &body, "groupId")
}

/// A managed prefix list of `family` this account creates, by its minted id.
async fn create_prefix_list(
    case: &str,
    router: &MultiServiceRouter,
    name: &str,
    family: &str,
    cidr: &str,
) -> String {
    let body = ok(
        case,
        router,
        "CreateManagedPrefixList",
        &[
            param("PrefixListName", name),
            param("AddressFamily", family),
            param("MaxEntries", "5"),
            param("Entry.1.Cidr", cidr),
        ],
    )
    .await;
    member(case, &body, "prefixListId")
}

/// The form fields of one permission granting the prefix list `prefix_list_id`.
fn permission_params(prefix_list_id: &str) -> Vec<Param> {
    vec![
        param("IpPermissions.1.IpProtocol", "tcp"),
        param("IpPermissions.1.FromPort", "443"),
        param("IpPermissions.1.ToPort", "443"),
        param(
            "IpPermissions.1.PrefixListIds.1.PrefixListId",
            prefix_list_id,
        ),
    ]
}

/// A rule granting a prefix list the account can see is stored and reported with that list,
/// on both sides of the group — the account's own lists, of either address family, and the
/// ones the Region publishes.
#[tokio::test]
async fn a_rule_may_grant_any_prefix_list_the_account_can_see() {
    for (action, is_egress) in DIRECTIONS {
        let (router, _engine) = emulator();
        let case = action;

        // The lists this account can see: two it creates itself, and every one the Region
        // publishes for its services.
        let mut visible: Vec<(String, String)> = vec![
            (
                "an IPv4 list this account created".to_string(),
                create_prefix_list(
                    case,
                    &router,
                    "office-v4",
                    PREFIX_LIST_ADDRESS_FAMILY_IPV4,
                    "203.0.113.0/24",
                )
                .await,
            ),
            (
                "an IPv6 list this account created".to_string(),
                create_prefix_list(
                    case,
                    &router,
                    "office-v6",
                    PREFIX_LIST_ADDRESS_FAMILY_IPV6,
                    "2001:db8::/32",
                )
                .await,
            ),
        ];
        for published in ec2::state::service_prefix_lists_in_region(REGION) {
            visible.push((
                format!("the published list {}", published.prefix_list_name),
                published.prefix_list_id,
            ));
        }

        for (row, prefix_list_id) in visible {
            let group_id = create_group(case, &router, &format!("granting-{prefix_list_id}")).await;
            let mut params = vec![param("GroupId", &group_id)];
            params.extend(permission_params(&prefix_list_id));
            let body = ok(&row, &router, action, &params).await;
            assert_eq!(
                element(&body, "return").as_deref(),
                Some("true"),
                "case {row:?}: expected <return>true</return> from {action}, got {body}"
            );
            let granted = elements(&body, "prefixListId");
            assert_eq!(
                granted,
                vec![prefix_list_id.clone()],
                "case {row:?}: expected the rule to grant {prefix_list_id:?}, got {body}"
            );
            assert_eq!(
                elements(&body, "isEgress"),
                vec![is_egress.to_string()],
                "case {row:?}: expected the rule on the {action} side, got {body}"
            );

            // The stored rule is what the group reports, so the grant is persisted rather
            // than assembled for the reply.
            let described = ok(
                &row,
                &router,
                "DescribeSecurityGroupRules",
                &[param("Filter.1.Name", "group-id"), param("Filter.1.Value.1", &group_id)],
            )
            .await;
            assert!(
                elements(&described, "prefixListId").contains(&prefix_list_id),
                "case {row:?}: expected the stored rule to carry {prefix_list_id:?}, got \
                 {described}"
            );
        }
    }
}

/// A rule naming a well-formed prefix list id the account cannot see is refused with the
/// prefix list's own not-found class, on both sides of the group, and nothing is stored.
#[tokio::test]
async fn a_rule_naming_a_prefix_list_the_account_cannot_see_is_refused() {
    // (case, the id the request names) — the length the service mints for a list of its own,
    // the length a published list carries, and a list of ANOTHER Region, which this Region
    // publishes to nobody.
    let unknown: [(&str, String); 3] = [
        (
            "an id of the length a created list carries",
            "pl-00000000000000bad".to_string(),
        ),
        (
            "an id of the length a published list carries",
            "pl-0a1b2c3d".to_string(),
        ),
        (
            "a list another Region publishes",
            ec2::state::service_prefix_lists_in_region("eu-west-1")
                .first()
                .map(|published| published.prefix_list_id.clone())
                .expect("every Region publishes a prefix list"),
        ),
    ];

    for (action, _) in DIRECTIONS {
        let (router, _engine) = emulator();
        for (row, prefix_list_id) in &unknown {
            let case = format!("{action}: {row}");
            let group_id = create_group(&case, &router, &format!("refused-{row}")).await;
            let mut params = vec![param("GroupId", &group_id)];
            params.extend(permission_params(prefix_list_id));
            let (status, body) = call(&router, action, &params).await;
            assert_eq!(
                status, 400,
                "case {case:?}: expected HTTP 400, got {status} with body {body}"
            );
            let code = error_code(&body);
            assert_eq!(
                code,
                ec2::state::INVALID_PREFIX_LIST_ID_NOT_FOUND,
                "case {case:?}: expected the prefix list's not-found code, got {code:?} in \
                 {body}"
            );
            assert!(
                body.contains(prefix_list_id.as_str()),
                "case {case:?}: expected the refusal to name {prefix_list_id:?}, got {body}"
            );
            // The sentence EC2 words this class with, whose only variable part is the id the
            // request sent.
            let expected = format!("The prefix list ID '{prefix_list_id}' does not exist");
            let message = error_message(&body);
            assert_eq!(
                message, expected,
                "case {case:?}: expected the message {expected:?}, got {message:?}"
            );

            // Nothing was stored: the group holds no rule at all beyond the outbound rule
            // every new group comes with.
            let described = ok(
                &case,
                &router,
                "DescribeSecurityGroupRules",
                &[param("Filter.1.Name", "group-id"), param("Filter.1.Value.1", &group_id)],
            )
            .await;
            assert!(
                elements(&described, "prefixListId").is_empty(),
                "case {case:?}: expected no rule granting a prefix list, got {described}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// The lists Amazon Web Services publishes, and a rule sourced from one
// ---------------------------------------------------------------------------

/// The account every ARN this emulator mints for the CALLER names.
fn account_id() -> String {
    engine::emulator_account_id().to_string()
}

/// One security group rule as a response reports it: every member read as text, the way the
/// wire carries it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Rule {
    rule_id: String,
    arn: String,
    group_id: String,
    is_egress: Option<String>,
    protocol: Option<String>,
    from_port: Option<String>,
    to_port: Option<String>,
    cidr_ipv4: Option<String>,
    prefix_list_id: Option<String>,
    referenced_group_id: Option<String>,
}

/// The rules a `securityGroupRuleSet` carries, in the order the response reported them.
fn rules_in(case: &str, body: &str) -> Vec<Rule> {
    let parsed = document(case, body);
    let Some(set) = parsed
        .descendants()
        .find(|node| node.is_element() && node.tag_name().name() == "securityGroupRuleSet")
    else {
        panic!("case {case:?}: no <securityGroupRuleSet> in {body}");
    };
    set.children()
        .filter(|node| node.is_element() && node.tag_name().name() == "item")
        .map(|item| Rule {
            rule_id: child_text(item, "securityGroupRuleId").unwrap_or_default(),
            arn: child_text(item, "securityGroupRuleArn").unwrap_or_default(),
            group_id: child_text(item, "groupId").unwrap_or_default(),
            is_egress: child_text(item, "isEgress"),
            protocol: child_text(item, "ipProtocol"),
            from_port: child_text(item, "fromPort"),
            to_port: child_text(item, "toPort"),
            cidr_ipv4: child_text(item, "cidrIpv4"),
            prefix_list_id: child_text(item, "prefixListId"),
            referenced_group_id: child(item, "referencedGroupInfo")
                .and_then(|info| child_text(info, "groupId")),
        })
        .collect()
}

/// Every rule of `group_id`, as the `group-id` filter of `DescribeSecurityGroupRules`
/// selects them.
async fn rules_of(case: &str, router: &MultiServiceRouter, group_id: &str) -> Vec<Rule> {
    let body = ok(
        case,
        router,
        "DescribeSecurityGroupRules",
        &filters(&[("group-id", &[group_id])]),
    )
    .await;
    rules_in(case, &body)
}

/// One aggregated permission of a security group, as `DescribeSecurityGroups` reports it:
/// the protocol, the port range where the permission has one, and the sources it names.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Permission {
    protocol: Option<String>,
    from_port: Option<String>,
    to_port: Option<String>,
    cidr_ipv4: Vec<String>,
    prefix_list_ids: Vec<String>,
    referenced_groups: Vec<(String, String)>,
}

/// The `member` of every `<item>` of the `set_name` list nested in `node`.
fn set_values(node: roxmltree::Node<'_, '_>, set_name: &str, member: &str) -> Vec<String> {
    child(node, set_name)
        .map(|set| {
            set.children()
                .filter(|inner| inner.is_element() && inner.tag_name().name() == "item")
                .filter_map(|item| child_text(item, member))
                .collect()
        })
        .unwrap_or_default()
}

/// The `(userId, groupId)` pair of every referenced group one permission names.
fn group_pairs(node: roxmltree::Node<'_, '_>) -> Vec<(String, String)> {
    child(node, "groups")
        .map(|set| {
            set.children()
                .filter(|inner| inner.is_element() && inner.tag_name().name() == "item")
                .map(|item| {
                    (
                        child_text(item, "userId").unwrap_or_default(),
                        child_text(item, "groupId").unwrap_or_default(),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The aggregated permissions `group_id` reports in one direction.
async fn permissions_of(
    case: &str,
    router: &MultiServiceRouter,
    group_id: &str,
    is_egress: bool,
) -> Vec<Permission> {
    let body = ok(
        case,
        router,
        "DescribeSecurityGroups",
        &[param("GroupId.1", group_id)],
    )
    .await;
    let set_name = if is_egress {
        "ipPermissionsEgress"
    } else {
        "ipPermissions"
    };
    let parsed = document(case, &body);
    let Some(set) = parsed
        .descendants()
        .find(|node| node.is_element() && node.tag_name().name() == set_name)
    else {
        panic!("case {case:?}: no <{set_name}> in {body}");
    };
    set.children()
        .filter(|node| node.is_element() && node.tag_name().name() == "item")
        .map(|item| Permission {
            protocol: child_text(item, "ipProtocol"),
            from_port: child_text(item, "fromPort"),
            to_port: child_text(item, "toPort"),
            cidr_ipv4: set_values(item, "ipRanges", "cidrIp"),
            prefix_list_ids: set_values(item, "prefixListIds", "prefixListId"),
            referenced_groups: group_pairs(item),
        })
        .collect()
}

/// One published prefix list as `DescribeManagedPrefixLists` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ManagedList {
    prefix_list_id: String,
    prefix_list_name: Option<String>,
    owner_id: Option<String>,
    address_family: Option<String>,
    state: Option<String>,
    arn: Option<String>,
    tags: usize,
}

/// The lists `DescribeManagedPrefixLists` answers for the `prefix-list-name` filter `name`.
async fn lists_named(case: &str, router: &MultiServiceRouter, name: &str) -> Vec<ManagedList> {
    let body = ok(
        case,
        router,
        "DescribeManagedPrefixLists",
        &filters(&[("prefix-list-name", &[name])]),
    )
    .await;
    let parsed = document(case, &body);
    let Some(set) = parsed
        .descendants()
        .find(|node| node.is_element() && node.tag_name().name() == "prefixListSet")
    else {
        panic!("case {case:?}: no <prefixListSet> in {body}");
    };
    set.children()
        .filter(|node| node.is_element() && node.tag_name().name() == "item")
        .map(|item| ManagedList {
            prefix_list_id: child_text(item, "prefixListId").unwrap_or_default(),
            prefix_list_name: child_text(item, "prefixListName"),
            owner_id: child_text(item, "ownerId"),
            address_family: child_text(item, "addressFamily"),
            state: child_text(item, "state"),
            arn: child_text(item, "prefixListArn"),
            tags: child(item, "tagSet")
                .map(|tags| {
                    tags.children()
                        .filter(|inner| inner.is_element() && inner.tag_name().name() == "item")
                        .count()
                })
                .unwrap_or_default(),
        })
        .collect()
}

/// One row of the published-list table: the case, the list's name in this Region, and the
/// address family its ranges are of.
struct Published {
    case: &'static str,
    name: &'static str,
    address_family: &'static str,
}

/// The published lists the rows below name.
///
/// Every name is one entry of the catalog `DescribeManagedPrefixLists` reports for a Region
/// (`ec2::state::managed_prefix_lists_in_region`, whose entries are the Amazon VPC User
/// Guide's "AWS-managed prefix lists"), spelled `com.amazonaws.<scope>.<label>` — the scope
/// being the Region for a list AWS publishes per Region and `global` for one it publishes
/// once for all of them. The rows differ in that scope and in the address family, and none
/// of them is in the gateway-service family the legacy `DescribePrefixLists` report carries,
/// which the suites above already name.
fn published_rows() -> Vec<Published> {
    vec![
        Published {
            case: "a Region's own IPv4 list",
            name: "com.amazonaws.us-east-1.ec2-instance-connect",
            address_family: PREFIX_LIST_ADDRESS_FAMILY_IPV4,
        },
        Published {
            case: "a Region's own IPv6 list",
            name: "com.amazonaws.us-east-1.ipv6.route53-healthchecks",
            address_family: PREFIX_LIST_ADDRESS_FAMILY_IPV6,
        },
        Published {
            case: "a globally published IPv4 list",
            name: "com.amazonaws.global.cloudfront.origin-facing",
            address_family: PREFIX_LIST_ADDRESS_FAMILY_IPV4,
        },
        Published {
            case: "a globally published IPv6 list",
            name: "com.amazonaws.global.ipv6.groundstation",
            address_family: PREFIX_LIST_ADDRESS_FAMILY_IPV6,
        },
    ]
}

/// Every name the rows above use is one the Region really publishes, so a row cannot come to
/// name a list no Region has.
#[tokio::test]
async fn every_named_published_list_is_one_the_region_publishes() {
    let published = ec2::state::managed_prefix_lists_in_region(REGION);
    for row in published_rows() {
        let case = row.case;
        assert!(
            published
                .iter()
                .any(|list| list.prefix_list_name == row.name),
            "case {case:?}: {name:?} must be one of the Region's published lists, got \
             {names:?}",
            name = row.name,
            names = published
                .iter()
                .map(|list| list.prefix_list_name.as_str())
                .collect::<Vec<_>>()
        );
    }
}

/// Each list Amazon Web Services publishes in a Region is reported by name with Amazon Web
/// Services as its owner, a settled state, the ARN whose account segment is the service's
/// own, and no tags — on an account that has created no prefix list of its own.
#[tokio::test]
async fn a_region_reports_the_prefix_lists_amazon_web_services_publishes() {
    let (router, _engine) = emulator();

    for row in published_rows() {
        let case = row.case;
        let reported = lists_named(case, &router, row.name).await;
        assert_eq!(
            reported.len(),
            1,
            "case {case:?}: expected exactly one list named {name:?}, got {reported:#?}",
            name = row.name
        );
        let list = &reported[0];
        assert!(
            list.prefix_list_id
                .starts_with(&format!("{}-", ec2::state::PREFIX_LIST_ID_PREFIX)),
            "case {case:?}: expected a prefix list id, got {actual:?}",
            actual = list.prefix_list_id
        );
        // "The ID of the owner of the prefix list" (`ManagedPrefixList$OwnerId`) is the
        // service itself for a list it publishes, and "the Amazon Resource Name (ARN) for
        // the prefix list" (`$PrefixListArn`) names that same owner rather than the account
        // the request was signed for.
        let expected = ManagedList {
            prefix_list_id: list.prefix_list_id.clone(),
            prefix_list_name: Some(row.name.to_string()),
            owner_id: Some(ec2::state::AWS_MANAGED_PREFIX_LIST_OWNER.to_string()),
            address_family: Some(row.address_family.to_string()),
            state: Some(ec2::state::PREFIX_LIST_STATE_CREATE_COMPLETE.to_string()),
            arn: Some(format!(
                "arn:aws:ec2:{REGION}:aws:prefix-list/{id}",
                id = list.prefix_list_id
            )),
            tags: 0,
        };
        assert_eq!(
            list, &expected,
            "case {case:?}: expected {expected:#?}, got {list:#?}"
        );
        assert_ne!(
            list.owner_id.as_deref(),
            Some(account_id().as_str()),
            "case {case:?}: a published list is not owned by the calling account, got \
             {list:#?}"
        );
    }
}

/// An inbound rule whose source is a published prefix list is stored as a rule of the group
/// with its own id and ARN, and reads back through `DescribeSecurityGroupRules` beside the
/// outbound rule the group was created with — so a client auditing the group's rules by id
/// sees the rule it created.
#[tokio::test]
async fn an_ingress_rule_sourced_from_a_published_prefix_list_reads_back_as_a_rule() {
    let (router, _engine) = emulator();
    let account = account_id();

    for (index, row) in published_rows().into_iter().enumerate() {
        let case = row.case;
        let prefix_list_id = lists_named(case, &router, row.name)
            .await
            .first()
            .map(|list| list.prefix_list_id.clone())
            .unwrap_or_else(|| panic!("case {case:?}: no list named {name:?}", name = row.name));
        let group_id = create_group(case, &router, &format!("sourced-{index}")).await;

        let mut params = vec![param("GroupId", &group_id)];
        params.extend(permission_params(&prefix_list_id));
        let body = ok(case, &router, "AuthorizeSecurityGroupIngress", &params).await;
        // The echo is the rule as it was STORED, so it already names the one rule the call
        // added.
        let echoed = rules_in(case, &body);
        assert_eq!(
            echoed.len(),
            1,
            "case {case:?}: expected one rule echoed, got {echoed:#?}"
        );

        let held = rules_of(case, &router, &group_id).await;
        assert_eq!(
            held.len(),
            2,
            "case {case:?}: expected the authored inbound rule beside the group's outbound \
             rule, got {held:#?}"
        );
        let inbound: Vec<&Rule> = held
            .iter()
            .filter(|rule| rule.is_egress.as_deref() == Some("false"))
            .collect();
        assert_eq!(
            inbound.len(),
            1,
            "case {case:?}: expected exactly one inbound rule, got {held:#?}"
        );
        let rule = inbound[0];
        assert_eq!(
            (
                rule.group_id.as_str(),
                rule.protocol.as_deref(),
                rule.from_port.as_deref(),
                rule.to_port.as_deref(),
                rule.prefix_list_id.as_deref(),
                rule.cidr_ipv4.as_deref(),
                rule.referenced_group_id.as_deref(),
            ),
            (
                group_id.as_str(),
                Some("tcp"),
                Some("443"),
                Some("443"),
                Some(prefix_list_id.as_str()),
                None,
                None,
            ),
            "case {case:?}: expected the prefix-list source reported on the inbound rule, \
             got {rule:#?}"
        );
        assert_eq!(
            rule.arn,
            format!(
                "arn:aws:ec2:{REGION}:{account}:security-group-rule/{id}",
                id = rule.rule_id
            ),
            "case {case:?}: expected the rule's ARN to name the account and the rule, got \
             {rule:#?}"
        );
        assert_eq!(
            echoed[0].rule_id, rule.rule_id,
            "case {case:?}: expected the echoed rule to be the stored one, got {echoed:#?} \
             then {rule:#?}"
        );

        // And the group's aggregate view names that one list, through that same rule.
        let aggregated = permissions_of(case, &router, &group_id, false).await;
        assert_eq!(
            aggregated.len(),
            1,
            "case {case:?}: expected one inbound permission, got {aggregated:#?}"
        );
        assert_eq!(
            aggregated[0].prefix_list_ids,
            vec![prefix_list_id.clone()],
            "case {case:?}: expected the permission to name exactly the valid list, got \
             {permission:#?}",
            permission = aggregated[0]
        );
    }
}

/// A source the service cannot resolve — a prefix list nobody can see, or a security group
/// the account does not hold — is refused, and the group's effective sources stay exactly
/// the ones the accepted calls left: a rule AWS never created is never reported as active.
#[tokio::test]
async fn a_source_that_resolves_to_nothing_leaves_the_groups_sources_as_they_were() {
    let (router, _engine) = emulator();
    let account = account_id();
    let case = "the effective sources";

    let valid_list = lists_named(case, &router, "com.amazonaws.us-east-1.ec2-instance-connect")
        .await
        .first()
        .map(|list| list.prefix_list_id.clone())
        .expect("the Region publishes the list");
    let valid_group = create_group(case, &router, "referenced").await;
    let group_id = create_group(case, &router, "authorised").await;

    // Two sources the service CAN resolve: one published list and one group of the account.
    for (row, source) in [
        (
            "a published prefix list",
            vec![param(
                "IpPermissions.1.PrefixListIds.1.PrefixListId",
                &valid_list,
            )],
        ),
        (
            "a group of the account",
            vec![param("IpPermissions.1.Groups.1.GroupId", &valid_group)],
        ),
    ] {
        let mut params = vec![
            param("GroupId", &group_id),
            param("IpPermissions.1.IpProtocol", "tcp"),
            param("IpPermissions.1.FromPort", "22"),
            param("IpPermissions.1.ToPort", "22"),
        ];
        params.extend(source);
        ok(row, &router, "AuthorizeSecurityGroupIngress", &params).await;
    }

    // Each row names a source of the right SHAPE that resolves to no resource — alone, and
    // beside a source that does resolve, which is the case a partially applied authorize
    // would store. The refusal is that resource's own not-found class, and nothing is
    // stored.
    let refusals: Vec<(&str, Vec<Param>, &str)> = vec![
        (
            "a prefix list id no list carries",
            vec![param(
                "IpPermissions.1.PrefixListIds.1.PrefixListId",
                "pl-0f1e2d3c4b5a69780",
            )],
            ec2::state::INVALID_PREFIX_LIST_ID_NOT_FOUND,
        ),
        (
            "an unknown prefix list beside the valid one",
            vec![
                param("IpPermissions.1.PrefixListIds.1.PrefixListId", "pl-0a1b2c3e"),
                param(
                    "IpPermissions.1.PrefixListIds.2.PrefixListId",
                    &valid_list,
                ),
            ],
            ec2::state::INVALID_PREFIX_LIST_ID_NOT_FOUND,
        ),
        (
            "a group id the account does not hold",
            vec![param(
                "IpPermissions.1.Groups.1.GroupId",
                "sg-0f1e2d3c4b5a69780",
            )],
            ec2::state::INVALID_GROUP_NOT_FOUND,
        ),
        (
            "an unknown group beside the valid one",
            vec![
                param("IpPermissions.1.Groups.1.GroupId", &valid_group),
                param("IpPermissions.1.Groups.2.GroupId", "sg-0a1b2c3e"),
            ],
            ec2::state::INVALID_GROUP_NOT_FOUND,
        ),
    ];

    for (row, source, code) in refusals {
        let before = rules_of(row, &router, &group_id).await;
        let mut params = vec![
            param("GroupId", &group_id),
            param("IpPermissions.1.IpProtocol", "tcp"),
            param("IpPermissions.1.FromPort", "8443"),
            param("IpPermissions.1.ToPort", "8443"),
        ];
        params.extend(source);
        let (status, body) = call(&router, "AuthorizeSecurityGroupIngress", &params).await;
        assert_eq!(
            status, 400,
            "case {row:?}: expected HTTP 400, got {status} with body {body}"
        );
        let answered = error_code(&body);
        assert_eq!(
            answered, code,
            "case {row:?}: expected {code}, got {answered:?} in {body}"
        );
        let after = rules_of(row, &router, &group_id).await;
        assert_eq!(
            after, before,
            "case {row:?}: expected the refused authorize to store nothing, got {after:#?} \
             from {before:#?}"
        );
    }

    // The group's effective sources are exactly the two the service resolved: one list and
    // one group, each named once.
    let inbound = permissions_of(case, &router, &group_id, false).await;
    let listed: Vec<String> = inbound
        .iter()
        .flat_map(|permission| permission.prefix_list_ids.clone())
        .collect();
    assert_eq!(
        listed,
        vec![valid_list.clone()],
        "expected exactly the valid prefix list as a source, got {inbound:#?}"
    );
    let referenced: Vec<(String, String)> = inbound
        .iter()
        .flat_map(|permission| permission.referenced_groups.clone())
        .collect();
    assert_eq!(
        referenced,
        vec![(account.clone(), valid_group.clone())],
        "expected exactly the group that exists as a source, got {inbound:#?}"
    );
}

// ---------------------------------------------------------------------------
// The port range a rule's protocol has
// ---------------------------------------------------------------------------

/// One row of the port-range table: the case, the protocol as the request writes it, the
/// range the request names, and the range the two views must then report — the rule's own
/// and the aggregate permission's.
struct Ports {
    case: &'static str,
    protocol: &'static str,
    requested: Option<(i32, i32)>,
    on_the_rule: (Option<&'static str>, Option<&'static str>),
    on_the_permission: (Option<&'static str>, Option<&'static str>),
}

/// The protocols and ranges the rows below span.
///
/// The four protocols that HAVE a port range are the ones the model names — "For `tcp`,
/// `udp`, and `icmp`, you must specify a port range. For `icmpv6`, the port range is
/// optional" (`IpPermission$IpProtocol`) — and they keep the range exactly as given,
/// including the `-1` the model gives for "all ICMP types"/"all ICMP codes"
/// (`IpPermission$FromPort`, `$ToPort`). Every other protocol admits none: "specifying `-1`
/// or a protocol number other than `tcp`, `udp`, `icmp`, or `icmpv6` allows traffic on all
/// ports, regardless of any port range you specify" (the same member), so the rule is on
/// every port whatever the request named — reported as the all-values sentinel on the rule
/// and as neither end on the permission. The rows differ in the protocol's kind (one of the
/// four names, `-1`, a protocol number) and in the range named (none, a real range, the
/// sentinel, and a range on a protocol that has none).
fn port_rows() -> Vec<Ports> {
    vec![
        Ports {
            case: "tcp with a single port",
            protocol: "tcp",
            requested: Some((22, 22)),
            on_the_rule: (Some("22"), Some("22")),
            on_the_permission: (Some("22"), Some("22")),
        },
        Ports {
            case: "udp with a range",
            protocol: "udp",
            requested: Some((1024, 65535)),
            on_the_rule: (Some("1024"), Some("65535")),
            on_the_permission: (Some("1024"), Some("65535")),
        },
        Ports {
            case: "icmp with a type and a code",
            protocol: "icmp",
            requested: Some((8, 0)),
            on_the_rule: (Some("8"), Some("0")),
            on_the_permission: (Some("8"), Some("0")),
        },
        Ports {
            case: "icmp with all types and all codes",
            protocol: "icmp",
            requested: Some((-1, -1)),
            on_the_rule: (Some("-1"), Some("-1")),
            on_the_permission: (Some("-1"), Some("-1")),
        },
        Ports {
            case: "icmpv6 with the optional range omitted",
            protocol: "icmpv6",
            requested: None,
            on_the_rule: (None, None),
            on_the_permission: (None, None),
        },
        Ports {
            case: "all protocols, no range named",
            protocol: "-1",
            requested: None,
            on_the_rule: (Some("-1"), Some("-1")),
            on_the_permission: (None, None),
        },
        Ports {
            case: "all protocols with a range named anyway",
            protocol: "-1",
            requested: Some((0, 0)),
            on_the_rule: (Some("-1"), Some("-1")),
            on_the_permission: (None, None),
        },
        Ports {
            case: "a protocol number, no range named",
            protocol: "47",
            requested: None,
            on_the_rule: (Some("-1"), Some("-1")),
            on_the_permission: (None, None),
        },
        Ports {
            case: "a protocol number with a range named anyway",
            protocol: "50",
            requested: Some((80, 80)),
            on_the_rule: (Some("-1"), Some("-1")),
            on_the_permission: (None, None),
        },
    ]
}

/// A rule reports the port range its PROTOCOL has: the range as given for the protocols that
/// carry one, and every port — the all-values sentinel on the rule, neither end on the
/// aggregate permission — for the protocols that do not, whatever range the request named.
#[tokio::test]
async fn a_rule_reports_the_port_range_its_protocol_has() {
    for (index, row) in port_rows().into_iter().enumerate() {
        // One emulator per row, so a row's rule cannot meet another row's on the
        // duplicate-rule rule.
        let (router, _engine) = emulator();
        let case = row.case;
        let group_id = create_group(case, &router, &format!("ported-{index}")).await;

        let mut params = vec![
            param("GroupId", &group_id),
            param("IpPermissions.1.IpProtocol", row.protocol),
            param("IpPermissions.1.IpRanges.1.CidrIp", "10.0.0.0/8"),
        ];
        if let Some((from_port, to_port)) = row.requested {
            params.push(param("IpPermissions.1.FromPort", &from_port.to_string()));
            params.push(param("IpPermissions.1.ToPort", &to_port.to_string()));
        }
        let body = ok(case, &router, "AuthorizeSecurityGroupIngress", &params).await;

        // The echo, the rule read back and the aggregate permission all agree, because all
        // three are readings of the one stored rule.
        let expected_rule = (
            row.on_the_rule.0.map(str::to_string),
            row.on_the_rule.1.map(str::to_string),
        );
        let echoed = rules_in(case, &body);
        assert_eq!(
            echoed.len(),
            1,
            "case {case:?}: expected one rule echoed, got {echoed:#?}"
        );
        assert_eq!(
            (echoed[0].from_port.clone(), echoed[0].to_port.clone()),
            expected_rule,
            "case {case:?}: expected the echo to report {expected_rule:?}, got {rule:#?}",
            rule = echoed[0]
        );

        let held = rules_of(case, &router, &group_id).await;
        let inbound: Vec<&Rule> = held
            .iter()
            .filter(|rule| rule.is_egress.as_deref() == Some("false"))
            .collect();
        assert_eq!(
            inbound.len(),
            1,
            "case {case:?}: expected exactly one inbound rule, got {held:#?}"
        );
        assert_eq!(
            (inbound[0].from_port.clone(), inbound[0].to_port.clone()),
            expected_rule,
            "case {case:?}: expected the rule to report {expected_rule:?}, got {rule:#?}",
            rule = inbound[0]
        );

        let aggregated = permissions_of(case, &router, &group_id, false).await;
        assert_eq!(
            aggregated.len(),
            1,
            "case {case:?}: expected one inbound permission, got {aggregated:#?}"
        );
        let expected_permission = (
            row.on_the_permission.0.map(str::to_string),
            row.on_the_permission.1.map(str::to_string),
        );
        assert_eq!(
            (
                aggregated[0].from_port.clone(),
                aggregated[0].to_port.clone(),
            ),
            expected_permission,
            "case {case:?}: expected the permission to report {expected_permission:?}, got \
             {permission:#?}",
            permission = aggregated[0]
        );
        assert_eq!(
            aggregated[0].cidr_ipv4,
            vec!["10.0.0.0/8".to_string()],
            "case {case:?}: expected the permission to name the authorised range, got \
             {permission:#?}",
            permission = aggregated[0]
        );
    }
}

/// The outbound rule every new security group carries is an all-protocols rule, so it is
/// reported on every port by the rule view and with no port members by the aggregate one —
/// the same reading as any other rule on a protocol that admits no range.
#[tokio::test]
async fn the_default_outbound_rule_is_reported_on_every_port() {
    let (router, _engine) = emulator();

    // The rows differ in the name a create chooses, which nothing about the default rule
    // depends on.
    for (case, group_name) in [
        ("a plain group", "plain"),
        ("a name with spaces", "edge group 9"),
    ] {
        let group_id = create_group(case, &router, group_name).await;
        let held = rules_of(case, &router, &group_id).await;
        assert_eq!(
            held.len(),
            1,
            "case {case:?}: expected exactly one rule on a new group, got {held:#?}"
        );
        let rule = &held[0];
        assert_eq!(
            (
                rule.is_egress.as_deref(),
                rule.protocol.as_deref(),
                rule.from_port.as_deref(),
                rule.to_port.as_deref(),
                rule.cidr_ipv4.as_deref(),
            ),
            (
                Some("true"),
                Some("-1"),
                Some("-1"),
                Some("-1"),
                Some("0.0.0.0/0"),
            ),
            "case {case:?}: expected the allow-all outbound rule on every port, got \
             {rule:#?}"
        );

        let aggregated = permissions_of(case, &router, &group_id, true).await;
        assert_eq!(
            aggregated.len(),
            1,
            "case {case:?}: expected one outbound permission, got {aggregated:#?}"
        );
        assert_eq!(
            (
                aggregated[0].protocol.as_deref(),
                aggregated[0].from_port.as_deref(),
                aggregated[0].to_port.as_deref(),
            ),
            (Some("-1"), None, None),
            "case {case:?}: expected the aggregate permission to carry no port range, got \
             {permission:#?}",
            permission = aggregated[0]
        );
    }
}
