// SPDX-License-Identifier: CC0-1.0

//! The sentences Amazon EC2 Auto Scaling states for three client errors, over the awsQuery
//! wire.
//!
//! The rule under test: a refusal carries the SERVICE's own sentence for that error class —
//! its fixed text exactly, with only the parts the service fills taken from the request or
//! from the stored resource, and nothing of the emulator's own invention. Three classes are
//! driven here, each over a table spanning what its rule generalises over:
//!
//! * a group configuration that states no placement at all — neither Availability Zone, nor
//!   zone ID, nor subnet — is refused with the sentence naming the three members ONE of which
//!   has to be stated, which quotes nothing from the request;
//! * a read that asks `DescribeAutoScalingGroups` to select groups by the group's OWN NAME is
//!   told that THAT criterion is not supported, while a filter naming no criterion the service
//!   knows is still answered with the filter types that ARE valid;
//! * `DetachLoadBalancerTargetGroups` refuses a group the account does not hold in its own
//!   absent-group form, whose detail field is left unfilled — so the sentence is the same
//!   whatever name the request carried.
//!
//! Every table carries a row the service must still SERVE, so a refusal cannot pass by
//! refusing everything, and the created groups are read back through
//! `DescribeAutoScalingGroups` so no row passes on an accepted-but-unstored change.

use smithy_server_runtime::MultiServiceRouter;

use crate::support::{borrowed, create_target_group, emulator, list_members, p, query,
                     seed_subnets, tag_value, tag_values, vpc_of, xml_unescape};

/// The launch source every group here is created from — a launch TEMPLATE, because no
/// account can create a launch configuration (see `launch_configurations_are_unavailable.rs`).
const LAUNCH_SOURCE: (&str, &str) = ("LaunchTemplate.LaunchTemplateName", "lt-1");

/// The whole message a group configuration that names no place to launch into is refused
/// with: the three members one of which states the placement, and nothing taken from the
/// request, which named none of them.
const NO_PLACEMENT_MESSAGE: &str =
    "You must specify 1 of either AvailabilityZones, AvailabilityZoneIds, or Subnets";

/// The whole message a read asking for the groups to be selected by the group's own name is
/// refused with: the fixed statement that the criteria are not valid, then the criterion that
/// is not supported.
const FILTER_BY_GROUP_NAME_MESSAGE: &str = "The filter criteria isn't valid. Filtering Auto \
                                            Scaling groups by group name isn't supported.";

/// The whole message a read carrying a filter that names no criterion this service knows is
/// refused with: it reports the filter types that ARE valid and quotes nothing from the
/// request. The literal `tag:<key>` is part of the sentence.
const VALID_FILTER_TYPES_MESSAGE: &str = "The filter criteria isn't valid. Valid filter types \
                                          for the Name attribute of filters are: tag-key, \
                                          tag-value and tag:<key>.";

/// The absent-group refusal in the form whose detail field the operation leaves unfilled, so
/// it travels as the literal token `null` and carries no part of the request.
const ABSENT_GROUP_WITHOUT_DETAIL: &str = "AutoScalingGroup name not found - null";

/// The error type, the wire code and the unescaped message of one answer.
fn refusal(body: &str) -> (String, String, String) {
    (
        tag_value(body, "Type"),
        tag_value(body, "Code"),
        xml_unescape(&tag_value(body, "Message")),
    )
}

/// Everything a create needs BESIDE its placement: the name, the launch template the account
/// holds and bounds that launch nothing.
fn group_without_placement(name: &str) -> Vec<(String, String)> {
    vec![
        p("AutoScalingGroupName", name),
        p(LAUNCH_SOURCE.0, LAUNCH_SOURCE.1),
        p("MinSize", "0"),
        p("MaxSize", "1"),
        p("DesiredCapacity", "0"),
    ]
}

/// Every group name the account holds, read back through the implemented reader.
async fn described_group_names(router: &MultiServiceRouter) -> Vec<String> {
    let (status, body) = query(router, "DescribeAutoScalingGroups", &[]).await;
    assert_eq!(
        status, 200,
        "DescribeAutoScalingGroups expected 200, got {status} with {body:?}"
    );
    tag_values(&body, "AutoScalingGroupName")
}

/// A group configuration that states no place to launch an instance into is refused with the
/// service's own sentence, whichever way the request manages to state nothing — and the
/// create stores no group.
#[tokio::test]
async fn a_group_stating_no_placement_is_refused_with_the_services_sentence() {
    let (router, engine) = emulator();
    let subnets = seed_subnets(&engine, 1);

    // (case, the placement members the create carries) — the members left out altogether,
    // and a `VPCZoneIdentifier` whose value names no subnet, since the ids it names are what
    // its commas separate. The member has a documented minimum length, so the value that
    // names no subnet is never the empty one.
    let cases: Vec<(&str, Vec<(String, String)>)> = vec![
        ("no placement member at all", vec![]),
        (
            "a VPCZoneIdentifier of one comma",
            vec![p("VPCZoneIdentifier", ",")],
        ),
        (
            "a VPCZoneIdentifier of commas and spaces",
            vec![p("VPCZoneIdentifier", " , , ")],
        ),
        (
            "a VPCZoneIdentifier of spaces alone",
            vec![p("VPCZoneIdentifier", "   ")],
        ),
    ];

    for (index, (case, placement)) in cases.iter().enumerate() {
        let name = format!("asg-no-placement-{index}");
        let mut params = group_without_placement(&name);
        params.extend(placement.iter().cloned());
        let (status, body) = query(&router, "CreateAutoScalingGroup", &borrowed(&params)).await;
        let (error_type, code, message) = refusal(&body);
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {body:?}"
        );
        assert_eq!(
            (error_type.as_str(), code.as_str()),
            ("Sender", "ValidationError"),
            "case {case:?}: expected a Sender ValidationError, got {body:?}"
        );
        assert_eq!(
            message, NO_PLACEMENT_MESSAGE,
            "case {case:?}: expected the message {NO_PLACEMENT_MESSAGE:?}, got {message:?}"
        );
        let held = described_group_names(&router).await;
        assert!(
            !held.contains(&name),
            "case {case:?}: the refused create must store no group, got {held:?}"
        );
    }

    // The controls: the same create is SERVED when it states the placement either way — as
    // the zone a subnet of the account is in, or as that subnet.
    let served: Vec<(&str, (String, String))> = vec![
        (
            "the zone the seeded subnet is in",
            p("AvailabilityZones.member.1", subnets[0].zone.clone()),
        ),
        (
            "the seeded subnet itself",
            p("VPCZoneIdentifier", subnets[0].id.clone()),
        ),
    ];
    for (index, (case, placement)) in served.into_iter().enumerate() {
        let name = format!("asg-placed-{index}");
        let mut params = group_without_placement(&name);
        params.push(placement);
        let (status, body) = query(&router, "CreateAutoScalingGroup", &borrowed(&params)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: a stated placement must be served, got {status} with {body:?}"
        );
        let held = described_group_names(&router).await;
        assert!(
            held.contains(&name),
            "case {case:?}: the created group must be readable back, got {held:?}"
        );
    }
}

/// Create one group with a tag, placed in the zone of a seeded subnet, and fail the test if
/// the create is refused.
async fn create_tagged_group(router: &MultiServiceRouter, name: &str, zone: &str) {
    let mut params = group_without_placement(name);
    params.push(p("AvailabilityZones.member.1", zone));
    params.push(p("Tags.member.1.Key", "env"));
    params.push(p("Tags.member.1.Value", "prod"));
    params.push(p("Tags.member.1.PropagateAtLaunch", "false"));
    let (status, body) = query(router, "CreateAutoScalingGroup", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "creating {name:?} must succeed, got {status} with {body:?}"
    );
}

/// A read that asks for the groups to be selected by the group's own name is told WHICH
/// criterion is unsupported, and a filter naming no criterion the service knows is still
/// answered with the valid filter types.
#[tokio::test]
async fn a_read_filtering_by_group_name_is_told_which_criterion_is_unsupported() {
    let (router, engine) = emulator();
    let subnets = seed_subnets(&engine, 1);
    create_tagged_group(&router, "asg-filtered", &subnets[0].zone).await;
    create_tagged_group(&router, "asg-other", &subnets[0].zone).await;

    // (case, the parameters the read carries) — the criterion asked for by the filter type
    // that names the group's own name member, and asked for by naming the groups beside a
    // filter: one value and several, one criterion and two, with and without the names.
    let by_group_name: Vec<(&str, Vec<(&str, &str)>)> = vec![
        (
            "the group-name criterion with one value",
            vec![
                ("Filters.member.1.Name", "auto-scaling-group-name"),
                ("Filters.member.1.Values.member.1", "asg-filtered"),
            ],
        ),
        (
            "the group-name criterion with two values",
            vec![
                ("Filters.member.1.Name", "auto-scaling-group-name"),
                ("Filters.member.1.Values.member.1", "asg-filtered"),
                ("Filters.member.1.Values.member.2", "asg-other"),
            ],
        ),
        (
            "the group-name criterion carrying no value",
            vec![("Filters.member.1.Name", "auto-scaling-group-name")],
        ),
        (
            "the group-name criterion after a tag criterion",
            vec![
                ("Filters.member.1.Name", "tag-key"),
                ("Filters.member.1.Values.member.1", "env"),
                ("Filters.member.2.Name", "auto-scaling-group-name"),
                ("Filters.member.2.Values.member.1", "asg-filtered"),
            ],
        ),
        (
            "the group-name criterion beside the group names",
            vec![
                ("Filters.member.1.Name", "auto-scaling-group-name"),
                ("Filters.member.1.Values.member.1", "asg-filtered"),
                ("AutoScalingGroupNames.member.1", "asg-filtered"),
            ],
        ),
        (
            "a tag-key criterion beside one group name",
            vec![
                ("Filters.member.1.Name", "tag-key"),
                ("Filters.member.1.Values.member.1", "env"),
                ("AutoScalingGroupNames.member.1", "asg-filtered"),
            ],
        ),
        (
            "a tag:<key> criterion beside two group names",
            vec![
                ("Filters.member.1.Name", "tag:env"),
                ("Filters.member.1.Values.member.1", "prod"),
                ("AutoScalingGroupNames.member.1", "asg-filtered"),
                ("AutoScalingGroupNames.member.2", "asg-other"),
            ],
        ),
    ];

    for (case, params) in &by_group_name {
        let (status, body) = query(&router, "DescribeAutoScalingGroups", params).await;
        let (error_type, code, message) = refusal(&body);
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {body:?}"
        );
        assert_eq!(
            (error_type.as_str(), code.as_str()),
            ("Sender", "ValidationError"),
            "case {case:?}: expected a Sender ValidationError, got {body:?}"
        );
        assert_eq!(
            message, FILTER_BY_GROUP_NAME_MESSAGE,
            "case {case:?}: expected the message {FILTER_BY_GROUP_NAME_MESSAGE:?}, got \
             {message:?}"
        );
        assert!(
            tag_values(&body, "AutoScalingGroupName").is_empty(),
            "case {case:?}: a refused read reports no group, got {body:?}"
        );
    }

    // The other side of the same rule: a filter name that asks for no criterion the service
    // knows is still answered with the filter types that are valid, so the sentence above
    // belongs to the criterion and not to every unusable filter.
    let unknown_criteria = [
        ("a name no operation documents", "foo"),
        ("the group's name member as written in the model", "AutoScalingGroupName"),
        ("the group-name criterion with a trailing space", "auto-scaling-group-name "),
        ("the group-name criterion in upper case", "AUTO-SCALING-GROUP-NAME"),
    ];
    for (case, name) in unknown_criteria {
        let (status, body) = query(
            &router,
            "DescribeAutoScalingGroups",
            &[
                ("Filters.member.1.Name", name),
                ("Filters.member.1.Values.member.1", "asg-filtered"),
            ],
        )
        .await;
        let (_, code, message) = refusal(&body);
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {body:?}"
        );
        assert_eq!(
            code, "ValidationError",
            "case {case:?}: expected the ValidationError code, got {body:?}"
        );
        assert_eq!(
            message, VALID_FILTER_TYPES_MESSAGE,
            "case {case:?}: expected the message {VALID_FILTER_TYPES_MESSAGE:?}, got {message:?}"
        );
    }

    // And the controls: either half on its own is still served — a documented criterion
    // selects by tag, and the group names alone select by name.
    let (status, body) = query(
        &router,
        "DescribeAutoScalingGroups",
        &[
            ("Filters.member.1.Name", "tag-key"),
            ("Filters.member.1.Values.member.1", "env"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "a documented criterion alone must be served, got {status} with {body:?}"
    );
    assert_eq!(
        tag_values(&body, "AutoScalingGroupName"),
        vec!["asg-filtered".to_string(), "asg-other".to_string()],
        "the tag criterion must select both tagged groups, got {body:?}"
    );

    let (status, body) = query(
        &router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", "asg-other")],
    )
    .await;
    assert_eq!(
        status, 200,
        "the group names alone must be served, got {status} with {body:?}"
    );
    assert_eq!(
        tag_values(&body, "AutoScalingGroupName"),
        vec!["asg-other".to_string()],
        "the named read must report the group it named, got {body:?}"
    );
}

/// The sentence a request whose subnets are not in the Availability Zones it states for the
/// group is refused with: it states that the two disagree and names neither the zones nor the
/// subnets.
const ZONE_MISMATCH_MESSAGE: &str =
    "The availability zones of the specified subnets and the Auto Scaling group do not match";

/// The `awsQueryError` code of the modelled `AlreadyExistsFault`, which is answered only to a
/// request whose configuration the service can otherwise satisfy.
const ALREADY_EXISTS: &str = "AlreadyExists";

/// A create whose stated zones and stated subnets disagree is refused for THAT defect even
/// when the name it states is already taken: the request defect is reported before the name
/// is weighed against the names the account holds.
#[tokio::test]
async fn a_create_whose_placement_disagrees_is_refused_before_its_name_is_weighed() {
    let (router, engine) = emulator();
    let subnets = seed_subnets(&engine, 3);

    // The group that holds the name every "taken" row re-states, placed by subnet so its own
    // placement agrees.
    let mut params = group_without_placement("asg-taken");
    params.push(p("VPCZoneIdentifier", subnets[0].id.clone()));
    let (status, body) = query(&router, "CreateAutoScalingGroup", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "creating the group that holds the name must succeed, got {status} with {body:?}"
    );

    // (case, the name the create states, the zones it states, the subnets it names) — the
    // name free and the name taken, one zone stated and two, one subnet outside the stated
    // zones and one outside beside one inside: the refusal is about the request's placement,
    // so it does not depend on which of those the request carries.
    let cases: Vec<(&str, &str, Vec<usize>, Vec<usize>)> = vec![
        (
            "a free name, one zone, a subnet of another",
            "asg-free-1",
            vec![0],
            vec![1],
        ),
        (
            "a taken name, one zone, a subnet of another",
            "asg-taken",
            vec![0],
            vec![1],
        ),
        (
            "a taken name, two zones, a subnet of a third",
            "asg-taken",
            vec![0, 1],
            vec![2],
        ),
        (
            "a taken name, one zone, a subnet inside beside one outside",
            "asg-taken",
            vec![0],
            vec![0, 2],
        ),
    ];

    for (case, name, zones, named) in &cases {
        let mut params = group_without_placement(name);
        for (index, zone) in zones.iter().enumerate() {
            params.push(p(
                format!("AvailabilityZones.member.{}", index + 1).as_str(),
                subnets[*zone].zone.clone(),
            ));
        }
        let ids: Vec<String> = named
            .iter()
            .map(|index| subnets[*index].id.clone())
            .collect();
        params.push(p("VPCZoneIdentifier", ids.join(",")));
        let (status, body) = query(&router, "CreateAutoScalingGroup", &borrowed(&params)).await;
        let (error_type, code, message) = refusal(&body);
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {body:?}"
        );
        assert_eq!(
            (error_type.as_str(), code.as_str()),
            ("Sender", "ValidationError"),
            "case {case:?}: expected a Sender ValidationError, got {body:?}"
        );
        assert_eq!(
            message, ZONE_MISMATCH_MESSAGE,
            "case {case:?}: expected the message {ZONE_MISMATCH_MESSAGE:?}, got {message:?}"
        );
    }

    // The controls: a create whose placement AGREES is served under a free name, and refused
    // for the name — and only for the name — under the taken one.
    let mut params = group_without_placement("asg-free-agreeing");
    params.push(p("AvailabilityZones.member.1", subnets[0].zone.clone()));
    params.push(p("VPCZoneIdentifier", subnets[0].id.clone()));
    let (status, body) = query(&router, "CreateAutoScalingGroup", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "an agreeing placement under a free name must be served, got {status} with {body:?}"
    );

    let mut params = group_without_placement("asg-taken");
    params.push(p("AvailabilityZones.member.1", subnets[0].zone.clone()));
    params.push(p("VPCZoneIdentifier", subnets[0].id.clone()));
    let (status, body) = query(&router, "CreateAutoScalingGroup", &borrowed(&params)).await;
    let (_, code, message) = refusal(&body);
    assert_eq!(
        status, 400,
        "an agreeing placement under a taken name must be refused, got {status} with {body:?}"
    );
    assert_eq!(
        code, ALREADY_EXISTS,
        "an agreeing placement under a taken name must be refused for the NAME, got {code:?} \
         {message:?}"
    );

    // And the refused creates stored nothing: only the groups the two served creates made are
    // held.
    let held = described_group_names(&router).await;
    assert_eq!(
        held,
        vec!["asg-free-agreeing".to_string(), "asg-taken".to_string()],
        "only the served creates may have stored a group, got {held:?}"
    );
}

/// Post one legacy target-group detach and answer its status, error type, wire code and
/// message.
async fn detach(
    router: &MultiServiceRouter,
    name: &str,
    arns: &[String],
) -> (u16, String, String, String) {
    let mut params = vec![p("AutoScalingGroupName", name)];
    for (index, arn) in arns.iter().enumerate() {
        params.push(p(
            format!("TargetGroupARNs.member.{}", index + 1).as_str(),
            arn.clone(),
        ));
    }
    let (status, body) = query(router, "DetachLoadBalancerTargetGroups", &borrowed(&params)).await;
    let (error_type, code, message) = refusal(&body);
    (status, error_type, code, message)
}

/// The target groups one group reports through the implemented reader of its
/// `TargetGroupARNs`.
async fn attached_target_groups(router: &MultiServiceRouter, name: &str) -> Vec<String> {
    let (status, body) = query(
        router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", name)],
    )
    .await;
    assert_eq!(
        status, 200,
        "DescribeAutoScalingGroups for {name:?} expected 200, got {status} with {body:?}"
    );
    list_members(&body, "TargetGroupARNs")
        .into_iter()
        .map(::std::string::ToString::to_string)
        .collect()
}

/// A legacy target-group detach naming a group the account does not hold is refused in the
/// absent-group form THIS operation answers with — the same sentence for every name, with the
/// detail field unfilled — while a detach from a group that exists still removes the
/// attachment.
#[tokio::test]
async fn a_detach_of_target_groups_from_an_absent_group_names_no_group() {
    let (router, engine) = emulator();
    let subnets = seed_subnets(&engine, 1);
    let vpc_id = vpc_of(&router, &subnets[0]).await;
    let arn = create_target_group(&router, "tg-detach", 80, "instance", &vpc_id).await;

    let mut params = group_without_placement("asg-detach");
    params.push(p("AvailabilityZones.member.1", subnets[0].zone.clone()));
    params.push(p("TargetGroupARNs.member.1", arn.clone()));
    let (status, body) = query(&router, "CreateAutoScalingGroup", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "creating the group to detach from must succeed, got {status} with {body:?}"
    );

    // A group created and then deleted is as absent as one never created, which is a row of
    // its own: the refusal is about what the account holds NOW.
    let mut deleted = group_without_placement("asg-detach-deleted");
    deleted.push(p("AvailabilityZones.member.1", subnets[0].zone.clone()));
    let (status, body) = query(&router, "CreateAutoScalingGroup", &borrowed(&deleted)).await;
    assert_eq!(
        status, 200,
        "creating the group to delete must succeed, got {status} with {body:?}"
    );
    let (status, body) = query(
        &router,
        "DeleteAutoScalingGroup",
        &[("AutoScalingGroupName", "asg-detach-deleted")],
    )
    .await;
    assert_eq!(
        status, 200,
        "deleting that group must succeed, got {status} with {body:?}"
    );

    // The names a request can carry that no group answers to: never created, differing from
    // a held name only in case or in length, carrying characters a group name may hold, one
    // that was created and deleted, and the longest a name may be (255 characters, the
    // length `XmlStringMaxLen255` bounds `AutoScalingGroupName` by).
    let absent = [
        ("a name never created", "asg-absent".to_string()),
        ("a held name in another case", "ASG-Detach".to_string()),
        ("a held name with more after it", "asg-detach-longer".to_string()),
        ("a name of punctuation and digits", "asg.detach_9-x".to_string()),
        ("a name created and then deleted", "asg-detach-deleted".to_string()),
        ("the longest name a request may carry", "a".repeat(255)),
    ];
    for (case, name) in &absent {
        let (status, error_type, code, message) =
            detach(&router, name, ::std::slice::from_ref(&arn)).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {code:?} {message:?}"
        );
        assert_eq!(
            (error_type.as_str(), code.as_str()),
            ("Sender", "ValidationError"),
            "case {case:?}: expected a Sender ValidationError, got {code:?} {message:?}"
        );
        assert_eq!(
            message, ABSENT_GROUP_WITHOUT_DETAIL,
            "case {case:?}: expected the message {ABSENT_GROUP_WITHOUT_DETAIL:?}, got {message:?}"
        );
    }

    // The group that DOES exist still holds its attachment: the refusals above touched
    // nothing.
    let held = attached_target_groups(&router, "asg-detach").await;
    assert_eq!(
        held,
        vec![arn.clone()],
        "the stored group must keep its target group, got {held:?}"
    );

    // The control: the same detach against the group that exists is served, and the
    // attachment is gone when the group is read back.
    let (status, _, code, message) =
        detach(&router, "asg-detach", ::std::slice::from_ref(&arn)).await;
    assert_eq!(
        status, 200,
        "the detach from the stored group must be served, got {status} with {code:?} {message:?}"
    );
    let held = attached_target_groups(&router, "asg-detach").await;
    assert!(
        held.is_empty(),
        "the detached target group must be gone, got {held:?}"
    );
}
