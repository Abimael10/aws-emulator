// SPDX-License-Identifier: CC0-1.0

//! Every operation that resolves a caller-named INSTANCE id is region-scoped.
//!
//! THE RULE: EC2 is a regional service reached through a regional endpoint, so the instances
//! an operation of it reads or acts on are the ones of the Region the request was signed
//! for. An instance that runs in another Region is invisible there, and an id naming one
//! names no instance at all — so the call is answered exactly the refusal that operation
//! answers for an instance id the account holds nothing under (EC2's per-operation
//! validators word that refusal their own way, which is why each row carries the code its
//! operation answers), and the instance is left exactly as it was.
//!
//! An absent error is the damaging divergence: a client that uses a describe as an existence
//! probe concludes the resource is there in a Region where it is not, and an operator acting
//! in the wrong Region moves an instance AWS would have protected by refusing.
//!
//! The rows differ along what the rule generalises over: WHICH operation resolves the id
//! (a report, an attribute read, an attribute write, a state write, a task create), how the
//! request names it (one `InstanceId` member, an `InstanceId.N` list), and which refusal that
//! operation words an absent instance with. The lifecycle writes — `StartInstances`,
//! `StopInstances`, `RebootInstances`, `TerminateInstances` — are the same rule, pinned in
//! `tests/all/instance_lifecycle_region_scope.rs`, and the paged reports of several
//! instances at once in `tests/all/region_scoped_state.rs`; this suite is about the
//! operations that resolve a NAMED instance.
//!
//! `ConfirmProductInstance` is the one operation of the class no row can probe: it resolves
//! the product code's OWNER before the instance, so a caller that owns no product code is
//! refused `AuthFailure` whichever Region it asks in, and the instance is never reached.

use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{
    call, described_instance_state, element, error_code, error_message, indexed,
};

/// The Region the instance of this suite runs in.
const HOME: &str = "us-west-2";

/// Another Region of the same partition, reached through its own endpoint: the instances of
/// `HOME` are invisible here.
const ELSEWHERE: &str = "us-east-1";

/// An AMI id of the right FORM that this account holds no image for: which image an instance
/// came from has nothing to do with which Region it runs in, so one serves every row.
const AMI: &str = "ami-0123456789abcdef0";

/// The code EC2 answers for an instance id the signed Region holds no instance under.
const NOT_FOUND: &str = "InvalidInstanceID.NotFound";

/// The code the validators that do NOT tell an unreadable id from an absent one answer for
/// the same value — the console readers, the root-volume replacement, the Mac modification
/// task creates and the suppression enable.
const MALFORMED: &str = "InvalidInstanceID.Malformed";

fn p(name: &str, value: &str) -> (String, String) {
    (name.to_string(), value.to_string())
}

/// One row of the table: the operation, the refusal it words an instance the signed Region
/// does not hold with, and whether the same call in the instance's OWN Region is served
/// (`false` for a row whose other preconditions — a product code the caller owns, a Mac
/// instance type, a scheduled event — refuse it for a reason of their own, which is itself
/// the proof that the Region scope is not what refused it).
struct Row {
    action: &'static str,
    absent_code: &'static str,
    served_at_home: bool,
}

const fn row(action: &'static str, absent_code: &'static str, served_at_home: bool) -> Row {
    Row {
        action,
        absent_code,
        served_at_home,
    }
}

/// The operations this rule covers, each named by the action a caller sends.
fn rows() -> Vec<Row> {
    vec![
        // Reports of the instance and of what hangs off it.
        row("DescribeInstances", NOT_FOUND, true),
        row("DescribeInstanceStatus", NOT_FOUND, true),
        row("DescribeInstanceCreditSpecifications", NOT_FOUND, true),
        row("DescribeInstanceImageMetadata", NOT_FOUND, true),
        row("DescribeInstanceAttribute", NOT_FOUND, true),
        row("DescribeApplicationStatus", NOT_FOUND, true),
        row("DescribeClassicLinkInstances", NOT_FOUND, true),
        row("GetPasswordData", NOT_FOUND, true),
        row("GetLaunchTemplateData", NOT_FOUND, true),
        row("GetInstanceUefiData", NOT_FOUND, false),
        row("GetConsoleOutput", MALFORMED, true),
        row("GetConsoleScreenshot", MALFORMED, false),
        row("DescribeInstanceSqlHaStates", NOT_FOUND, true),
        // Writes over the instance.
        row("MonitorInstances", NOT_FOUND, true),
        row("UnmonitorInstances", NOT_FOUND, true),
        row("ModifyInstanceAttribute", NOT_FOUND, true),
        row("ModifyInstanceMetadataOptions", NOT_FOUND, true),
        row("ModifyInstanceMaintenanceOptions", NOT_FOUND, false),
        row("ModifyInstanceNetworkPerformanceOptions", NOT_FOUND, false),
        row("ModifyInstanceCapacityReservationAttributes", NOT_FOUND, false),
        row("ModifyInstancePlacement", NOT_FOUND, false),
        row("ModifyPrivateDnsNameOptions", "UnknownResource", false),
        row("ModifyInstanceEventStartTime", NOT_FOUND, false),
        row("ResetInstanceAttribute", NOT_FOUND, true),
        row("ReportInstanceStatus", NOT_FOUND, false),
        row("SendDiagnosticInterrupt", NOT_FOUND, true),
        row("AttachVolume", NOT_FOUND, false),
        row("EnableInstanceSqlHaStandbyDetections", NOT_FOUND, true),
        row("DisableInstanceSqlHaStandbyDetections", NOT_FOUND, true),
        row("EnableApplicationStatusCheckSuppression", MALFORMED, false),
        row("DisableApplicationStatusCheckSuppression", NOT_FOUND, false),
        // Creates that take the instance as their subject.
        row("CreateReplaceRootVolumeTask", MALFORMED, false),
        row("CreateInstanceExportTask", "InvalidParameter", false),
        row("CreateMacSystemIntegrityProtectionModificationTask", MALFORMED, false),
        row("CreateDelegateMacVolumeOwnershipTask", MALFORMED, false),
    ]
}

/// The request fields each row sends: the instance id the way that operation's member names
/// it, plus the minimum every other member of the request needs so that the instance is what
/// the call turns on.
fn params(action: &str, instance_id: &str, volume_id: &str) -> Vec<(String, String)> {
    let one = |fields: &[(&str, &str)]| -> Vec<(String, String)> {
        let mut params = vec![p("InstanceId", instance_id)];
        params.extend(fields.iter().map(|(name, value)| p(name, value)));
        params
    };
    match action {
        "DescribeInstances"
        | "DescribeInstanceStatus"
        | "DescribeInstanceCreditSpecifications"
        | "DescribeInstanceImageMetadata"
        | "DescribeApplicationStatus"
        | "DescribeClassicLinkInstances"
        | "DescribeInstanceSqlHaStates"
        | "MonitorInstances"
        | "UnmonitorInstances"
        | "EnableInstanceSqlHaStandbyDetections"
        | "DisableInstanceSqlHaStandbyDetections"
        | "EnableApplicationStatusCheckSuppression"
        | "DisableApplicationStatusCheckSuppression" => indexed("InstanceId", &[instance_id]),
        "ReportInstanceStatus" => {
            let mut params = indexed("InstanceId", &[instance_id]);
            params.push(p("Status", "impaired"));
            params.extend(indexed("ReasonCode", &["other"]));
            params
        }
        "DescribeInstanceAttribute" => one(&[("Attribute", "instanceType")]),
        "ResetInstanceAttribute" => one(&[("Attribute", "sourceDestCheck")]),
        "ModifyInstanceAttribute" => one(&[("SourceDestCheck.Value", "false")]),
        "ModifyInstanceMetadataOptions" => one(&[("HttpTokens", "optional")]),
        "ModifyInstanceMaintenanceOptions" => one(&[("AutoRecovery", "default")]),
        "ModifyInstanceNetworkPerformanceOptions" => one(&[("BandwidthWeighting", "default")]),
        "ModifyInstanceCapacityReservationAttributes" => one(&[(
            "CapacityReservationSpecification.CapacityReservationPreference",
            "none",
        )]),
        "ModifyInstancePlacement" => one(&[("Tenancy", "default")]),
        "ModifyPrivateDnsNameOptions" => one(&[("EnableResourceNameDnsARecord", "true")]),
        "ModifyInstanceEventStartTime" => one(&[
            ("InstanceEventId", "instance-event-0123456789abcdef0"),
            ("NotBefore", "2030-01-01T00:00:00Z"),
        ]),
        "AttachVolume" => one(&[("VolumeId", volume_id), ("Device", "/dev/sdf")]),
        "CreateInstanceExportTask" => one(&[
            ("TargetEnvironment", "vmware"),
            ("ExportToS3.S3Bucket", "exports"),
        ]),
        "CreateMacSystemIntegrityProtectionModificationTask" => {
            one(&[("MacSystemIntegrityProtectionStatus", "disabled")])
        }
        // "Specifies the following credentials: Internal disk administrative user" —
        // `@required`, so the task create is refused for a missing one before it resolves the
        // instance (`CreateDelegateMacVolumeOwnershipTaskRequest$MacCredentials`).
        "CreateDelegateMacVolumeOwnershipTask" => one(&[("MacCredentials", "mac-admin-secret")]),
        // Every other row names the instance and nothing else.
        _ => one(&[]),
    }
}

/// Launch one instance in `HOME` and answer its id.
async fn launch(router: &MultiServiceRouter) -> String {
    let (status, body) = call(
        router,
        HOME,
        "RunInstances",
        &[p("ImageId", AMI), p("MinCount", "1"), p("MaxCount", "1")],
    )
    .await;
    assert_eq!(
        status, 200,
        "the launch must be served, got {status} with body {body}"
    );
    element(&body, "instanceId").unwrap_or_else(|| panic!("expected an instanceId in {body}"))
}

/// An instance of another Region names no instance, whichever operation resolves the id:
/// every row is refused with its own absent-instance refusal, nothing of the instance moves,
/// and the same call in the instance's own Region is not refused for that reason.
#[tokio::test]
async fn an_instance_of_another_region_names_no_instance() {
    let (router, _engine) = super::ec2_query::emulator();
    let instance_id = launch(&router).await;
    // One volume of `HOME`, so the row that attaches one turns on the instance rather than
    // on a volume that is not there.
    let (status, body) = call(
        &router,
        HOME,
        "CreateVolume",
        &[p("AvailabilityZone", "us-west-2a"), p("Size", "8")],
    )
    .await;
    assert_eq!(
        status, 200,
        "the volume must be created, got {status} with body {body}"
    );
    let volume_id =
        element(&body, "volumeId").unwrap_or_else(|| panic!("expected a volumeId in {body}"));

    // The instance is enrolled in SQL Server High Availability standby detection in `HOME`,
    // so the row that reports that enrolment turns on which Region the request reached
    // rather than on whether the instance was ever enrolled.
    let (status, body) = call(
        &router,
        HOME,
        "EnableInstanceSqlHaStandbyDetections",
        &indexed("InstanceId", &[instance_id.as_str()]),
    )
    .await;
    assert_eq!(
        status, 200,
        "the enrolment must be served in the instance's own Region, got {status} with body \
         {body}"
    );

    let before = described_instance_state(&router, HOME, &instance_id).await;

    for Row {
        action,
        absent_code,
        served_at_home,
    } in rows()
    {
        let case = action;
        let fields = params(action, &instance_id, &volume_id);

        // The signed Region holds no such instance, so the id names none.
        let (status, body) = call(&router, ELSEWHERE, action, &fields).await;
        assert_eq!(
            status, 400,
            "case {case:?}: an instance of another Region names no instance here, so the \
             call is refused, got {status} with body {body}"
        );
        let code = error_code(&body);
        assert_eq!(
            code, absent_code,
            "case {case:?}: expected {absent_code}, got {code:?} in {body}"
        );
        let message = error_message(&body);
        assert!(
            message.contains(&instance_id),
            "case {case:?}: the refusal names the id the request sent, got {message:?}"
        );

        // Nothing moved: the refusal is a refusal, not a partial write.
        let after = described_instance_state(&router, HOME, &instance_id).await;
        assert_eq!(
            after, before,
            "case {case:?}: the instance must be left exactly as it was, expected \
             {before:?}, got {after:?}"
        );

        // And the same call in the instance's OWN Region does not answer that refusal, so
        // what the row above proves is the Region scope rather than a refusal of the
        // operation itself.
        let (status, body) = call(&router, HOME, action, &fields).await;
        if served_at_home {
            assert_eq!(
                status, 200,
                "case {case:?}: the same call in the instance's own Region must be served, \
                 got {status} with body {body}"
            );
        } else {
            let code = error_code(&body);
            assert_ne!(
                code, absent_code,
                "case {case:?}: in the instance's own Region the instance EXISTS, so the \
                 call must not be refused {absent_code}, got {body}"
            );
        }
    }

    // The instance is still the one instance the account holds in its own Region, and still
    // reports the state it was launched into.
    let (status, body) = call(
        &router,
        HOME,
        "DescribeInstances",
        &indexed("InstanceId", &[instance_id.as_str()]),
    )
    .await;
    assert_eq!(
        status, 200,
        "the instance must still be reported in its own Region, got {status} with body {body}"
    );
    assert_eq!(
        element(&body, "instanceId").as_deref(),
        Some(instance_id.as_str()),
        "the read must report the instance it was asked for, got {body}"
    );
}
