// SPDX-License-Identifier: CC0-1.0

//! @op GetBackupVaultNotifications
//!
//! Reports the event notifications the named vault carries: the topic and the
//! events [`PutBackupVaultNotifications`](crate::ops::put_backup_vault_notifications)
//! stored on it ([`crate::state::BackupVault::notifications`]), so this read
//! answers from the SAME stored vault the write configured.
//!
//! Two steps, in the order AWS Backup applies them:
//!
//! * the named vault is resolved by the step every vault-scoped read shares
//!   ([`crate::state::ComAmazonawsBackupCryocontrollerusermanagerState::visible_vault`]),
//!   which decides access before existence, so a vault the caller cannot see is
//!   never disclosed;
//! * the configuration it carries is reported beside the vault's own name and
//!   ARN. A vault carrying none has nothing to report, so the read refuses it
//!   `ResourceNotFoundException` with the sentence this service states for that
//!   condition ([`crate::state::vault_notifications_not_found`]) — the same
//!   answer it gives for a vault whose configuration has been deleted.
//!
//! Shared types live in `super::types`; the error channel in `crate::scaffolded`.
//!
//! ---
//!
//! # Modeled contract for `GetBackupVaultNotifications`
//!
//! What AWS documents for this operation, from this service's Smithy model. HTML is
//! stripped and long entries are clipped at ` …`. For the FULL text of any one item,
//! ask the model rather than reading the raw JSON:
//!
//! ```text
//! smithy_query(query="doc",shape="<Shape>",member="<Member>",model_path="services/service-com-amazonaws-backup-cryocontrollerusermanager/smithy-model/com-amazonaws-backup-cryocontrollerusermanager.json")
//! ```
//!
//! Returns event notifications for the specified backup vault.
//!
//! ## Input
//!
//! * `BackupVaultName` (BackupVaultName, pattern ^[a-zA-Z0-9\-\_]{2,50}$) **required** — The
//!   name of a logical container where backups are stored. Backup vaults are identified by
//!   names that are unique to the account used to create them and the Amazon Web Services
//!   Region where they are created.
//!
//! ## Output
//!
//! * `BackupVaultArn` (ARN) — An Amazon Resource Name (ARN) that uniquely identifies a backup
//!   vault; for example, arn:aws:backup:us-east-1:123456789012:backup-vault:aBackupVault.
//! * `BackupVaultEvents` (BackupVaultEvents) — An array of events that indicate the status of
//!   jobs to back up resources to the backup vault.
//! * `BackupVaultName` (BackupVaultName, pattern ^[a-zA-Z0-9\-\_]{2,50}$) — The name of a
//!   logical container where backups are stored. Backup vaults are identified by names that
//!   are unique to the account used to create them and the Region where they are created.
//! * `SNSTopicArn` (ARN) — An ARN that uniquely identifies an Amazon Simple Notification
//!   Service (Amazon SNS) topic; for example, arn:aws:sns:us-west-2:111122223333:MyTopic.
//!
//! ## Errors
//!
//! * `InvalidParameterValueException` (HTTP 400) — Indicates that something is wrong with a
//!   parameter's value. For example, the value is out of range.
//! * `MissingParameterValueException` (HTTP 400) — Indicates that a required parameter is
//!   missing.
//! * `ResourceNotFoundException` (HTTP 400) — A resource that is required for the action
//!   doesn't exist.
//! * `ServiceUnavailableException` (HTTP 500) — The request failed due to a temporary failure
//!   of the server.

#[allow(unused_imports)]
use super::types::*;

// `#[operation_error]` below expands to an invocation of the LOCAL declarative
// `smithy!` macro, which this line is what emits into the module. Without it the
// expansion fails with `cannot find macro 'smithy' in this scope` and the
// `GetBackupVaultNotificationsError` enum is never produced.
smithy_macros::smithy_model!("smithy-model/com-amazonaws-backup-cryocontrollerusermanager.json");

// The operation's MODELED errors. `#[operation_error]` accepts only variants the
// model declares for this operation, so the scaffold's `NotImplemented` cannot live
// here — hence the `GetBackupVaultNotificationsError` alias below. An operation that models no errors
// generates an UNINHABITED enum here, which is correct and expected: there is nothing
// to construct until the model says otherwise.
#[smithy_macros::operation_error("com.amazonaws.backup#GetBackupVaultNotifications")]
#[derive(Debug)]
pub enum GetBackupVaultNotificationsErrorModeled {
    InvalidParameterValueException(InvalidParameterValueException),
    MissingParameterValueException(MissingParameterValueException),
    ResourceNotFoundException(ResourceNotFoundException),
    ServiceUnavailableException(ServiceUnavailableException),
}

/// This operation's error channel, as the generated `impl Operation` names it
/// (`crate::ops::get_backup_vault_notifications::GetBackupVaultNotificationsError`). Wrapping the modeled enum is what
/// lets a scaffolded operation answer 501 for one whose model declares no errors at all.
pub type GetBackupVaultNotificationsError = crate::scaffolded::Scaffolded<GetBackupVaultNotificationsErrorModeled>;

/// Lets `?` and `.into()` lift a modeled failure into the error channel, so an
/// implemented handler writes `Err(SomeModeledError.into())` rather than naming the
/// wrapper variant. See `crate::scaffolded` for how to drop the wrapper entirely once
/// this operation is implemented.
impl From<GetBackupVaultNotificationsErrorModeled> for GetBackupVaultNotificationsError {
    fn from(modeled: GetBackupVaultNotificationsErrorModeled) -> Self {
        crate::scaffolded::Scaffolded::Modeled(modeled)
    }
}

/// The `GetBackupVaultNotifications` operation.
///
/// Owns the shared engine (the constructor seam) and implements
/// [`smithy_server_runtime::Operation`]. Generic over the engine type `Eng`, so this
/// crate names no concrete engine type: a migrated service that needs state picks a
/// concrete engine where it mounts the service, not here.
pub struct GetBackupVaultNotifications<Eng> {
    engine: ::std::sync::Arc<Eng>,
}

impl<Eng> GetBackupVaultNotifications<Eng> {
    /// Construct the operation from the shared engine. INFALLIBLE — the engine is a
    /// construction-time dependency, so engine access is guaranteed by the time any
    /// request runs. Called by the generated `EmulatedService::new(engine)` constructor.
    pub fn new(engine: ::std::sync::Arc<Eng>) -> Self {
        Self { engine }
    }
}

impl<Eng: ::std::marker::Send + ::std::marker::Sync + 'static> smithy_server_runtime::Operation
    for GetBackupVaultNotifications<Eng>
{
    type Input = GetBackupVaultNotificationsInput;
    type Output = GetBackupVaultNotificationsOutput;
    type Error = GetBackupVaultNotificationsError;

    const NAME: &'static str = "GetBackupVaultNotifications";
    const INPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::GET_BACKUP_VAULT_NOTIFICATIONS_INPUT_SCHEMA;
    const OUTPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::GET_BACKUP_VAULT_NOTIFICATIONS_OUTPUT_SCHEMA;
    const SUCCESS_STATUS: u16 = 200u16;

    async fn handle(
        &self,
        input: Self::Input,
        ctx: &smithy_server_runtime::OperationContext<'_>,
    ) -> Result<Self::Output, Self::Error> {
        let region = crate::state::request_region(ctx);
        let now = ::std::time::SystemTime::now();
        let state = crate::state::service_state(&*self.engine).ok_or_else(|| {
            GetBackupVaultNotificationsErrorModeled::ServiceUnavailableException(
                ServiceUnavailableException {
                    message: Some("the backup vault store is unavailable".to_string()),
                    ..Default::default()
                },
            )
        })?;
        let mut store = state.lock();
        // The one write a read performs: the `CREATING` -> `AVAILABLE`
        // transition the clock had already made due for this vault.
        store.settle_vault(&region, &input.backup_vault_name, now);
        let vault = store.visible_vault(&region, &input.backup_vault_name, None)?;
        let Some(notifications) = vault.notifications.clone() else {
            return Err(GetBackupVaultNotificationsErrorModeled::ResourceNotFoundException(
                crate::state::vault_notifications_not_found(),
            )
            .into());
        };
        Ok(GetBackupVaultNotificationsOutput {
            backup_vault_name: Some(vault.name.clone()),
            backup_vault_arn: Some(vault.arn.clone()),
            sns_topic_arn: Some(notifications.sns_topic_arn),
            backup_vault_events: Some(notifications.events),
        })
    }
}
