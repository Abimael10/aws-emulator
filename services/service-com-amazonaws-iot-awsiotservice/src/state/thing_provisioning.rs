// SPDX-License-Identifier: CC0-1.0

//! SINGLE-DEVICE PROVISIONING: what one `RegisterThing` call makes of the provisioning
//! template body it carries.
//!
//! `RegisterThing` "provisions a thing in the device registry" from a provisioning
//! template supplied INLINE, with the call's `parameters` substituted into it — the same
//! template a bulk thing-registration task runs one line of the input file at a time
//! ([`super::thing_registration`]). So the two read ONE template: the body is read through
//! [`ProvisioningTemplate`], the thing it provisions is the thing that reader describes,
//! and the registry write is the one [`ComAmazonawsIotAwsiotserviceState::register_thing`]
//! every writer of a thing makes.
//!
//! What a template DECLARES, though, is more than the thing: its `Resources` section names
//! each resource under a LOGICAL ID, which is what `RegisterThing$resourceArns` is keyed by
//! (`ResourceArns` is `map<ResourceLogicalId, ResourceArn>`), and a fleet provisioning
//! template declares the device's CERTIFICATE and its POLICY beside the thing. This module
//! reads those declarations and provisions each one against the registry that OWNS it — the
//! thing registry, the certificate store and the policy registry this service already keeps
//! — so every ARN the response reports addresses a resource a later read of that registry
//! returns.
//!
//! Two different faults refuse a call, and they are kept apart because they are two
//! different answers ([`ProvisioningRefusal`]): a BODY that is no provisioning template
//! this service can carry out — one that is not a template at all, or a resource
//! declaration that names nothing to provision — is a fact about the request, while a
//! declaration it can carry out that the account's STATE will not take is a registration
//! that failed.
//!
//! A refused call leaves the store as it found it: everything every declaration needs — the
//! record a certificate names, the status it asks for, the document a policy is created
//! from, and whether the attachment relation will take that policy — is resolved read-only
//! BEFORE the first write, so the only write that can still refuse is the thing's own, and
//! it is made first.

use std::collections::{BTreeMap, HashMap};
use std::time::SystemTime;

use ::serde_json::{Map, Value};

use super::thing_registration::{render_text, ProvisioningTemplate};
use super::ComAmazonawsIotAwsiotserviceState;
use crate::ops::types::CERTIFICATE_STATUS_VALUES;

/// The section of a template body that declares what provisioning creates.
const RESOURCES_MEMBER: &str = "Resources";

/// The members one declared resource carries: which kind of resource it is, and the
/// properties that describe it.
const TYPE_MEMBER: &str = "Type";
const PROPERTIES_MEMBER: &str = "Properties";

/// The resource type that declares the THING a template provisions — the one
/// [`super::thing_registration`] reads for a bulk task, named here so both paths agree on
/// which declaration describes a thing.
const THING_RESOURCE_TYPE: &str = "AWS::IoT::Thing";

/// The resource type that declares the device's X.509 CERTIFICATE.
const CERTIFICATE_RESOURCE_TYPE: &str = "AWS::IoT::Certificate";

/// The resource type that declares the IoT POLICY the device is provisioned with.
const POLICY_RESOURCE_TYPE: &str = "AWS::IoT::Policy";

/// The properties a certificate declaration names the certificate by, and the status it
/// asks for it.
const CERTIFICATE_ID_PROPERTY: &str = "CertificateId";
const CERTIFICATE_PEM_PROPERTY: &str = "CertificatePem";
const CERTIFICATE_STATUS_PROPERTY: &str = "Status";

/// The properties a policy declaration names the policy by, and the document it is created
/// with when the account holds none under that name.
const POLICY_NAME_PROPERTY: &str = "PolicyName";
const POLICY_DOCUMENT_PROPERTY: &str = "PolicyDocument";

/// The clause every refusal of a template BODY opens with, and the family every such
/// refusal's detail hangs off: AWS IoT refuses a `RegisterThing` whose `templateBody` it
/// cannot read with "Invalid registration template. Template format error: <detail>", so a
/// caller reads one wording whichever part of the body is at fault.
pub fn template_format_error_message(detail: &str) -> String {
    format!("Invalid registration template. Template format error: {detail}")
}

/// The detail a body that is not JSON at all carries: the LINE and COLUMN the JSON parser
/// stopped at, so the caller is told where in its own body to look.
pub fn json_syntax_detail(line: usize, column: usize) -> String {
    format!("JSON syntax error at line {line}, column {column}")
}

/// The detail a body that parses as JSON but is no JSON object carries: a provisioning
/// template is an object of sections, so nothing else can be read as one.
pub const NOT_AN_OBJECT_DETAIL: &str = "the provisioning template is not a JSON object";

/// The detail a body declaring no `Resources` carries: that section is where a template
/// says what to provision, so a template without it provisions nothing.
pub const NO_RESOURCES_DETAIL: &str = "no Resources member is defined";

/// The detail a body whose `Resources` declare no thing carries: `RegisterThing`
/// "provisions a thing in the device registry", so a template describing no thing has
/// nothing for it to provision.
pub const NO_THING_RESOURCE_DETAIL: &str = "no AWS::IoT::Thing resource is defined";

/// Why a `RegisterThing` provisioned nothing.
///
/// The two cases are two different faults, and a writer maps each onto the error ITS model
/// declares: a body it cannot carry out is a fact about the REQUEST, while a template it
/// can carry out that the account's state will not take is a registration that failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProvisioningRefusal {
    /// `templateBody`, with the parameters supplied, is no provisioning template this
    /// service can carry out.
    Template(String),
    /// It is one it can carry out, but the account's state would not take a resource it
    /// declares.
    Resource(String),
}

impl ProvisioningRefusal {
    /// What the refusal reports: the rule the request broke.
    pub fn message(&self) -> String {
        match self {
            Self::Template(reason) | Self::Resource(reason) => reason.clone(),
        }
    }
}

/// How this service refuses a `templateBody` it cannot read as a provisioning template at
/// all: the parser's own line and column for a body that is not JSON, and the rule it
/// broke for one that is JSON but no template.
///
/// Reading the body again is what tells the two apart, and it happens only on the refusal
/// path.
fn unreadable_template_message(body: &str) -> String {
    match ::serde_json::from_str::<Value>(body) {
        Err(error) => {
            template_format_error_message(&json_syntax_detail(error.line(), error.column()))
        }
        Ok(Value::Object(sections)) if !sections.contains_key(RESOURCES_MEMBER) => {
            template_format_error_message(NO_RESOURCES_DETAIL)
        }
        // The body is an object declaring `Resources`, so what it is missing is the thing
        // resource: the only other reading [`ProvisioningTemplate::read`] refuses.
        Ok(Value::Object(_)) => template_format_error_message(NO_THING_RESOURCE_DETAIL),
        Ok(_) => template_format_error_message(NOT_AN_OBJECT_DETAIL),
    }
}

/// How this service states that a declared resource is of a kind it does not provision.
///
/// The fixed text is the family's, and the variable parts are the logical id the template
/// declared the resource under and the type it gave it.
pub fn unsupported_resource_type_message(logical_id: &str, resource_type: &str) -> String {
    template_format_error_message(&format!(
        "resource {logical_id} is of type {resource_type}, which cannot be provisioned"
    ))
}

/// How this service states that a declared resource says nothing about what to provision —
/// it carries no `Type`, or no `Properties` to provision from.
pub fn undescribed_resource_message(logical_id: &str) -> String {
    template_format_error_message(&format!(
        "resource {logical_id} declares no {TYPE_MEMBER} and {PROPERTIES_MEMBER}"
    ))
}

/// How this service states that the template's thing resource describes no thing it can
/// register with the parameters supplied: a property whose `Ref` names a parameter the call
/// did not supply and the template gives no default for, a property that is not the value
/// the property means, a missing `ThingName`, or one no thing may be registered under.
pub fn unprovisionable_thing_message() -> String {
    template_format_error_message(&format!(
        "the {THING_RESOURCE_TYPE} resource describes no thing the parameters supplied can \
         register"
    ))
}

/// How this service states that a certificate resource names no certificate.
pub fn unnamed_certificate_message(logical_id: &str) -> String {
    template_format_error_message(&format!(
        "resource {logical_id} names no {CERTIFICATE_ID_PROPERTY} or {CERTIFICATE_PEM_PROPERTY}"
    ))
}

/// How this service states that a certificate resource asks for a status no caller may set.
///
/// `CertificateStatus` models six values, but two of them are the service's own
/// ([`super::INTERNAL_CERTIFICATE_STATUSES`]) and no writer may put a certificate into one,
/// so the statuses this names are the ones a declaration may ask for
/// ([`settable_certificate_statuses`]).
pub fn unmodeled_certificate_status_message(logical_id: &str, status: &str) -> String {
    template_format_error_message(&format!(
        "resource {logical_id} asks for certificate status {status}; the statuses are {}",
        settable_certificate_statuses().join(", ")
    ))
}

/// The `CertificateStatus` values a caller may ask a certificate to be left in: the modelled
/// ones less the service's own ([`super::INTERNAL_CERTIFICATE_STATUSES`], which every writer
/// of a certificate's status refuses).
pub fn settable_certificate_statuses() -> Vec<&'static str> {
    CERTIFICATE_STATUS_VALUES
        .iter()
        .copied()
        .filter(|status| !super::INTERNAL_CERTIFICATE_STATUSES.contains(status))
        .collect()
}

/// How this service states that a policy resource names no policy and describes none
/// either: without a `PolicyName` or a `PolicyDocument` the declaration provisions nothing.
pub fn unnamed_policy_message(logical_id: &str) -> String {
    template_format_error_message(&format!(
        "resource {logical_id} states no {POLICY_NAME_PROPERTY} or {POLICY_DOCUMENT_PROPERTY}"
    ))
}

/// How this service states that a policy resource's document is no IoT policy document: the
/// template-format family, carrying the rule the document broke
/// ([`super::validate_policy_document`]).
pub fn unreadable_policy_document_message(rejection: &str) -> String {
    template_format_error_message(rejection)
}

impl ComAmazonawsIotAwsiotserviceState {
    /// Mint the name of a policy a provisioning template states the DOCUMENT of without
    /// naming it: the name is then the service's, so it is minted here and no policy in the
    /// registry holds it.
    ///
    /// A version-4 UUID, which is inside the modelled `PolicyName` (len 1..128, pattern
    /// `^[\w+=,.@-]+$`); the loop re-mints on the (astronomically unlikely) collision, so
    /// one provisioning run can never be handed a name another policy already has.
    pub fn mint_provisioned_policy_name(&self) -> String {
        loop {
            let candidate = super::fresh_uuid();
            if !self.policies.contains_key(&candidate) {
                return candidate;
            }
        }
    }
}

/// How this service states that a certificate declaration names a certificate the account
/// does not hold: the sentence every reader and writer of one certificate states a miss
/// with ([`super::missing_certificate_message`]) when the declaration named an id, and the
/// property it named the certificate by when it named a PEM instead.
pub fn unheld_certificate_message(named_id: Option<&str>) -> String {
    match named_id {
        Some(certificate_id) => super::missing_certificate_message(certificate_id),
        None => format!("The certificate {CERTIFICATE_PEM_PROPERTY} names does not exist"),
    }
}

/// What one `RegisterThing` provisioned: the ARN of each resource, under the logical id the
/// template declared it as, and the PEM of the certificate the device was provisioned with.
#[derive(Debug, Clone, Default)]
pub struct ProvisionedResources {
    /// `RegisterThing$resourceArns`: "ARNs for the generated resources", keyed by the
    /// `ResourceLogicalId` the template declares each resource under.
    pub resource_arns: BTreeMap<String, String>,
    /// `RegisterThing$certificatePem`, "The certificate data, in PEM format": the stored
    /// PEM of the certificate the template's certificate resource names, so what this call
    /// reports and what `DescribeCertificate` reads are one value. `None` for a template
    /// that declares no certificate.
    pub certificate_pem: Option<String>,
}

/// One resource of a template's `Resources` section, as the body declares it.
#[derive(Debug, Clone)]
struct DeclaredResource {
    /// The `ResourceLogicalId` the template declares it under — the key
    /// [`ProvisionedResources::resource_arns`] reports its ARN under.
    logical_id: String,
    /// Which kind of resource it is.
    resource_type: String,
    /// What describes it, with its `Ref`s unsubstituted.
    properties: Map<String, Value>,
}

/// Provision everything `template_body` declares, with `parameters` substituted into it.
///
/// The ARNs answered are the ARNs of records this store holds afterwards: the thing the
/// template's thing resource describes, the certificate its certificate resource names and
/// the policy its policy resource names — plus the two attachments provisioning a device
/// amounts to: the policy attached to that certificate, and that certificate attached to
/// the thing as the principal the device connects as.
pub fn provision_template(
    store: &mut ComAmazonawsIotAwsiotserviceState,
    region: &str,
    template_body: &str,
    parameters: &HashMap<String, String>,
    now: SystemTime,
) -> Result<ProvisionedResources, ProvisioningRefusal> {
    // The body is read through the SAME reader a bulk thing-registration task reads it
    // with, so a template either path refuses is refused by both for the same reason, and
    // the thing it describes is one thing.
    let template = ProvisioningTemplate::read(template_body)
        .map_err(|_| ProvisioningRefusal::Template(unreadable_template_message(template_body)))?;
    let declared = declared_resources(template_body)?;
    let supplied: Map<String, Value> = parameters
        .iter()
        .map(|(name, value)| (name.clone(), Value::String(value.clone())))
        .collect();
    let substitutions = template.substitutions(&supplied);

    // Everything the declarations need is resolved BEFORE the first write, so a resource
    // this service cannot provision leaves the store untouched rather than half-written.
    // `ProvisioningTemplate::read` has already refused a body declaring no thing resource,
    // so there is one to find.
    let thing_logical_id = declared
        .iter()
        .find(|resource| resource.resource_type == THING_RESOURCE_TYPE)
        .map(|resource| resource.logical_id.clone())
        .ok_or_else(|| ProvisioningRefusal::Template(unprovisionable_thing_message()))?;
    let thing = template
        .provisioned_thing(&supplied)
        .ok_or_else(|| ProvisioningRefusal::Template(unprovisionable_thing_message()))?;
    let certificate = declared
        .iter()
        .find(|resource| resource.resource_type == CERTIFICATE_RESOURCE_TYPE)
        .map(|resource| planned_certificate(store, resource, &substitutions))
        .transpose()?;
    let policy = declared
        .iter()
        .find(|resource| resource.resource_type == POLICY_RESOURCE_TYPE)
        .map(|resource| planned_policy(store, resource, &substitutions))
        .transpose()?;
    // A device's policy governs the device through the certificate it connects with, so a
    // template declaring both provisions the ATTACHMENT too — and whether the relation will
    // take it is read HERE, where nothing has been written, from the one place its ceilings
    // live ([`super::PolicyAttachments::refusal_for`], which the write itself answers).
    if let (Some(certificate), Some(policy)) = (&certificate, &policy) {
        if let Some(limit) = store
            .policy_attachments
            .refusal_for(&certificate.target, &policy.policy_name)
        {
            return Err(ProvisioningRefusal::Resource(limit.message().to_string()));
        }
    }
    // The device CONNECTS as that certificate, so a template declaring one provisions the
    // thing-principal attachment too — the relation `AttachThingPrincipal` writes and
    // `ListThingPrincipals` reads — and whether the relation will take it is read HERE, where
    // nothing has been written, from the one place its ceiling lives
    // ([`ComAmazonawsIotAwsiotserviceState::thing_principal_refusal_for`], which the write
    // itself answers).
    if let Some(certificate) = &certificate {
        if let Some(refusal) =
            store.thing_principal_refusal_for(&thing.thing_name, &certificate.principal)
        {
            return Err(ProvisioningRefusal::Resource(refusal.message()));
        }
    }

    // The thing's own write is the one that can still refuse — a name the registry holds
    // with a different configuration, a thing type it does not hold — so it is made first
    // and nothing else is written until it succeeds. Every other write below has had its
    // refusals read off the store already, so none of them can fail once this one has.
    let thing_name = thing.thing_name.clone();
    let registered = store
        .register_thing(region, thing.thing_name, thing.configuration)
        .map_err(|refusal| ProvisioningRefusal::Resource(refusal.message()))?;
    let mut provisioned = ProvisionedResources {
        resource_arns: BTreeMap::from([(thing_logical_id, registered.thing_arn)]),
        certificate_pem: None,
    };

    if let Some(policy) = &policy {
        let policy_arn = provision_policy(store, region, policy, now);
        provisioned
            .resource_arns
            .insert(policy.logical_id.clone(), policy_arn);
    }

    if let Some(certificate) = &certificate {
        if let Some(status) = &certificate.status {
            // The plan refused both of this write's own refusals already: a status no caller
            // may set, and a certificate the store does not hold.
            store
                .update_certificate_status(&certificate.certificate_id, status, now)
                .map_err(|refusal| {
                    ProvisioningRefusal::Resource(refusal.message().to_string())
                })?;
        }
        provisioned.resource_arns.insert(
            certificate.logical_id.clone(),
            certificate.certificate_arn.clone(),
        );
        provisioned.certificate_pem = Some(certificate.certificate_pem.clone());

        // The provisioned thing is reached through this certificate, so the attachment is
        // recorded in the one relation that keeps it; the plan has read its ceiling, so this
        // cannot refuse either. It is what makes a `DeleteThing` of a provisioned device
        // refuse until the caller detaches the certificate, exactly as AWS IoT refuses it.
        store
            .record_thing_principal(&thing_name, &certificate.principal, None)
            .map_err(|refusal| ProvisioningRefusal::Resource(refusal.message()))?;

        // Recorded in the one relation `AttachPolicy` writes and `ListAttachedPolicies`
        // reads; the plan has read its ceilings, so this cannot refuse either.
        if let Some(policy) = &policy {
            store
                .policy_attachments
                .attach(&certificate.target, &policy.policy_name)
                .map_err(|limit| {
                    ProvisioningRefusal::Resource(limit.message().to_string())
                })?;
        }
    }

    Ok(provisioned)
}

/// Every resource the body's `Resources` section declares, in the body's own order.
///
/// A resource that says nothing about what to provision, and one of a kind this service
/// does not provision, is refused here rather than silently left out of the response: a
/// template declares what provisioning creates, so a declaration nothing acts on is a
/// template this service cannot carry out.
fn declared_resources(body: &str) -> Result<Vec<DeclaredResource>, ProvisioningRefusal> {
    // The body has already been read as a provisioning template by the caller, so the two
    // `else` arms below are unreachable; they state the same rule rather than unwrapping.
    let Ok(Value::Object(template)) = ::serde_json::from_str::<Value>(body) else {
        return Err(ProvisioningRefusal::Template(unreadable_template_message(
            body,
        )));
    };
    let Some(resources) = template.get(RESOURCES_MEMBER).and_then(Value::as_object) else {
        return Err(ProvisioningRefusal::Template(unreadable_template_message(
            body,
        )));
    };
    resources
        .iter()
        .map(|(logical_id, declaration)| {
            let described = declaration
                .get(TYPE_MEMBER)
                .and_then(Value::as_str)
                .zip(declaration.get(PROPERTIES_MEMBER).and_then(Value::as_object));
            let Some((resource_type, properties)) = described else {
                return Err(ProvisioningRefusal::Template(undescribed_resource_message(
                    logical_id,
                )));
            };
            if ![
                THING_RESOURCE_TYPE,
                CERTIFICATE_RESOURCE_TYPE,
                POLICY_RESOURCE_TYPE,
            ]
            .contains(&resource_type)
            {
                return Err(ProvisioningRefusal::Template(
                    unsupported_resource_type_message(logical_id, resource_type),
                ));
            }
            Ok(DeclaredResource {
                logical_id: logical_id.clone(),
                resource_type: resource_type.to_string(),
                properties: properties.clone(),
            })
        })
        .collect()
}

/// The certificate a template's certificate resource names, resolved against the store
/// WITHOUT writing: the record it addresses, the status the declaration asks for it, and the
/// policy-attachment target it is.
#[derive(Debug, Clone)]
struct PlannedCertificate {
    /// The logical id the template declares the resource under.
    logical_id: String,
    /// The id of the record the declaration names — the certificate store's key.
    certificate_id: String,
    /// That record's ARN, as the response reports it.
    certificate_arn: String,
    /// That record's PEM, as the response reports it.
    certificate_pem: String,
    /// The status the declaration asks the certificate to be left in, when it asks for
    /// one; `None` leaves the record's own status alone.
    status: Option<String>,
    /// The certificate as a target of the policy-attachment relation, resolved here so the
    /// attachment a declared policy makes is admissible before anything is written.
    target: super::ResolvedPolicyTarget,
    /// The certificate as a PRINCIPAL of the thing-principal relation, resolved here for the
    /// same reason: the thing the template provisions is reached through this certificate,
    /// so provisioning records that attachment too.
    principal: super::ResolvedThingPrincipal,
}

/// Resolve one certificate declaration, or say why it provisions nothing.
///
/// A certificate is the device's own identity, minted before provisioning runs
/// (`CreateKeysAndCertificate`, `CreateProvisioningClaim`), so the declaration NAMES one
/// the account holds — by its id, or by the PEM of the record itself — rather than
/// describing one to create.
fn planned_certificate(
    store: &ComAmazonawsIotAwsiotserviceState,
    resource: &DeclaredResource,
    substitutions: &BTreeMap<String, String>,
) -> Result<PlannedCertificate, ProvisioningRefusal> {
    // A status no caller may set is a fact about the BODY, independent of what the account
    // holds, so it is refused before the store is consulted at all.
    let status = resource
        .properties
        .get(CERTIFICATE_STATUS_PROPERTY)
        .and_then(|property| render_text(property, substitutions));
    if let Some(status) = &status {
        if !settable_certificate_statuses().contains(&status.as_str()) {
            return Err(ProvisioningRefusal::Template(
                unmodeled_certificate_status_message(&resource.logical_id, status),
            ));
        }
    }

    let named_id = resource
        .properties
        .get(CERTIFICATE_ID_PROPERTY)
        .and_then(|property| render_text(property, substitutions));
    let named_pem = resource
        .properties
        .get(CERTIFICATE_PEM_PROPERTY)
        .and_then(|property| render_text(property, substitutions));
    let record = match (&named_id, &named_pem) {
        (Some(certificate_id), _) => store.certificates.get(certificate_id),
        (None, Some(certificate_pem)) => store
            .certificates
            .values()
            .find(|held| held.certificate_pem.trim() == certificate_pem.trim()),
        (None, None) => {
            return Err(ProvisioningRefusal::Template(unnamed_certificate_message(
                &resource.logical_id,
            )))
        }
    };
    let Some(record) = record else {
        return Err(ProvisioningRefusal::Resource(unheld_certificate_message(
            named_id.as_deref(),
        )));
    };

    // The certificate is the principal a device's policy is attached to, so the target it is
    // in that relation is resolved here, where nothing has been written yet.
    let target = store
        .resolve_policy_target(&record.certificate_arn)
        .map_err(|refusal| ProvisioningRefusal::Resource(refusal.message().to_string()))?;

    // It is also the principal the provisioned THING is reached through, so what it is in
    // the thing-principal relation ([`super::thing_principals`]) is resolved here beside it.
    let principal = store
        .resolve_thing_principal(&record.certificate_arn)
        .map_err(|refusal| ProvisioningRefusal::Resource(refusal.message()))?;

    Ok(PlannedCertificate {
        logical_id: resource.logical_id.clone(),
        certificate_id: record.certificate_id.clone(),
        certificate_arn: record.certificate_arn.clone(),
        certificate_pem: record.certificate_pem.clone(),
        status,
        target,
        principal,
    })
}

/// The policy a template's policy resource names, resolved against the store WITHOUT
/// writing: the name it is provisioned under, and either the record already held or the
/// document to create it from.
#[derive(Debug, Clone)]
struct PlannedPolicy {
    /// The logical id the template declares the resource under.
    logical_id: String,
    /// The name the policy is provisioned under — the policy registry's key.
    policy_name: String,
    /// What provisioning this declaration amounts to.
    provision: PolicyProvision,
}

/// What provisioning one policy declaration amounts to.
#[derive(Debug, Clone)]
enum PolicyProvision {
    /// The registry already holds a policy under that name: that record's ARN, and
    /// nothing to write.
    Held(String),
    /// It holds none under that name: the document the policy is created from.
    Create(String),
}

/// Resolve one policy declaration, or say why it provisions nothing.
///
/// A declaration names the policy by `PolicyName`, or states the `PolicyDocument` of one
/// the service names itself — the policy the device gets is then a record minted by this
/// call, under a name no policy in the registry holds.
///
/// Every device run through one template gets the SAME policy where the template names it,
/// so a name the registry already holds is the policy that is already there and nothing is
/// written for it — which is what makes provisioning a second device under one template
/// succeed rather than conflict on the policy.
fn planned_policy(
    store: &ComAmazonawsIotAwsiotserviceState,
    resource: &DeclaredResource,
    substitutions: &BTreeMap<String, String>,
) -> Result<PlannedPolicy, ProvisioningRefusal> {
    // A template may state the document as the JSON object it is, or as the text of one;
    // either way the policy is created from the document text the registry keeps.
    let document = match resource.properties.get(POLICY_DOCUMENT_PROPERTY) {
        Some(Value::Object(document)) => Some(Value::Object(document.clone()).to_string()),
        Some(property) => render_text(property, substitutions),
        None => None,
    };
    let named = resource
        .properties
        .get(POLICY_NAME_PROPERTY)
        .and_then(|property| render_text(property, substitutions));

    let policy_name = match (named, &document) {
        // The declaration names the policy: a name the registry holds is the policy that is
        // already there, whatever document the declaration also states.
        (Some(policy_name), _) => {
            if let Some(held) = store.policies.get(&policy_name) {
                return Ok(PlannedPolicy {
                    logical_id: resource.logical_id.clone(),
                    policy_name,
                    provision: PolicyProvision::Held(held.policy_arn.clone()),
                });
            }
            // Nothing names a policy the account holds, and nothing describes one to
            // create: the declaration names a policy that does not exist, in the sentence
            // every reader and writer of one policy states a miss with.
            if document.is_none() {
                return Err(ProvisioningRefusal::Resource(super::missing_policy_message(
                    &policy_name,
                )));
            }
            policy_name
        }
        // It states a document and no name, so the policy's name is this service's to mint.
        (None, Some(_)) => store.mint_provisioned_policy_name(),
        (None, None) => {
            return Err(ProvisioningRefusal::Template(unnamed_policy_message(
                &resource.logical_id,
            )))
        }
    };

    let document = document.unwrap_or_default();
    // A policy IS its document, so one that is no IoT policy document is a template this
    // service cannot carry out — the same reading `CreatePolicy` applies
    // ([`super::validate_policy_document`]).
    if let Err(rejection) = super::validate_policy_document(&document) {
        return Err(ProvisioningRefusal::Template(
            unreadable_policy_document_message(&rejection.message()),
        ));
    }
    if store.policies.len() >= super::MAX_POLICIES {
        return Err(ProvisioningRefusal::Resource(format!(
            "the maximum of {} policies is already reached",
            super::MAX_POLICIES
        )));
    }
    Ok(PlannedPolicy {
        logical_id: resource.logical_id.clone(),
        policy_name,
        provision: PolicyProvision::Create(document),
    })
}

/// Write the planned policy, and answer the ARN of the record the device is provisioned
/// with. A declaration whose policy the registry already holds writes nothing and reports
/// that record's ARN, so a second device provisioned under one template gets the policy the
/// first one did.
fn provision_policy(
    store: &mut ComAmazonawsIotAwsiotserviceState,
    region: &str,
    planned: &PlannedPolicy,
    now: SystemTime,
) -> String {
    let document = match &planned.provision {
        PolicyProvision::Held(policy_arn) => return policy_arn.clone(),
        PolicyProvision::Create(document) => document.clone(),
    };
    let policy_arn = super::policy_arn(region, &planned.policy_name);
    let policy = super::RegisteredPolicy::new(
        planned.policy_name.clone(),
        policy_arn.clone(),
        document,
        BTreeMap::new(),
        now,
    );
    store.policies.insert(policy.policy_name.clone(), policy);
    policy_arn
}
