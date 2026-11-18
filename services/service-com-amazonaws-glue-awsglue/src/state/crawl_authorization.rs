// SPDX-License-Identifier: CC0-1.0

//! Whether the account's own records permit a CRAWL to read the data behind a target.
//!
//! # The rule this module implements
//!
//! A crawler does not read its data stores as the caller that started it: the crawl is
//! performed BY Glue acting as the crawler's `Role` — "the IAM role or Amazon Resource Name
//! (ARN) of an IAM role used by the new crawler to access customer resources"
//! (`CreateCrawler$Role`) — so every object-store call the crawl makes is a call by that
//! role, and the documents in force decide it. Two of them do, in IAM's own order: the
//! IDENTITY-based policies on the role (its inline policies and the documents of the managed
//! policies attached to it, collected by [`super::role_policy_documents`]) and the target
//! BUCKET's own resource policy, where an explicit `Deny` from either side refuses and, within
//! one account, an `Allow` from either is enough
//! ([`::service_common::iam_authorization::is_allowed`]).
//!
//! So a crawl asks, for each Amazon S3 target, exactly what it is about to do:
//!
//! * [`CRAWL_LIST_ACTION`] on the target's BUCKET before it lists the objects under the
//!   target's prefix — the listing carries that prefix, so a grant narrowed to a prefix
//!   ([`LIST_PREFIX_KEY`]) is decided as narrowly as it is written;
//! * [`CRAWL_READ_ACTION`] on the OBJECT whose bytes a dataset's schema is read from, which
//!   is authorized against that object and not against the prefix it sits under.
//!
//! A call the records WITHHOLD is one the crawl does not make, so the data behind that target
//! is not read, nothing is classified from it and NO table and no partition is written for it:
//! a crawler whose role may not read a bucket leaves the catalog exactly as it was. The crawl
//! itself is CARRIED OUT — it reached every target it was permitted to read, and a target it
//! was not holds no dataset it could describe — so what it could not read is logged against
//! the crawl rather than reported as a crawl that failed.
//!
//! # What a refusal has to be grounded in
//!
//! Nothing is withheld unless a document the account HOLDS says so: a crawler that names no
//! role, an emulator with no IAM mounted to ask, a role this account holds nothing under, and a
//! role carrying an attachment whose document cannot be read all leave the crawl to go ahead —
//! a role is routinely provisioned out of band, so what cannot be read about it says nothing
//! about its permissions. That is the reading [`super::role_policy_documents`] already answers
//! `None` for. So does a crawl no document is in force over at all: a role the account holds
//! no policy for, reading a bucket that carries no policy of its own, is decided by nothing,
//! and a crawl of it reads its target exactly as it does with no IAM mounted. A decision is
//! made as soon as there IS something to decide it from — one identity document in force on the
//! role, or a policy on the bucket it reads. Where the ROLE's half is the one that cannot be
//! read, the bucket's own policy is still a document the account holds, in force whatever the
//! role carries: an explicit `Deny` in it withholds the call on its own, while a grant it does
//! not make decides nothing, because the grant the role may carry is what could not be read.
//!
//! The role's documents are read ONCE per crawl, under the IAM guard, and PREPARED once; each
//! bucket's policy is read through the object store's own `GetBucketPolicy` and remembered for
//! the rest of the crawl ([`MAX_CRAWLED_BUCKET_POLICIES`]). So a crawl over many targets in one
//! bucket pays for one lookup of each, and the number of decisions is the number of calls the
//! crawl was going to make anyway.

use ::std::collections::HashMap;

use ::aws_smithy_types::error::metadata::ProvideErrorMetadata;
use ::service_common::iam_authorization as authorization;

use super::*;

/// The Amazon S3 action a crawl's LISTING of a target makes: "you must have the
/// `s3:ListBucket` permission" to list the objects of a bucket, and the ARN that action is
/// taken on is the BUCKET's.
pub const CRAWL_LIST_ACTION: &str = "s3:ListBucket";

/// The Amazon S3 action a crawl's READ of one object makes: "you must have the `s3:GetObject`
/// permission" to fetch an object, taken on that OBJECT's own ARN.
pub const CRAWL_READ_ACTION: &str = "s3:GetObject";

/// IAM's global condition key naming the identity whose policies are in force. For a call a
/// service makes under an assumed role it is that role's ARN, which is what a statement
/// narrowing a grant to one principal compares against.
const PRINCIPAL_ARN_KEY: &str = "aws:PrincipalArn";

/// The condition key a bucket listing carries the prefix it lists under as, so a grant of
/// [`CRAWL_LIST_ACTION`] written only for one prefix is decided on the prefix the crawl
/// really asks for.
const LIST_PREFIX_KEY: &str = "s3:prefix";

/// The Amazon S3 error code in which the object store answers that a bucket carries no policy
/// of its own, which is a bucket whose resource-based policy states nothing.
const NO_SUCH_BUCKET_POLICY_CODE: &str = "NoSuchBucketPolicy";

/// How many DISTINCT buckets' policies one crawl REMEMBERS.
///
/// CHOSEN, not an AWS quota: a crawler names as many targets as its create cared to give it,
/// so what one crawl holds in memory about the buckets it reached is bounded here. It bounds
/// the memory and not the calls: a bucket past the ceiling is asked about again rather than
/// remembered, which is the same answer read a second time. A crawl stays well inside it in
/// practice — a target's bucket is asked about once for the listing and once for the object
/// read, and the targets one crawler carries are themselves resolved under a ceiling
/// ([`MAX_RESOLVED_S3_TARGETS`]).
pub const MAX_CRAWLED_BUCKET_POLICIES: usize = 32;

/// What the account's records say the crawl may read: the role it runs as, the documents in
/// force on that role, and the policy each bucket it has reached carries.
///
/// Resolved once per crawl ([`Self::of_role`]) and consulted before each call the crawl makes
/// ([`Self::withholds_listing`], [`Self::withholds_object_read`]).
#[derive(Debug, Default)]
pub struct CrawlReadAuthorization {
    /// The ARN of the role the crawl runs as, which is the principal a bucket policy names it
    /// by and the value of [`PRINCIPAL_ARN_KEY`]. `None` for a crawler that names no role,
    /// about which nothing is shown.
    role_arn: Option<String>,
    /// Every spelling of that role a resource-based policy may name: its own ARN beside the
    /// account spellings a deny of the account refuses each of its principals under
    /// ([`authorization::account_principals`]).
    principals: Vec<String>,
    /// The identity-based documents in force on the role, read once. `None` when nothing can
    /// be SHOWN about the role's permissions, which withholds nothing.
    identity: Option<Vec<authorization::PreparedDocument>>,
    /// The policy each bucket this crawl has asked about carries: `Some(document)` for a
    /// bucket whose policy the store served, `None` for one it answered holds none and for one
    /// it answered nothing readable about. Bounded by [`MAX_CRAWLED_BUCKET_POLICIES`].
    policies: HashMap<String, Option<String>>,
}

impl CrawlReadAuthorization {
    /// What this engine's own IAM shows about the crawl role `role`.
    ///
    /// IAM is the SAME store in the SAME shared engine the Glue store lives in, so the
    /// documents read here are the ones `CreateRole`, `PutRolePolicy` and `AttachRolePolicy`
    /// wrote. The guard is taken for the read alone and the documents are CLONED out of it, so
    /// nothing is parsed or decided under it.
    #[must_use]
    pub fn of_role<Eng: Send + Sync + 'static>(engine: &Eng, role: &str) -> Self {
        let role = role.trim();
        if role.is_empty() {
            return Self::default();
        }
        let arn = glue_role_reference(role).arn;
        let documents = iam_state(engine).and_then(|iam| {
            let records = iam.lock();
            role_policy_documents(role, &records)
        });
        let mut principals = vec![arn.clone()];
        principals.extend(authorization::account_principals(&arn));
        Self {
            role_arn: Some(arn),
            principals,
            identity: documents.map(|documents| authorization::prepare_documents(&documents)),
            policies: HashMap::new(),
        }
    }

    /// Why the records withhold the LISTING of the objects under `prefix` in `bucket` from the
    /// crawl role, or `None` when the crawl may list them.
    ///
    /// The answer is the account of the refusal, for the crawl's own LOG: what a crawl was not
    /// permitted to read is not reported to the caller — the target simply holds no dataset the
    /// crawl could describe.
    pub async fn withholds_listing(
        &mut self,
        client: &::aws_sdk_s3::Client,
        bucket: &str,
        prefix: &str,
    ) -> Option<String> {
        let resource = bucket_arn(bucket);
        let context = [(LIST_PREFIX_KEY.to_string(), prefix.to_string())];
        self.withholds(client, bucket, CRAWL_LIST_ACTION, &resource, &context)
            .await
    }

    /// Why the records withhold the READ of the object `key` in `bucket` from the crawl role,
    /// or `None` when the crawl may read it. See [`Self::withholds_listing`].
    pub async fn withholds_object_read(
        &mut self,
        client: &::aws_sdk_s3::Client,
        bucket: &str,
        key: &str,
    ) -> Option<String> {
        let resource = object_arn(bucket, key);
        self.withholds(client, bucket, CRAWL_READ_ACTION, &resource, &[])
            .await
    }

    /// Why the documents in force withhold `action` on `resource` from the crawl role, or
    /// `None` when they permit it — which includes every case where nothing is in force over
    /// the call at all.
    ///
    /// Decided in IAM's own order: the bucket's own policy and the role's identity policies are
    /// read together, an explicit `Deny` from either refuses, and an `Allow` from either is
    /// enough. A call no document covers is decided by nothing and so withholds nothing. Where
    /// the role's own documents are the half that cannot be read, the bucket's policy is still
    /// a document the account holds and in force whatever the role carries, so an explicit
    /// `Deny` in it withholds the call on its own — while an absent grant there decides
    /// nothing, because the grant the role may carry cannot be read.
    async fn withholds(
        &mut self,
        client: &::aws_sdk_s3::Client,
        bucket: &str,
        action: &str,
        resource: &str,
        extra_context: &[(String, String)],
    ) -> Option<String> {
        let role_arn = self.role_arn.clone()?;
        let policy = self.bucket_policy(client, bucket).await;
        // Read after the policy lookup, which takes the whole record.
        let identity = self.identity.as_deref();
        // Neither the role nor the bucket carries a document, so there is nothing in force
        // over this call to decide it: the crawl reads its target the way it does where no IAM
        // is mounted to ask at all.
        if identity.is_none_or(<[_]>::is_empty) && policy.is_none() {
            return None;
        }
        let mut context: Vec<(String, String)> = vec![(PRINCIPAL_ARN_KEY.to_string(), role_arn)];
        context.extend(extra_context.iter().cloned());
        let request = authorization::Request::new(action, resource, &self.principals)
            .with_context(&context);
        let by_resource = policy.map_or(authorization::Decision::NotStated, |document| {
            authorization::resource_decision(&document, &request)
        });
        let refusal = |by_identity: &str| {
            format!(
                "the crawl role is not permitted to perform {action} on {resource}: its \
                 identity-based policies answered {by_identity} and the bucket's own policy \
                 answered {by_resource:?}"
            )
        };
        let Some(identity) = identity else {
            // Nothing can be shown about the role's own documents, so the bucket's explicit
            // deny is the only thing left that withholds the call.
            return (by_resource == authorization::Decision::Denied)
                .then(|| refusal("nothing this account can read"));
        };
        let by_identity = authorization::identity_decision_prepared(identity, &request);
        if authorization::is_allowed(by_identity, by_resource) {
            return None;
        }
        Some(refusal(&format!("{by_identity:?}")))
    }

    /// The policy `bucket` carries, as the object store answers it — read once per bucket.
    ///
    /// The store's own `GetBucketPolicy` is what answers, over the in-process callback this
    /// crawl already reads its objects through, so the policy decided against is the one
    /// `PutBucketPolicy` wrote. `None` for a bucket the store answered holds no policy, and for
    /// one it answered nothing readable about: a policy that could not be read states nothing,
    /// which leaves the decision to the role's own documents.
    async fn bucket_policy(
        &mut self,
        client: &::aws_sdk_s3::Client,
        bucket: &str,
    ) -> Option<String> {
        if let Some(held) = self.policies.get(bucket) {
            return held.clone();
        }
        let answered = match client.get_bucket_policy().bucket(bucket).send().await {
            Ok(answered) => answered.policy,
            Err(error) => {
                let code = error.code().unwrap_or_default();
                if code != NO_SUCH_BUCKET_POLICY_CODE {
                    ::tracing::debug!(
                        bucket = ?bucket,
                        code = %code,
                        "the object store answered nothing readable about a crawl target's \
                         bucket policy"
                    );
                }
                None
            }
        };
        // The cache is what bounds the lookups: past the ceiling a bucket is asked about
        // again rather than remembered, so one crawl's memory does not grow with the targets
        // its crawler names.
        if self.policies.len() < MAX_CRAWLED_BUCKET_POLICIES {
            self.policies.insert(bucket.to_string(), answered.clone());
        }
        answered
    }
}

/// The ARN of an Amazon S3 BUCKET: `arn:aws:s3:::<bucket>`.
///
/// An S3 bucket name is global, so the ARN carries neither a Region nor an account — which is
/// the form a bucket policy and an identity policy both name a bucket in.
#[must_use]
pub fn bucket_arn(bucket: &str) -> String {
    format!("arn:aws:s3:::{bucket}")
}

/// The ARN of one OBJECT in a bucket: `arn:aws:s3:::<bucket>/<key>`, the form a statement
/// granting an object action names it in.
#[must_use]
pub fn object_arn(bucket: &str, key: &str) -> String {
    format!("arn:aws:s3:::{bucket}/{key}")
}
