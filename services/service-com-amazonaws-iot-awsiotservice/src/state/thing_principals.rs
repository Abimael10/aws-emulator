// SPDX-License-Identifier: CC0-1.0

//! WHICH PRINCIPALS are attached to which thing, and what that attachment holds back.
//!
//! `AttachThingPrincipal` "Attaches the specified principal to the specified thing. A
//! principal can be X.509 certificates, Amazon Cognito identities or federated
//! identities", `DetachThingPrincipal` "Detaches the specified principal from the
//! specified thing" and `ListThingPrincipals` "Lists the principals associated with the
//! specified thing" — three views of ONE relation, and that relation is what this module
//! keeps.
//!
//! It lives on the THING's own record ([`super::RegisteredThing::principals`]) rather than
//! in a map beside the registry, for the reason a group's membership lives on the group
//! ([`super::thing_group_membership`]): an attachment is an attachment OF a thing, so it
//! disappears with the thing, and the thing is the record both ends of the relation are
//! reached through.
//!
//! A principal's IDENTITY is the resource it names and not the string that names it, the
//! way a policy target's is ([`super::policy_attachments`]) — a certificate is named by
//! its ARN, an Amazon Cognito identity by its `<region>:<id>` form — so the relation is
//! keyed by the resource each of those names ([`ThingPrincipal::key`]) and the same
//! certificate named by two ARNs is one principal. The two forms are read by the one
//! reader of these addresses this service has ([`super::parse_policy_target`]), less the
//! thing group, which is a target of a POLICY and no principal of a thing. Only a
//! principal that EXISTS is attached: a certificate ARN is resolved against the
//! certificate store, while a Cognito identity is resolved no further than its form,
//! because identity pools live in another service and this one holds no record of them.
//!
//! The relation is also what two DELETES read before they remove anything, because AWS IoT
//! requires detach-before-delete at BOTH ends: a thing that still has a principal attached
//! ([`attached_principals_thing_deletion_message`]), and a certificate that still has a
//! thing attached ("A certificate cannot be deleted if it has a policy or IoT thing
//! attached to it", `DeleteCertificate`, refused by
//! [`super::certificates::attached_things_deletion_message`]). So neither end is ever torn
//! out from under the other.

use crate::ops::types::THING_PRINCIPAL_TYPE_VALUES;

/// How many principals one thing may hold.
///
/// The relation is one a CALLER grows — one pair per `AttachThingPrincipal` — so it is
/// bounded for the reason [`super::MAX_POLICIES_PER_TARGET`] bounds the policy attachments:
/// past the ceiling nothing is recorded and the attach is refused rather than one thing's
/// record growing without bound. The operation models no `LimitExceededException`
/// (`smithy_query(query="operation",operation="AttachThingPrincipal")` lists
/// InternalFailure, InvalidRequest, ResourceNotFound, ServiceUnavailable, Throttling and
/// Unauthorized), so the refusal is the modelled `InvalidRequestException` naming the
/// limit; the figure is this emulator's own capacity.
pub const MAX_PRINCIPALS_PER_THING: usize = 10;

/// The `ThingPrincipalType` an attachment is recorded under when the request named none.
///
/// `ThingPrincipalObject$thingPrincipalType` is "The type of the relation you want to
/// specify when you attach a principal to a thing. The value defaults to
/// `NON_EXCLUSIVE_THING`"
/// (`smithy_query(query="shape",shape="ThingPrincipalObject")`), so every recorded
/// attachment carries one whether or not the writer stated it — the same rule
/// [`super::DEFAULT_DYNAMIC_GROUP_QUERY_VERSION`] states for a dynamic group's query.
pub const DEFAULT_THING_PRINCIPAL_TYPE: &str = "NON_EXCLUSIVE_THING";

/// The resource type segment of a certificate ARN, as [`super::certificate_arn`] mints it.
const CERTIFICATE_RESOURCE_TYPE: &str = "cert";

/// The key prefix a Cognito identity is recorded under, keeping it apart from a
/// certificate of the same name.
const COGNITO_IDENTITY_KEY_PREFIX: &str = "cognito";

/// The resource a thing principal names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThingPrincipal {
    /// `arn:<partition>:iot:<region>:<account>:cert/<certificateId>`: one X.509 device
    /// certificate, carrying its `certificateId`.
    Certificate(String),
    /// `<region>:<id>`: one Amazon Cognito identity, carrying the identity as supplied.
    CognitoIdentity(String),
}

impl ThingPrincipal {
    /// The key the relation records this principal under: the resource it names, so two
    /// ARNs of the same certificate are one principal. The resource type prefixes the key,
    /// so a certificate and a Cognito identity never collide.
    pub fn key(&self) -> String {
        match self {
            Self::Certificate(certificate_id) => {
                format!("{CERTIFICATE_RESOURCE_TYPE}/{certificate_id}")
            }
            Self::CognitoIdentity(identity) => {
                format!("{COGNITO_IDENTITY_KEY_PREFIX}/{identity}")
            }
        }
    }
}

/// One attachment, as the thing's record holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThingPrincipalAttachment {
    /// The principal as every listing of one reports it: the ARN this account holds for the
    /// resource, or the Cognito identity as supplied.
    pub reported: String,
    /// The `ThingPrincipalType` the attachment was recorded under, which an attach naming
    /// none records at [`DEFAULT_THING_PRINCIPAL_TYPE`].
    pub principal_type: String,
}

/// A thing principal resolved against this account's state: everything an attachment or a
/// listing needs about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedThingPrincipal {
    /// Which resource it names.
    pub principal: ThingPrincipal,
    /// The key the relation records it under ([`ThingPrincipal::key`]).
    pub key: String,
    /// The principal as every listing reports it.
    pub reported: String,
}

/// Why the relation will not take — or will not report — a thing principal.
///
/// Each variant names the modelled error class the refusal answers with, so every operation
/// that reads or writes the relation refuses one principal for one reason, in one wording,
/// with one status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThingPrincipalRefusal {
    /// The request named a thing the registry does not hold, carrying the name:
    /// `ResourceNotFoundException` (HTTP 404).
    MissingThing(String),
    /// The value is none of the forms a thing principal takes: `InvalidRequestException`
    /// (HTTP 400).
    Invalid(String),
    /// A well-formed principal naming a resource this account does not hold:
    /// `ResourceNotFoundException` (HTTP 404).
    NotFound(String),
    /// The thing already holds [`MAX_PRINCIPALS_PER_THING`] principals, carrying the
    /// thing's name: `InvalidRequestException` (HTTP 400).
    TooManyPrincipals(String),
}

impl ThingPrincipalRefusal {
    /// What the refusal reports, in the wording this service states each of these in.
    pub fn message(&self) -> String {
        match self {
            Self::MissingThing(thing_name) => super::not_found::missing_thing_message(thing_name),
            Self::Invalid(message) | Self::NotFound(message) => message.clone(),
            Self::TooManyPrincipals(thing_name) => format!(
                "Thing {thing_name} cannot have more than {MAX_PRINCIPALS_PER_THING} \
                 attached principals"
            ),
        }
    }
}

/// How this service states that a value is none of the forms a thing principal takes.
///
/// `AttachThingPrincipal` documents them — "A principal can be X.509 certificates, Amazon
/// Cognito identities or federated identities" — and they are named in the forms a policy
/// target is named in ([`super::parse_policy_target`]) less the thing group. AWS publishes
/// no sentence for this refusal and no recorded call names one, so the fixed text is this
/// emulator's rendering of that documented rule; the variable part is the value the request
/// sent.
pub fn unsupported_thing_principal_message(principal: &str) -> String {
    format!(
        "Invalid principal: {principal}. A thing principal is a certificate ARN or an \
         Amazon Cognito identity of the form region:id"
    )
}

/// The message a delete of a THING that still has a principal attached is refused with.
///
/// AWS IoT makes detach-before-delete mandatory, and words this refusal after the thing the
/// request addressed: the fixed text is the class's and the only variable part is the
/// `thingName` the request carried. How MANY principals are attached is not part of it —
/// the sentence says "one or more principals" whether the thing holds one or ten.
pub fn attached_principals_thing_deletion_message(thing_name: &str) -> String {
    format!("Cannot delete. Thing {thing_name} is still attached to one or more principals")
}

impl super::ComAmazonawsIotAwsiotserviceState {
    /// Resolve `principal` against this account's state: the resource it names, the key the
    /// relation records it under and the form every listing reports it in — or the reason it
    /// is no principal of a thing of this account.
    pub fn resolve_thing_principal(
        &self,
        principal: &str,
    ) -> Result<ResolvedThingPrincipal, ThingPrincipalRefusal> {
        let named = match super::parse_policy_target(principal) {
            Ok(super::PolicyTarget::Certificate(certificate_id)) => {
                ThingPrincipal::Certificate(certificate_id)
            }
            Ok(super::PolicyTarget::CognitoIdentity(identity)) => {
                ThingPrincipal::CognitoIdentity(identity)
            }
            // A thing GROUP is a target a policy attaches to and no principal a thing is
            // reached through, so its ARN is refused here exactly as an unreadable value is.
            Ok(super::PolicyTarget::ThingGroup(_)) | Err(_) => {
                return Err(ThingPrincipalRefusal::Invalid(
                    unsupported_thing_principal_message(principal),
                ))
            }
        };
        let reported = match &named {
            ThingPrincipal::Certificate(certificate_id) => {
                let Some(certificate) = self.certificates.get(certificate_id) else {
                    return Err(ThingPrincipalRefusal::NotFound(
                        super::missing_certificate_target_message(),
                    ));
                };
                certificate.certificate_arn.clone()
            }
            ThingPrincipal::CognitoIdentity(identity) => identity.clone(),
        };
        Ok(ResolvedThingPrincipal {
            key: named.key(),
            principal: named,
            reported,
        })
    }

    /// Why the relation would refuse attaching `principal` to the thing named `thing_name`,
    /// or `None` when it would take it.
    ///
    /// READ-ONLY, and the one place the ceiling is read: [`Self::record_thing_principal`]
    /// answers this before it writes, so a caller that has to know whether the attachment is
    /// admissible BEFORE making other writes — the provisioning path, which registers the
    /// thing and the attachment in one call ([`super::thing_provisioning`]) — asks the same
    /// question and cannot drift from what the write then does.
    ///
    /// A pair already recorded is admissible and no nearer the ceiling, because recording it
    /// again adds nothing. Whether the thing EXISTS is not read here: that is the write's own
    /// check, so a caller about to register the thing can ask this first.
    pub fn thing_principal_refusal_for(
        &self,
        thing_name: &str,
        principal: &ResolvedThingPrincipal,
    ) -> Option<ThingPrincipalRefusal> {
        let thing = self.things.get(thing_name)?;
        if thing.principals.contains_key(&principal.key) {
            return None;
        }
        (thing.principals.len() >= MAX_PRINCIPALS_PER_THING)
            .then(|| ThingPrincipalRefusal::TooManyPrincipals(thing_name.to_string()))
    }

    /// Record that the already resolved `principal` is attached to the thing `thing_name`:
    /// the ONE relation write, so every writer of it — the attach, and the provisioning path
    /// that provisions a device with its certificate — agrees on what it takes.
    ///
    /// IDEMPOTENT: the pair is keyed by the resource the principal names, so a repeated
    /// attach leaves the same one attachment and is no nearer the ceiling. The type the
    /// request named is recorded on it, and an attach naming none records
    /// [`DEFAULT_THING_PRINCIPAL_TYPE`].
    pub fn record_thing_principal(
        &mut self,
        thing_name: &str,
        principal: &ResolvedThingPrincipal,
        thing_principal_type: Option<&str>,
    ) -> Result<(), ThingPrincipalRefusal> {
        if !self.things.contains_key(thing_name) {
            return Err(ThingPrincipalRefusal::MissingThing(thing_name.to_string()));
        }
        if let Some(refusal) = self.thing_principal_refusal_for(thing_name, principal) {
            return Err(refusal);
        }
        // A value outside the modelled enum is refused before any handler runs, so this
        // keeps the record inside `ThingPrincipalType` without depending on that.
        let principal_type = thing_principal_type
            .filter(|named| THING_PRINCIPAL_TYPE_VALUES.contains(named))
            .unwrap_or(DEFAULT_THING_PRINCIPAL_TYPE)
            .to_string();
        if let Some(thing) = self.things.get_mut(thing_name) {
            thing.principals.insert(
                principal.key.clone(),
                ThingPrincipalAttachment {
                    reported: principal.reported.clone(),
                    principal_type,
                },
            );
        }
        Ok(())
    }

    /// Attach `principal` to the thing `thing_name`, answering the principal as the relation
    /// recorded it.
    ///
    /// Both ends have to EXIST — an attachment between records this account does not hold
    /// would be a relation over nothing — which is why the thing is resolved before the
    /// principal is: the request addresses the THING, so a request naming neither is told
    /// about the resource it addressed.
    pub fn attach_thing_principal(
        &mut self,
        thing_name: &str,
        principal: &str,
        thing_principal_type: Option<&str>,
    ) -> Result<ResolvedThingPrincipal, ThingPrincipalRefusal> {
        if !self.things.contains_key(thing_name) {
            return Err(ThingPrincipalRefusal::MissingThing(thing_name.to_string()));
        }
        let resolved = self.resolve_thing_principal(principal)?;
        self.record_thing_principal(thing_name, &resolved, thing_principal_type)?;
        Ok(resolved)
    }

    /// Detach `principal` from the thing `thing_name`, answering whether an attachment was
    /// there to remove: the inverse of [`Self::attach_thing_principal`], under the same rule
    /// that both ends exist.
    ///
    /// A principal that is not attached removes nothing and succeeds: the detach names no
    /// identity, so the relation holds the same attachments afterwards however often it is
    /// called.
    pub fn detach_thing_principal(
        &mut self,
        thing_name: &str,
        principal: &str,
    ) -> Result<bool, ThingPrincipalRefusal> {
        if !self.things.contains_key(thing_name) {
            return Err(ThingPrincipalRefusal::MissingThing(thing_name.to_string()));
        }
        let resolved = self.resolve_thing_principal(principal)?;
        Ok(self
            .things
            .get_mut(thing_name)
            .is_some_and(|thing| thing.principals.remove(&resolved.key).is_some()))
    }

    /// The principals attached to the thing `thing_name`, as a listing of them reports them,
    /// in ASCENDING order of the reported value — the total order that makes a page boundary
    /// over them meaningful, since a page marker names a position in it.
    pub fn thing_principals(&self, thing_name: &str) -> Vec<String> {
        let mut reported: Vec<String> = self
            .things
            .get(thing_name)
            .map(|thing| {
                thing
                    .principals
                    .values()
                    .map(|attachment| attachment.reported.clone())
                    .collect()
            })
            .unwrap_or_default();
        reported.sort();
        reported
    }

    /// The names of the things the principal recorded under `key` is attached to, in
    /// registry order.
    ///
    /// Derived from the things' own records, so there is one record of the relation and not a
    /// second copy keyed by the principal — which is what lets a delete of the PRINCIPAL ask
    /// whether a thing still holds it.
    pub fn things_attached_to_principal(&self, key: &str) -> Vec<String> {
        self.things
            .values()
            .filter(|thing| thing.principals.contains_key(key))
            .map(|thing| thing.thing_name.clone())
            .collect()
    }
}
