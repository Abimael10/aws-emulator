// SPDX-License-Identifier: CC0-1.0

//! The Amazon Neptune RELATIONAL resources a stack declares — a DB cluster parameter group, a
//! DB parameter group, a DB subnet group and a global database — end to end over the awsQuery
//! wire.
//!
//! RULE: a resource type Amazon Web Services publishes is one a template may declare, and a
//! declaration of it is DEPLOYED by calling the API that owns the type — so the handle the
//! stack publishes (`Ref`) resolves in that API, the configuration the template declared is
//! the configuration that API reports, deleting the stack takes the resource away again, and a
//! declaration that API REFUSES fails the resource with its own refusal and rolls the stack
//! back. Amazon Neptune's control plane is served on the one relational front door (its model
//! carries `sigv4Name: rds` and `endpointPrefix: rds`, and its parameter-group, subnet-group
//! and global-database actions are spelled identically to Amazon RDS's), so that door is where
//! a client reads these resources back — which is exactly where the stack has to create them.
//!
//! The rows differ along what the rule generalises over: the resource KIND (the cluster-scoped
//! parameter group, its instance-scoped sibling, the subnet group, the global database),
//! whether the template NAMES the resource or leaves CloudFormation to generate the name, the
//! template spelling (YAML and JSON), the engine family (three of the `neptune*` families the
//! engine-version catalogue publishes), and how MANY parameters a group's declaration carries
//! (none, one, two).
//!
//! The refusal rows differ along the LEVEL whose published range a declared value falls
//! outside — the cluster parameter catalogue's and the instance parameter catalogue's — each
//! read from that catalogue through the owning API's own engine-default report rather than
//! written here, so the case is the rule and not one recorded number.
//!
//! `crate::state::provisioner::neptune_relational` is the layer under test, reached through
//! the deployment that drives it; `crate::state::resource_types` is the other half, since a
//! type the registry does not know is refused before any of this runs.

use service_com_amazonaws_cloudformation_cloudformation::state::{resource_status, status};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{
    assert_sdk_diagnostic_with_attempts, failed_resource_reason, quoted_and_class,
    settled_stack_status, stack_event_reasons,
};
use crate::stack_lifecycle::{
    call, create_stack, describe_stack, member, output_value, rds, router, xml_text, xml_texts,
    REGION,
};

/// The fixed text CloudFormation refuses a template declaring a type it does NOT know with,
/// which no row of this suite may ever see: every type here is one Amazon Web Services
/// publishes.
const UNRECOGNIZED_TYPES: &str = "Unrecognized resource types";

/// The name the exception a refused `AWS::Neptune::*` call raises states the service under:
/// this control plane's own `sdkId`, which is the name of the SDK client its handlers hold —
/// not the `Rds` the signing name resolves to.
const NEPTUNE_SDK_SERVICE: &str = "Neptune";

/// The status the relational front door answers a value outside a parameter's published
/// domain with, and the `HandlerErrorCode` class such a refusal of the REQUEST falls in.
const REFUSED_STATUS: u16 = 400;
const INVALID_REQUEST: &str = "InvalidRequest";

/// The key a stack of this suite carries its own tag under, which CloudFormation propagates to
/// every resource it creates.
const STACK_TAG_KEY: &str = "sim2real";

/// How long a case waits for the owning API to stop holding a resource the stack's deletion
/// removed, and how often it looks again.
///
/// Generous on purpose: the removal is due at an INSTANT the stack's own clock records and
/// what is waited FOR is the API no longer holding the resource, so the wait ends as soon as
/// that holds rather than after a span of wall clock.
const GONE_DEADLINE: ::std::time::Duration = ::std::time::Duration::from_secs(60);
const GONE_POLL_INTERVAL: ::std::time::Duration = ::std::time::Duration::from_millis(50);

/// Read one resource through the owning API until it answers that it no longer holds it, and
/// answer that last read — the 200 it kept answering where the deadline passed first, which is
/// what the caller's assertion then names.
async fn until_gone(
    router: &MultiServiceRouter,
    action: &str,
    name_member: &str,
    value: &str,
) -> (u16, String) {
    let deadline = ::tokio::time::Instant::now() + GONE_DEADLINE;
    loop {
        let answered = rds(router, action, &[(name_member, value)]).await;
        if answered.0 != 200 || ::tokio::time::Instant::now() >= deadline {
            return answered;
        }
        ::tokio::time::sleep(GONE_POLL_INTERVAL).await;
    }
}

/// The `PhysicalResourceId` the stack reports for one of its resources.
async fn physical_id(router: &MultiServiceRouter, stack: &str, logical_id: &str) -> String {
    let (status_code, body) = call(
        router,
        "DescribeStackResources",
        &[
            member("StackName", stack),
            member("LogicalResourceId", logical_id),
        ],
    )
    .await;
    assert_eq!(
        status_code, 200,
        "DescribeStackResources({stack}, {logical_id}) must answer 200, got {status_code} \
         {body}"
    );
    xml_text(&body, "PhysicalResourceId").unwrap_or_default()
}

/// Submit `template` as `stack_name` carrying the stack's own tag, and hold the submission to
/// the registry half of the rule: a type Amazon Web Services publishes is ADMITTED, so this
/// never sees the template-format refusal.
async fn submitted(
    router: &MultiServiceRouter,
    case: &str,
    stack_name: &str,
    template: &str,
) -> String {
    let (status_code, body) = create_stack(
        router,
        stack_name,
        template,
        &[
            member("Tags.member.1.Key", STACK_TAG_KEY),
            member("Tags.member.1.Value", stack_name),
        ],
    )
    .await;
    assert!(
        !body.contains(UNRECOGNIZED_TYPES),
        "case {case:?}: a type Amazon Web Services publishes must be recognised, got {body}"
    );
    assert_eq!(
        status_code, 200,
        "case {case:?}: CreateStack must answer 200, got {status_code} {body}"
    );
    body
}

/// Deploy `template` as `stack_name` and answer the `DescribeStacks` body of the completed
/// stack, quoting the stack's own failure reasons when the create did not complete.
async fn deployed(
    router: &MultiServiceRouter,
    case: &str,
    stack_name: &str,
    template: &str,
) -> String {
    submitted(router, case, stack_name, template).await;
    let reached = settled_stack_status(case, router, REGION, stack_name).await;
    assert_eq!(
        reached,
        status::CREATE_COMPLETE,
        "case {case:?}: the stack must reach CREATE_COMPLETE, got {reached} with failure \
         reasons {:?}",
        stack_event_reasons(router, REGION, stack_name).await
    );
    let (status_code, described) = describe_stack(router, stack_name).await;
    assert_eq!(
        status_code, 200,
        "case {case:?}: DescribeStacks must answer 200, got {status_code} {described}"
    );
    described
}

/// The `(ParameterName, ParameterValue)` pairs the owning API reports for the parameters a
/// CALLER set on `group` — its parameter report under the `user` source, the vocabulary that
/// member publishes ("Valid Values: user | system | engine-default").
async fn user_parameters(
    router: &MultiServiceRouter,
    parameters_describe: &str,
    name_member: &str,
    group: &str,
) -> (u16, String, Vec<(String, String)>) {
    let (status_code, body) = rds(
        router,
        parameters_describe,
        &[(name_member, group), ("Source", "user")],
    )
    .await;
    let reported = xml_texts(&body, "ParameterName")
        .into_iter()
        .zip(xml_texts(&body, "ParameterValue"))
        .collect();
    (status_code, body, reported)
}

/// Hold one resource's tags, as the owning API reports them by the ARN it minted, to the pairs
/// the declaration asked for and the one CloudFormation propagates for the stack itself.
///
/// Containment rather than equality: CloudFormation gives every resource it creates its own
/// three `aws:cloudformation:*` tags beside the stack's, and which of those it adds is no part
/// of this rule.
async fn assert_tags(
    router: &MultiServiceRouter,
    case: &str,
    arn: &str,
    wanted: &[(&str, &str)],
) {
    let (status_code, body) = rds(router, "ListTagsForResource", &[("ResourceName", arn)]).await;
    assert_eq!(
        status_code, 200,
        "case {case:?}: ListTagsForResource({arn}) must answer 200, got {status_code} {body}"
    );
    let held: Vec<(String, String)> = xml_texts(&body, "Key")
        .into_iter()
        .zip(xml_texts(&body, "Value"))
        .collect();
    for (key, value) in wanted {
        assert!(
            held.iter()
                .any(|(held_key, held_value)| held_key == key && held_value == value),
            "case {case:?}: the resource must carry the tag ({key}, {value}), got {held:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// The two parameter-group levels
// ---------------------------------------------------------------------------

/// One row of the parameter-group table: the stack a template declares, and the group the
/// owning API must hold once it is deployed.
struct GroupCase<'a> {
    case: &'a str,
    stack_name: &'a str,
    template: &'a str,
    /// The member the owning API addresses this group KIND by, which is also what the type's
    /// `Ref` returns: `DBParameterGroupName` for the instance-scoped group,
    /// `DBClusterParameterGroupName` for the cluster-scoped one.
    name_member: &'a str,
    /// The action that reports the group itself, and the one that reports its parameters.
    group_describe: &'a str,
    parameters_describe: &'a str,
    /// The name the stack must report for the group: `Some` where the template NAMES it — the
    /// name the API STORES for it, which is the declared name folded to lower case, since
    /// either name member "is stored as a lowercase string" — and `None` where CloudFormation
    /// generates the name.
    declared_name: Option<&'a str>,
    /// `DBParameterGroupFamily` the API must report for the group.
    family: &'a str,
    /// `Description` it must report.
    description: &'a str,
    /// The parameter settings the template declares, which the group must carry as the ones a
    /// caller set, in the name order the report answers in.
    parameters: &'a [(&'a str, &'a str)],
    /// The resource's own tag the declaration carries, which the group must end up holding
    /// beside the stack's propagated one.
    resource_tag: Option<(&'a str, &'a str)>,
}

/// The rows: both group levels, both template spellings, a declared and a generated name,
/// three published families, and declarations carrying none, one and two parameters.
///
/// Every parameter named is one its family's catalogue publishes — the cluster level's
/// `neptune_enable_audit_log`, `neptune_streams` and `neptune_streams_expiry_days`, the
/// instance level's `neptune_query_timeout` — and every value is inside the domain that
/// catalogue publishes for it, which the refusal test below reads from the API rather than
/// restating.
fn group_cases() -> Vec<GroupCase<'static>> {
    vec![
        GroupCase {
            case: "the CLUSTER level, a declared name, two parameters, in YAML",
            stack_name: "neptune-cluster-params",
            template: concat!(
                "Resources:\n",
                "  ClusterParamGroup:\n",
                "    Type: AWS::Neptune::DBClusterParameterGroup\n",
                "    Properties:\n",
                "      Name: cfn-probe-neptune-cpg\n",
                "      Family: neptune1.3\n",
                "      Description: the probe stack's cluster parameters\n",
                "      Parameters:\n",
                "        neptune_enable_audit_log: '1'\n",
                "        neptune_streams_expiry_days: '30'\n",
                "      Tags:\n",
                "        - Key: tier\n",
                "          Value: graph\n",
            ),
            name_member: "DBClusterParameterGroupName",
            group_describe: "DescribeDBClusterParameterGroups",
            parameters_describe: "DescribeDBClusterParameters",
            declared_name: Some("cfn-probe-neptune-cpg"),
            family: "neptune1.3",
            description: "the probe stack's cluster parameters",
            parameters: &[
                ("neptune_enable_audit_log", "1"),
                ("neptune_streams_expiry_days", "30"),
            ],
            resource_tag: Some(("tier", "graph")),
        },
        GroupCase {
            case: "the CLUSTER level, a GENERATED name, one parameter, in JSON",
            stack_name: "neptune-generated-cpg",
            template: r#"{"Resources":{"ClusterParamGroup":{
                     "Type":"AWS::Neptune::DBClusterParameterGroup",
                     "Properties":{
                       "Family":"neptune1.4",
                       "Description":"a generated-name cluster group",
                       "Parameters":{"neptune_streams":"1"}}}}}"#,
            name_member: "DBClusterParameterGroupName",
            group_describe: "DescribeDBClusterParameterGroups",
            parameters_describe: "DescribeDBClusterParameters",
            declared_name: None,
            family: "neptune1.4",
            description: "a generated-name cluster group",
            parameters: &[("neptune_streams", "1")],
            resource_tag: None,
        },
        GroupCase {
            case: "the INSTANCE level, a declared name, one parameter, in YAML",
            stack_name: "neptune-db-params",
            template: concat!(
                "Resources:\n",
                "  DbParamGroup:\n",
                "    Type: AWS::Neptune::DBParameterGroup\n",
                "    Properties:\n",
                "      Name: cfn-probe-neptune-dpg\n",
                "      Family: neptune1.3\n",
                "      Description: the probe stack's instance parameters\n",
                "      Parameters:\n",
                "        neptune_query_timeout: '20000'\n",
                "      Tags:\n",
                "        - Key: tier\n",
                "          Value: graph\n",
            ),
            name_member: "DBParameterGroupName",
            group_describe: "DescribeDBParameterGroups",
            parameters_describe: "DescribeDBParameters",
            declared_name: Some("cfn-probe-neptune-dpg"),
            family: "neptune1.3",
            description: "the probe stack's instance parameters",
            parameters: &[("neptune_query_timeout", "20000")],
            resource_tag: Some(("tier", "graph")),
        },
        GroupCase {
            case: "the INSTANCE level, a GENERATED name, NO parameters, an earlier family, \
                   in JSON",
            stack_name: "neptune-empty-dpg",
            template: r#"{"Resources":{"DbParamGroup":{
                     "Type":"AWS::Neptune::DBParameterGroup",
                     "Properties":{
                       "Family":"neptune1.2",
                       "Description":"a group declaring no parameters"}}}}"#,
            name_member: "DBParameterGroupName",
            group_describe: "DescribeDBParameterGroups",
            parameters_describe: "DescribeDBParameters",
            declared_name: None,
            family: "neptune1.2",
            description: "a group declaring no parameters",
            parameters: &[],
            resource_tag: None,
        },
    ]
}

#[tokio::test]
async fn a_neptune_parameter_group_a_stack_declares_exists_with_the_parameters_it_declared() {
    for case in group_cases() {
        let router = router();
        let logical_id = match case.name_member {
            "DBClusterParameterGroupName" => "ClusterParamGroup",
            _ => "DbParamGroup",
        };
        let described = deployed(&router, case.case, case.stack_name, case.template).await;
        // Addressed by stack id from the delete on, since a completed deletion leaves the
        // live set this service answers a name from.
        let stack_id = xml_text(&described, "StackId").unwrap_or_default();

        let group = physical_id(&router, case.stack_name, logical_id).await;
        if let Some(declared) = case.declared_name {
            assert_eq!(
                group, declared,
                "case {:?}: the template named the group, so its physical id is the name the \
                 owning API stored it under, got {group:?}",
                case.case
            );
        }
        assert!(
            !group.is_empty(),
            "case {:?}: the stack must publish a physical id for the group",
            case.case
        );

        // The whole of the rule: the owning API holds the group the stack reports, under the
        // name the stack published, with the configuration the template declared.
        let (status_code, reported) = rds(
            &router,
            case.group_describe,
            &[(case.name_member, group.as_str())],
        )
        .await;
        assert_eq!(
            status_code, 200,
            "case {:?}: {}({group}) must answer 200, got {status_code} {reported}",
            case.case, case.group_describe
        );
        assert_eq!(
            xml_text(&reported, "DBParameterGroupFamily").as_deref(),
            Some(case.family),
            "case {:?}: the declared family must be reported, got {reported}",
            case.case
        );
        assert_eq!(
            xml_text(&reported, "Description").as_deref(),
            Some(case.description),
            "case {:?}: the declared description must be reported, got {reported}",
            case.case
        );

        // The parameters the template declared are the ones a caller set on the group.
        let (status_code, body, settings) = user_parameters(
            &router,
            case.parameters_describe,
            case.name_member,
            &group,
        )
        .await;
        assert_eq!(
            status_code, 200,
            "case {:?}: {}({group}) must answer 200, got {status_code} {body}",
            case.case, case.parameters_describe
        );
        let expected: Vec<(String, String)> = case
            .parameters
            .iter()
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect();
        assert_eq!(
            settings, expected,
            "case {:?}: the declared parameters must be the group's own, got {settings:?} in \
             {body}",
            case.case
        );

        // The group's ARN carries the tags the declaration asked for AND the one the stack
        // propagates to every resource it creates.
        let arn = xml_text(&reported, &format!("{}Arn", group_record(case.name_member)))
            .unwrap_or_default();
        assert!(
            !arn.is_empty(),
            "case {:?}: the owning API must report an ARN for the group, got {reported}",
            case.case
        );
        let mut wanted = vec![(STACK_TAG_KEY, case.stack_name)];
        wanted.extend(case.resource_tag);
        assert_tags(&router, case.case, &arn, &wanted).await;

        // ...and deleting the stack takes the group away again, under the owning API's own
        // not-found code and the sentence it words for a group it was ASKED about.
        let (status_code, deleted) = call(
            &router,
            "DeleteStack",
            &[member("StackName", case.stack_name)],
        )
        .await;
        assert_eq!(
            status_code, 200,
            "case {:?}: DeleteStack must answer 200, got {status_code} {deleted}",
            case.case
        );
        let reached = settled_stack_status(case.case, &router, REGION, &stack_id).await;
        assert_eq!(
            reached,
            status::DELETE_COMPLETE,
            "case {:?}: the deletion must complete, got {reached:?} with reasons {:?}",
            case.case,
            stack_event_reasons(&router, REGION, &stack_id).await
        );
        let (status_code, reported) =
            until_gone(&router, case.group_describe, case.name_member, &group).await;
        assert_eq!(
            status_code, 404,
            "case {:?}: deleting the stack must delete the group it created, got \
             {status_code} {reported}",
            case.case
        );
        assert_eq!(
            xml_text(&reported, "Code").as_deref(),
            Some("DBParameterGroupNotFound"),
            "case {:?}: the group must be gone under the API's own not-found code, got \
             {reported}",
            case.case
        );
        assert_eq!(
            xml_text(&reported, "Message").as_deref(),
            Some(format!("{} not found: {group}", group_record(case.name_member)).as_str()),
            "case {:?}: the refusal must name the group the request asked about, got \
             {reported}",
            case.case
        );
    }
}

/// The element the owning API reports one group of this KIND as, which is also what its ARN
/// and its not-found sentence are spelled with: read off the member the group is addressed
/// by, so the two levels cannot drift apart here.
fn group_record(name_member: &str) -> &'static str {
    match name_member {
        "DBClusterParameterGroupName" => "DBClusterParameterGroup",
        _ => "DBParameterGroup",
    }
}

/// A property a type does NOT publish is refused rather than passed through, for either level:
/// these types state the group's name in plain `Name`, so a declaration spelling the owning
/// API's own member instead names a property of no `AWS::Neptune::*` group — the other half of
/// the rule, which is what keeps a template that asks for a group this emulator cannot
/// provision as declared from completing.
#[tokio::test]
async fn a_property_a_neptune_group_does_not_publish_is_refused() {
    // One row per LEVEL, each naming the member its own owning call addresses a group by.
    let rows: [(&str, &str, &str, &str); 2] = [
        (
            "the CLUSTER level's own API member stated as a property",
            "neptune-unpublished-cpg",
            "AWS::Neptune::DBClusterParameterGroup",
            "DBClusterParameterGroupName",
        ),
        (
            "the INSTANCE level's own API member stated as a property",
            "neptune-unpublished-dpg",
            "AWS::Neptune::DBParameterGroup",
            "DBParameterGroupName",
        ),
    ];

    for (case, stack_name, resource_type, unpublished) in rows {
        let router = router();
        let template = format!(
            r#"{{"Resources":{{"Group":{{"Type":"{resource_type}","Properties":{{
                 "{unpublished}":"{stack_name}-group","Family":"neptune1.3",
                 "Description":"a group stating a property the type does not publish"}}}}}}}}"#
        );
        submitted(&router, case, stack_name, &template).await;
        let reached = settled_stack_status(case, &router, REGION, stack_name).await;
        assert_eq!(
            reached,
            status::ROLLBACK_COMPLETE,
            "case {case:?}: a property the type does not publish must roll the stack back, \
             got {reached} with reasons {:?}",
            stack_event_reasons(&router, REGION, stack_name).await
        );
        // WHICH property could not be provisioned is the resource's own diagnosis.
        let reason = failed_resource_reason(
            case,
            &router,
            REGION,
            stack_name,
            "Group",
            resource_status::CREATE_FAILED,
        )
        .await;
        assert!(
            reason.contains(unpublished),
            "case {case:?}: the reason must name the property that could not be provisioned, \
             got {reason:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// A declared value outside its parameter's published range
// ---------------------------------------------------------------------------

/// One row of the refusal table: the level whose catalogue the declared value falls outside,
/// and the engine-default report that publishes the range it falls outside of.
struct RangeCase {
    case: &'static str,
    stack_name: &'static str,
    resource_type: &'static str,
    logical_id: &'static str,
    family: &'static str,
    parameter: &'static str,
    /// The action that reports the family's own engine defaults, which is where the range the
    /// row steps past is READ from.
    defaults_describe: &'static str,
    /// The member and action the group itself is addressed and reported by.
    name_member: &'static str,
    group_describe: &'static str,
}

const RANGE_CASES: &[RangeCase] = &[
    RangeCase {
        case: "a CLUSTER parameter one above its published maximum",
        stack_name: "neptune-cpg-out-of-range",
        resource_type: "AWS::Neptune::DBClusterParameterGroup",
        logical_id: "Cpg",
        family: "neptune1.4",
        parameter: "neptune_streams_expiry_days",
        defaults_describe: "DescribeEngineDefaultClusterParameters",
        name_member: "DBClusterParameterGroupName",
        group_describe: "DescribeDBClusterParameterGroups",
    },
    RangeCase {
        case: "an INSTANCE parameter one below its published minimum",
        stack_name: "neptune-dpg-out-of-range",
        resource_type: "AWS::Neptune::DBParameterGroup",
        logical_id: "Dpg",
        family: "neptune1.4",
        parameter: "neptune_query_timeout",
        defaults_describe: "DescribeEngineDefaultParameters",
        name_member: "DBParameterGroupName",
        group_describe: "DescribeDBParameterGroups",
    },
];

/// The `AllowedValues` the family's engine-default report publishes for `parameter`, read from
/// the owning API itself so the case's expected sentence carries the range that API publishes
/// rather than one written here.
async fn published_range(
    router: &MultiServiceRouter,
    case: &RangeCase,
) -> (String, i64, i64) {
    let (status_code, body) = rds(
        router,
        case.defaults_describe,
        &[("DBParameterGroupFamily", case.family)],
    )
    .await;
    assert_eq!(
        status_code, 200,
        "case {:?}: {}({}) must answer 200, got {status_code} {body}",
        case.case, case.defaults_describe, case.family
    );
    let allowed = xml_texts(&body, "ParameterName")
        .into_iter()
        .zip(xml_texts(&body, "AllowedValues"))
        .find(|(name, _)| name == case.parameter)
        .map(|(_, allowed)| allowed)
        .unwrap_or_else(|| {
            panic!(
                "case {:?}: {} must publish {}, got {body}",
                case.case, case.family, case.parameter
            )
        });
    let (low, high) = allowed.split_once('-').unwrap_or_else(|| {
        panic!(
            "case {:?}: {} must publish a numeric range for {}, got {allowed:?}",
            case.case, case.family, case.parameter
        )
    });
    let low: i64 = low.trim().parse().unwrap_or_else(|_| {
        panic!("case {:?}: the published low bound must be a number, got {low:?}", case.case)
    });
    let high: i64 = high.trim().parse().unwrap_or_else(|_| {
        panic!(
            "case {:?}: the published high bound must be a number, got {high:?}",
            case.case
        )
    });
    (allowed, low, high)
}

#[tokio::test]
async fn a_declared_value_outside_its_published_range_rolls_the_stack_back() {
    for case in RANGE_CASES {
        let router = router();
        let (allowed, low, high) = published_range(&router, case).await;
        // One step PAST a published bound: above the maximum at the cluster level, below the
        // minimum at the instance level, so the row walks the catalogue's edge rather than
        // naming a value of its own.
        let refused = match case.logical_id {
            "Cpg" => high + 1,
            _ => low - 1,
        };
        let group = format!("{}-group", case.stack_name);
        let template = format!(
            r#"{{"Resources":{{"{logical}":{{"Type":"{resource_type}","Properties":{{
                 "Name":"{group}","Family":"{family}",
                 "Description":"a group declaring a value outside its range",
                 "Parameters":{{"{parameter}":"{refused}"}}}}}}}}}}"#,
            logical = case.logical_id,
            resource_type = case.resource_type,
            family = case.family,
            parameter = case.parameter,
        );

        // CloudFormation does NOT refuse the template: the value is the owning API's to judge,
        // so the stack is accepted and then rolled back.
        submitted(&router, case.case, case.stack_name, &template).await;
        let reached = settled_stack_status(case.case, &router, REGION, case.stack_name).await;
        assert_eq!(
            reached,
            status::ROLLBACK_COMPLETE,
            "case {:?}: a value the owning API refuses must roll the stack back, got \
             {reached} with reasons {:?}",
            case.case,
            stack_event_reasons(&router, REGION, case.stack_name).await
        );

        // WHY it failed is the resource's own diagnosis: the exception the provider's SDK
        // raised, quoted in the registry handler's envelope.
        let reason = failed_resource_reason(
            case.case,
            &router,
            REGION,
            case.stack_name,
            case.logical_id,
            resource_status::CREATE_FAILED,
        )
        .await;
        let (quoted, class) = quoted_and_class(case.case, &reason);
        let sentence = format!(
            "Value: {refused} is outside of range: {allowed} for parameter: {}",
            case.parameter
        );
        assert!(
            quoted.starts_with(&sentence),
            "case {:?}: the quoted message must open with the owning API's own sentence \
             {sentence:?}, got {quoted:?}",
            case.case
        );
        assert_eq!(
            class, INVALID_REQUEST,
            "case {:?}: a refusal of the request falls in the {INVALID_REQUEST} class, got \
             {class:?} in {reason:?}",
            case.case
        );
        assert_sdk_diagnostic_with_attempts(
            case.case,
            &quoted,
            NEPTUNE_SDK_SERVICE,
            REFUSED_STATUS,
        );

        // ...and no group survives the rollback.
        let (status_code, reported) = rds(
            &router,
            case.group_describe,
            &[(case.name_member, group.as_str())],
        )
        .await;
        assert_eq!(
            status_code, 404,
            "case {:?}: the rolled-back stack must leave no group behind, got {status_code} \
             {reported}",
            case.case
        );
    }
}

// ---------------------------------------------------------------------------
// The global database
// ---------------------------------------------------------------------------

/// The identifier, engine and version the global-database case declares: the engine this
/// control plane publishes for its own `CreateGlobalCluster` ("Valid values: neptune") and a
/// version its engine-version catalogue carries.
const GLOBAL_CLUSTER_ID: &str = "cfn-probe-neptune-global";
const NEPTUNE_ENGINE: &str = "neptune";
const NEPTUNE_ENGINE_VERSION: &str = "1.3.3.0";

/// The status a global database that provisions no DB cluster of its own is usable in.
const GLOBAL_CLUSTER_AVAILABLE: &str = "available";

#[tokio::test]
async fn a_neptune_global_cluster_a_stack_declares_exists_and_goes_with_the_stack() {
    let case = "a declared identifier, engine, version and tag, with the Ref as an output";
    let router = router();
    let template = format!(
        r#"{{"AWSTemplateFormatVersion":"2010-09-09",
            "Resources":{{"GlobalCluster":{{"Type":"AWS::Neptune::GlobalCluster","Properties":{{
              "GlobalClusterIdentifier":"{GLOBAL_CLUSTER_ID}",
              "Engine":"{NEPTUNE_ENGINE}",
              "EngineVersion":"{NEPTUNE_ENGINE_VERSION}",
              "StorageEncrypted":"true",
              "Tags":[{{"Key":"tier","Value":"graph"}}]}}}}}},
            "Outputs":{{"GlobalClusterId":{{"Value":{{"Ref":"GlobalCluster"}}}}}}}}"#
    );
    let described = deployed(&router, case, "neptune-global-cluster", &template).await;
    // Addressed by stack id from the delete on, for the reason the group cases record.
    let stack_id = xml_text(&described, "StackId").unwrap_or_default();

    // "Ref returns the GlobalClusterIdentifier", so the output and the physical id are the
    // identifier the template declared.
    assert_eq!(
        output_value(&described, "GlobalClusterId").as_deref(),
        Some(GLOBAL_CLUSTER_ID),
        "case {case:?}: the Ref must be the declared identifier, got {described}"
    );
    assert_eq!(
        physical_id(&router, "neptune-global-cluster", "GlobalCluster").await,
        GLOBAL_CLUSTER_ID,
        "case {case:?}: the resource's physical id must be the declared identifier"
    );

    // The whole of the rule: the owning API holds the global database the stack reports, with
    // the engine and version the template declared, usable.
    let (status_code, reported) = rds(
        &router,
        "DescribeGlobalClusters",
        &[("GlobalClusterIdentifier", GLOBAL_CLUSTER_ID)],
    )
    .await;
    assert_eq!(
        status_code, 200,
        "case {case:?}: DescribeGlobalClusters must answer 200, got {status_code} {reported}"
    );
    assert_eq!(
        xml_text(&reported, "Engine").as_deref(),
        Some(NEPTUNE_ENGINE),
        "case {case:?}: the declared engine must be reported, got {reported}"
    );
    assert_eq!(
        xml_text(&reported, "EngineVersion").as_deref(),
        Some(NEPTUNE_ENGINE_VERSION),
        "case {case:?}: the declared engine version must be reported, got {reported}"
    );
    assert_eq!(
        xml_text(&reported, "Status").as_deref(),
        Some(GLOBAL_CLUSTER_AVAILABLE),
        "case {case:?}: a global database that provisions nothing of its own is usable at \
         once, got {reported}"
    );
    assert_eq!(
        xml_text(&reported, "StorageEncrypted").as_deref(),
        Some("true"),
        "case {case:?}: the declared storage encryption must be reported, got {reported}"
    );

    let arn = xml_text(&reported, "GlobalClusterArn").unwrap_or_default();
    assert!(
        !arn.is_empty(),
        "case {case:?}: the owning API must report an ARN for the global database, got \
         {reported}"
    );
    assert_tags(
        &router,
        case,
        &arn,
        &[
            (STACK_TAG_KEY, "neptune-global-cluster"),
            ("tier", "graph"),
        ],
    )
    .await;

    // ...and deleting the stack takes it away again, under the owning API's own not-found
    // code and the sentence it words for a global database it was asked about.
    let (status_code, deleted) = call(
        &router,
        "DeleteStack",
        &[member("StackName", "neptune-global-cluster")],
    )
    .await;
    assert_eq!(
        status_code, 200,
        "case {case:?}: DeleteStack must answer 200, got {status_code} {deleted}"
    );
    let reached = settled_stack_status(case, &router, REGION, &stack_id).await;
    assert_eq!(
        reached,
        status::DELETE_COMPLETE,
        "case {case:?}: the deletion must complete, got {reached:?} with reasons {:?}",
        stack_event_reasons(&router, REGION, &stack_id).await
    );
    let (status_code, reported) = until_gone(
        &router,
        "DescribeGlobalClusters",
        "GlobalClusterIdentifier",
        GLOBAL_CLUSTER_ID,
    )
    .await;
    assert_eq!(
        status_code, 404,
        "case {case:?}: deleting the stack must delete the global database it created, got \
         {status_code} {reported}"
    );
    assert_eq!(
        xml_text(&reported, "Code").as_deref(),
        Some("GlobalClusterNotFoundFault"),
        "case {case:?}: it must be gone under the API's own not-found code, got {reported}"
    );
    assert_eq!(
        xml_text(&reported, "Message").as_deref(),
        Some(format!("Global cluster '{GLOBAL_CLUSTER_ID}' not found").as_str()),
        "case {case:?}: the refusal must name the identifier the request asked about, got \
         {reported}"
    );
}

// ---------------------------------------------------------------------------
// The subnet group
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_neptune_db_subnet_group_a_stack_declares_exists_over_the_subnets_it_names() {
    let case = "a subnet group over two subnets of the stack's own VPC";
    let router = router();
    let template = concat!(
        "Resources:\n",
        "  Vpc:\n",
        "    Type: AWS::EC2::VPC\n",
        "    Properties:\n",
        "      CidrBlock: 10.0.0.0/16\n",
        "  Subnet0:\n",
        "    Type: AWS::EC2::Subnet\n",
        "    Properties:\n",
        "      VpcId: !Ref Vpc\n",
        "      CidrBlock: 10.0.1.0/24\n",
        "      AvailabilityZone: us-east-1a\n",
        "  Subnet1:\n",
        "    Type: AWS::EC2::Subnet\n",
        "    Properties:\n",
        "      VpcId: !Ref Vpc\n",
        "      CidrBlock: 10.0.2.0/24\n",
        "      AvailabilityZone: us-east-1b\n",
        "  SubnetGroup:\n",
        "    Type: AWS::Neptune::DBSubnetGroup\n",
        "    Properties:\n",
        "      DBSubnetGroupName: cfn-probe-neptune-subnets\n",
        "      DBSubnetGroupDescription: the probe stack's graph subnets\n",
        "      SubnetIds:\n",
        "        - !Ref Subnet0\n",
        "        - !Ref Subnet1\n",
    );
    deployed(&router, case, "neptune-subnet-group", template).await;

    let group = physical_id(&router, "neptune-subnet-group", "SubnetGroup").await;
    assert_eq!(
        group, "cfn-probe-neptune-subnets",
        "case {case:?}: the template named the group, so that is its physical id"
    );
    let mut subnets = Vec::new();
    for logical_id in ["Subnet0", "Subnet1"] {
        subnets.push(physical_id(&router, "neptune-subnet-group", logical_id).await);
    }

    let (status_code, reported) = rds(
        &router,
        "DescribeDBSubnetGroups",
        &[("DBSubnetGroupName", group.as_str())],
    )
    .await;
    assert_eq!(
        status_code, 200,
        "case {case:?}: DescribeDBSubnetGroups({group}) must answer 200, got {status_code} \
         {reported}"
    );
    assert_eq!(
        xml_text(&reported, "DBSubnetGroupDescription").as_deref(),
        Some("the probe stack's graph subnets"),
        "case {case:?}: the declared description must be reported, got {reported}"
    );
    let mut identifiers = xml_texts(&reported, "SubnetIdentifier");
    identifiers.sort();
    let mut declared = subnets.clone();
    declared.sort();
    assert_eq!(
        identifiers, declared,
        "case {case:?}: the group must hold exactly the subnets the template named, got \
         {reported}"
    );
}
