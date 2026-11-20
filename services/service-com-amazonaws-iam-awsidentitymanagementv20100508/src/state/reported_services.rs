// SPDX-License-Identifier: CC0-1.0

//! The service prefixes IAM publishes, which of them service-last-accessed data reports an
//! entry for, and the wording of a read asked for a namespace none of them is published
//! under.
//!
//! One concern, held in one place: the catalog [`PUBLISHED_SERVICES`], the row type
//! [`PublishedService`], the subset a report enumerates ([`last_accessed_services`]), the
//! lookup over the catalog ([`published_service`]) and the refusal wording for a namespace
//! it does not carry ([`unsupported_service_namespace`]).

#[allow(unused_imports)]
use super::*;

/// One service IAM publishes a prefix for: the namespace a policy names that service's
/// actions under, the name it is published under, and whether service-last-accessed data
/// reports an entry for it.
#[derive(Debug, Clone, Copy)]
pub struct PublishedService {
    /// "The namespace of the service in which access was attempted"
    /// (`ServiceLastAccessed$ServiceNamespace`): the service prefix an action element
    /// spells before the separator.
    pub service_namespace: &'static str,
    /// "The name of the service in which access was attempted"
    /// (`ServiceLastAccessed$ServiceName`): the name this namespace is published under,
    /// which for a service a report enumerates is the name last-accessed data reports it
    /// under.
    pub service_name: &'static str,
    /// Whether service-last-accessed data reports an ENTRY for this service.
    ///
    /// The two sets this catalog holds are not the same one: every row is a prefix a policy
    /// can name an action of — which is what a read taking a `ServiceNamespace` accepts —
    /// while a report enumerates only the rows last-accessed data reports on
    /// ([`last_accessed_services`]).
    pub reports_last_accessed: bool,
}

impl PublishedService {
    /// One catalog row for a service last-accessed data reports on: the published prefix,
    /// and the name that data reports it under. A constructor rather than a struct literal
    /// because the catalog is long enough that the field names would hide the data.
    const fn new(service_namespace: &'static str, service_name: &'static str) -> Self {
        Self {
            service_namespace,
            service_name,
            reports_last_accessed: true,
        }
    }

    /// One catalog row for a published prefix last-accessed data reports NO entry for: a
    /// policy can name an action of it, so a read taking a `ServiceNamespace` accepts it,
    /// and a report enumerates nothing for it.
    const fn unreported(service_namespace: &'static str, service_name: &'static str) -> Self {
        Self {
            service_namespace,
            service_name,
            reports_last_accessed: false,
        }
    }
}

/// Every service prefix IAM publishes, ascending by namespace, with the name each is
/// published under and whether a service-last-accessed report enumerates it.
///
/// SOURCE: the Service Authorization Reference ("Actions, resources, and condition keys for
/// Amazon Web Services services"), which `ServiceLastAccessed$ServiceNamespace`'s own
/// documentation points at to learn a service's namespace ("choose the name of the service
/// to view details for that service. In the first paragraph, find the service prefix"), so
/// every row is one service of that reference: a prefix a policy can name an action of, and
/// the name it is published under. A read taking a `ServiceNamespace` accepts any of them
/// ([`published_service`]).
///
/// A REPORT enumerates the rows last-accessed data reports an entry for
/// ([`last_accessed_services`]) — the rest are prefixes a policy can name without that data
/// covering them — and reports each under the name THAT data publishes for the namespace,
/// which is not always the service's own marketing name: where one service is reported under
/// two namespaces, one for managing it and one for using it, each namespace carries its own
/// name.
///
/// The reference's own breadth, not a shortlist: a last-accessed report is what a
/// least-privilege review is built on, so the services it enumerates have to be the ones an
/// account's principals can actually be granted and last-accessed data reports on — hundreds
/// of them — and a report short of them reads as a principal that reaches a handful of
/// services. A namespace this catalog does not carry at all names no service: it is refused
/// as an invalid input rather than reported without a name.
///
/// Ascending by namespace, because "by default, the list is sorted by service namespace"
/// (`GetServiceLastAccessedDetails`), so the catalog's own order IS the reported order: a
/// namespace missing from it would shift every later entry of a report read positionally,
/// which is why the order is asserted to ascend strictly and a new service is filed at its
/// place rather than appended.
pub const PUBLISHED_SERVICES: &[PublishedService] = &[
    PublishedService::new("a4b", "Alexa for Business"),
    PublishedService::new("access-analyzer", "AWS IAM Access Analyzer"),
    PublishedService::new("account", "AWS Account Management"),
    PublishedService::new("acm", "AWS Certificate Manager"),
    PublishedService::new("acm-pca", "AWS Private Certificate Authority"),
    PublishedService::new("action-recommendations", "AWS Action Recommendations"),
    PublishedService::new("agent-registry", "AWS Agent Registry"),
    PublishedService::new("aidevops", "AWS DevOps Agent Service"),
    PublishedService::new("aiops", "Amazon AI Operations"),
    PublishedService::new("airflow", "Amazon Managed Workflows for Apache Airflow"),
    PublishedService::new("amplify", "AWS Amplify"),
    // The two Amplify prefixes a policy can name an action of that last-accessed data
    // reports no entry for: a report serves nothing for them, while a read taking one of
    // them as its `ServiceNamespace` still accepts it.
    PublishedService::unreported("amplifybackend", "AWS Amplify Admin"),
    PublishedService::unreported("amplifyuibuilder", "AWS Amplify UI Builder"),
    PublishedService::new("aoss", "Amazon OpenSearch Serverless"),
    // `apigateway` is the service's MANAGEMENT plane and `execute-api` the calls made
    // through a deployed API, so last-accessed data reports API Gateway under two
    // namespaces and only this one carries the `Manage - ` prefix.
    PublishedService::new("apigateway", "Manage - Amazon API Gateway"),
    PublishedService::new("appconfig", "AWS AppConfig"),
    PublishedService::new("appfabric", "AWS AppFabric"),
    PublishedService::new("appflow", "Amazon AppFlow"),
    PublishedService::unreported("appintegrations", "Amazon AppIntegrations"),
    PublishedService::new("application-autoscaling", "AWS Application Auto Scaling"),
    // A prefix the Service Authorization Reference still publishes, so a policy can name an
    // action of it, for a service access-advisor data reports no entry for.
    PublishedService::unreported("application-cost-profiler", "AWS Application Cost Profiler Service"),
    PublishedService::new("application-signals", "Amazon CloudWatch Application Signals"),
    PublishedService::new("applicationinsights", "Amazon CloudWatch Application Insights"),
    PublishedService::new("appmesh", "AWS App Mesh"),
    PublishedService::new("apprunner", "AWS App Runner"),
    PublishedService::new("appstream", "Amazon AppStream 2.0"),
    PublishedService::new("appstudio", "AWS App Studio"),
    PublishedService::new("appsync", "AWS AppSync"),
    // Mainframe Modernization is published under two prefixes, one for the service itself
    // (`m2`) and this one for its application-testing capability, each under its own name.
    PublishedService::new("apptest", "AWS Mainframe Modernization Application Testing"),
    PublishedService::new("aps", "Amazon Managed Service for Prometheus"),
    // Application Recovery Controller is published under two prefixes, one per capability,
    // each under its own name.
    PublishedService::new("arc-region-switch", "AWS ARC - Region switch"),
    PublishedService::new("arc-zonal-shift", "AWS ARC - Zonal Shift"),
    PublishedService::new("arsenal", "Application Discovery Arsenal"),
    PublishedService::new("artifact", "AWS Artifact"),
    PublishedService::new("athena", "Amazon Athena"),
    PublishedService::new("auditmanager", "AWS Audit Manager"),
    PublishedService::new("autoscaling", "Amazon EC2 Auto Scaling"),
    PublishedService::new("autoscaling-plans", "AWS Auto Scaling"),
    PublishedService::new("aws-marketplace", "AWS Marketplace"),
    PublishedService::new("aws-portal", "AWS Billing Console"),
    PublishedService::new("b2bi", "AWS B2B Data Interchange"),
    PublishedService::new("backup", "AWS Backup"),
    PublishedService::new("backup-gateway", "AWS Backup Gateway"),
    PublishedService::new("batch", "AWS Batch"),
    PublishedService::new("bedrock", "Amazon Bedrock"),
    PublishedService::new("billing", "AWS Billing"),
    PublishedService::new("billingconductor", "AWS Billing Conductor"),
    PublishedService::new("braket", "Amazon Braket"),
    PublishedService::new("budgets", "AWS Budget Service"),
    PublishedService::new("cases", "Amazon Connect Cases"),
    PublishedService::new("cassandra", "Amazon Keyspaces"),
    PublishedService::new("ce", "AWS Cost Explorer Service"),
    PublishedService::new("chatbot", "AWS Chatbot"),
    PublishedService::new("chime", "Amazon Chime"),
    PublishedService::new("cleanrooms", "AWS Clean Rooms"),
    PublishedService::new("cloud9", "AWS Cloud9"),
    PublishedService::new("cloudcontrolapi", "AWS Cloud Control API"),
    PublishedService::new("clouddirectory", "Amazon Cloud Directory"),
    PublishedService::new("cloudformation", "AWS CloudFormation"),
    PublishedService::new("cloudfront", "Amazon CloudFront"),
    PublishedService::new("cloudfront-keyvaluestore", "Amazon CloudFront KeyValueStore"),
    PublishedService::new("cloudhsm", "AWS CloudHSM"),
    PublishedService::new("cloudsearch", "Amazon CloudSearch"),
    PublishedService::new("cloudshell", "AWS CloudShell"),
    PublishedService::new("cloudtrail", "AWS CloudTrail"),
    PublishedService::new("cloudwatch", "Amazon CloudWatch"),
    PublishedService::new("codeartifact", "AWS CodeArtifact"),
    PublishedService::new("codebuild", "AWS CodeBuild"),
    PublishedService::new("codecatalyst", "Amazon CodeCatalyst"),
    PublishedService::new("codecommit", "AWS CodeCommit"),
    PublishedService::new("codeconnections", "AWS CodeConnections"),
    PublishedService::new("codedeploy", "AWS CodeDeploy"),
    PublishedService::new("codeguru-profiler", "Amazon CodeGuru Profiler"),
    PublishedService::new("codeguru-reviewer", "Amazon CodeGuru Reviewer"),
    PublishedService::new("codeguru-security", "Amazon CodeGuru Security"),
    PublishedService::new("codepipeline", "AWS CodePipeline"),
    PublishedService::new("codestar-connections", "AWS CodeStar Connections"),
    PublishedService::new("codestar-notifications", "AWS CodeStar Notifications"),
    PublishedService::new("cognito-identity", "Amazon Cognito Identity"),
    PublishedService::new("cognito-idp", "Amazon Cognito User Pools"),
    PublishedService::new("cognito-sync", "Amazon Cognito Sync"),
    PublishedService::new("comprehend", "Amazon Comprehend"),
    PublishedService::new("comprehendmedical", "Amazon Comprehend Medical"),
    PublishedService::new("compute-optimizer", "AWS Compute Optimizer"),
    PublishedService::new("config", "AWS Config"),
    PublishedService::new("connect", "Amazon Connect"),
    PublishedService::new("connect-campaigns", "Amazon Connect Outbound Campaigns"),
    PublishedService::new("controlcatalog", "AWS Control Catalog"),
    PublishedService::new("controltower", "AWS Control Tower"),
    PublishedService::new("cost-optimization-hub", "AWS Cost Optimization Hub"),
    PublishedService::new("cur", "AWS Cost and Usage Report"),
    PublishedService::new("databrew", "AWS Glue DataBrew"),
    PublishedService::new("dataexchange", "AWS Data Exchange"),
    PublishedService::new("datapipeline", "AWS Data Pipeline"),
    PublishedService::new("datasync", "AWS DataSync"),
    PublishedService::new("datazone", "Amazon DataZone"),
    PublishedService::new("dax", "Amazon DynamoDB Accelerator (DAX)"),
    PublishedService::new("deadline", "AWS Deadline Cloud"),
    PublishedService::new("deepracer", "AWS DeepRacer"),
    PublishedService::new("detective", "Amazon Detective"),
    PublishedService::new("devicefarm", "AWS Device Farm"),
    PublishedService::new("devops-guru", "Amazon DevOps Guru"),
    PublishedService::new("directconnect", "AWS Direct Connect"),
    PublishedService::new("discovery", "AWS Application Discovery Service"),
    PublishedService::new("dlm", "Amazon Data Lifecycle Manager"),
    PublishedService::new("dms", "AWS Database Migration Service"),
    PublishedService::new("docdb-elastic", "Amazon DocumentDB Elastic Clusters"),
    PublishedService::new("drs", "AWS Elastic Disaster Recovery"),
    PublishedService::new("ds", "AWS Directory Service"),
    PublishedService::new("ds-data", "AWS Directory Service Data"),
    PublishedService::new("dsql", "Amazon Aurora DSQL"),
    PublishedService::new("dynamodb", "Amazon DynamoDB"),
    PublishedService::new("ebs", "Amazon Elastic Block Store"),
    PublishedService::new("ec2", "Amazon Elastic Compute Cloud"),
    PublishedService::new("ec2-instance-connect", "Amazon EC2 Instance Connect"),
    PublishedService::new("ec2messages", "Amazon Message Delivery Service"),
    PublishedService::new("ecr", "Amazon Elastic Container Registry"),
    PublishedService::new("ecr-public", "Amazon Elastic Container Registry Public"),
    PublishedService::new("ecs", "Amazon Elastic Container Service"),
    PublishedService::new("eks", "Amazon Elastic Kubernetes Service"),
    PublishedService::new("eks-auth", "Amazon EKS Auth"),
    PublishedService::new("elasticache", "Amazon ElastiCache"),
    PublishedService::new("elasticbeanstalk", "AWS Elastic Beanstalk"),
    PublishedService::new("elasticfilesystem", "Amazon Elastic File System"),
    PublishedService::new("elasticloadbalancing", "Elastic Load Balancing"),
    PublishedService::new("elasticmapreduce", "Amazon EMR"),
    PublishedService::new("elastictranscoder", "Amazon Elastic Transcoder"),
    PublishedService::new("emr-containers", "Amazon EMR on EKS"),
    PublishedService::new("emr-serverless", "Amazon EMR Serverless"),
    PublishedService::new("entityresolution", "AWS Entity Resolution"),
    PublishedService::new("es", "Amazon OpenSearch Service"),
    PublishedService::new("events", "Amazon EventBridge"),
    PublishedService::new("evidently", "Amazon CloudWatch Evidently"),
    PublishedService::new("evs", "Amazon Elastic VMware Service"),
    PublishedService::new("execute-api", "Amazon API Gateway"),
    PublishedService::new("firehose", "Amazon Data Firehose"),
    PublishedService::new("fis", "AWS Fault Injection Simulator"),
    PublishedService::new("fms", "AWS Firewall Manager"),
    PublishedService::new("forecast", "Amazon Forecast"),
    PublishedService::new("frauddetector", "Amazon Fraud Detector"),
    PublishedService::new("freetier", "AWS Free Tier"),
    PublishedService::new("fsx", "Amazon FSx"),
    PublishedService::new("gamelift", "Amazon GameLift"),
    PublishedService::new("gameliftstreams", "Amazon GameLift Streams"),
    PublishedService::new("geo", "Amazon Location Service"),
    PublishedService::new("geo-maps", "Amazon Location Service Maps"),
    PublishedService::new("geo-places", "Amazon Location Service Places"),
    PublishedService::new("geo-routes", "Amazon Location Service Routes"),
    PublishedService::new("glacier", "Amazon S3 Glacier"),
    PublishedService::new("globalaccelerator", "AWS Global Accelerator"),
    PublishedService::new("glue", "AWS Glue"),
    PublishedService::new("grafana", "Amazon Managed Grafana"),
    PublishedService::new("greengrass", "AWS IoT Greengrass"),
    PublishedService::new("groundstation", "AWS Ground Station"),
    PublishedService::new("guardduty", "Amazon GuardDuty"),
    PublishedService::new("health", "AWS Health APIs and Notifications"),
    PublishedService::new("healthlake", "AWS HealthLake"),
    PublishedService::new("iam", "AWS Identity and Access Management"),
    PublishedService::new("identitystore", "AWS Identity Store"),
    PublishedService::new("imagebuilder", "EC2 Image Builder"),
    PublishedService::new("inspector", "Amazon Inspector"),
    PublishedService::new("inspector-scan", "Amazon Inspector Scan"),
    PublishedService::new("inspector2", "Amazon Inspector2"),
    PublishedService::new("internetmonitor", "Amazon CloudWatch Internet Monitor"),
    PublishedService::new("invoicing", "AWS Invoicing"),
    PublishedService::new("iot", "AWS IoT"),
    PublishedService::new("iotanalytics", "AWS IoT Analytics"),
    PublishedService::new("iotdeviceadvisor", "AWS IoT Core Device Advisor"),
    PublishedService::new("iotevents", "AWS IoT Events"),
    PublishedService::new("iotfleethub", "AWS IoT Fleet Hub for Device Management"),
    PublishedService::new("iotfleetwise", "AWS IoT FleetWise"),
    PublishedService::new("iotsitewise", "AWS IoT SiteWise"),
    PublishedService::new("iottwinmaker", "AWS IoT TwinMaker"),
    PublishedService::new("iotwireless", "AWS IoT Core for LoRaWAN"),
    PublishedService::new("ivs", "Amazon Interactive Video Service"),
    PublishedService::new("ivschat", "Amazon Interactive Video Service Chat"),
    PublishedService::new("kafka", "Amazon Managed Streaming for Apache Kafka"),
    PublishedService::new("kafkaconnect", "Amazon Managed Streaming for Kafka Connect"),
    PublishedService::new("kendra", "Amazon Kendra"),
    PublishedService::new("kinesis", "Amazon Kinesis"),
    PublishedService::new("kinesisanalytics", "Amazon Kinesis Analytics"),
    PublishedService::new("kinesisvideo", "Amazon Kinesis Video Streams"),
    PublishedService::new("kms", "AWS Key Management Service"),
    PublishedService::new("lakeformation", "AWS Lake Formation"),
    PublishedService::new("lambda", "AWS Lambda"),
    PublishedService::new("launchwizard", "AWS Launch Wizard"),
    PublishedService::new("lex", "Amazon Lex"),
    PublishedService::new("license-manager", "AWS License Manager"),
    PublishedService::new("lightsail", "Amazon Lightsail"),
    PublishedService::new("logs", "Amazon CloudWatch Logs"),
    PublishedService::new("lookoutequipment", "Amazon Lookout for Equipment"),
    PublishedService::new("lookoutmetrics", "Amazon Lookout for Metrics"),
    PublishedService::new("lookoutvision", "Amazon Lookout for Vision"),
    PublishedService::new("m2", "AWS Mainframe Modernization Service"),
    PublishedService::new("machinelearning", "Amazon Machine Learning"),
    PublishedService::new("macie2", "Amazon Macie"),
    PublishedService::new("managedblockchain", "Amazon Managed Blockchain"),
    PublishedService::new("mediaconnect", "AWS Elemental MediaConnect"),
    PublishedService::new("mediaconvert", "AWS Elemental MediaConvert"),
    PublishedService::new("medialive", "AWS Elemental MediaLive"),
    PublishedService::new("mediapackage", "AWS Elemental MediaPackage"),
    PublishedService::new("mediapackage-vod", "AWS Elemental MediaPackage VOD"),
    PublishedService::new("mediapackagev2", "AWS Elemental MediaPackage V2"),
    PublishedService::new("mediastore", "AWS Elemental MediaStore"),
    PublishedService::new("mediatailor", "AWS Elemental MediaTailor"),
    PublishedService::new("memorydb", "Amazon MemoryDB"),
    PublishedService::new("mgh", "AWS Migration Hub"),
    PublishedService::new("mgn", "AWS Application Migration Service"),
    PublishedService::new("mobiletargeting", "Amazon Pinpoint"),
    PublishedService::new("monitron", "Amazon Monitron"),
    PublishedService::new("mq", "Amazon MQ"),
    PublishedService::new("neptune-db", "Amazon Neptune"),
    PublishedService::new("neptune-graph", "Amazon Neptune Analytics"),
    PublishedService::new("network-firewall", "AWS Network Firewall"),
    PublishedService::new("networkmanager", "AWS Network Manager"),
    PublishedService::new("nimble", "Amazon Nimble Studio"),
    PublishedService::new("notifications", "AWS User Notifications"),
    PublishedService::new("notifications-contacts", "AWS User Notifications Contacts"),
    PublishedService::new("oam", "Amazon CloudWatch Observability Access Manager"),
    PublishedService::new("observabilityadmin", "Amazon CloudWatch Observability Admin Service"),
    PublishedService::new("omics", "Amazon Omics"),
    PublishedService::new("opsworks", "AWS OpsWorks"),
    PublishedService::new("organizations", "AWS Organizations"),
    PublishedService::new("osis", "Amazon OpenSearch Ingestion"),
    PublishedService::new("outposts", "AWS Outposts"),
    PublishedService::new("panorama", "AWS Panorama"),
    PublishedService::new("payment-cryptography", "AWS Payment Cryptography"),
    PublishedService::new("pcs", "AWS Parallel Computing Service"),
    PublishedService::new("personalize", "Amazon Personalize"),
    PublishedService::new("pi", "AWS Performance Insights"),
    PublishedService::new("pipes", "Amazon EventBridge Pipes"),
    PublishedService::new("polly", "Amazon Polly"),
    PublishedService::new("pricing", "AWS Price List"),
    PublishedService::new("profile", "Amazon Connect Customer Profiles"),
    PublishedService::new("proton", "AWS Proton"),
    PublishedService::new("q", "Amazon Q"),
    PublishedService::new("qapps", "Amazon Q Apps"),
    PublishedService::new("qbusiness", "Amazon Q Business"),
    PublishedService::new("qdeveloper", "Amazon Q Developer"),
    PublishedService::new("qldb", "Amazon QLDB"),
    PublishedService::new("quicksight", "Amazon QuickSight"),
    PublishedService::new("ram", "AWS Resource Access Manager"),
    PublishedService::new("rbin", "Amazon Recycle Bin"),
    PublishedService::new("rds", "Amazon Relational Database Service"),
    PublishedService::new("rds-data", "Amazon RDS Data API"),
    PublishedService::new("redshift", "Amazon Redshift"),
    PublishedService::new("redshift-data", "Amazon Redshift Data API"),
    PublishedService::new("redshift-serverless", "Amazon Redshift Serverless"),
    PublishedService::new("rekognition", "Amazon Rekognition"),
    PublishedService::new("repostspace", "AWS re:Post Private"),
    PublishedService::new("resiliencehub", "AWS Resilience Hub"),
    PublishedService::new("resource-explorer-2", "AWS Resource Explorer"),
    PublishedService::new("resource-groups", "AWS Resource Groups"),
    PublishedService::new("robomaker", "AWS RoboMaker"),
    PublishedService::new("rolesanywhere", "AWS Roles Anywhere"),
    PublishedService::new("route53", "Amazon Route 53"),
    PublishedService::new("route53-recovery-cluster", "Amazon Route 53 Recovery Cluster"),
    PublishedService::new("route53-recovery-control-config", "Amazon Route 53 Recovery Controls"),
    PublishedService::new("route53-recovery-readiness", "Amazon Route 53 Recovery Readiness"),
    PublishedService::new("route53domains", "Amazon Route 53 Domains"),
    PublishedService::new("route53profiles", "Amazon Route 53 Profiles"),
    PublishedService::new("route53resolver", "Amazon Route 53 Resolver"),
    PublishedService::new("rum", "Amazon CloudWatch RUM"),
    PublishedService::new("s3", "Amazon Simple Storage Service"),
    PublishedService::new("s3-object-lambda", "Amazon S3 Object Lambda"),
    PublishedService::new("s3express", "Amazon S3 Express"),
    PublishedService::new("s3outposts", "Amazon S3 on Outposts"),
    PublishedService::new("s3tables", "Amazon S3 Tables"),
    PublishedService::new("sagemaker", "Amazon SageMaker"),
    PublishedService::new("savingsplans", "AWS Savings Plans"),
    PublishedService::new("scheduler", "Amazon EventBridge Scheduler"),
    PublishedService::new("schemas", "Amazon EventBridge Schemas"),
    PublishedService::new("scn", "AWS Supply Chain"),
    PublishedService::new("sdb", "Amazon SimpleDB"),
    PublishedService::new("secretsmanager", "AWS Secrets Manager"),
    PublishedService::new("security-ir", "AWS Security Incident Response"),
    PublishedService::new("securityhub", "AWS Security Hub"),
    PublishedService::new("securitylake", "Amazon Security Lake"),
    PublishedService::new("serverlessrepo", "AWS Serverless Application Repository"),
    PublishedService::new("servicecatalog", "AWS Service Catalog"),
    PublishedService::new("servicediscovery", "AWS Cloud Map"),
    PublishedService::new("servicequotas", "Service Quotas"),
    PublishedService::new("ses", "Amazon Simple Email Service"),
    PublishedService::new("shield", "AWS Shield"),
    PublishedService::new("signer", "AWS Signer"),
    PublishedService::new("signin", "AWS Sign-In"),
    PublishedService::new("simspaceweaver", "AWS SimSpace Weaver"),
    PublishedService::new("sms", "AWS Server Migration Service"),
    PublishedService::new("sms-voice", "Amazon Pinpoint SMS and Voice Service"),
    PublishedService::new("snow-device-management", "AWS Snow Device Management"),
    PublishedService::new("snowball", "AWS Snowball"),
    PublishedService::new("sns", "Amazon Simple Notification Service"),
    PublishedService::new("social-messaging", "AWS End User Messaging Social"),
    PublishedService::new("sqlworkbench", "AWS SQL Workbench"),
    PublishedService::new("sqs", "Amazon Simple Queue Service"),
    PublishedService::new("ssm", "AWS Systems Manager"),
    PublishedService::new("ssm-contacts", "AWS Systems Manager Incident Manager Contacts"),
    PublishedService::new("ssm-guiconnect", "AWS Systems Manager GUI Connect"),
    PublishedService::new("ssm-incidents", "AWS Systems Manager Incident Manager"),
    PublishedService::new("ssm-quicksetup", "AWS Systems Manager Quick Setup"),
    PublishedService::new("ssm-sap", "AWS Systems Manager for SAP"),
    PublishedService::new("sso", "AWS IAM Identity Center"),
    PublishedService::new("sso-directory", "AWS IAM Identity Center Directory"),
    PublishedService::new("states", "AWS Step Functions"),
    PublishedService::new("storagegateway", "AWS Storage Gateway"),
    PublishedService::new("sts", "AWS Security Token Service"),
    PublishedService::new("support", "AWS Support"),
    PublishedService::new("supportapp", "AWS Support App in Slack"),
    PublishedService::new("swf", "Amazon Simple Workflow Service"),
    PublishedService::new("synthetics", "Amazon CloudWatch Synthetics"),
    PublishedService::new("tag", "Amazon Resource Group Tagging API"),
    PublishedService::new("tax", "AWS Tax Settings"),
    PublishedService::new("textract", "Amazon Textract"),
    PublishedService::new("thinclient", "Amazon WorkSpaces Thin Client"),
    PublishedService::new("timestream", "Amazon Timestream"),
    PublishedService::new("timestream-influxdb", "Amazon Timestream InfluxDB"),
    PublishedService::new("transcribe", "Amazon Transcribe"),
    PublishedService::new("transfer", "AWS Transfer Family"),
    PublishedService::new("translate", "Amazon Translate"),
    PublishedService::new("trustedadvisor", "AWS Trusted Advisor"),
    PublishedService::new("vendor-insights", "AWS Marketplace Vendor Insights"),
    PublishedService::new("verified-access", "AWS Verified Access"),
    PublishedService::new("voiceid", "Amazon Connect Voice ID"),
    PublishedService::new("vpc-lattice", "Amazon VPC Lattice"),
    PublishedService::new("waf", "AWS WAF"),
    PublishedService::new("waf-regional", "AWS WAF Regional"),
    PublishedService::new("wafv2", "AWS WAF V2"),
    PublishedService::new("wellarchitected", "AWS Well-Architected Tool"),
    PublishedService::new("wickr", "AWS Wickr"),
    PublishedService::new("wisdom", "Amazon Connect Wisdom"),
    PublishedService::new("workdocs", "Amazon WorkDocs"),
    PublishedService::new("worklink", "Amazon WorkLink"),
    PublishedService::new("workmail", "Amazon WorkMail"),
    PublishedService::new("workspaces", "Amazon WorkSpaces"),
    PublishedService::new("workspaces-web", "Amazon WorkSpaces Web"),
    PublishedService::new("xray", "AWS X-Ray"),
];

/// How many attempts to reach a service this account's records hold: the count
/// `ServiceLastAccessed$TotalAuthenticatedEntities` reports, "the total number of
/// authenticated principals (root user, IAM users, or IAM roles) that have attempted to
/// access the service".
///
/// Nothing in this emulator records a principal reaching a service — no operation writes
/// such a record and none reads one — so the count is the same for every service a report
/// covers, and that count is zero. It is NOT the absence of a count: recorded AWS reports
/// the number for every service of a report, including the ones nobody has reached, which
/// is how a least-privilege consumer tells "nobody used this" from "nothing is known".
pub const RECORDED_SERVICE_ACCESS_ATTEMPTS: i32 = 0;

/// How many services one page of a service-last-accessed report carries when the request
/// names no `MaxItems`.
///
/// A report covers hundreds of services, and a read of one that names no page size is
/// served this many entries and told, through `IsTruncated` and a `Marker`, that the rest
/// remain. `MaxItems`'s own documentation says an omitted value "defaults to 100", which is
/// the default every entity listing of this service takes ([`super::DEFAULT_MAX_ITEMS`]);
/// recorded AWS answers a read of THIS report that names no size with the page below
/// instead, so the report carries its own page size rather than that one. The recorded read
/// it is taken from is the one the report's own issue cites
/// (`issues/things-that-now-work/20261110T144755Z-iam-getservicelastaccesseddetails-ignores-maxitems-and-serves-a-service.md`).
/// A size the caller names bounds the page instead, up to the thousand `maxItemsType`
/// admits.
pub const SERVICE_LAST_ACCESSED_PAGE_SIZE: i32 = 200;

/// The catalog entry for `service_namespace`, or `None` when [`PUBLISHED_SERVICES`] carries
/// no service under that namespace.
///
/// Every published prefix, whether or not last-accessed data reports an entry for it: this
/// is the lookup a read taking a `ServiceNamespace` decides on, and a policy can name an
/// action of any of them.
///
/// A service prefix is published in lower case and an action element may spell it in any
/// (the comparison [`grants_service_access`] makes is case-insensitive too), so the lookup
/// folds case: two spellings of one namespace name one service.
pub fn published_service(service_namespace: &str) -> Option<&'static PublishedService> {
    PUBLISHED_SERVICES
        .iter()
        .find(|service| service.service_namespace.eq_ignore_ascii_case(service_namespace))
}

/// The services a service-last-accessed REPORT enumerates: the rows of
/// [`PUBLISHED_SERVICES`] last-accessed data reports an entry for, in that catalog's own
/// ascending namespace order, which is the order "the list is sorted by".
///
/// The one definition of the report's set, so the enumeration and every expectation about
/// it read the same subset: a prefix the catalog carries without that data covering it is
/// accepted by a read that NAMES it ([`published_service`]) and enumerated by no report.
pub fn last_accessed_services() -> impl ::std::iter::Iterator<Item = &'static PublishedService>
{
    PUBLISHED_SERVICES
        .iter()
        .filter(|service| service.reports_last_accessed)
}

/// The message a read asked for ONE service's last accessed data under a namespace no
/// service of [`PUBLISHED_SERVICES`] is published under is refused with — the
/// `InvalidInputException` message, whose fixed text is `Service namepace ` and ` is either
/// invalid or unsupported.` (IAM's own spelling of the word) around the namespace the
/// request supplied.
///
/// The one wording for it, so every read taking a single `ServiceNamespace` to name the
/// service it reports on describes an unusable namespace the same way.
pub fn unsupported_service_namespace(service_namespace: &str) -> String {
    format!("Service namepace {service_namespace} is either invalid or unsupported.")
}

/// Every namespace of `service_namespaces` that no service of [`PUBLISHED_SERVICES`] is
/// published under, in the order the request named them.
///
/// `serviceNamespaceType`'s pattern (`^[\w-]*$`) admits any word, so a value that passes
/// it still has to NAME a service: the namespaces are AWS's own published prefixes, and a
/// value that is not one of them names no service whose access could be reported on. The
/// comparison is the catalog's own ([`published_service`]), over every prefix it publishes
/// rather than the subset a report enumerates.
pub fn unknown_service_namespaces<'a, I>(service_namespaces: I) -> Vec<&'a str>
where
    I: IntoIterator<Item = &'a str>,
{
    service_namespaces
        .into_iter()
        .filter(|service_namespace| published_service(service_namespace).is_none())
        .collect()
}

/// The message a call asked about service namespaces IAM publishes no service under is
/// refused with: the fixed text `Invalid service namespace: ` followed by the rejected
/// namespaces inside square brackets, separated by `, ` — every one of them, in the order
/// the request named them, so a caller that mistyped two learns about both.
///
/// The one wording for it, so every member taking a LIST of namespaces to name the
/// services it asks about describes an unusable namespace the same way (the reads taking a
/// single `ServiceNamespace` to name the service they report ON state
/// [`unsupported_service_namespace`] instead).
pub fn invalid_service_namespaces(rejected: &[&str]) -> String {
    format!("Invalid service namespace: [{}]", rejected.join(", "))
}
