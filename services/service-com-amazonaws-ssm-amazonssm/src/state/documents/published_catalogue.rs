// SPDX-License-Identifier: CC0-1.0

//! The CATALOGUE of SSM documents Systems Manager publishes: every document that exists in
//! every account and Region without anybody creating it.
//!
//! An unfiltered `ListDocuments` in a fresh account reports this catalogue — several
//! hundred documents, all owned by `Amazon` — and every read of a document
//! (`DescribeDocument`, `GetDocument`, `ListDocumentVersions`) resolves a name it holds
//! without a create, so what a client discovers by listing is what it can then run.
//!
//! What this file carries per entry is the catalogue's own metadata, which is Amazon's to
//! declare and is not in this service's model: the NAME, the `DocumentType`, the
//! `TargetType` where the catalogue names one, and WHEN Amazon published it. Everything
//! else a read reports — the description, the schema version, the parameters, the platform
//! types, the hash — is derived from the entry's body by the one content reader
//! ([`parse_document_content`]), exactly as it is for a document a caller created.
//!
//! Two kinds of body appear here ([`PublishedBody`]): the bodies this crate AUTHORS for the
//! documents whose parameters the Systems Manager references document one by one, and the
//! FAMILY STAND-IN of every other entry. A stand-in names the document and declares what
//! its FAMILY has in common - the schema version its type is published at
//! ([`supported_schema_versions`]), the assume-role parameter every published Automation
//! runbook takes, the session type a session document opens, the publisher of a Distributor
//! package - and nothing that belongs to the one document: none of the runbook-specific
//! parameters Amazon publishes for it, and no step, so the document is reported compatible
//! with every platform the model lists, which is what Systems Manager reports for a
//! document whose steps restrict nothing.
//!
//! The families, and what gives each its `DocumentType` (the modelled enum
//! `crate::ops::types::DOCUMENT_TYPE_VALUES`), are named in the comment above each group
//! below.

use super::*;

/// The `DocumentType` of a runbook Systems Manager Automation runs.
const AUTOMATION: &str = "Automation";

/// The `DocumentType` of a document Run Command runs on a managed node.
const COMMAND: &str = "Command";

/// The `DocumentType` of a Distributor package.
const PACKAGE: &str = "Package";

/// The `DocumentType` of a document Session Manager opens a session with.
const SESSION: &str = DOCUMENT_TYPE_SESSION;

/// The `DocumentType` of a change template Change Manager raises a request from.
const CHANGE_TEMPLATE: &str = "Automation.ChangeTemplate";

/// The `DocumentType` of an AWS Config conformance pack template.
const CONFORMANCE_PACK_TEMPLATE: &str = "ConformancePackTemplate";

/// The `DocumentType` of a Quick Setup configuration definition.
const QUICK_SETUP: &str = "QuickSetup";

/// The `DocumentType` of a document whose content is a CloudFormation template.
const CLOUD_FORMATION: &str = "CloudFormation";

/// The `TargetType` of a document that runs on, or acts on, an Amazon EC2 instance:
/// "/AWS::EC2::Instance" is the example `DocumentIdentifier$TargetType` itself gives.
const EC2_INSTANCE: Option<&str> = Some("/AWS::EC2::Instance");

/// The `TargetType` of a document that acts on an Amazon S3 bucket.
const S3_BUCKET: Option<&str> = Some("/AWS::S3::Bucket");

/// The ONE publication date a catalogue entry carries when this crate does not hold that
/// document's own date, in milliseconds since the Unix epoch: 2016-01-01T00:00:00Z, the
/// turn of the year Systems Manager began publishing documents in.
///
/// `CreatedDate` is a fact of the catalogue rather than a reading of this store's clock
/// ([`PublishedDocument::created_date_millis`]), and the day Amazon published any one
/// document is not in this service's model: the entries whose date this crate holds carry
/// it as a literal, and every other entry carries THIS one instant — one value, so the date
/// is visibly the catalogue's stand-in rather than a per-document claim, and it is identical
/// in every account, every Region and every run of the emulator, so a restart cannot move
/// it.
const CATALOGUE_PUBLISHED_MILLIS: u64 = 1_451_606_400_000;

/// A catalogue entry whose body this crate AUTHORS: the name, the type, the body const, the
/// publication date the recorded catalogue pins for it, and the target type the catalogue
/// names (or `None`).
macro_rules! authored {
    ($name:expr, $type:expr, $content:expr, $millis:literal, $target:expr) => {
        PublishedDocument {
            name: $name,
            document_type: $type,
            content: $content,
            body: PublishedBody::Authored,
            created_date_millis: $millis,
            target_type: $target,
        }
    };
}

/// An Automation runbook of the catalogue, carried with the family stand-in body.
macro_rules! runbook {
    ($name:literal) => {
        runbook!($name, None)
    };
    ($name:literal, $target:expr) => {
        PublishedDocument {
            name: $name,
            document_type: AUTOMATION,
            content: concat!(
                "{\n  \"schemaVersion\": \"0.3\",\n  \"description\": \"",
                $name,
                ", an Automation runbook Amazon Web Services Systems Manager publishes.\
                 \",\n  \"assumeRole\": \"{{ AutomationAssumeRole }}\",\n  \"parameters\": \
                 {\n    \"AutomationAssumeRole\": {\n      \"type\": \"String\",\n      \
                 \"description\": \"(Optional) The ARN of the role that allows Automation \
                 to perform the actions on your behalf.\",\n      \"default\": \"\"\n    \
                 }\n  }\n}"
            ),
            body: PublishedBody::FamilyStandIn,
            created_date_millis: CATALOGUE_PUBLISHED_MILLIS,
            target_type: $target,
        }
    };
}

/// A command document of the catalogue, carried with the family stand-in body.
macro_rules! command {
    ($name:literal) => {
        command!($name, None)
    };
    ($name:literal, $target:expr) => {
        PublishedDocument {
            name: $name,
            document_type: COMMAND,
            content: concat!(
                "{\n  \"schemaVersion\": \"2.2\",\n  \"description\": \"",
                $name,
                ", a command document Amazon Web Services Systems Manager publishes.\"\n}"
            ),
            body: PublishedBody::FamilyStandIn,
            created_date_millis: CATALOGUE_PUBLISHED_MILLIS,
            target_type: $target,
        }
    };
}

/// A Distributor package of the catalogue, carried with the family stand-in body.
macro_rules! package {
    ($name:literal) => {
        PublishedDocument {
            name: $name,
            document_type: PACKAGE,
            content: concat!(
                "{\n  \"schemaVersion\": \"2.0\",\n  \"description\": \"",
                $name,
                ", a Distributor package Amazon Web Services Systems Manager publishes.\",\n  \
                 \"publisher\": \"Amazon Web Services\",\n  \"version\": \"1.0\"\n}"
            ),
            body: PublishedBody::FamilyStandIn,
            created_date_millis: CATALOGUE_PUBLISHED_MILLIS,
            target_type: None,
        }
    };
}

/// A session document of the catalogue, carried with the family stand-in body.
macro_rules! session {
    ($name:literal) => {
        PublishedDocument {
            name: $name,
            document_type: SESSION,
            content: concat!(
                "{\n  \"schemaVersion\": \"1.0\",\n  \"description\": \"",
                $name,
                ", a session document Amazon Web Services Systems Manager publishes.\",\n  \
                 \"sessionType\": \"Standard_Stream\"\n}"
            ),
            body: PublishedBody::FamilyStandIn,
            created_date_millis: CATALOGUE_PUBLISHED_MILLIS,
            target_type: None,
        }
    };
}

/// A change template of the catalogue, carried with the family stand-in body. A change
/// template declares the same schema version an Automation runbook does
/// ([`supported_schema_versions`]), because Change Manager runs one as a runbook.
macro_rules! change_template {
    ($name:literal) => {
        PublishedDocument {
            name: $name,
            document_type: CHANGE_TEMPLATE,
            content: concat!(
                "{\n  \"schemaVersion\": \"0.3\",\n  \"description\": \"",
                $name,
                ", a change template Amazon Web Services Systems Manager publishes.\"\n}"
            ),
            body: PublishedBody::FamilyStandIn,
            created_date_millis: CATALOGUE_PUBLISHED_MILLIS,
            target_type: None,
        }
    };
}

/// A template-typed entry of the catalogue — a conformance pack template, a Quick Setup
/// definition or a CloudFormation template — carried with the family stand-in body. Its
/// type carries no Systems Manager document schema ([`supported_schema_versions`]), so the
/// stand-in declares the 1.0 every template of the catalogue is published at.
macro_rules! template {
    ($name:literal, $type:expr) => {
        PublishedDocument {
            name: $name,
            document_type: $type,
            content: concat!(
                "{\n  \"schemaVersion\": \"1.0\",\n  \"description\": \"",
                $name,
                ", a template Amazon Web Services Systems Manager publishes.\"\n}"
            ),
            body: PublishedBody::FamilyStandIn,
            created_date_millis: CATALOGUE_PUBLISHED_MILLIS,
            target_type: None,
        }
    };
}

/// The documents Systems Manager publishes, which exist in every account and Region
/// without anybody creating them.
///
/// Held in one table so that ONE walk reports the catalogue: the listing
/// ([`super::super::document_listing`]) and every document read project their answers from
/// these entries, so no two reads of a published document can disagree.
///
/// Every name here starts with one of the prefixes [`RESERVED_DOCUMENT_NAME_PREFIXES`]
/// reserves, which is why no `CreateDocument` can collide with one.
pub const PUBLISHED_DOCUMENTS: &[PublishedDocument] = &[
    // The runbooks an AWS Config remediation configuration names as its target, and the
    // command documents Run Command, Distributor, State Manager and the patch and update
    // workflows name: the documents whose parameters the "Systems Manager Automation
    // runbook reference" and the "Systems Manager command document reference" document one
    // by one, so this crate authors a body declaring them. Each one the recorded catalogue
    // pins a date for carries that date.
    authored!(
        "AWS-ConfigureS3BucketVersioning",
        AUTOMATION,
        CONFIGURE_S3_BUCKET_VERSIONING,
        1_545_264_000_000,
        S3_BUCKET
    ),
    authored!(
        "AWS-ConfigureS3BucketPublicAccessBlock",
        AUTOMATION,
        CONFIGURE_S3_BUCKET_PUBLIC_ACCESS_BLOCK,
        1_549_152_000_000,
        S3_BUCKET
    ),
    authored!(
        "AWS-DisableS3BucketPublicReadWrite",
        AUTOMATION,
        DISABLE_S3_BUCKET_PUBLIC_READ_WRITE,
        1_547_510_400_000,
        S3_BUCKET
    ),
    authored!(
        "AWS-EnableS3BucketEncryption",
        AUTOMATION,
        ENABLE_S3_BUCKET_ENCRYPTION,
        1_551_398_400_000,
        S3_BUCKET
    ),
    authored!(
        "AWS-PublishSNSNotification",
        AUTOMATION,
        PUBLISH_SNS_NOTIFICATION,
        1_554_076_800_000,
        None
    ),
    authored!(
        "AWS-RunShellScript",
        COMMAND,
        RUN_SHELL_SCRIPT,
        1_503_695_717_821,
        None
    ),
    authored!(
        "AWS-RunPowerShellScript",
        COMMAND,
        RUN_POWER_SHELL_SCRIPT,
        1_503_695_741_940,
        None
    ),
    authored!(
        "AWS-UpdateSSMAgent",
        COMMAND,
        UPDATE_SSM_AGENT,
        1_622_567_950_979,
        None
    ),
    authored!(
        inventory::GATHER_SOFTWARE_INVENTORY_DOCUMENT,
        DOCUMENT_TYPE_POLICY,
        inventory::GATHER_SOFTWARE_INVENTORY,
        1_564_011_832_472,
        None
    ),
    authored!(
        patch_runs::RUN_PATCH_BASELINE_DOCUMENT,
        COMMAND,
        patch_runs::RUN_PATCH_BASELINE,
        1_786_631_569_844,
        EC2_INSTANCE
    ),
    authored!(
        "AWS-ConfigureAWSPackage",
        COMMAND,
        published_command_documents::CONFIGURE_AWS_PACKAGE,
        1_706_038_480_989,
        None
    ),
    authored!(
        "AWS-InstallApplication",
        COMMAND,
        published_command_documents::INSTALL_APPLICATION,
        1_503_695_753_117,
        None
    ),
    authored!(
        "AWS-InstallPowerShellModule",
        COMMAND,
        published_command_documents::INSTALL_POWER_SHELL_MODULE,
        1_503_695_761_438,
        None
    ),
    authored!(
        "AWS-InstallWindowsUpdates",
        COMMAND,
        published_command_documents::INSTALL_WINDOWS_UPDATES,
        1_781_017_685_843,
        None
    ),
    authored!(
        "AWS-FindWindowsUpdates",
        COMMAND,
        published_command_documents::FIND_WINDOWS_UPDATES,
        1_503_695_776_925,
        None
    ),
    authored!(
        "AWS-RunRemoteScript",
        COMMAND,
        published_command_documents::RUN_REMOTE_SCRIPT,
        1_508_978_913_261,
        None
    ),
    authored!(
        "AWS-RunDocument",
        COMMAND,
        published_command_documents::RUN_DOCUMENT,
        1_503_695_792_309,
        None
    ),
    authored!(
        "AWS-RefreshAssociation",
        COMMAND,
        published_command_documents::REFRESH_ASSOCIATION,
        1_503_695_800_174,
        None
    ),
    authored!(
        "AWS-JoinDirectoryServiceDomain",
        COMMAND,
        published_command_documents::JOIN_DIRECTORY_SERVICE_DOMAIN,
        1_503_695_808_461,
        None
    ),
    authored!(
        "AWS-ApplyAnsiblePlaybooks",
        COMMAND,
        published_command_documents::APPLY_ANSIBLE_PLAYBOOKS,
        1_758_834_443_390,
        None
    ),
    authored!(
        "AWS-ConfigureCloudWatch",
        COMMAND,
        published_command_documents::CONFIGURE_CLOUD_WATCH,
        1_525_720_957_739,
        None
    ),
    authored!(
        "AWS-RunInspecChecks",
        COMMAND,
        published_command_documents::RUN_INSPEC_CHECKS,
        1_758_834_488_387,
        None
    ),
    // The session documents Session Manager starts a session with: the ones the recorded
    // catalogue pins a date for carry that date.
    authored!(
        "AWS-StartSSHSession",
        SESSION,
        published_command_documents::START_SSH_SESSION,
        1_562_087_817_013,
        None
    ),
    authored!(
        "AWS-StartPortForwardingSession",
        SESSION,
        published_command_documents::START_PORT_FORWARDING_SESSION,
        1_563_487_178_453,
        None
    ),
    authored!(
        "AWS-StartPortForwardingSessionToRemoteHost",
        SESSION,
        published_command_documents::START_PORT_FORWARDING_SESSION_TO_REMOTE_HOST,
        1_629_741_532_908,
        None
    ),
    authored!(
        "AWS-StartInteractiveCommand",
        SESSION,
        published_command_documents::START_INTERACTIVE_COMMAND,
        1_641_332_078_651,
        None
    ),
    authored!(
        "AWS-StartNonInteractiveCommand",
        SESSION,
        published_command_documents::START_NON_INTERACTIVE_COMMAND,
        1_641_332_086_774,
        None
    ),
    // The Automation runbooks that act on an Amazon EC2 instance or its volumes.
    authored!(
        "AWS-StartEC2Instance",
        AUTOMATION,
        published_automation_runbooks::START_EC2_INSTANCE,
        1_782_421_644_712,
        EC2_INSTANCE
    ),
    authored!(
        "AWS-StopEC2Instance",
        AUTOMATION,
        published_automation_runbooks::STOP_EC2_INSTANCE,
        1_531_242_313_476,
        EC2_INSTANCE
    ),
    authored!(
        "AWS-RestartEC2Instance",
        AUTOMATION,
        published_automation_runbooks::RESTART_EC2_INSTANCE,
        1_511_217_229_956,
        EC2_INSTANCE
    ),
    authored!(
        "AWS-TerminateEC2Instance",
        AUTOMATION,
        published_automation_runbooks::TERMINATE_EC2_INSTANCE,
        1_514_908_825_638,
        EC2_INSTANCE
    ),
    authored!(
        "AWS-CreateImage",
        AUTOMATION,
        published_automation_runbooks::CREATE_IMAGE,
        1_533_661_478_787,
        EC2_INSTANCE
    ),
    authored!(
        "AWS-CreateSnapshot",
        AUTOMATION,
        published_automation_runbooks::CREATE_SNAPSHOT,
        1_514_908_841_805,
        None
    ),
    authored!(
        "AWS-DeleteSnapshot",
        AUTOMATION,
        published_automation_runbooks::DELETE_SNAPSHOT,
        1_514_908_849_923,
        None
    ),
    authored!(
        "AWS-UpdateLinuxAmi",
        AUTOMATION,
        published_automation_runbooks::UPDATE_LINUX_AMI,
        1_775_747_623_945,
        None
    ),
    authored!(
        "AWS-PatchInstanceWithRollback",
        AUTOMATION,
        published_automation_runbooks::PATCH_INSTANCE_WITH_ROLLBACK,
        1_761_232_679_723,
        EC2_INSTANCE
    ),
    authored!(
        "AWS-ResizeInstance",
        AUTOMATION,
        published_automation_runbooks::RESIZE_INSTANCE,
        1_514_908_874_238,
        EC2_INSTANCE
    ),
    // The rest of the `AWS-*` family: the Automation runbooks of the "Systems Manager
    // Automation runbook reference", the command documents of the command-document
    // reference, the Session Manager session document and the Change Manager change
    // template. The type of each is the one its own reference entry gives it.
    runbook!("AWS-ASGEnterStandby", EC2_INSTANCE),
    runbook!("AWS-ASGExitStandby", EC2_INSTANCE),
    runbook!("AWS-AddOpsItemDedupStringToEventBridgeRule"),
    runbook!("AWS-ArchiveEBSVolumeSnapshots"),
    runbook!("AWS-ArchiveS3BucketToIntelligentTiering", S3_BUCKET),
    runbook!("AWS-AttachEBSVolume"),
    runbook!("AWS-AttachExcludeConditionToS3DenyPolicies"),
    runbook!("AWS-AttachIAMToInstance", EC2_INSTANCE),
    runbook!("AWS-BulkDeleteAssociation"),
    runbook!("AWS-BulkEditOpsItems"),
    runbook!("AWS-BulkResolveOpsItems"),
    runbook!("AWS-ChangeDDBRWCapacityMode"),
    runbook!("AWS-CloseSecurityGroup"),
    runbook!("AWS-ConfigureCloudTrailLogging"),
    runbook!("AWS-ConfigureCloudWatchOnEC2Instance", EC2_INSTANCE),
    runbook!("AWS-ConfigureMaintenanceWindows"),
    runbook!("AWS-ConfigureS3BucketLogging", S3_BUCKET),
    runbook!("AWS-CopySnapshot"),
    runbook!("AWS-CreateDSManagementInstance"),
    runbook!("AWS-CreateDynamoDbBackup"),
    runbook!("AWS-CreateEKSClusterWithFargateProfile"),
    runbook!("AWS-CreateEKSClusterWithNodegroup"),
    runbook!("AWS-CreateEncryptedRdsSnapshot"),
    runbook!("AWS-CreateJiraIssue"),
    runbook!("AWS-CreateManagedLinuxInstance"),
    runbook!("AWS-CreateManagedLinuxInstanceWithApproval"),
    runbook!("AWS-CreateManagedWindowsInstance"),
    runbook!("AWS-CreateManagedWindowsInstanceWithApproval"),
    runbook!("AWS-CreateRdsSnapshot"),
    runbook!("AWS-CreateS3PolicyToExpireMultipartUploads", S3_BUCKET),
    runbook!("AWS-CreateServiceNowIncident"),
    runbook!("AWS-CreateWorkspace"),
    runbook!("AWS-DeleteCloudFormationStack"),
    runbook!("AWS-DeleteCloudFormationStackWithApproval"),
    runbook!("AWS-DeleteDynamoDbBackup"),
    runbook!("AWS-DeleteDynamoDbTableBackups"),
    runbook!("AWS-DeleteEKSCluster"),
    runbook!("AWS-DeleteEbsVolumeSnapshots"),
    runbook!("AWS-DeleteEbsVolumeSnapshots-V2"),
    runbook!("AWS-DeleteIAMInlinePolicy"),
    runbook!("AWS-DeleteImage"),
    runbook!("AWS-DeregisterAMIs"),
    runbook!("AWS-DetachEBSVolume"),
    runbook!("AWS-DiagnoseHybridActivationIssues"),
    runbook!("AWS-DiagnoseUnmanagedEC2NetworkIssues", EC2_INSTANCE),
    runbook!("AWS-DiagnoseUnmanagedEC2OSIssues", EC2_INSTANCE),
    runbook!("AWS-DiagnoseUnmanagedEC2OldSSMAgent", EC2_INSTANCE),
    runbook!("AWS-DiagnoseUnmanagedEC2PermissionsIssues", EC2_INSTANCE),
    runbook!("AWS-DiagnoseUnmanagedEC2StatusCheck", EC2_INSTANCE),
    runbook!("AWS-DiagnoseUnmanagedEC2withoutDHMC", EC2_INSTANCE),
    runbook!("AWS-DisableEventBridgeRule"),
    runbook!("AWS-DisableIncomingSSHOnPort22"),
    runbook!("AWS-DisablePublicAccessForSecurityGroup"),
    runbook!("AWS-DisableSageMakerNotebookRootAccess"),
    runbook!("AWS-ECSRunTask"),
    runbook!("AWS-EnableAppSyncGraphQLApiLogging"),
    runbook!("AWS-EnableAthenaWorkGroupEncryptionAtRest"),
    runbook!("AWS-EnableCLBAccessLogs"),
    runbook!("AWS-EnableCLBConnectionDraining"),
    runbook!("AWS-EnableCWAlarm"),
    runbook!("AWS-EnableCloudFormationStackSNSNotification"),
    runbook!("AWS-EnableCloudTrail"),
    runbook!("AWS-EnableCloudTrailCloudWatchLogs"),
    runbook!("AWS-EnableCloudTrailKmsEncryption"),
    runbook!("AWS-EnableCloudTrailLogFileValidation"),
    runbook!("AWS-EnableDocDbClusterBackupRetentionPeriod"),
    runbook!("AWS-EnableDynamoDbAutoscaling"),
    runbook!("AWS-EnableExplorer"),
    runbook!("AWS-EnableKinesisStreamEncryption"),
    runbook!("AWS-EnableNeptuneDbAuditLogsToCloudWatch"),
    runbook!("AWS-EnableNeptuneDbBackupRetentionPeriod"),
    runbook!("AWS-EnableNeptuneDbClusterDeletionProtection"),
    runbook!("AWS-EnableS3BucketEventNotifications", S3_BUCKET),
    runbook!("AWS-EnableS3BucketKeys", S3_BUCKET),
    runbook!("AWS-EnableSQSEncryption"),
    runbook!("AWS-EnableStepFunctionsStateMachineLogging"),
    runbook!("AWS-EnableVPCFlowLogs"),
    runbook!("AWS-ExportNodes"),
    runbook!("AWS-ExportOpsDataToS3"),
    runbook!("AWS-ExportPatchReportToS3"),
    runbook!("AWS-ExtendEbsVolume"),
    runbook!("AWS-HelloWorld"),
    runbook!("AWS-InstallDistributorPackageOnAzure"),
    runbook!("AWS-InstallSSMAgentOnAzure"),
    runbook!("AWS-MigrateIncidentManagerCloudWatchAlarms"),
    runbook!("AWS-MigrateIncidentManagerEventBridgeRules"),
    runbook!("AWS-MigrateSSMDocumentSharingToRAM"),
    runbook!("AWS-MigrateToNewEKSSelfManagedLinuxNodeGroup"),
    runbook!("AWS-ModifyDynamoDBProvisionedCapacity"),
    runbook!("AWS-OrchestrateUnmanagedEC2Actions", EC2_INSTANCE),
    runbook!("AWS-PasswordReset", EC2_INSTANCE),
    runbook!("AWS-PatchAsgInstance", EC2_INSTANCE),
    runbook!("AWS-QuarantineEC2Instance", EC2_INSTANCE),
    runbook!("AWS-QueryCloudTrailLogs"),
    runbook!("AWS-RebootRdsInstance"),
    runbook!("AWS-ReleaseElasticIP"),
    runbook!("AWS-RemediateSSMAgentHTTPSAccess"),
    runbook!("AWS-RemediateSSMAgentVPCAttributes"),
    runbook!("AWS-RemediateSSMAgentVPCEndpoints"),
    runbook!("AWS-RestartEC2InstanceWithApproval", EC2_INSTANCE),
    runbook!("AWS-RestoreDynamoDbTable"),
    runbook!("AWS-RestrictIncomingTraffic"),
    runbook!("AWS-RunCfnLint"),
    runbook!("AWS-RunPacker"),
    runbook!("AWS-SetRequiredTags"),
    runbook!("AWS-SetupInventory"),
    runbook!("AWS-SetupJupyter"),
    runbook!("AWS-SetupManagedInstance", EC2_INSTANCE),
    runbook!("AWS-SetupManagedRoleOnEc2Instance", EC2_INSTANCE),
    runbook!("AWS-StartEC2InstanceWithApproval", EC2_INSTANCE),
    runbook!("AWS-StartRdsInstance"),
    runbook!("AWS-StartStopAuroraCluster"),
    runbook!("AWS-StopEC2InstanceWithApproval", EC2_INSTANCE),
    runbook!("AWS-StopRdsInstance"),
    runbook!("AWS-TerminateEC2InstanceWithApproval", EC2_INSTANCE),
    runbook!("AWS-UpdateALBDesyncMitigationMode"),
    runbook!("AWS-UpdateCLBDesyncMitigationMode"),
    runbook!("AWS-UpdateCloudFormationStack"),
    runbook!("AWS-UpdateCloudFormationStackWithApproval"),
    runbook!("AWS-UpdateEKSCluster"),
    runbook!("AWS-UpdateEKSManagedNodeGroup"),
    runbook!("AWS-UpdateEKSManagedNodegroupVersion"),
    runbook!("AWS-UpdateEKSSelfManagedLinuxNodeGroups"),
    runbook!("AWS-UpdateSQSPermissions"),
    runbook!("AWS-UpdateWindowsAmi"),
    command!("AWS-ApplyChefRecipes", EC2_INSTANCE),
    command!("AWS-ApplyDSCMofs", EC2_INSTANCE),
    command!("AWS-ApplyPatchBaseline", EC2_INSTANCE),
    command!("AWS-ConfigureDocker"),
    command!("AWS-ConfigureKernelLivePatching"),
    command!("AWS-ConfigureWindowsUpdate"),
    command!("AWS-InstallAmazonECSAgent"),
    command!("AWS-InstallMissingWindowsUpdates"),
    command!("AWS-InstallSpecificWindowsUpdates"),
    command!("AWS-InstanceRebootWithHooks", EC2_INSTANCE),
    command!("AWS-JoinDirectoryServiceDomain-V2"),
    command!("AWS-ListWindowsInventory"),
    command!("AWS-RunAnsiblePlaybook"),
    command!("AWS-RunDockerAction"),
    command!("AWS-RunPatchBaselineAssociation", EC2_INSTANCE),
    command!("AWS-RunPatchBaselineWithHooks", EC2_INSTANCE),
    command!("AWS-RunSaltState"),
    command!("AWS-UpdateAmazonECSAgent"),
    session!("AWS-StartPortForwardingSessionToSocket"),
    change_template!("AWS-HelloWorldChangeTemplate"),
    // The AWS Config remediation runbooks (`AWSConfigRemediation-*`): every one is an
    // Automation runbook a Config remediation configuration names as its target, which is
    // what the "AWS Config remediation" entries of the Automation runbook reference publish
    // them as.
    runbook!("AWSConfigRemediation-CancelKeyDeletion"),
    runbook!("AWSConfigRemediation-ConfigureCodeBuildProjectWithKMSCMK"),
    runbook!("AWSConfigRemediation-ConfigureLambdaFunctionXRayTracing"),
    runbook!("AWSConfigRemediation-ConfigureS3BucketPublicAccessBlock", S3_BUCKET),
    runbook!("AWSConfigRemediation-ConfigureS3PublicAccessBlock"),
    runbook!("AWSConfigRemediation-CreateCloudTrailMultiRegionTrail"),
    runbook!("AWSConfigRemediation-CreateGuardDutyDetector"),
    runbook!("AWSConfigRemediation-DeleteAPIGatewayStage"),
    runbook!("AWSConfigRemediation-DeleteAccessKeysFromCodeBuildProject"),
    runbook!("AWSConfigRemediation-DeleteDefaultVPCRoutes"),
    runbook!("AWSConfigRemediation-DeleteDynamoDbTable"),
    runbook!("AWSConfigRemediation-DeleteEgressOnlyInternetGateway"),
    runbook!("AWSConfigRemediation-DeleteElasticsearchDomain"),
    runbook!("AWSConfigRemediation-DeleteIAMRole"),
    runbook!("AWSConfigRemediation-DeleteIAMUser"),
    runbook!("AWSConfigRemediation-DeleteLambdaFunction"),
    runbook!("AWSConfigRemediation-DeleteOpenSearchDomain"),
    runbook!("AWSConfigRemediation-DeleteRDSCluster"),
    runbook!("AWSConfigRemediation-DeleteRDSClusterSnapshot"),
    runbook!("AWSConfigRemediation-DeleteRDSInstance"),
    runbook!("AWSConfigRemediation-DeleteRDSInstanceSnapshot"),
    runbook!("AWSConfigRemediation-DeleteRedshiftCluster"),
    runbook!("AWSConfigRemediation-DeleteSecret"),
    runbook!("AWSConfigRemediation-DeleteUnusedEBSVolume"),
    runbook!("AWSConfigRemediation-DeleteUnusedENI"),
    runbook!("AWSConfigRemediation-DeleteUnusedIAMGroup"),
    runbook!("AWSConfigRemediation-DeleteUnusedIAMPolicy"),
    runbook!("AWSConfigRemediation-DeleteUnusedSecurityGroup"),
    runbook!("AWSConfigRemediation-DeleteUnusedVPCNetworkACL"),
    runbook!("AWSConfigRemediation-DeleteVPCFlowLog"),
    runbook!("AWSConfigRemediation-DetachAndDeleteInternetGateway"),
    runbook!("AWSConfigRemediation-DetachAndDeleteVirtualPrivateGateway"),
    runbook!("AWSConfigRemediation-DetachIAMPolicy"),
    runbook!("AWSConfigRemediation-DisablePublicAccessToRDSInstance"),
    runbook!("AWSConfigRemediation-DisablePublicAccessToRedshiftCluster"),
    runbook!("AWSConfigRemediation-DisableSubnetAutoAssignPublicIP"),
    runbook!("AWSConfigRemediation-DropInvalidHeadersForALB"),
    runbook!("AWSConfigRemediation-EnableAPIGatewayTracing"),
    runbook!("AWSConfigRemediation-EnableAccountAccessAnalyzer"),
    runbook!("AWSConfigRemediation-EnableAutoScalingGroupELBHealthCheck"),
    runbook!("AWSConfigRemediation-EnableBeanstalkEnvironmentNotifications"),
    runbook!("AWSConfigRemediation-EnableCLBCrossZoneLoadBalancing"),
    runbook!("AWSConfigRemediation-EnableCWLoggingForSessionManager"),
    runbook!("AWSConfigRemediation-EnableCloudFrontAccessLogs"),
    runbook!("AWSConfigRemediation-EnableCloudFrontDefaultRootObject"),
    runbook!("AWSConfigRemediation-EnableCloudFrontOriginAccessIdentity"),
    runbook!("AWSConfigRemediation-EnableCloudFrontOriginFailover"),
    runbook!("AWSConfigRemediation-EnableCloudFrontViewerPolicyHTTPS"),
    runbook!("AWSConfigRemediation-EnableCloudTrailEncryptionWithKMS"),
    runbook!("AWSConfigRemediation-EnableCloudTrailLogFileValidation"),
    runbook!("AWSConfigRemediation-EnableCopyTagsToSnapshotOnRDSCluster"),
    runbook!("AWSConfigRemediation-EnableCopyTagsToSnapshotOnRDSDBInstance"),
    runbook!("AWSConfigRemediation-EnableELBDeletionProtection"),
    runbook!("AWSConfigRemediation-EnableEbsEncryptionByDefault"),
    runbook!("AWSConfigRemediation-EnableElasticBeanstalkEnvironmentLogStreaming"),
    runbook!("AWSConfigRemediation-EnableEncryptionOnDynamoDbTable"),
    runbook!("AWSConfigRemediation-EnableEnhancedMonitoringOnRDSInstance"),
    runbook!("AWSConfigRemediation-EnableKeyRotation"),
    runbook!("AWSConfigRemediation-EnableLoggingForALBAndCLB"),
    runbook!("AWSConfigRemediation-EnableMinorVersionUpgradeOnRDSDBInstance"),
    runbook!("AWSConfigRemediation-EnableMultiAZOnRDSInstance"),
    runbook!("AWSConfigRemediation-EnableNLBCrossZoneLoadBalancing"),
    runbook!("AWSConfigRemediation-EnablePITRForDynamoDbTable"),
    runbook!("AWSConfigRemediation-EnablePerformanceInsightsOnRDSInstance"),
    runbook!("AWSConfigRemediation-EnableRDSClusterDeletionProtection"),
    runbook!("AWSConfigRemediation-EnableRDSInstanceBackup"),
    runbook!("AWSConfigRemediation-EnableRDSInstanceDeletionProtection"),
    runbook!("AWSConfigRemediation-EnableRedshiftClusterAuditLogging"),
    runbook!("AWSConfigRemediation-EnableRedshiftClusterAutomatedSnapshot"),
    runbook!("AWSConfigRemediation-EnableRedshiftClusterEncryption"),
    runbook!("AWSConfigRemediation-EnableRedshiftClusterEnhancedVPCRouting"),
    runbook!("AWSConfigRemediation-EnableSecurityHub"),
    runbook!("AWSConfigRemediation-EnableSystemsManagerSessionManagerAuditLogsToS3"),
    runbook!("AWSConfigRemediation-EnableVPCFlowLogsToCloudWatch"),
    runbook!("AWSConfigRemediation-EnableVPCFlowLogsToS3Bucket"),
    runbook!("AWSConfigRemediation-EnableWAFClassicRegionalLogging"),
    runbook!("AWSConfigRemediation-EnableWAFV2Logging"),
    runbook!("AWSConfigRemediation-EncryptLambdaEnvironmentVariablesWithCMK"),
    runbook!("AWSConfigRemediation-EncryptSNSTopic"),
    runbook!("AWSConfigRemediation-EnforceEC2InstanceIMDSv2", EC2_INSTANCE),
    runbook!("AWSConfigRemediation-EnforceHTTPSOnOpenSearchDomain"),
    runbook!("AWSConfigRemediation-EnforceHttpsOnEsDomain"),
    runbook!("AWSConfigRemediation-EnforceSSLOnlyConnectionsToRedshiftCluster"),
    runbook!("AWSConfigRemediation-ModifyEBSVolumeType"),
    runbook!("AWSConfigRemediation-ModifyRDSInstancePortNumber"),
    runbook!("AWSConfigRemediation-ModifyRedshiftClusterMaintenanceSettings"),
    runbook!("AWSConfigRemediation-ModifyRedshiftClusterNodeType"),
    runbook!("AWSConfigRemediation-MoveLambdaToVPC"),
    runbook!("AWSConfigRemediation-RemovePrincipalStarFromS3BucketPolicy", S3_BUCKET),
    runbook!("AWSConfigRemediation-RemoveUnrestrictedSourceIngressRules"),
    runbook!("AWSConfigRemediation-RemoveUserPolicies"),
    runbook!("AWSConfigRemediation-RemoveVPCDefaultSecurityGroupRules"),
    runbook!("AWSConfigRemediation-ReplaceIAMInlinePolicy"),
    runbook!("AWSConfigRemediation-RestrictBucketSSLRequestsOnly", S3_BUCKET),
    runbook!("AWSConfigRemediation-RevokeUnusedIAMUserCredentials"),
    runbook!("AWSConfigRemediation-RotateSecret"),
    runbook!("AWSConfigRemediation-SetIAMPasswordPolicy"),
    runbook!("AWSConfigRemediation-UpdateAPIGatewayMethodCaching"),
    runbook!("AWSConfigRemediation-UpdateElasticsearchDomainSecurityGroups"),
    runbook!("AWSConfigRemediation-UpdateOpenSearchDomainSecurityGroups"),
    runbook!("AWSConfigRemediation-UpdateXRayKMSKey"),
    // The AWS Config conformance pack templates (`AWSConformancePacks-*`): a conformance
    // pack is deployed from an SSM document, which is what the modelled
    // `ConformancePackTemplate` document type is for.
    template!(
        "AWSConformancePacks-AIMLSecurityAndGovernanceSupportingInfrastructureBestPractices",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-AmazonBedrockSecurityAndGovernanceBestPractices",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-AmazonSageMakerAISecurityAndGovernanceBestPractices",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-CyberResilienceBestPracticesForS3EBSDynamoDB",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-OperationalBestPracticesForNIST800181",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-OperationalBestPracticesforABSCCIGv2Material",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-OperationalBestPracticesforABSCCIGv2Standard",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-OperationalBestPracticesforACSCEssential8",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-OperationalBestPracticesforACSCISM",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-OperationalBestPracticesforACSCISMPart1",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-OperationalBestPracticesforACSCISMPart2",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-OperationalBestPracticesforAIandML",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-OperationalBestPracticesforAPIGateway",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-OperationalBestPracticesforAPRACPG234",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-OperationalBestPracticesforAWSBackup",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-OperationalBestPracticesforAWSControlTowerDetectiveGuardrails",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-OperationalBestPracticesforAWSIdentityAndAccessManagement",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-OperationalBestPracticesforAWSWellArchitectedFrameworkReliabilityPillar",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-OperationalBestPracticesforAWSWellArchitectedFrameworkSecurityPillar",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-OperationalBestPracticesforAmazonDynamoDB",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-OperationalBestPracticesforAmazonDynamoDBwithRemediation",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-OperationalBestPracticesforAmazonS3",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-OperationalBestPracticesforAmazonS3withRemediation",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-OperationalBestPracticesforAssetManagement",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-OperationalBestPracticesforBCPandDR",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-OperationalBestPracticesforBNMRMiT",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-OperationalBestPracticesforCCCSMedium",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-OperationalBestPracticesforCCNENSHigh",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-OperationalBestPracticesforCCNENSLow",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-OperationalBestPracticesforCCNENSMedium",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-OperationalBestPracticesforCIS",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-OperationalBestPracticesforCISACyberEssentials",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-OperationalBestPracticesforCISAWSFBv1.3Level1",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-OperationalBestPracticesforCISAWSFBv1.3Level2",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-OperationalBestPracticesforCISAWSFoundationsBenchmarkv1.4Level1",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-OperationalBestPracticesforCISAWSFoundationsBenchmarkv1.4Level2",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-OperationalBestPracticesforCISCriticalSecurityControlsv8IG1",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-OperationalBestPracticesforCISCriticalSecurityControlsv8IG2",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-OperationalBestPracticesforCISCriticalSecurityControlsv8IG3",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-OperationalBestPracticesforCISTop20",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-OperationalBestPracticesforCMMCLevel1",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-OperationalBestPracticesforCMMCLevel2",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-OperationalBestPracticesforCMMCLevel3",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-OperationalBestPracticesforCMMCLevel4",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-OperationalBestPracticesforCMMCLevel5",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-OperationalBestPracticesforComputeServices",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-OperationalBestPracticesforDataResiliency",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-OperationalBestPracticesforDatabasesServices",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-OperationalBestPracticesforDatalakesandAnalyticsServices",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-OperationalBestPracticesforDevOps",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-OperationalBestPracticesforEC2",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-OperationalBestPracticesforENISACybersecurityGuide",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-OperationalBestPracticesforEncryptionandKeyManagement",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-OperationalBestPracticesforFDA21CFRPart11",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-OperationalBestPracticesforFFIEC",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-OperationalBestPracticesforFedRAMPHighPart1",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-OperationalBestPracticesforFedRAMPHighPart2",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-OperationalBestPracticesforFedRAMPLow",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-OperationalBestPracticesforFedRAMPModerate",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-OperationalBestPracticesforGrammLeachBlileyAct",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-OperationalBestPracticesforGxPEUAnnex11",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-OperationalBestPracticesforHIPAASecurity",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-OperationalBestPracticesforKISMS",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-OperationalBestPracticesforLoadBalancing",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-OperationalBestPracticesforLogging",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-OperationalBestPracticesforMASNotice655",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-OperationalBestPracticesforMASTRMG",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-OperationalBestPracticesforManagementGovernanceServices",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-OperationalBestPracticesforMonitoring",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-OperationalBestPracticesforNBCTRMG",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-OperationalBestPracticesforNCSCCAF",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-OperationalBestPracticesforNCSCCloudSecPrinciples",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-OperationalBestPracticesforNERCCIPBCSI",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-OperationalBestPracticesforNIST180025",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-OperationalBestPracticesforNIST800171",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-OperationalBestPracticesforNIST800172",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-OperationalBestPracticesforNIST80053rev4",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-OperationalBestPracticesforNIST80053rev5",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-OperationalBestPracticesforNISTCSF",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-OperationalBestPracticesforNISTPrivacyFramework",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-OperationalBestPracticesforNYDFS23NYCRR500",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-OperationalBestPracticesforNZISM",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-OperationalBestPracticesforNZISM-Extension",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-OperationalBestPracticesforNZISM-Foundation",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-OperationalBestPracticesforNZISM-NZTransition",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-OperationalBestPracticesforNetworkingandContentDeliveryServices",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-OperationalBestPracticesforPCIDSS",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-OperationalBestPracticesforPCIDSSv4ExcludingGlobalResourceTypes",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-OperationalBestPracticesforPCIDSSv4IncludingGlobalResourceTypes",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-OperationalBestPracticesforPubliclyAccessibleResources",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-OperationalBestPracticesforRBIBasicCyberSecurityFramework",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-OperationalBestPracticesforRBIMasterDirection",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-OperationalBestPracticesforSecurityServices",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-OperationalBestPracticesforServerless",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-OperationalBestPracticesforStorageServices",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-SecurityAndGovernanceBestPracticesforAIMLSupportingInfrastructure",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-SecurityAndGovernanceBestPracticesforAmazonBedrock",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-SecurityAndGovernanceBestPracticesforAmazonSageMakerAI",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-SecurityAndGovernanceBestPracticesforSelfHostedAIML",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-SecurityBestPracticesforAWSWAF",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-SecurityBestPracticesforAmazonOpenSearchService",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-SecurityBestPracticesforAutoScaling",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-SecurityBestPracticesforCloudFront",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-SecurityBestPracticesforCloudTrail",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-SecurityBestPracticesforCodeBuild",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-SecurityBestPracticesforECS",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-SecurityBestPracticesforEFS",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-SecurityBestPracticesforEKS",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-SecurityBestPracticesforLambda",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-SecurityBestPracticesforNetworkFirewall",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-SecurityBestPracticesforRDS",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-SecurityBestPracticesforRedshift",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-SecurityBestPracticesforSecretsManager",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-SelfHostedAIMLSecurityAndGovernanceBestPractices",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-ThreatInformedSecurityBestPracticesFundamental",
        CONFORMANCE_PACK_TEMPLATE
    ),
    template!(
        "AWSConformancePacks-customconformancepack",
        CONFORMANCE_PACK_TEMPLATE
    ),
    // The Distributor packages Systems Manager publishes (`DocumentType` `Package`): the
    // agents and drivers `AWS-ConfigureAWSPackage` installs on a managed node.
    package!("AWSCodeDeployAgent"),
    package!("AWSCodeDeployAgentV2"),
    package!("AWSDistroOTel-Collector"),
    package!("AWSEC2Launch-Agent"),
    package!("AWSKinesisTap"),
    package!("AWSNVMe"),
    package!("AWSNitroEnclavesWindows"),
    package!("AWSPVDriver"),
    package!("AmazonCloudWatchAgent"),
    package!("AmazonCloudWatchNetworkFlowMonitorAgent"),
    package!("AmazonEFSUtils"),
    package!("AmazonGuardDuty-RuntimeMonitoringSsmPlugin"),
    package!("AmazonInspector2-InspectorSsmPlugin"),
    package!("AmazonInspector2-InspectorSsmPluginLinux"),
    package!("AmazonInspector2-InspectorVmScanner"),
    package!("AwsEnaNetworkDriver"),
    package!("AwsVssComponents"),
    // The session documents the container services open a session with.
    session!("AmazonECS-ExecuteInteractiveCommand"),
    session!("AmazonEKS-ControlPlaneInstanceProxy"),
    session!("AmazonEKS-ExecuteNonInteractiveCommand"),
    // The command documents the agents are managed and invoked through on a node.
    command!("AmazonCloudWatch-ManageAgent", EC2_INSTANCE),
    command!("AmazonCloudWatch-MigrateCloudWatchAgent", EC2_INSTANCE),
    command!("AmazonCloudWatch-NetworkFlowMonitorManageAgent", EC2_INSTANCE),
    command!("AmazonGuardDuty-ConfigureRuntimeMonitoringSsmPlugin", EC2_INSTANCE),
    command!("AmazonInspector-ManageAWSAgent", EC2_INSTANCE),
    command!("AmazonInspector2-ConfigureInspectorSsmPlugin", EC2_INSTANCE),
    command!("AmazonInspector2-ConfigureInspectorSsmPluginLinux", EC2_INSTANCE),
    command!("AmazonInspector2-InvokeInspectorSsmPlugin", EC2_INSTANCE),
    command!("AmazonInspector2-InvokeInspectorSsmPluginCIS", EC2_INSTANCE),
    command!("AmazonInspector2-InvokeInspectorSsmPluginLinux", EC2_INSTANCE),
    runbook!("AmazonInspector2-AttachTelemetryPolicy"),
    // The Fleet Manager node documents (`AWSFleetManager-*`): each one is a command
    // document the Fleet Manager console runs on the node it is browsing.
    command!("AWSFleetManager-AddUsersToGroups", EC2_INSTANCE),
    command!("AWSFleetManager-CopyFileSystemItem", EC2_INSTANCE),
    command!("AWSFleetManager-CreateDirectory", EC2_INSTANCE),
    command!("AWSFleetManager-CreateGroup", EC2_INSTANCE),
    command!("AWSFleetManager-CreateUser", EC2_INSTANCE),
    command!("AWSFleetManager-CreateUserInteractive", EC2_INSTANCE),
    command!("AWSFleetManager-CreateWindowsRegistryKey", EC2_INSTANCE),
    command!("AWSFleetManager-DeleteFileSystemItem", EC2_INSTANCE),
    command!("AWSFleetManager-DeleteGroup", EC2_INSTANCE),
    command!("AWSFleetManager-DeleteUser", EC2_INSTANCE),
    command!("AWSFleetManager-DeleteWindowsRegistryKey", EC2_INSTANCE),
    command!("AWSFleetManager-DeleteWindowsRegistryValue", EC2_INSTANCE),
    command!("AWSFleetManager-GetDiskInformation", EC2_INSTANCE),
    command!("AWSFleetManager-GetFileContent", EC2_INSTANCE),
    command!("AWSFleetManager-GetFileSystemContent", EC2_INSTANCE),
    command!("AWSFleetManager-GetGroups", EC2_INSTANCE),
    command!("AWSFleetManager-GetPerformanceCounters", EC2_INSTANCE),
    command!("AWSFleetManager-GetProcessDetails", EC2_INSTANCE),
    command!("AWSFleetManager-GetUsers", EC2_INSTANCE),
    command!("AWSFleetManager-GetWindowsEvents", EC2_INSTANCE),
    command!("AWSFleetManager-GetWindowsRegistryContent", EC2_INSTANCE),
    command!("AWSFleetManager-MountVolume", EC2_INSTANCE),
    command!("AWSFleetManager-MoveFileSystemItem", EC2_INSTANCE),
    command!("AWSFleetManager-RemoveUsersFromGroups", EC2_INSTANCE),
    command!("AWSFleetManager-RenameFileSystemItem", EC2_INSTANCE),
    command!("AWSFleetManager-SetWindowsRegistryValue", EC2_INSTANCE),
    command!("AWSFleetManager-StartGUIConnection", EC2_INSTANCE),
    command!("AWSFleetManager-StartProcess", EC2_INSTANCE),
    command!("AWSFleetManager-TerminateProcess", EC2_INSTANCE),
    // The AWS Fault Injection Service actions (`AWSFIS-Run-*`): each one is a command
    // document an experiment template runs on its targets.
    command!("AWSFIS-Run-CPU-Stress", EC2_INSTANCE),
    command!("AWSFIS-Run-Disk-Fill", EC2_INSTANCE),
    command!("AWSFIS-Run-IO-Stress", EC2_INSTANCE),
    command!("AWSFIS-Run-Kill-Process", EC2_INSTANCE),
    command!("AWSFIS-Run-Memory-Stress", EC2_INSTANCE),
    command!("AWSFIS-Run-Network-Blackhole-Port", EC2_INSTANCE),
    command!("AWSFIS-Run-Network-Blackhole-Port-ECS"),
    command!("AWSFIS-Run-Network-Latency", EC2_INSTANCE),
    command!("AWSFIS-Run-Network-Latency-ECS"),
    command!("AWSFIS-Run-Network-Latency-Sources", EC2_INSTANCE),
    command!("AWSFIS-Run-Network-Packet-Loss", EC2_INSTANCE),
    command!("AWSFIS-Run-Network-Packet-Loss-ECS"),
    command!("AWSFIS-Run-Network-Packet-Loss-Sources", EC2_INSTANCE),
    // The command documents the observability exporters and the agents of the other
    // services are installed and configured with on a node.
    command!("AWSEC2-ApplicationInsightsCloudwatchAgentInstallAndConfigure", EC2_INSTANCE),
    command!("AWSEC2-CheckPerformanceCounterSets", EC2_INSTANCE),
    command!("AWSEC2-ConfigureSTIG", EC2_INSTANCE),
    command!("AWSEC2-DetectSqlHaState", EC2_INSTANCE),
    command!("AWSEC2-DetectWorkload", EC2_INSTANCE),
    command!("AWSEC2-RunSysprep", EC2_INSTANCE),
    command!("AWSEC2-UpdateLaunchAgent", EC2_INSTANCE),
    command!("AWSObservabilityExporter-HAClusterExporterInstallAndConfigure", EC2_INSTANCE),
    command!("AWSObservabilityExporter-JMXExporterInstallAndConfigure", EC2_INSTANCE),
    command!("AWSObservabilityExporter-SAP-HANADBExporterInstallAndConfigure", EC2_INSTANCE),
    command!("AWSObservabilityExporter-SAP-SAPHostExporterInstallAndConfigure", EC2_INSTANCE),
    command!("AWSObservabilityExporter-SQLExporterInstallAndConfigure", EC2_INSTANCE),
    command!("AWSLicenseManager-DiscoverLicenseAssets", EC2_INSTANCE),
    command!("AWSSAP-Backint", EC2_INSTANCE),
    command!("AWSSAP-InstallBackint", EC2_INSTANCE),
    command!("AWSSAP-InstallBackintForAWSBackup", EC2_INSTANCE),
    command!("AWSSAPTools-DataProvider", EC2_INSTANCE),
    command!("AWSSQLServer-Backup", EC2_INSTANCE),
    command!("AWSSQLServer-DBCC", EC2_INSTANCE),
    command!("AWSSQLServer-Index", EC2_INSTANCE),
    command!("AWSSQLServer-Restore", EC2_INSTANCE),
    // The Automation runbooks of the EC2, migration, disaster-recovery, incident and
    // tutorial families.
    runbook!("AWSApp2Container-ReplatformApplications"),
    runbook!("AWSDRS-InstallDRAgentOnInstance", EC2_INSTANCE),
    runbook!("AWSDRS-LinuxTimeSyncSetting", EC2_INSTANCE),
    runbook!("AWSDRS-ValidateDiskSpace", EC2_INSTANCE),
    runbook!("AWSDRS-ValidateHttpResponse", EC2_INSTANCE),
    runbook!("AWSDRS-ValidateNetworkConnectivity", EC2_INSTANCE),
    runbook!("AWSDRS-VerifyMountedVolumes", EC2_INSTANCE),
    runbook!("AWSDRS-VerifyProcessIsRunning", EC2_INSTANCE),
    runbook!("AWSDRS-VerifyTags", EC2_INSTANCE),
    runbook!("AWSDisasterRecovery-InstallDRAgentOnInstance", EC2_INSTANCE),
    runbook!("AWSDocs-ClassicLoadBalancerSSMDocument"),
    runbook!("AWSDocs-Configure-SSL-TLS-AL"),
    runbook!("AWSDocs-Configure-SSL-TLS-AL2"),
    runbook!("AWSDocs-HostingAWordPressBlog-AL"),
    runbook!("AWSDocs-HostingAWordPressBlog-AL2"),
    runbook!("AWSDocs-InstallALAMPServer-AL"),
    runbook!("AWSDocs-InstallALAMPServer-AL2"),
    runbook!("AWSDocs-LambdaWithS3SSMDocument"),
    runbook!("AWSDocs-S3StaticWebsite"),
    runbook!("AWSDocs-S3StaticWebsiteCustomDomain"),
    runbook!("AWSDocs-ScaleLoadBalanced"),
    runbook!("AWSEC2-CloneInstanceAndUpgradeSQLServer", EC2_INSTANCE),
    runbook!("AWSEC2-CloneInstanceAndUpgradeWindows", EC2_INSTANCE),
    runbook!("AWSEC2-CloneInstanceAndUpgradeWindows2019", EC2_INSTANCE),
    runbook!("AWSEC2-CreateVssSnapshot", EC2_INSTANCE),
    runbook!("AWSEC2-PatchLoadBalancerInstance", EC2_INSTANCE),
    runbook!("AWSEC2-PrepareVssRestore", EC2_INSTANCE),
    runbook!("AWSEC2-RestoreSqlServerDatabaseWithVss", EC2_INSTANCE),
    runbook!("AWSEC2-RunVssRestoreForSqlDatabase", EC2_INSTANCE),
    runbook!("AWSEC2-SQLServerDBRestore", EC2_INSTANCE),
    runbook!("AWSEC2-VssInstallAndSnapshot", EC2_INSTANCE),
    runbook!("AWSIncidents-CriticalIncidentRunbookTemplate"),
    runbook!("AWSIncidents-PostIncidentAnalysisTemplate"),
    runbook!("AWSMigration-ConvertCentOsToRockyLinuxDistribution", EC2_INSTANCE),
    runbook!("AWSMigration-CreateLoadBalanceAutoScaleGroup"),
    runbook!("AWSMigration-EnableInspector"),
    runbook!("AWSMigration-LinuxTimeSyncSetting", EC2_INSTANCE),
    runbook!("AWSMigration-ReplaceSuseSubscriptionWithAwsSubscription", EC2_INSTANCE),
    runbook!("AWSMigration-RunSourceServerAction"),
    runbook!("AWSMigration-ValidateDiskSpace", EC2_INSTANCE),
    runbook!("AWSMigration-ValidateHttpResponse", EC2_INSTANCE),
    runbook!("AWSMigration-ValidateNetworkConnectivity", EC2_INSTANCE),
    runbook!("AWSMigration-VerifyMountedVolumes", EC2_INSTANCE),
    runbook!("AWSMigration-VerifyProcessIsRunning", EC2_INSTANCE),
    runbook!("AWSMigration-VerifySqlAWSSubscription", EC2_INSTANCE),
    runbook!("AWSMigration-VerifyTags", EC2_INSTANCE),
    runbook!("AWSQuickStarts-AWS-Managed-Microsoft-AD"),
    runbook!("AWSQuickStarts-AWS-Managed-Microsoft-AD-Existing-VPC"),
    runbook!("AWSQuickStarts-AWS-VPC"),
    runbook!("AWSQuickStarts-AmazonEKS"),
    runbook!("AWSQuickStarts-AmazonEKS-Existing-VPC"),
    runbook!("AWSQuickStarts-Microsoft-AD-Extend-OnPrem"),
    runbook!("AWSQuickStarts-Microsoft-AD-Extend-OnPrem-Existing-VPC"),
    runbook!("AWSQuickStarts-Microsoft-AD-SelfManaged"),
    runbook!("AWSQuickStarts-Microsoft-AD-SelfManaged-Existing-VPC"),
    runbook!("AWSQuickStarts-Microsoft-Exchange"),
    runbook!("AWSQuickStarts-Microsoft-IIS"),
    runbook!("AWSQuickStarts-Microsoft-IIS-Existing-VPC"),
    runbook!("AWSQuickStarts-Microsoft-RdGateway"),
    runbook!("AWSQuickStarts-Microsoft-RdGateway-Domain-Joined"),
    runbook!("AWSQuickStarts-Microsoft-RdGateway-Existing-VPC"),
    runbook!("AWSRefactorSpaces-CreateResources"),
    runbook!("AWSSSO-CreateSSOUser"),
    // The Quick Setup configuration definitions (`AWSQuickSetupType-*`): a configuration
    // type is defined by a `QuickSetup` document, and the `-CFN-*` entries beside it hold
    // the CloudFormation template that type deploys.
    template!("AWSQuickSetupType-CFGCPacks", QUICK_SETUP),
    template!("AWSQuickSetupType-CFGCPacks-CFN-DA", CLOUD_FORMATION),
    template!("AWSQuickSetupType-CFGRecording", QUICK_SETUP),
    template!("AWSQuickSetupType-CFGRecording-CFN-TA", CLOUD_FORMATION),
    template!("AWSQuickSetupType-CWASetup", QUICK_SETUP),
    template!("AWSQuickSetupType-CWASetup-CFN-TA", CLOUD_FORMATION),
    template!("AWSQuickSetupType-ConfigureDevOpsGuru", QUICK_SETUP),
    template!("AWSQuickSetupType-DHMC", QUICK_SETUP),
    template!("AWSQuickSetupType-DHMC-CFN-TA", CLOUD_FORMATION),
    template!("AWSQuickSetupType-DeployConformancePack", QUICK_SETUP),
    template!("AWSQuickSetupType-DevOpsGuru", QUICK_SETUP),
    template!("AWSQuickSetupType-DevOpsGuru-CFN-TA", CLOUD_FORMATION),
    template!("AWSQuickSetupType-Distributor", QUICK_SETUP),
    template!("AWSQuickSetupType-Distributor-CFN-TA", CLOUD_FORMATION),
    template!("AWSQuickSetupType-EnableAREX", QUICK_SETUP),
    template!("AWSQuickSetupType-EnableConfigRecording", QUICK_SETUP),
    template!("AWSQuickSetupType-EnableDHMC", QUICK_SETUP),
    template!("AWSQuickSetupType-InstallAndManageCloudWatchAgent", QUICK_SETUP),
    template!("AWSQuickSetupType-JITNA", QUICK_SETUP),
    template!("AWSQuickSetupType-JITNA-CFN-DA", CLOUD_FORMATION),
    template!("AWSQuickSetupType-JITNA-CFN-TA", CLOUD_FORMATION),
    template!("AWSQuickSetupType-ManageInstanceProfile", QUICK_SETUP),
    template!("AWSQuickSetupType-PatchPolicy", QUICK_SETUP),
    template!("AWSQuickSetupType-PatchPolicy-CFN-MA", CLOUD_FORMATION),
    template!("AWSQuickSetupType-PatchPolicy-CFN-TA", CLOUD_FORMATION),
    template!("AWSQuickSetupType-PropagateJustInTimeNodeAccessPolicies", QUICK_SETUP),
    template!("AWSQuickSetupType-ResourceExplorer", QUICK_SETUP),
    template!("AWSQuickSetupType-ResourceExplorer-CFN-TA", CLOUD_FORMATION),
    template!("AWSQuickSetupType-SSM", QUICK_SETUP),
    template!("AWSQuickSetupType-SSM-CFN-DA", CLOUD_FORMATION),
    template!("AWSQuickSetupType-SSM-CFN-TA", CLOUD_FORMATION),
    template!("AWSQuickSetupType-SSM-ManageResources", QUICK_SETUP),
    template!("AWSQuickSetupType-SSMChangeMgr", QUICK_SETUP),
    template!("AWSQuickSetupType-SSMChangeMgr-CFN-DA", CLOUD_FORMATION),
    template!("AWSQuickSetupType-SSMChangeMgr-CFN-TA", CLOUD_FORMATION),
    template!("AWSQuickSetupType-SSMHostMgmt", QUICK_SETUP),
    template!("AWSQuickSetupType-SSMHostMgmt-CFN-TA", CLOUD_FORMATION),
    template!("AWSQuickSetupType-SSMOpsCenter", QUICK_SETUP),
    template!("AWSQuickSetupType-SSMOpsCenter-CFN-DA", CLOUD_FORMATION),
    template!("AWSQuickSetupType-SSMOpsCenter-CFN-TA", CLOUD_FORMATION),
    template!("AWSQuickSetupType-Scheduler", QUICK_SETUP),
    template!("AWSQuickSetupType-Scheduler-ApplyInstanceState", QUICK_SETUP),
    template!("AWSQuickSetupType-Scheduler-CFN-TA", CLOUD_FORMATION),
    template!("AWSQuickSetupType-Scheduler-ChangeCalendarState", QUICK_SETUP),
    template!("AWSQuickSetupType-SetupJITNAResources", QUICK_SETUP),
    // The Resilience Hub standard operating procedures and chaos tests
    // (`AWSResilienceHub-*`): each is an Automation runbook an assessment's recommendation
    // names, apart from the two whose names say they are the command documents the tests
    // run on a node.
    runbook!("AWSResilienceHub-BacktrackRdsSOP_2020-04-01"),
    runbook!("AWSResilienceHub-BlockSQSDeleteMessageTest_2021-03-09"),
    runbook!("AWSResilienceHub-BreakEFSSecurityGroupTest_2020-09-21"),
    runbook!("AWSResilienceHub-BreakLambdaSecurityGroupTest_2020-09-21"),
    runbook!("AWSResilienceHub-BreakSNSSubscriptionDeliveryToSQSTest_2020-04-01"),
    runbook!("AWSResilienceHub-BreakSNSSubscriptionRedrivePolicyTest_2020-04-01"),
    runbook!("AWSResilienceHub-BreakSQSQueuePolicyTest_2020-11-27"),
    runbook!("AWSResilienceHub-BreakTargetGroupsHealthCheckPortTest_2020-04-01"),
    runbook!("AWSResilienceHub-ChangeEFSProvisionedThroughputSOP_2020-10-26"),
    runbook!("AWSResilienceHub-ChangeHttpWsApiGwThrottlingSettingsSOP_2020-10-26"),
    runbook!("AWSResilienceHub-ChangeLambdaConcurrencyLimitSOP_2020-10-26"),
    runbook!("AWSResilienceHub-ChangeLambdaExecutionTimeLimitSOP_2020-10-26"),
    runbook!("AWSResilienceHub-ChangeLambdaMemorySizeSOP_2020-10-26"),
    runbook!("AWSResilienceHub-ChangeLambdaProvisionedConcurrencySOP_2020-10-26"),
    runbook!("AWSResilienceHub-ChangeRestApiGwQuotaLimitSOP_2020-10-26"),
    runbook!("AWSResilienceHub-ChangeRestApiGwThrottlingSettingsSOP_2020-10-26"),
    runbook!("AWSResilienceHub-CleanS3BucketUtil_2020-03-03"),
    runbook!("AWSResilienceHub-CopyDynamoDBTablePropertiesUtil_2020-04-01"),
    runbook!("AWSResilienceHub-CreateEFSBackupSOP_2020-10-26"),
    runbook!("AWSResilienceHub-CreateEFSReplicationSOP_2023-01-01"),
    runbook!("AWSResilienceHub-CreateNewDocumentDBInstanceSOP_2020-09-21"),
    runbook!("AWSResilienceHub-DeleteEFSReplicationSOP_2023-01-01"),
    runbook!("AWSResilienceHub-DenyEFSClientMountTest_2022-12-22"),
    runbook!("AWSResilienceHub-DenyLambdaAllTrafficTest_2022-04-12"),
    runbook!("AWSResilienceHub-EnableSNSTopicDLQSOP_2020-11-26"),
    runbook!("AWSResilienceHub-ExceedRestApiGwQuotaTest_2020-09-21"),
    runbook!("AWSResilienceHub-FailoverRdsAuroraClusterTest_2020-04-01"),
    runbook!("AWSResilienceHub-FailoverRdsInstanceTest_2020-04-01"),
    runbook!("AWSResilienceHub-ForceDocumentDBDatabaseToBeInaccessibleTest_2020-09-21"),
    runbook!("AWSResilienceHub-ForceDynamoDBTableReadThrottlingTest_2020-09-21"),
    runbook!("AWSResilienceHub-ForceECSServiceTaskFailureTest_2020-04-01"),
    runbook!("AWSResilienceHub-ForceLambdaThrottlingTest_2020-10-26"),
    runbook!("AWSResilienceHub-ForceNetworkUnavailableTest_2020-04-01"),
    runbook!("AWSResilienceHub-ForceSQSCapacityFailureTest_2021-03-13"),
    runbook!("AWSResilienceHub-ForceSQSFifoQueueMaxReceiveFailureTest_2020-11-27"),
    runbook!("AWSResilienceHub-ForceSQSStandardQueueMaxReceiveFailureTest_2020-11-27"),
    runbook!("AWSResilienceHub-IncreaseVolumeSizeEbsSOP_2020-05-26"),
    runbook!("AWSResilienceHub-InjectCpuLoadInAsgTest_2021-09-22"),
    runbook!("AWSResilienceHub-InjectCpuLoadInEc2Test_2020-07-28", EC2_INSTANCE),
    runbook!("AWSResilienceHub-InjectMemoryLoadInAsgTest_2020-10-11"),
    runbook!("AWSResilienceHub-InjectMemoryLoadInEc2Test_2020-07-28", EC2_INSTANCE),
    runbook!("AWSResilienceHub-KillProcessInAsgTest_2021-10-22"),
    runbook!("AWSResilienceHub-KillProcessInEc2Test_2021-10-22", EC2_INSTANCE),
    command!("AWSResilienceHub-KillStressCommand_2020-07-28", EC2_INSTANCE),
    runbook!("AWSResilienceHub-KillStressOnHealthyInstances_2020-07-28"),
    runbook!("AWSResilienceHub-MisconfigureAsgR53ArcSecondarySiteTest_2020-07-18"),
    runbook!("AWSResilienceHub-MisconfigureDynamoDBR53ArcSecondarySiteTest_2020-09-21"),
    runbook!("AWSResilienceHub-MisconfigureElasticLBR53ArcSecondarySiteTest_2020-04-01"),
    runbook!("AWSResilienceHub-MisconfigureR53ArcRDSAuroraSecondarySiteTest_2020-04-01"),
    runbook!("AWSResilienceHub-MoveSQSMessagesBetweenQueuesSOP_2021-03-11"),
    command!("AWSResilienceHub-NetworkUnavailableCommand_2020-07-23", EC2_INSTANCE),
    runbook!("AWSResilienceHub-PerformDRSDrillSOP_2022-09-18"),
    runbook!("AWSResilienceHub-PromoteDocumentDBReadReplicaSOP_2020-09-21"),
    runbook!("AWSResilienceHub-PromoteRdsReadReplicaSOP_2020-04-01"),
    runbook!("AWSResilienceHub-PurgeSQSQueueSOP_2021-03-11"),
    runbook!("AWSResilienceHub-RebootDocumentDBInstanceTest_2020-09-21"),
    runbook!("AWSResilienceHub-RebootEc2InstanceSOP_2020-05-20", EC2_INSTANCE),
    runbook!("AWSResilienceHub-RebootRdsInstanceTest_2020-04-01"),
    runbook!("AWSResilienceHub-RefreshInstancesInAsgTest_2020-07-23"),
    runbook!("AWSResilienceHub-RestoreAuroraDBClusterFromBackupSOP_2022-10-25"),
    runbook!("AWSResilienceHub-RestoreDocumentDBClusterFromBackupSOP_2020-09-21"),
    runbook!("AWSResilienceHub-RestoreDocumentDBClusterFromPointInTimeSOP_2020-09-21"),
    runbook!("AWSResilienceHub-RestoreDynamoDBTableFromBackupSOP_2020-04-01"),
    runbook!("AWSResilienceHub-RestoreDynamoDBTableToPointInTimeSOP_2020-04-01"),
    runbook!("AWSResilienceHub-RestoreEFSBackupInAnotherRegionSOP_2020-10-26"),
    runbook!("AWSResilienceHub-RestoreFromBackupEbsSOP_2020-05-26"),
    runbook!("AWSResilienceHub-RestoreFromRdsBackupSOP_2020-04-01"),
    runbook!("AWSResilienceHub-RestoreS3BucketFromBackupSOP_2020-09-21", S3_BUCKET),
    runbook!("AWSResilienceHub-RestoreS3ObjectToPreviousVersionSOP_2020-09-21", S3_BUCKET),
    runbook!("AWSResilienceHub-ResubmitSNSTopicDeadLetterQueueSOP_2020-04-01"),
    runbook!("AWSResilienceHub-ScaleDownDocumentDBClusterSOP_2020-09-21"),
    runbook!("AWSResilienceHub-ScaleECSServiceSOP_2020-04-01"),
    runbook!("AWSResilienceHub-ScaleOutAsgSOP_2020-07-01"),
    runbook!("AWSResilienceHub-ScaleUpAsgSOP_2020-07-01"),
    runbook!("AWSResilienceHub-ScaleUpDocumentDBClusterSOP_2020-09-21"),
    runbook!("AWSResilienceHub-ScaleUpEc2SOP_2020-05-20", EC2_INSTANCE),
    runbook!("AWSResilienceHub-SendSNSBrokenMessagesTest_2020-04-01"),
    runbook!("AWSResilienceHub-SimulateAzOutageInAsgTest_2020-07-23"),
    runbook!("AWSResilienceHub-SimulateNatGwInternetUnavailableTest_2020-09-21"),
    runbook!("AWSResilienceHub-SimulateNetworkUnavailableInAsgTest_2020-07-23"),
    runbook!("AWSResilienceHub-SimulateNetworkUnavailableInEc2Test_2020-07-23", EC2_INSTANCE),
    runbook!("AWSResilienceHub-SimulateRestApiGwNetworkUnavailableTest_2020-09-21"),
    runbook!("AWSResilienceHub-SimulateS3ObjectsAccidentalDeleteTest_2020-04-01", S3_BUCKET),
    runbook!("AWSResilienceHub-StartDRSRecoverySOP_2022-09-18"),
    runbook!("AWSResilienceHub-SwitchLambdaVersionInAliasSOP_2020-10-26"),
    runbook!("AWSResilienceHub-TestEFSUnderIOLoadTest_2022-12-22"),
    runbook!("AWSResilienceHub-TriggerHttpWsApiGwThrottlingTest_2020-09-21"),
    runbook!("AWSResilienceHub-TriggerRestApiGwThrottlingTest_2020-09-21"),
    runbook!("AWSResilienceHub-UpdateDynamoDBTableProvisionedCapacitySOP_2020-04-01"),
    runbook!("AWSResilienceHub-UpdateHttpWsApiGwVersionSOP_2020-10-26"),
    runbook!("AWSResilienceHub-UpdateRestApiGwVersionSOP_2020-10-26"),
    // The AWS Support self-service automations (`AWSSupport-*`): each is an Automation
    // runbook the Support troubleshooting guides name, apart from the EC2Rescue tool
    // documents, which Run Command runs on the node.
    command!("AWSSupport-EC2Rescue", EC2_INSTANCE),
    command!("AWSSupport-InstallEC2Rescue", EC2_INSTANCE),
    runbook!("AWSSupport-ActivateWindowsWithAmazonLicense", EC2_INSTANCE),
    runbook!("AWSSupport-AnalyzeAWSEndpointReachabilityFromEC2", EC2_INSTANCE),
    runbook!("AWSSupport-AnalyzeEBSResourceUsage"),
    runbook!("AWSSupport-AnalyzeEMRLogs"),
    runbook!("AWSSupport-AnalyzeSESMessageSendingStatus"),
    runbook!("AWSSupport-AssociatePhoneNumbersToConnectContactFlows"),
    runbook!("AWSSupport-CalculateEBSPerformanceMetrics"),
    runbook!("AWSSupport-CheckAndMountEFS", EC2_INSTANCE),
    runbook!("AWSSupport-CheckXenToNitroMigrationRequirements", EC2_INSTANCE),
    runbook!("AWSSupport-CleanupRDSParameterGroups"),
    runbook!("AWSSupport-CloneXenEC2InstanceAndMigrateToNitro", EC2_INSTANCE),
    runbook!("AWSSupport-CollectAmazonConnectContactFlowLog"),
    runbook!("AWSSupport-CollectECSInstanceLogs", EC2_INSTANCE),
    runbook!("AWSSupport-CollectEKSInstanceLogs", EC2_INSTANCE),
    runbook!("AWSSupport-CollectEKSLinuxNodeStatistics", EC2_INSTANCE),
    runbook!("AWSSupport-CollectElasticBeanstalkLogs"),
    runbook!("AWSSupport-CollectSAPHANALogs", EC2_INSTANCE),
    runbook!("AWSSupport-ConfigureDNSQueryLogging"),
    runbook!("AWSSupport-ConfigureEC2Metadata", EC2_INSTANCE),
    runbook!("AWSSupport-ConfigureS3ReplicationSameAndCrossAccount", S3_BUCKET),
    runbook!("AWSSupport-ConfigureTrafficMirroring"),
    runbook!("AWSSupport-ConnectivityTroubleshooter"),
    runbook!("AWSSupport-ContainEC2Instance", EC2_INSTANCE),
    runbook!("AWSSupport-ContainIAMPrincipal"),
    runbook!("AWSSupport-ContainS3Resource", S3_BUCKET),
    runbook!("AWSSupport-CopyEC2Instance", EC2_INSTANCE),
    runbook!("AWSSupport-DeploySESSendingLogsToCloudWatchLogs"),
    runbook!("AWSSupport-DiagnoseEMRLogsWithAthena"),
    runbook!("AWSSupport-EmptyS3Bucket", S3_BUCKET),
    runbook!("AWSSupport-EnableVPCFlowLogs"),
    runbook!("AWSSupport-EnableWindowsEC2SerialConsole", EC2_INSTANCE),
    runbook!("AWSSupport-ExecuteEC2Rescue", EC2_INSTANCE),
    runbook!("AWSSupport-GrantPermissionsToIAMUser"),
    runbook!("AWSSupport-ListEC2Resources"),
    runbook!("AWSSupport-ManageRDPSettings", EC2_INSTANCE),
    runbook!("AWSSupport-ManageWindowsService", EC2_INSTANCE),
    runbook!("AWSSupport-MigrateXenToNitroLinux", EC2_INSTANCE),
    runbook!("AWSSupport-ModifyEBSSnapshotPermission"),
    runbook!("AWSSupport-ModifyRDSSnapshotPermission"),
    runbook!("AWSSupport-RecoverWorkSpace"),
    runbook!("AWSSupport-RecoverWorkSpaceWithApproval"),
    runbook!("AWSSupport-RemediateLambdaS3Event"),
    runbook!("AWSSupport-RequestSageMakerLimitIncrease"),
    runbook!("AWSSupport-ResetAccess", EC2_INSTANCE),
    runbook!("AWSSupport-ResetLinuxUserPassword", EC2_INSTANCE),
    runbook!("AWSSupport-RestoreEC2InstanceFromSnapshot", EC2_INSTANCE),
    runbook!("AWSSupport-RunEC2RescueForWindowsTool", EC2_INSTANCE),
    runbook!("AWSSupport-SendLogBundleToS3Bucket", EC2_INSTANCE),
    runbook!("AWSSupport-SetupConfig"),
    runbook!("AWSSupport-SetupIPMonitoringFromVPC"),
    runbook!("AWSSupport-SetupK8sApiProxyForEKS"),
    runbook!("AWSSupport-ShareEncryptedAMIOrEBSSnapshot"),
    runbook!("AWSSupport-ShareRDSSnapshot"),
    runbook!("AWSSupport-StartEC2RescueWorkflow", EC2_INSTANCE),
    runbook!("AWSSupport-TroubleshootADConnectorConnectivity"),
    runbook!("AWSSupport-TroubleshootALB5XXErrors"),
    runbook!("AWSSupport-TroubleshootAPIGatewayCustomDomainConfig"),
    runbook!("AWSSupport-TroubleshootAPIGatewayHttpErrors"),
    runbook!("AWSSupport-TroubleshootAPIGatewayLambdaInvocation"),
    runbook!("AWSSupport-TroubleshootAPIGatewayVpcLinkIntegration"),
    runbook!("AWSSupport-TroubleshootAWSBatchJob"),
    runbook!("AWSSupport-TroubleshootAWSECSHealthChecks"),
    runbook!("AWSSupport-TroubleshootActiveDirectoryReplication"),
    runbook!("AWSSupport-TroubleshootCFNCustomResource"),
    runbook!("AWSSupport-TroubleshootCloudWatchAgent", EC2_INSTANCE),
    runbook!("AWSSupport-TroubleshootCloudWatchAlarm"),
    runbook!("AWSSupport-TroubleshootCodeDeploy"),
    runbook!("AWSSupport-TroubleshootConnectivityToRDS"),
    runbook!("AWSSupport-TroubleshootDMSEndpointConnection"),
    runbook!("AWSSupport-TroubleshootDMSTableErrors"),
    runbook!("AWSSupport-TroubleshootDirectoryTrust"),
    runbook!("AWSSupport-TroubleshootEC2ConnectivityToRedshift"),
    runbook!("AWSSupport-TroubleshootEC2InstanceConnect", EC2_INSTANCE),
    runbook!("AWSSupport-TroubleshootECSContainerInstance", EC2_INSTANCE),
    runbook!("AWSSupport-TroubleshootECSTaskFailedToStart"),
    runbook!("AWSSupport-TroubleshootEKSALBControllerIssues"),
    runbook!("AWSSupport-TroubleshootEKSCNI"),
    runbook!("AWSSupport-TroubleshootEKSDNSFailure"),
    runbook!("AWSSupport-TroubleshootEKSNetwork"),
    runbook!("AWSSupport-TroubleshootEKSWorkerNode"),
    runbook!("AWSSupport-TroubleshootELBHealthChecks"),
    runbook!("AWSSupport-TroubleshootEbsCsiDriversForEks"),
    runbook!("AWSSupport-TroubleshootElasticBeanstalk"),
    runbook!("AWSSupport-TroubleshootGlueConnection"),
    runbook!("AWSSupport-TroubleshootIAMAccessDeniedEvents"),
    runbook!("AWSSupport-TroubleshootLambdaInternetAccess"),
    runbook!("AWSSupport-TroubleshootLambdaS3Event"),
    runbook!("AWSSupport-TroubleshootLambdaVpcConnectivity"),
    runbook!("AWSSupport-TroubleshootLinuxMGNDRSAgentLogs", EC2_INSTANCE),
    runbook!("AWSSupport-TroubleshootMSKClusterConnection"),
    runbook!("AWSSupport-TroubleshootMWAAEnvironmentCreation"),
    runbook!("AWSSupport-TroubleshootManagedInstance", EC2_INSTANCE),
    runbook!("AWSSupport-TroubleshootOpenSearchHighCPU"),
    runbook!("AWSSupport-TroubleshootOpenSearchRedYellowCluster"),
    runbook!("AWSSupport-TroubleshootPatchManagerLinux", EC2_INSTANCE),
    runbook!("AWSSupport-TroubleshootR53DNSResolution"),
    runbook!("AWSSupport-TroubleshootRDP", EC2_INSTANCE),
    runbook!("AWSSupport-TroubleshootRDSIAMAuthentication"),
    runbook!("AWSSupport-TroubleshootS3AccessSameAccount", S3_BUCKET),
    runbook!("AWSSupport-TroubleshootS3EventNotifications", S3_BUCKET),
    runbook!("AWSSupport-TroubleshootS3PublicRead", S3_BUCKET),
    runbook!("AWSSupport-TroubleshootSAMLIssues"),
    runbook!("AWSSupport-TroubleshootSSH", EC2_INSTANCE),
    runbook!("AWSSupport-TroubleshootSUSERegistration", EC2_INSTANCE),
    runbook!("AWSSupport-TroubleshootSessionManager", EC2_INSTANCE),
    runbook!("AWSSupport-TroubleshootVPN"),
    runbook!("AWSSupport-TroubleshootWindowsPerformance", EC2_INSTANCE),
    runbook!("AWSSupport-TroubleshootWindowsUpdate", EC2_INSTANCE),
    runbook!("AWSSupport-UpgradeWindowsAWSDrivers", EC2_INSTANCE),
    runbook!("AWSSupport-ValidateFSxWindowsADConfig"),
    runbook!("AWSSupport-ValidateMWAADependencies"),
    runbook!("AWSSupport-ValidateRdsNetworkConfiguration"),
    // The AWS Systems Manager for SAP runbooks (`AWSSAP*` / `AWSSystemsManagerSAP-*`).
    runbook!("AWSSystemsManagerSAP-BackupHanaDatabase"),
    runbook!("AWSSystemsManagerSAP-BackupJobIdRenewal"),
    runbook!("AWSSystemsManagerSAP-CheckComponentStatus"),
    runbook!("AWSSystemsManagerSAP-ConfigurationChecksDiscovery"),
    runbook!("AWSSystemsManagerSAP-CreateDLMSnapshotForSAPHANA"),
    runbook!("AWSSystemsManagerSAP-DeleteHanaBackupFromCatalog"),
    runbook!("AWSSystemsManagerSAP-Discovery"),
    runbook!("AWSSystemsManagerSAP-FinishStartStopApplication"),
    runbook!("AWSSystemsManagerSAP-FinishStartStopComponent"),
    runbook!("AWSSystemsManagerSAP-HanaBackupRestoreStatus"),
    runbook!("AWSSystemsManagerSAP-HanaLogBackupStatusCheck"),
    runbook!("AWSSystemsManagerSAP-RecoverHanaDatabase"),
    runbook!("AWSSystemsManagerSAP-RetrieveBackupMetadata"),
    runbook!("AWSSystemsManagerSAP-StartStopApplication"),
    runbook!("AWSSystemsManagerSAP-StartStopComponent"),
    runbook!("AWSSystemsManagerSAP-Support"),
    runbook!("AWSSystemsManagerSAP-UpdateHanaBackupSettings"),
];
