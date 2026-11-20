// SPDX-License-Identifier: CC0-1.0

//! The POLICY DOCUMENT of the resource-policy surface: what a submitted document has to be
//! for the surface to attach it, the form the surface reports a stored one in, and what a
//! resource carrying none reads as.
//!
//! The ARN half of the surface's rules lives beside the service's other identity readings
//! ([`super::resource_policy_arn_refusal`]); this module is about the DOCUMENT, which is
//! judged only once the ARN names a resource this service holds:
//!
//! * the text has to be a resource-based policy document in the IAM JSON policy language —
//!   the reading every service that attaches one applies
//!   ([`::service_common::iam_policy::attached_policy_defect`]) plus the grammar of a
//!   resource-based document ([`::service_common::iam_policy::resource_policy_defect`]) — so
//!   an unparseable text, a document carrying a member name the language does not define or
//!   no version string it defines, and one that declares no statement are all refused on the
//!   document's FORMAT ([`ResourcePolicyDocumentDefect::Format`]);
//! * a statement's resource element names the resource the policy is being attached TO and
//!   nothing else, a statement's action element names an action this surface can SHARE
//!   ([`SHAREABLE_VPC_ORIGIN_ACTIONS`]), and no statement makes its grant conditional
//!   ([`CONDITION_ELEMENT`]); a document that breaks any of those describes a grant this
//!   surface would never enforce, and is refused as an unusable POLICY
//!   ([`ResourcePolicyDocumentDefect::Policy`]);
//! * every identity the document grants to has to EXIST, because a share with an identity
//!   nothing holds grants nothing: each principal a statement names is resolved against the
//!   one account this emulator serves and that account's IAM
//!   ([`resource_policy_principal_is_absent`]), and a document naming one that does not
//!   resolve is refused on its PRINCIPALS
//!   ([`RESOURCE_POLICY_PRINCIPAL_ABSENT_MESSAGE`]);
//! * what is stored is the PARSED document rendered in the one canonical layout
//!   ([`canonical_resource_policy`]), so a read reports the service's own form of the
//!   document rather than the caller's bytes;
//! * a resource that carries no policy reads as the empty document
//!   ([`EMPTY_RESOURCE_POLICY_DOCUMENT`]) rather than as an absent entity: the policy is a
//!   property OF the resource, so there is nothing missing to report once the resource is
//!   there.

/// What a resource the surface accepts, carrying no policy, reports as its policy document.
///
/// A resource takes a policy, so what a read answers for one nothing has been attached to is
/// the document that grants nothing — an empty JSON object — and not an absent-entity
/// refusal: the resource itself is there, and so is its (empty) policy. That is also what a
/// read answers after the policy is removed, which is what makes the removal idempotent for
/// a client that reads the policy back.
pub const EMPTY_RESOURCE_POLICY_DOCUMENT: &str = "{}";

/// The actions a resource policy on a VPC origin may grant.
///
/// The surface SHARES one named resource with another account, so what a statement may name
/// is an action that READS the very VPC origin the policy is attached to: of the five
/// VPC-origin operations the model defines, `GetVpcOrigin` is the one that addresses a SINGLE
/// origin by its id and is served with an HTTP `GET` (`/2020-05-31/vpc-origin/{Id}`).
/// `CreateVpcOrigin`, `UpdateVpcOrigin` and `DeleteVpcOrigin` change the resource (`POST`,
/// `PUT` and `DELETE`), and `ListVpcOrigins` reads the account's collection rather than one
/// origin (`GET /2020-05-31/vpc-origin`, which takes no id at all) — so neither family names
/// something a policy attached to ONE origin could grant
/// (`smithy_query(query="operation",
/// operation="GetVpcOrigin,ListVpcOrigins,CreateVpcOrigin,UpdateVpcOrigin,DeleteVpcOrigin")`).
pub const SHAREABLE_VPC_ORIGIN_ACTIONS: [&str; 1] = ["cloudfront:GetVpcOrigin"];

/// Whether `action` names an action a resource policy on a VPC origin may grant: one of
/// [`SHAREABLE_VPC_ORIGIN_ACTIONS`], spelled in any case, since IAM reads an action name
/// case-insensitively.
///
/// A WILDCARD is not one of them, whether it stands for every action or for every action of
/// this service's namespace: it names more than the actions this surface shares — the
/// mutating VPC-origin actions among them — so a statement carrying one is not a share of
/// the resource.
pub fn shareable_vpc_origin_action(action: &str) -> bool {
    SHAREABLE_VPC_ORIGIN_ACTIONS
        .iter()
        .any(|shareable| shareable.eq_ignore_ascii_case(action))
}

/// The statement element that makes a grant CONDITIONAL, which this surface's share does not
/// take: the policy language's condition block, named as the language names it.
pub const CONDITION_ELEMENT: &str = "Condition";

/// Why the resource-policy surface will not attach a submitted `PolicyDocument`.
///
/// Two clauses, because the surface words two sentences: one about the document's FORMAT —
/// the text is not a policy at all — and one about what the policy SAYS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourcePolicyDocumentDefect {
    /// The text is not a resource-based policy document: it is not parseable JSON, or it is
    /// JSON that declares no usable statement.
    Format,
    /// The text is a policy document, and what it declares is not a plain share of the
    /// resource it is being attached to: a statement names another resource or none, an
    /// action this surface does not share ([`SHAREABLE_VPC_ORIGIN_ACTIONS`]), or a condition
    /// on the grant ([`CONDITION_ELEMENT`]).
    Policy,
}

impl ResourcePolicyDocumentDefect {
    /// The sentence CloudFront answers this clause with. Both are fixed text in full:
    /// neither names the ARN, the action or any other part of the request. Recorded
    /// behaviour.
    pub const fn message(self) -> &'static str {
        match self {
            Self::Format => INVALID_POLICY_FORMAT_MESSAGE,
            Self::Policy => INVALID_POLICY_MESSAGE,
        }
    }
}

/// The sentence a `PolicyDocument` that is not a policy document at all is refused with:
/// what the caller has to fix is the FORMAT of the text it submitted. Fixed text in full.
pub const INVALID_POLICY_FORMAT_MESSAGE: &str =
    "The specified policy format isn't valid. Verify that the format is valid JSON and try again.";

/// The sentence a document whose statements do not describe a share of the resource they are
/// being attached to is refused with. Fixed text in full — CloudFront's own wording,
/// including the spelling `ResourceARN` inside the sentence and the absence of a closing
/// period.
pub const INVALID_POLICY_MESSAGE: &str = "The specified policy isn't valid (InvalidPolicy). \
     Verify that it's both valid JSON, and that the ARN specified in the policy is the same \
     ARN in the ResourceARN parameter";

/// Why the surface will not attach `document` to the resource `resource_arn` names, or
/// `None` for a document it stores.
///
/// Read in the order the service decides them, and entirely from the request: the document
/// is a policy at all, then what that policy says about THIS resource.
///
/// * the FORMAT clause is the reading every service that ATTACHES a policy document applies,
///   stated once for the workspace: the text is a JSON object whose member names the policy
///   language defines, carrying a statement element and a version string the language defines
///   ([`::service_common::iam_policy::attached_policy_defect`]), and every statement is a
///   RESOURCE-policy statement — an effect, an action and, this being the element that says
///   who the resource is shared with, a principal
///   ([`::service_common::iam_policy::resource_policy_defect`]). A text that is not JSON, one
///   that is JSON but no policy, and one declaring an empty statement list are the same
///   answer;
/// * the POLICY clause is this surface's own, and is what makes a document a SHARE of one
///   named resource with one named principal rather than a policy in general:
///   * the document NAMES the resource it is being attached to and names no other — every
///     value a statement's resource element names is the request's `ResourceArn`, and at
///     least one of them does
///     ([`::service_common::iam_policy::resource_element_values`]), which is what the
///     sentence for this clause demands ("the ARN specified in the policy is the same ARN in
///     the ResourceARN parameter"): a policy attached under one resource while speaking about
///     another — or about no resource at all — scopes its grant to something other than the
///     resource it is attached to;
///   * every value its action element names is one this surface can share
///     ([`shareable_vpc_origin_action`]);
///   * no statement makes its grant CONDITIONAL
///     ([`::service_common::iam_policy::statements_carry_element`] over
///     [`CONDITION_ELEMENT`]): the grant this surface publishes is the plain share of the
///     named resource with the named principal, so a condition block states a restriction
///     the share would have to enforce and does not. Recorded behaviour.
///
/// Nothing about the store is read, so the answer is the same whatever this service holds,
/// and the caller applies it before it writes anything.
pub fn resource_policy_document_defect(
    resource_arn: &str,
    document: &str,
) -> Option<ResourcePolicyDocumentDefect> {
    // The action clause of the attaching validator is this surface's own catalogue below, so
    // the shared reading is asked for the document's SHAPE alone.
    if ::service_common::iam_policy::attached_policy_defect(document, |_| true).is_some()
        || ::service_common::iam_policy::resource_policy_defect(document).is_some()
    {
        return Some(ResourcePolicyDocumentDefect::Format);
    }
    let resources = ::service_common::iam_policy::resource_element_values(document);
    let names_the_resource =
        !resources.is_empty() && resources.iter().all(|value| value == resource_arn);
    let shares_another_action = ::service_common::iam_policy::action_values(document)
        .into_iter()
        .any(|value| !shareable_vpc_origin_action(&value));
    let conditions_the_share =
        ::service_common::iam_policy::statements_carry_element(document, CONDITION_ELEMENT);
    (!names_the_resource || shares_another_action || conditions_the_share)
        .then_some(ResourcePolicyDocumentDefect::Policy)
}

/// The modelled `InvalidArgument` (HTTP 400) a submitted `PolicyDocument` the surface will
/// not attach is refused with — "An argument is invalid." — worded with the sentence its
/// clause states ([`ResourcePolicyDocumentDefect::message`]).
pub fn invalid_resource_policy_document(
    defect: ResourcePolicyDocumentDefect,
) -> crate::ops::types::InvalidArgument {
    crate::ops::types::InvalidArgument {
        message: Some(defect.message().to_string()),
    }
}

/// The sentence a `PolicyDocument` granting to an identity that does not exist is refused
/// with: what the caller has to fix is the PRINCIPALS of the policy it submitted. Fixed text
/// in full — it names neither the principal it could not resolve nor the resource.
pub const RESOURCE_POLICY_PRINCIPAL_ABSENT_MESSAGE: &str =
    "The specified policy references a principal that doesn't exist. Verify the principals in \
     the policy and try again.";

/// The modelled `InvalidArgument` (HTTP 400) a submitted `PolicyDocument` that grants to an
/// identity this account does not hold is refused with — "An argument is invalid." — worded
/// with the sentence that condition carries ([`RESOURCE_POLICY_PRINCIPAL_ABSENT_MESSAGE`]).
pub fn invalid_resource_policy_principal() -> crate::ops::types::InvalidArgument {
    crate::ops::types::InvalidArgument {
        message: Some(RESOURCE_POLICY_PRINCIPAL_ABSENT_MESSAGE.to_string()),
    }
}

/// Whether one of the identities the statements of `document` grant to does not exist, so the
/// surface will not attach it.
///
/// A resource policy states WHO the resource is shared with, and a share with an identity
/// that does not exist grants nothing, so each identity a statement names
/// ([`::service_common::iam_policy_principals::policy_principals`]) is RESOLVED before the
/// document is stored:
///
/// * an identity of ANOTHER ACCOUNT — a whole account by its id or by that account's root
///   ARN, and any principal ARN whose account field is not this one's
///   ([`names_another_account`]) — exists nowhere this emulator can see, since it serves the
///   one account ([`::engine::emulator_account_id`]);
/// * an IAM role or user OF THIS ACCOUNT is resolved against the account's own IAM
///   ([`::service_common::iam_principal::identity_is_absent`]), the tree's one reading of a
///   policy principal, so a refusal is grounded in what IAM answered;
/// * every other form — the `*` wildcard, a service principal, a federated or canonical-user
///   principal, this account's own root — names no identity for either reading to resolve and
///   refuses nothing.
///
/// Read from the request and the account alone, so the answer does not depend on which
/// resource the policy is being attached to, and the caller applies it before it writes. The
/// reads it makes of IAM are bounded: each principal is resolved ONCE however many statements
/// name it, and at most [`MAX_RESOURCE_POLICY_PRINCIPALS_RESOLVED`] of them are put to IAM at
/// all.
pub async fn resource_policy_principal_is_absent(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    document: &str,
) -> bool {
    let mut resolved: Vec<String> = Vec::new();
    for principal in ::service_common::iam_policy_principals::policy_principals(document) {
        if names_another_account(&principal) {
            return true;
        }
        if resolved.contains(&principal) {
            continue;
        }
        if resolved.len() >= MAX_RESOURCE_POLICY_PRINCIPALS_RESOLVED {
            break;
        }
        resolved.push(principal.clone());
        // CloudFront is a global service, so the account's IAM is read where the partition
        // publishes the account's global records.
        if ::service_common::iam_principal::identity_is_absent(
            ctx,
            ::service_common::management_events::PARTITION_LEADER_REGION,
            &principal,
        )
        .await
        {
            return true;
        }
    }
    false
}

/// How many of a submitted document's principals ONE write may put to the account's IAM.
///
/// The account clause above is computed from the request alone, but resolving a role or a user
/// is a READ of another service, so the number of them one request can cause is bounded here
/// rather than by the size of the document — the same bound, for the same reason, that this
/// service puts on the Lambda and IAM reads one stored distribution configuration can cause
/// ([`super::MAX_EDGE_ROLES_RESOLVED_PER_REQUEST`]). It is this emulator's own ceiling, not a
/// published quota, and it is a ceiling on the READS: a document naming more identities than
/// this is stored rather than refused, since a refusal has to be grounded in what IAM
/// answered.
pub const MAX_RESOURCE_POLICY_PRINCIPALS_RESOLVED: usize = 64;

/// Whether `principal` names an account other than the one this emulator serves: the account
/// itself, written as a bare account id or as any ARN whose account field carries one.
///
/// This emulator serves a single account, so an identity under another account id — a whole
/// account, its root, or one of its roles, users or sessions — is an identity nothing holds.
///
/// The ARN and account-id readings it is built from are the tree's shared ones
/// ([`::service_common::arn`]); what is this SURFACE's own is the conclusion it draws from
/// them. The shared resolution beside it
/// ([`::service_common::iam_principal::identity_is_absent`]) deliberately refuses nothing for
/// an account principal, because whether a foreign account is a grantee a service will store is
/// that service's recorded behaviour rather than a property of the policy language.
fn names_another_account(principal: &str) -> bool {
    let this_account = ::engine::emulator_account_id();
    if ::service_common::arn::is_account_id(principal) {
        return principal != this_account;
    }
    ::service_common::arn::parse(principal).is_some_and(|arn| {
        ::service_common::arn::is_account_id(arn.account) && arn.account != this_account
    })
}

/// The form the surface STORES and reports a policy document in: the parsed document
/// rendered in the one canonical layout an attached IAM policy document is reported in
/// ([`::service_common::iam_policy::canonical_document`]).
///
/// The service parses the submitted document and re-renders it, so what a read reports is
/// the service's own form of the policy rather than the caller's bytes — a one-value list
/// collapses to the value alone (`"Action": ["cloudfront:GetVpcOrigin"]` reads back
/// `"Action": "cloudfront:GetVpcOrigin"`), while every token the caller wrote survives. The
/// rendering is a fixed point, so a client that puts back what it read sees no drift.
pub fn canonical_resource_policy(document: &str) -> String {
    ::service_common::iam_policy::canonical_document(document)
}
