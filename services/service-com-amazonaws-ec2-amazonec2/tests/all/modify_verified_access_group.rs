// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for `ModifyVerifiedAccessGroup`, driven over the wire the service actually
//! speaks — `ec2Query`, through the crate's shared [`super::ec2_query`] harness.
//!
//! The rule under test: a modify APPLIES THE MEMBERS THE REQUEST CARRIES to the stored Verified
//! Access group, leaves every member it does not carry as it was, stamps the group's
//! `LastUpdatedTime`, and answers the record the store now holds — so a later read of the same
//! group reports what the modify left behind. Around that:
//!
//! 1. every shape a modify request can take applies exactly what it carries, and the members
//!    the service owns (the group id, its ARN, the owning account, `CreationTime`) are not
//!    touched; the answer is a report of the STORED group, so a description nothing ever set
//!    reads as empty text;
//! 2. the answer is the store's record rather than the request echoed back, so the operations
//!    that RESOLVE a group — `CreateVerifiedAccessEndpoint`, which reads the instance of an
//!    endpoint off its group, and `DescribeVerifiedAccessEndpoints` — report the instance the
//!    modify moved the group to;
//! 3. every EC2 resource the request references must be one the account holds: a group id and
//!    an instance id that resolve to nothing are each refused at HTTP 400 with that resource's
//!    own `Invalid<Kind>.NotFound` code, naming the id the request gave, and NOTHING is
//!    applied — and so is a client token past the bound this service accepts;
//! 4. one `ClientToken` names one modification — a repeat applies nothing further, and the same
//!    token carrying a DIFFERENT request is refused rather than applied twice — and a `DryRun`
//!    reports the outcome the request would have had while writing nothing.
//!
//! Each table row is a distinct input along what the rule generalises over — which members the
//! request carries, whether the group already held a value for them, which reference is
//! unresolvable and in which form, how long a client token is — so no row is the
//! specification.
//!
//! The instances the groups hang off, and the VPC resources an endpoint on one names, are
//! seeded through the same insert seams the creating operations land on, the way the
//! [`super::verified_access_groups`] suite seeds them.

use ec2::ops::types::VerifiedAccessInstance;
use ec2::state::{
    availability_zones_in_region, ComAmazonawsEc2Amazonec2State as Ec2State, SecurityGroupSpec,
    SubnetSpec, MAX_CLIENT_TOKEN_CHARS,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{as_timestamp, call, element, elements, error_code, error_message, indexed};

/// The Region every call here is signed for: the group's ARN names it, so a Region that is not
/// the emulator's default is the stronger case.
const REGION: &str = "eu-west-1";

/// A `(key, value)` request parameter.
fn p(key: &str, value: impl Into<String>) -> (String, String) {
    (key.to_string(), value.into())
}

/// One request's form parameters, as the ec2Query harness takes them.
type Params = Vec<(String, String)>;

/// What the seeded store holds, for the tables to name rows by.
struct Fixture {
    /// Two Verified Access instances a group may belong to.
    instance_ids: Vec<String>,
    /// A subnet an endpoint on a modified group may name.
    subnet_id: String,
    /// A security group an endpoint on a modified group may be associated with.
    security_group_id: String,
}

/// Stand up an emulator and seed the resources a create and a modify reference, the way the
/// creating operations would.
fn emulator() -> (MultiServiceRouter, std::sync::Arc<engine::InMemoryEngine>, Fixture) {
    let (router, engine) = super::ec2_query::emulator();
    let fixture = seed(&engine);
    (router, engine, fixture)
}

/// Write the referenced resources into the engine's store and report the ids they were given.
fn seed(engine: &engine::InMemoryEngine) -> Fixture {
    let store = engine.state::<Ec2State>();
    let mut guard = store.lock();

    let instance_ids: Vec<String> = ["the sales applications", "the support applications"]
        .into_iter()
        .map(|description| {
            guard.insert_verified_access_instance(
                VerifiedAccessInstance {
                    description: Some(description.to_string()),
                    ..VerifiedAccessInstance::default()
                },
                None,
            )
        })
        .collect();

    let vpc_id = guard.insert_vpc("10.0.0.0/16", false, "default", "available", REGION, Vec::new());
    let zone = &availability_zones_in_region(REGION)[0];
    let subnet_id = guard.insert_subnet(SubnetSpec {
        vpc_id: vpc_id.clone(),
        cidr_block: "10.0.0.0/24".to_string(),
        availability_zone: zone.zone_name.clone(),
        availability_zone_id: zone.zone_id.clone(),
        region: REGION.to_string(),
        state: "available".to_string(),
        default_for_az: false,
        map_public_ip_on_launch: false,
        tags: Vec::new(),
    });
    let security_group_id = guard.insert_security_group(SecurityGroupSpec {
        vpc_id,
        group_name: "verified-access".to_string(),
        description: "reaches the application".to_string(),
        region: REGION.to_string(),
        tags: Vec::new(),
        ip_permissions: Vec::new(),
        ip_permissions_egress: Vec::new(),
    });

    Fixture {
        instance_ids,
        subnet_id,
        security_group_id,
    }
}

/// One `CreateVerifiedAccessGroup` call on `instance_id`, answering the id it minted and the
/// body it answered. Every group under test carries a tag and no server-side-encryption
/// specification: neither is a member a modify carries, so both must read back untouched.
async fn create_group(
    router: &MultiServiceRouter,
    case: &str,
    instance_id: &str,
    extra: Params,
) -> (String, String) {
    let mut params = vec![
        p("VerifiedAccessInstanceId", instance_id.to_string()),
        p("TagSpecification.1.ResourceType", "verified-access-group"),
        p("TagSpecification.1.Tag.1.Key", "Team"),
        p("TagSpecification.1.Tag.1.Value", "platform"),
    ];
    params.extend(extra);
    let (status, body) = call(router, REGION, "CreateVerifiedAccessGroup", &params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the create under test must succeed, got {status} with body {body}"
    );
    let group_id = element(&body, "verifiedAccessGroupId").unwrap_or_else(|| {
        panic!("case {case:?}: the create must report the group it minted, got {body}")
    });
    (group_id, body)
}

/// One `ModifyVerifiedAccessGroup` call on `group_id`, with `params` beside the id.
async fn modify(router: &MultiServiceRouter, group_id: &str, params: &[(String, String)]) -> (u16, String) {
    let mut body = vec![p("VerifiedAccessGroupId", group_id)];
    body.extend_from_slice(params);
    call(router, REGION, "ModifyVerifiedAccessGroup", &body).await
}

/// The parameters of one `CreateVerifiedAccessEndpoint` on `group_id`: the endpoint names no
/// instance at all, so the instance it reports is the one read off the stored GROUP.
fn endpoint_params(group_id: &str, cidr: &str, fixture: &Fixture) -> Params {
    [
        vec![
            p("VerifiedAccessGroupId", group_id.to_string()),
            p("EndpointType", "cidr"),
            p("AttachmentType", "vpc"),
            p("CidrOptions.Protocol", "tcp"),
            p("CidrOptions.Cidr", cidr.to_string()),
            p("CidrOptions.PortRange.1.FromPort", "5000"),
            p("CidrOptions.PortRange.1.ToPort", "5000"),
        ],
        indexed("CidrOptions.SubnetId", &[fixture.subnet_id.as_str()]),
        indexed("SecurityGroupId", &[fixture.security_group_id.as_str()]),
    ]
    .concat()
}

/// The `(description, instance id)` the STORE kept for `group_id`, read off the state the
/// handlers share.
fn stored(engine: &engine::InMemoryEngine, group_id: &str) -> (Option<String>, Option<String>) {
    let state = engine.state::<Ec2State>();
    let guard = state.lock();
    let record = guard.verified_access_groups.get(group_id);
    (
        record.and_then(|record| record.view.description.clone()),
        record.and_then(|record| record.view.verified_access_instance_id.clone()),
    )
}

/// RULE: each shape a modify request can take applies exactly the members it carries, leaves
/// the ones it does not carry as they were, and the service's own members — the group id, its
/// ARN, the owning account and the creation time — are not touched. The answer is a report of
/// the stored record, so `DeleteVerifiedAccessGroup`, which answers the group as the store held
/// it, reports the same group afterwards.
#[tokio::test]
async fn a_modify_applies_what_it_carries_and_leaves_the_rest() {
    // (case, the create's optional members, the modify's members, the instance the modify
    // names when it names one, the description the group must then report).
    type Row = (&'static str, Params, Params, Option<usize>, &'static str);
    let rows: Vec<Row> = vec![
        (
            "a description onto a group that had none",
            Vec::new(),
            vec![p("Description", "the sales applications")],
            None,
            "the sales applications",
        ),
        (
            "a description over one the create set",
            vec![p("Description", "the sales applications")],
            vec![p("Description", "the support applications")],
            None,
            "the support applications",
        ),
        (
            "the group onto the other instance",
            vec![p("Description", "the sales applications")],
            Vec::new(),
            Some(1),
            "the sales applications",
        ),
        (
            "both members at once",
            Vec::new(),
            vec![p("Description", "the support applications")],
            Some(1),
            "the support applications",
        ),
        (
            "the instance the group is already on",
            vec![p("Description", "the sales applications")],
            Vec::new(),
            Some(0),
            "the sales applications",
        ),
        (
            "no member at all beside the group id",
            vec![p("Description", "the sales applications")],
            Vec::new(),
            None,
            "the sales applications",
        ),
        (
            "no member on a group no caller gave a description",
            Vec::new(),
            Vec::new(),
            None,
            "",
        ),
        (
            "a description emptied again",
            vec![p("Description", "the sales applications")],
            vec![p("Description", "")],
            None,
            "",
        ),
    ];

    for (case, created_with, modified_with, names_instance, description) in rows {
        let (router, engine, fixture) = emulator();
        let (group_id, created) =
            create_group(&router, case, &fixture.instance_ids[0], created_with).await;

        // The instance the modify NAMES: instance 1 is the modify that moves the group,
        // instance 0 one that names the instance the group is already on, and no instance at
        // all leaves the group where it was — so every form of that member is exercised.
        let instance = names_instance.unwrap_or(0);
        let mut params = modified_with.clone();
        if let Some(index) = names_instance {
            params.push(p(
                "VerifiedAccessInstanceId",
                fixture.instance_ids[index].clone(),
            ));
        }

        let (status, body) = modify(&router, &group_id, &params).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} with body {body}"
        );
        assert_eq!(
            element(&body, "verifiedAccessGroupId").as_deref(),
            Some(group_id.as_str()),
            "case {case:?}: expected the group the request named, got {:?}",
            element(&body, "verifiedAccessGroupId")
        );
        assert_eq!(
            element(&body, "description").as_deref(),
            Some(description),
            "case {case:?}: expected description {description:?}, got {:?}",
            element(&body, "description")
        );
        assert_eq!(
            element(&body, "verifiedAccessInstanceId").as_deref(),
            Some(fixture.instance_ids[instance].as_str()),
            "case {case:?}: expected the group on instance {instance}, got {:?}",
            element(&body, "verifiedAccessInstanceId")
        );

        // The members the SERVICE owns are not touched by a modify: the ARN, the owning
        // account and the creation time read back exactly as the create answered them.
        for member in ["verifiedAccessGroupArn", "owner", "creationTime"] {
            assert_eq!(
                element(&body, member),
                element(&created, member),
                "case {case:?}: a modify does not move {member}, got {:?} where the create \
                 answered {:?}",
                element(&body, member),
                element(&created, member)
            );
        }
        // The members a modify carries no value for are the group's own: its tags and the
        // server-side-encryption block the create derived.
        assert_eq!(
            element(&body, "key").as_deref(),
            Some("Team"),
            "case {case:?}: a modify carries no tag, so the group keeps the create's, got {body}"
        );
        assert_eq!(
            element(&body, "customerManagedKeyEnabled"),
            element(&created, "customerManagedKeyEnabled"),
            "case {case:?}: a modify carries no encryption member, so the group keeps the \
             create's, got {:?}",
            element(&body, "customerManagedKeyEnabled")
        );
        assert_eq!(
            element(&body, "deletionTime"),
            None,
            "case {case:?}: a modify deletes nothing, so the group has no deletion time, got \
             {:?}",
            element(&body, "deletionTime")
        );

        // The last updated time is the instant this modification was applied — so it is at or
        // after the creation time the create answered.
        let last_updated = element(&body, "lastUpdatedTime").unwrap_or_default();
        let creation_time = element(&created, "creationTime").unwrap_or_default();
        assert!(
            as_timestamp(&last_updated)
                .zip(as_timestamp(&creation_time))
                .is_some_and(|(updated, created)| updated >= created),
            "case {case:?}: expected an ISO 8601 lastUpdatedTime at or after the creationTime \
             {creation_time:?}, got {last_updated:?}"
        );

        // RULE, read back: the answer is the STORE's record, not the request echoed. The store
        // itself, and the delete — which reports the group as the store held it.
        let (stored_description, stored_instance) = stored(&engine, &group_id);
        assert_eq!(
            stored_description.as_deref().unwrap_or_default(),
            description,
            "case {case:?}: expected the store to keep description {description:?}, got \
             {stored_description:?}"
        );
        assert_eq!(
            stored_instance.as_deref(),
            Some(fixture.instance_ids[instance].as_str()),
            "case {case:?}: expected the store to keep the group on instance {instance}, got \
             {stored_instance:?}"
        );
        let (status, deleted) = call(
            &router,
            REGION,
            "DeleteVerifiedAccessGroup",
            &[p("VerifiedAccessGroupId", group_id.clone())],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the read-back delete must succeed, got {status} with body {deleted}"
        );
        assert_eq!(
            element(&deleted, "description").as_deref(),
            Some(description),
            "case {case:?}: expected a later read to report description {description:?}, got \
             {:?}",
            element(&deleted, "description")
        );
        assert_eq!(
            element(&deleted, "verifiedAccessInstanceId").as_deref(),
            Some(fixture.instance_ids[instance].as_str()),
            "case {case:?}: expected a later read to report instance {instance}, got {:?}",
            element(&deleted, "verifiedAccessInstanceId")
        );
        assert_eq!(
            element(&deleted, "lastUpdatedTime").as_deref(),
            Some(last_updated.as_str()),
            "case {case:?}: expected a later read to report the lastUpdatedTime the modify \
             stamped, got {:?}",
            element(&deleted, "lastUpdatedTime")
        );
    }
}

/// RULE, through the operations that RESOLVE a group: an endpoint belongs to the instance its
/// group belongs to, so a modify that moves the group onto another instance is what a later
/// `CreateVerifiedAccessEndpoint` reads off it — and the endpoints the group already had report
/// that instance too, rather than the one the group has left.
#[tokio::test]
async fn a_moved_group_is_read_back_through_its_endpoints() {
    let case = "a group moved to the other instance";
    let (router, _engine, fixture) = emulator();
    let (group_id, _) = create_group(
        &router,
        case,
        &fixture.instance_ids[0],
        vec![p("Description", "the group under test")],
    )
    .await;

    // An endpoint created BEFORE the move: the instance it reports comes from the group.
    let (status, first) = call(
        &router,
        REGION,
        "CreateVerifiedAccessEndpoint",
        &endpoint_params(&group_id, "10.0.0.0/25", &fixture),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the endpoint before the move must be accepted, got {status} with body \
         {first}"
    );
    assert_eq!(
        element(&first, "verifiedAccessInstanceId").as_deref(),
        Some(fixture.instance_ids[0].as_str()),
        "case {case:?}: expected the endpoint to report the instance of its group, got {:?}",
        element(&first, "verifiedAccessInstanceId")
    );

    let (status, moved) = modify(
        &router,
        &group_id,
        &[p("VerifiedAccessInstanceId", fixture.instance_ids[1].clone())],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the modify must succeed, got {status} with body {moved}"
    );

    // An endpoint created AFTER the move resolves the group out of the store, so it reports
    // the instance the modify left on it.
    let (status, second) = call(
        &router,
        REGION,
        "CreateVerifiedAccessEndpoint",
        &endpoint_params(&group_id, "10.0.0.128/25", &fixture),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the endpoint after the move must be accepted, got {status} with body \
         {second}"
    );
    assert_eq!(
        element(&second, "verifiedAccessInstanceId").as_deref(),
        Some(fixture.instance_ids[1].as_str()),
        "case {case:?}: expected the endpoint to report the instance the modify set, got {:?}",
        element(&second, "verifiedAccessInstanceId")
    );

    // And the endpoint that was already there: an endpoint cannot name an instance its group
    // does not belong to, so the describe reports the moved-to instance for both of them.
    let (status, described) = call(
        &router,
        REGION,
        "DescribeVerifiedAccessEndpoints",
        &[p("VerifiedAccessGroupId", group_id.clone())],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the describe must succeed, got {status} with body {described}"
    );
    let reported = elements(&described, "verifiedAccessInstanceId");
    assert_eq!(
        reported,
        vec![
            fixture.instance_ids[1].clone(),
            fixture.instance_ids[1].clone()
        ],
        "case {case:?}: expected both endpoints of the group to report the instance the modify \
         set, got {reported:?}"
    );
}

/// RULE's refusals: an id this account holds no group for modifies nothing, whatever the id
/// looks like, and so does an instance id it holds no instance for — each answered with that
/// resource's own `NotFound` code, naming the id the request gave.
#[tokio::test]
async fn a_modify_refuses_a_reference_the_account_does_not_hold() {
    // (case, an id no create minted) — the long and the short EC2 id form, an id of another
    // Verified Access resource kind, and a string that is no EC2 id at all.
    let unminted_groups: Vec<(&str, &str)> = vec![
        ("an unminted long-form group id", "vagr-0123456789abcdef0"),
        ("an unminted short-form group id", "vagr-01234567"),
        ("an instance id", "vai-0123456789abcdef0"),
        ("an id of no EC2 resource", "not-a-group"),
    ];
    for (case, group_id) in unminted_groups {
        let (router, engine, fixture) = emulator();
        let (held, _) = create_group(
            &router,
            case,
            &fixture.instance_ids[0],
            vec![p("Description", "the one group")],
        )
        .await;
        let (status, body) = modify(
            &router,
            group_id,
            &[p("Description", "a description nothing keeps")],
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400 for a group nothing minted, got {status} with \
             body {body}"
        );
        assert_eq!(
            error_code(&body),
            "InvalidVerifiedAccessGroupId.NotFound",
            "case {case:?}: expected the group's own NotFound code, got {body}"
        );
        assert_eq!(
            error_message(&body),
            format!("The verified-access-group ID '{group_id}' does not exist"),
            "case {case:?}: expected the refusal to name the id the request gave, got {}",
            error_message(&body)
        );
        assert_eq!(
            stored(&engine, &held).0.as_deref(),
            Some("the one group"),
            "case {case:?}: a refused modify changes no other group, got {:?}",
            stored(&engine, &held).0
        );
    }

    // The same four forms for the OTHER reference a modify carries: the instance it would move
    // the group to.
    let unminted_instances: Vec<(&str, &str)> = vec![
        ("an unminted long-form instance id", "vai-0123456789abcdef0"),
        ("an unminted short-form instance id", "vai-01234567"),
        ("a group id", "vagr-0123456789abcdef0"),
        ("an id of no EC2 resource", "not-an-instance"),
    ];
    for (case, instance_id) in unminted_instances {
        let (router, engine, fixture) = emulator();
        let (group_id, _) = create_group(
            &router,
            case,
            &fixture.instance_ids[0],
            vec![p("Description", "the one group")],
        )
        .await;
        let (status, body) = modify(
            &router,
            &group_id,
            &[
                p("VerifiedAccessInstanceId", instance_id),
                p("Description", "a description nothing keeps"),
            ],
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400 for an instance nothing minted, got {status} with \
             body {body}"
        );
        assert_eq!(
            error_code(&body),
            "InvalidVerifiedAccessInstanceId.NotFound",
            "case {case:?}: expected the instance's own NotFound code, got {body}"
        );
        assert_eq!(
            error_message(&body),
            format!("The verified-access-instance ID '{instance_id}' does not exist"),
            "case {case:?}: expected the refusal to name the id the request gave, got {}",
            error_message(&body)
        );
        assert_eq!(
            stored(&engine, &group_id),
            (
                Some("the one group".to_string()),
                Some(fixture.instance_ids[0].clone())
            ),
            "case {case:?}: a refused modify applies nothing, got {:?}",
            stored(&engine, &group_id)
        );
    }

    // The client token is caller-chosen text this call retains, so the bound this service
    // accepts holds here too: on the bound and one character past it.
    let token = |chars: usize| "t".repeat(chars);
    let rows: Vec<(&str, Params, u16, &str)> = vec![
        (
            "as long a client token as the service takes",
            vec![p("ClientToken", token(MAX_CLIENT_TOKEN_CHARS))],
            200,
            "",
        ),
        (
            "one client token character too many",
            vec![p("ClientToken", token(MAX_CLIENT_TOKEN_CHARS + 1))],
            400,
            "InvalidParameterValue",
        ),
    ];
    for (case, extra, expected_status, expected_code) in rows {
        let (router, engine, fixture) = emulator();
        let (group_id, _) = create_group(
            &router,
            case,
            &fixture.instance_ids[0],
            vec![p("Description", "the one group")],
        )
        .await;
        let mut params = vec![p("Description", "the modified group")];
        params.extend(extra);
        let (status, body) = modify(&router, &group_id, &params).await;
        assert_eq!(
            status, expected_status,
            "case {case:?}: expected HTTP {expected_status}, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            expected_code,
            "case {case:?}: expected the {expected_code:?} refusal, got {body}"
        );
        let expected_description = if expected_status == 200 {
            "the modified group"
        } else {
            "the one group"
        };
        assert_eq!(
            stored(&engine, &group_id).0.as_deref(),
            Some(expected_description),
            "case {case:?}: expected the store to hold {expected_description:?}, got {:?}",
            stored(&engine, &group_id).0
        );
    }
}

/// RULE's idempotency: one `ClientToken` names one modification, so a repeat of the request
/// applies nothing further — not even a second `LastUpdatedTime` — while the same token
/// carrying a different request is refused rather than applied a second time, and a token the
/// store has not seen applies a modification of its own.
#[tokio::test]
async fn one_client_token_names_one_modification() {
    let (router, engine, fixture) = emulator();
    let (group_id, _) = create_group(
        &router,
        "the group under test",
        &fixture.instance_ids[0],
        vec![
            p("ClientToken", "the-create-token"),
            p("Description", "the group under test"),
        ],
    )
    .await;

    let first_params = vec![
        p("ClientToken", "the-first-token"),
        p("Description", "the modified group"),
    ];
    let (status, body) = modify(&router, &group_id, &first_params).await;
    assert_eq!(
        status, 200,
        "the first modify must succeed, got {status} with body {body}"
    );
    let applied_at = element(&body, "lastUpdatedTime").unwrap_or_default();

    // (case, the members of the next call, its HTTP status, its error code, the description the
    // store must hold afterwards, whether its lastUpdatedTime must be the first's).
    type Row = (&'static str, Params, u16, &'static str, &'static str, bool);
    let rows: Vec<Row> = vec![
        (
            "the same token and the same request",
            first_params.clone(),
            200,
            "",
            "the modified group",
            true,
        ),
        (
            "the same token and a different description",
            vec![
                p("ClientToken", "the-first-token"),
                p("Description", "a description nothing keeps"),
            ],
            400,
            "IdempotentParameterMismatch",
            "the modified group",
            false,
        ),
        (
            "the same token and one member more",
            vec![
                p("ClientToken", "the-first-token"),
                p("Description", "the modified group"),
                p("VerifiedAccessInstanceId", fixture.instance_ids[0].clone()),
            ],
            400,
            "IdempotentParameterMismatch",
            "the modified group",
            false,
        ),
        (
            // A token is read against the request it was given with, so the token the CREATE
            // was given names no modification: this is a modification, not a replay of it.
            "the token the create was given",
            vec![
                p("ClientToken", "the-create-token"),
                p("Description", "modified under the create's token"),
            ],
            200,
            "",
            "modified under the create's token",
            false,
        ),
        (
            "a token the store has not seen",
            vec![
                p("ClientToken", "the-second-token"),
                p("Description", "modified again"),
            ],
            200,
            "",
            "modified again",
            false,
        ),
        (
            "no token at all",
            vec![p("Description", "modified without a token")],
            200,
            "",
            "modified without a token",
            false,
        ),
    ];

    for (case, params, expected_status, expected_code, expected_description, replayed) in rows {
        let (status, body) = modify(&router, &group_id, &params).await;
        assert_eq!(
            status, expected_status,
            "case {case:?}: expected HTTP {expected_status}, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            expected_code,
            "case {case:?}: expected the {expected_code:?} refusal, got {body}"
        );
        assert_eq!(
            stored(&engine, &group_id).0.as_deref(),
            Some(expected_description),
            "case {case:?}: expected the store to hold {expected_description:?}, got {:?}",
            stored(&engine, &group_id).0
        );
        if replayed {
            assert_eq!(
                element(&body, "lastUpdatedTime").as_deref(),
                Some(applied_at.as_str()),
                "case {case:?}: a replay applies nothing further, so the group keeps the \
                 lastUpdatedTime the first call stamped, got {:?}",
                element(&body, "lastUpdatedTime")
            );
        }
        // No row but the refused ones names the other instance, so a group still on the
        // instance it was created on is one no refused call wrote to.
        assert_eq!(
            stored(&engine, &group_id).1.as_deref(),
            Some(fixture.instance_ids[0].as_str()),
            "case {case:?}: expected the group to stay on the instance it was created on, got \
             {:?}",
            stored(&engine, &group_id).1
        );
    }
}

/// RULE's `DryRun`: the flag reports the outcome the request WOULD have had — the refusal when
/// it would have been refused — and writes nothing either way.
#[tokio::test]
async fn a_dry_run_reports_the_outcome_and_writes_nothing() {
    // (case, whether the request names the group that exists, the members beside the flag, the
    // HTTP status, the error code).
    type Row = (&'static str, bool, Params, u16, &'static str);
    let rows: Vec<Row> = vec![
        (
            "a modify that would have succeeded",
            true,
            vec![p("Description", "a description nothing keeps")],
            412,
            "DryRunOperation",
        ),
        (
            "a modify that carries no member to apply",
            true,
            Vec::new(),
            412,
            "DryRunOperation",
        ),
        (
            "a modify that would have been refused for its instance id",
            true,
            vec![p("VerifiedAccessInstanceId", "vai-0123456789abcdef0")],
            400,
            "InvalidVerifiedAccessInstanceId.NotFound",
        ),
        (
            "a modify that would have been refused for its client token",
            true,
            vec![p("ClientToken", "t".repeat(MAX_CLIENT_TOKEN_CHARS + 1))],
            400,
            "InvalidParameterValue",
        ),
        (
            "a modify that would have been refused for its group id",
            false,
            vec![p("Description", "a description nothing keeps")],
            400,
            "InvalidVerifiedAccessGroupId.NotFound",
        ),
    ];

    for (case, resolvable, extra, expected_status, expected_code) in rows {
        let (router, engine, fixture) = emulator();
        let (group_id, _) = create_group(
            &router,
            case,
            &fixture.instance_ids[0],
            vec![p("Description", "the one group")],
        )
        .await;
        let named = if resolvable {
            group_id.clone()
        } else {
            "vagr-0123456789abcdef0".to_string()
        };
        let mut params = vec![p("DryRun", "true")];
        params.extend(extra);
        let (status, body) = modify(&router, &named, &params).await;
        assert_eq!(
            status, expected_status,
            "case {case:?}: expected HTTP {expected_status}, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            expected_code,
            "case {case:?}: expected the {expected_code:?} refusal, got {body}"
        );
        assert_eq!(
            stored(&engine, &group_id),
            (
                Some("the one group".to_string()),
                Some(fixture.instance_ids[0].clone())
            ),
            "case {case:?}: a dry run writes nothing, got {:?}",
            stored(&engine, &group_id)
        );
    }
}
