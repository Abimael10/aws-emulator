// SPDX-License-Identifier: CC0-1.0

//! Amazon MSK's IAM ACCESS CONTROL, as the data plane enforces it.
//!
//! A client that authenticates with IAM is not thereby allowed to do anything: what it
//! may do is what the IAM policies of the identity behind its credential allow, stated as
//! `kafka-cluster:` actions over the cluster, its topics and its consumer groups. So a
//! connection on the SASL/IAM path carries the permissions of the principal that signed
//! its exchange, and every request on it is decided against them — a produce needs
//! `WriteData` on the topic, a fetch `ReadData`, joining a group `AlterGroup` on the
//! group, and a refusal is the protocol's own `TOPIC_AUTHORIZATION_FAILED` /
//! `GROUP_AUTHORIZATION_FAILED`.
//!
//! Three pieces, each held in exactly one place, as the control plane's own gate holds
//! them ([`crate::state::identity_authorization`]):
//!
//! 1. **who** — the identity the exchange's access key id speaks for: the session that key
//!    was minted for, or the IAM user that owns it;
//! 2. **what it may do** — the identity-based policy documents the IAM store holds for
//!    that identity, read through IAM's OWN accessor, so a document
//!    `iam:PutUserPolicy` wrote takes effect on the next connection;
//! 3. **the decision** — [`::service_common::iam_authorization`], the ONE shared
//!    evaluation of those documents against `(action, resource)`.
//!
//! The credentials nothing readable bounds are not refused for a missing grant — the
//! account's own published credential, and a principal this emulator holds no policy
//! record of — because nothing that can be read says what they may do; the same rule the
//! control-plane gate follows. An explicit `Deny` in a document that CAN be read still
//! refuses.

use std::time::SystemTime;

use ::service_com_amazonaws_iam_awsidentitymanagementv20100508::state as iam_store;
use ::service_com_amazonaws_sts_awssecuritytokenservicev20110615::state as sts_store;
use ::service_common::iam_authorization::{self as iam, Decision};

/// The action a client needs to produce to a topic, and the one it needs to consume from
/// it (Amazon MSK IAM access control actions).
pub const ACTION_WRITE_DATA: &str = "kafka-cluster:WriteData";
/// See [`ACTION_WRITE_DATA`].
pub const ACTION_READ_DATA: &str = "kafka-cluster:ReadData";
/// The action creating a topic over the wire needs, and the one deleting it needs.
pub const ACTION_CREATE_TOPIC: &str = "kafka-cluster:CreateTopic";
/// See [`ACTION_CREATE_TOPIC`].
pub const ACTION_DELETE_TOPIC: &str = "kafka-cluster:DeleteTopic";
/// The action joining or committing in a consumer group needs, and the one reading the
/// group's committed offsets needs.
pub const ACTION_ALTER_GROUP: &str = "kafka-cluster:AlterGroup";
/// See [`ACTION_ALTER_GROUP`].
pub const ACTION_DESCRIBE_GROUP: &str = "kafka-cluster:DescribeGroup";
/// The action deleting a consumer group needs.
pub const ACTION_DELETE_GROUP: &str = "kafka-cluster:DeleteGroup";

/// The ARN resource type a topic of a cluster is addressed by in such a policy, and the
/// one a consumer group is: `topic/<cluster name>/<cluster uuid>/<topic>` and
/// `group/<cluster name>/<cluster uuid>/<group>` (Amazon MSK IAM access control
/// resources).
const TOPIC_RESOURCE_TYPE: &str = "topic";
/// See [`TOPIC_RESOURCE_TYPE`].
const GROUP_RESOURCE_TYPE: &str = "group";

/// The resource type a cluster ARN itself carries, which the two above replace.
const CLUSTER_RESOURCE_PREFIX: &str = "cluster/";

/// The ARN a policy statement addresses the topic `topic` of the cluster `cluster_arn`
/// by.
#[must_use]
pub fn topic_resource(cluster_arn: &str, topic: &str) -> String {
    scoped_resource(cluster_arn, TOPIC_RESOURCE_TYPE, topic)
}

/// The ARN a policy statement addresses the consumer group `group` of the cluster
/// `cluster_arn` by.
#[must_use]
pub fn group_resource(cluster_arn: &str, group: &str) -> String {
    scoped_resource(cluster_arn, GROUP_RESOURCE_TYPE, group)
}

/// One resource OF a cluster, as its ARN is written: the cluster's own ARN with its
/// `cluster/` resource type replaced by this resource's and the resource's name appended
/// — so the cluster name and uuid a statement is scoped by are the cluster's own.
fn scoped_resource(cluster_arn: &str, resource_type: &str, name: &str) -> String {
    let Some((prefix, resource)) = cluster_arn.rsplit_once(':') else {
        return cluster_arn.to_string();
    };
    let Some(identity) = resource.strip_prefix(CLUSTER_RESOURCE_PREFIX) else {
        return cluster_arn.to_string();
    };
    format!("{prefix}:{resource_type}/{identity}/{name}")
}

/// The stores a connection's permissions are read out of: IAM's, which holds the
/// documents, and STS's, which says which identity a session credential speaks for.
///
/// Held as the state handles themselves, so a document written after the endpoint was
/// opened is in force on the next connection — the endpoint outlives every request that
/// changes a policy.
#[derive(Clone, Default)]
pub struct PolicySource {
    /// IAM's store, or `None` on an engine that carries none.
    iam: Option<::engine::ServiceState<iam_store::ComAmazonawsIamAwsidentitymanagementv20100508State>>,
    /// STS's store, or `None` on an engine that carries none.
    sts: Option<::engine::ServiceState<sts_store::ComAmazonawsStsAwssecuritytokenservicev20110615State>>,
}

/// Written by hand rather than derived: the state handles behind this carry no `Debug`,
/// and what is diagnosable about the source is whether each store is there.
impl std::fmt::Debug for PolicySource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PolicySource")
            .field("iam", &self.iam.is_some())
            .field("sts", &self.sts.is_some())
            .finish()
    }
}

impl PolicySource {
    /// The two stores as the engine this service is mounted on holds them.
    #[must_use]
    pub fn of_engine<Eng: Send + Sync + 'static>(engine: &Eng) -> Self {
        Self {
            iam: iam_store::service_state(engine),
            sts: sts_store::service_state(engine),
        }
    }

    /// The permissions in force on a connection whose IAM exchange was signed with
    /// `access_key_id`.
    #[must_use]
    pub fn permissions(&self, access_key_id: &str, now: SystemTime) -> Permissions {
        let Some(iam) = self.iam.as_ref() else {
            return Permissions::unbounded();
        };
        let Some(identity_arn) = self.identity_arn(access_key_id, now) else {
            return Permissions::unbounded();
        };
        let store = iam.lock();
        // A principal the IAM store holds no record of, and one carrying an attachment it
        // resolves no document for (an entry of AWS's own managed-policy catalog), are
        // principals whose permissions cannot be read in full.
        let Some(policies) = store.principal_policies(&identity_arn) else {
            return Permissions::unbounded();
        };
        if !policies.uncarried.is_empty() {
            return Permissions::unbounded();
        }
        Permissions {
            principal: identity_arn,
            documents: policies.documents,
            bounded: true,
        }
    }

    /// The ARN of the identity whose identity-based policies are in force on a credential:
    /// for a session, the role it was minted for (or the user behind it); for a long-term
    /// key, the IAM user that owns it. `None` for a credential neither store accounts for
    /// — the account's own published credential.
    fn identity_arn(&self, access_key_id: &str, now: SystemTime) -> Option<String> {
        if let Some(sts) = self.sts.as_ref() {
            let store = sts.lock();
            if let Some(session) = store.session(access_key_id) {
                // An expired credential speaks for nobody; the layer that authenticates a
                // request is what rejects it.
                if session.expires_at <= now {
                    return None;
                }
                return session
                    .role_arn
                    .clone()
                    .or_else(|| session.federating_user_arn.clone())
                    .or_else(|| Some(session.identity.arn.clone()));
            }
        }
        let iam = self.iam.as_ref()?;
        let store = iam.lock();
        // IAM's store maps an access key id to the USER that owns it, and a connection
        // signed with one speaks for that user and is judged on the user's policies.
        store
            .access_keys
            .get(access_key_id)
            .and_then(|record| store.users.get(&record.user_key))
            .map(|user| user.arn.clone())
    }
}

/// What one connection's principal may do on this cluster's data plane.
#[derive(Debug, Clone, Default)]
pub struct Permissions {
    /// The identity the connection speaks for, empty for one nothing readable bounds.
    principal: String,
    /// The identity-based policy documents in force on it.
    documents: Vec<String>,
    /// Set when the documents collected ARE this principal's permission set, so the
    /// absence of a grant is IAM's default deny rather than something unread.
    bounded: bool,
}

impl Permissions {
    /// The permissions of a credential nothing readable bounds: every action no readable
    /// document denies (see the module docs).
    #[must_use]
    pub fn unbounded() -> Self {
        Self::default()
    }

    /// The identity these permissions belong to.
    #[must_use]
    pub fn principal(&self) -> &str {
        &self.principal
    }

    /// Whether the connection may perform `action` on `resource`: an `Allow` about the
    /// request, with no explicit `Deny` over it — and anything, for a credential nothing
    /// readable bounds.
    #[must_use]
    pub fn allows(&self, action: &str, resource: &str) -> bool {
        if !self.bounded {
            return true;
        }
        if self.documents.is_empty() {
            return false;
        }
        iam::identity_decision(
            &self.documents,
            // An identity-based document states no `Principal` element, so the decision
            // rests on the action and the resource the request addressed.
            &iam::Request::new(action, resource, &[]),
        ) == Decision::Allowed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: a topic and a group of a cluster are addressed by the cluster's own name and
    /// uuid under their own resource type, which is what an IAM policy scoped to one topic
    /// or one group of one cluster is written over.
    #[test]
    fn a_clusters_resources_are_addressed_under_the_clusters_identity() {
        let cluster = ::engine::emulator_arn(
            "kafka",
            "us-west-2",
            "cluster/orders/11111111-2222-3333-4444-555555555555-1",
        );
        let rows = [
            (
                "a topic",
                topic_resource(&cluster, "events"),
                "topic/orders/11111111-2222-3333-4444-555555555555-1/events",
            ),
            (
                "a group",
                group_resource(&cluster, "readers"),
                "group/orders/11111111-2222-3333-4444-555555555555-1/readers",
            ),
        ];
        for (case, reached, tail) in rows {
            let expected = ::engine::emulator_arn("kafka", "us-west-2", tail);
            assert_eq!(
                reached, expected,
                "case {case:?}: expected {expected}, got {reached}"
            );
        }
    }

    /// RULE: a connection nothing readable bounds may do anything, one whose readable
    /// documents allow the action may do it, and one whose documents do not allow it may
    /// not — IAM's default deny.
    #[test]
    fn a_grant_is_needed_only_where_the_permission_set_is_readable() {
        let cluster = ::engine::emulator_arn(
            "kafka",
            "us-west-2",
            "cluster/orders/11111111-2222-3333-4444-555555555555-1",
        );
        let topic = topic_resource(&cluster, "events");
        let allowing = format!(
            r#"{{"Statement":[{{"Effect":"Allow","Action":"kafka-cluster:ReadData","Resource":"{topic}"}}]}}"#
        );
        let rows: Vec<(&str, Permissions, &str, bool)> = vec![
            ("nothing readable bounds it", Permissions::unbounded(), ACTION_WRITE_DATA, true),
            (
                "a readable document allows the read",
                Permissions {
                    principal: "arn:aws:iam::123456789012:user/reader".to_string(),
                    documents: vec![allowing.clone()],
                    bounded: true,
                },
                ACTION_READ_DATA,
                true,
            ),
            (
                "no readable document allows the write",
                Permissions {
                    principal: "arn:aws:iam::123456789012:user/reader".to_string(),
                    documents: vec![allowing],
                    bounded: true,
                },
                ACTION_WRITE_DATA,
                false,
            ),
            (
                "a readable permission set that is empty",
                Permissions {
                    principal: "arn:aws:iam::123456789012:user/nobody".to_string(),
                    documents: Vec::new(),
                    bounded: true,
                },
                ACTION_READ_DATA,
                false,
            ),
        ];
        for (case, permissions, action, expected) in rows {
            let reached = permissions.allows(action, &topic);
            assert_eq!(
                reached, expected,
                "case {case:?}: expected {action} on {topic} to be {expected}, got {reached}"
            );
        }
    }
}
