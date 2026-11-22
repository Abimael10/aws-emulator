// SPDX-License-Identifier: CC0-1.0

//! The Amazon RDS half of the SDK seam a RUNNING handler calls another service through: the
//! DB instance or cluster a handler reads, the MASTER CREDENTIAL it changes on one, and the
//! Amazon S3 EXPORT of a snapshot it starts, reads and cancels together with the IAM role
//! associations that gate a database's S3 integration.
//!
//! CHANGING A DATABASE'S OWN CREDENTIAL is what a handler at the end of a credential
//! rotation does: the rotation function AWS Secrets Manager invokes stages a new password in
//! `createSecret` and then puts it in force in the database in `setSecret` — which for an
//! RDS master credential is `ModifyDBInstance` (or `ModifyDBCluster`) on the instance the
//! secret belongs to, the one way to change that credential without a database connection of
//! its own. A handler that cannot reach this service cannot carry out that step at all, so
//! the rotation it is driven through never finishes.
//!
//! READING WHAT THE SERVICE OFFERS is the shape of a handler that decides before it acts: the
//! engine versions an account can launch and the configurations each of them can be launched in
//! are reference data the service publishes, and a periodic or scheduled handler reads them to
//! report what has become available. A handler that cannot reach this service reads none of it.
//!
//! EXPORTING A DATABASE'S DATA TO AMAZON S3 is the third shape, and the one a CloudFormation
//! custom resource's provider takes: the handler starts an export task for a snapshot, polls it
//! to a terminal status, cancels one that is no longer wanted, and associates or drops the IAM
//! role under which the engine reaches Amazon S3. A handler that cannot reach this service can
//! drive none of it.
//!
//! READING THE PROXY A CONNECTION GOES THROUGH is the fourth: an application that connects to
//! its database through RDS Proxy confirms its own wiring before it dials, which is reading the
//! proxy (the secret each `Auth` entry names as the credential the proxy logs in with, and the
//! address and the status the proxy publishes), the target group behind it, the endpoints it
//! answers on, and the health of each registered target. A handler that cannot reach this
//! service reads none of that, so it cannot tell a proxy it may connect through from one whose
//! targets are unhealthy.
//!
//! Most services a handler reaches bind their requests with an AWS JSON protocol, where the
//! request body IS the operation's input document, so one code path serves every operation of
//! every one of them ([`super::aws_api`]). This service does not: its model declares
//! `awsQuery` (`smithy_query(query="service_info")`), so a request to it is a form-encoded
//! `Action` with the input flattened into the parameters beside it, and the answer is an XML
//! document. That binding is not written out here: the request is built, signed, dispatched
//! and parsed by the CLIENT this crate holds (`aws_sdk_rds`), over the same in-process
//! callback transport every other call of this seam uses, so the wire details are the
//! client's and the answer is the emulated Amazon RDS service's own.
//!
//! What a guest can and cannot do here:
//!
//! * The destination is ALWAYS this emulator: the client is given the callback's own
//!   transport and the service's own regional endpoint, so no host name is resolved and no
//!   socket is opened.
//! * The call is signed as the identity the client was built with — the session of the
//!   function's execution role, or the credentials the handler passed that client
//!   ([`super::aws_api::AwsAccess::signing_as`]) — so Amazon RDS judges it against the
//!   policies in force on that identity exactly as it judges any other signed request.
//! * Only the operations in [`BOUND_OPERATIONS`] are bound, and only the input members each
//!   entry names. An operation or a member this seam does not bind is reported as a call that
//!   was NOT MADE, naming it, rather than sent with a value dropped — a dropped member would
//!   make a call that did something other than what the handler asked for.
//! * The answer's own status and document are what Amazon RDS answered: a success carries the
//!   output members this module reads off it ([`db_instance_document`],
//!   [`db_cluster_document`] and the export task's own arms name every one of them), and a
//!   refusal is that service's own error code, message and status, which the SDK the guest
//!   calls raises as `ClientError` ([`super::aws_sdk_guest`]).
//!
//! How one call's input is read, what the answer carried and how a refusal is handed back are
//! the same for every service reached through an SDK client, so they are [`super::sdk_call`]'s
//! rather than this module's.

use ::aws_sdk_rds::Client;
use ::serde_json::{Map, Value};
use ::smithy_server_runtime::EmbeddedEmulator;

use super::aws_api::{Answer, CallerIdentity, Refusal};
use super::sdk_call::{
    insert_double, insert_flag, insert_number, insert_text, insert_timestamp, Input, Observed,
    PROVIDER_NAME,
};

/// The service a refusal of one of these calls names, as the AWS SDKs name it.
const SERVICE: &str = "Amazon RDS";

/// The operations of Amazon RDS this seam binds, in the order a refusal lists them.
///
/// One entry per operation [`call`] builds a request for: the surface a handler that manages
/// a database credential uses — reading the DB instance or DB cluster the credential belongs
/// to, and changing that resource's own configuration, which is how the master credential is
/// put in force (`MasterUserPassword` / `RotateMasterUserPassword` on either modify) — the
/// CATALOGUE a handler reads to learn what the service offers before it acts on a database (the
/// engine versions it can launch and the configurations each can be launched in) — and the
/// surface a handler that drives the database's Amazon S3 integration uses: the export task it
/// starts, reads and cancels, and the role associations that let the engine reach Amazon S3 at
/// all — and the PROXY READS an application makes to confirm the wiring of the connection it is
/// about to open: the proxy itself, its target group, its endpoints and the health of each
/// registered target. A handler naming any other operation of the service is told the call was
/// not made and why, because a request this module cannot bind is one it would otherwise be
/// guessing at.
pub const BOUND_OPERATIONS: &[&str] = &[
    "DescribeDBInstances",
    "ModifyDBInstance",
    "DescribeDBClusters",
    "ModifyDBCluster",
    "DescribeDBEngineVersions",
    "DescribeOrderableDBInstanceOptions",
    "DescribeDBProxies",
    "DescribeDBProxyTargets",
    "DescribeDBProxyTargetGroups",
    "DescribeDBProxyEndpoints",
    "StartExportTask",
    "DescribeExportTasks",
    "CancelExportTask",
    "AddRoleToDBInstance",
    "RemoveRoleFromDBInstance",
];

/// Make ONE Amazon RDS call: `operation`, with `body` as the operation's input document (the
/// keyword arguments the handler passed its client method), in `region`.
pub async fn call(
    callback: &EmbeddedEmulator,
    identity: Option<&CallerIdentity>,
    region: &str,
    operation: &str,
    body: &str,
    chain: Option<&str>,
) -> Result<Answer, Refusal> {
    let input = Input::read(SERVICE, body)?;
    let observed = Observed::of(SERVICE);
    let client = client(callback, identity, region, observed.clone(), chain);
    match bound_operation(operation) {
        Some("DescribeDBInstances") => describe_db_instances(&client, &input, &observed).await,
        Some("ModifyDBInstance") => modify_db_instance(&client, &input, &observed).await,
        Some("DescribeDBClusters") => describe_db_clusters(&client, &input, &observed).await,
        Some("ModifyDBCluster") => modify_db_cluster(&client, &input, &observed).await,
        Some("DescribeDBEngineVersions") => {
            describe_db_engine_versions(&client, &input, &observed).await
        }
        Some("DescribeOrderableDBInstanceOptions") => {
            describe_orderable_db_instance_options(&client, &input, &observed).await
        }
        Some("DescribeDBProxies") => describe_db_proxies(&client, &input, &observed).await,
        Some("DescribeDBProxyTargets") => {
            describe_db_proxy_targets(&client, &input, &observed).await
        }
        Some("DescribeDBProxyTargetGroups") => {
            describe_db_proxy_target_groups(&client, &input, &observed).await
        }
        Some("DescribeDBProxyEndpoints") => {
            describe_db_proxy_endpoints(&client, &input, &observed).await
        }
        Some("StartExportTask") => start_export_task(&client, &input, &observed).await,
        Some("DescribeExportTasks") => describe_export_tasks(&client, &input, &observed).await,
        Some("CancelExportTask") => cancel_export_task(&client, &input, &observed).await,
        Some("AddRoleToDBInstance") => add_role_to_db_instance(&client, &input, &observed).await,
        Some("RemoveRoleFromDBInstance") => {
            remove_role_from_db_instance(&client, &input, &observed).await
        }
        _ => Err(Refusal::NotMade(format!(
            "the AWS SDK this runtime supplies binds no {operation} call on {SERVICE}; the \
             operations it binds are {}",
            BOUND_OPERATIONS.join(", ")
        ))),
    }
}

/// Which bound operation `asked` names, or `None` for one this seam does not bind.
///
/// Matched without regard to CASE, because the name the seam is handed is the one the SDK the
/// guest calls derived from the method it was called by (`describe_db_instances`), and where
/// an operation's own name carries an acronym — every `DB` in this service's — that spelling
/// is not recoverable from the method name. The canonical spelling is this service's own, and
/// it is what reaches the wire: the request is built by the client below from the operation
/// the arm names.
fn bound_operation(asked: &str) -> Option<&'static str> {
    BOUND_OPERATIONS
        .iter()
        .copied()
        .find(|bound| bound.eq_ignore_ascii_case(asked))
}

/// An Amazon RDS client over the in-process callback into this same emulator, signing as the
/// identity the SDK client the guest built was given.
///
/// The transport is in-process, so everything a network client needs and this one cannot have —
/// retries, timeouts, identity caching and stalled-stream protection, each of which needs a
/// sleep implementation the callback transport is not given — is switched off.
///
/// Every request it makes carries the tracing header of the chain this invocation belongs to,
/// where the invocation has one ([`::service_common::xray_trace::ChainHop`]): a call the
/// handler makes is one HOP of the chain that reached the handler.
fn client(
    callback: &EmbeddedEmulator,
    identity: Option<&CallerIdentity>,
    region: &str,
    observed: Observed,
    chain: Option<&str>,
) -> Client {
    let config = ::aws_sdk_rds::Config::builder()
        .behavior_version(::aws_sdk_rds::config::BehaviorVersion::latest())
        .region(::aws_sdk_rds::config::Region::new(region.to_string()))
        .endpoint_url(format!("https://rds.{region}.amazonaws.com"))
        .http_client(callback.http_client())
        .interceptor(observed)
        .retry_config(::aws_sdk_rds::config::retry::RetryConfig::disabled())
        .timeout_config(::aws_sdk_rds::config::timeout::TimeoutConfig::disabled())
        .identity_cache(::aws_sdk_rds::config::IdentityCache::no_cache())
        .stalled_stream_protection(
            ::aws_sdk_rds::config::StalledStreamProtectionConfig::disabled(),
        );
    let config = match identity {
        Some(identity) => config.credentials_provider(::aws_sdk_rds::config::Credentials::new(
            identity.access_key_id.clone(),
            identity.secret_access_key.clone(),
            identity.token().map(str::to_string),
            None,
            PROVIDER_NAME,
        )),
        None => config.credentials_provider(callback.aws_credentials_provider()),
    };
    // A call the handler makes is one HOP of the chain that reached the handler.
    let config = match chain {
        Some(chain) => config.interceptor(::service_common::xray_trace::ChainHop::carrying(chain)),
        None => config,
    };
    Client::from_conf(config.build())
}

/// `StartExportTask`: the snapshot or cluster data a task exports to an Amazon S3 bucket.
async fn start_export_task(
    client: &Client,
    input: &Input,
    observed: &Observed,
) -> Result<Answer, Refusal> {
    input.only(&[
        "ExportTaskIdentifier",
        "SourceArn",
        "S3BucketName",
        "IamRoleArn",
        "KmsKeyId",
        "S3Prefix",
        "ExportOnly",
    ])?;
    let started = client
        .start_export_task()
        .export_task_identifier(input.required_text("ExportTaskIdentifier")?)
        .source_arn(input.required_text("SourceArn")?)
        .s3_bucket_name(input.required_text("S3BucketName")?)
        .iam_role_arn(input.required_text("IamRoleArn")?)
        .kms_key_id(input.required_text("KmsKeyId")?)
        .set_s3_prefix(input.text("S3Prefix")?)
        .set_export_only(some_list(input.text_list("ExportOnly")?))
        .send()
        .await;
    let started = match started {
        Ok(started) => started,
        Err(error) => return observed.refused(&error),
    };
    let mut document = Map::new();
    insert_text(
        &mut document,
        "ExportTaskIdentifier",
        started.export_task_identifier(),
    );
    insert_text(&mut document, "SourceArn", started.source_arn());
    insert_list(&mut document, "ExportOnly", started.export_only());
    insert_timestamp(&mut document, "SnapshotTime", started.snapshot_time());
    insert_timestamp(&mut document, "TaskStartTime", started.task_start_time());
    insert_timestamp(&mut document, "TaskEndTime", started.task_end_time());
    insert_text(&mut document, "S3Bucket", started.s3_bucket());
    insert_text(&mut document, "S3Prefix", started.s3_prefix());
    insert_text(&mut document, "IamRoleArn", started.iam_role_arn());
    insert_text(&mut document, "KmsKeyId", started.kms_key_id());
    insert_text(&mut document, "Status", started.status());
    insert_number(
        &mut document,
        "PercentProgress",
        started.percent_progress().map(i64::from),
    );
    insert_number(
        &mut document,
        "TotalExtractedDataInGB",
        started.total_extracted_data_in_gb().map(i64::from),
    );
    insert_text(&mut document, "FailureCause", started.failure_cause());
    insert_text(&mut document, "WarningMessage", started.warning_message());
    insert_text(
        &mut document,
        "SourceType",
        started.source_type().map(|kind| kind.as_str()),
    );
    observed.answer(document)
}

/// `DescribeExportTasks`: the export tasks of this account, and what each one has reached.
async fn describe_export_tasks(
    client: &Client,
    input: &Input,
    observed: &Observed,
) -> Result<Answer, Refusal> {
    input.only(&[
        "ExportTaskIdentifier",
        "SourceArn",
        "Marker",
        "MaxRecords",
        "SourceType",
    ])?;
    let described = client
        .describe_export_tasks()
        .set_export_task_identifier(input.text("ExportTaskIdentifier")?)
        .set_source_arn(input.text("SourceArn")?)
        .set_marker(input.text("Marker")?)
        .set_max_records(input.integer("MaxRecords")?)
        .set_source_type(
            input
                .text("SourceType")?
                .map(|kind| ::aws_sdk_rds::types::ExportSourceType::from(kind.as_str())),
        )
        .send()
        .await;
    let described = match described {
        Ok(described) => described,
        Err(error) => return observed.refused(&error),
    };
    let mut document = Map::new();
    insert_text(&mut document, "Marker", described.marker());
    let tasks: Vec<Value> = described
        .export_tasks()
        .iter()
        .map(|task| {
            let mut reported = Map::new();
            insert_text(
                &mut reported,
                "ExportTaskIdentifier",
                task.export_task_identifier(),
            );
            insert_text(&mut reported, "SourceArn", task.source_arn());
            insert_list(&mut reported, "ExportOnly", task.export_only());
            insert_timestamp(&mut reported, "SnapshotTime", task.snapshot_time());
            insert_timestamp(&mut reported, "TaskStartTime", task.task_start_time());
            insert_timestamp(&mut reported, "TaskEndTime", task.task_end_time());
            insert_text(&mut reported, "S3Bucket", task.s3_bucket());
            insert_text(&mut reported, "S3Prefix", task.s3_prefix());
            insert_text(&mut reported, "IamRoleArn", task.iam_role_arn());
            insert_text(&mut reported, "KmsKeyId", task.kms_key_id());
            insert_text(&mut reported, "Status", task.status());
            insert_number(
                &mut reported,
                "PercentProgress",
                task.percent_progress().map(i64::from),
            );
            insert_number(
                &mut reported,
                "TotalExtractedDataInGB",
                task.total_extracted_data_in_gb().map(i64::from),
            );
            insert_text(&mut reported, "FailureCause", task.failure_cause());
            insert_text(&mut reported, "WarningMessage", task.warning_message());
            insert_text(
                &mut reported,
                "SourceType",
                task.source_type().map(|kind| kind.as_str()),
            );
            Value::Object(reported)
        })
        .collect();
    document.insert("ExportTasks".to_string(), Value::Array(tasks));
    observed.answer(document)
}

/// `CancelExportTask`: the export task a handler stops before it finished.
async fn cancel_export_task(
    client: &Client,
    input: &Input,
    observed: &Observed,
) -> Result<Answer, Refusal> {
    input.only(&["ExportTaskIdentifier"])?;
    let cancelled = client
        .cancel_export_task()
        .export_task_identifier(input.required_text("ExportTaskIdentifier")?)
        .send()
        .await;
    let cancelled = match cancelled {
        Ok(cancelled) => cancelled,
        Err(error) => return observed.refused(&error),
    };
    let mut document = Map::new();
    insert_text(
        &mut document,
        "ExportTaskIdentifier",
        cancelled.export_task_identifier(),
    );
    insert_text(&mut document, "SourceArn", cancelled.source_arn());
    insert_list(&mut document, "ExportOnly", cancelled.export_only());
    insert_timestamp(&mut document, "SnapshotTime", cancelled.snapshot_time());
    insert_timestamp(&mut document, "TaskStartTime", cancelled.task_start_time());
    insert_timestamp(&mut document, "TaskEndTime", cancelled.task_end_time());
    insert_text(&mut document, "S3Bucket", cancelled.s3_bucket());
    insert_text(&mut document, "S3Prefix", cancelled.s3_prefix());
    insert_text(&mut document, "IamRoleArn", cancelled.iam_role_arn());
    insert_text(&mut document, "KmsKeyId", cancelled.kms_key_id());
    insert_text(&mut document, "Status", cancelled.status());
    insert_number(
        &mut document,
        "PercentProgress",
        cancelled.percent_progress().map(i64::from),
    );
    insert_number(
        &mut document,
        "TotalExtractedDataInGB",
        cancelled.total_extracted_data_in_gb().map(i64::from),
    );
    insert_text(&mut document, "FailureCause", cancelled.failure_cause());
    insert_text(&mut document, "WarningMessage", cancelled.warning_message());
    insert_text(
        &mut document,
        "SourceType",
        cancelled.source_type().map(|kind| kind.as_str()),
    );
    observed.answer(document)
}

/// `AddRoleToDBInstance`: the IAM role a DB instance's engine acts under for one feature.
///
/// The operation answers no output members at all, so a served call reports the empty
/// document the SDK gives a handler for one.
async fn add_role_to_db_instance(
    client: &Client,
    input: &Input,
    observed: &Observed,
) -> Result<Answer, Refusal> {
    input.only(&["DBInstanceIdentifier", "RoleArn", "FeatureName"])?;
    let added = client
        .add_role_to_db_instance()
        .db_instance_identifier(input.required_text("DBInstanceIdentifier")?)
        .role_arn(input.required_text("RoleArn")?)
        .feature_name(input.required_text("FeatureName")?)
        .send()
        .await;
    match added {
        Ok(_) => observed.answer(Map::new()),
        Err(error) => observed.refused(&error),
    }
}

/// `RemoveRoleFromDBInstance`: the same association, dropped.
async fn remove_role_from_db_instance(
    client: &Client,
    input: &Input,
    observed: &Observed,
) -> Result<Answer, Refusal> {
    input.only(&["DBInstanceIdentifier", "RoleArn", "FeatureName"])?;
    let removed = client
        .remove_role_from_db_instance()
        .db_instance_identifier(input.required_text("DBInstanceIdentifier")?)
        .role_arn(input.required_text("RoleArn")?)
        .feature_name(input.required_text("FeatureName")?)
        .send()
        .await;
    match removed {
        Ok(_) => observed.answer(Map::new()),
        Err(error) => observed.refused(&error),
    }
}

/// A list member as the SDK's setter takes it: `None` where the call carried none, so a
/// member the handler left out is left out of the request rather than sent empty.
fn some_list(values: Vec<String>) -> Option<Vec<String>> {
    (!values.is_empty()).then_some(values)
}

/// Put a list-of-text member on an answer's document, where the answer carried entries.
fn insert_list(document: &mut Map<String, Value>, name: &str, values: &[String]) {
    if values.is_empty() {
        return;
    }
    document.insert(
        name.to_string(),
        Value::Array(
            values
                .iter()
                .map(|value| Value::String(value.clone()))
                .collect(),
        ),
    );
}

/// `DescribeDBInstances`: the DB instances the account holds, or the one the call names.
async fn describe_db_instances(
    client: &Client,
    input: &Input,
    observed: &Observed,
) -> Result<Answer, Refusal> {
    input.only(&["DBInstanceIdentifier", "Filters", "MaxRecords", "Marker"])?;
    let described = client
        .describe_db_instances()
        .set_db_instance_identifier(input.text("DBInstanceIdentifier")?)
        .set_filters(filters(input)?)
        .set_max_records(input.integer("MaxRecords")?)
        .set_marker(input.text("Marker")?)
        .send()
        .await;
    let described = match described {
        Ok(described) => described,
        Err(error) => return observed.refused(&error),
    };
    let mut document = Map::new();
    insert_text(&mut document, "Marker", described.marker());
    document.insert(
        "DBInstances".to_string(),
        Value::Array(
            described
                .db_instances()
                .iter()
                .map(db_instance_document)
                .collect(),
        ),
    );
    observed.answer(document)
}

/// `ModifyDBInstance`: the declared change to one DB instance, which is where a new master
/// credential is put in force.
async fn modify_db_instance(
    client: &Client,
    input: &Input,
    observed: &Observed,
) -> Result<Answer, Refusal> {
    input.only(&[
        "DBInstanceIdentifier",
        "ApplyImmediately",
        "MasterUserPassword",
        "ManageMasterUserPassword",
        "RotateMasterUserPassword",
        "MasterUserSecretKmsKeyId",
        "NewDBInstanceIdentifier",
        "DBInstanceClass",
        "AllocatedStorage",
        "MaxAllocatedStorage",
        "StorageType",
        "Iops",
        "BackupRetentionPeriod",
        "PreferredBackupWindow",
        "PreferredMaintenanceWindow",
        "MultiAZ",
        "Engine",
        "EngineVersion",
        "AllowMajorVersionUpgrade",
        "AutoMinorVersionUpgrade",
        "LicenseModel",
        "OptionGroupName",
        "DBParameterGroupName",
        "DBSubnetGroupName",
        "VpcSecurityGroupIds",
        "DBPortNumber",
        "PubliclyAccessible",
        "NetworkType",
        "CACertificateIdentifier",
        "CertificateRotationRestart",
        "CopyTagsToSnapshot",
        "DeletionProtection",
        "DedicatedLogVolume",
        "EnableIAMDatabaseAuthentication",
        "EnablePerformanceInsights",
        "PerformanceInsightsKMSKeyId",
        "PerformanceInsightsRetentionPeriod",
        "MonitoringInterval",
        "MonitoringRoleArn",
        "PromotionTier",
    ])?;
    let modified = client
        .modify_db_instance()
        .db_instance_identifier(input.required_text("DBInstanceIdentifier")?)
        .set_apply_immediately(input.boolean("ApplyImmediately")?)
        .set_master_user_password(input.text("MasterUserPassword")?)
        .set_manage_master_user_password(input.boolean("ManageMasterUserPassword")?)
        .set_rotate_master_user_password(input.boolean("RotateMasterUserPassword")?)
        .set_master_user_secret_kms_key_id(input.text("MasterUserSecretKmsKeyId")?)
        .set_new_db_instance_identifier(input.text("NewDBInstanceIdentifier")?)
        .set_db_instance_class(input.text("DBInstanceClass")?)
        .set_allocated_storage(input.integer("AllocatedStorage")?)
        .set_max_allocated_storage(input.integer("MaxAllocatedStorage")?)
        .set_storage_type(input.text("StorageType")?)
        .set_iops(input.integer("Iops")?)
        .set_backup_retention_period(input.integer("BackupRetentionPeriod")?)
        .set_preferred_backup_window(input.text("PreferredBackupWindow")?)
        .set_preferred_maintenance_window(input.text("PreferredMaintenanceWindow")?)
        .set_multi_az(input.boolean("MultiAZ")?)
        .set_engine(input.text("Engine")?)
        .set_engine_version(input.text("EngineVersion")?)
        .set_allow_major_version_upgrade(input.boolean("AllowMajorVersionUpgrade")?)
        .set_auto_minor_version_upgrade(input.boolean("AutoMinorVersionUpgrade")?)
        .set_license_model(input.text("LicenseModel")?)
        .set_option_group_name(input.text("OptionGroupName")?)
        .set_db_parameter_group_name(input.text("DBParameterGroupName")?)
        .set_db_subnet_group_name(input.text("DBSubnetGroupName")?)
        .set_vpc_security_group_ids(optional_list(input.text_list("VpcSecurityGroupIds")?))
        .set_db_port_number(input.integer("DBPortNumber")?)
        .set_publicly_accessible(input.boolean("PubliclyAccessible")?)
        .set_network_type(input.text("NetworkType")?)
        .set_ca_certificate_identifier(input.text("CACertificateIdentifier")?)
        .set_certificate_rotation_restart(input.boolean("CertificateRotationRestart")?)
        .set_copy_tags_to_snapshot(input.boolean("CopyTagsToSnapshot")?)
        .set_deletion_protection(input.boolean("DeletionProtection")?)
        .set_dedicated_log_volume(input.boolean("DedicatedLogVolume")?)
        .set_enable_iam_database_authentication(input.boolean("EnableIAMDatabaseAuthentication")?)
        .set_enable_performance_insights(input.boolean("EnablePerformanceInsights")?)
        .set_performance_insights_kms_key_id(input.text("PerformanceInsightsKMSKeyId")?)
        .set_performance_insights_retention_period(
            input.integer("PerformanceInsightsRetentionPeriod")?,
        )
        .set_monitoring_interval(input.integer("MonitoringInterval")?)
        .set_monitoring_role_arn(input.text("MonitoringRoleArn")?)
        .set_promotion_tier(input.integer("PromotionTier")?)
        .send()
        .await;
    let modified = match modified {
        Ok(modified) => modified,
        Err(error) => return observed.refused(&error),
    };
    let mut document = Map::new();
    if let Some(instance) = modified.db_instance() {
        document.insert("DBInstance".to_string(), db_instance_document(instance));
    }
    observed.answer(document)
}

/// `DescribeDBClusters`: the DB clusters the account holds, or the one the call names.
async fn describe_db_clusters(
    client: &Client,
    input: &Input,
    observed: &Observed,
) -> Result<Answer, Refusal> {
    input.only(&["DBClusterIdentifier", "Filters", "MaxRecords", "Marker"])?;
    let described = client
        .describe_db_clusters()
        .set_db_cluster_identifier(input.text("DBClusterIdentifier")?)
        .set_filters(filters(input)?)
        .set_max_records(input.integer("MaxRecords")?)
        .set_marker(input.text("Marker")?)
        .send()
        .await;
    let described = match described {
        Ok(described) => described,
        Err(error) => return observed.refused(&error),
    };
    let mut document = Map::new();
    insert_text(&mut document, "Marker", described.marker());
    document.insert(
        "DBClusters".to_string(),
        Value::Array(
            described
                .db_clusters()
                .iter()
                .map(db_cluster_document)
                .collect(),
        ),
    );
    observed.answer(document)
}

/// `ModifyDBCluster`: the same change to a CLUSTER's own configuration, which is where an
/// Aurora master credential is put in force.
async fn modify_db_cluster(
    client: &Client,
    input: &Input,
    observed: &Observed,
) -> Result<Answer, Refusal> {
    input.only(&[
        "DBClusterIdentifier",
        "ApplyImmediately",
        "MasterUserPassword",
        "ManageMasterUserPassword",
        "RotateMasterUserPassword",
        "MasterUserSecretKmsKeyId",
        "NewDBClusterIdentifier",
        "BackupRetentionPeriod",
        "PreferredBackupWindow",
        "PreferredMaintenanceWindow",
        "EngineVersion",
        "AllowMajorVersionUpgrade",
        "Port",
        "StorageType",
        "AllocatedStorage",
        "Iops",
        "DBClusterParameterGroupName",
        "VpcSecurityGroupIds",
        "CopyTagsToSnapshot",
        "DeletionProtection",
        "EnableIAMDatabaseAuthentication",
    ])?;
    let modified = client
        .modify_db_cluster()
        .db_cluster_identifier(input.required_text("DBClusterIdentifier")?)
        .set_apply_immediately(input.boolean("ApplyImmediately")?)
        .set_master_user_password(input.text("MasterUserPassword")?)
        .set_manage_master_user_password(input.boolean("ManageMasterUserPassword")?)
        .set_rotate_master_user_password(input.boolean("RotateMasterUserPassword")?)
        .set_master_user_secret_kms_key_id(input.text("MasterUserSecretKmsKeyId")?)
        .set_new_db_cluster_identifier(input.text("NewDBClusterIdentifier")?)
        .set_backup_retention_period(input.integer("BackupRetentionPeriod")?)
        .set_preferred_backup_window(input.text("PreferredBackupWindow")?)
        .set_preferred_maintenance_window(input.text("PreferredMaintenanceWindow")?)
        .set_engine_version(input.text("EngineVersion")?)
        .set_allow_major_version_upgrade(input.boolean("AllowMajorVersionUpgrade")?)
        .set_port(input.integer("Port")?)
        .set_storage_type(input.text("StorageType")?)
        .set_allocated_storage(input.integer("AllocatedStorage")?)
        .set_iops(input.integer("Iops")?)
        .set_db_cluster_parameter_group_name(input.text("DBClusterParameterGroupName")?)
        .set_vpc_security_group_ids(optional_list(input.text_list("VpcSecurityGroupIds")?))
        .set_copy_tags_to_snapshot(input.boolean("CopyTagsToSnapshot")?)
        .set_deletion_protection(input.boolean("DeletionProtection")?)
        .set_enable_iam_database_authentication(input.boolean("EnableIAMDatabaseAuthentication")?)
        .send()
        .await;
    let modified = match modified {
        Ok(modified) => modified,
        Err(error) => return observed.refused(&error),
    };
    let mut document = Map::new();
    if let Some(cluster) = modified.db_cluster() {
        document.insert("DBCluster".to_string(), db_cluster_document(cluster));
    }
    observed.answer(document)
}

/// `DescribeDBEngineVersions`: the engine versions this account can launch, which is the
/// CATALOGUE a handler reads to learn what the service offers before it acts on a database.
async fn describe_db_engine_versions(
    client: &Client,
    input: &Input,
    observed: &Observed,
) -> Result<Answer, Refusal> {
    input.only(&[
        "Engine",
        "EngineVersion",
        "DBParameterGroupFamily",
        "Filters",
        "MaxRecords",
        "Marker",
        "DefaultOnly",
        "ListSupportedCharacterSets",
        "ListSupportedTimezones",
        "IncludeAll",
    ])?;
    let described = client
        .describe_db_engine_versions()
        .set_engine(input.text("Engine")?)
        .set_engine_version(input.text("EngineVersion")?)
        .set_db_parameter_group_family(input.text("DBParameterGroupFamily")?)
        .set_filters(filters(input)?)
        .set_max_records(input.integer("MaxRecords")?)
        .set_marker(input.text("Marker")?)
        .set_default_only(input.boolean("DefaultOnly")?)
        .set_list_supported_character_sets(input.boolean("ListSupportedCharacterSets")?)
        .set_list_supported_timezones(input.boolean("ListSupportedTimezones")?)
        .set_include_all(input.boolean("IncludeAll")?)
        .send()
        .await;
    let described = match described {
        Ok(described) => described,
        Err(error) => return observed.refused(&error),
    };
    let mut document = Map::new();
    insert_text(&mut document, "Marker", described.marker());
    document.insert(
        "DBEngineVersions".to_string(),
        Value::Array(
            described
                .db_engine_versions()
                .iter()
                .map(db_engine_version_document)
                .collect(),
        ),
    );
    observed.answer(document)
}

/// `DescribeOrderableDBInstanceOptions`: the configurations an engine version can be launched
/// in, the other half of the catalogue a handler reads before it creates or resizes a database.
async fn describe_orderable_db_instance_options(
    client: &Client,
    input: &Input,
    observed: &Observed,
) -> Result<Answer, Refusal> {
    input.only(&[
        "Engine",
        "EngineVersion",
        "DBInstanceClass",
        "LicenseModel",
        "AvailabilityZoneGroup",
        "Vpc",
        "Filters",
        "MaxRecords",
        "Marker",
    ])?;
    let described = client
        .describe_orderable_db_instance_options()
        .engine(input.required_text("Engine")?)
        .set_engine_version(input.text("EngineVersion")?)
        .set_db_instance_class(input.text("DBInstanceClass")?)
        .set_license_model(input.text("LicenseModel")?)
        .set_availability_zone_group(input.text("AvailabilityZoneGroup")?)
        .set_vpc(input.boolean("Vpc")?)
        .set_filters(filters(input)?)
        .set_max_records(input.integer("MaxRecords")?)
        .set_marker(input.text("Marker")?)
        .send()
        .await;
    let described = match described {
        Ok(described) => described,
        Err(error) => return observed.refused(&error),
    };
    let mut document = Map::new();
    insert_text(&mut document, "Marker", described.marker());
    document.insert(
        "OrderableDBInstanceOptions".to_string(),
        Value::Array(
            described
                .orderable_db_instance_options()
                .iter()
                .map(orderable_option_document)
                .collect(),
        ),
    );
    observed.answer(document)
}

/// `DescribeDBProxies`: the proxies the account holds, or the one the call names — the first
/// half of the wiring an application reads before it connects through one, because each proxy
/// carries the `Auth` whose entries name the secret it logs in to the database with.
async fn describe_db_proxies(
    client: &Client,
    input: &Input,
    observed: &Observed,
) -> Result<Answer, Refusal> {
    input.only(&["DBProxyName", "Filters", "MaxRecords", "Marker"])?;
    let described = client
        .describe_db_proxies()
        .set_db_proxy_name(input.text("DBProxyName")?)
        .set_filters(filters(input)?)
        .set_max_records(input.integer("MaxRecords")?)
        .set_marker(input.text("Marker")?)
        .send()
        .await;
    let described = match described {
        Ok(described) => described,
        Err(error) => return observed.refused(&error),
    };
    let mut document = Map::new();
    insert_text(&mut document, "Marker", described.marker());
    document.insert(
        "DBProxies".to_string(),
        Value::Array(
            described
                .db_proxies()
                .iter()
                .map(db_proxy_document)
                .collect(),
        ),
    );
    observed.answer(document)
}

/// `DescribeDBProxyTargets`: the databases registered behind a proxy, each with the HEALTH of
/// the proxy's connection to it — the other half of that wiring, and what an application gates
/// a connection attempt on.
async fn describe_db_proxy_targets(
    client: &Client,
    input: &Input,
    observed: &Observed,
) -> Result<Answer, Refusal> {
    input.only(&[
        "DBProxyName",
        "TargetGroupName",
        "Filters",
        "MaxRecords",
        "Marker",
    ])?;
    let described = client
        .describe_db_proxy_targets()
        .db_proxy_name(input.required_text("DBProxyName")?)
        .set_target_group_name(input.text("TargetGroupName")?)
        .set_filters(filters(input)?)
        .set_max_records(input.integer("MaxRecords")?)
        .set_marker(input.text("Marker")?)
        .send()
        .await;
    let described = match described {
        Ok(described) => described,
        Err(error) => return observed.refused(&error),
    };
    let mut document = Map::new();
    insert_text(&mut document, "Marker", described.marker());
    document.insert(
        "Targets".to_string(),
        Value::Array(
            described
                .targets()
                .iter()
                .map(db_proxy_target_document)
                .collect(),
        ),
    );
    observed.answer(document)
}

/// `DescribeDBProxyTargetGroups`: the target group a proxy's registrations live in, and the
/// connection pool settings in force for it.
async fn describe_db_proxy_target_groups(
    client: &Client,
    input: &Input,
    observed: &Observed,
) -> Result<Answer, Refusal> {
    input.only(&[
        "DBProxyName",
        "TargetGroupName",
        "Filters",
        "MaxRecords",
        "Marker",
    ])?;
    let described = client
        .describe_db_proxy_target_groups()
        .db_proxy_name(input.required_text("DBProxyName")?)
        .set_target_group_name(input.text("TargetGroupName")?)
        .set_filters(filters(input)?)
        .set_max_records(input.integer("MaxRecords")?)
        .set_marker(input.text("Marker")?)
        .send()
        .await;
    let described = match described {
        Ok(described) => described,
        Err(error) => return observed.refused(&error),
    };
    let mut document = Map::new();
    insert_text(&mut document, "Marker", described.marker());
    document.insert(
        "TargetGroups".to_string(),
        Value::Array(
            described
                .target_groups()
                .iter()
                .map(db_proxy_target_group_document)
                .collect(),
        ),
    );
    observed.answer(document)
}

/// `DescribeDBProxyEndpoints`: the endpoints a proxy answers on — the address a client puts in
/// its connection string, and whether that address is read/write or read-only.
async fn describe_db_proxy_endpoints(
    client: &Client,
    input: &Input,
    observed: &Observed,
) -> Result<Answer, Refusal> {
    input.only(&[
        "DBProxyName",
        "DBProxyEndpointName",
        "Filters",
        "MaxRecords",
        "Marker",
    ])?;
    let described = client
        .describe_db_proxy_endpoints()
        .set_db_proxy_name(input.text("DBProxyName")?)
        .set_db_proxy_endpoint_name(input.text("DBProxyEndpointName")?)
        .set_filters(filters(input)?)
        .set_max_records(input.integer("MaxRecords")?)
        .set_marker(input.text("Marker")?)
        .send()
        .await;
    let described = match described {
        Ok(described) => described,
        Err(error) => return observed.refused(&error),
    };
    let mut document = Map::new();
    insert_text(&mut document, "Marker", described.marker());
    document.insert(
        "DBProxyEndpoints".to_string(),
        Value::Array(
            described
                .db_proxy_endpoints()
                .iter()
                .map(db_proxy_endpoint_document)
                .collect(),
        ),
    );
    observed.answer(document)
}

/// The `Filters` a describe carries, read as the name/values pairs the service takes; `None`
/// where the call carries none, so the member is left off the request rather than sent
/// emptied.
fn filters(input: &Input) -> Result<Option<Vec<::aws_sdk_rds::types::Filter>>, Refusal> {
    let mut held = Vec::new();
    for filter in input.structures("Filters")? {
        filter.only(&["Name", "Values"])?;
        held.push(
            ::aws_sdk_rds::types::Filter::builder()
                .name(filter.required_text("Name")?)
                .set_values(Some(filter.required_text_list("Values")?))
                .build(),
        );
    }
    Ok(optional_list(held))
}

/// A list member as the request carries it: absent where the call named none, so a member the
/// handler left out is left out of the request too.
fn optional_list<T>(held: Vec<T>) -> Option<Vec<T>> {
    (!held.is_empty()).then_some(held)
}

/// One `DBInstance` as the answer's document carries it: what the service reported about the
/// instance, member by member.
///
/// Every member named here is one of that shape's own
/// (`smithy_query(query="shape",shape="DBInstance")`): its identity and ARN, the engine it
/// runs, the status it is in, the address a client connects to, the credential it is reached
/// with and the configuration a handler reads back after changing it. A member of the shape
/// this seam does not read is left off rather than guessed at.
fn db_instance_document(instance: &::aws_sdk_rds::types::DbInstance) -> Value {
    let mut document = Map::new();
    insert_text(
        &mut document,
        "DBInstanceIdentifier",
        instance.db_instance_identifier(),
    );
    insert_text(&mut document, "DBInstanceArn", instance.db_instance_arn());
    insert_text(&mut document, "DBInstanceClass", instance.db_instance_class());
    insert_text(&mut document, "DBInstanceStatus", instance.db_instance_status());
    insert_text(&mut document, "Engine", instance.engine());
    insert_text(&mut document, "EngineVersion", instance.engine_version());
    insert_text(&mut document, "MasterUsername", instance.master_username());
    insert_text(&mut document, "DBName", instance.db_name());
    insert_text(&mut document, "AvailabilityZone", instance.availability_zone());
    insert_text(
        &mut document,
        "DBClusterIdentifier",
        instance.db_cluster_identifier(),
    );
    insert_text(&mut document, "DbiResourceId", instance.dbi_resource_id());
    insert_text(&mut document, "StorageType", instance.storage_type());
    insert_text(&mut document, "NetworkType", instance.network_type());
    insert_text(&mut document, "LicenseModel", instance.license_model());
    insert_text(&mut document, "KmsKeyId", instance.kms_key_id());
    insert_text(
        &mut document,
        "CACertificateIdentifier",
        instance.ca_certificate_identifier(),
    );
    insert_text(
        &mut document,
        "PreferredBackupWindow",
        instance.preferred_backup_window(),
    );
    insert_text(
        &mut document,
        "PreferredMaintenanceWindow",
        instance.preferred_maintenance_window(),
    );
    insert_number(
        &mut document,
        "AllocatedStorage",
        instance.allocated_storage().map(i64::from),
    );
    insert_number(
        &mut document,
        "MaxAllocatedStorage",
        instance.max_allocated_storage().map(i64::from),
    );
    insert_number(
        &mut document,
        "BackupRetentionPeriod",
        instance.backup_retention_period().map(i64::from),
    );
    insert_number(&mut document, "Iops", instance.iops().map(i64::from));
    insert_number(
        &mut document,
        "DbInstancePort",
        instance.db_instance_port().map(i64::from),
    );
    insert_timestamp(
        &mut document,
        "InstanceCreateTime",
        instance.instance_create_time(),
    );
    insert_flag(&mut document, "MultiAZ", instance.multi_az());
    insert_flag(
        &mut document,
        "PubliclyAccessible",
        instance.publicly_accessible(),
    );
    insert_flag(
        &mut document,
        "StorageEncrypted",
        instance.storage_encrypted(),
    );
    insert_flag(
        &mut document,
        "AutoMinorVersionUpgrade",
        instance.auto_minor_version_upgrade(),
    );
    insert_flag(
        &mut document,
        "IAMDatabaseAuthenticationEnabled",
        instance.iam_database_authentication_enabled(),
    );
    insert_flag(
        &mut document,
        "DeletionProtection",
        instance.deletion_protection(),
    );
    insert_flag(
        &mut document,
        "DedicatedLogVolume",
        instance.dedicated_log_volume(),
    );
    if let Some(endpoint) = instance.endpoint() {
        let mut held = Map::new();
        insert_text(&mut held, "Address", endpoint.address());
        insert_number(&mut held, "Port", endpoint.port().map(i64::from));
        insert_text(&mut held, "HostedZoneId", endpoint.hosted_zone_id());
        document.insert("Endpoint".to_string(), Value::Object(held));
    }
    insert_master_user_secret(&mut document, instance.master_user_secret());
    insert_vpc_security_groups(&mut document, instance.vpc_security_groups());
    Value::Object(document)
}

/// One `DBCluster` as the answer's document carries it, read the same way
/// (`smithy_query(query="shape",shape="DBCluster")`): its identity and ARN, the engine it
/// runs, the status it is in, the two addresses a client connects to, the credential it is
/// reached with and the members it holds.
fn db_cluster_document(cluster: &::aws_sdk_rds::types::DbCluster) -> Value {
    let mut document = Map::new();
    insert_text(
        &mut document,
        "DBClusterIdentifier",
        cluster.db_cluster_identifier(),
    );
    insert_text(&mut document, "DBClusterArn", cluster.db_cluster_arn());
    insert_text(
        &mut document,
        "DbClusterResourceId",
        cluster.db_cluster_resource_id(),
    );
    insert_text(&mut document, "Status", cluster.status());
    insert_text(&mut document, "Engine", cluster.engine());
    insert_text(&mut document, "EngineVersion", cluster.engine_version());
    insert_text(&mut document, "EngineMode", cluster.engine_mode());
    insert_text(&mut document, "Endpoint", cluster.endpoint());
    insert_text(&mut document, "ReaderEndpoint", cluster.reader_endpoint());
    insert_text(&mut document, "MasterUsername", cluster.master_username());
    insert_text(&mut document, "DatabaseName", cluster.database_name());
    insert_text(&mut document, "StorageType", cluster.storage_type());
    insert_text(&mut document, "KmsKeyId", cluster.kms_key_id());
    insert_text(
        &mut document,
        "PreferredBackupWindow",
        cluster.preferred_backup_window(),
    );
    insert_text(
        &mut document,
        "PreferredMaintenanceWindow",
        cluster.preferred_maintenance_window(),
    );
    insert_number(&mut document, "Port", cluster.port().map(i64::from));
    insert_number(
        &mut document,
        "AllocatedStorage",
        cluster.allocated_storage().map(i64::from),
    );
    insert_number(
        &mut document,
        "BackupRetentionPeriod",
        cluster.backup_retention_period().map(i64::from),
    );
    insert_number(&mut document, "Iops", cluster.iops().map(i64::from));
    insert_timestamp(&mut document, "ClusterCreateTime", cluster.cluster_create_time());
    insert_flag(&mut document, "MultiAZ", cluster.multi_az());
    insert_flag(&mut document, "StorageEncrypted", cluster.storage_encrypted());
    insert_flag(
        &mut document,
        "IAMDatabaseAuthenticationEnabled",
        cluster.iam_database_authentication_enabled(),
    );
    insert_flag(
        &mut document,
        "DeletionProtection",
        cluster.deletion_protection(),
    );
    let zones: Vec<Value> = cluster
        .availability_zones()
        .iter()
        .map(|zone| Value::String(zone.clone()))
        .collect();
    if !zones.is_empty() {
        document.insert("AvailabilityZones".to_string(), Value::Array(zones));
    }
    let members: Vec<Value> = cluster
        .db_cluster_members()
        .iter()
        .map(|member| {
            let mut held = Map::new();
            insert_text(
                &mut held,
                "DBInstanceIdentifier",
                member.db_instance_identifier(),
            );
            insert_flag(&mut held, "IsClusterWriter", member.is_cluster_writer());
            insert_number(
                &mut held,
                "PromotionTier",
                member.promotion_tier().map(i64::from),
            );
            Value::Object(held)
        })
        .collect();
    if !members.is_empty() {
        document.insert("DBClusterMembers".to_string(), Value::Array(members));
    }
    insert_master_user_secret(&mut document, cluster.master_user_secret());
    insert_vpc_security_groups(&mut document, cluster.vpc_security_groups());
    Value::Object(document)
}

/// The MANAGED MASTER CREDENTIAL a resource reports, where the service holds one for it: the
/// secret it lives in, the state that secret is in and the key it is encrypted with
/// (`MasterUserSecret`). The secret's VALUE is not part of it — that is Secrets Manager's own
/// answer to whoever may read the secret.
fn insert_master_user_secret(
    document: &mut Map<String, Value>,
    secret: Option<&::aws_sdk_rds::types::MasterUserSecret>,
) {
    let Some(secret) = secret else {
        return;
    };
    let mut held = Map::new();
    insert_text(&mut held, "SecretArn", secret.secret_arn());
    insert_text(&mut held, "SecretStatus", secret.secret_status());
    insert_text(&mut held, "KmsKeyId", secret.kms_key_id());
    document.insert("MasterUserSecret".to_string(), Value::Object(held));
}

/// The VPC security groups a resource reports being in, each with the state of its
/// membership (`VpcSecurityGroupMembership`).
fn insert_vpc_security_groups(
    document: &mut Map<String, Value>,
    groups: &[::aws_sdk_rds::types::VpcSecurityGroupMembership],
) {
    if groups.is_empty() {
        return;
    }
    let held: Vec<Value> = groups
        .iter()
        .map(|group| {
            let mut entry = Map::new();
            insert_text(
                &mut entry,
                "VpcSecurityGroupId",
                group.vpc_security_group_id(),
            );
            insert_text(&mut entry, "Status", group.status());
            Value::Object(entry)
        })
        .collect();
    document.insert("VpcSecurityGroups".to_string(), Value::Array(held));
}

/// One `DBEngineVersion` as the answer's document carries it, read the same way
/// (`smithy_query(query="shape",shape="DBEngineVersion")`): the engine and version it names, the
/// parameter-group family and descriptions a client shows, the status it is published in, the
/// versions it can be upgraded to, and what the version SUPPORTS — which is the whole reason a
/// handler reads this catalogue before it launches or changes a database.
fn db_engine_version_document(version: &::aws_sdk_rds::types::DbEngineVersion) -> Value {
    let mut document = Map::new();
    insert_text(&mut document, "Engine", version.engine());
    insert_text(&mut document, "EngineVersion", version.engine_version());
    insert_text(
        &mut document,
        "MajorEngineVersion",
        version.major_engine_version(),
    );
    insert_text(
        &mut document,
        "DBParameterGroupFamily",
        version.db_parameter_group_family(),
    );
    insert_text(
        &mut document,
        "DBEngineDescription",
        version.db_engine_description(),
    );
    insert_text(
        &mut document,
        "DBEngineVersionDescription",
        version.db_engine_version_description(),
    );
    insert_text(
        &mut document,
        "DBEngineVersionArn",
        version.db_engine_version_arn(),
    );
    insert_text(
        &mut document,
        "DBEngineMediaType",
        version.db_engine_media_type(),
    );
    insert_text(&mut document, "Status", version.status());
    insert_text(&mut document, "KMSKeyId", version.kms_key_id());
    insert_timestamp(&mut document, "CreateTime", version.create_time());
    insert_flag(
        &mut document,
        "SupportsLogExportsToCloudwatchLogs",
        version.supports_log_exports_to_cloudwatch_logs(),
    );
    insert_flag(
        &mut document,
        "SupportsReadReplica",
        version.supports_read_replica(),
    );
    insert_flag(
        &mut document,
        "SupportsParallelQuery",
        version.supports_parallel_query(),
    );
    insert_flag(
        &mut document,
        "SupportsGlobalDatabases",
        version.supports_global_databases(),
    );
    insert_flag(&mut document, "SupportsBabelfish", version.supports_babelfish());
    insert_flag(
        &mut document,
        "SupportsLimitlessDatabase",
        version.supports_limitless_database(),
    );
    insert_flag(
        &mut document,
        "SupportsIntegrations",
        version.supports_integrations(),
    );
    insert_flag(
        &mut document,
        "SupportsCertificateRotationWithoutRestart",
        version.supports_certificate_rotation_without_restart(),
    );
    insert_flag(
        &mut document,
        "SupportsLocalWriteForwarding",
        version.supports_local_write_forwarding(),
    );
    insert_list(
        &mut document,
        "ExportableLogTypes",
        version.exportable_log_types(),
    );
    insert_list(
        &mut document,
        "SupportedEngineModes",
        version.supported_engine_modes(),
    );
    insert_list(
        &mut document,
        "SupportedFeatureNames",
        version.supported_feature_names(),
    );
    insert_list(
        &mut document,
        "SupportedCACertificateIdentifiers",
        version.supported_ca_certificate_identifiers(),
    );
    insert_list(
        &mut document,
        "DatabaseInstallationFiles",
        version.database_installation_files(),
    );
    let upgrades: Vec<Value> = version
        .valid_upgrade_target()
        .iter()
        .map(|target| {
            let mut held = Map::new();
            insert_text(&mut held, "Engine", target.engine());
            insert_text(&mut held, "EngineVersion", target.engine_version());
            insert_text(&mut held, "Description", target.description());
            insert_flag(&mut held, "AutoUpgrade", target.auto_upgrade());
            insert_flag(
                &mut held,
                "IsMajorVersionUpgrade",
                target.is_major_version_upgrade(),
            );
            insert_list(&mut held, "SupportedEngineModes", target.supported_engine_modes());
            Value::Object(held)
        })
        .collect();
    if !upgrades.is_empty() {
        document.insert("ValidUpgradeTarget".to_string(), Value::Array(upgrades));
    }
    insert_character_sets(
        &mut document,
        "SupportedCharacterSets",
        version.supported_character_sets(),
    );
    insert_character_sets(
        &mut document,
        "SupportedNcharCharacterSets",
        version.supported_nchar_character_sets(),
    );
    if let Some(default) = version.default_character_set() {
        document.insert(
            "DefaultCharacterSet".to_string(),
            character_set_document(default),
        );
    }
    let zones: Vec<Value> = version
        .supported_timezones()
        .iter()
        .map(|zone| {
            let mut held = Map::new();
            insert_text(&mut held, "TimezoneName", zone.timezone_name());
            Value::Object(held)
        })
        .collect();
    if !zones.is_empty() {
        document.insert("SupportedTimezones".to_string(), Value::Array(zones));
    }
    Value::Object(document)
}

/// The CHARACTER SETS an engine version publishes for a member, where it publishes any.
fn insert_character_sets(
    document: &mut Map<String, Value>,
    name: &str,
    sets: &[::aws_sdk_rds::types::CharacterSet],
) {
    if sets.is_empty() {
        return;
    }
    let held: Vec<Value> = sets.iter().map(character_set_document).collect();
    document.insert(name.to_string(), Value::Array(held));
}

/// One `CharacterSet` as the answer's document carries it.
fn character_set_document(set: &::aws_sdk_rds::types::CharacterSet) -> Value {
    let mut held = Map::new();
    insert_text(&mut held, "CharacterSetName", set.character_set_name());
    insert_text(
        &mut held,
        "CharacterSetDescription",
        set.character_set_description(),
    );
    Value::Object(held)
}

/// One `OrderableDBInstanceOption` as the answer's document carries it, read the same way
/// (`smithy_query(query="shape",shape="OrderableDBInstanceOption")`): the engine version and
/// instance class the option is for, where it can be launched, and the storage and feature
/// bounds a handler sizes a database against.
fn orderable_option_document(
    option: &::aws_sdk_rds::types::OrderableDbInstanceOption,
) -> Value {
    let mut document = Map::new();
    insert_text(&mut document, "Engine", option.engine());
    insert_text(&mut document, "EngineVersion", option.engine_version());
    insert_text(&mut document, "DBInstanceClass", option.db_instance_class());
    insert_text(&mut document, "LicenseModel", option.license_model());
    insert_text(
        &mut document,
        "AvailabilityZoneGroup",
        option.availability_zone_group(),
    );
    insert_text(&mut document, "StorageType", option.storage_type());
    insert_flag(&mut document, "Vpc", option.vpc());
    insert_flag(&mut document, "MultiAZCapable", option.multi_az_capable());
    insert_flag(
        &mut document,
        "ReadReplicaCapable",
        option.read_replica_capable(),
    );
    insert_flag(
        &mut document,
        "SupportsStorageEncryption",
        option.supports_storage_encryption(),
    );
    insert_flag(&mut document, "SupportsIops", option.supports_iops());
    insert_flag(
        &mut document,
        "SupportsStorageThroughput",
        option.supports_storage_throughput(),
    );
    insert_flag(
        &mut document,
        "SupportsEnhancedMonitoring",
        option.supports_enhanced_monitoring(),
    );
    insert_flag(
        &mut document,
        "SupportsIAMDatabaseAuthentication",
        option.supports_iam_database_authentication(),
    );
    insert_flag(
        &mut document,
        "SupportsPerformanceInsights",
        option.supports_performance_insights(),
    );
    insert_flag(
        &mut document,
        "SupportsGlobalDatabases",
        option.supports_global_databases(),
    );
    insert_flag(&mut document, "SupportsClusters", option.supports_clusters());
    insert_flag(
        &mut document,
        "SupportsStorageAutoscaling",
        option.supports_storage_autoscaling(),
    );
    insert_flag(
        &mut document,
        "SupportsKerberosAuthentication",
        option.supports_kerberos_authentication(),
    );
    insert_flag(
        &mut document,
        "SupportsDedicatedLogVolume",
        option.supports_dedicated_log_volume(),
    );
    insert_flag(
        &mut document,
        "SupportsHttpEndpoint",
        option.supports_http_endpoint(),
    );
    insert_flag(&mut document, "OutpostCapable", option.outpost_capable());
    insert_number(
        &mut document,
        "MinStorageSize",
        option.min_storage_size().map(i64::from),
    );
    insert_number(
        &mut document,
        "MaxStorageSize",
        option.max_storage_size().map(i64::from),
    );
    insert_number(
        &mut document,
        "MinIopsPerDbInstance",
        option.min_iops_per_db_instance().map(i64::from),
    );
    insert_number(
        &mut document,
        "MaxIopsPerDbInstance",
        option.max_iops_per_db_instance().map(i64::from),
    );
    insert_number(
        &mut document,
        "MinStorageThroughputPerDbInstance",
        option.min_storage_throughput_per_db_instance().map(i64::from),
    );
    insert_number(
        &mut document,
        "MaxStorageThroughputPerDbInstance",
        option.max_storage_throughput_per_db_instance().map(i64::from),
    );
    insert_double(&mut document, "MinIopsPerGib", option.min_iops_per_gib());
    insert_double(&mut document, "MaxIopsPerGib", option.max_iops_per_gib());
    insert_double(
        &mut document,
        "MinStorageThroughputPerIops",
        option.min_storage_throughput_per_iops(),
    );
    insert_double(
        &mut document,
        "MaxStorageThroughputPerIops",
        option.max_storage_throughput_per_iops(),
    );
    insert_list(
        &mut document,
        "SupportedEngineModes",
        option.supported_engine_modes(),
    );
    insert_list(
        &mut document,
        "SupportedNetworkTypes",
        option.supported_network_types(),
    );
    insert_list(
        &mut document,
        "SupportedActivityStreamModes",
        option.supported_activity_stream_modes(),
    );
    let zones: Vec<Value> = option
        .availability_zones()
        .iter()
        .map(|zone| {
            let mut held = Map::new();
            insert_text(&mut held, "Name", zone.name());
            Value::Object(held)
        })
        .collect();
    if !zones.is_empty() {
        document.insert("AvailabilityZones".to_string(), Value::Array(zones));
    }
    let features: Vec<Value> = option
        .available_processor_features()
        .iter()
        .map(|feature| {
            let mut held = Map::new();
            insert_text(&mut held, "Name", feature.name());
            insert_text(&mut held, "DefaultValue", feature.default_value());
            insert_text(&mut held, "AllowedValues", feature.allowed_values());
            Value::Object(held)
        })
        .collect();
    if !features.is_empty() {
        document.insert(
            "AvailableProcessorFeatures".to_string(),
            Value::Array(features),
        );
    }
    Value::Object(document)
}

/// One `DBProxy` as the answer's document carries it, read the same way
/// (`smithy_query(query="shape",shape="DBProxy")`): its name and ARN, the status it is in, the
/// engine family and network placement it serves, the address a client connects to, the role it
/// reads its credentials with and the `Auth` entries naming those credentials.
fn db_proxy_document(proxy: &::aws_sdk_rds::types::DbProxy) -> Value {
    let mut document = Map::new();
    insert_text(&mut document, "DBProxyName", proxy.db_proxy_name());
    insert_text(&mut document, "DBProxyArn", proxy.db_proxy_arn());
    insert_text(
        &mut document,
        "Status",
        proxy.status().map(|status| status.as_str()),
    );
    insert_text(&mut document, "EngineFamily", proxy.engine_family());
    insert_text(&mut document, "VpcId", proxy.vpc_id());
    insert_list(
        &mut document,
        "VpcSecurityGroupIds",
        proxy.vpc_security_group_ids(),
    );
    insert_list(&mut document, "VpcSubnetIds", proxy.vpc_subnet_ids());
    insert_text(
        &mut document,
        "DefaultAuthScheme",
        proxy.default_auth_scheme(),
    );
    insert_text(
        &mut document,
        "EndpointNetworkType",
        proxy.endpoint_network_type().map(|kind| kind.as_str()),
    );
    insert_text(
        &mut document,
        "TargetConnectionNetworkType",
        proxy
            .target_connection_network_type()
            .map(|kind| kind.as_str()),
    );
    insert_text(&mut document, "RoleArn", proxy.role_arn());
    insert_text(&mut document, "Endpoint", proxy.endpoint());
    insert_flag(&mut document, "RequireTLS", proxy.require_tls());
    insert_number(
        &mut document,
        "IdleClientTimeout",
        proxy.idle_client_timeout().map(i64::from),
    );
    insert_flag(&mut document, "DebugLogging", proxy.debug_logging());
    insert_timestamp(&mut document, "CreatedDate", proxy.created_date());
    insert_timestamp(&mut document, "UpdatedDate", proxy.updated_date());
    let auth: Vec<Value> = proxy.auth().iter().map(user_auth_config_document).collect();
    if !auth.is_empty() {
        document.insert("Auth".to_string(), Value::Array(auth));
    }
    Value::Object(document)
}

/// One `UserAuthConfigInfo` as the answer's document carries it
/// (`smithy_query(query="shape",shape="UserAuthConfigInfo")`): the database user the proxy logs
/// in as, the scheme it authenticates with and the SECRET that credential lives in — which is
/// what a handler resolves to find the credential its proxy connects with. The secret's VALUE is
/// not part of it: that is Secrets Manager's own answer to whoever may read the secret.
fn user_auth_config_document(auth: &::aws_sdk_rds::types::UserAuthConfigInfo) -> Value {
    let mut document = Map::new();
    insert_text(&mut document, "Description", auth.description());
    insert_text(&mut document, "UserName", auth.user_name());
    insert_text(
        &mut document,
        "AuthScheme",
        auth.auth_scheme().map(|scheme| scheme.as_str()),
    );
    insert_text(&mut document, "SecretArn", auth.secret_arn());
    insert_text(
        &mut document,
        "IAMAuth",
        auth.iam_auth().map(|mode| mode.as_str()),
    );
    insert_text(
        &mut document,
        "ClientPasswordAuthType",
        auth.client_password_auth_type().map(|kind| kind.as_str()),
    );
    Value::Object(document)
}

/// One `DBProxyTarget` as the answer's document carries it, read the same way
/// (`smithy_query(query="shape",shape="DBProxyTarget")`): the database it represents, the
/// address and port the proxy reaches it at, the kind of database it is, whether it is
/// read/write or read-only, and the HEALTH of the proxy's connection to it.
fn db_proxy_target_document(target: &::aws_sdk_rds::types::DbProxyTarget) -> Value {
    let mut document = Map::new();
    insert_text(&mut document, "TargetArn", target.target_arn());
    insert_text(&mut document, "Endpoint", target.endpoint());
    insert_text(&mut document, "TrackedClusterId", target.tracked_cluster_id());
    insert_text(&mut document, "RdsResourceId", target.rds_resource_id());
    insert_number(&mut document, "Port", target.port().map(i64::from));
    insert_text(
        &mut document,
        "Type",
        target.r#type().map(|kind| kind.as_str()),
    );
    insert_text(
        &mut document,
        "Role",
        target.role().map(|role| role.as_str()),
    );
    if let Some(health) = target.target_health() {
        let mut held = Map::new();
        insert_text(
            &mut held,
            "State",
            health.state().map(|state| state.as_str()),
        );
        insert_text(
            &mut held,
            "Reason",
            health.reason().map(|reason| reason.as_str()),
        );
        insert_text(&mut held, "Description", health.description());
        document.insert("TargetHealth".to_string(), Value::Object(held));
    }
    Value::Object(document)
}

/// One `DBProxyTargetGroup` as the answer's document carries it, read the same way
/// (`smithy_query(query="shape",shape="DBProxyTargetGroup")`): the proxy it belongs to, its own
/// name and ARN, the status it is in and the connection pool settings in force for it.
///
/// `ConnectionPoolConfig$InitQuery` is among them: the model marks that member sensitive, which
/// governs what is LOGGED rather than what the service answers — the setting is part of the target
/// group's configuration, and this seam hands a handler the members the service reported, nothing
/// of its own and nothing less.
fn db_proxy_target_group_document(group: &::aws_sdk_rds::types::DbProxyTargetGroup) -> Value {
    let mut document = Map::new();
    insert_text(&mut document, "DBProxyName", group.db_proxy_name());
    insert_text(&mut document, "TargetGroupName", group.target_group_name());
    insert_text(&mut document, "TargetGroupArn", group.target_group_arn());
    insert_flag(&mut document, "IsDefault", group.is_default());
    insert_text(&mut document, "Status", group.status());
    insert_timestamp(&mut document, "CreatedDate", group.created_date());
    insert_timestamp(&mut document, "UpdatedDate", group.updated_date());
    if let Some(pool) = group.connection_pool_config() {
        let mut held = Map::new();
        insert_number(
            &mut held,
            "MaxConnectionsPercent",
            pool.max_connections_percent().map(i64::from),
        );
        insert_number(
            &mut held,
            "MaxIdleConnectionsPercent",
            pool.max_idle_connections_percent().map(i64::from),
        );
        insert_number(
            &mut held,
            "ConnectionBorrowTimeout",
            pool.connection_borrow_timeout().map(i64::from),
        );
        insert_list(
            &mut held,
            "SessionPinningFilters",
            pool.session_pinning_filters(),
        );
        insert_text(&mut held, "InitQuery", pool.init_query());
        document.insert("ConnectionPoolConfig".to_string(), Value::Object(held));
    }
    Value::Object(document)
}

/// One `DBProxyEndpoint` as the answer's document carries it, read the same way
/// (`smithy_query(query="shape",shape="DBProxyEndpoint")`): the proxy it belongs to, its own
/// name and ARN, the status it is in, where it is placed, the address a client connects to and
/// whether that address is the proxy's default one.
fn db_proxy_endpoint_document(endpoint: &::aws_sdk_rds::types::DbProxyEndpoint) -> Value {
    let mut document = Map::new();
    insert_text(
        &mut document,
        "DBProxyEndpointName",
        endpoint.db_proxy_endpoint_name(),
    );
    insert_text(
        &mut document,
        "DBProxyEndpointArn",
        endpoint.db_proxy_endpoint_arn(),
    );
    insert_text(&mut document, "DBProxyName", endpoint.db_proxy_name());
    insert_text(
        &mut document,
        "Status",
        endpoint.status().map(|status| status.as_str()),
    );
    insert_text(&mut document, "VpcId", endpoint.vpc_id());
    insert_list(
        &mut document,
        "VpcSecurityGroupIds",
        endpoint.vpc_security_group_ids(),
    );
    insert_list(&mut document, "VpcSubnetIds", endpoint.vpc_subnet_ids());
    insert_text(&mut document, "Endpoint", endpoint.endpoint());
    insert_timestamp(&mut document, "CreatedDate", endpoint.created_date());
    insert_text(
        &mut document,
        "TargetRole",
        endpoint.target_role().map(|role| role.as_str()),
    );
    insert_flag(&mut document, "IsDefault", endpoint.is_default());
    insert_text(
        &mut document,
        "EndpointNetworkType",
        endpoint.endpoint_network_type().map(|kind| kind.as_str()),
    );
    Value::Object(document)
}

#[cfg(test)]
mod tests {
    use super::{bound_operation, BOUND_OPERATIONS};

    /// RULE: a call names one of this service's own operations whatever case the SDK the guest
    /// calls derived the name in, and names nothing else.
    ///
    /// The rows differ along the spelling a client arrives with: the service's own, the one a
    /// method name titled word by word gives it (`describe_db_instances` ->
    /// `DescribeDbInstances`), a lower-cased one, and the names that are no bound operation at
    /// all — one of this service the seam does not bind, one of no service, and the empty name.
    #[test]
    fn a_calls_operation_is_the_services_own_whatever_case_it_arrives_in() {
        let rows: &[(&str, &str, Option<&str>)] = &[
            (
                "the service's own spelling",
                "DescribeDBInstances",
                Some("DescribeDBInstances"),
            ),
            (
                "a method name titled word by word",
                "DescribeDbInstances",
                Some("DescribeDBInstances"),
            ),
            (
                "the same for a modify",
                "ModifyDbInstance",
                Some("ModifyDBInstance"),
            ),
            (
                "the cluster pair, titled the same way",
                "ModifyDbCluster",
                Some("ModifyDBCluster"),
            ),
            ("a lower-cased name", "describedbclusters", Some("DescribeDBClusters")),
            (
                "the engine-version catalogue read, titled word by word",
                "DescribeDbEngineVersions",
                Some("DescribeDBEngineVersions"),
            ),
            (
                "the orderable-options catalogue read, titled the same way",
                "DescribeOrderableDbInstanceOptions",
                Some("DescribeOrderableDBInstanceOptions"),
            ),
            (
                "the proxy read, titled word by word",
                "DescribeDbProxies",
                Some("DescribeDBProxies"),
            ),
            (
                "the proxy's target health read, titled the same way",
                "DescribeDbProxyTargets",
                Some("DescribeDBProxyTargets"),
            ),
            ("an operation of this service the seam does not bind", "DeleteDBInstance", None),
            ("an operation of no service at all", "Elsewhere", None),
            ("the empty name", "", None),
        ];
        for (case, asked, expected) in rows {
            let reached = bound_operation(asked);
            assert_eq!(
                reached, *expected,
                "case {case:?}: expected {asked:?} to name {expected:?}, got {reached:?}"
            );
        }
        for bound in BOUND_OPERATIONS {
            assert_eq!(
                bound_operation(bound),
                Some(*bound),
                "case {bound:?}: every bound operation names itself"
            );
        }
    }
}
