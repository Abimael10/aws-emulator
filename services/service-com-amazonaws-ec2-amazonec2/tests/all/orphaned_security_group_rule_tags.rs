// SPDX-License-Identifier: CC0-1.0

//! What a delete that ORPHANS security-group rules does to the tags written on them, driven
//! over the wire the service speaks (`ec2Query`, through the shared [`super::ec2_query`]
//! harness).
//!
//! The rule under test: a tag written on a security-group rule is held under the RULE's id
//! rather than on the rule itself, so a delete that removes rules as a consequence of
//! removing the container they belong to — a security group, or a VPC that takes its groups
//! with it — removes the deleted resources' OWN tags and leaves the orphaned rules' tags
//! reported by `DescribeTags`. Deleting a RULE, which is what a revoke does, takes that
//! rule's tags with it.
//!
//! The rows span what the rule generalises over: which delete orphans the rule (a VPC delete
//! and a group delete), who wrote the rule (the service, with the VPC's default group, and
//! the caller, through an authorize), which direction it grants in, and which resources'
//! own tags the same delete does remove (the VPC, its default security group, its default
//! network ACL and its default route table). Each assertion names its case and the rows the
//! report actually carried.

use std::collections::BTreeSet;

use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{DescribedTag, call, element, elements, filters, tag_report, tagging};
use super::wire::{Param, param};

/// The Region every call here is signed for.
const REGION: &str = "us-west-2";

/// The emulator every test here drives.
fn emulator() -> MultiServiceRouter {
    super::ec2_query::emulator().0
}

/// One `action` call that must succeed, as its response body.
async fn ok(router: &MultiServiceRouter, case: &str, action: &str, params: &[Param]) -> String {
    let (status, body) = call(router, REGION, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    body
}

/// One VPC, as its id.
async fn create_vpc(router: &MultiServiceRouter, case: &str, cidr: &str) -> String {
    let body = ok(router, case, "CreateVpc", &[param("CidrBlock", cidr)]).await;
    element(&body, "vpcId")
        .unwrap_or_else(|| panic!("case {case:?}: expected a <vpcId> in {body}"))
}

/// The security group of `vpc_id` named `group_name` — the VPC's own default group is named
/// `default` ([`ec2::state::DEFAULT_SECURITY_GROUP_NAME`]), so this reaches the group the
/// CREATE of the VPC made as well as one the caller created.
async fn group_named(
    router: &MultiServiceRouter,
    case: &str,
    vpc_id: &str,
    group_name: &str,
) -> String {
    let params = filters(&[("vpc-id", &[vpc_id]), ("group-name", &[group_name])]);
    let body = ok(router, case, "DescribeSecurityGroups", &params).await;
    // `<groupId>` is also the name of the member a rule's referenced group carries, so the
    // ids are read as a SET: the filters select one group, and that group's own id is what
    // every occurrence in this body names.
    let found: BTreeSet<String> = elements(&body, "groupId").into_iter().collect();
    assert_eq!(
        found.len(),
        1,
        "case {case:?}: expected exactly one group named {group_name:?} in {vpc_id}, got \
         {found:?}"
    );
    found
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: expected a <groupId> in the selected group"))
}

/// Every rule of the security group `group_id`, as its `SecurityGroupRuleId`.
async fn rules_of(router: &MultiServiceRouter, case: &str, group_id: &str) -> Vec<String> {
    let params = filters(&[("group-id", &[group_id])]);
    let body = ok(router, case, "DescribeSecurityGroupRules", &params).await;
    elements(&body, "securityGroupRuleId")
}

/// One inbound rule on `group_id` granting `cidr` on one TCP port, as the rule id it was
/// minted with.
async fn authorize_ingress(
    router: &MultiServiceRouter,
    case: &str,
    group_id: &str,
    cidr: &str,
    port: u16,
) -> String {
    let body = ok(
        router,
        case,
        "AuthorizeSecurityGroupIngress",
        &[
            param("GroupId", group_id),
            param("IpPermissions.1.IpProtocol", "tcp"),
            param("IpPermissions.1.FromPort", &port.to_string()),
            param("IpPermissions.1.ToPort", &port.to_string()),
            param("IpPermissions.1.IpRanges.1.CidrIp", cidr),
        ],
    )
    .await;
    element(&body, "securityGroupRuleId")
        .unwrap_or_else(|| panic!("case {case:?}: expected a <securityGroupRuleId> in {body}"))
}

/// Write the one tag `(key, value)` onto `resource_id`, through the tagging fields the shared
/// harness builds.
async fn tag(router: &MultiServiceRouter, case: &str, resource_id: &str, key: &str, value: &str) {
    ok(
        router,
        case,
        "CreateTags",
        &tagging(resource_id, &[(key, value)]),
    )
    .await;
}

/// Every tag `DescribeTags` reports for `resource_id`, read by the shared harness:
/// `(resourceId, resourceType, key, value)` rows as a SET, because "the order of the elements
/// in the response … might vary" (`DescribeTags`).
async fn reported_tags(
    router: &MultiServiceRouter,
    case: &str,
    resource_id: &str,
) -> BTreeSet<DescribedTag> {
    tag_report(router, REGION, case, resource_id).await
}

/// The one row a surviving security-group-rule tag has to be reported as.
fn rule_tag_row(resource_id: &str, key: &str, value: &str) -> BTreeSet<DescribedTag> {
    [(
        resource_id.to_string(),
        ec2::state::RESOURCE_TYPE_SECURITY_GROUP_RULE.to_string(),
        key.to_string(),
        value.to_string(),
    )]
    .into_iter()
    .collect()
}

/// The tag key every row here writes.
const TAG_KEY: &str = "owner";

/// A VPC delete leaves the tags of the security-group rules it orphans reported, and takes
/// the tags of the resources it DOES delete — the VPC itself and its default group, network
/// ACL and route table — with them.
#[tokio::test]
async fn a_vpc_delete_keeps_the_tags_of_the_rules_it_orphans() {
    let case = "a VPC delete orphans its default group's rules";
    let router = emulator();

    let vpc_id = create_vpc(&router, case, "10.0.0.0/16").await;
    let group_id = group_named(
        &router,
        case,
        &vpc_id,
        ec2::state::DEFAULT_SECURITY_GROUP_NAME,
    )
    .await;
    // A rule the CALLER wrote beside the ones the VPC's create wrote with its default group,
    // so the rule's author is one of the things the rows differ along.
    let authorized = authorize_ingress(&router, case, &group_id, "198.51.100.0/24", 443).await;

    let rule_ids = rules_of(&router, case, &group_id).await;
    assert!(
        rule_ids.len() >= 2 && rule_ids.contains(&authorized),
        "case {case:?}: expected the default group to hold the rules its create wrote plus \
         the authorized one {authorized:?}, got {rule_ids:?}"
    );
    for (index, rule_id) in rule_ids.iter().enumerate() {
        tag(&router, case, rule_id, TAG_KEY, &format!("rule-{index}")).await;
    }

    // The resources the delete itself removes, each tagged too: the VPC and the three
    // default resources "when you delete the VPC, it deletes the default security group,
    // network ACL, and route table for the VPC" (`DeleteVpc`) names.
    let acl_body = ok(
        &router,
        case,
        "DescribeNetworkAcls",
        &filters(&[("vpc-id", &[vpc_id.as_str()])]),
    )
    .await;
    let acl_id = element(&acl_body, "networkAclId")
        .unwrap_or_else(|| panic!("case {case:?}: expected a <networkAclId> in {acl_body}"));
    let table_body = ok(
        &router,
        case,
        "DescribeRouteTables",
        &filters(&[("vpc-id", &[vpc_id.as_str()])]),
    )
    .await;
    let route_table_id = element(&table_body, "routeTableId")
        .unwrap_or_else(|| panic!("case {case:?}: expected a <routeTableId> in {table_body}"));
    for resource_id in [&vpc_id, &group_id, &acl_id, &route_table_id] {
        tag(&router, case, resource_id, TAG_KEY, "deleted-with-the-vpc").await;
    }

    ok(&router, case, "DeleteVpc", &[param("VpcId", &vpc_id)]).await;

    for (index, rule_id) in rule_ids.iter().enumerate() {
        let expected = rule_tag_row(rule_id, TAG_KEY, &format!("rule-{index}"));
        let actual = reported_tags(&router, case, rule_id).await;
        assert_eq!(
            actual, expected,
            "case {case:?}: the tag on the orphaned rule {rule_id} must survive the VPC \
             delete: expected {expected:?}, got {actual:?}"
        );
    }
    for resource_id in [&vpc_id, &group_id, &acl_id, &route_table_id] {
        let actual = reported_tags(&router, case, resource_id).await;
        assert!(
            actual.is_empty(),
            "case {case:?}: the tags of {resource_id}, which the delete removes, must go with \
             it, got {actual:?}"
        );
    }
}

/// A group delete orphans its rules on the same terms, while a REVOKE — which deletes the
/// rule itself — takes that rule's tags with it.
#[tokio::test]
async fn a_revoked_rule_loses_its_tags_and_a_deleted_group_orphans_them() {
    let case = "a revoke deletes a rule, a group delete orphans one";
    let router = emulator();

    let vpc_id = create_vpc(&router, case, "10.1.0.0/16").await;
    let group_body = ok(
        &router,
        case,
        "CreateSecurityGroup",
        &[
            param("GroupName", "orphaning-group"),
            param("GroupDescription", "a group whose rules are tagged"),
            param("VpcId", &vpc_id),
        ],
    )
    .await;
    let group_id = element(&group_body, "groupId")
        .unwrap_or_else(|| panic!("case {case:?}: expected a <groupId> in {group_body}"));

    let revoked = authorize_ingress(&router, case, &group_id, "203.0.113.0/24", 22).await;
    let orphaned = authorize_ingress(&router, case, &group_id, "192.0.2.0/24", 80).await;
    tag(&router, case, &revoked, TAG_KEY, "revoked").await;
    tag(&router, case, &orphaned, TAG_KEY, "orphaned").await;

    ok(
        &router,
        case,
        "RevokeSecurityGroupIngress",
        &[
            param("GroupId", &group_id),
            param("SecurityGroupRuleId.1", &revoked),
        ],
    )
    .await;
    let after_revoke = reported_tags(&router, case, &revoked).await;
    assert!(
        after_revoke.is_empty(),
        "case {case:?}: a revoke deletes the rule {revoked}, so its tags go with it, got \
         {after_revoke:?}"
    );
    // The rule the revoke did not name keeps its tag, so the revoke removed one rule's tags
    // rather than the group's.
    let untouched = reported_tags(&router, case, &orphaned).await;
    assert_eq!(
        untouched,
        rule_tag_row(&orphaned, TAG_KEY, "orphaned"),
        "case {case:?}: the rule {orphaned} the revoke did not name keeps its tag, got \
         {untouched:?}"
    );

    ok(
        &router,
        case,
        "DeleteSecurityGroup",
        &[param("GroupId", &group_id)],
    )
    .await;
    let after_delete = reported_tags(&router, case, &orphaned).await;
    assert_eq!(
        after_delete,
        rule_tag_row(&orphaned, TAG_KEY, "orphaned"),
        "case {case:?}: the group delete orphans the rule {orphaned}, so its tag survives, \
         got {after_delete:?}"
    );
}

/// What a delete RETAINS is bounded, like every other record this store never reclaims: a
/// retained tag record costs the byte budget at least a stored record's overhead, and once
/// the store is at the count ceiling such a store is held to, a removed rule's tags go with
/// the rule instead of accumulating.
#[tokio::test]
async fn what_a_delete_retains_is_charged_and_capped() {
    use ec2::state::{
        ComAmazonawsEc2Amazonec2State as Ec2State, MAX_STORED_RECORDS_PER_STORE,
        STORED_RECORD_OVERHEAD_BYTES,
    };

    /// Create a VPC, tag the first rule of its default group when `tag_the_rule`, delete the
    /// VPC, and answer the bytes the store holds afterwards.
    async fn delete_cycle(case: &str, tag_the_rule: bool) -> usize {
        let (router, engine) = super::ec2_query::emulator();
        let vpc_id = create_vpc(&router, case, "10.4.0.0/16").await;
        let group_id = group_named(
            &router,
            case,
            &vpc_id,
            ec2::state::DEFAULT_SECURITY_GROUP_NAME,
        )
        .await;
        let rule_id = rules_of(&router, case, &group_id)
            .await
            .first()
            .unwrap_or_else(|| panic!("case {case:?}: expected the default group to hold a rule"))
            .clone();
        if tag_the_rule {
            tag(&router, case, &rule_id, TAG_KEY, "orphaned").await;
        }
        ok(&router, case, "DeleteVpc", &[param("VpcId", &vpc_id)]).await;
        engine.state::<Ec2State>().lock().stored_bytes()
    }

    // The same cycle with and without the tag: the difference is what retaining it costs.
    let with_tag = delete_cycle("a delete that retains a tag", true).await;
    let without_tag = delete_cycle("a delete that retains nothing", false).await;
    let charged = with_tag.saturating_sub(without_tag);
    assert!(
        charged >= STORED_RECORD_OVERHEAD_BYTES,
        "expected a retained tag record to cost at least {STORED_RECORD_OVERHEAD_BYTES} \
         bytes against the budget, got {charged} ({with_tag} held with the tag, \
         {without_tag} without)"
    );

    // At the count ceiling, nothing further is retained: the tags go with the rule, which is
    // what keeps repeated delete cycles from growing this process.
    let case = "a delete at the retention ceiling";
    let (router, engine) = super::ec2_query::emulator();
    {
        let state = engine.state::<Ec2State>();
        let mut guard = state.lock();
        for index in 0..MAX_STORED_RECORDS_PER_STORE {
            guard
                .orphaned_security_group_rule_tags
                .insert(format!("sgr-held-{index:016x}"), Vec::new());
        }
    }
    let vpc_id = create_vpc(&router, case, "10.5.0.0/16").await;
    let group_id = group_named(
        &router,
        case,
        &vpc_id,
        ec2::state::DEFAULT_SECURITY_GROUP_NAME,
    )
    .await;
    let rule_id = rules_of(&router, case, &group_id)
        .await
        .first()
        .unwrap_or_else(|| panic!("case {case:?}: expected the default group to hold a rule"))
        .clone();
    tag(&router, case, &rule_id, TAG_KEY, "dropped-at-the-ceiling").await;
    ok(&router, case, "DeleteVpc", &[param("VpcId", &vpc_id)]).await;

    let held = engine
        .state::<Ec2State>()
        .lock()
        .orphaned_security_group_rule_tags
        .len();
    assert_eq!(
        held, MAX_STORED_RECORDS_PER_STORE,
        "case {case:?}: at the ceiling the store must retain nothing further, it holds {held}"
    );
    let actual = reported_tags(&router, case, &rule_id).await;
    assert!(
        actual.is_empty(),
        "case {case:?}: past the ceiling the rule's tags go with the rule, got {actual:?}"
    );
}

/// A tagging call naming a rule a delete orphaned is refused as not found: the rule is gone,
/// so the surviving tag record is reported and nothing more.
#[tokio::test]
async fn an_orphaned_rule_id_names_no_rule_a_tagging_call_can_reach() {
    let case = "an orphaned rule id resolves to no rule";
    let router = emulator();

    let vpc_id = create_vpc(&router, case, "10.2.0.0/16").await;
    let group_id = group_named(
        &router,
        case,
        &vpc_id,
        ec2::state::DEFAULT_SECURITY_GROUP_NAME,
    )
    .await;
    let rule_ids = rules_of(&router, case, &group_id).await;
    let rule_id = rule_ids
        .first()
        .unwrap_or_else(|| panic!("case {case:?}: expected the default group to hold a rule"))
        .clone();
    tag(&router, case, &rule_id, TAG_KEY, "orphaned").await;
    ok(&router, case, "DeleteVpc", &[param("VpcId", &vpc_id)]).await;

    for (what, action) in [("a write", "CreateTags"), ("a delete", "DeleteTags")] {
        let (status, body) = call(
            &router,
            REGION,
            action,
            &[
                param("ResourceId.1", &rule_id),
                param("Tag.1.Key", TAG_KEY),
                param("Tag.1.Value", "written-after-the-delete"),
            ],
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: {what} naming the orphaned rule {rule_id} must be refused, got \
             {status} with body {body}"
        );
        let code = super::ec2_query::error_code(&body);
        assert_eq!(
            code, "InvalidSecurityGroupRuleId.NotFound",
            "case {case:?}: {what} naming the orphaned rule must be refused as not found, got \
             {code:?} in {body}"
        );
    }

    // The report still carries what was written before the delete, unchanged by the refused
    // calls above.
    let actual = reported_tags(&router, case, &rule_id).await;
    assert_eq!(
        actual,
        rule_tag_row(&rule_id, TAG_KEY, "orphaned"),
        "case {case:?}: the surviving tag of {rule_id} must be reported as written, got \
         {actual:?}"
    );
}
