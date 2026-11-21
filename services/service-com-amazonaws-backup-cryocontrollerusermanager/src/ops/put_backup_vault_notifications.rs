// SPDX-License-Identifier: CC0-1.0

//! @op PutBackupVaultNotifications
//!
//! Turns notifications on for a backup vault: the topic and the events the
//! request states are stored on the named vault in [`crate::state`]
//! ([`crate::state::BackupVault::notifications`]) — the SAME stored vault
//! `GetBackupVaultNotifications` reports them from and
//! `DeleteBackupVaultNotifications` clears them on.
//!
//! Three steps, in the order AWS Backup applies them:
//!
//! * `SNSTopicArn` is judged BEFORE the vault is resolved and before anything
//!   is stored, so a refused call configures nothing: the model types the
//!   member as `ARN` with no pattern, so a value that is not an ARN at all is a
//!   parameter value this service refuses ([`crate::state::rejected_arn`])
//!   rather than a topic it would publish to;
//! * the named vault is resolved by the step every vault-scoped write shares
//!   ([`crate::state::ComAmazonawsBackupCryocontrollerusermanagerState::visible_vault_mut`]),
//!   which decides access before existence, so a vault the caller cannot see is
//!   never disclosed;
//! * the configuration is stored on that vault. A vault carries at most one, so
//!   a later accepted call REPLACES the topic and the event list before it
//!   rather than adding to them.
//!
//! The event list is stored exactly as stated: every entry is a token of the
//! model's `BackupVaultEvent` enum by the time this handler runs, since the
//! framework's boundary validator holds each one to that set, and the list has
//! no part this service fills in.
//!
//! Shared types live in `super::types`; the error channel in `crate::scaffolded`.
//!
//! ---
//!
//! # Modeled contract for `PutBackupVaultNotifications`
//!
//! What AWS documents for this operation, from this service's Smithy model. HTML is
//! stripped and long entries are clipped at ` …`. For the FULL text of any one item,
//! ask the model rather than reading the raw JSON:
//!
//! ```text
//! smithy_query(query="doc",shape="<Shape>",member="<Member>",model_path="services/service-com-amazonaws-backup-cryocontrollerusermanager/smithy-model/com-amazonaws-backup-cryocontrollerusermanager.json")
//! ```
//!
//! Turns on notifications on a backup vault for the specified topic and events.
//!
//! ## Input
//!
//! * `BackupVaultEvents` (BackupVaultEvents) **required** — An array of events that indicate
//!   the status of jobs to back up resources to the backup vault. For the list of supported
//!   events, common use cases, and code samples, see Notification options with Backup.
//! * `BackupVaultName` (BackupVaultName, pattern ^[a-zA-Z0-9\-\_]{2,50}$) **required** — The
//!   name of a logical container where backups are stored. Backup vaults are identified by
//!   names that are unique to the account used to create them and the Amazon Web Services
//!   Region where they are created.
//! * `SNSTopicArn` (ARN) **required** — The Amazon Resource Name (ARN) that specifies the
//!   topic for a backup vault’s events; for example,
//!   arn:aws:sns:us-west-2:111122223333:MyVaultTopic.
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
// `PutBackupVaultNotificationsError` enum is never produced.
smithy_macros::smithy_model!("smithy-model/com-amazonaws-backup-cryocontrollerusermanager.json");

#[derive(Debug, Clone, Default)]
pub struct PutBackupVaultNotificationsOutput;

impl PutBackupVaultNotificationsOutput {
    // `with_http` carries this operation's modelled response status. Remove it and
    // the status falls back to the protocol default, so a modelled 204 answers 200.
    pub const SCHEMA: ::aws_smithy_schema::Schema = ::aws_smithy_schema::Schema::new_struct(
        ::aws_smithy_schema::ShapeId::from_static("smithy.api#Unit", "smithy.api", "Unit"),
        ::aws_smithy_schema::ShapeType::Structure,
        &[],
    )
    .with_http(::aws_smithy_schema::traits::HttpTrait::new(
        "PUT",
        "/backup-vaults/{BackupVaultName}/notification-configuration",
        Some(200u16),
    ));
}

pub static PUT_BACKUP_VAULT_NOTIFICATIONS_OUTPUT_SCHEMA: &::aws_smithy_schema::Schema = &PutBackupVaultNotificationsOutput::SCHEMA;

impl ::aws_smithy_schema::serde::SerializableStruct for PutBackupVaultNotificationsOutput {
    fn serialize_members(&self, _s: &mut dyn ::aws_smithy_schema::serde::ShapeSerializer) -> Result<(), ::aws_smithy_schema::serde::SerdeError> { Ok(()) }
}

// The operation's MODELED errors. `#[operation_error]` accepts only variants the
// model declares for this operation, so the scaffold's `NotImplemented` cannot live
// here — hence the `PutBackupVaultNotificationsError` alias below. An operation that models no errors
// generates an UNINHABITED enum here, which is correct and expected: there is nothing
// to construct until the model says otherwise.
#[smithy_macros::operation_error("com.amazonaws.backup#PutBackupVaultNotifications")]
#[derive(Debug)]
pub enum PutBackupVaultNotificationsErrorModeled {
    InvalidParameterValueException(InvalidParameterValueException),
    MissingParameterValueException(MissingParameterValueException),
    ResourceNotFoundException(ResourceNotFoundException),
    ServiceUnavailableException(ServiceUnavailableException),
}

/// This operation's error channel, as the generated `impl Operation` names it
/// (`crate::ops::put_backup_vault_notifications::PutBackupVaultNotificationsError`). Wrapping the modeled enum is what
/// lets a scaffolded operation answer 501 for one whose model declares no errors at all.
pub type PutBackupVaultNotificationsError = crate::scaffolded::Scaffolded<PutBackupVaultNotificationsErrorModeled>;

/// Lets `?` and `.into()` lift a modeled failure into the error channel, so an
/// implemented handler writes `Err(SomeModeledError.into())` rather than naming the
/// wrapper variant. See `crate::scaffolded` for how to drop the wrapper entirely once
/// this operation is implemented.
impl From<PutBackupVaultNotificationsErrorModeled> for PutBackupVaultNotificationsError {
    fn from(modeled: PutBackupVaultNotificationsErrorModeled) -> Self {
        crate::scaffolded::Scaffolded::Modeled(modeled)
    }
}

/// The `PutBackupVaultNotifications` operation.
///
/// Owns the shared engine (the constructor seam) and implements
/// [`smithy_server_runtime::Operation`]. Generic over the engine type `Eng`, so this
/// crate names no concrete engine type: a migrated service that needs state picks a
/// concrete engine where it mounts the service, not here.
pub struct PutBackupVaultNotifications<Eng> {
    engine: ::std::sync::Arc<Eng>,
}

impl<Eng> PutBackupVaultNotifications<Eng> {
    /// Construct the operation from the shared engine. INFALLIBLE — the engine is a
    /// construction-time dependency, so engine access is guaranteed by the time any
    /// request runs. Called by the generated `EmulatedService::new(engine)` constructor.
    pub fn new(engine: ::std::sync::Arc<Eng>) -> Self {
        Self { engine }
    }
}

impl<Eng: ::std::marker::Send + ::std::marker::Sync + 'static> smithy_server_runtime::Operation
    for PutBackupVaultNotifications<Eng>
{
    type Input = PutBackupVaultNotificationsInput;
    type Output = PutBackupVaultNotificationsOutput;
    type Error = PutBackupVaultNotificationsError;

    const NAME: &'static str = "PutBackupVaultNotifications";
    const INPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::PUT_BACKUP_VAULT_NOTIFICATIONS_INPUT_SCHEMA;
    const OUTPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = PUT_BACKUP_VAULT_NOTIFICATIONS_OUTPUT_SCHEMA;
    const SUCCESS_STATUS: u16 = 200u16;

    async fn handle(
        &self,
        input: Self::Input,
        ctx: &smithy_server_runtime::OperationContext<'_>,
    ) -> Result<Self::Output, Self::Error> {
        let region = crate::state::request_region(ctx);
        let now = ::std::time::SystemTime::now();

        // The topic is judged before the vault is resolved and before anything
        // is stored, so a refused call leaves the vault configured exactly as
        // it was: the member names a topic by ARN, and the check is the one
        // every `ARN`-typed member of this service shares.
        if let Some(refusal) = crate::state::rejected_arn("SNSTopicArn", &input.sns_topic_arn) {
            return Err(
                PutBackupVaultNotificationsErrorModeled::InvalidParameterValueException(refusal)
                    .into(),
            );
        }

        let state = crate::state::service_state(&*self.engine).ok_or_else(|| {
            PutBackupVaultNotificationsErrorModeled::ServiceUnavailableException(
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
        // A vault carries at most one notification configuration, so an
        // accepted call REPLACES whatever it carried.
        vault.notifications = Some(crate::state::VaultNotifications {
            sns_topic_arn: input.sns_topic_arn,
            events: input.backup_vault_events,
        });
        Ok(PutBackupVaultNotificationsOutput)
    }
}
