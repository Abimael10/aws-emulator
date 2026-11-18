// SPDX-License-Identifier: CC0-1.0

//! The RESOURCE-BASED policy in force on the resource a `Task` state's integration call
//! acts on.
//!
//! A call an IAM principal makes on a resource of its own account is allowed when EITHER
//! kind of document allows it: "if an identity-based policy or a resource-based policy
//! grants the request, then AWS allows the request" — the two are evaluated together, so
//! a role carrying no identity-based grant at all may still make a call the TARGET's own
//! policy grants it. That is why the decision about a state machine's execution role
//! ([`super::task_authorization`]) reads this beside the role's own documents: a refusal
//! taken on the identity documents alone refuses calls the invoked service itself would
//! have performed.
//!
//! The document is read out of the OWNING service's store, through that service's own
//! accessor, so this service judges the call against the very statements that service's
//! handlers wrote rather than against a second copy of them. A target whose owning store
//! this emulator cannot read, and a resource that holds no such policy, answer `None`:
//! nothing is refused on evidence this emulator does not have, and nothing is granted on
//! a document it has not read.
//!
//! A pure READ: it takes the owning store's guard, copies the document out and releases
//! it, so nothing a later caller sees is changed and no second guard is held while this
//! one is.
//!
//! WHICH target is resolved is decided by the ARN's own vendor field — the service that
//! OWNS the resource and therefore holds its policy — not by the integration that named
//! the call, because the same resource is reached through several of them. The store this
//! crate can read is the one of a service it depends on: a Lambda FUNCTION today, whose
//! policy decides the invocation every Lambda integration makes. A target whose owning
//! store this crate does not reach answers `None` here and is decided on the role's
//! documents alone, and a customer managed KMS key the target seals its data under is a
//! decision of its own ([`super::encryption`]) rather than one of these documents.

use ::service_common::arn;

/// The ARN namespace of the service that owns a Lambda FUNCTION, and the resource-type
/// prefix a function's ARN carries after it
/// (`arn:aws:lambda:<region>:<account>:function:<name>[:<qualifier>]`).
const LAMBDA_NAMESPACE: &str = "lambda";
/// See [`LAMBDA_NAMESPACE`].
const FUNCTION_RESOURCE_PREFIX: &str = "function:";

/// The resource-based policy document in force on the resource `resource_arn` names, or
/// `None` when this emulator holds no such document to read (see the module docs).
///
/// `resource_arn` is the ARN the access request is over
/// ([`super::integration::access_request`]), which is the resource the invoked
/// operation's own request named — for a Lambda invocation the function AT the qualifier
/// the call addressed, which is the key a grant was stored under.
#[must_use]
pub fn in_force<Eng: Send + Sync + 'static>(engine: &Eng, resource_arn: &str) -> Option<String> {
    let parsed = arn::parse(resource_arn)?;
    match parsed.vendor {
        // A function's policy is the one `lambda:AddPermission` and
        // `lambda:PutResourcePolicy` wrote, read through Lambda's own accessor onto it.
        LAMBDA_NAMESPACE if parsed.resource.starts_with(FUNCTION_RESOURCE_PREFIX) => {
            ::service_com_amazonaws_lambda_awsgirapiservice::state::function_resource_policy(
                engine,
                resource_arn,
            )
        }
        _ => None,
    }
}
