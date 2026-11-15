// SPDX-License-Identifier: CC0-1.0

//! The tag keys a CALLER may put on a stack, a change set or a stack set, and the reserved
//! namespace every part of this crate reads tag keys against.
//!
//! `Tag$Key` is "a string used to identify this tag ... Tags owned by Amazon Web Services
//! have the reserved prefix: `aws:`", so a key under that prefix names a tag the SERVICE
//! owns: CloudFormation writes its own `aws:cloudformation:*` keys onto every resource a
//! stack provisions ([`super::provisioning`]), and a request supplying one would be claiming
//! a key the service decides. Every operation that takes a caller's `Tags` is held to the
//! same rule, so a tag one request cannot put on a stack no other request can put on it
//! either.
//!
//! The namespace itself is defined here ([`RESERVED_TAG_KEY_PREFIX`],
//! [`is_reserved_tag_key`]) for the parts of this crate that read tag keys against it: this
//! refusal, and the provisioning arms that keep service-owned keys out of the tag lists
//! another service's API is called with. `super::provisioner` carries a private copy of the
//! same two items that this module cannot take over, because that file is far longer than a
//! file a change may touch; a change that splits it should delegate it to here.

/// The tag-key prefix `Tag$Key` documents as the one the tags Amazon Web Services owns
/// carry, and therefore the one no caller's key may open with.
pub const RESERVED_TAG_KEY_PREFIX: &str = "aws:";

/// Whether `key` is in the reserved [`RESERVED_TAG_KEY_PREFIX`] namespace.
///
/// Matched case-insensitively, as the services that refuse such a key match it: the
/// namespace is AWS's whichever case a request spells it in.
pub fn is_reserved_tag_key(key: &str) -> bool {
    key.get(..RESERVED_TAG_KEY_PREFIX.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(RESERVED_TAG_KEY_PREFIX))
}

/// `Err(message)` for a `Tags` list carrying a key in the reserved namespace.
pub fn validate_tags(tags: &[crate::ops::types::Tag]) -> Result<(), String> {
    if tags.iter().any(|tag| is_reserved_tag_key(&tag.key)) {
        return Err(RESERVED_TAG_KEY.to_string());
    }
    Ok(())
}

/// [`validate_optional_tags`] for the request being SERVED, which is what decides whether a
/// key in the reserved namespace is "for external use" at all.
///
/// A tag under the reserved prefix is a tag the SERVICE that owns the namespace decides, and
/// an AWS service deploying a stack for one of its own resources is that service rather than
/// an external caller: Service Catalog's launch of a provisioned product carries the
/// `aws:servicecatalog:*` tags that record the launch, and nothing a caller sends can.
///
/// Which service is making the call is read from the credential the request PRESENTS — the
/// one thing about a request a client cannot choose for itself — through the shared directory
/// of the calls a service is making as its own service principal
/// ([`::service_common::service_principal_calls`]), and such a call may carry keys in ITS
/// OWN namespace only. Every other request is held to [`validate_optional_tags`]: a caller
/// cannot claim a service's keys by naming them.
pub fn validate_optional_tags_from<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    tags: Option<&[crate::ops::types::Tag]>,
) -> Result<(), String> {
    let reserved: Vec<&str> = tags
        .unwrap_or_default()
        .iter()
        .map(|tag| tag.key.as_str())
        .filter(|key| is_reserved_tag_key(key))
        .collect();
    if reserved.is_empty() {
        return Ok(());
    }
    let calling_service = calling_service(engine, ctx);
    let permitted = |key: &&str| {
        calling_service
            .as_deref()
            .is_some_and(|service_id| service_owns_tag_key(service_id, key))
    };
    match reserved.iter().all(permitted) {
        true => Ok(()),
        false => Err(RESERVED_TAG_KEY.to_string()),
    }
}

/// Whether `key` is a tag key the service whose id is `service_id` owns: the namespace
/// `aws:<service id>:`, which is where the tags a service applies to what it deploys live.
fn service_owns_tag_key(service_id: &str, key: &str) -> bool {
    is_reserved_tag_key(key)
        && key
            .get(RESERVED_TAG_KEY_PREFIX.len()..)
            .and_then(|rest| rest.strip_prefix(service_id))
            .is_some_and(|rest| rest.starts_with(':'))
}

/// The id of the service whose own principal is making this request, for a request another
/// emulated service is making as its service principal on behalf of one of its resources.
///
/// The id is the first label of that principal (`servicecatalog.amazonaws.com` →
/// `servicecatalog`), which is the label its reserved tag namespace carries.
fn calling_service<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
) -> Option<String> {
    let directory = service_principal_calls(engine)?;
    let access_key_id = ::engine::extract_akid(&ctx.parts().headers);
    let account = ::engine::emulator_account_id();
    let now = ::std::time::SystemTime::now();
    let held = directory.lock();
    let call = held.call(&access_key_id, account, now)?;
    let service_id = call.principal.split('.').next().unwrap_or_default();
    (!service_id.is_empty()).then(|| service_id.to_string())
}

/// The shared directory of the calls another service is making as its own service
/// principal, in the one shared engine: written by that service, read here.
fn service_principal_calls<Eng: Send + Sync + 'static>(
    engine: &Eng,
) -> Option<::engine::ServiceState<::service_common::service_principal_calls::ServicePrincipalCalls>>
{
    (engine as &dyn ::std::any::Any)
        .downcast_ref::<::engine::InMemoryEngine>()
        .map(
            ::engine::InMemoryEngine::state::<
                ::service_common::service_principal_calls::ServicePrincipalCalls,
            >,
        )
}

/// [`validate_tags`] for a request whose `Tags` member may be absent, which is every
/// operation that takes one: a request naming no tags states no key to hold to the rule.
pub fn validate_optional_tags(tags: Option<&[crate::ops::types::Tag]>) -> Result<(), String> {
    validate_tags(tags.unwrap_or_default())
}

/// The `ValidationError` message CloudFormation answers a caller-supplied tag key in the
/// reserved namespace.
///
/// ENTIRELY fixed text: the sentence states which keys a caller may not set and names
/// neither the key the request sent nor the operation that refused it — so the one sentence
/// is the answer wherever a `Tags` list reaches this service.
pub const RESERVED_TAG_KEY: &str = "aws: prefixed tag key names are not allowed for external use.";

#[cfg(test)]
mod tests {
    use super::*;

    fn tag(key: &str) -> crate::ops::types::Tag {
        crate::ops::types::Tag {
            key: key.to_string(),
            value: "v".to_string(),
        }
    }

    /// A service may apply the `aws:` keys of ITS OWN namespace and no other's: the rows
    /// differ along which namespace the key is in and how it is cased.
    #[test]
    fn a_calling_service_owns_the_keys_of_its_own_namespace() {
        for (case, service_id, key, owned) in [
            ("its own namespace", "servicecatalog", "aws:servicecatalog:portfolioArn", true),
            ("its own namespace, uppercase prefix", "servicecatalog", "AWS:servicecatalog:productArn", true),
            ("another service's namespace", "servicecatalog", "aws:cloudformation:stack-name", false),
            ("the bare prefix", "servicecatalog", "aws:", false),
            ("a prefix without the separator", "servicecatalog", "aws:servicecatalogue", false),
            ("a key outside the namespace", "servicecatalog", "team", false),
        ] {
            let judged = service_owns_tag_key(service_id, key);
            assert_eq!(
                judged, owned,
                "case {case:?}: expected {owned} for {key:?} as {service_id}, got {judged}"
            );
        }
    }

    #[test]
    fn a_caller_owned_key_is_admitted() {
        for key in ["Name", "aw:s", "team", "a", "AWSTeam", "not-aws:prefixed"] {
            let judged = validate_tags(&[tag(key)]);
            assert!(
                judged.is_ok(),
                "case {key:?}: expected the key to be admitted, got {judged:?}"
            );
        }
    }

    #[test]
    fn a_key_in_the_reserved_namespace_is_refused_in_any_case() {
        for key in [
            "aws:mine",
            "AWS:mine",
            "Aws:cloudformation:stack-name",
            "aws:",
        ] {
            let refused = validate_tags(&[tag("ok"), tag(key)]);
            assert_eq!(
                refused,
                Err(RESERVED_TAG_KEY.to_string()),
                "case {key:?}: expected the reserved-namespace refusal, got {refused:?}"
            );
        }
    }
}
