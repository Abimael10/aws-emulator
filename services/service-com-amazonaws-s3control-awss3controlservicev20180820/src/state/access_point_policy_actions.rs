// SPDX-License-Identifier: CC0-1.0

//! Which ACTIONS a policy attached to an access point may name, WHO it may grant to and
//! WHICH resource it may apply to, and the refusal a document naming anything else is
//! answered with.
//!
//! An access point fronts S3 storage, so a statement of its policy grants an action of
//! the S3 family's own API. That makes two separate readings of an action element, in
//! the order the validator applies them:
//!
//! * the policy language's reading of an action's FORM — the wildcard, or a
//!   `<namespace>:<name>` pair written in the characters each half is written in
//!   ([`::service_common::iam_policy::readable_action`]). A document whose action
//!   element names something else (a token with no namespace, a value that is not a
//!   string, an element naming nothing at all) is not a document the policy parser can
//!   read, so it is refused as a policy whose syntax is invalid
//!   ([`POLICY_SYNTAX_INVALID_MESSAGE`]);
//! * this surface's reading of WHICH actions exist — a readable token names an action
//!   only if its namespace is one of the S3 family's ([`S3_ACTION_NAMESPACES`]) and its
//!   name is one those APIs publish ([`PUBLISHED_ACTION_NAMES`]). A token that reads as
//!   an action but names none describes a permission no policy engine could enforce, so
//!   it is refused as an invalid action, with the offending token reported back in the
//!   refusal ([`POLICY_DETAIL_MEMBER`]) rather than left for the caller to find
//!   in the document it sent.
//!
//! A statement's PRINCIPAL elements are read beside those two, because a resource-based
//! policy states WHO its grants reach: a value in none of the forms the policy language
//! defines for its principal type denotes no identity any policy engine could evaluate,
//! and neither does an IAM identity this account's IAM does not hold
//! ([`POLICY_INVALID_PRINCIPAL_MESSAGE`], [`unresolved_policy_principal`]). Either way the
//! refusal quotes the submitted document's own MEMBER naming that principal — the type key
//! and the value together ([`invalid_policy_principal`]) — because a principal is valid only
//! relative to the type it is named under.
//!
//! A statement's RESOURCE elements are read beside them, because a resource-based
//! policy governs the resource it is attached to: an access point's policy names that
//! access point, or the objects reached through it, so a statement naming anything else
//! — a bucket, another access point, a value that is no S3 ARN at all — grants
//! permissions this document could never apply to
//! ([`access_point_resource_refusal`]).
//!
//! Each clause is a distinct cause and carries a distinct sentence, because a caller
//! reading the refusal has to fix a different thing in each case: the shape of the
//! document, the action it named, who it granted to, or what it applied to.

use ::aws_smithy_schema::{Schema, ShapeId, ShapeType};

/// The IAM service namespaces an action an access point policy grants is named in: the
/// prefixes of the S3 family.
///
/// An access point fronts S3 storage, so an action named in another service's namespace
/// names nothing the access point could allow. The prefixes are the ones the S3 family
/// publishes for its APIs — general-purpose and directory buckets (`s3`, `s3express`),
/// Object Lambda access points (`s3-object-lambda`) and S3 on Outposts (`s3-outposts`).
pub const S3_ACTION_NAMESPACES: [&str; 4] = ["s3", "s3-object-lambda", "s3-outposts", "s3express"];

/// Every action NAME the S3 family's APIs publish — the half after the namespace of an
/// action a policy may name.
///
/// The catalogue is the union of the actions AWS's Service Authorization Reference
/// publishes for the family's namespaces ("Actions defined by Amazon S3", "… by Amazon
/// S3 on Outposts", "… by Amazon S3 Object Lambda" and "… by Amazon S3 Express One
/// Zone"), read as ONE set across them: an access point may front a general-purpose
/// bucket, a directory bucket or an Outposts bucket, and the same document is attached to
/// each, so this reading refuses the names NO S3 API publishes rather than deciding which
/// namespace publishes which name. Pairing one namespace with another's action name is
/// therefore accepted — a deliberate over-acceptance, because the clause AWS's recorded
/// refusal states is that the action does not exist, and the namespaces share most of
/// their names.
///
/// A name is read as the policy language reads an action name: case-insensitively, with
/// the wildcards it carries standing for every published name they match
/// ([`names_published_action`]).
///
/// The catalogue is a table of AWS's published action NAMES, which is not the same list
/// as this service's model's operation names and is not derived from them: IAM names
/// several operations under one action (a bucket's tag removal is `PutBucketTagging`, a
/// lifecycle read `GetLifecycleConfiguration`) and publishes actions no operation of this
/// model carries (the object actions of the data plane).
pub const PUBLISHED_ACTION_NAMES: &[&str] = &[
    "AbortMultipartUpload",
    "AssociateAccessGrantsIdentityCenter",
    "BypassGovernanceRetention",
    "CreateAccessGrant",
    "CreateAccessGrantsInstance",
    "CreateAccessGrantsLocation",
    "CreateAccessPoint",
    "CreateAccessPointForObjectLambda",
    "CreateBucket",
    "CreateBucketMetadataConfiguration",
    "CreateBucketMetadataTableConfiguration",
    "CreateEndpoint",
    "CreateJob",
    "CreateMultiRegionAccessPoint",
    "CreateSession",
    "CreateStorageLensGroup",
    "DeleteAccessGrant",
    "DeleteAccessGrantsInstance",
    "DeleteAccessGrantsInstanceResourcePolicy",
    "DeleteAccessGrantsLocation",
    "DeleteAccessPoint",
    "DeleteAccessPointForObjectLambda",
    "DeleteAccessPointPolicy",
    "DeleteAccessPointPolicyForObjectLambda",
    "DeleteAccessPointScope",
    "DeleteBucket",
    "DeleteBucketMetadataConfiguration",
    "DeleteBucketMetadataTableConfiguration",
    "DeleteBucketPolicy",
    "DeleteBucketWebsite",
    "DeleteEndpoint",
    "DeleteJobTagging",
    "DeleteMultiRegionAccessPoint",
    "DeleteObject",
    "DeleteObjectTagging",
    "DeleteObjectVersion",
    "DeleteObjectVersionTagging",
    "DeleteStorageLensConfiguration",
    "DeleteStorageLensConfigurationTagging",
    "DeleteStorageLensGroup",
    "DescribeJob",
    "DescribeMultiRegionAccessPointOperation",
    "DissociateAccessGrantsIdentityCenter",
    "GetAccelerateConfiguration",
    "GetAccessGrant",
    "GetAccessGrantsInstance",
    "GetAccessGrantsInstanceForPrefix",
    "GetAccessGrantsInstanceResourcePolicy",
    "GetAccessGrantsLocation",
    "GetAccessPoint",
    "GetAccessPointConfigurationForObjectLambda",
    "GetAccessPointForObjectLambda",
    "GetAccessPointPolicy",
    "GetAccessPointPolicyForObjectLambda",
    "GetAccessPointPolicyStatus",
    "GetAccessPointPolicyStatusForObjectLambda",
    "GetAccessPointScope",
    "GetAccountPublicAccessBlock",
    "GetAnalyticsConfiguration",
    "GetBucket",
    "GetBucketAcl",
    "GetBucketCORS",
    "GetBucketLocation",
    "GetBucketLogging",
    "GetBucketMetadataConfiguration",
    "GetBucketMetadataTableConfiguration",
    "GetBucketNotification",
    "GetBucketObjectLockConfiguration",
    "GetBucketOwnershipControls",
    "GetBucketPolicy",
    "GetBucketPolicyStatus",
    "GetBucketPublicAccessBlock",
    "GetBucketRequestPayment",
    "GetBucketTagging",
    "GetBucketVersioning",
    "GetBucketWebsite",
    "GetDataAccess",
    "GetEncryptionConfiguration",
    "GetIntelligentTieringConfiguration",
    "GetInventoryConfiguration",
    "GetJobTagging",
    "GetLifecycleConfiguration",
    "GetMetricsConfiguration",
    "GetMultiRegionAccessPoint",
    "GetMultiRegionAccessPointPolicy",
    "GetMultiRegionAccessPointPolicyStatus",
    "GetMultiRegionAccessPointRoutes",
    "GetObject",
    "GetObjectAcl",
    "GetObjectAttributes",
    "GetObjectLegalHold",
    "GetObjectRetention",
    "GetObjectTagging",
    "GetObjectTorrent",
    "GetObjectVersion",
    "GetObjectVersionAcl",
    "GetObjectVersionAttributes",
    "GetObjectVersionForReplication",
    "GetObjectVersionTagging",
    "GetObjectVersionTorrent",
    "GetReplicationConfiguration",
    "GetStorageLensConfiguration",
    "GetStorageLensConfigurationTagging",
    "GetStorageLensDashboard",
    "GetStorageLensGroup",
    "InitiateReplication",
    "ListAccessGrants",
    "ListAccessGrantsInstances",
    "ListAccessGrantsLocations",
    "ListAccessPoints",
    "ListAccessPointsForDirectoryBuckets",
    "ListAccessPointsForObjectLambda",
    "ListAllMyBuckets",
    "ListAllMyDirectoryBuckets",
    "ListBucket",
    "ListBucketMultipartUploads",
    "ListBucketVersions",
    "ListCallerAccessGrants",
    "ListEndpoints",
    "ListJobs",
    "ListMultiRegionAccessPoints",
    "ListMultipartUploadParts",
    "ListOutpostsWithS3",
    "ListRegionalBuckets",
    "ListSharedEndpoints",
    "ListStorageLensConfigurations",
    "ListStorageLensGroups",
    "ListTagsForResource",
    "ObjectOwnerOverrideToBucketOwner",
    "PauseReplication",
    "PutAccelerateConfiguration",
    "PutAccessGrantsInstanceResourcePolicy",
    "PutAccessPointConfigurationForObjectLambda",
    "PutAccessPointPolicy",
    "PutAccessPointPolicyForObjectLambda",
    "PutAccessPointPublicAccessBlock",
    "PutAccessPointScope",
    "PutAccountPublicAccessBlock",
    "PutAnalyticsConfiguration",
    "PutBucketAcl",
    "PutBucketCORS",
    "PutBucketLogging",
    "PutBucketNotification",
    "PutBucketObjectLockConfiguration",
    "PutBucketOwnershipControls",
    "PutBucketPolicy",
    "PutBucketPublicAccessBlock",
    "PutBucketRequestPayment",
    "PutBucketTagging",
    "PutBucketVersioning",
    "PutBucketWebsite",
    "PutEncryptionConfiguration",
    "PutIntelligentTieringConfiguration",
    "PutInventoryConfiguration",
    "PutJobTagging",
    "PutLifecycleConfiguration",
    "PutMetricsConfiguration",
    "PutMultiRegionAccessPointPolicy",
    "PutObject",
    "PutObjectAcl",
    "PutObjectLegalHold",
    "PutObjectRetention",
    "PutObjectTagging",
    "PutObjectVersionAcl",
    "PutObjectVersionTagging",
    "PutReplicationConfiguration",
    "PutStorageLensConfiguration",
    "PutStorageLensConfigurationTagging",
    "ReplicateDelete",
    "ReplicateObject",
    "ReplicateTags",
    "RestoreObject",
    "SubmitMultiRegionAccessPointRoutes",
    "TagResource",
    "UntagResource",
    "UpdateAccessGrantsLocation",
    "UpdateBucketMetadataInventoryTableConfiguration",
    "UpdateBucketMetadataJournalTableConfiguration",
    "UpdateJobPriority",
    "UpdateJobStatus",
    "UpdateStorageLensGroup",
    "WriteGetObjectResponse",
];

/// The sentence a document the policy parser cannot READ is refused with: the text is
/// JSON and carries the names the language defines, but an element of it names a value
/// the language has no reading for, so the document has no policy syntax to interpret.
/// Fixed text, carrying nothing of the submitted document.
pub const POLICY_SYNTAX_INVALID_MESSAGE: &str = "Invalid policy syntax.";

/// The `<Error>` member a refusal of a policy document names the OFFENDING value in.
///
/// The message states the class of failure in fixed words, so which value the document
/// named travels in a member beside it — the same split every refusal of this service
/// keeps (`crate::state` carries the resource-naming members). S3 Control models no
/// error shape for these operations, so the member exists only on the wire.
pub static POLICY_DETAIL_MEMBER: Schema = Schema::new_member(
    ShapeId::from_static(
        "com.amazonaws.s3control#ErrorDetails$Detail",
        "com.amazonaws.s3control",
        "Detail",
    ),
    ShapeType::String,
    "Detail",
    1,
)
.with_xml_name("Detail");

/// The refusal a document the ACCESS POINT policy surface will not attach is answered
/// with, or `None` for one it stores.
///
/// Every clause of the validator that surface runs, each answered with AWS's own
/// sentence for it: the text is a policy object, every member name it carries is one the
/// policy language defines, it carries the element the language requires, it declares a
/// version string the language defines
/// ([`::service_common::iam_policy::attached_policy_defect`]), every action element
/// names an action the language can READ, every action it then names is one the S3
/// family publishes ([`grantable_action`]) and every principal its statements name is a
/// principal of the type it was named under ([`unreadable_principal_of`]).
///
/// The RESOURCE elements are read beside this, by the surface that knows WHICH access
/// point the document is being attached to ([`access_point_resource_refusal`]).
///
/// A document breaking any clause describes nothing an access point's policy could
/// enforce, so it is refused rather than attached. The whole reading is this surface's
/// and not every policy surface's: the Access Grants instance resource policy accepts a
/// member the language does not define and reports it back in the document it renders,
/// so that operation reads only [`crate::state::policy_document_refusal`].
pub fn access_point_policy_refusal<E>(document: &str) -> Option<crate::scaffolded::Scaffolded<E>> {
    // The language's own reading first, with its action clause left at the FORM of an
    // action element (`|_| true`): a document the parser cannot read is refused for its
    // syntax, before anything is read out of it as an action name.
    if let Some(defect) = ::service_common::iam_policy::attached_policy_defect(document, |_| true) {
        return Some(match defect {
            ::service_common::iam_policy::AttachedPolicyDefect::UnreadableAction => {
                crate::state::malformed_policy_stating(POLICY_SYNTAX_INVALID_MESSAGE)
            }
            stated => crate::state::malformed_policy_stating(stated.message()),
        });
    }
    // Every action element reads as an action, so what is left is WHICH actions they
    // name: the first one this surface cannot grant is the one the refusal reports.
    if let Some(action) = ::service_common::iam_policy::action_values(document)
        .into_iter()
        .find(|action| !grantable_action(action))
    {
        return Some(invalid_policy_action(&action));
    }
    // And the PRINCIPAL elements name identities, read against the forms the policy
    // language defines per principal type: a value in none of them denotes no identity,
    // so the document grants to nobody a policy engine could evaluate.
    ::service_common::iam_policy_principals::unreadable_policy_principal(document)
        .map(|named| invalid_policy_principal(document, &named))
}

/// `MalformedPolicy` (HTTP 400) naming `principal`: a statement of `document` names
/// something that is not a principal of the type it was named under, or names an identity
/// this account's IAM does not hold.
///
/// The message is the fixed sentence AWS words this clause with; WHERE in the submitted
/// document the fault is travels in the member beside it, as the invalid-action and
/// invalid-resource refusals carry theirs.
///
/// What that member carries is the document's own MEMBER naming the principal — the type
/// key, the separator and the value, written as a policy member is
/// ([`::service_common::iam_policy_principals::principal_member`]) — and not the value
/// lifted out of it: a principal is valid only relative to the type key it is named under,
/// so the entry is what the caller has to fix and what tooling locates the offending
/// statement by. An action or a resource element names its values on their own, which is
/// why those two refusals report a value where this one reports a member. A value no
/// statement of the document names (a document whose text could not be parsed at all) has
/// no member to quote, so the value itself is reported.
pub fn invalid_policy_principal<E>(
    document: &str,
    principal: &str,
) -> crate::scaffolded::Scaffolded<E> {
    let named = ::service_common::iam_policy_principals::principal_member(document, principal)
        .unwrap_or_else(|| principal.to_string());
    crate::scaffolded::Scaffolded::service_failure_naming(
        crate::state::MALFORMED_POLICY_CODE,
        crate::state::MALFORMED_POLICY_STATUS,
        POLICY_INVALID_PRINCIPAL_MESSAGE,
        vec![crate::scaffolded::ErrorDetail::new(
            &POLICY_DETAIL_MEMBER,
            named,
        )],
    )
}

/// The sentence a document naming something that is no principal is refused with: fixed
/// text, carrying nothing of the submitted document.
pub const POLICY_INVALID_PRINCIPAL_MESSAGE: &str = "Invalid principal in policy";

/// The refusal a document granting to an identity that is NOT THERE earns, or `None`
/// when every identity it names resolves.
///
/// A resource-based policy states who a permission is granted to, so the identities a
/// document names are resolved against this account's IAM before the document is stored:
/// a grant to an identity that does not exist is a grant to nobody, and the access point
/// would carry a policy no authorization could ever apply. It is the same clause as the
/// form reading above and carries the same sentence — what the document named is no
/// principal — so the refusal names the offending value the same way.
///
/// The resolution itself is [`::service_common::iam_principal::identity_is_absent`], the
/// tree's one reading of a policy principal against IAM, so the refusal is grounded in
/// what IAM ANSWERED: a value naming no IAM role or user of this account — the wildcard,
/// an account or its root, a service principal, another account's identity — is resolved
/// by nothing and refuses nothing.
pub async fn unresolved_policy_principal<E>(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
    document: &str,
) -> Option<crate::scaffolded::Scaffolded<E>> {
    for principal in ::service_common::iam_policy_principals::policy_principals(document) {
        if ::service_common::iam_principal::identity_is_absent(ctx, region, &principal).await {
            return Some(invalid_policy_principal(document, &principal));
        }
    }
    None
}

/// The refusal a document naming a resource the access point it is being attached to
/// does not reach is answered with, or `None` when every resource it names is that
/// access point or something under it.
///
/// A resource-based policy governs the resource it is attached to, so an access point's
/// policy names that access point: a statement naming another resource — a bucket, a
/// different access point, something that is no S3 resource at all — grants permissions
/// on something this document can never apply to. `addressed` is the access point the
/// request names, as the request names it ([`addressed_access_point_name`]).
pub fn access_point_resource_refusal<E>(
    document: &str,
    addressed: &str,
) -> Option<crate::scaffolded::Scaffolded<E>> {
    let addressed = addressed_access_point_name(addressed);
    ::service_common::iam_policy::resource_element_values(document)
        .into_iter()
        .find(|resource| !resource_of_access_point(resource, addressed))
        .map(|resource| invalid_policy_resource(&resource))
}

/// The access point a request addresses, read from the label it names it in: the name
/// itself, or the access point named by the ARN the Outposts addressing form puts there
/// (`PutAccessPointPolicy`'s `Name` documents both).
pub fn addressed_access_point_name(label: &str) -> &str {
    s3_arn_resource_path(label)
        .and_then(access_point_named_by)
        .unwrap_or(label)
}

/// `MalformedPolicy` (HTTP 400) naming `resource`: the document names a resource that is
/// not one an access point's policy can apply to.
///
/// The message is the fixed sentence AWS words this clause with; the resource travels in
/// the member beside it, taken from the submitted document, as the invalid-action
/// refusal carries its action.
pub fn invalid_policy_resource<E>(resource: &str) -> crate::scaffolded::Scaffolded<E> {
    crate::scaffolded::Scaffolded::service_failure_naming(
        crate::state::MALFORMED_POLICY_CODE,
        crate::state::MALFORMED_POLICY_STATUS,
        POLICY_INVALID_RESOURCE_MESSAGE,
        vec![crate::scaffolded::ErrorDetail::new(
            &POLICY_DETAIL_MEMBER,
            resource,
        )],
    )
}

/// The sentence a document naming a resource an access point's policy cannot apply to is
/// refused with: fixed text, carrying nothing of the submitted document.
pub const POLICY_INVALID_RESOURCE_MESSAGE: &str = "Policy has invalid resource";

/// Whether `resource` names the access point `addressed`, or something reached through
/// it.
///
/// A resource is named by its ARN, so the value is an ARN in one of the
/// [`S3_ACTION_NAMESPACES`] whose path names that access point
/// ([`access_point_named_by`]) — the access point itself, or the objects under it
/// (`accesspoint/<name>/object/<key>`). The name is read as the policy language reads a
/// resource value, so the wildcards it carries stand for the names they match.
///
/// Only the SERVICE and the PATH of the ARN are read. Which account and which Region the
/// access point belongs to is not the document's to state: the request already names the
/// account (`x-amz-account-id`) and is signed for the Region, and within those an access
/// point is identified by its name.
pub fn resource_of_access_point(resource: &str, addressed: &str) -> bool {
    s3_arn_resource_path(resource)
        .and_then(access_point_named_by)
        .is_some_and(|named| {
            ::service_common::iam_authorization::wildcard_match(named, addressed, true)
        })
}

/// The resource PATH of `resource` when it is an ARN in one of the
/// [`S3_ACTION_NAMESPACES`], or `None` for any other value.
///
/// Read through the workspace's ONE caller-supplied-ARN parse
/// ([`::service_common::arn::parse`]), so this service does not split an ARN its own
/// way: what is left to decide here is only whether the vendor it names is one of the S3
/// family's.
fn s3_arn_resource_path(resource: &str) -> Option<&str> {
    let arn = ::service_common::arn::parse(resource)?;
    S3_ACTION_NAMESPACES
        .contains(&arn.vendor)
        .then_some(arn.resource)
}

/// The path segment an S3 ARN names an access point under.
const ACCESS_POINT_SEGMENT: &str = "accesspoint";

/// The path segment an S3 on Outposts ARN nests its access points under: on Outposts an
/// access point belongs to an Outpost, so its ARN names the Outpost first.
const OUTPOST_SEGMENT: &str = "outpost";

/// The access point `path` — the resource path of an S3 ARN — names, or `None` when it
/// names no access point at all.
///
/// The access point sits at the head of the path (`accesspoint/<name>`, and the objects
/// under it as `accesspoint/<name>/object/<key>`) or, on Outposts, under its Outpost
/// (`outpost/<id>/accesspoint/<name>`). A path naming a bucket, or naming the access
/// point type with no access point after it, names none.
fn access_point_named_by(path: &str) -> Option<&str> {
    let segments: ::std::vec::Vec<&str> = path.split('/').collect();
    let named = match segments.as_slice() {
        [kind, named, ..] if kind.eq_ignore_ascii_case(ACCESS_POINT_SEGMENT) => named,
        [outpost, _id, kind, named, ..]
            if outpost.eq_ignore_ascii_case(OUTPOST_SEGMENT)
                && kind.eq_ignore_ascii_case(ACCESS_POINT_SEGMENT) =>
        {
            named
        }
        _ => return None,
    };
    (!named.is_empty()).then_some(*named)
}

/// `MalformedPolicy` (HTTP 400) naming `action`: the document names an action that is
/// readable but is not one an access point's policy can grant.
///
/// The message is the fixed sentence AWS words this clause with; the action travels in
/// the member beside it, taken from the submitted document.
pub fn invalid_policy_action<E>(action: &str) -> crate::scaffolded::Scaffolded<E> {
    crate::scaffolded::Scaffolded::service_failure_naming(
        crate::state::MALFORMED_POLICY_CODE,
        crate::state::MALFORMED_POLICY_STATUS,
        ::service_common::iam_policy::POLICY_INVALID_ACTION_MESSAGE,
        vec![crate::scaffolded::ErrorDetail::new(
            &POLICY_DETAIL_MEMBER,
            action,
        )],
    )
}

/// Whether `action` names an action an access point policy can grant: the policy
/// language's wildcard, or a `<namespace>:<name>` pair whose namespace is one of
/// [`S3_ACTION_NAMESPACES`] and whose name is one the S3 family publishes.
///
/// Both halves are read case-insensitively, because IAM reads an action token that way.
/// A token whose namespace is written in characters a service prefix is not written in
/// never reaches here: the FORM reading beside this one
/// ([`::service_common::iam_policy::readable_action`], applied first by
/// [`access_point_policy_refusal`]) holds a namespace to lowercase letters, digits and
/// hyphens, so such a token is refused for its syntax before any catalogue is consulted.
pub fn grantable_action(action: &str) -> bool {
    if action == ::service_common::iam_policy::ANY_ACTION {
        return true;
    }
    action.split_once(':').is_some_and(|(namespace, name)| {
        S3_ACTION_NAMESPACES
            .iter()
            .any(|published| published.eq_ignore_ascii_case(namespace))
            && names_published_action(name)
    })
}

/// Whether `name` — the half of an action after its namespace — names at least one
/// action of [`PUBLISHED_ACTION_NAMES`].
///
/// Read as the policy language reads an action name: case-insensitively, with its `*` and
/// `?` wildcards standing for any run of characters and for any single one
/// ([`::service_common::iam_authorization::wildcard_match`], the tree's one reading of
/// those wildcards), so `Get*` names the read actions and `*` names them all.
pub fn names_published_action(name: &str) -> bool {
    PUBLISHED_ACTION_NAMES
        .iter()
        .any(|published| ::service_common::iam_authorization::wildcard_match(name, published, false))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A one-statement document whose action element is written as `action`.
    fn policy(action: &str) -> String {
        format!(
            "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"Allow\",\
             \"Principal\":\"*\",\"Action\":{action}}}]}}"
        )
    }

    /// The message and the named value of the refusal `document` earns from the clause
    /// that reads its elements, or `None` when every clause of it reads.
    fn refusal(document: &str) -> Option<(String, Option<String>)> {
        stated(access_point_policy_refusal::<()>(document))
    }

    /// The same, for the clause that reads the document's resources against the access
    /// point it is attached to.
    fn resource_refusal(document: &str) -> Option<(String, Option<String>)> {
        stated(access_point_resource_refusal::<()>(document, ADDRESSED))
    }

    /// The message and the named value a refusal carries.
    fn stated(
        refusal: Option<crate::scaffolded::Scaffolded<()>>,
    ) -> Option<(String, Option<String>)> {
        match refusal {
            None => None,
            Some(crate::scaffolded::Scaffolded::Service(failure)) => Some((
                failure.message.clone(),
                failure.details.first().map(|detail| detail.value.clone()),
            )),
            Some(other) => panic!("expected a service failure, got {other:?}"),
        }
    }

    #[test]
    fn a_document_naming_an_action_no_s3_api_publishes_is_refused_naming_it() {
        // The rows differ along HOW the token fails to name a published action: a name
        // no API publishes, a misspelling of one that is published, a wildcard matching
        // none, a namespace outside the family, and the same in the complement element.
        let rows = [
            ("a name no S3 API publishes", "\"s3:Frobnicate\"", "s3:Frobnicate"),
            ("a misspelled published name", "\"s3:GetObjectt\"", "s3:GetObjectt"),
            ("a wildcard matching no published name", "\"s3:Zz*\"", "s3:Zz*"),
            (
                "a namespace outside the S3 family",
                "\"ec2:RunInstances\"",
                "ec2:RunInstances",
            ),
            (
                "the first unpublished name of a list",
                "[\"s3:GetObject\",\"s3express:Frobnicate\"]",
                "s3express:Frobnicate",
            ),
        ];
        for (case, element, named) in rows {
            let found = refusal(&policy(element));
            assert_eq!(
                found,
                Some((
                    ::service_common::iam_policy::POLICY_INVALID_ACTION_MESSAGE.to_string(),
                    Some(named.to_string())
                )),
                "case {case:?}: expected the invalid-action refusal naming {named:?}, got \
                 {found:?}"
            );
        }
    }

    #[test]
    fn a_document_whose_action_element_cannot_be_read_is_refused_for_its_syntax() {
        // The rows differ along the FORM the element takes: a token with no namespace,
        // a namespace with no name, characters an action name is not written in, a
        // value that is not a string, and an element naming nothing at all.
        for (case, element) in [
            ("no namespace at all", "\"GetObject\""),
            ("a namespace and no name", "\"s3:\""),
            ("a space inside the name", "\"s3:Get Object\""),
            ("a value that is not a string", "5"),
            ("an element naming nothing", "[]"),
        ] {
            let found = refusal(&policy(element));
            assert_eq!(
                found,
                Some((POLICY_SYNTAX_INVALID_MESSAGE.to_string(), None)),
                "case {case:?}: expected the invalid-syntax refusal, got {found:?}"
            );
        }
    }

    #[test]
    fn a_document_naming_something_that_is_no_principal_is_refused_naming_it() {
        // The rows differ along HOW the value fails to name a principal: a bare word
        // where only the wildcard is written without a type, a type the language does
        // not define, an account id that is no account id, an ARN that names no
        // identity, a service principal that is no DNS name, a canonical user of the
        // wrong length, and the same in the complement element. Each row states the
        // document's own MEMBER the refusal quotes: the type key and the value, or the
        // statement element itself where the shorthand names no type.
        let rows = [
            (
                "a bare word for the untyped form",
                "\"everyone\"",
                "\"Principal\" : \"everyone\"",
            ),
            (
                "a type the language does not define",
                "{\"Account\":\"123456789012\"}",
                "\"Account\" : \"123456789012\"",
            ),
            (
                "an account id of the wrong length",
                "{\"AWS\":\"12345\"}",
                "\"AWS\" : \"12345\"",
            ),
            (
                "an ARN that names no identity",
                "{\"AWS\":\"arn:aws:s3:::reports\"}",
                "\"AWS\" : \"arn:aws:s3:::reports\"",
            ),
            (
                "a service principal that is no DNS name",
                "{\"Service\":\"lambda\"}",
                "\"Service\" : \"lambda\"",
            ),
            (
                "a canonical user id of the wrong length",
                "{\"CanonicalUser\":\"abc\"}",
                "\"CanonicalUser\" : \"abc\"",
            ),
            (
                "the first value outside the forms in a list",
                "{\"AWS\":[\"123456789012\",\"nobody\"]}",
                "\"AWS\" : \"nobody\"",
            ),
        ];
        for (case, element, quoted) in rows {
            let document = format!(
                "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"Allow\",\
                 \"Principal\":{element},\"Action\":\"s3:GetObject\"}}]}}"
            );
            let found = refusal(&document);
            assert_eq!(
                found,
                Some((
                    POLICY_INVALID_PRINCIPAL_MESSAGE.to_string(),
                    Some(quoted.to_string())
                )),
                "case {case:?}: expected the invalid-principal refusal quoting {quoted:?}, got \
                 {found:?}"
            );
        }
    }

    #[test]
    fn a_document_naming_principals_the_language_defines_is_attached() {
        // One row per principal type the language keys a statement's principal map by,
        // beside the untyped wildcard and the complement element.
        for (case, element) in [
            ("the untyped wildcard", "\"*\""),
            ("an account by its id", "{\"AWS\":\"123456789012\"}"),
            (
                "an account's root by its ARN",
                "{\"AWS\":\"arn:aws:iam::123456789012:root\"}",
            ),
            (
                "a role by its ARN",
                "{\"AWS\":\"arn:aws:iam::123456789012:role/reader\"}",
            ),
            (
                "a service by its principal name",
                "{\"Service\":\"lambda.amazonaws.com\"}",
            ),
            (
                "an identity provider",
                "{\"Federated\":\"accounts.google.com\"}",
            ),
            (
                "a canonical user",
                "{\"CanonicalUser\":\"79a59df900b949e55d96a1e698fbacedfd6e09d98eacf8f8d5218e7cd47ef2be\"}",
            ),
            ("every principal under the AWS type", "{\"AWS\":\"*\"}"),
        ] {
            let document = format!(
                "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"Allow\",\
                 \"Principal\":{element},\"Action\":\"s3:GetObject\"}}]}}"
            );
            let found = refusal(&document);
            assert_eq!(
                found, None,
                "case {case:?}: expected the document to be attached, got {found:?}"
            );
        }
    }

    /// The access point the documents of these rows are attached to.
    const ADDRESSED: &str = "reports-ap";

    #[test]
    fn a_document_naming_a_resource_the_access_point_does_not_reach_is_refused_naming_it() {
        // The rows differ along HOW the value fails to name the access point addressed:
        // a bare name, a name that merely starts like an ARN, an ARN of another
        // service, an ARN truncated before its resource path, a bucket's own ARN, the
        // objects of a bucket, another access point, the language's bare wildcard, and
        // the first such value of a list.
        let rows = [
            ("a bare resource name", "\"reports\"", "reports"),
            ("a name opening like an ARN", "\"arnold\"", "arnold"),
            (
                "the wildcard, which names no resource",
                "\"*\"",
                "*",
            ),
            (
                "an ARN of another service",
                "\"arn:aws:ec2:us-west-2:123456789012:instance/i-1\"",
                "arn:aws:ec2:us-west-2:123456789012:instance/i-1",
            ),
            (
                "an ARN truncated before its resource path",
                "\"arn:aws:s3:us-west-2:123456789012\"",
                "arn:aws:s3:us-west-2:123456789012",
            ),
            (
                "a bucket's own ARN",
                "\"arn:aws:s3:::reports\"",
                "arn:aws:s3:::reports",
            ),
            (
                "the objects of a bucket",
                "\"arn:aws:s3:::reports/*\"",
                "arn:aws:s3:::reports/*",
            ),
            (
                "another access point",
                "\"arn:aws:s3:us-west-2:123456789012:accesspoint/finance-ap\"",
                "arn:aws:s3:us-west-2:123456789012:accesspoint/finance-ap",
            ),
            (
                "the first value outside the access point in a list",
                "[\"arn:aws:s3:us-west-2:123456789012:accesspoint/reports-ap\",\"everything\"]",
                "everything",
            ),
        ];
        for (case, element, named) in rows {
            let document = format!(
                "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"Allow\",\
                 \"Principal\":\"*\",\"Action\":\"s3:GetObject\",\"Resource\":{element}}}]}}"
            );
            let found = resource_refusal(&document);
            assert_eq!(
                found,
                Some((
                    POLICY_INVALID_RESOURCE_MESSAGE.to_string(),
                    Some(named.to_string())
                )),
                "case {case:?}: expected the invalid-resource refusal naming {named:?}, got \
                 {found:?}"
            );
        }
    }

    #[test]
    fn the_access_point_addressed_and_what_it_reaches_are_the_resources_a_policy_applies_to() {
        // One row per namespace of the family and per form the access point's ARN takes,
        // beside the values that name something else.
        for (case, resource, applies) in [
            (
                "the access point's own ARN",
                "arn:aws:s3:us-west-2:123456789012:accesspoint/reports-ap",
                true,
            ),
            (
                "the objects reached through it",
                "arn:aws:s3:us-west-2:123456789012:accesspoint/reports-ap/object/*",
                true,
            ),
            (
                "its ARN with no region or account",
                "arn:aws:s3:::accesspoint/reports-ap",
                true,
            ),
            (
                "its ARN in another account and Region",
                "arn:aws:s3:eu-west-1:999999999999:accesspoint/reports-ap/object/keys/*",
                true,
            ),
            (
                "an Outposts access point of that name",
                "arn:aws:s3-outposts:us-west-2:123456789012:outpost/op-1/accesspoint/reports-ap",
                true,
            ),
            (
                "an Object Lambda access point of that name",
                "arn:aws:s3-object-lambda:us-west-2:123456789012:accesspoint/reports-ap",
                true,
            ),
            (
                "a name carrying the language's wildcard",
                "arn:aws:s3:us-west-2:123456789012:accesspoint/reports-*/object/*",
                true,
            ),
            ("a value that is no ARN", "reports-ap", false),
            ("the language's bare wildcard", "*", false),
            (
                "an ARN of another service",
                "arn:aws:sqs:us-west-2:123456789012:queue",
                false,
            ),
            ("a bucket's own ARN", "arn:aws:s3:::reports", false),
            ("the objects of a bucket", "arn:aws:s3:::reports/*", false),
            (
                "an Outposts bucket ARN",
                "arn:aws:s3-outposts:us-west-2:123456789012:outpost/op-1/bucket/reports",
                false,
            ),
            (
                "another access point",
                "arn:aws:s3:us-west-2:123456789012:accesspoint/finance-ap",
                false,
            ),
            (
                "the access point type with no access point after it",
                "arn:aws:s3:us-west-2:123456789012:accesspoint",
                false,
            ),
        ] {
            assert_eq!(
                resource_of_access_point(resource, ADDRESSED),
                applies,
                "case {case:?}: expected {resource:?} to be a resource the policy of \
                 {ADDRESSED:?} applies to: {applies}"
            );
        }
    }

    #[test]
    fn the_access_point_a_request_addresses_is_read_from_the_label_it_names_it_in() {
        // Both forms the label documents: the access point's name, and the ARN the
        // Outposts REST API puts there.
        for (case, label) in [
            ("the access point's name", ADDRESSED.to_string()),
            (
                "an Outposts access point ARN",
                format!(
                    "arn:aws:s3-outposts:us-west-2:123456789012:outpost/op-1/accesspoint/{ADDRESSED}"
                ),
            ),
            (
                "an access point ARN",
                format!("arn:aws:s3:us-west-2:123456789012:accesspoint/{ADDRESSED}"),
            ),
        ] {
            assert_eq!(
                addressed_access_point_name(&label),
                ADDRESSED,
                "case {case:?}: expected the label to address {ADDRESSED:?}"
            );
        }
    }

    #[test]
    fn every_published_action_of_every_namespace_of_the_family_is_grantable() {
        for namespace in S3_ACTION_NAMESPACES {
            for published in PUBLISHED_ACTION_NAMES {
                let action = format!("{namespace}:{published}");
                assert!(
                    grantable_action(&action),
                    "case {action:?}: expected a published action to be grantable"
                );
                // IAM reads an action token case-insensitively, so the same token
                // spelled in another case — in either half — names the same action.
                let shouted = format!(
                    "{}:{}",
                    namespace.to_ascii_uppercase(),
                    published.to_ascii_uppercase()
                );
                assert!(
                    grantable_action(&shouted),
                    "case {shouted:?}: expected a published action to be grantable in any case"
                );
            }
        }
    }

    #[test]
    fn the_wildcards_a_name_carries_stand_for_the_published_names_they_match() {
        // One row per wildcard form the language admits, each paired with whether any
        // published name matches it — including a run of wildcards and a name no
        // published one is long enough to match.
        for (case, name, matches) in [
            ("every action", "*", true),
            ("a prefix", "Get*", true),
            ("a suffix", "*Object", true),
            ("a run inside the name", "Put*Policy", true),
            ("one character", "GetObjec?", true),
            ("a run of wildcards", "Get***Object", true),
            ("a wildcard between every character", "G*e*t*O*b*j*e*c*t", true),
            ("a prefix no published name carries", "Zz*", false),
            ("one character too many", "GetObject?", false),
            (
                "a name no published one is long enough to match",
                &"a".repeat(512),
                false,
            ),
        ] {
            assert_eq!(
                names_published_action(name),
                matches,
                "case {case:?}: expected {name:?} to match a published name: {matches}"
            );
        }
    }

    #[test]
    fn the_catalogue_names_each_published_action_once() {
        for (index, published) in PUBLISHED_ACTION_NAMES.iter().enumerate() {
            assert!(
                ::service_common::iam_policy::readable_action(&format!("s3:{published}")),
                "case {published:?}: every catalogued name is one the policy language reads"
            );
            let elsewhere = PUBLISHED_ACTION_NAMES
                .iter()
                .enumerate()
                .any(|(other, name)| other != index && name.eq_ignore_ascii_case(published));
            assert!(
                !elsewhere,
                "case {published:?}: expected the catalogue to name each action once"
            );
        }
    }
}
