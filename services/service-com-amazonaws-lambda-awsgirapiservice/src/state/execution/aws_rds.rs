// SPDX-License-Identifier: CC0-1.0

//! The Amazon RDS half of the SDK seam a RUNNING handler calls another service through: the
//! DB instance or cluster a handler reads, and the MASTER CREDENTIAL it changes on one.
//!
//! CHANGING A DATABASE'S OWN CREDENTIAL is what a handler at the end of a credential
//! rotation does: the rotation function AWS Secrets Manager invokes stages a new password in
//! `createSecret` and then puts it in force in the database in `setSecret` — which for an
//! RDS master credential is `ModifyDBInstance` (or `ModifyDBCluster`) on the instance the
//! secret belongs to, the one way to change that credential without a database connection of
//! its own. A handler that cannot reach this service cannot carry out that step at all, so
//! the rotation it is driven through never finishes.
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
//!   [`db_cluster_document`] name every one of them), and a refusal is that service's own
//!   error code, message and status, which the SDK the guest calls raises as `ClientError`
//!   ([`super::aws_sdk_guest`]).
//!
//! How one call's input is read, what the answer carried and how a refusal is handed back are
//! the same for every service reached through an SDK client, so they are [`super::sdk_call`]'s
//! rather than this module's.

use ::aws_sdk_rds::Client;
use ::serde_json::{Map, Value};
use ::smithy_server_runtime::EmbeddedEmulator;

use super::aws_api::{Answer, CallerIdentity, Refusal};
use super::sdk_call::{insert_flag, insert_number, insert_text, insert_timestamp, Input, Observed, PROVIDER_NAME};

/// The service a refusal of one of these calls names, as the AWS SDKs name it.
const SERVICE: &str = "Amazon RDS";

/// The operations of Amazon RDS this seam binds, in the order a refusal lists them.
///
/// One entry per operation [`call`] builds a request for: the surface a handler that manages
/// a database credential uses — reading the DB instance or DB cluster the credential belongs
/// to, and changing that resource's own configuration, which is how the master credential is
/// put in force (`MasterUserPassword` / `RotateMasterUserPassword` on either modify). A
/// handler naming any other operation of the service is told the call was not made and why,
/// because a request this module cannot bind is one it would otherwise be guessing at.
pub const BOUND_OPERATIONS: &[&str] = &[
    "DescribeDBInstances",
    "ModifyDBInstance",
    "DescribeDBClusters",
    "ModifyDBCluster",
];

/// Make ONE Amazon RDS call: `operation`, with `body` as the operation's input document (the
/// keyword arguments the handler passed its client method), in `region`.
pub async fn call(
    callback: &EmbeddedEmulator,
    identity: Option<&CallerIdentity>,
    region: &str,
    operation: &str,
    body: &str,
) -> Result<Answer, Refusal> {
    let input = Input::read(SERVICE, body)?;
    let observed = Observed::of(SERVICE);
    let client = client(callback, identity, region, observed.clone());
    match bound_operation(operation) {
        Some("DescribeDBInstances") => describe_db_instances(&client, &input, &observed).await,
        Some("ModifyDBInstance") => modify_db_instance(&client, &input, &observed).await,
        Some("DescribeDBClusters") => describe_db_clusters(&client, &input, &observed).await,
        Some("ModifyDBCluster") => modify_db_cluster(&client, &input, &observed).await,
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
fn client(
    callback: &EmbeddedEmulator,
    identity: Option<&CallerIdentity>,
    region: &str,
    observed: Observed,
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
    Client::from_conf(config.build())
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
