// SPDX-License-Identifier: CC0-1.0

//! WHICH fault a request's own identifiers are diagnosed with, for the two reference members
//! every cluster-scoped and revision-scoped call takes.
//!
//! Two rules, each asserted over a table that spans the class rather than one recorded
//! request:
//!
//! 1. A `cluster` reference is "The short name or full Amazon Resource Name (ARN) of the
//!    cluster" (every `cluster` member's doc,
//!    `smithy_query(query="doc",shape="DescribeServicesRequest",member="cluster")`), so a
//!    value that is NEITHER a cluster ARN nor a cluster name names no cluster in any account:
//!    it is refused as the bad parameter it is, naming the member, the pattern a cluster name
//!    has to match and the value the request spelled — and refused BEFORE the cluster is
//!    looked up, so "this value could never name a cluster" is never reported as "this
//!    account does not hold that cluster", which is what a well-formed name that resolves to
//!    nothing still earns. The pattern is the character set and the length bound
//!    `CreateCluster$clusterName` documents ("Up to 255 letters (uppercase and lowercase),
//!    numbers, underscores, and hyphens are allowed"), so the rows walk both of its published
//!    bounds and one past the longer.
//! 2. A `taskDefinition` reference is "The family and revision (family:revision) or full ARN
//!    of the task definition" (`RunTaskRequest$taskDefinition`), and a family is the same
//!    character set and bound (`RegisterTaskDefinition$family`). A value that names no
//!    revision carries exactly one of three faults, and each is diagnosed as itself: the
//!    member left BLANK, a family part spelled in characters a family name is not spelled in,
//!    or a family that IS a family name beside a revision part that names no revision number.
//!    The sentence and the modelled class each fault earns belong to the OPERATION that
//!    resolved the reference — a read reports the revision it could not describe, a call that
//!    puts the revision to work reports the parameter it was given — and neither swallows the
//!    LOOKUP refusal a well-formed reference naming nothing earns.
//!
//! Every case drives the emulator through the shared wire harness in [`crate::ecs_wire`], the
//! same front door a client uses, so nothing reaches into the state struct and a response
//! that is not computed from persisted state cannot pass.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ecs_wire::{
    answered, cluster_with_capacity, refusal, register_definition, service, string, ACCOUNT,
    SIGNED_REGION,
};

/// The cluster the rows below work in.
const CLUSTER: &str = "diagnosiscluster";

/// The family holding the one revision the rows below resolve.
const FAMILY: &str = "diagnosisfamily";

/// The modelled error a bad parameter is answered in.
const INVALID_PARAMETER: &str = "InvalidParameterException";

/// The modelled error an identifier this service can make nothing of is answered in.
const CLIENT_EXCEPTION: &str = "ClientException";

/// The modelled error a cluster reference that resolves to no cluster record is answered in.
const CLUSTER_NOT_FOUND_EXCEPTION: &str = "ClusterNotFoundException";

/// The sentence a cluster reference that resolves to no cluster record carries.
const CLUSTER_NOT_FOUND: &str = "Cluster not found.";

/// The sentence a `taskDefinition` whose family part is spelled in characters a family name
/// is not spelled in carries.
const FAMILY_CHARACTERS: &str = "Family contains invalid characters.";

/// The sentence a READ of a revision answers a reference it cannot resolve.
const UNABLE_TO_DESCRIBE: &str = "Unable to describe task definition.";

/// The sentence a call that would put a revision to WORK answers a reference that resolves to
/// no revision.
const NOT_FOUND_TO_RUN: &str = "TaskDefinition not found.";

/// The sentence a READ answers a `taskDefinition` member left blank.
const TASK_DEFINITION_BLANK: &str = "Task Definition can not be blank.";

/// The sentence a call that would put a revision to work answers a reference carrying no
/// family text.
const FAMILY_BLANK: &str = "Family should not be null or empty.";

/// The sentence a call that would put a revision to work answers a revision part that names
/// no revision number.
const REVISION_NUMBER: &str = "Invalid revision number.";

/// The refusal a `cluster` value outside the two forms the member documents earns: the
/// member, the pattern a cluster name has to match, and the value as the request spelled it.
fn cluster_pattern_message(value: &str) -> String {
    format!(r"Cluster must match ^[a-zA-Z0-9\-_]{{1,255}}$, but was {value}")
}

/// What a row expects: the request is served (HTTP 200), or refused with this modelled code
/// and sentence.
#[derive(Debug, Clone)]
enum Expected {
    Served,
    Refused(&'static str, String),
}

/// An ECS ARN of this caller's own account and Region, for the resource path `resource`.
fn ecs_arn(resource: &str) -> String {
    format!("arn:aws:ecs:{SIGNED_REGION}:{ACCOUNT}:{resource}")
}

/// Assert `operation` answers `body` the way the row expects.
async fn expect(
    router: &MultiServiceRouter,
    operation: &str,
    body: Value,
    expected: &Expected,
    case: &str,
) {
    match expected {
        Expected::Served => {
            let (status, answer) = answered(router, operation, body).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected {operation} to serve the request, got {status} with \
                 {answer}"
            );
        }
        Expected::Refused(code, message) => {
            let (actual_code, actual_message) = refusal(router, operation, body, case).await;
            assert_eq!(
                (actual_code.as_str(), actual_message.as_str()),
                (*code, message.as_str()),
                "case {case:?}: expected {code}/{message:?} from {operation}, got \
                 {actual_code}/{actual_message:?}"
            );
        }
    }
}

#[tokio::test]
async fn a_cluster_reference_outside_the_two_forms_it_documents_is_the_bad_parameter_it_is() {
    let router = service();
    let case = "setup";
    let cluster_arn = cluster_with_capacity(&router, CLUSTER, case).await;
    register_definition(&router, FAMILY, case).await;

    // The rows differ along the FORM the value takes (a name, a cluster ARN, an ARN of
    // another resource kind, an ARN truncated at each of its segments), the ALPHABET the
    // name is spelled in (the set the pattern admits, a dot, a space, a slash), its LENGTH
    // (both published bounds and one past the longer) and whether the cluster it names is
    // one this account holds. The recorded divergence — a truncated ECS ARN reported as a
    // cluster this account does not hold — is ONE row.
    let longest_name = "c".repeat(255);
    let one_past_the_bound = "c".repeat(256);
    let truncated_arn = format!("arn:aws:ecs:{SIGNED_REGION}:{ACCOUNT}");
    let foreign_kind_arn = ecs_arn(&format!("service/{CLUSTER}/web"));
    let resource_type_only = ecs_arn("cluster");
    let empty_resource_arn = ecs_arn("cluster/");
    let pattern =
        |value: &str| Expected::Refused(INVALID_PARAMETER, cluster_pattern_message(value));
    let not_found = Expected::Refused(CLUSTER_NOT_FOUND_EXCEPTION, CLUSTER_NOT_FOUND.to_string());
    let rows: Vec<(&str, String, Expected)> = vec![
        (
            "the stored cluster, by name",
            CLUSTER.to_string(),
            Expected::Served,
        ),
        (
            "the stored cluster, by its full ARN",
            cluster_arn.clone(),
            Expected::Served,
        ),
        (
            "a well-formed name this account holds no cluster of",
            "ghostcluster".to_string(),
            not_found.clone(),
        ),
        (
            "a one-character name, the shortest the pattern admits",
            "c".to_string(),
            not_found.clone(),
        ),
        (
            "a 255-character name, the longest the pattern admits",
            longest_name,
            not_found.clone(),
        ),
        (
            "a name one character past the documented bound",
            one_past_the_bound.clone(),
            pattern(&one_past_the_bound),
        ),
        (
            "an ECS ARN truncated before its resource",
            truncated_arn.clone(),
            pattern(&truncated_arn),
        ),
        (
            "an ARN of another resource kind",
            foreign_kind_arn.clone(),
            pattern(&foreign_kind_arn),
        ),
        (
            "a cluster ARN carrying the resource type and no name",
            resource_type_only.clone(),
            pattern(&resource_type_only),
        ),
        (
            "a cluster ARN whose resource name is empty",
            empty_resource_arn.clone(),
            pattern(&empty_resource_arn),
        ),
        (
            "a name carrying a dot",
            "my.cluster".to_string(),
            pattern("my.cluster"),
        ),
        (
            "a name carrying a space",
            "my cluster".to_string(),
            pattern("my cluster"),
        ),
        (
            "a name carrying a slash",
            "clusters/mine".to_string(),
            pattern("clusters/mine"),
        ),
        (
            // "If you do not specify a cluster, the default cluster is assumed": a value
            // with no text in it names no cluster of its own, so it is read as the default
            // cluster, which this account holds none of.
            "a blank value, which names the default cluster",
            "   ".to_string(),
            not_found.clone(),
        ),
    ];

    // The member is the same member on every operation that takes it, so the rule is
    // asserted across reads and a placement rather than on the one operation the report
    // named.
    let request_for = |operation: &str, cluster: &str| -> Value {
        match operation {
            "DescribeServices" => json!({ "cluster": cluster, "services": ["web"] }),
            "ListTasks" => json!({ "cluster": cluster }),
            "ListServices" => json!({ "cluster": cluster }),
            "ListContainerInstances" => json!({ "cluster": cluster }),
            "RunTask" => json!({
                "cluster": cluster,
                "taskDefinition": FAMILY,
                "launchType": "EC2",
                "count": 1,
            }),
            other => panic!("unexpected operation {other:?}"),
        }
    };
    for operation in [
        "DescribeServices",
        "ListTasks",
        "ListServices",
        "ListContainerInstances",
        "RunTask",
    ] {
        for (row, cluster, expected) in &rows {
            let case = format!("{operation}: {row}");
            expect(
                &router,
                operation,
                request_for(operation, cluster),
                expected,
                &case,
            )
            .await;
        }
    }
}

#[tokio::test]
async fn a_task_definition_reference_is_diagnosed_for_the_fault_it_actually_carries() {
    let router = service();
    let case = "setup";
    cluster_with_capacity(&router, CLUSTER, case).await;
    let revision_arn = register_definition(&router, FAMILY, case).await;
    // The service the `UpdateService` rows point at a reference.
    let created = crate::ecs_wire::ok(
        &router,
        "CreateService",
        json!({
            "cluster": CLUSTER,
            "serviceName": "updatable",
            "taskDefinition": FAMILY,
            "desiredCount": 0,
            "launchType": "EC2",
        }),
        case,
    )
    .await;
    assert_eq!(
        string(&created["service"], "taskDefinition", case),
        revision_arn,
        "case {case:?}: expected the service to run the stored revision, got {created}"
    );

    // The rows differ along WHICH part of the reference is at fault (none, the family, the
    // revision), the FORM the value takes (bare family, `family:revision`, the wildcard
    // revision, an ARN of another resource kind), and whether what it names is stored. The
    // two answers per row are the requirement: the read's and the work call's, which differ
    // exactly where the sentence belongs to the operation.
    let foreign_kind_arn = ecs_arn(&format!("service/{CLUSTER}/web"));
    let blank_read = Expected::Refused(INVALID_PARAMETER, TASK_DEFINITION_BLANK.to_string());
    let blank_work = Expected::Refused(INVALID_PARAMETER, FAMILY_BLANK.to_string());
    let characters = Expected::Refused(CLIENT_EXCEPTION, FAMILY_CHARACTERS.to_string());
    let revision_read = Expected::Refused(CLIENT_EXCEPTION, UNABLE_TO_DESCRIBE.to_string());
    let revision_work = Expected::Refused(INVALID_PARAMETER, REVISION_NUMBER.to_string());
    let missing_read = Expected::Refused(CLIENT_EXCEPTION, UNABLE_TO_DESCRIBE.to_string());
    let missing_work = Expected::Refused(CLIENT_EXCEPTION, NOT_FOUND_TO_RUN.to_string());
    let rows: Vec<(&str, String, Expected, Expected)> = vec![
        (
            "the stored family, bare",
            FAMILY.to_string(),
            Expected::Served,
            Expected::Served,
        ),
        (
            "the stored revision, as family:revision",
            format!("{FAMILY}:1"),
            Expected::Served,
            Expected::Served,
        ),
        (
            "the stored revision, by its full ARN",
            revision_arn.clone(),
            Expected::Served,
            Expected::Served,
        ),
        (
            "the stored family with the wildcard revision",
            format!("{FAMILY}:*"),
            Expected::Served,
            Expected::Served,
        ),
        (
            "the member left empty",
            String::new(),
            blank_read.clone(),
            blank_work.clone(),
        ),
        (
            "nothing but whitespace",
            "   ".to_string(),
            blank_read.clone(),
            blank_work.clone(),
        ),
        (
            "a reference whose family part carries no text",
            ":1".to_string(),
            blank_read.clone(),
            blank_work.clone(),
        ),
        (
            "a family part outside the family character set",
            "bad!family".to_string(),
            characters.clone(),
            characters.clone(),
        ),
        (
            "a family part carrying a space",
            "bad family:1".to_string(),
            characters.clone(),
            characters.clone(),
        ),
        (
            "an ARN of another resource kind",
            foreign_kind_arn,
            characters.clone(),
            characters.clone(),
        ),
        (
            "a revision part that is no number",
            format!("{FAMILY}:latest"),
            revision_read.clone(),
            revision_work.clone(),
        ),
        (
            "a revision of zero",
            format!("{FAMILY}:0"),
            revision_read.clone(),
            revision_work.clone(),
        ),
        (
            "a negative revision",
            format!("{FAMILY}:-2"),
            revision_read.clone(),
            revision_work.clone(),
        ),
        (
            "a revision number this family holds none of",
            format!("{FAMILY}:9"),
            missing_read.clone(),
            missing_work.clone(),
        ),
        (
            "a family that was never registered",
            "ghostfamily".to_string(),
            missing_read.clone(),
            missing_work.clone(),
        ),
    ];

    for (slot, (row, reference, read_answer, work_answer)) in rows.iter().enumerate() {
        let case = format!("DescribeTaskDefinition: {row}");
        expect(
            &router,
            "DescribeTaskDefinition",
            json!({ "taskDefinition": reference }),
            read_answer,
            &case,
        )
        .await;

        // The same value on the calls that put a revision to WORK: a placement, a create and
        // an update, so the answer is keyed on the value's fault and on the KIND of call
        // rather than on one operation.
        let work_request = |operation: &str| -> Value {
            match operation {
                "RunTask" => json!({
                    "cluster": CLUSTER,
                    "taskDefinition": reference,
                    "launchType": "EC2",
                    "count": 1,
                }),
                "CreateService" => json!({
                    "cluster": CLUSTER,
                    "serviceName": format!("fresh{slot}"),
                    "taskDefinition": reference,
                    "desiredCount": 0,
                    "launchType": "EC2",
                }),
                "UpdateService" => json!({
                    "cluster": CLUSTER,
                    "service": "updatable",
                    "taskDefinition": reference,
                }),
                other => panic!("unexpected operation {other:?}"),
            }
        };
        for operation in ["RunTask", "CreateService", "UpdateService"] {
            let case = format!("{operation}: {row}");
            expect(
                &router,
                operation,
                work_request(operation),
                work_answer,
                &case,
            )
            .await;
        }
    }
}
