// SPDX-License-Identifier: CC0-1.0

//! How the master user secret RDS MANAGES is rotated when the rotation is asked of SECRETS
//! MANAGER rather than of this service.
//!
//! The secret holding a resource's master user password is one RDS owns, and its rotation is
//! RDS's own work: a new password has to be generated, staked on the resource it logs in to,
//! and written to the secret — "the secret value contains the updated password"
//! (`ModifyDBClusterMessage$RotateMasterUserPassword`). A caller asks for that rotation
//! through this service, with `RotateMasterUserPassword`, and Secrets Manager — which holds
//! the secret but not the database — can only ASK this service for it. Such a secret uses
//! MANAGED ROTATION: there is no rotation function for Secrets Manager to invoke ("For
//! secrets that use managed rotation, omit this field",
//! `RotateSecretRequest$RotationLambdaARN`).
//!
//! So this is the record this service writes for each managed secret it mints
//! ([`::service_common::managed_secret_rotations`], the one register in the shared engine both
//! sides read): the request that rotates that resource's master user password, made against
//! this service's own front door. A rotation Secrets Manager hands over is therefore the very
//! `ModifyDB*` call a caller would make — the same validation, the same new password staked
//! on the resource, and the same write of the secret as this service's own service principal
//! — rather than a second account of what a rotation does.
//!
//! The record is written when the secret is minted and forgotten when it is deleted
//! ([`super::managed_master_secret`]), so what Secrets Manager can hand over is exactly the
//! set of secrets this service is managing right now.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

/// `ModifyDBInstanceMessage$RotateMasterUserPassword` and its DB-cluster twin: "Specifies
/// whether to rotate the secret managed by Amazon Web Services Secrets Manager for the master
/// user password."
const ROTATE_MASTER_USER_PASSWORD: &str = "RotateMasterUserPassword";

/// `ApplyImmediately`, which the rotation above is documented to require: "Constraints: You
/// must apply the change immediately when rotating the master user password."
const APPLY_IMMEDIATELY: &str = "ApplyImmediately";

/// The media type an `awsQuery` front door takes its form body in.
const FORM_MEDIA_TYPE: &str = "application/x-www-form-urlencoded";

/// How many bytes of this service's own answer a failed rotation reports back.
///
/// The answer is this emulator's, not a caller's, and what it is read for is the sentence to
/// report a rotation that did not happen with — so a bounded prefix of it is enough and a
/// large body cannot be carried into another service's response.
const MAX_ANSWER_BYTES: usize = 1_024;

/// How often Amazon RDS rotates the secret it manages for a master user password, in days.
///
/// RDS sets the rotation schedule of that secret itself when it creates it — "RDS rotates the
/// secret automatically every seven days" (Password management with Amazon RDS and Amazon Web
/// Services Secrets Manager, Amazon RDS User Guide) — so this is the schedule Secrets Manager
/// reports for such a secret, and the interval its next rotation is measured by.
pub const MANAGED_ROTATION_INTERVAL_DAYS: i64 = 7;
/// The rotation of ONE resource's master user password, as the request that performs it.
///
/// Held in the shared register under the ARN of the secret it rotates, and asked for by
/// Secrets Manager when a caller rotates that secret there.
pub struct MasterUserSecretRotation {
    /// The Region the resource lives in, which is the Region this service's front door is
    /// addressed in.
    region: String,
    /// The `Action` that rotates this kind of resource's master user password.
    action: &'static str,
    /// The member that resource's identifier travels in.
    identifier_member: &'static str,
    /// The resource's own identifier.
    identifier: String,
}

/// Prints what the record ADDRESSES — never a credential, which this record holds none of.
impl std::fmt::Debug for MasterUserSecretRotation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MasterUserSecretRotation")
            .field("action", &self.action)
            .field("identifier", &self.identifier)
            .finish_non_exhaustive()
    }
}

impl ::service_common::managed_secret_rotations::ManagedRotation for MasterUserSecretRotation {
    /// The id this service's secrets report as their `OwningService`: the first label of this
    /// service's own principal, which is also the prefix such a secret is named under.
    fn owning_service(&self) -> &str {
        service_id()
    }

    /// The schedule RDS sets on the secret it manages
    /// ([`MANAGED_ROTATION_INTERVAL_DAYS`]).
    fn rotation_interval_days(&self) -> Option<i64> {
        Some(MANAGED_ROTATION_INTERVAL_DAYS)
    }

    fn rotate<'a>(
        &'a self,
        callback: &'a ::smithy_server_runtime::EmbeddedEmulator,
    ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>> {
        Box::pin(async move { self.ask_for_the_rotation(callback).await })
    }
}

impl MasterUserSecretRotation {
    /// Make the `ModifyDB*` request that rotates this resource's master user password, over
    /// the in-process callback into this same emulator, and report what it answered.
    ///
    /// The request is signed with the credential the framework hands this process, through the
    /// tree's one in-process signer, because the emulator verifies every request signature.
    /// Nothing leaves the process and nothing of the credential is reported.
    async fn ask_for_the_rotation(
        &self,
        callback: &::smithy_server_runtime::EmbeddedEmulator,
    ) -> Result<(), String> {
        let region = ::service_common::arn::region_label(&self.region)
            .ok_or_else(|| unaskable_region_message(self.action))?
            .to_string();
        let body = ::form_urlencoded::Serializer::new(String::new())
            .append_pair("Action", self.action)
            .append_pair("Version", crate::service::api_version::API_VERSION)
            .append_pair(self.identifier_member, &self.identifier)
            .append_pair(ROTATE_MASTER_USER_PASSWORD, "true")
            .append_pair(APPLY_IMMEDIATELY, "true")
            .finish()
            .into_bytes();
        let mut request = ::http::Request::builder()
            .method("POST")
            .uri("/")
            .header("host", format!("rds.{region}.amazonaws.com"))
            .header("content-type", FORM_MEDIA_TYPE)
            .body(body)
            .map_err(|error| unmakable_request_message(self.action, &error.to_string()))?;
        ::service_common::in_process_sigv4::sign_callback_request(
            &mut request,
            super::published_values::ARN_NAMESPACE,
            &region,
            callback,
        )
        .await
        .map_err(|error| unmakable_request_message(self.action, &error))?;
        let (parts, body) = request.into_parts();
        let request = ::aws_smithy_runtime_api::http::Request::try_from(
            ::http::Request::from_parts(
                parts,
                ::aws_smithy_types::body::SdkBody::from(::bytes::Bytes::from(body)),
            ),
        )
        .map_err(|error| unmakable_request_message(self.action, &error.to_string()))?;
        let answer = {
            use ::aws_smithy_runtime_api::client::http::HttpConnector;
            let connector = callback.http_connector();
            HttpConnector::call(&connector, request)
                .await
                .map_err(|error| unmakable_request_message(self.action, &error.to_string()))?
        };
        let status = answer.status().as_u16();
        if (200..300).contains(&status) {
            return Ok(());
        }
        let said: String = answer
            .body()
            .bytes()
            .map(|bytes| {
                String::from_utf8_lossy(&bytes[..bytes.len().min(MAX_ANSWER_BYTES)]).to_string()
            })
            .unwrap_or_default();
        Err(refused_rotation_message(self.action, status, &said))
    }
}

/// The record that rotates the master user password of the resource `owner` names, or `None`
/// for a secret whose rotation is not one a request to this service performs.
///
/// A DB INSTANCE's and a DB CLUSTER's master user password are each rotated by that
/// resource's own `ModifyDB*` request. A TENANT DATABASE's is not one of those two — its
/// master user is a user of the container instance, and the request that rotates it names the
/// tenant database as well — so no record is written for it and Secrets Manager keeps
/// answering a caller's rotation of that secret as a write of a secret it does not own.
#[must_use]
pub fn master_user_secret_rotation(
    region: &str,
    owner: &super::ManagedSecretOwner<'_>,
) -> Option<Arc<dyn ::service_common::managed_secret_rotations::ManagedRotation>> {
    let (action, identifier_member, resource_type) = match owner.resource {
        super::ManagedSecretResource::DbInstance => {
            ("ModifyDBInstance", "DBInstanceIdentifier", "db")
        }
        super::ManagedSecretResource::DbCluster => {
            ("ModifyDBCluster", "DBClusterIdentifier", "cluster")
        }
        super::ManagedSecretResource::TenantDatabase => return None,
    };
    // The resource's identifier is read back out of its own ARN — the one value this record is
    // built from — so the record addresses the resource the secret was minted for and nothing
    // else.
    let identifier = ::service_common::arn::resource_id_of_type(owner.arn, resource_type)?;
    Some(Arc::new(MasterUserSecretRotation {
        region: region.to_string(),
        action,
        identifier_member,
        identifier: identifier.to_string(),
    }))
}

/// The id this service is known by where a service is named by its id: the first label of its
/// own service principal, which is what a secret it owns reports as `OwningService`.
#[must_use]
pub fn service_id() -> &'static str {
    super::associated_roles::RDS_SERVICE_PRINCIPAL
        .split('.')
        .next()
        .unwrap_or(super::published_values::ARN_NAMESPACE)
}

/// The sentence reported when the Region the resource lives in names no Region at all, so no
/// front door of this service can be addressed for it.
fn unaskable_region_message(action: &str) -> String {
    format!(
        "the master user secret could not be rotated by Amazon RDS: {action} could not be \
         addressed, because the request's signing scope names no Amazon Web Services Region"
    )
}

/// The sentence reported when this emulator could not MAKE the request that rotates the
/// password — a failure of the emulator rather than anything a caller asked for.
fn unmakable_request_message(action: &str, error: &str) -> String {
    format!(
        "the master user secret could not be rotated by Amazon RDS: this emulator could not \
         make the {action} request that rotates it: {error}"
    )
}

/// The sentence reported when this service REFUSED the rotation — the resource is in a state
/// that does not allow it, say — carrying the status and what it answered, which is the only
/// account of why the credential was not re-staked.
fn refused_rotation_message(action: &str, status: u16, said: &str) -> String {
    format!(
        "the master user secret was not rotated by Amazon RDS: {action} answered {status}: \
         {said}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the record addresses the resource whose ARN the secret's owner names, by that
    /// resource's own modify request; a resource whose password no such request rotates gets
    /// no record at all.
    ///
    /// The rows span what the reading generalises over: the kind of resource (DB instance, DB
    /// cluster, tenant database), and an owner ARN that names no resource of the kind it
    /// claims.
    #[test]
    fn a_record_addresses_the_resource_its_owner_names() {
        let instance = "arn:aws:rds:us-east-1:000000000000:db:emu-one";
        let cluster = "arn:aws:rds:us-east-1:000000000000:cluster:emu-two";
        for (case, owner, expected) in [
            (
                "a DB instance",
                super::super::ManagedSecretOwner::db_instance(instance),
                Some(("ModifyDBInstance", "DBInstanceIdentifier", "emu-one")),
            ),
            (
                "a DB cluster",
                super::super::ManagedSecretOwner::db_cluster(cluster),
                Some(("ModifyDBCluster", "DBClusterIdentifier", "emu-two")),
            ),
            (
                "a tenant database of an instance",
                super::super::ManagedSecretOwner::tenant_database(instance),
                None,
            ),
            (
                "an owner ARN naming no resource of its kind",
                super::super::ManagedSecretOwner::db_cluster(instance),
                None,
            ),
        ] {
            let record = master_user_secret_rotation("us-east-1", &owner);
            match expected {
                None => assert!(
                    record.is_none(),
                    "case {case:?}: expected no record, got {:?}",
                    record.is_some()
                ),
                Some((action, member, identifier)) => {
                    let record = record.unwrap_or_else(|| {
                        panic!("case {case:?}: expected the record for {action}")
                    });
                    let shown = format!("{record:?}");
                    assert!(
                        shown.contains(action) && shown.contains(identifier),
                        "case {case:?}: expected a record addressing {action} on \
                         {identifier:?}, got {shown}"
                    );
                    assert_eq!(
                        record.owning_service(),
                        "rds",
                        "case {case:?}: expected the record to be this service's own"
                    );
                    // The member the identifier travels in is the one that request models.
                    assert!(
                        member.starts_with("DB"),
                        "case {case:?}: expected a modelled identifier member, got {member:?}"
                    );
                }
            }
        }
    }
}
