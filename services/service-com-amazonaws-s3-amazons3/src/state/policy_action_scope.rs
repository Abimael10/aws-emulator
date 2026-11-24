// SPDX-License-Identifier: CC0-1.0

//! Whether the actions a bucket-policy statement names can be performed on the resources
//! that same statement names.
//!
//! A resource policy states a grant as a triple — who, which actions, which resources — and a
//! statement whose action cannot be performed on any resource it names grants nothing at all:
//! an object read scoped to the bucket itself, or a bucket listing scoped to the bucket's
//! objects. S3 refuses such a document rather than storing it, so the caller learns the grant
//! it wrote cannot work while it is still looking at the error — as it does for a value the
//! policy LANGUAGE cannot read as an action at all
//! (`::service_common::iam_policy::readable_action`, the reading every service judges that
//! by).
//!
//! Two facts decide applicability, and each is read from the document alone:
//!
//! * which RESOURCES a value names — a general purpose bucket is
//!   `arn:<partition>:s3:::<bucket>` and an object in one `arn:<partition>:s3:::<bucket>/<key>`
//!   (the ARN format Amazon S3 publishes for its `bucket` and `object` resource types, which
//!   carries neither a Region nor an owning account), so a resource part with a literal `/`
//!   in it names objects and one without names a bucket ([`resource_scope`]);
//! * which RESOURCE TYPE an action is performed on — the `bucket` type, the `object` type, or
//!   no resource type at all for an action performed on the account ([`ACTION_SCOPES`]).
//!
//! A value either reading cannot place is not judged here: a resource that is no general
//! purpose bucket or object ARN (the wildcard, an access point, another service's resource,
//! a bucket name carrying an IAM wildcard, which could expand into either form), an action
//! outside the `s3:` namespace (`s3express:CreateSession`, the one action a directory bucket's
//! policy names) and an action no row of the catalog knows leave the statement carrying them
//! unjudged, because nothing about them says the grant cannot work.
//!
//! The reading is the document's own, so it holds for every statement of every document put
//! on the subresource rather than for the forms one request happened to use.

#[allow(unused_imports)]
use super::*;

/// What kind of resource an S3 IAM action is performed on — the "resource types" column of the
/// action's own row in the publication [`ACTION_SCOPES`] cites.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionScope {
    /// The `bucket` resource type: the action is performed on a bucket, named
    /// `arn:<partition>:s3:::<bucket>`.
    Bucket,
    /// The `object` resource type: the action is performed on an object in a bucket, named
    /// `arn:<partition>:s3:::<bucket>/<key>`.
    Object,
    /// NO resource type: the action is performed on the account itself (its own bucket
    /// listing, its account-wide Block Public Access configuration), so a policy reaches it
    /// only through an element covering every resource of the service.
    Account,
}

/// Why a statement's action cannot be performed on anything the statement names.
///
/// Both are a `MalformedPolicy` (400) — see [`super::MALFORMED_POLICY_CODE`] — and each
/// carries the sentence S3 states for its own condition ([`Self::message`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyActionFault {
    /// The `Action` element is no action the policy LANGUAGE can read
    /// (`::service_common::iam_policy::readable_action`): a value that is neither the
    /// wildcard nor a `<namespace>:<action>` pair, so it names no action of any service.
    Unreadable(super::PolicyStatementLocation),
    /// The action is one this service publishes, but it is performed on a resource TYPE none
    /// of the resources the statement names is of.
    InapplicableToResources(super::PolicyStatementLocation),
}

impl PolicyActionFault {
    /// The `<Message>` S3 sends for this condition.
    ///
    /// Fixed text per condition, naming neither the action nor the statement: both of those
    /// are the variable part, and they travel in the `<Detail>` member of the same document
    /// ([`Self::detail`]).
    #[must_use]
    pub fn message(&self) -> &'static str {
        match self {
            Self::Unreadable(_) => ::service_common::iam_policy::POLICY_INVALID_ACTION_MESSAGE,
            Self::InapplicableToResources(_) => INAPPLICABLE_ACTION_MESSAGE,
        }
    }

    /// The `<Detail>` of that document: the faulted action and the statement carrying it,
    /// both as the submitted document spells them — the one place such a refusal says which
    /// of a document's statements the caller has to change
    /// ([`super::PolicyStatementLocation`] is the locator every policy-document refusal says
    /// where it is with).
    #[must_use]
    pub fn detail(&self) -> String {
        match self {
            Self::Unreadable(location) | Self::InapplicableToResources(location) => {
                location.detail()
            }
        }
    }
}

/// The `<Message>` S3 sends with [`super::MALFORMED_POLICY_CODE`] for a statement whose
/// action is performed on no resource type any resource it names is of — fixed text stating
/// the condition and nothing of the request.
pub const INAPPLICABLE_ACTION_MESSAGE: &str =
    "Action does not apply to any resource(s) in statement";

/// The first action of `policy` that can be performed on nothing the statement carrying it
/// names, or `None` when every statement's actions can.
///
/// The statements are walked in the order the document writes them and, within one, the
/// actions in the order it names them, so the fault reported is the first one a reader of the
/// document would reach. A text this cannot read as a policy carries no fault here: what
/// makes a document unusable as a policy at all is [`super::is_valid_policy_document`]'s
/// reading, taken before this one.
#[must_use]
pub fn policy_action_fault(policy: &str) -> Option<PolicyActionFault> {
    let parsed = ::serde_json::from_str::<::serde_json::Value>(policy).ok()?;
    let statements = super::policy_statements(&parsed)?;
    statements
        .iter()
        .filter_map(|statement| statement.as_object())
        .find_map(statement_action_fault)
}

/// [`policy_action_fault`] for ONE statement.
fn statement_action_fault(
    statement: &::serde_json::Map<String, ::serde_json::Value>,
) -> Option<PolicyActionFault> {
    let location = super::PolicyStatementLocation::of(statement);
    // The resource types the statement's own `Resource` element names, or `None` when it
    // names a value this reading does not place — in which case only the SPELLING of an
    // action can be faulted, never its applicability.
    let named = statement_resource_scopes(statement);
    element_values(statement.get("Action")?)
        .into_iter()
        .find_map(|action| {
            // The refusal is about THIS action of the statement, so the locator names it
            // rather than the first one the statement carries.
            let faulted = location.clone().naming(&action);
            if !::service_common::iam_policy::readable_action(&action) {
                return Some(PolicyActionFault::Unreadable(faulted));
            }
            let published = published_action_scopes(&action)?;
            let named = named.as_ref()?;
            let applies = published
                .iter()
                .any(|scope| named.iter().any(|resource| resource == scope));
            (!applies).then_some(PolicyActionFault::InapplicableToResources(faulted))
        })
}

/// The resource types the `Resource` element of `statement` names, or `None` when the
/// statement names no resource at all or names one this reading does not place (see the module
/// docs): either way nothing about the resources says an action cannot be performed on them.
fn statement_resource_scopes(
    statement: &::serde_json::Map<String, ::serde_json::Value>,
) -> Option<Vec<ActionScope>> {
    let named = element_values(statement.get("Resource")?);
    if named.is_empty() {
        return None;
    }
    let mut scopes: Vec<ActionScope> = Vec::new();
    for resource in named {
        for scope in resource_scope(&resource)? {
            if !scopes.contains(scope) {
                scopes.push(*scope);
            }
        }
    }
    Some(scopes)
}

/// The resource types `resource` names, or `None` for a value this reading does not place.
///
/// The general purpose bucket and object ARN format Amazon S3 publishes carries neither a
/// Region nor an owning account, so the value is read as `arn:<partition>:s3:::<resource>`:
/// a resource part with a literal `/` in it names objects — the `/` has to be matched
/// literally, so such a pattern can match no bucket ARN — and one without names a bucket.
/// A resource part carrying an IAM wildcard before any `/` is not placed: `*` stands for any
/// run of characters, the `/` of an object ARN among them, so such a value can name either.
#[must_use]
pub fn resource_scope(resource: &str) -> Option<&'static [ActionScope]> {
    let mut sections = resource.split(':');
    if sections.next()? != "arn" {
        return None;
    }
    // The partition, which the resource type does not depend on.
    sections.next()?;
    if sections.next()? != "s3" {
        return None;
    }
    // A general purpose bucket's name is global, so the Region and the owning account are
    // both empty in the ARN of a bucket or of an object in one; anything else — an access
    // point, an Outposts bucket, a job — is a resource type this reading does not place.
    if !sections.next()?.is_empty() || !sections.next()?.is_empty() {
        return None;
    }
    let path: Vec<&str> = sections.collect();
    let path = path.join(":");
    match path.split_once('/') {
        Some(_) => Some(&[ActionScope::Object]),
        None if path.contains(&super::WILDCARD_CHARACTERS[..]) => None,
        None if path.is_empty() => None,
        None => Some(&[ActionScope::Bucket]),
    }
}

/// The resource types the S3 actions `element` names are performed on, or `None` when this
/// catalog does not answer for the element: one naming actions of another service's namespace
/// or of every namespace, and one naming an `s3:` action no row of the catalog knows.
///
/// An element is a pattern — "the wildcards `*` and `?` … within an action name" — so it is
/// matched against the catalogued actions rather than looked up, and the types it names are
/// those of every action it matches. IAM compares action names case-insensitively, which is
/// how the match is taken.
///
/// The catalog is read in ONE direction only: it says which resource types an action it knows
/// is performed on, and never that an action is no action. An excerpt that answered the other
/// question would refuse a grant S3 accepts for every published action a row has not been
/// filed for yet; whether a value names an action AT ALL is the policy language's reading
/// (`::service_common::iam_policy::readable_action`), which no catalog can go stale against.
#[must_use]
pub fn published_action_scopes(element: &str) -> Option<Vec<ActionScope>> {
    let (namespace, _) = element.split_once(':')?;
    // An element whose namespace is itself a pattern reaches other services' actions too;
    // `s3express:` is the namespace a directory bucket's policy names, and it publishes its
    // own actions.
    if !namespace.eq_ignore_ascii_case(S3_ACTION_NAMESPACE) {
        return None;
    }
    let mut scopes: Vec<ActionScope> = Vec::new();
    for (action, scope) in ACTION_SCOPES {
        let matched = ::service_common::iam_authorization::wildcard_match(element, action, false);
        if matched && !scopes.contains(scope) {
            scopes.push(*scope);
        }
    }
    (!scopes.is_empty()).then_some(scopes)
}

/// The values one policy element names: a bare string, or a list of them — the two forms IAM
/// writes an element in.
///
/// `pub(super)`: the one reading of a STATEMENT's own element in this crate, which the
/// statement locator ([`super::PolicyStatementLocation`]) reads its action by too. The
/// document-level readings of the same elements are
/// `::service_common::iam_policy::{action_values, resource_element_values}`; this one is
/// needed because applicability is a question about one statement's action beside THAT
/// statement's resources.
pub(super) fn element_values(element: &::serde_json::Value) -> Vec<String> {
    match element {
        ::serde_json::Value::String(value) => vec![value.clone()],
        ::serde_json::Value::Array(list) => list.iter().flat_map(element_values).collect(),
        _ => Vec::new(),
    }
}

/// The namespace Amazon S3's own actions are published under, which is the one
/// [`ACTION_SCOPES`] answers for.
const S3_ACTION_NAMESPACE: &str = "s3";

/// Every action Amazon S3 publishes, with the resource type it is performed on.
///
/// SOURCE: the Service Authorization Reference's "Actions defined by Amazon S3" table — the
/// resource-types column of each action's row — together with this service's own model, which
/// states the permission each of its operations needs in that operation's documentation
/// (`smithy_query(query="doc",shape="com.amazonaws.s3#GetObject")` and its siblings: "you must
/// have the `s3:GetObject` permission", "you must have the `s3:ListBucket` action", and so on
/// for every operation). An action performed on an object carries the `object` resource type,
/// one performed on a bucket or one of its subresources the `bucket` type, and one performed
/// on the account itself no resource type at all.
///
/// Ascending by action, so a new row is filed at its place rather than appended and a
/// duplicate cannot hide in the list (asserted below). It is an EXCERPT of that publication,
/// and the reading over it ([`published_action_scopes`]) resolves an action no row names to
/// the PERMISSIVE answer — such a statement is stored exactly as before this catalog existed —
/// so a row that has not been filed yet can never refuse a grant S3 accepts. A row is added
/// here as the applicability of another action comes up.
pub const ACTION_SCOPES: &[(&str, ActionScope)] = &[
    ("s3:AbortMultipartUpload", ActionScope::Object),
    ("s3:BypassGovernanceRetention", ActionScope::Object),
    ("s3:CreateBucket", ActionScope::Bucket),
    ("s3:CreateBucketMetadataConfiguration", ActionScope::Bucket),
    (
        "s3:CreateBucketMetadataTableConfiguration",
        ActionScope::Bucket,
    ),
    ("s3:DeleteAccountPublicAccessBlock", ActionScope::Account),
    ("s3:DeleteBucket", ActionScope::Bucket),
    ("s3:DeleteBucketMetadataConfiguration", ActionScope::Bucket),
    (
        "s3:DeleteBucketMetadataTableConfiguration",
        ActionScope::Bucket,
    ),
    ("s3:DeleteBucketPolicy", ActionScope::Bucket),
    ("s3:DeleteBucketWebsite", ActionScope::Bucket),
    ("s3:DeleteObject", ActionScope::Object),
    ("s3:DeleteObjectAnnotation", ActionScope::Object),
    ("s3:DeleteObjectTagging", ActionScope::Object),
    ("s3:DeleteObjectVersion", ActionScope::Object),
    ("s3:DeleteObjectVersionTagging", ActionScope::Object),
    ("s3:GetAccelerateConfiguration", ActionScope::Bucket),
    ("s3:GetAccountPublicAccessBlock", ActionScope::Account),
    ("s3:GetAnalyticsConfiguration", ActionScope::Bucket),
    ("s3:GetBucketABAC", ActionScope::Bucket),
    ("s3:GetBucketAcl", ActionScope::Bucket),
    ("s3:GetBucketCORS", ActionScope::Bucket),
    ("s3:GetBucketLocation", ActionScope::Bucket),
    ("s3:GetBucketLogging", ActionScope::Bucket),
    ("s3:GetBucketMetadataTableConfiguration", ActionScope::Bucket),
    ("s3:GetBucketNotification", ActionScope::Bucket),
    ("s3:GetBucketObjectLockConfiguration", ActionScope::Bucket),
    ("s3:GetBucketOwnershipControls", ActionScope::Bucket),
    ("s3:GetBucketPolicy", ActionScope::Bucket),
    ("s3:GetBucketPolicyStatus", ActionScope::Bucket),
    ("s3:GetBucketPublicAccessBlock", ActionScope::Bucket),
    ("s3:GetBucketRequestPayment", ActionScope::Bucket),
    ("s3:GetBucketTagging", ActionScope::Bucket),
    ("s3:GetBucketVersioning", ActionScope::Bucket),
    ("s3:GetBucketWebsite", ActionScope::Bucket),
    ("s3:GetEncryptionConfiguration", ActionScope::Bucket),
    ("s3:GetIntelligentTieringConfiguration", ActionScope::Bucket),
    ("s3:GetInventoryConfiguration", ActionScope::Bucket),
    ("s3:GetLifecycleConfiguration", ActionScope::Bucket),
    ("s3:GetMetricsConfiguration", ActionScope::Bucket),
    ("s3:GetObject", ActionScope::Object),
    ("s3:GetObjectAcl", ActionScope::Object),
    ("s3:GetObjectAnnotation", ActionScope::Object),
    ("s3:GetObjectAttributes", ActionScope::Object),
    ("s3:GetObjectLegalHold", ActionScope::Object),
    ("s3:GetObjectRetention", ActionScope::Object),
    ("s3:GetObjectTagging", ActionScope::Object),
    ("s3:GetObjectTorrent", ActionScope::Object),
    ("s3:GetObjectVersion", ActionScope::Object),
    ("s3:GetObjectVersionAcl", ActionScope::Object),
    ("s3:GetObjectVersionAnnotation", ActionScope::Object),
    ("s3:GetObjectVersionAttributes", ActionScope::Object),
    ("s3:GetObjectVersionForReplication", ActionScope::Object),
    ("s3:GetObjectVersionTagging", ActionScope::Object),
    ("s3:GetObjectVersionTorrent", ActionScope::Object),
    ("s3:GetReplicationConfiguration", ActionScope::Bucket),
    ("s3:InitiateReplication", ActionScope::Object),
    ("s3:ListAllMyBuckets", ActionScope::Account),
    ("s3:ListBucket", ActionScope::Bucket),
    ("s3:ListBucketMultipartUploads", ActionScope::Bucket),
    ("s3:ListBucketVersions", ActionScope::Bucket),
    ("s3:ListMultipartUploadParts", ActionScope::Object),
    ("s3:ListObjectAnnotations", ActionScope::Object),
    ("s3:ObjectOwnerOverrideToBucketOwner", ActionScope::Object),
    ("s3:PutAccelerateConfiguration", ActionScope::Bucket),
    ("s3:PutAccountPublicAccessBlock", ActionScope::Account),
    ("s3:PutAnalyticsConfiguration", ActionScope::Bucket),
    ("s3:PutBucketABAC", ActionScope::Bucket),
    ("s3:PutBucketAcl", ActionScope::Bucket),
    ("s3:PutBucketCORS", ActionScope::Bucket),
    ("s3:PutBucketLogging", ActionScope::Bucket),
    ("s3:PutBucketNotification", ActionScope::Bucket),
    ("s3:PutBucketObjectLockConfiguration", ActionScope::Bucket),
    ("s3:PutBucketOwnershipControls", ActionScope::Bucket),
    ("s3:PutBucketPolicy", ActionScope::Bucket),
    ("s3:PutBucketPublicAccessBlock", ActionScope::Bucket),
    ("s3:PutBucketRequestPayment", ActionScope::Bucket),
    ("s3:PutBucketTagging", ActionScope::Bucket),
    ("s3:PutBucketVersioning", ActionScope::Bucket),
    ("s3:PutBucketWebsite", ActionScope::Bucket),
    ("s3:PutEncryptionConfiguration", ActionScope::Bucket),
    ("s3:PutIntelligentTieringConfiguration", ActionScope::Bucket),
    ("s3:PutInventoryConfiguration", ActionScope::Bucket),
    ("s3:PutLifecycleConfiguration", ActionScope::Bucket),
    ("s3:PutMetricsConfiguration", ActionScope::Bucket),
    ("s3:PutObject", ActionScope::Object),
    ("s3:PutObjectAcl", ActionScope::Object),
    ("s3:PutObjectAnnotation", ActionScope::Object),
    ("s3:PutObjectLegalHold", ActionScope::Object),
    ("s3:PutObjectRetention", ActionScope::Object),
    ("s3:PutObjectTagging", ActionScope::Object),
    ("s3:PutObjectVersionAcl", ActionScope::Object),
    ("s3:PutObjectVersionTagging", ActionScope::Object),
    ("s3:PutReplicationConfiguration", ActionScope::Bucket),
    ("s3:ReplicateDelete", ActionScope::Object),
    ("s3:ReplicateObject", ActionScope::Object),
    ("s3:ReplicateTags", ActionScope::Object),
    ("s3:RestoreObject", ActionScope::Object),
    (
        "s3:UpdateBucketMetadataAnnotationTableConfiguration",
        ActionScope::Bucket,
    ),
    (
        "s3:UpdateBucketMetadataInventoryTableConfiguration",
        ActionScope::Bucket,
    ),
    (
        "s3:UpdateBucketMetadataJournalTableConfiguration",
        ActionScope::Bucket,
    ),
    ("s3:UpdateObjectEncryption", ActionScope::Object),
];

#[cfg(test)]
mod tests {
    use super::*;

    /// The catalog ascends strictly — so no action is listed twice — and every row names an
    /// action in the `s3:<Action>` form a policy element is matched against.
    #[test]
    fn the_catalog_is_ordered_and_every_row_names_an_s3_action() {
        for pair in ACTION_SCOPES.windows(2) {
            assert!(
                pair[0].0 < pair[1].0,
                "the catalog must ascend strictly: {:?} is not before {:?}",
                pair[0].0,
                pair[1].0
            );
        }
        for (action, _) in ACTION_SCOPES {
            let named = action
                .split_once(':')
                .is_some_and(|(namespace, operation)| {
                    namespace == S3_ACTION_NAMESPACE && !operation.is_empty()
                });
            assert!(
                named,
                "case {action:?}: every row must name an action as s3:<Action>"
            );
        }
    }

    /// The two readings the rule rests on, over the forms a document writes them in.
    #[test]
    fn a_resource_is_placed_by_the_published_arn_format() {
        let rows: &[(&str, Option<&[ActionScope]>)] = &[
            ("arn:aws:s3:::reports", Some(&[ActionScope::Bucket])),
            ("arn:aws:s3:::reports/key", Some(&[ActionScope::Object])),
            ("arn:aws:s3:::reports/*", Some(&[ActionScope::Object])),
            // A key may itself carry a colon, which the ARN does not escape.
            ("arn:aws:s3:::reports/a:b", Some(&[ActionScope::Object])),
            ("arn:aws-us-gov:s3:::reports", Some(&[ActionScope::Bucket])),
            // Values this reading does not place.
            ("*", None),
            ("arn:aws:s3:::*", None),
            ("arn:aws:s3:::reports*", None),
            ("arn:aws:s3:::", None),
            ("arn:aws:s3:us-east-1:000000000000:accesspoint/reports", None),
            ("arn:aws:sqs:us-east-1:000000000000:queue", None),
            ("not-an-arn-at-all", None),
        ];
        for (resource, expected) in rows {
            assert_eq!(
                resource_scope(resource),
                *expected,
                "case {resource:?}: expected {expected:?}, got {:?}",
                resource_scope(resource)
            );
        }
    }

    /// An action element is matched as the pattern it is; only the `s3:` namespace is answered
    /// for, and an action no row names is left to the policy language.
    #[test]
    fn an_action_element_names_the_scopes_of_every_action_it_matches() {
        let object = Some(vec![ActionScope::Object]);
        let bucket = Some(vec![ActionScope::Bucket]);
        let rows: &[(&str, Option<Vec<ActionScope>>)] = &[
            ("s3:GetObject", object.clone()),
            // IAM compares action names without regard to case.
            ("s3:getobject", object.clone()),
            ("s3:ListBucket", bucket.clone()),
            ("s3:ListAllMyBuckets", Some(vec![ActionScope::Account])),
            // A pattern reaches every action it matches, so its scopes are their union.
            (
                "s3:*",
                Some(vec![
                    ActionScope::Object,
                    ActionScope::Bucket,
                    ActionScope::Account,
                ]),
            ),
            ("s3:GetObject*", object.clone()),
            ("s3:ListBucket*", bucket.clone()),
            // An action this excerpt has filed no row for is not placed, so the statement
            // carrying it is stored: the catalog can only fail open.
            ("s3:GetAccessPoint", None),
            ("s3:GetObjects", None),
            // Another namespace, a value with none, and one that is itself a pattern.
            ("s3express:CreateSession", None),
            ("GetObject", None),
            ("*", None),
            ("sqs:SendMessage", None),
        ];
        for (element, expected) in rows {
            assert_eq!(
                published_action_scopes(element),
                *expected,
                "case {element:?}: expected {expected:?}, got {:?}",
                published_action_scopes(element)
            );
        }
    }
}
