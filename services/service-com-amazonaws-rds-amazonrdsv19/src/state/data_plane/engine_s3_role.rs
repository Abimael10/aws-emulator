// SPDX-License-Identifier: CC0-1.0

//! WHICH IAM role an engine's own Amazon S3 statement acts under, and whether that role's
//! permissions let it touch the object.
//!
//! An S3 integration of a database engine is two grants, and this module owns both:
//!
//! 1. **the association** — Amazon RDS acts under an IAM role the RESOURCE has associated
//!    with it for the feature the statement needs (`s3Import` for a read, `s3Export` for a
//!    write), so a resource with no such association has no credentials for the engine to
//!    call Amazon S3 with at all;
//! 2. **what that role may do** — the identity-based policies IAM holds for it, evaluated
//!    by [`::service_common::iam_authorization`], the tree's one reading of a policy
//!    document, so an engine statement that reaches S3 under a role whose policies refuse
//!    the object operation fails exactly as the real call would.
//!
//! Both wires ask here, because both engines perform the same two-grant call: the
//! PostgreSQL `aws_s3` functions ([`super::pg_aws_s3`]) and the MySQL `LOAD DATA FROM S3`
//! ([`super::mysql_s3_load`]) differ in their grammar and in the words of their refusals,
//! not in who they act as.
//!
//! Nothing is refused on evidence this emulator does not have. Each of these shows nothing
//! about the role and lets the statement go ahead: an engine carrying no IAM store, a role
//! ARN that store holds no role for (it may belong to another account), a role carrying an
//! attachment whose document this account does not hold, and a role carrying NO
//! identity-based policy at all — a role provisioned out of band routinely carries none, so
//! a policy set that merely says nothing about the call is no evidence about the call. What
//! the documents DO state is read in IAM's own order: an explicit `Deny` refuses, a grant
//! allows unless the permissions boundary attached to the role withholds it, and a role
//! that STATES permissions none of which covers the call withholds it.

use ::service_common::iam_authorization::{self as iam, Decision};

/// The IAM store an engine statement's role is resolved against: IAM's OWN state struct,
/// reached through the SAME shared engine this service's store lives in, so the policies
/// read here are the ones IAM's handlers wrote.
pub type IamState =
    ::service_com_amazonaws_iam_awsidentitymanagementv20100508::ComAmazonawsIamAwsidentitymanagementv20100508State;

/// The IAM store on `engine`, or `None` when no IAM service is mounted on it — a deployment
/// with no IAM to ask, which is evidence of nothing.
#[must_use]
pub fn iam_store<Eng: Send + Sync + 'static>(
    engine: &Eng,
) -> Option<::engine::ServiceState<IamState>> {
    ::service_com_amazonaws_iam_awsidentitymanagementv20100508::state::service_state(engine)
}

/// WHICH S3 integration of the engine a statement performs, which decides both the feature
/// the resource must have associated a role for and the object action that role must be
/// allowed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum S3Capability {
    /// Reading an object into the database: the `s3Import` feature.
    Import,
    /// Writing the result of a query out as an object: the `s3Export` feature.
    Export,
}

impl S3Capability {
    /// The `FeatureName` an association for this capability names — one of the
    /// `SupportedFeatureNames` the PostgreSQL engine families publish
    /// (`crate::ops::describe_db_engine_versions`), which is what a client associates a role
    /// under.
    #[must_use]
    pub fn feature_name(self) -> &'static str {
        match self {
            Self::Import => "s3Import",
            Self::Export => "s3Export",
        }
    }

    /// The feature as the engine's own extension names it in its messages: the feature name
    /// folded to the lower case the extension's internal token carries.
    #[must_use]
    pub fn engine_token(self) -> String {
        self.feature_name().to_ascii_lowercase()
    }

    /// The Amazon S3 object action the statement performs under the role: the read an import
    /// makes of the object, and the write an export makes of it.
    ///
    /// Source: the policies "Setting up access to an Amazon S3 bucket" (Amazon RDS User
    /// Guide) asks for — `s3:GetObject` for the import and `s3:PutObject` for the export,
    /// granted on the objects of the bucket.
    #[must_use]
    pub fn object_action(self) -> &'static str {
        match self {
            Self::Import => "s3:GetObject",
            Self::Export => "s3:PutObject",
        }
    }
}

/// Whether IAM ANSWERS that the permissions in force on `role_arn` do not allow `action` on
/// the object `resource` names.
///
/// `false` for every case that is not such an answer — the module docs list them — because a
/// statement is refused only on what IAM said about the role.
#[must_use]
pub fn role_withholds_object_action(
    iam_state: Option<&::engine::ServiceState<IamState>>,
    role_arn: &str,
    action: &str,
    resource: &str,
) -> bool {
    let Some(iam_state) = iam_state else {
        return false;
    };
    let store = iam_state.lock();
    // A role this account does not hold shows nothing: it may belong to another account.
    if !store.roles.values().any(|role| role.arn == role_arn) {
        return false;
    }
    let Some(policies) = store.principal_policies(role_arn) else {
        return false;
    };
    // An attachment whose document this account does not hold is a grant nothing can read,
    // so the set says nothing about the call.
    if !policies.uncarried.is_empty() {
        return false;
    }
    let request = iam::Request::new(action, resource, &[]);
    match iam::identity_decision(&policies.documents, &request) {
        // An explicit deny is an answer about this role whatever else it carries.
        Decision::Denied => true,
        // A grant stands unless the ceiling attached to the role withholds the call, which
        // caps what the role's own policies grant.
        Decision::Allowed => store
            .permissions_boundary_document(role_arn)
            .is_some_and(|boundary| {
                iam::identity_decision(&[boundary], &request) != Decision::Allowed
            }),
        // Policies that state nothing about the call are an answer only where the role
        // states permissions at all: a role carrying none is one nothing is known about.
        Decision::NotStated => !policies.documents.is_empty(),
    }
}

/// The ARN of the object `bucket`/`key` names, which is the resource the object action is
/// authorized on — Amazon S3's own form for it.
#[must_use]
pub fn object_arn(bucket: &str, key: &str) -> String {
    ::service_com_amazonaws_s3_amazons3::state::object_arn(bucket, key)
}
