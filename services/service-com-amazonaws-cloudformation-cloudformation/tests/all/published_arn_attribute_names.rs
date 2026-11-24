// SPDX-License-Identifier: CC0-1.0

//! Every attribute name a resource type publishes its ARN under resolves, to the ONE ARN the
//! service that owns the resource reports for it.
//!
//! The rule under test: **a resource of a complete stack answers `Fn::GetAtt` for its ARN
//! under each of the names a "Return values" section spells that member with — `Arn`,
//! `<Type>Arn`, `ARN` and `<Type>ARN` — and every one of them answers the same ARN, the one
//! the owning service reports for a resource of that name**. A type whose own model spells
//! the member in upper case (Amazon MemoryDB's `ParameterGroup$ARN` / `SubnetGroup$ARN` /
//! `Cluster$ARN` / `User$ARN`) is read by a template as `!GetAtt Group.ARN`, and that read
//! must not be the one spelling that resolves to nothing: an output over it would arrive with
//! a key and no `OutputValue`, which is what a client reading `{o["OutputKey"]:
//! o["OutputValue"]}` out of a `CREATE_COMPLETE` stack raises `KeyError` on.
//!
//! The rows differ along what the rule generalises over: the resource type (both Amazon
//! MemoryDB groups, whose ARN path that service writes as one word, a MemoryDB type whose
//! path the uniform convention already spells, and a type outside MemoryDB whose published
//! ARN is not of the `<type>/<name>` shape at all), whether the template NAMES the resource
//! or leaves the name to CloudFormation, and the template spelling (JSON and YAML, long-form
//! and short-form intrinsics). Each row reads all four spellings back through `Outputs`
//! beside the `Ref` the ARN is built on.

use smithy_server_runtime::MultiServiceRouter;

use crate::stack_lifecycle::{
    create_stack, describe_stack, router, wait_for_the_transition, xml_text, ACCOUNT_ID, REGION,
};
use crate::stack_outputs::{output_for, wire_outputs};
use service_com_amazonaws_cloudformation_cloudformation::state::template::{
    published_attribute_names, MEMORYDB_PARAMETER_GROUP, MEMORYDB_PARAMETER_GROUP_SEGMENT,
    MEMORYDB_SUBNET_GROUP, MEMORYDB_SUBNET_GROUP_SEGMENT, UPPER_CASE_ARN_ATTRIBUTE,
};
// The owning service's own rule for the name a group exists under, which CloudFormation
// fits a GENERATED name to.
use service_com_amazonaws_memorydb_amazonmemorydb::state::MAX_RESOURCE_NAME_LEN;

/// How many reads a stack is polled through before a case gives up on it: a create settles
/// once the transition the clock owes it is due, which is not the instant `CreateStack`
/// answered.
const POLL_ATTEMPTS: usize = 12;

/// The logical id every row declares its one resource under.
const LOGICAL_ID: &str = "Group";

/// One row: a template declaring one resource, and what the ARN of that resource is.
struct Row {
    case: &'static str,
    stack_name: &'static str,
    /// The template body, which declares `LOGICAL_ID` and reads its `Ref` and all four ARN
    /// spellings back as outputs.
    template: String,
    /// The ARN namespace the owning service's ARNs carry (that model's `arnNamespace`).
    service: &'static str,
    /// The resource path the owning service writes for a resource of this name.
    path: fn(&str) -> String,
    /// The name the declaration states, or `None` for a declaration that names the resource
    /// nowhere — whose name CloudFormation generates.
    declared_name: Option<&'static str>,
}

/// `parametergroup/<name>`, as Amazon MemoryDB writes a parameter group's ARN path.
fn parameter_group_path(name: &str) -> String {
    format!("{MEMORYDB_PARAMETER_GROUP_SEGMENT}/{name}")
}

/// `subnetgroup/<name>`, as Amazon MemoryDB writes a subnet group's ARN path.
fn subnet_group_path(name: &str) -> String {
    format!("{MEMORYDB_SUBNET_GROUP_SEGMENT}/{name}")
}

/// `user/<name>`, the MemoryDB path the uniform convention already spells (the type's one-word
/// segment needs no hyphen), for a type this crate carries no catalogue entry for.
fn user_path(name: &str) -> String {
    format!("user/{name}")
}

/// `log-group:<name>:*`, the path an Amazon CloudWatch Logs log-group ARN carries: a
/// published ARN that is NOT of the `<type>/<name>` shape, so the row pins that the upper-case
/// spellings answer the type's OWN published ARN rather than one minted beside it.
fn log_group_path(name: &str) -> String {
    format!("log-group:{name}:*")
}

/// The outputs every row declares, in the JSON spelling: the resource's `Ref` and the four
/// names its ARN is published under.
fn json_outputs(type_segment: &str) -> String {
    format!(
        r#""Outputs":{{"Name":{{"Value":{{"Ref":"{LOGICAL_ID}"}}}},
             "Upper":{{"Value":{{"Fn::GetAtt":["{LOGICAL_ID}","ARN"]}}}},
             "Mixed":{{"Value":{{"Fn::GetAtt":["{LOGICAL_ID}","Arn"]}}}},
             "TypeUpper":{{"Value":{{"Fn::GetAtt":["{LOGICAL_ID}","{type_segment}ARN"]}}}},
             "TypeMixed":{{"Value":{{"Fn::GetAtt":["{LOGICAL_ID}","{type_segment}Arn"]}}}}}}"#
    )
}

/// The same outputs in the YAML spelling, with the short-form intrinsics.
fn yaml_outputs(type_segment: &str) -> String {
    format!(
        "Outputs:\n  \
           Name:\n    Value: !Ref {LOGICAL_ID}\n  \
           Upper:\n    Value: !GetAtt {LOGICAL_ID}.ARN\n  \
           Mixed:\n    Value: !GetAtt {LOGICAL_ID}.Arn\n  \
           TypeUpper:\n    Value: !GetAtt {LOGICAL_ID}.{type_segment}ARN\n  \
           TypeMixed:\n    Value: !GetAtt {LOGICAL_ID}.{type_segment}Arn\n"
    )
}

fn rows() -> Vec<Row> {
    vec![
        // The reported case: the parameter group a MemoryDB estate is deployed with, whose
        // ARN path that service writes as one word.
        Row {
            case: "a memorydb parameter group, named by the template",
            stack_name: "memorydb-parameter-group",
            template: format!(
                r#"{{"Resources":{{"{LOGICAL_ID}":{{"Type":"{MEMORYDB_PARAMETER_GROUP}",
                     "Properties":{{"ParameterGroupName":"probe-parameters",
                       "Family":"memorydb_valkey7",
                       "Description":"the engine settings of a probe cluster"}}}}}},
                   {}}}"#,
                json_outputs("ParameterGroup")
            ),
            service: "memorydb",
            path: parameter_group_path,
            declared_name: Some("probe-parameters"),
        },
        // The subnet group of the same estate, in the YAML spelling with short-form
        // intrinsics, and with a LIST property beside the name. The group is placed over a
        // subnet of the SAME stack: Amazon MemoryDB resolves every `SubnetIds` entry against
        // the service that owns subnets, so a group can only be created over one that exists.
        Row {
            case: "a memorydb subnet group in the YAML spelling",
            stack_name: "memorydb-subnet-group",
            template: format!(
                "Resources:\n  \
                   Vpc:\n    \
                     Type: AWS::EC2::VPC\n    \
                     Properties:\n      \
                       CidrBlock: 10.60.0.0/16\n  \
                   Subnet:\n    \
                     Type: AWS::EC2::Subnet\n    \
                     Properties:\n      \
                       VpcId: !Ref Vpc\n      \
                       CidrBlock: 10.60.1.0/24\n      \
                       AvailabilityZone: {REGION}a\n  \
                   {LOGICAL_ID}:\n    \
                     Type: {MEMORYDB_SUBNET_GROUP}\n    \
                     Properties:\n      \
                       SubnetGroupName: probe-subnets\n      \
                       Description: the placement of a probe cluster\n      \
                       SubnetIds:\n        \
                         - !Ref Subnet\n{}",
                yaml_outputs("SubnetGroup")
            ),
            service: "memorydb",
            path: subnet_group_path,
            declared_name: Some("probe-subnets"),
        },
        // The same type with the name left to CloudFormation: the ARN is built on the
        // generated physical id, so the rule holds for a name no request fixed.
        Row {
            case: "a memorydb subnet group whose name CloudFormation generates",
            stack_name: "memorydb-generated-name",
            template: format!(
                r#"{{"Resources":{{
                     "Vpc":{{"Type":"AWS::EC2::VPC",
                       "Properties":{{"CidrBlock":"10.61.0.0/16"}}}},
                     "Subnet":{{"Type":"AWS::EC2::Subnet",
                       "Properties":{{"VpcId":{{"Ref":"Vpc"}},"CidrBlock":"10.61.1.0/24",
                         "AvailabilityZone":"{REGION}a"}}}},
                     "{LOGICAL_ID}":{{"Type":"{MEMORYDB_SUBNET_GROUP}",
                     "Properties":{{"Description":"a group the template does not name",
                       "SubnetIds":[{{"Ref":"Subnet"}}]}}}}}},
                   {}}}"#,
                json_outputs("SubnetGroup")
            ),
            service: "memorydb",
            path: subnet_group_path,
            declared_name: None,
        },
        // A MemoryDB type with no catalogue entry of its own, whose ARN path the uniform
        // convention already spells: the upper-case names answer it too.
        Row {
            case: "a memorydb user, whose path the uniform convention spells",
            stack_name: "memorydb-user",
            template: format!(
                r#"{{"Resources":{{"{LOGICAL_ID}":{{"Type":"AWS::MemoryDB::User",
                     "Properties":{{"UserName":"probe-user",
                       "AccessString":"on ~* &* +@all"}}}}}},
                   {}}}"#,
                json_outputs("User")
            ),
            service: "memorydb",
            path: user_path,
            declared_name: Some("probe-user"),
        },
        // A type outside MemoryDB whose own published ARN is not of the `<type>/<name>`
        // shape: the upper-case spellings must answer THAT ARN, not a minted one.
        Row {
            case: "a log group, whose published ARN ends in :*",
            stack_name: "logs-group-arn-names",
            template: format!(
                "Resources:\n  \
                   {LOGICAL_ID}:\n    \
                     Type: AWS::Logs::LogGroup\n    \
                     Properties:\n      \
                       LogGroupName: /probe/arn-names\n{}",
                yaml_outputs("LogGroup")
            ),
            service: "logs",
            path: log_group_path,
            declared_name: Some("/probe/arn-names"),
        },
    ]
}

/// Read a stack until it reports a status that is no longer in progress, and answer that
/// read's body.
async fn poll_to_terminal(router: &MultiServiceRouter, case: &str, stack_name: &str) -> String {
    let mut described = String::new();
    for _ in 0..POLL_ATTEMPTS {
        wait_for_the_transition().await;
        let (code, body) = describe_stack(router, stack_name).await;
        assert_eq!(
            code, 200,
            "case {case:?}: DescribeStacks must answer 200, got {code} {body}"
        );
        described = body;
        if !xml_text(&described, "StackStatus")
            .unwrap_or_default()
            .ends_with("_IN_PROGRESS")
        {
            break;
        }
    }
    described
}

/// One output's value, the way a client reads it — an output reported with a key and no
/// value is what this reports, since that is the defect the rule is about.
fn value(case: &str, described: &str, key: &str) -> String {
    let outputs = wire_outputs(described);
    let output = output_for(&outputs, key).unwrap_or_else(|| {
        panic!("case {case:?}: expected a declared output {key:?}, got {described}")
    });
    output.value.clone().unwrap_or_else(|| {
        panic!(
            "case {case:?}: every Output of a CREATE_COMPLETE stack carries an OutputValue; \
             {key:?} carries none"
        )
    })
}

/// Every name a type publishes its ARN under answers the ARN the owning service reports for
/// the resource, for a stack that reached a complete status.
#[tokio::test]
async fn every_published_arn_attribute_name_answers_the_resources_own_arn() {
    for row in rows() {
        let router = router();
        let (code, body) = create_stack(&router, row.stack_name, &row.template, &[]).await;
        assert_eq!(
            code, 200,
            "case {:?}: CreateStack must answer 200, got {code} {body}",
            row.case
        );
        let described = poll_to_terminal(&router, row.case, row.stack_name).await;
        assert_eq!(
            xml_text(&described, "StackStatus").as_deref(),
            Some("CREATE_COMPLETE"),
            "case {:?}: expected the create to complete, got {described}",
            row.case
        );

        // `Ref` is the resource's name: the one the declaration stated, or the generated
        // one fitted to the owning service's own name rule where it stated none.
        let name = value(row.case, &described, "Name");
        match row.declared_name {
            Some(declared) => assert_eq!(
                name, declared,
                "case {:?}: expected Ref to answer the declared name, got {name:?}",
                row.case
            ),
            None => assert!(
                name.chars().count() <= MAX_RESOURCE_NAME_LEN
                    && name.starts_with(|first: char| first.is_ascii_lowercase())
                    && name.chars().all(|character| {
                        character.is_ascii_lowercase()
                            || character.is_ascii_digit()
                            || character == '-'
                    }),
                "case {:?}: expected a generated name fitted to the owning service's name \
                 rule, got {name:?}",
                row.case
            ),
        }

        let expected = format!(
            "arn:aws:{}:{REGION}:{ACCOUNT_ID}:{}",
            row.service,
            (row.path)(&name)
        );
        for key in ["Upper", "Mixed", "TypeUpper", "TypeMixed"] {
            let actual = value(row.case, &described, key);
            assert_eq!(
                actual, expected,
                "case {:?}: expected the output {key:?} to answer {expected:?}, got {actual:?}",
                row.case
            );
        }
    }
}

/// The catalogue half the rule rests on for the two MemoryDB groups: each publishes its ARN
/// under the upper-case member name its own service's model spells it with.
#[test]
fn each_memorydb_group_publishes_its_arn_under_the_models_member_name() {
    for resource_type in [MEMORYDB_PARAMETER_GROUP, MEMORYDB_SUBNET_GROUP] {
        let published = published_attribute_names(resource_type);
        assert_eq!(
            published,
            &[UPPER_CASE_ARN_ATTRIBUTE],
            "case {resource_type:?}: expected the ARN to be the type's published return \
             value, got {published:?}"
        );
    }
}
