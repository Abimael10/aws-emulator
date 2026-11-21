// SPDX-License-Identifier: CC0-1.0

//! @op DeleteBackupVaultNotifications
//!
//! Turns notifications off for a backup vault, by clearing the member
//! [`PutBackupVaultNotifications`](crate::ops::put_backup_vault_notifications)
//! stores them in ([`crate::state::BackupVault::notifications`]) — so after
//! this call the vault is in the state a vault that was never configured is in,
//! and [`GetBackupVaultNotifications`](crate::ops::get_backup_vault_notifications)
//! answers for it exactly as it does for that vault.
//!
//! Two steps, in the order AWS Backup applies them:
//!
//! * the named vault is resolved by the step every vault-scoped write shares
//!   ([`crate::state::ComAmazonawsBackupCryocontrollerusermanagerState::visible_vault_mut`]),
//!   which decides access before existence, so a vault the caller cannot see is
//!   never disclosed;
//! * the vault's one notification member is cleared. A vault carrying no
//!   configuration has nothing left to delete, so the call answers the same
//!   success: the delete is `@idempotent` (the model's trait for it) and states
//!   the end state, not what it had to change to reach it.
//!
//! Shared types live in `super::types`; the error channel in `crate::scaffolded`.
//!
//! ---
//!
//! # Modeled contract for `DeleteBackupVaultNotifications`
//!
//! What AWS documents for this operation, from this service's Smithy model. HTML is
//! stripped and long entries are clipped at ` …`. For the FULL text of any one item,
//! ask the model rather than reading the raw JSON:
//!
//! ```text
//! smithy_query(query="doc",shape="<Shape>",member="<Member>",model_path="services/service-com-amazonaws-backup-cryocontrollerusermanager/smithy-model/com-amazonaws-backup-cryocontrollerusermanager.json")
//! ```
//!
//! Deletes event notifications for the specified backup vault.
//!
//! ## Input
//!
//! * `BackupVaultName` (BackupVaultName, pattern ^[a-zA-Z0-9\-\_]{2,50}$) **required** — The
//!   name of a logical container where backups are stored. Backup vaults are identified by
//!   names that are unique to the account used to create them and the Region where they are
//!   created.
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
// `DeleteBackupVaultNotificationsError` enum is never produced.
smithy_macros::smithy_model!("smithy-model/com-amazonaws-backup-cryocontrollerusermanager.json");

#[derive(Debug, Clone, Default)]
pub struct DeleteBackupVaultNotificationsOutput;

impl DeleteBackupVaultNotificationsOutput {
    // `with_http` carries this operation's modelled response status. Remove it and
    // the status falls back to the protocol default, so a modelled 204 answers 200.
    pub const SCHEMA: ::aws_smithy_schema::Schema = ::aws_smithy_schema::Schema::new_struct(
        ::aws_smithy_schema::ShapeId::from_static("smithy.api#Unit", "smithy.api", "Unit"),
        ::aws_smithy_schema::ShapeType::Structure,
        &[],
    )
    .with_http(::aws_smithy_schema::traits::HttpTrait::new(
        "DELETE",
        "/backup-vaults/{BackupVaultName}/notification-configuration",
        Some(200u16),
    ));
}

pub static DELETE_BACKUP_VAULT_NOTIFICATIONS_OUTPUT_SCHEMA: &::aws_smithy_schema::Schema = &DeleteBackupVaultNotificationsOutput::SCHEMA;

impl ::aws_smithy_schema::serde::SerializableStruct for DeleteBackupVaultNotificationsOutput {
    fn serialize_members(&self, _s: &mut dyn ::aws_smithy_schema::serde::ShapeSerializer) -> Result<(), ::aws_smithy_schema::serde::SerdeError> { Ok(()) }
}

// The operation's MODELED errors. `#[operation_error]` accepts only variants the
// model declares for this operation, so the scaffold's `NotImplemented` cannot live
// here — hence the `DeleteBackupVaultNotificationsError` alias below. An operation that models no errors
// generates an UNINHABITED enum here, which is correct and expected: there is nothing
// to construct until the model says otherwise.
#[smithy_macros::operation_error("com.amazonaws.backup#DeleteBackupVaultNotifications")]
#[derive(Debug)]
pub enum DeleteBackupVaultNotificationsErrorModeled {
    InvalidParameterValueException(InvalidParameterValueException),
    MissingParameterValueException(MissingParameterValueException),
    ResourceNotFoundException(ResourceNotFoundException),
    ServiceUnavailableException(ServiceUnavailableException),
}

/// This operation's error channel, as the generated `impl Operation` names it
/// (`crate::ops::delete_backup_vault_notifications::DeleteBackupVaultNotificationsError`). Wrapping the modeled enum is what
/// lets a scaffolded operation answer 501 for one whose model declares no errors at all.
pub type DeleteBackupVaultNotificationsError = crate::scaffolded::Scaffolded<DeleteBackupVaultNotificationsErrorModeled>;

/// Lets `?` and `.into()` lift a modeled failure into the error channel, so an
/// implemented handler writes `Err(SomeModeledError.into())` rather than naming the
/// wrapper variant. See `crate::scaffolded` for how to drop the wrapper entirely once
/// this operation is implemented.
impl From<DeleteBackupVaultNotificationsErrorModeled> for DeleteBackupVaultNotificationsError {
    fn from(modeled: DeleteBackupVaultNotificationsErrorModeled) -> Self {
        crate::scaffolded::Scaffolded::Modeled(modeled)
    }
}

/// The `DeleteBackupVaultNotifications` operation.
///
/// Owns the shared engine (the constructor seam) and implements
/// [`smithy_server_runtime::Operation`]. Generic over the engine type `Eng`, so this
/// crate names no concrete engine type: a migrated service that needs state picks a
/// concrete engine where it mounts the service, not here.
pub struct DeleteBackupVaultNotifications<Eng> {
    engine: ::std::sync::Arc<Eng>,
}

impl<Eng> DeleteBackupVaultNotifications<Eng> {
    /// Construct the operation from the shared engine. INFALLIBLE — the engine is a
    /// construction-time dependency, so engine access is guaranteed by the time any
    /// request runs. Called by the generated `EmulatedService::new(engine)` constructor.
    pub fn new(engine: ::std::sync::Arc<Eng>) -> Self {
        Self { engine }
    }
}

impl<Eng: ::std::marker::Send + ::std::marker::Sync + 'static> smithy_server_runtime::Operation
    for DeleteBackupVaultNotifications<Eng>
{
    type Input = DeleteBackupVaultNotificationsInput;
    type Output = DeleteBackupVaultNotificationsOutput;
    type Error = DeleteBackupVaultNotificationsError;

    const NAME: &'static str = "DeleteBackupVaultNotifications";
    const INPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::DELETE_BACKUP_VAULT_NOTIFICATIONS_INPUT_SCHEMA;
    const OUTPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = DELETE_BACKUP_VAULT_NOTIFICATIONS_OUTPUT_SCHEMA;
    const SUCCESS_STATUS: u16 = 200u16;

    async fn handle(
        &self,
        input: Self::Input,
        ctx: &smithy_server_runtime::OperationContext<'_>,
    ) -> Result<Self::Output, Self::Error> {
        let region = crate::state::request_region(ctx);
        let now = ::std::time::SystemTime::now();
        let state = crate::state::service_state(&*self.engine).ok_or_else(|| {
            DeleteBackupVaultNotificationsErrorModeled::ServiceUnavailableException(
                ServiceUnavailableException {
                    message: Some("the backup vault store is unavailable".to_string()),
                    ..Default::default()
                },
            )
        })?;
        let mut store = state.lock();
        // The write is on the vault, so the transition the clock owes it is
        // materialised first and the vault resolved is the vault as it stands
        // now. A vault-scoped write decides access before existence through the
        // step every such call shares; this input models no
        // `BackupVaultAccountId`.
        store.settle_vault(&region, &input.backup_vault_name, now);
        let vault = store.visible_vault_mut(&region, &input.backup_vault_name, None)?;
        // A vault carries at most one notification configuration, so turning
        // notifications off is clearing that one member: the vault is left as a
        // vault that was never configured, which is what the notifications read
        // then answers for it.
        vault.notifications = None;
        Ok(DeleteBackupVaultNotificationsOutput)
    }
}
