// SPDX-License-Identifier: CC0-1.0

//! Where a metadata configuration's tables really are: the S3 Tables store that owns them.
//!
//! "`s3tables:CreateTableBucket` `s3tables:CreateNamespace` `s3tables:CreateTable`" are among
//! the permissions `CreateBucketMetadataConfiguration` requires (that operation's own
//! documentation), so provisioning a metadata configuration provisions S3 TABLES resources:
//! the Amazon Web Services managed table bucket, the general purpose bucket's own namespace
//! inside it ([`metadata_table_namespace`]) and the journal, inventory and annotation tables
//! themselves. They are that service's resources, so they are written into its store in the
//! shared engine through its own entry points for a resource a service provisions
//! ([`tables_store::ComAmazonawsS3tablesS3tablebucketsState::ensure_service_managed_namespace`],
//! [`tables_store::ComAmazonawsS3tablesS3tablebucketsState::create_service_managed_table_with_id`]):
//! `GetTable` and `ListTables` resolve them exactly as they resolve a table a caller created,
//! `DeleteTable` and `DeleteNamespace` remove them, and every identity and bound a table
//! carries stays in the crate that owns tables.
//!
//! Nothing about a table is copied into this service's own records: a table dropped in S3
//! Tables is dropped for the configuration's reader too, which is what makes the published
//! `*AlreadyExists` provisioning failures tell the truth after a caller has deleted the
//! leftover table the way those sentences ask it to.
//!
//! LOCK ORDER: the bucket store is already held by the handler that calls in, and the S3
//! Tables store is taken under it — this service locks S3 then S3 Tables, never the other way
//! round.

use std::time::SystemTime;

use ::service_com_amazonaws_s3tables_s3tablebuckets::state as tables_store;

use super::{metadata_qualified_table_name, METADATA_TABLE_BUCKET_NAME};

/// The service principal S3 Tables reports as the manager of a table this service creates on
/// an account's behalf ("The service that manages the table", `GetTable$managedByService`).
///
/// This service's own principal, which is its ARN namespace under the Amazon Web Services
/// service domain — the value that tells an S3 Metadata table from a table a caller created
/// and from one another service provisioned.
pub fn managing_service_principal() -> String {
    format!("{METADATA_TABLE_ARN_SERVICE_PRINCIPAL_NAMESPACE}.amazonaws.com")
}

/// The namespace of [`managing_service_principal`]: Amazon S3's own.
const METADATA_TABLE_ARN_SERVICE_PRINCIPAL_NAMESPACE: &str = "s3";

/// What identifies ONE metadata table of the managed table bucket: where it really stands,
/// and the folded identity two general purpose bucket names can share.
///
/// Both, because they answer different questions. A table stands at `namespace`/`name` and is
/// addressed there by every S3 Tables operation, while `qualified`
/// ([`metadata_qualified_table_name`]) is what two bucket names differing only in the
/// characters an S3 Tables name cannot carry fold to — which is why one bucket's configuration
/// can find another's table already holding the name it would provision under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetadataTableIdentity {
    /// The namespace of the managed table bucket the table stands in.
    pub namespace: String,
    /// The table's own name: which of the configuration's three tables it is.
    pub name: String,
    /// The two of them folded into the one value that identifies the table inside the managed
    /// table bucket, which is also the id its ARN carries.
    pub qualified: String,
}

impl MetadataTableIdentity {
    /// The identity of the table `name` of the namespace `namespace`.
    pub fn new(namespace: &str, name: &str) -> Self {
        Self {
            namespace: namespace.to_string(),
            name: name.to_string(),
            qualified: metadata_qualified_table_name(namespace, name),
        }
    }
}

/// The managed table bucket of one Region, as the store that owns it holds it: the one view
/// through which this service creates, finds and takes back a metadata table.
///
/// Built around a borrow of the S3 Tables store (the guard is the caller's, so the lock order
/// above is the handler's to keep) and the Region the general purpose bucket lives in, which
/// is the Region its metadata tables live in.
#[derive(Debug)]
pub struct ManagedTableBucket<'a> {
    tables: &'a mut tables_store::ComAmazonawsS3tablesS3tablebucketsState,
    region: String,
}

impl<'a> ManagedTableBucket<'a> {
    /// The view onto `tables` for the managed table bucket of `region`.
    pub fn new(
        tables: &'a mut tables_store::ComAmazonawsS3tablesS3tablebucketsState,
        region: &str,
    ) -> Self {
        Self {
            tables,
            region: region.to_string(),
        }
    }

    /// The managed table bucket's `tableBucketARN` in this Region — the ARN every reader of a
    /// metadata configuration reports and the one S3 Tables answers to, through the ONE
    /// derivation of it this service has ([`super::metadata_table_bucket_arn`]).
    pub fn arn(&self) -> String {
        super::metadata_table_bucket_arn(&self.region)
    }

    /// Whether a table stands at `identity` now, whoever created it.
    ///
    /// A table name is unique inside its namespace, so a name a table already holds is a name
    /// no second table can be created under — which is the condition the published
    /// `JournalTableAlreadyExists` and `InventoryTableAlreadyExists` failures report.
    pub fn holds(&self, identity: &MetadataTableIdentity) -> bool {
        self.tables
            .holds_table(&self.arn(), &identity.namespace, &identity.name)
    }

    /// Create the table `identity` in the managed table bucket, reporting whether S3 Tables
    /// took it.
    ///
    /// The managed table bucket and the namespace are created first when they are not there
    /// yet, which is what the configuration's own permissions say this provisioning does. The
    /// table's ARN is the one this service PUBLISHES for it ([`super::metadata_table_arn`]),
    /// so the two services name one table by one ARN.
    pub fn provision(&mut self, identity: &MetadataTableIdentity, now: SystemTime) -> bool {
        let arn = match self.tables.ensure_service_managed_namespace(
            &self.region,
            METADATA_TABLE_BUCKET_NAME,
            &identity.namespace,
            now,
        ) {
            Ok(arn) => arn,
            Err(refusal) => {
                // Visible to an operator: the configuration's reader must not report a table
                // in service that no store holds, and this is the one place that can tell
                // them apart.
                ::tracing::warn!(
                    namespace = ?identity.namespace,
                    reason = ?refusal,
                    "S3 Tables did not take the namespace this bucket's metadata tables \
                     would live in, so no metadata table was created"
                );
                return false;
            }
        };
        match self.tables.create_service_managed_table_with_id(
            &arn,
            &identity.namespace,
            &identity.name,
            &managing_service_principal(),
            Some(&identity.qualified),
            now,
        ) {
            Ok(_) => true,
            Err(refusal) => {
                ::tracing::warn!(
                    namespace = ?identity.namespace,
                    table = ?identity.name,
                    reason = ?refusal,
                    "S3 Tables did not take this metadata table, so the configuration has \
                     none under that name"
                );
                false
            }
        }
    }

    /// Take back the table `identity` this service provisioned, when it is still one this
    /// service manages.
    ///
    /// For the one case a metadata table is removed by this service: a configuration carried
    /// away before the table it asked for ever came into service, which leaves nothing for a
    /// caller to delete. A table any other service manages, and a table a caller created, is
    /// left where it is.
    pub fn release(&mut self, identity: &MetadataTableIdentity) {
        self.tables.delete_service_managed_table(
            &self.arn(),
            &identity.namespace,
            &identity.name,
            &managing_service_principal(),
        );
    }
}

/// The S3 Tables store in the shared engine, for a handler that is about to provision or take
/// back a metadata table: call it with that handler's `&*self.engine`.
///
/// `None` for an engine that carries no such store, which a handler answers with the same
/// internal failure it answers for its own store being unreachable — never by reporting a
/// table it did not create.
pub fn metadata_tables_store<Eng: Send + Sync + 'static>(
    engine: &Eng,
) -> Option<::engine::ServiceState<tables_store::ComAmazonawsS3tablesS3tablebucketsState>> {
    tables_store::service_state(engine)
}
