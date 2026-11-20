// SPDX-License-Identifier: CC0-1.0

//! @op AssociateEncryptionConfig
//!
//! IMPLEMENTED. `AssociateEncryptionConfig` starts one asynchronous update on an
//! existing cluster: it validates the requested `encryptionConfig`, mints a
//! [`crate::state::UpdateRecord`] on that cluster's record in the shared engine's store
//! and answers with the `Update` it just wrote. `DescribeUpdate` reads back the same
//! record, and `DescribeCluster` reports the cluster as `UPDATING` until it completes.
//!
//! The update reports `InProgress` and the clock owes it `Successful`
//! [`crate::state::UPDATE_COMPLETION_DELAY`] later; the due instant is stored on the
//! record, and whichever read first runs after it materialises both the update's status
//! and the `encryptionConfig` it puts on the cluster.
//!
//! Shared types live in `super::types`; the error channel in `crate::scaffolded`.
//!
//! ---
//!
//! # Modeled contract for `AssociateEncryptionConfig`
//!
//! What AWS documents for this operation, from this service's Smithy model. HTML is
//! stripped and long entries are clipped at ` …`. For the FULL text of any one item,
//! ask the model rather than reading the raw JSON:
//!
//! ```text
//! smithy_query(query="doc",shape="<Shape>",member="<Member>",model_path="services/service-com-amazonaws-eks-awswesleyfrontend/smithy-model/com-amazonaws-eks-awswesleyfrontend.json")
//! ```
//!
//! Associates an encryption configuration to an existing cluster. Use this API to enable
//! encryption on existing clusters that don't already have encryption enabled. This allows
//! you to implement a defense-in-depth security strategy without migrating applications to
//! new Amazon EKS clusters.
//!
//! ## Input
//!
//! * `clientRequestToken` (String, @idempotencyToken) — A unique, case-sensitive identifier
//!   that you provide to ensure the idempotency of the request.
//! * `clusterName` (String) **required** — The name of your cluster.
//! * `encryptionConfig` (EncryptionConfigList, len 0..1) **required** — The configuration you
//!   are using for encryption.
//!
//! ## Output
//!
//! * `update` (Update) — An object representing an asynchronous update.
//!
//! ## Errors
//!
//! * `ClientException` (HTTP 400) — These errors are usually caused by a client action.
//!   Actions can include using an action or resource on behalf of an IAM principal that
//!   doesn't have permissions to use the action or resource or specifying an identifier that
//!   is not valid.
//! * `InvalidParameterException` (HTTP 400) — The specified parameter is invalid. Review the
//!   available parameters for the API request.
//! * `InvalidRequestException` (HTTP 400) — The request is invalid given the state of the
//!   cluster. Check the state of the cluster and the associated operations.
//! * `ResourceInUseException` (HTTP 409) — The specified resource is in use.
//! * `ResourceNotFoundException` (HTTP 404) — The specified resource could not be found. You
//!   can view your available clusters with ListClusters. You can view your available managed
//!   node groups with ListNodegroups. Amazon EKS clusters and node groups are Amazon Web
//!   Services Region specific.
//! * `ServerException` (HTTP 500) — These errors are usually caused by a server-side issue.
//! * `ThrottlingException` (HTTP 429) — The request or operation couldn't be performed
//!   because a service is throttling requests.

use std::time::SystemTime;

#[allow(unused_imports)]
use super::types::*;
use crate::state::{
    self, PendingClusterConfig, UpdateRecord, UpdateScope, STATUS_ACTIVE, STATUS_UPDATING,
    UPDATE_COMPLETION_DELAY, UPDATE_IN_PROGRESS, UPDATE_PARAM_ENCRYPTION_CONFIG,
    UPDATE_TYPE_ASSOCIATE_ENCRYPTION_CONFIG,
};

// `#[operation_error]` below expands to an invocation of the LOCAL declarative
// `smithy!` macro, which this line is what emits into the module. Without it the
// expansion fails with `cannot find macro 'smithy' in this scope` and the
// `AssociateEncryptionConfigError` enum is never produced.
smithy_macros::smithy_model!("smithy-model/com-amazonaws-eks-awswesleyfrontend.json");

// The operation's MODELED errors. `#[operation_error]` accepts only variants the
// model declares for this operation, so the scaffold's `NotImplemented` cannot live
// here — hence the `AssociateEncryptionConfigError` alias below. An operation that models no errors
// generates an UNINHABITED enum here, which is correct and expected: there is nothing
// to construct until the model says otherwise.
#[smithy_macros::operation_error("com.amazonaws.eks#AssociateEncryptionConfig")]
#[derive(Debug)]
pub enum AssociateEncryptionConfigErrorModeled {
    ClientException(ClientException),
    InvalidParameterException(InvalidParameterException),
    InvalidRequestException(InvalidRequestException),
    ResourceInUseException(ResourceInUseException),
    ResourceNotFoundException(ResourceNotFoundException),
    ServerException(ServerException),
    ThrottlingException(ThrottlingException),
}

/// This operation's error channel, as the generated `impl Operation` names it
/// (`crate::ops::associate_encryption_config::AssociateEncryptionConfigError`). Wrapping the modeled enum is what
/// lets a scaffolded operation answer 501 for one whose model declares no errors at all.
pub type AssociateEncryptionConfigError = crate::scaffolded::Scaffolded<AssociateEncryptionConfigErrorModeled>;

/// Lets `?` and `.into()` lift a modeled failure into the error channel, so an
/// implemented handler writes `Err(SomeModeledError.into())` rather than naming the
/// wrapper variant. See `crate::scaffolded` for how to drop the wrapper entirely once
/// this operation is implemented.
impl From<AssociateEncryptionConfigErrorModeled> for AssociateEncryptionConfigError {
    fn from(modeled: AssociateEncryptionConfigErrorModeled) -> Self {
        crate::scaffolded::Scaffolded::Modeled(modeled)
    }
}

/// The `AssociateEncryptionConfig` operation.
///
/// Owns the shared engine (the constructor seam) and implements
/// [`smithy_server_runtime::Operation`]. Generic over the engine type `Eng`, so this
/// crate names no concrete engine type: a migrated service that needs state picks a
/// concrete engine where it mounts the service, not here.
pub struct AssociateEncryptionConfig<Eng> {
    engine: ::std::sync::Arc<Eng>,
}

impl<Eng> AssociateEncryptionConfig<Eng> {
    /// Construct the operation from the shared engine. INFALLIBLE — the engine is a
    /// construction-time dependency, so engine access is guaranteed by the time any
    /// request runs. Called by the generated `EmulatedService::new(engine)` constructor.
    pub fn new(engine: ::std::sync::Arc<Eng>) -> Self {
        Self { engine }
    }
}

impl<Eng: ::std::marker::Send + ::std::marker::Sync + 'static> smithy_server_runtime::Operation
    for AssociateEncryptionConfig<Eng>
{
    type Input = AssociateEncryptionConfigInput;
    type Output = AssociateEncryptionConfigOutput;
    type Error = AssociateEncryptionConfigError;

    const NAME: &'static str = "AssociateEncryptionConfig";
    const INPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::ASSOCIATE_ENCRYPTION_CONFIG_INPUT_SCHEMA;
    const OUTPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::ASSOCIATE_ENCRYPTION_CONFIG_OUTPUT_SCHEMA;
    const SUCCESS_STATUS: u16 = 200u16;

    async fn handle(
        &self,
        input: Self::Input,
        ctx: &smithy_server_runtime::OperationContext<'_>,
    ) -> Result<Self::Output, Self::Error> {
        let cluster_name = input.cluster_name.clone();
        let region = state::request_region(ctx.parts());
        let store = crate::state::service_state(&*self.engine).ok_or_else(|| {
            AssociateEncryptionConfigError::from(
                AssociateEncryptionConfigErrorModeled::ServerException(ServerException {
                    cluster_name: Some(cluster_name.clone()),
                    message: Some("the eks emulator store is unavailable".to_string()),
                    ..Default::default()
                }),
            )
        })?;

        let now = SystemTime::now();
        let mut guard = store.lock();
        guard.materialize_due_transitions(now);

        // A cluster name identifies a cluster only within one region, so the record this
        // updates is the one in the region the request is signed for. The cluster is
        // resolved BEFORE the request body is judged: `clusterName` is the `@httpLabel`
        // that names the resource being acted on, and a call against a cluster that does
        // not exist is a 404 whatever else the body carries.
        let key = (region.clone(), cluster_name.clone());
        let Some(record) = guard.clusters.get(&key) else {
            return Err(
                AssociateEncryptionConfigErrorModeled::ResourceNotFoundException(
                    crate::state::cluster_not_found(&cluster_name),
                )
                .into(),
            );
        };

        // An encryption configuration encrypts the secrets of an Amazon EKS control
        // plane, and a cluster `RegisterCluster` connected has none: this operation does
        // not apply to it at all, which is what the connected-cluster gate reports. The
        // gate is a fact about the CLUSTER, so it is answered as soon as the cluster is
        // resolved — before anything in the body is judged against it, and before the
        // cluster's STATUS is, because no wait makes a connected cluster take this call.
        if record.is_connected() {
            return Err(AssociateEncryptionConfigErrorModeled::ClientException(
                crate::state::connected_cluster_refusal(&cluster_name),
            )
            .into());
        }

        // `clientRequestToken` is `@idempotencyToken`: an SDK generates it once per call
        // and RESENDS the same value when it retries, so a retry of the call that started
        // an update is answered with THAT update rather than starting a second one. A
        // blank token identifies nothing, so it never matches.
        let replayed_token = input
            .client_request_token
            .as_deref()
            .map(str::trim)
            .filter(|token| !token.is_empty());
        if let Some(update) = replayed_token.and_then(|token| record.update_for_token(token)) {
            return Ok(AssociateEncryptionConfigOutput {
                update: Some(update.to_update()),
            });
        }

        // One update at a time per cluster: while another is running the cluster is in
        // use, which is exactly what `ResourceInUseException` reports, worded by
        // [`crate::state::update_in_progress`] for the update type this call would have
        // minted.
        if let Some(in_flight) = record.in_flight_update() {
            return Err(
                AssociateEncryptionConfigErrorModeled::ResourceInUseException(
                    crate::state::update_in_progress(
                        UPDATE_TYPE_ASSOCIATE_ENCRYPTION_CONFIG,
                        &cluster_name,
                        in_flight,
                    ),
                )
                .into(),
            );
        }

        // An update can only be started on a cluster that is running: one still being
        // created, or being deleted, or that failed, cannot take one.
        // `InvalidRequestException` is the modelled error for exactly that — "the request
        // is invalid given the state of the cluster".
        if record.status != STATUS_ACTIVE {
            return Err(
                AssociateEncryptionConfigErrorModeled::InvalidRequestException(
                    InvalidRequestException {
                        cluster_name: Some(cluster_name.clone()),
                        message: Some(format!(
                            "Cluster {cluster_name} is in {} status; it must be {STATUS_ACTIVE} \
                             to associate an encryption configuration",
                            record.status
                        )),
                        ..Default::default()
                    },
                )
                .into(),
            );
        }

        // "Use this API to enable encryption on existing clusters that don't already have
        // encryption enabled": the association is one-way, so a cluster that already
        // reports an `encryptionConfig` cannot take another. What the caller has to change
        // is the `encryptionConfig` it sent — no wait makes this call succeed — so this is
        // the PARAMETER refusal, in the sentence this API states for it
        // ([`crate::state::ENCRYPTION_ALREADY_ENABLED`]).
        if !record.encryption_config.is_empty() {
            return Err(
                AssociateEncryptionConfigErrorModeled::InvalidParameterException(
                    InvalidParameterException {
                        cluster_name: Some(cluster_name.clone()),
                        message: Some(state::ENCRYPTION_ALREADY_ENABLED.to_string()),
                        ..Default::default()
                    },
                )
                .into(),
            );
        }

        // Everything the model cannot express about `encryptionConfig` is judged here,
        // through the one reading of that member this service has
        // ([`crate::state::encryption_config_key_arn`], shared with `CreateCluster`); the
        // modelled constraints (required, at most one entry) were already refused before
        // this handler ran.
        let key_arn = state::encryption_config_key_arn(&input.encryption_config, &region)
            .map_err(|message| {
                AssociateEncryptionConfigError::from(
                    AssociateEncryptionConfigErrorModeled::InvalidParameterException(
                        InvalidParameterException {
                            cluster_name: Some(cluster_name.clone()),
                            message: Some(message),
                            ..Default::default()
                        },
                    ),
                )
            })?;

        let sequence = guard.next_update_sequence();
        let id = state::new_update_id(sequence);
        let update = UpdateRecord {
            id: id.clone(),
            r#type: UPDATE_TYPE_ASSOCIATE_ENCRYPTION_CONFIG.to_string(),
            status: UPDATE_IN_PROGRESS.to_string(),
            created_at: now,
            // "A key-value map that contains the parameters associated with the update":
            // one entry, keyed by the `UpdateParamType` for an encryption configuration,
            // whose value is the requested configuration in the wire form it arrived in.
            params: vec![UpdateParam {
                r#type: Some(UPDATE_PARAM_ENCRYPTION_CONFIG.to_string()),
                value: Some(encryption_config_param(&input.encryption_config, &key_arn)),
            }],
            client_request_token: replayed_token.map(str::to_owned),
            succeeds_at: Some(now + UPDATE_COMPLETION_DELAY),
            pending_config: Some(PendingClusterConfig {
                encryption_config: Some(input.encryption_config.clone()),
                ..Default::default()
            }),
            // "This field is present only if any cancellation is attempted for the
            // update": none has been, so `CancelUpdate` is the only writer of either.
            cancellation: None,
            cancellation_request_token: None,
            // The encryption configuration is the cluster's own, so this is an
            // update of the cluster rather than of a resource inside it.
            scope: UpdateScope::Cluster,
        };
        let answer = update.to_update();

        let record = guard.clusters.get_mut(&key).ok_or_else(|| {
            AssociateEncryptionConfigError::from(
                AssociateEncryptionConfigErrorModeled::ServerException(ServerException {
                    cluster_name: Some(cluster_name.clone()),
                    message: Some("the cluster record disappeared while updating it".to_string()),
                    ..Default::default()
                }),
            )
        })?;
        // The cluster is under an update until the clock completes it, and the
        // `encryptionConfig` it reports changes only then — the update is asynchronous,
        // so the association is not visible on the cluster the instant it is requested.
        record.status = STATUS_UPDATING.to_string();
        record.updates.insert(id, update);

        Ok(AssociateEncryptionConfigOutput {
            update: Some(answer),
        })
    }
}

/// The requested configuration as an `UpdateParam$value`: the `encryptionConfig` list in
/// the same JSON shape it arrived in, so a caller reading the update back sees what it
/// asked for rather than a rendering of this service's internals.
fn encryption_config_param(entries: &[EncryptionConfig], key_arn: &str) -> String {
    let resources = entries
        .first()
        .and_then(|entry| entry.resources.clone())
        .unwrap_or_default();
    let mut entry = ::serde_json::Map::new();
    if !resources.is_empty() {
        entry.insert("resources".to_string(), ::serde_json::json!(resources));
    }
    entry.insert(
        "provider".to_string(),
        ::serde_json::json!({ "keyArn": key_arn }),
    );
    ::serde_json::Value::Array(vec![::serde_json::Value::Object(entry)]).to_string()
}
