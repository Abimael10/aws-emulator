// SPDX-License-Identifier: CC0-1.0

//! What an `AWS::RDS::DBInstance` whose declared `AssociatedRoles` Amazon RDS will not
//! associate reports on its own failed event, end to end: the CloudFormation calls over this
//! service's awsQuery wire, the role records over IAM's wire and the read-back over Amazon
//! RDS's own wire, through the one router `harness.rs` builds.
//!
//! RULE under test: **a declared property that is no member of the owning service's create is
//! carried to that service by the call that DOES take it, and a refusal of that call fails the
//! resource — reported as the exception the provider's SDK raised, that service's own sentence
//! followed by the SDK's diagnostic of the call (the service as its SDK names it, the HTTP
//! status it answered, the id of the call and how many attempts the SDK made at it), under the
//! `HandlerErrorCode` class that refusal falls in — so the stack rolls back and the owning
//! service holds no resource the refusal preceded.** For this type the property is
//! `AssociatedRoles` and the call is `AddRoleToDBInstance`, which Amazon RDS refuses whenever
//! it cannot assume the role the entry names: IAM holds no role under the name the ARN carries,
//! or the role it does hold trusts another service principal.
//!
//! Why it matters: a template AWS refuses must not deploy clean here — a stack whose database
//! can never use the role it declared is one that cannot exist in production — and the failed
//! resource's `ResourceStatusReason` is the only machine-readable account a client gets of why
//! the deployment rolled back. The SDK diagnostic is what attributes the failure to the service
//! that refused it, carries the id of the call that is correlated with that service's own log
//! of it, and states the attempt count a caller tells a first-attempt refusal from a retried
//! one by; a reason that stops at the service's sentence leaves every such parse with nothing.
//!
//! The rows differ along what the rule generalises over: WHY Amazon RDS cannot assume the
//! declared role (a role name nothing created, the same under an IAM path, a role the stack
//! itself creates trusting the object-storage principal, one trusting the compute principal)
//! and WHICH feature the entry names, beside the positive control — a role trusting the
//! principal Amazon RDS acts under, which is associated and reported `ACTIVE`, so what the
//! refused rows pin is a refusal rather than an estate nothing can deploy.

use serde_json::{json, Value};
use service_com_amazonaws_rds_amazonrdsv19::state::ROLE_NOT_TRUSTED_MESSAGE;

use crate::harness::{
    assert_sdk_diagnostic_with_attempts, create_stack, failed_resource_reason, member,
    quoted_and_class, router, settled_stack_status, stack_event_reasons, xml_blocks, xml_text,
    REGION,
};
use crate::stack_lifecycle::{iam, rds};

/// The SDK's own name for Amazon RDS, as the exception its SDK raises states it: that model's
/// own `sdkId` (`RDS`) written with only its initial capitalised.
const RDS_SERVICE: &str = "Rds";

/// The status Amazon RDS answers a role integration it cannot perform with — the query
/// protocol's service-common client refusal, since that operation models no fault for it
/// (`smithy_query(query="operation",operation="AddRoleToDBInstance")`).
const REFUSED_STATUS: u16 = 400;

/// The `HandlerErrorCode` class a refusal of the REQUEST falls in, one of the model's own
/// `HandlerErrorCode` values.
const INVALID_REQUEST: &str = "InvalidRequest";

/// The head the SDK's diagnostic of a refused call opens with, which is where the owning
/// service's own sentence ends.
const DIAGNOSTIC_OPENS: &str = " (Service: ";

/// The `StackStatus` a create every owning service accepted settles in, and the one a create
/// whose rolled-back resources were all removed again settles in.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const ROLLBACK_COMPLETE: &str = "ROLLBACK_COMPLETE";

/// The resource status a resource whose create its provider could not carry out records.
const CREATE_FAILED: &str = "CREATE_FAILED";

/// What Amazon RDS answers for a DB instance it does not hold: the `httpError` of the
/// `DBInstanceNotFoundFault` `DescribeDBInstances` models, and the `awsQueryError` code that
/// fault carries (`smithy_query(query="operation",operation="DescribeDBInstances")`).
const NOT_FOUND_STATUS: u16 = 404;
const DB_INSTANCE_NOT_FOUND: &str = "DBInstanceNotFound";

/// The logical ids the templates below declare their resources under.
const ROLE_ID: &str = "ImportRole";
const DB_INSTANCE_ID: &str = "DbInstance";

/// The engine every instance runs, and the class and storage it is offered with. RDS for
/// PostgreSQL is the family whose published `SupportedFeatureNames` carry the S3 features an
/// association names, which is where a client reads them (`DescribeDBEngineVersions`).
const ENGINE: &str = "postgres";
const INSTANCE_CLASS: &str = "db.t3.micro";
const ALLOCATED_STORAGE: &str = "20";

/// The service principal a role Amazon RDS acts under for a DB instance feature must trust,
/// and two principals of other services, which it must not accept in its place.
const RDS_PRINCIPAL: &str = "rds.amazonaws.com";
const STORAGE_PRINCIPAL: &str = "s3.amazonaws.com";
const COMPUTE_PRINCIPAL: &str = "ec2.amazonaws.com";

/// The `Status` an association that holds reports.
const ACTIVE: &str = "ACTIVE";

/// The `Capabilities` a create whose template names an IAM role under a name of its own has to
/// acknowledge ("if you have IAM resources with custom names, you must specify
/// CAPABILITY_NAMED_IAM").
const NAMED_IAM: &str = "CAPABILITY_NAMED_IAM";

/// The role one row's association names.
enum Declared {
    /// A role the STACK creates, trusting this service principal — so the ARN the association
    /// names is resolved off the role IAM really holds (`Fn::GetAtt`).
    Held(&'static str),
    /// A role ARN of this account whose resource field nothing holds: the `role/<name>` or
    /// `role/<path>/<name>` of a role that was never created.
    Absent(&'static str),
}

/// One declaration: the role its `AssociatedRoles` entry names, the feature the entry is for,
/// and whether Amazon RDS associates it.
struct Row {
    case: &'static str,
    stack: &'static str,
    role: Declared,
    /// `FeatureName`: "the name of the feature associated with the ... IAM role".
    feature_name: &'static str,
    /// Whether the association is made, which is what decides the whole outcome of the deploy.
    associated: bool,
}

const ROWS: &[Row] = &[
    Row {
        // The recorded shape: a well-formed IAM role ARN of this account naming a role nobody
        // created.
        case: "a role name nothing created",
        stack: "rds-role-absent",
        role: Declared::Absent("role/rds-role-absent-never-created"),
        feature_name: "s3Import",
        associated: false,
    },
    Row {
        case: "a role name under an IAM path nothing created",
        stack: "rds-role-absent-path",
        role: Declared::Absent("role/service-role/rds-role-absent-never-created"),
        feature_name: "s3Export",
        associated: false,
    },
    Row {
        case: "a role whose trust policy names the object-storage service's principal",
        stack: "rds-role-storage-trust",
        role: Declared::Held(STORAGE_PRINCIPAL),
        feature_name: "s3Import",
        associated: false,
    },
    Row {
        case: "a role whose trust policy names the compute service's principal",
        stack: "rds-role-compute-trust",
        role: Declared::Held(COMPUTE_PRINCIPAL),
        feature_name: "s3Export",
        associated: false,
    },
    Row {
        case: "a role whose trust policy names the principal Amazon RDS acts under",
        stack: "rds-role-trusted",
        role: Declared::Held(RDS_PRINCIPAL),
        feature_name: "s3Import",
        associated: true,
    },
];

/// The DB instance identifier one row's declaration asks for, which is also the handle Amazon
/// RDS is read back by.
fn db_instance_of(stack: &str) -> String {
    format!("{stack}-db")
}

/// The name one row's declared role is created under.
fn role_name_of(stack: &str) -> String {
    format!("{stack}-role")
}

/// An IAM role ARN of the account this emulator serves, over the resource field a row states.
fn role_arn(resource: &str) -> String {
    format!("arn:aws:iam::{}:{resource}", engine::emulator_account_id())
}

/// The trust policy a declared role carries: `sts:AssumeRole` for the one service principal
/// the row states.
fn trust_policy(principal: &str) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": principal },
            "Action": "sts:AssumeRole",
        }],
    })
}

/// The template one row deploys: the role it declares, where the row creates one, and the DB
/// instance whose one `AssociatedRoles` entry names it.
fn template(row: &Row) -> String {
    let mut resources = serde_json::Map::new();
    let named_role = match row.role {
        Declared::Held(principal) => {
            resources.insert(
                ROLE_ID.to_string(),
                json!({
                    "Type": "AWS::IAM::Role",
                    "Properties": {
                        "RoleName": role_name_of(row.stack),
                        "AssumeRolePolicyDocument": trust_policy(principal),
                    },
                }),
            );
            json!({ "Fn::GetAtt": [ROLE_ID, "Arn"] })
        }
        Declared::Absent(resource) => Value::String(role_arn(resource)),
    };
    resources.insert(
        DB_INSTANCE_ID.to_string(),
        json!({
            "Type": "AWS::RDS::DBInstance",
            "Properties": {
                "DBInstanceIdentifier": db_instance_of(row.stack),
                "Engine": ENGINE,
                "DBInstanceClass": INSTANCE_CLASS,
                "AllocatedStorage": ALLOCATED_STORAGE,
                "AssociatedRoles": [{
                    "RoleArn": named_role,
                    "FeatureName": row.feature_name,
                }],
            },
        }),
    );
    json!({ "Resources": Value::Object(resources) }).to_string()
}

/// The `(RoleArn, FeatureName, Status)` triples Amazon RDS reports on a DB instance, read one
/// per `AssociatedRoles` entry of the answer.
fn reported_roles(described: &str) -> Vec<(String, String, String)> {
    xml_blocks(described, "AssociatedRoles")
        .iter()
        .flat_map(|listed| xml_blocks(listed, "DBInstanceRole"))
        .map(|entry| {
            (
                xml_text(&entry, "RoleArn").unwrap_or_default(),
                xml_text(&entry, "FeatureName").unwrap_or_default(),
                xml_text(&entry, "Status").unwrap_or_default(),
            )
        })
        .collect()
}

/// Hold one failed resource's reason to the whole envelope: Amazon RDS's own sentence for a
/// role integration it cannot perform, that service's SDK diagnostic after it — asserted as the
/// SHAPE an SDK that counts attempts words, so the per-run id and attempt count are read rather
/// than fixed — and the class a refused request falls in.
fn assert_role_refusal_reason(case: &str, reason: &str) {
    let (quoted, class) = quoted_and_class(case, reason);
    let (sentence, _) = quoted.split_once(DIAGNOSTIC_OPENS).unwrap_or_else(|| {
        panic!("case {case:?}: expected the SDK diagnostic after the sentence, got {quoted:?}")
    });
    assert_eq!(
        sentence, ROLE_NOT_TRUSTED_MESSAGE,
        "case {case:?}: expected Amazon RDS's own sentence for a role integration it cannot \
         perform, got {quoted:?}"
    );
    assert_eq!(
        class, INVALID_REQUEST,
        "case {case:?}: expected the {INVALID_REQUEST} class, got {class:?} in {reason:?}"
    );
    assert_sdk_diagnostic_with_attempts(case, &quoted, RDS_SERVICE, REFUSED_STATUS);
}

#[tokio::test]
async fn a_db_instance_role_amazon_rds_will_not_associate_fails_in_the_sdk_envelope() {
    for row in ROWS {
        let case = row.case;
        let router = router();
        let identifier = db_instance_of(row.stack);
        let capabilities = match row.role {
            Declared::Held(_) => vec![member("Capabilities.member.1", NAMED_IAM)],
            Declared::Absent(_) => Vec::new(),
        };
        let (status, created) =
            create_stack(&router, row.stack, &template(row), &capabilities).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateStack — the declaration is one a \
             template may state — got {status} with body {created}"
        );
        let reached = settled_stack_status(case, &router, REGION, row.stack).await;

        if row.associated {
            assert_eq!(
                reached, CREATE_COMPLETE,
                "case {case:?}: expected the deploy to complete, got {reached} with failure \
                 reasons {:?}",
                stack_event_reasons(&router, REGION, row.stack).await
            );
            // The role IAM really holds is the one the association names, and the association
            // is reported for the feature the declaration asked for.
            let (status, held) = iam(
                &router,
                "GetRole",
                &[("RoleName", &role_name_of(row.stack))],
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected IAM to hold the declared role, got {status} with {held}"
            );
            let arn = xml_text(&held, "Arn").unwrap_or_default();
            let (status, described) = rds(
                &router,
                "DescribeDBInstances",
                &[("DBInstanceIdentifier", &identifier)],
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected Amazon RDS to resolve {identifier:?}, got {status} \
                 with {described}"
            );
            let reported = reported_roles(&described);
            let wanted = vec![(
                arn,
                row.feature_name.to_string(),
                ACTIVE.to_string(),
            )];
            assert_eq!(
                reported, wanted,
                "case {case:?}: expected the instance's associations {wanted:?}, got {reported:?}"
            );
            continue;
        }

        assert_eq!(
            reached, ROLLBACK_COMPLETE,
            "case {case:?}: expected the refused association to roll the stack back, got \
             {reached} with failure reasons {:?}",
            stack_event_reasons(&router, REGION, row.stack).await
        );
        let reason = failed_resource_reason(
            case,
            &router,
            REGION,
            row.stack,
            DB_INSTANCE_ID,
            CREATE_FAILED,
        )
        .await;
        assert_role_refusal_reason(case, &reason);

        // The refusal was real and the rollback took the instance away again: Amazon RDS holds
        // no DB instance for the declaration it refused, and says so in its own words.
        let (status, read) = rds(
            &router,
            "DescribeDBInstances",
            &[("DBInstanceIdentifier", &identifier)],
        )
        .await;
        assert_eq!(
            status, NOT_FOUND_STATUS,
            "case {case:?}: expected no DB instance after the rollback, got {status} with {read}"
        );
        assert_eq!(
            xml_text(&read, "Code").as_deref(),
            Some(DB_INSTANCE_NOT_FOUND),
            "case {case:?}: expected the modelled not-found code, got {read}"
        );
        assert_eq!(
            xml_text(&read, "Message").as_deref(),
            Some(format!("DBInstance {identifier} not found.").as_str()),
            "case {case:?}: expected that service's own sentence about the instance it does \
             not hold, got {read}"
        );
    }
}
