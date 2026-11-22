// SPDX-License-Identifier: CC0-1.0

//! The seeded INVENTORY of the AWS-managed `aws.events` catalogue: which event sources
//! it publishes schemas for, and which event types it publishes for each of them.
//!
//! Which entries the catalogue holds is per-source published data rather than anything
//! derived, so the two tables that carry it live here, beside the renderers in `super`
//! that turn a row into the schema the store seeds.

use super::{ManagedEventType, TAG_CHANGE_COMPONENTS};

/// The SERVICE-SPECIFIC event types the service-owned catalogue holds a schema for:
/// the event types a publishing source publishes of its own, beside the
/// CloudTrail-delivered family [`super::CLOUD_TRAIL_EVENT_FAMILY`] carries for the
/// sources in [`AWS_EVENT_SOURCES`].
///
/// This table is PER SOURCE, not a template over sources: which event types a source
/// publishes is published data, so a row exists for an event type AWS publishes and a
/// source carries exactly the rows written for it. A source may publish its own event
/// types and no CloudTrail-delivered schema at all, in which case it appears here and
/// not in [`AWS_EVENT_SOURCES`].
///
/// Where AWS has published ONE event type under more than one `detail-type` spelling —
/// a legacy name and a current one, differing in a letter's case — the catalogue holds
/// the entry each spelling yields, because each is a name a client resolves by, so both
/// are written out as rows of their own.
///
/// Source: the `detail` of each event as AWS documents it (the EventBridge event
/// reference for the publishing service); the `detail_type` is the `detail-type`
/// EventBridge delivers it under and the `event_name` is the name the catalogue
/// publishes it as, which AWS derives from that detail-type by taking the spaces out.
/// A row AWS documents the identity but not the `detail` of is written with
/// [`ManagedEventType::identified`].
///
/// This is seeded content, not a response: `ListRegistries`, `ListSchemas` and
/// `DescribeSchema` report it from the store like any other registry, and
/// `ExportSchema` converts it. A caller cannot write to it — see
/// [`super::RegistryScope::Aws`]. [`super::aws_managed_schemas`] is the whole catalogue,
/// and is what seeds the store.
pub const AWS_SERVICE_EVENT_TYPES: &[ManagedEventType] = &[
    ManagedEventType {
        source: "aws.autoscaling",
        detail_type: "EC2 Instance Launch Successful",
        event_name: "EC2InstanceLaunchSuccessful",
        components: r##"{"EC2InstanceLaunchSuccessful":{"type":"object","required":["AutoScalingGroupName","EC2InstanceId","RequestId"],"properties":{"AutoScalingGroupName":{"type":"string"},"EC2InstanceId":{"type":"string"},"RequestId":{"type":"string"}}}}"##,
    },
    ManagedEventType {
        source: "aws.ec2",
        detail_type: "EC2 Instance State-change Notification",
        event_name: "EC2InstanceStateChangeNotification",
        components: r##"{"EC2InstanceStateChangeNotification":{"type":"object","required":["instance-id","state"],"properties":{"instance-id":{"type":"string"},"state":{"type":"string"}}}}"##,
    },
    ManagedEventType {
        source: "aws.s3",
        detail_type: "Object Created",
        event_name: "ObjectCreated",
        components: r##"{"Bucket":{"type":"object","required":["name"],"properties":{"name":{"type":"string"}}},"Object":{"type":"object","required":["key","size"],"properties":{"key":{"type":"string"},"size":{"type":"integer","format":"int64"},"etag":{"type":"string"}}},"ObjectCreated":{"type":"object","required":["bucket","object","reason","version"],"properties":{"bucket":{"$ref":"#/components/schemas/Bucket"},"object":{"$ref":"#/components/schemas/Object"},"reason":{"type":"string"},"version":{"type":"string"}}}}"##,
    },
    ManagedEventType {
        source: "aws.batch",
        detail_type: "Batch Job State Change",
        event_name: "BatchJobStateChange",
        components: r##"{"BatchJobStateChange":{"type":"object","required":["jobArn","jobDefinition","jobId","jobName","jobQueue","status"],"properties":{"createdAt":{"type":"integer","format":"int64"},"jobArn":{"type":"string"},"jobDefinition":{"type":"string"},"jobId":{"type":"string"},"jobName":{"type":"string"},"jobQueue":{"type":"string"},"startedAt":{"type":"integer","format":"int64"},"status":{"type":"string"},"statusReason":{"type":"string"},"stoppedAt":{"type":"integer","format":"int64"}}}}"##,
    },
    ManagedEventType {
        source: "aws.codebuild",
        detail_type: "CodeBuild Build State Change",
        event_name: "CodeBuildBuildStateChange",
        components: r##"{"CodeBuildBuildStateChange":{"type":"object","required":["build-id","build-status","project-name","version"],"properties":{"build-id":{"type":"string"},"build-status":{"type":"string"},"current-phase":{"type":"string"},"current-phase-context":{"type":"string"},"project-name":{"type":"string"},"version":{"type":"string"}}}}"##,
    },
    ManagedEventType {
        source: "aws.codepipeline",
        detail_type: "CodePipeline Pipeline Execution State Change",
        event_name: "CodePipelinePipelineExecutionStateChange",
        components: r##"{"CodePipelinePipelineExecutionStateChange":{"type":"object","required":["execution-id","pipeline","state","version"],"properties":{"execution-id":{"type":"string"},"pipeline":{"type":"string"},"state":{"type":"string"},"version":{"type":"number"}}}}"##,
    },
    ManagedEventType {
        source: "aws.ecs",
        detail_type: "ECS Task State Change",
        event_name: "ECSTaskStateChange",
        components: r##"{"ECSContainer":{"type":"object","required":["containerArn","lastStatus","name","taskArn"],"properties":{"containerArn":{"type":"string"},"exitCode":{"type":"integer"},"lastStatus":{"type":"string"},"name":{"type":"string"},"reason":{"type":"string"},"taskArn":{"type":"string"}}},"ECSTaskStateChange":{"type":"object","required":["clusterArn","desiredStatus","lastStatus","taskArn","taskDefinitionArn"],"properties":{"clusterArn":{"type":"string"},"containers":{"type":"array","items":{"$ref":"#/components/schemas/ECSContainer"}},"createdAt":{"type":"string","format":"date-time"},"desiredStatus":{"type":"string"},"group":{"type":"string"},"lastStatus":{"type":"string"},"launchType":{"type":"string"},"startedBy":{"type":"string"},"stoppedReason":{"type":"string"},"taskArn":{"type":"string"},"taskDefinitionArn":{"type":"string"},"updatedAt":{"type":"string","format":"date-time"}}}}"##,
    },
    ManagedEventType {
        source: "aws.events",
        detail_type: "Scheduled Event",
        event_name: "ScheduledEvent",
        components: r##"{"ScheduledEvent":{"type":"object","properties":{}}}"##,
    },
    ManagedEventType {
        source: "aws.health",
        detail_type: "AWS Health Event",
        event_name: "AWSHealthEvent",
        components: r##"{"AWSHealthEvent":{"type":"object","required":["eventArn","eventTypeCategory","eventTypeCode","service","startTime"],"properties":{"affectedEntities":{"type":"array","items":{"$ref":"#/components/schemas/AWSHealthAffectedEntity"}},"endTime":{"type":"string"},"eventArn":{"type":"string"},"eventDescription":{"type":"array","items":{"$ref":"#/components/schemas/AWSHealthEventDescription"}},"eventTypeCategory":{"type":"string"},"eventTypeCode":{"type":"string"},"service":{"type":"string"},"startTime":{"type":"string"}}},"AWSHealthAffectedEntity":{"type":"object","properties":{"entityValue":{"type":"string"},"status":{"type":"string"}}},"AWSHealthEventDescription":{"type":"object","properties":{"language":{"type":"string"},"latestDescription":{"type":"string"}}}}"##,
    },
    // The tag-change event, published under BOTH spellings AWS has published it as:
    // the `detail-type` has been written with a lower-case and with a capital "on", and
    // the catalogue holds the entry each spelling yields — see
    // [`aws_managed_event_types`] on why two spellings are two entries. One components
    // template serves them, because they describe the SAME event.
    ManagedEventType {
        source: "aws.tag",
        detail_type: "Tag Change on Resource",
        event_name: "TagChangeonResource",
        components: TAG_CHANGE_COMPONENTS,
    },
    ManagedEventType {
        source: "aws.tag",
        detail_type: "Tag Change On Resource",
        event_name: "TagChangeOnResource",
        components: TAG_CHANGE_COMPONENTS,
    },
    // The other event type the tag source publishes: the notification that a resource's
    // effective tag policy is invalid. AWS documents the event, not the members of its
    // `detail`, so it is written with [`ManagedEventType::identified`].
    ManagedEventType::identified(
        "aws.tag",
        "Invalid Effective Tag Policy",
        "InvalidEffectiveTagPolicy",
    ),
    ManagedEventType {
        source: "aws.acm",
        detail_type: "ACM Certificate Available",
        event_name: "ACMCertificateAvailable",
        components: r##"{"ACMCertificateAvailable":{"type":"object","required":["Action","CertificateType","CommonName"],"properties":{"Action":{"type":"string"},"CertificateType":{"type":"string"},"CommonName":{"type":"string"},"DomainValidationMethod":{"type":"string"}}}}"##,
    },
    // The event types below are published by the catalogue with their identity (the
    // source and the `detail-type` EventBridge delivers them under) but with an
    // unconstrained `detail`: AWS documents that these events exist, not the members
    // their `detail` carries. [`UNDOCUMENTED_DETAIL`] says what that means for a
    // reader, and [`ManagedEventType::identified`] is how such a row is written.
    //
    // They are grouped by publishing source, because WHICH event types a source
    // publishes is per-source published data: a source publishes the event types AWS
    // publishes for it and no others, so a row is added for an event type AWS
    // publishes rather than derived from a template over sources.
    ManagedEventType::identified("aws.a4b", "Room State Change", "RoomStateChange"),
    ManagedEventType::identified(
        "aws.a4b",
        "Room Device Calendar Error Status",
        "RoomDeviceCalendarErrorStatus",
    ),
    // `aws.access-analyzer` publishes its finding events and NOTHING of the
    // CloudTrail-delivered family, which is why it is absent from
    // [`AWS_EVENT_SOURCES`].
    ManagedEventType::identified(
        "aws.access-analyzer",
        "Access Analyzer Finding",
        "AccessAnalyzerFinding",
    ),
    ManagedEventType::identified(
        "aws.access-analyzer",
        "Unused Access Finding for IAM entities",
        "UnusedAccessFindingforIAMentities",
    ),
    ManagedEventType::identified(
        "aws.account",
        "Region Opt-In Status Change",
        "RegionOpt-InStatusChange",
    ),
    // The certificate lifecycle event types `aws.acm` publishes. The two revocation
    // entries whose names differ only in a letter's case are two entries, not one: AWS
    // has published the revocation event under more than one `detail-type` spelling and
    // the catalogue holds each of them.
    ManagedEventType::identified(
        "aws.acm",
        "ACM Certificate Approaching Expiration",
        "ACMCertificateApproachingExpiration",
    ),
    ManagedEventType::identified("aws.acm", "ACM Certificate Expired", "ACMCertificateExpired"),
    ManagedEventType::identified(
        "aws.acm",
        "ACM Certificate Renewal Action Required",
        "ACMCertificateRenewalActionRequired",
    ),
    ManagedEventType::identified("aws.acm", "ACM Certificate Revoked", "ACMCertificateRevoked"),
    ManagedEventType::identified(
        "aws.acm",
        "ACM Certificate Revoked By CA",
        "ACMCertificateRevokedByCA",
    ),
    ManagedEventType::identified(
        "aws.acm",
        "ACM Certificate Revoked by Certificate Authority CA",
        "ACMCertificateRevokedbyCertificateAuthorityCA",
    ),
    ManagedEventType::identified(
        "aws.acm-pca",
        "ACM Private CA Audit Report Generation",
        "ACMPrivateCAAuditReportGeneration",
    ),
    ManagedEventType::identified(
        "aws.acm-pca",
        "ACM Private CA CRL Generation",
        "ACMPrivateCACRLGeneration",
    ),
    ManagedEventType::identified(
        "aws.acm-pca",
        "ACM Private CA Certificate Issuance",
        "ACMPrivateCACertificateIssuance",
    ),
    ManagedEventType::identified(
        "aws.acm-pca",
        "ACM Private CA Certificate Revocation",
        "ACMPrivateCACertificateRevocation",
    ),
    ManagedEventType::identified(
        "aws.acm-pca",
        "ACM Private CA Creation",
        "ACMPrivateCACreation",
    ),
    ManagedEventType::identified(
        "aws.act",
        "Trust Bundle Association Version Updated",
        "TrustBundleAssociationVersionUpdated",
    ),
    // The investigation lifecycle `aws.aidevops` publishes: one event type per state an
    // investigation reaches.
    ManagedEventType::identified(
        "aws.aidevops",
        "Investigation Cancelled",
        "InvestigationCancelled",
    ),
    ManagedEventType::identified(
        "aws.aidevops",
        "Investigation Completed",
        "InvestigationCompleted",
    ),
    ManagedEventType::identified(
        "aws.aidevops",
        "Investigation Created",
        "InvestigationCreated",
    ),
    ManagedEventType::identified("aws.aidevops", "Investigation Failed", "InvestigationFailed"),
    ManagedEventType::identified(
        "aws.aidevops",
        "Investigation In Progress",
        "InvestigationInProgress",
    ),
    ManagedEventType::identified("aws.aidevops", "Investigation Linked", "InvestigationLinked"),
    ManagedEventType::identified(
        "aws.aidevops",
        "Investigation Pending Triage",
        "InvestigationPendingTriage",
    ),
    ManagedEventType::identified(
        "aws.appflow",
        "AppFlow Automatic Field Import Report",
        "AppFlowAutomaticFieldImportReport",
    ),
    ManagedEventType::identified(
        "aws.application-autoscaling",
        "Application Auto Scaling Scaling Activity State Change",
        "ApplicationAutoScalingScalingActivityStateChange",
    ),
    // `aws.b2bi` publishes the acknowledgement event of its own BESIDE the
    // CloudTrail-delivered family [`AWS_EVENT_SOURCES`] carries for it.
    ManagedEventType::identified(
        "aws.b2bi",
        "Acknowledgement Completed",
        "AcknowledgementCompleted",
    ),
    ManagedEventType::identified(
        "aws.cleanrooms",
        "Analysis Template Created",
        "AnalysisTemplateCreated",
    ),
    ManagedEventType::identified(
        "aws.cleanrooms",
        "Analysis Template Deleted",
        "AnalysisTemplateDeleted",
    ),
    ManagedEventType::identified(
        "aws.cleanrooms",
        "Analysis Template Updated",
        "AnalysisTemplateUpdated",
    ),
    ManagedEventType::identified(
        "aws.deadline",
        "Budget Threshold Reached",
        "BudgetThresholdReached",
    ),
    ManagedEventType::identified(
        "aws.deadline",
        "Fleet Size Recommendation Change",
        "FleetSizeRecommendationChange",
    ),
    ManagedEventType::identified(
        "aws.deadline",
        "Job Lifecycle Status Change",
        "JobLifecycleStatusChange",
    ),
    ManagedEventType::identified(
        "aws.deadline",
        "Job Run Status Change",
        "JobRunStatusChange",
    ),
    ManagedEventType::identified(
        "aws.deadline",
        "Step Lifecycle Status Change",
        "StepLifecycleStatusChange",
    ),
    ManagedEventType::identified(
        "aws.deadline",
        "Step Run Status Change",
        "StepRunStatusChange",
    ),
    // The migration event types `aws.dms` publishes BESIDE the CloudTrail-delivered
    // family [`AWS_EVENT_SOURCES`] carries for it: one per migration resource whose
    // state change the service delivers — a data migration, an endpoint, a replication
    // instance and a replication task.
    ManagedEventType::identified(
        "aws.dms",
        "DMS Data Migration State Change",
        "DMSDataMigrationStateChange",
    ),
    ManagedEventType::identified(
        "aws.dms",
        "DMS Replication Endpoint State Change",
        "DMSReplicationEndpointStateChange",
    ),
    ManagedEventType::identified(
        "aws.dms",
        "DMS Replication Instance State Change",
        "DMSReplicationInstanceStateChange",
    ),
    ManagedEventType::identified(
        "aws.dms",
        "DMS Replication Task State Change",
        "DMSReplicationTaskStateChange",
    ),
    // The discovery update `aws.ds` publishes BESIDE the CloudTrail-delivered family
    // [`AWS_EVENT_SOURCES`] carries for it.
    ManagedEventType::identified(
        "aws.ds",
        "Directory Service Domain Controller Discovery Update",
        "DirectoryServiceDomainControllerDiscoveryUpdate",
    ),
    // The fleet event types `aws.ec2fleet` publishes: the five an EC2 Fleet delivers
    // and the five a Spot Fleet delivers, both under this one source. It publishes
    // nothing of the CloudTrail-delivered family, which is why it is absent from
    // [`AWS_EVENT_SOURCES`].
    ManagedEventType::identified("aws.ec2fleet", "EC2 Fleet Error", "EC2FleetError"),
    ManagedEventType::identified(
        "aws.ec2fleet",
        "EC2 Fleet Information",
        "EC2FleetInformation",
    ),
    ManagedEventType::identified(
        "aws.ec2fleet",
        "EC2 Fleet Instance Change",
        "EC2FleetInstanceChange",
    ),
    ManagedEventType::identified(
        "aws.ec2fleet",
        "EC2 Fleet Spot Instance Request Change",
        "EC2FleetSpotInstanceRequestChange",
    ),
    ManagedEventType::identified(
        "aws.ec2fleet",
        "EC2 Fleet State Change",
        "EC2FleetStateChange",
    ),
    ManagedEventType::identified("aws.ec2fleet", "EC2 Spot Fleet Error", "EC2SpotFleetError"),
    ManagedEventType::identified(
        "aws.ec2fleet",
        "EC2 Spot Fleet Information",
        "EC2SpotFleetInformation",
    ),
    ManagedEventType::identified(
        "aws.ec2fleet",
        "EC2 Spot Fleet Instance Change",
        "EC2SpotFleetInstanceChange",
    ),
    ManagedEventType::identified(
        "aws.ec2fleet",
        "EC2 Spot Fleet Spot Instance Request Change",
        "EC2SpotFleetSpotInstanceRequestChange",
    ),
    ManagedEventType::identified(
        "aws.ec2fleet",
        "EC2 Spot Fleet State Change",
        "EC2SpotFleetStateChange",
    ),
    // `aws.ec2spotfleet` publishes the same ten fleet event types as `aws.ec2fleet`
    // above: WHICH event types a source publishes is per-source published data, so a
    // second source delivering the same event types carries ten entries of its own
    // rather than sharing that source's. It publishes nothing of the CloudTrail-delivered
    // family, which is why it is absent from [`AWS_EVENT_SOURCES`].
    ManagedEventType::identified("aws.ec2spotfleet", "EC2 Fleet Error", "EC2FleetError"),
    ManagedEventType::identified(
        "aws.ec2spotfleet",
        "EC2 Fleet Information",
        "EC2FleetInformation",
    ),
    ManagedEventType::identified(
        "aws.ec2spotfleet",
        "EC2 Fleet Instance Change",
        "EC2FleetInstanceChange",
    ),
    ManagedEventType::identified(
        "aws.ec2spotfleet",
        "EC2 Fleet Spot Instance Request Change",
        "EC2FleetSpotInstanceRequestChange",
    ),
    ManagedEventType::identified(
        "aws.ec2spotfleet",
        "EC2 Fleet State Change",
        "EC2FleetStateChange",
    ),
    ManagedEventType::identified(
        "aws.ec2spotfleet",
        "EC2 Spot Fleet Error",
        "EC2SpotFleetError",
    ),
    ManagedEventType::identified(
        "aws.ec2spotfleet",
        "EC2 Spot Fleet Information",
        "EC2SpotFleetInformation",
    ),
    ManagedEventType::identified(
        "aws.ec2spotfleet",
        "EC2 Spot Fleet Instance Change",
        "EC2SpotFleetInstanceChange",
    ),
    ManagedEventType::identified(
        "aws.ec2spotfleet",
        "EC2 Spot Fleet Spot Instance Request Change",
        "EC2SpotFleetSpotInstanceRequestChange",
    ),
    ManagedEventType::identified(
        "aws.ec2spotfleet",
        "EC2 Spot Fleet State Change",
        "EC2SpotFleetStateChange",
    ),
    // `aws.epfs` publishes the job and session lifecycle event types below and nothing
    // of the CloudTrail-delivered family.
    ManagedEventType::identified(
        "aws.epfs",
        "Node Identification Job State Changed",
        "NodeIdentificationJobStateChanged",
    ),
    ManagedEventType::identified(
        "aws.epfs",
        "Power Flow Job State Changed",
        "PowerFlowJobStateChanged",
    ),
    ManagedEventType::identified(
        "aws.epfs",
        "Power Flow Node Job State Changed",
        "PowerFlowNodeJobStateChanged",
    ),
    ManagedEventType::identified(
        "aws.epfs",
        "Session State Changed",
        "SessionStateChanged",
    ),
    ManagedEventType::identified(
        "aws.epfs",
        "Subsystem Analysis Job State Changed",
        "SubsystemAnalysisJobStateChanged",
    ),
    ManagedEventType::identified(
        "aws.events",
        "API Destination Activated",
        "APIDestinationActivated",
    ),
    ManagedEventType::identified(
        "aws.events",
        "API Destination Deactivated",
        "APIDestinationDeactivated",
    ),
    // The connection event types `aws.events` publishes: a connection's authorization
    // and its lifecycle, each delivered under its own `detail-type`. The pair of
    // "started" events ("Connection Creation Started", "Connection Update Started",
    // "Connection Deletion Started") report work beginning, and the rest report the
    // state a connection reached.
    ManagedEventType::identified("aws.events", "Connection Activated", "ConnectionActivated"),
    ManagedEventType::identified(
        "aws.events",
        "Connection Authorization Started",
        "ConnectionAuthorizationStarted",
    ),
    ManagedEventType::identified(
        "aws.events",
        "Connection Authorized",
        "ConnectionAuthorized",
    ),
    ManagedEventType::identified(
        "aws.events",
        "Connection Creation Started",
        "ConnectionCreationStarted",
    ),
    ManagedEventType::identified(
        "aws.events",
        "Connection Deauthorization Started",
        "ConnectionDeauthorizationStarted",
    ),
    ManagedEventType::identified(
        "aws.events",
        "Connection Deauthorized",
        "ConnectionDeauthorized",
    ),
    ManagedEventType::identified(
        "aws.events",
        "Connection Deletion Started",
        "ConnectionDeletionStarted",
    ),
    ManagedEventType::identified(
        "aws.events",
        "Connection Failed Connectivity",
        "ConnectionFailedConnectivity",
    ),
    ManagedEventType::identified(
        "aws.events",
        "Connection Update Started",
        "ConnectionUpdateStarted",
    ),
    // The scheduled events `aws.events` delivers to a rule's targets: the plain
    // scheduled event ([`AWS_SERVICE_EVENT_TYPES`] above, which carries a document)
    // and the one whose payload is JSON.
    ManagedEventType::identified("aws.events", "Scheduled Json", "ScheduledJson"),
    // `aws.factstore` publishes one event type of its own and nothing of the
    // CloudTrail-delivered family.
    ManagedEventType::identified("aws.factstore", "New Commit Pushed", "NewCommitPushed"),
    // The event types `aws.gamelift` publishes BESIDE the CloudTrail-delivered family
    // [`AWS_EVENT_SOURCES`] carries for it: the fleet event, the matchmaking event and
    // the queue placement event, each of which reports the kind of change in its own
    // `detail`.
    ManagedEventType::identified("aws.gamelift", "GameLift Fleet Event", "GameLiftFleetEvent"),
    ManagedEventType::identified(
        "aws.gamelift",
        "GameLift Matchmaking Event",
        "GameLiftMatchmakingEvent",
    ),
    ManagedEventType::identified(
        "aws.gamelift",
        "GameLift Queue Placement Event",
        "GameLiftQueuePlacementEvent",
    ),
    // The event types `aws.glue-autodebug` publishes — the auto-version job lifecycle,
    // the data-quality results and the metering event — and nothing of the
    // CloudTrail-delivered family. The entries whose published names run words
    // together carry the `detail-type` they were published under, spelling for
    // spelling: the catalogue derives the name from the detail-type by taking the
    // spaces out, so the name is what fixes the spelling of each word.
    ManagedEventType::identified(
        "aws.glue-autodebug",
        "Auto Version Job State Change",
        "AutoVersionJobStateChange",
    ),
    ManagedEventType::identified(
        "aws.glue-autodebug",
        "Autodebug metadata key shared from Orca",
        "AutodebugmetadatakeysharedfromOrca",
    ),
    ManagedEventType::identified(
        "aws.glue-autodebug",
        "Autoversion job run completed",
        "Autoversionjobruncompleted",
    ),
    ManagedEventType::identified(
        "aws.glue-autodebug",
        "Autoversion job version updated",
        "Autoversionjobversionupdated",
    ),
    ManagedEventType::identified(
        "aws.glue-autodebug",
        "Data Quality Evaluation Results Available",
        "DataQualityEvaluationResultsAvailable",
    ),
    ManagedEventType::identified(
        "aws.glue-autodebug",
        "Data Quality Task State Change",
        "DataQualityTaskStateChange",
    ),
    ManagedEventType::identified(
        "aws.glue-autodebug",
        "Entity Metering Event",
        "EntityMeteringEvent",
    ),
    // `aws.goshawk` publishes these two repository event types and nothing of the
    // CloudTrail-delivered family.
    ManagedEventType::identified(
        "aws.goshawk",
        "CodeArtifact Package Version State Change",
        "CodeArtifactPackageVersionStateChange",
    ),
    ManagedEventType::identified(
        "aws.goshawk",
        "Goshawk Repository State Change",
        "GoshawkRepositoryStateChange",
    ),
    // The abuse event `aws.health` publishes beside its health event (above, which
    // carries a document).
    ManagedEventType::identified(
        "aws.health",
        "AWS Health Abuse Event",
        "AWSHealthAbuseEvent",
    ),
    // The event types `aws.ivs` publishes BESIDE the CloudTrail-delivered family
    // [`AWS_EVENT_SOURCES`] carries for it: the ad-break and channel-agent session
    // changes, the limit breach, a channel's recording and stream state changes and the
    // update of a real-time stage.
    ManagedEventType::identified(
        "aws.ivs",
        "IVS Ad Break State Change",
        "IVSAdBreakStateChange",
    ),
    ManagedEventType::identified(
        "aws.ivs",
        "IVS Channel Agent Session State Change",
        "IVSChannelAgentSessionStateChange",
    ),
    ManagedEventType::identified("aws.ivs", "IVS Limit Breach", "IVSLimitBreach"),
    ManagedEventType::identified(
        "aws.ivs",
        "IVS Recording State Change",
        "IVSRecordingStateChange",
    ),
    ManagedEventType::identified("aws.ivs", "IVS Stage Update", "IVSStageUpdate"),
    ManagedEventType::identified(
        "aws.ivs",
        "IVS Stream State Change",
        "IVSStreamStateChange",
    ),
    // `aws.labs` and `aws.maester` publish their own event types and NOTHING of the
    // CloudTrail-delivered family, which is why neither is in [`AWS_EVENT_SOURCES`].
    ManagedEventType::identified("aws.labs", "Lab Auto Cancelled", "LabAutoCancelled"),
    ManagedEventType::identified("aws.labs", "Lab Cost Alert", "LabCostAlert"),
    ManagedEventType::identified("aws.labs", "Lab Cost Update", "LabCostUpdate"),
    ManagedEventType::identified(
        "aws.labs",
        "Lab Launch Status Change",
        "LabLaunchStatusChange",
    ),
    ManagedEventType::identified(
        "aws.labs",
        "Lab Termination Status Change",
        "LabTerminationStatusChange",
    ),
    ManagedEventType::identified("aws.maester", "Document Created", "DocumentCreated"),
    ManagedEventType::identified("aws.maester", "Document Deleted", "DocumentDeleted"),
    ManagedEventType::identified("aws.maester", "Document Updated", "DocumentUpdated"),
    // The link event `aws.oam` publishes BESIDE the CloudTrail-delivered family
    // [`AWS_EVENT_SOURCES`] carries for it.
    ManagedEventType::identified("aws.oam", "Attached Link Created", "AttachedLinkCreated"),
    // The deliverability-advisor recommendation event types `aws.ses` publishes BESIDE
    // the CloudTrail-delivered family [`AWS_EVENT_SOURCES`] carries for it: one per
    // status a recommendation reaches.
    ManagedEventType::identified(
        "aws.ses",
        "Advisor Recommendation Status Open",
        "AdvisorRecommendationStatusOpen",
    ),
    ManagedEventType::identified(
        "aws.ses",
        "Advisor Recommendation Status Resolved",
        "AdvisorRecommendationStatusResolved",
    ),
    // The WhatsApp event types `aws.social-messaging` publishes: the call lifecycle and
    // the message lifecycle, each event type published under the `detail-type` the
    // service delivers it as. It publishes nothing of the CloudTrail-delivered family,
    // which is why it is absent from [`AWS_EVENT_SOURCES`].
    //
    // Read off the published catalogue one entry at a time, like every other row of this
    // table: a row is written for an event type the catalogue is known to publish, and a
    // name nobody publishes is not invented here — an entry this table does not carry is
    // one schema missing from a listing, while one it carries that AWS does not publish
    // is a schema no client can describe anywhere else.
    ManagedEventType::identified(
        "aws.social-messaging",
        "WhatsApp Call Accepted",
        "WhatsAppCallAccepted",
    ),
    ManagedEventType::identified(
        "aws.social-messaging",
        "WhatsApp Call Connect",
        "WhatsAppCallConnect",
    ),
    ManagedEventType::identified(
        "aws.social-messaging",
        "WhatsApp Call Failed",
        "WhatsAppCallFailed",
    ),
    ManagedEventType::identified(
        "aws.social-messaging",
        "WhatsApp Call Handover Failed",
        "WhatsAppCallHandoverFailed",
    ),
    ManagedEventType::identified(
        "aws.social-messaging",
        "WhatsApp Call Handover Succeeded",
        "WhatsAppCallHandoverSucceeded",
    ),
    ManagedEventType::identified(
        "aws.social-messaging",
        "WhatsApp Call Rejected",
        "WhatsAppCallRejected",
    ),
    ManagedEventType::identified(
        "aws.social-messaging",
        "WhatsApp Call Ringing",
        "WhatsAppCallRinging",
    ),
    ManagedEventType::identified(
        "aws.social-messaging",
        "WhatsApp Call Terminate",
        "WhatsAppCallTerminate",
    ),
    ManagedEventType::identified(
        "aws.social-messaging",
        "WhatsApp Message Delivered",
        "WhatsAppMessageDelivered",
    ),
    ManagedEventType::identified(
        "aws.social-messaging",
        "WhatsApp Message Failed",
        "WhatsAppMessageFailed",
    ),
    ManagedEventType::identified(
        "aws.social-messaging",
        "WhatsApp Message Handover Failed",
        "WhatsAppMessageHandoverFailed",
    ),
    ManagedEventType::identified(
        "aws.social-messaging",
        "WhatsApp Message Handover Succeeded",
        "WhatsAppMessageHandoverSucceeded",
    ),
    ManagedEventType::identified(
        "aws.social-messaging",
        "WhatsApp Message Read",
        "WhatsAppMessageRead",
    ),
    ManagedEventType::identified(
        "aws.social-messaging",
        "WhatsApp Message Received",
        "WhatsAppMessageReceived",
    ),
    ManagedEventType::identified(
        "aws.social-messaging",
        "WhatsApp Message Sent",
        "WhatsAppMessageSent",
    ),
    // The event types `aws.ssm` publishes BESIDE the CloudTrail-delivered family
    // [`AWS_EVENT_SOURCES`] carries for it: the access-request and change-calendar
    // events, the compliance and inventory state changes, the command and automation
    // status-change notifications, the State Manager association state changes, the
    // maintenance-window notifications and the Parameter Store events. The published
    // name of a `Status-change Notification` event keeps the hyphen of its
    // `detail-type`, because the name is that detail-type with the spaces taken out.
    ManagedEventType::identified(
        "aws.ssm",
        "Approver Access Request Status Update",
        "ApproverAccessRequestStatusUpdate",
    ),
    ManagedEventType::identified("aws.ssm", "Calendar State Change", "CalendarStateChange"),
    ManagedEventType::identified(
        "aws.ssm",
        "Configuration Compliance State Change",
        "ConfigurationComplianceStateChange",
    ),
    ManagedEventType::identified(
        "aws.ssm",
        "EC2 Automation Execution Status-change Notification",
        "EC2AutomationExecutionStatus-changeNotification",
    ),
    ManagedEventType::identified(
        "aws.ssm",
        "EC2 Automation Step Status-change Notification",
        "EC2AutomationStepStatus-changeNotification",
    ),
    ManagedEventType::identified(
        "aws.ssm",
        "EC2 Command Invocation Status-change Notification",
        "EC2CommandInvocationStatus-changeNotification",
    ),
    ManagedEventType::identified(
        "aws.ssm",
        "EC2 Command Status-change Notification",
        "EC2CommandStatus-changeNotification",
    ),
    ManagedEventType::identified(
        "aws.ssm",
        "EC2 State Manager Association State Change",
        "EC2StateManagerAssociationStateChange",
    ),
    ManagedEventType::identified(
        "aws.ssm",
        "EC2 State Manager Instance Association State Change",
        "EC2StateManagerInstanceAssociationStateChange",
    ),
    ManagedEventType::identified(
        "aws.ssm",
        "Inventory Resource State Change",
        "InventoryResourceStateChange",
    ),
    ManagedEventType::identified(
        "aws.ssm",
        "Maintenance Window Execution State-change Notification",
        "MaintenanceWindowExecutionState-changeNotification",
    ),
    ManagedEventType::identified(
        "aws.ssm",
        "Maintenance Window State-change Notification",
        "MaintenanceWindowState-changeNotification",
    ),
    ManagedEventType::identified(
        "aws.ssm",
        "Maintenance Window Target Registration Notification",
        "MaintenanceWindowTargetRegistrationNotification",
    ),
    ManagedEventType::identified(
        "aws.ssm",
        "Maintenance Window Task Execution State-change Notification",
        "MaintenanceWindowTaskExecutionState-changeNotification",
    ),
    ManagedEventType::identified(
        "aws.ssm",
        "Maintenance Window Task Target Invocation State-change Notification",
        "MaintenanceWindowTaskTargetInvocationState-changeNotification",
    ),
    ManagedEventType::identified("aws.ssm", "Parameter Store Change", "ParameterStoreChange"),
    ManagedEventType::identified(
        "aws.ssm",
        "Parameter Store Policy Action",
        "ParameterStorePolicyAction",
    ),
];

/// The AWS event sources the managed catalogue holds a CloudTrail-delivered schema
/// FOR — the `aws.<service>` namespaces whose CloudTrail records EventBridge delivers,
/// under the detail-types of [`super::CLOUD_TRAIL_EVENT_FAMILY`].
///
/// Source: the event-source namespaces the published catalogue carries those schemas
/// under. WHICH sources they are is published data, read off the catalogue one source
/// at a time — it is not every `aws.<service>` namespace there is, and a source's
/// entry here says nothing about which of its OWN event types the catalogue publishes
/// ([`AWS_SERVICE_EVENT_TYPES`] carries those, per source, and a source may appear
/// there and not here). Kept in alphabetical order for reading; the store sorts by
/// name on its own, and a name repeated here seeds one schema, not two.
pub const AWS_EVENT_SOURCES: &[&str] = &[
    "aws.a4b",
    "aws.abuse",
    // `aws.access-analyzer` is deliberately NOT here: the catalogue publishes its
    // finding event types ([`AWS_SERVICE_EVENT_TYPES`]) and no CloudTrail-delivered
    // schema for it. Membership of this list is per-source published data, not every
    // event source there is.
    "aws.access-insights",
    "aws.account",
    "aws.acm",
    "aws.acm-pca",
    "aws.act",
    // `aws.aidevops` is deliberately NOT here: the catalogue publishes its investigation
    // lifecycle event types ([`AWS_SERVICE_EVENT_TYPES`]) and no CloudTrail-delivered
    // schema for it, so its block of the catalogue is those entries alone.
    "aws.airflow",
    "aws.amazon.connect",
    "aws.amazonmq",
    "aws.amazonq",
    "aws.amplify",
    "aws.amplifybackend",
    "aws.amplifyuibuilder",
    "aws.apigateway",
    "aws.appconfig",
    "aws.appflow",
    "aws.appintegrations",
    "aws.application-autoscaling",
    "aws.application-insights",
    "aws.applicationcostprofiler",
    "aws.appmesh",
    "aws.apprunner",
    "aws.appstream",
    "aws.appsync",
    "aws.aps",
    "aws.artifact",
    "aws.athena",
    "aws.auditmanager",
    "aws.autoscaling",
    "aws.autoscaling-plans",
    "aws.b2bi",
    "aws.backup",
    "aws.backup-gateway",
    "aws.batch",
    "aws.billingconductor",
    "aws.braket",
    "aws.budgets",
    "aws.cassandra",
    "aws.ce",
    "aws.chatbot",
    "aws.chime",
    "aws.chime-sdk-media-pipelines",
    "aws.chime-sdk-voice",
    "aws.cleanrooms",
    "aws.cloud9",
    "aws.clouddirectory",
    "aws.cloudformation",
    "aws.cloudfront",
    "aws.cloudhsm",
    "aws.cloudsearch",
    "aws.cloudshell",
    "aws.cloudtrail",
    "aws.cloudwatch",
    "aws.codeartifact",
    "aws.codebuild",
    "aws.codecommit",
    "aws.codedeploy",
    "aws.codeguru-profiler",
    "aws.codeguru-reviewer",
    "aws.codepipeline",
    "aws.codestar",
    "aws.codestar-connections",
    "aws.codestar-notifications",
    "aws.cognito-identity",
    "aws.cognito-idp",
    "aws.cognito-sync",
    "aws.comprehend",
    "aws.comprehendmedical",
    "aws.compute-optimizer",
    "aws.config",
    "aws.connect",
    "aws.connect-contact-lens",
    "aws.controltower",
    "aws.databrew",
    "aws.dataexchange",
    "aws.datapipeline",
    "aws.datasync",
    "aws.dax",
    "aws.deadline",
    "aws.deepracer",
    "aws.detective",
    "aws.devicefarm",
    "aws.devops-guru",
    "aws.directconnect",
    "aws.discovery",
    "aws.dlm",
    "aws.dms",
    "aws.drs",
    "aws.ds",
    "aws.dynamodb",
    "aws.ebs",
    "aws.ec2",
    "aws.ec2-instance-connect",
    "aws.ecr",
    "aws.ecr-public",
    "aws.ecs",
    "aws.eks",
    "aws.elasticache",
    "aws.elasticbeanstalk",
    "aws.elasticfilesystem",
    "aws.elasticloadbalancing",
    "aws.elasticmapreduce",
    "aws.elastictranscoder",
    "aws.email",
    "aws.emr-containers",
    "aws.emr-serverless",
    "aws.es",
    "aws.events",
    "aws.evidently",
    "aws.finspace",
    "aws.finspace-api",
    "aws.firehose",
    "aws.fis",
    "aws.fms",
    "aws.forecast",
    "aws.frauddetector",
    "aws.fsx",
    "aws.gamelift",
    "aws.glacier",
    "aws.globalaccelerator",
    "aws.glue",
    "aws.grafana",
    "aws.greengrass",
    "aws.groundstation",
    "aws.guardduty",
    "aws.health",
    "aws.health-agent",
    "aws.healthlake",
    "aws.iam",
    "aws.identitystore",
    "aws.imagebuilder",
    "aws.inspector",
    "aws.inspector-scan",
    "aws.inspector2",
    "aws.iot",
    "aws.iot1click",
    "aws.iotanalytics",
    "aws.iotdeviceadvisor",
    "aws.iotevents",
    "aws.iotfleethub",
    "aws.iotsecuredtunneling",
    "aws.iotsitewise",
    "aws.iottwinmaker",
    "aws.iotwireless",
    "aws.ivs",
    "aws.kafka",
    "aws.kafka-cluster",
    "aws.kafkaconnect",
    "aws.kendra",
    "aws.kinesis",
    "aws.kinesisanalytics",
    "aws.kinesisvideo",
    "aws.kms",
    "aws.lakeformation",
    "aws.lambda",
    "aws.lex",
    "aws.license-manager",
    "aws.lightsail",
    "aws.location",
    "aws.logs",
    "aws.lookoutequipment",
    "aws.lookoutmetrics",
    "aws.lookoutvision",
    "aws.machinelearning",
    "aws.macie",
    "aws.managedblockchain",
    "aws.mediaconnect",
    "aws.mediaconvert",
    "aws.medialive",
    "aws.mediapackage",
    "aws.mediapackage-vod",
    "aws.mediastore",
    "aws.mediatailor",
    "aws.memorydb",
    "aws.mgh",
    "aws.mgn",
    "aws.mq",
    "aws.mturk",
    "aws.neptune",
    "aws.network-firewall",
    "aws.networkmanager",
    "aws.nimble",
    "aws.oam",
    "aws.opensearch",
    "aws.opsworks",
    "aws.organizations",
    "aws.outposts",
    "aws.panorama",
    "aws.personalize",
    "aws.pi",
    "aws.pinpoint",
    "aws.polly",
    "aws.pricing",
    "aws.proton",
    "aws.qldb",
    "aws.quicksight",
    "aws.ram",
    "aws.rbin",
    "aws.rds",
    "aws.redshift",
    "aws.redshift-data",
    "aws.redshift-serverless",
    "aws.rekognition",
    "aws.resiliencehub",
    "aws.resource-groups",
    "aws.robomaker",
    "aws.route53",
    "aws.route53domains",
    "aws.route53resolver",
    "aws.rum",
    "aws.s3",
    "aws.s3-outposts",
    "aws.sagemaker",
    "aws.savingsplans",
    "aws.scheduler",
    "aws.schemas",
    "aws.sdb",
    "aws.secretsmanager",
    "aws.securityhub",
    "aws.serverlessrepo",
    "aws.servicecatalog",
    "aws.servicediscovery",
    "aws.servicequotas",
    "aws.ses",
    "aws.shield",
    "aws.signer",
    "aws.signin",
    "aws.sms",
    "aws.snowball",
    "aws.sns",
    "aws.sqs",
    "aws.ssm",
    "aws.ssm-contacts",
    "aws.ssm-incidents",
    "aws.sso",
    "aws.states",
    "aws.storagegateway",
    "aws.sts",
    "aws.support",
    "aws.swf",
    "aws.synthetics",
    // `aws.tag` is deliberately NOT here: it is the source EventBridge delivers tag
    // notifications under, not the namespace of an API whose calls CloudTrail records,
    // so the catalogue publishes the event types held for it
    // ([`AWS_SERVICE_EVENT_TYPES`]) and no CloudTrail-delivered schema. The calls of the
    // tagging API are recorded under the namespace below.
    "aws.tagging",
    "aws.textract",
    "aws.timestream",
    "aws.transcribe",
    "aws.transfer",
    "aws.translate",
    "aws.trustedadvisor",
    "aws.voiceid",
    "aws.waf",
    "aws.waf-regional",
    "aws.wafv2",
    "aws.wellarchitected",
    "aws.wisdom",
    "aws.workdocs",
    "aws.worklink",
    "aws.workmail",
    "aws.workspaces",
    "aws.xray",
];

/// The event sources the catalogue publishes the LEGACY spelling of the API-call event
/// for, BESIDE the current spelling every source in [`AWS_EVENT_SOURCES`] carries.
///
/// AWS has delivered the CloudTrail API-call event under two spellings of its
/// `detail-type` — the current one, which writes "via" in lower case
/// ([`super::CLOUD_TRAIL_DETAIL_TYPE`]), and an older one writing it with a capital V
/// ([`super::LEGACY_CLOUD_TRAIL_DETAIL_TYPE`]) — and the catalogue holds the entry each
/// spelling yields, because each is a name a client resolves a schema by. The legacy
/// entry collates immediately AHEAD of the current one, a capital `V` preceding a
/// lower-case `v`, so a source that carries it opens its block with it.
///
/// Source: the sources the published catalogue carries the legacy entry under, read off
/// the catalogue one source at a time. It is per-source published data exactly as
/// membership of [`AWS_EVENT_SOURCES`] is: the catalogue publishes the legacy entry for
/// the sources whose API calls it recorded under that spelling and for no others, so a
/// source is written here rather than the spelling being fanned out over every source.
/// Kept in alphabetical order for reading.
pub const LEGACY_API_CALL_SOURCES: &[&str] =
    &["aws.autoscaling", "aws.ec2", "aws.ecs", "aws.sagemaker"];
