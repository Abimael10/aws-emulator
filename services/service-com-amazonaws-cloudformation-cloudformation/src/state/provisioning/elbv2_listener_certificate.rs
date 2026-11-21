// SPDX-License-Identifier: CC0-1.0

//! `AWS::ElasticLoadBalancingV2::ListenerCertificate`: the certificates a stack adds to the
//! CERTIFICATE LIST of a listener, added in Elastic Load Balancing itself.
//!
//! The type is no resource of that service's own. It is one or more ENTRIES of the certificate
//! list of the listener it names — the certificates an HTTPS or TLS listener serves beside the
//! default one `CreateListener` and `ModifyListener` own — which `AddListenerCertificates` adds
//! ("Adds the specified SSL server certificate to the certificate list for the specified HTTPS
//! or TLS listener") and `RemoveListenerCertificates` removes. Its two properties are those
//! calls' own members under the same names, and Elastic Load Balancing mints no identifier for
//! a list entry, so the resource keeps the physical id CloudFormation generated for it.
//!
//! Because the resource IS the attachment, each of its lifecycle calls is one of those two: the
//! create ADDS every declared certificate, an update adds the ones the new declaration names and
//! removes the ones it no longer names, and the delete removes exactly the certificates the
//! stack attached. So `DescribeListenerCertificates` on the listener reports the stack's
//! certificates with `IsDefault` false beside the listener's own default one, a stack that drops
//! the resource leaves the listener presenting its default alone, and a declaration naming a
//! certificate the owning service resolves to none fails the resource with that service's own
//! refusal instead of being reported as deployed.
//!
//! It lives beside [`super::elastic_load_balancing`], whose four types ARE resources of that
//! service, and reaches Elastic Load Balancing through that module's own call seam
//! ([`super::elastic_load_balancing::call_elb`]), so one place addresses that service.

use serde_json::Value;

use super::elastic_load_balancing::{call_elb, LISTENER_NOT_FOUND};
use super::{xml_member_structures, Created, Refusal};
use crate::state::StoredResource;

/// The resource type this module provisions.
pub(super) const ELB_LISTENER_CERTIFICATE: &str =
    "AWS::ElasticLoadBalancingV2::ListenerCertificate";

/// The property naming the listener the certificates are added to, which is the member both
/// calls address a certificate list by (`AddListenerCertificates$ListenerArn`).
const LISTENER_ARN_PROPERTY: &str = "ListenerArn";

/// The property carrying the certificates themselves, which is the member both calls take them
/// under (`AddListenerCertificates$Certificates`).
const CERTIFICATES_PROPERTY: &str = "Certificates";

/// The member of one certificate that names it (`Certificate$CertificateArn`). `IsDefault` is
/// the other member of that shape and is NOT sent: "Set CertificateArn to the certificate ARN
/// but do not set IsDefault", the default certificate being the listener's own.
const CERTIFICATE_ARN_MEMBER: &str = "CertificateArn";

/// The element an awsQuery list carries each entry under, which is the Smithy default every
/// list of that model uses.
const ENTRY: &str = "member";

/// Every property `AWS::ElasticLoadBalancingV2::ListenerCertificate` publishes: the listener and
/// the certificates. A declared property this list does not carry is REFUSED rather than
/// dropped, so an attachment is never reported as made with less than its declaration asked for.
///
/// Source: the type's own property set, which is `AddListenerCertificates`' member list
/// (`smithy_query(query="operation",operation="AddListenerCertificates")`) under the same names.
const LISTENER_CERTIFICATE_PROPERTIES: &[&str] = &[CERTIFICATES_PROPERTY, LISTENER_ARN_PROPERTY];

/// The call that puts one certificate on a listener's certificate list.
const ADD_LISTENER_CERTIFICATES: &str = "AddListenerCertificates";
/// The call that takes one off it.
const REMOVE_LISTENER_CERTIFICATES: &str = "RemoveListenerCertificates";
/// The read that answers which certificates a listener presents.
const DESCRIBE_LISTENER_CERTIFICATES: &str = "DescribeListenerCertificates";

/// The value `DescribeListenerCertificates` reports a listener's OWN default certificate under,
/// which is the one entry of its answer that is no list entry: "`IsDefault`: indicates whether
/// the certificate is the default certificate". A list entry carries the other value, and that
/// is the half of the answer this resource is about.
const DEFAULT_CERTIFICATE: &str = "true";

/// Add the certificates an `AWS::ElasticLoadBalancingV2::ListenerCertificate` declares to the
/// certificate list of the listener it names — so a client that deployed the stack reads them
/// back with `DescribeListenerCertificates`, and the listener really presents them.
///
/// `Err` is Elastic Load Balancing's own refusal: a listener it does not hold, a certificate
/// that resolves to none of the account's, a list already at its limit — which is also what
/// bounds how many calls one declaration makes, since that service refuses an add past the
/// certificates a listener's list may hold and the walk stops on the first refusal.
pub(super) async fn create(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<Created, Refusal> {
    let declaration = Declaration::read(&resource.properties)?;
    for certificate in &declaration.certificates {
        declaration.add(ctx, region, certificate).await?;
    }
    // The attachment publishes no attribute and carries no identity of its own: that service
    // mints no handle for a certificate-list entry, so the resource keeps the physical id the
    // naming rules generated for it.
    Ok(Created::default())
}

/// Apply a CHANGED declaration to the certificates the stack attached, through the two calls
/// Elastic Load Balancing publishes for a certificate list: the list ends up holding exactly the
/// certificates the declaration names, rather than the stack failing while the caller's change
/// went nowhere.
///
/// The ADDS precede the removals, so a declared certificate that service REFUSES leaves the
/// listener still presenting the certificates the stack had attached: the resource is reported
/// failed with that service's own refusal and the certificate in force is untouched, rather than
/// the stack rolling back over a listener that quietly lost a certificate. Adding one the list
/// already holds changes nothing — "if the certificate is already in the certificate list, the
/// call is successful but the certificate is not added again" — so every declared certificate is
/// sent and the list is left holding what the declaration states.
///
/// The LISTENER a declaration names is not changed here: it is the resource's own create-only
/// property ([`crate::state::template::replacement_properties`] has no row for this type, so the
/// deployment reports such a change as not applied), and what is removed is therefore removed
/// from the listener the certificates were added to.
pub(super) async fn update(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    declared: &Value,
) -> Result<Option<Created>, Refusal> {
    let declaration = Declaration::read(declared)?;
    if declaration.listener != listener_of(&resource.properties).unwrap_or_default() {
        // A certificate list belongs to ONE listener and both calls address it by that listener,
        // so a declaration naming another listener asks for a different attachment than the one
        // the stack has: there is no call that moves an entry, and the change is reported as not
        // applied rather than as a success that dropped it.
        return Ok(None);
    }
    for certificate in &declaration.certificates {
        declaration.add(ctx, region, certificate).await?;
    }
    for certificate in attached_certificates(&resource.properties)
        .iter()
        .filter(|held| !declaration.certificates.contains(held))
    {
        declaration.remove(ctx, region, certificate).await?;
    }
    Ok(Some(Created::default()))
}

/// Remove the certificates the stack attached from the listener they were added to, so the
/// listener presents exactly what it did before the resource was created.
///
/// A listener Elastic Load Balancing no longer holds is already in the state the removal asks
/// for — the certificate list went with the listener — judged on that service's own not-found
/// CODE and never on a message; and a certificate the list no longer holds leaves it as it is.
/// Elastic Load Balancing takes no snapshot of a certificate attachment on the way out, there
/// being no such call, so a `Snapshot` deletion policy on one removes it with no snapshot
/// invented for it.
pub(super) async fn delete(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<(), Refusal> {
    // Read off the RECORD of what was deployed rather than re-validated: a declaration this
    // module refused attached nothing, so there is nothing of it to take away.
    let Some(listener) = listener_of(&resource.properties) else {
        return Ok(());
    };
    let declaration = Declaration {
        listener,
        certificates: attached_certificates(&resource.properties),
    };
    for certificate in &declaration.certificates {
        match declaration.remove(ctx, region, certificate).await {
            Ok(()) => {}
            Err(refusal) if refusal.code == LISTENER_NOT_FOUND => return Ok(()),
            Err(refusal) => return Err(refusal),
        }
    }
    Ok(())
}

/// The attachment's configuration as Elastic Load Balancing reports it, for drift detection.
///
/// `DescribeListenerCertificates` is the read that answers what a listener presents, and of what
/// it reports only the entries the DECLARATION names are compared: "only resource properties
/// explicitly defined in the stack template are checked for drift", and a certificate another
/// resource attached to the same listener is not this one's. So a declared certificate the list
/// still holds is reported as that entry of the declared order, and one it no longer holds is
/// absent — which the comparison reports as that entry having been removed.
///
/// The listener's own DEFAULT certificate is left out: it is no entry of the certificate list
/// ([`DEFAULT_CERTIFICATE`]) but the listener resource's own `Certificates` property, so an
/// attachment is never compared against it.
///
/// A listener that service no longer holds is the attachment being GONE, judged on its
/// not-found code.
pub(super) async fn actual_configuration(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<Option<Value>, Refusal> {
    let Some(listener) = listener_of(&resource.properties) else {
        return Ok(None);
    };
    let parameters = vec![(LISTENER_ARN_PROPERTY.to_string(), listener.clone())];
    let answered = match call_elb(ctx, region, DESCRIBE_LISTENER_CERTIFICATES, &parameters).await {
        Ok(answered) => answered,
        Err(refusal) if refusal.code == LISTENER_NOT_FOUND => return Ok(None),
        Err(refusal) => return Err(refusal),
    };
    let held = listed_certificates(&answered);
    let entries = attached_certificates(&resource.properties)
        .into_iter()
        .filter(|arn| held.contains(arn))
        .map(|arn| serde_json::json!({ CERTIFICATE_ARN_MEMBER: arn }))
        .collect();
    let mut reported = ::serde_json::Map::new();
    reported.insert(LISTENER_ARN_PROPERTY.to_string(), Value::String(listener));
    reported.insert(CERTIFICATES_PROPERTY.to_string(), Value::Array(entries));
    Ok(Some(Value::Object(reported)))
}

/// The certificates a listener's certificate LIST holds, as that service's own read reports
/// them: every entry of the answer that is not the listener's default certificate.
fn listed_certificates(answered: &str) -> Vec<String> {
    xml_member_structures(answered, CERTIFICATES_PROPERTY)
        .into_iter()
        .filter(|entry| {
            entry.get("IsDefault").and_then(Value::as_str) != Some(DEFAULT_CERTIFICATE)
        })
        .filter_map(|entry| {
            entry
                .get(CERTIFICATE_ARN_MEMBER)
                .and_then(Value::as_str)
                .map(ToString::to_string)
        })
        .collect()
}

/// One declaration of this type, read into the two things both calls take: the listener the
/// certificate list belongs to, and the certificates to put on it.
struct Declaration {
    /// The listener the declaration names.
    listener: String,
    /// The certificates it names, in the declared order and each one once.
    certificates: Vec<String>,
}

impl Declaration {
    /// The declaration as the two calls take it, or the refusal a declaration that names no
    /// listener or no certificate earns — which is the model validation a provider runs before
    /// it calls anybody, so nothing is asked of Elastic Load Balancing for one.
    fn read(properties: &Value) -> Result<Self, Refusal> {
        if let Value::Object(declared) = properties {
            if let Some(property) = declared
                .keys()
                .find(|property| !LISTENER_CERTIFICATE_PROPERTIES.contains(&property.as_str()))
            {
                return Err(Refusal {
                    code: "NotImplemented".to_string(),
                    message: format!(
                        "this emulator does not apply the {property} property of \
                         {ELB_LISTENER_CERTIFICATE}"
                    ),
                });
            }
        }
        let listener = listener_of(properties).ok_or_else(|| required(LISTENER_ARN_PROPERTY))?;
        let certificates = attached_certificates(properties);
        if certificates.is_empty() {
            return Err(required(CERTIFICATES_PROPERTY));
        }
        Ok(Self {
            listener,
            certificates,
        })
    }

    /// Put ONE certificate on the listener's certificate list.
    async fn add(
        &self,
        ctx: &smithy_server_runtime::OperationContext<'_>,
        region: &str,
        certificate: &str,
    ) -> Result<(), Refusal> {
        self.call(ctx, region, ADD_LISTENER_CERTIFICATES, certificate)
            .await
    }

    /// Take ONE certificate off it.
    async fn remove(
        &self,
        ctx: &smithy_server_runtime::OperationContext<'_>,
        region: &str,
        certificate: &str,
    ) -> Result<(), Refusal> {
        self.call(ctx, region, REMOVE_LISTENER_CERTIFICATES, certificate)
            .await
    }

    /// One of the two calls, carrying ONE certificate: that is the unit both take — "you can
    /// specify one certificate per call" (`AddListenerCertificates$Certificates`) — so a
    /// declaration naming several is carried out as one call each, in the declared order.
    async fn call(
        &self,
        ctx: &smithy_server_runtime::OperationContext<'_>,
        region: &str,
        action: &str,
        certificate: &str,
    ) -> Result<(), Refusal> {
        let parameters = vec![
            (LISTENER_ARN_PROPERTY.to_string(), self.listener.clone()),
            (
                format!("{CERTIFICATES_PROPERTY}.{ENTRY}.1.{CERTIFICATE_ARN_MEMBER}"),
                certificate.to_string(),
            ),
        ];
        call_elb(ctx, region, action, &parameters).await.map(|_| ())
    }
}

/// The listener a declaration names, or `None` for one that names none.
fn listener_of(properties: &Value) -> Option<String> {
    properties
        .get(LISTENER_ARN_PROPERTY)
        .and_then(Value::as_str)
        .filter(|listener| !listener.is_empty())
        .map(ToString::to_string)
}

/// The certificates a declaration names, in the declared order and each one once: naming one
/// certificate twice puts it on the list once, the list being a set of certificates.
///
/// An entry carrying no `CertificateArn`, or an empty one, names no certificate and is passed
/// over: a declaration whose entries all do names none at all, which [`Declaration::read`]
/// refuses.
fn attached_certificates(properties: &Value) -> Vec<String> {
    let mut certificates: Vec<String> = Vec::new();
    let Some(declared) = properties.get(CERTIFICATES_PROPERTY).and_then(Value::as_array) else {
        return certificates;
    };
    for entry in declared {
        let Some(arn) = entry
            .get(CERTIFICATE_ARN_MEMBER)
            .and_then(Value::as_str)
            .filter(|arn| !arn.is_empty())
        else {
            continue;
        };
        if !certificates.iter().any(|held| held == arn) {
            certificates.push(arn.to_string());
        }
    }
    certificates
}

/// The refusal a declaration missing a property its type requires earns, naming the property.
fn required(property: &str) -> Refusal {
    Refusal {
        code: "InvalidRequest".to_string(),
        message: format!("{ELB_LISTENER_CERTIFICATE} requires the {property} property"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the certificates a declaration names are carried out in the declared order, each
    /// one once, and an entry that names no certificate is no certificate to attach.
    #[test]
    fn the_declared_certificates_are_read_in_order_and_once_each() {
        let leaf = "arn:aws:acm:us-west-2:000000000000:certificate/11111111-1111-1111-1111-111111111111";
        let other =
            "arn:aws:acm:us-west-2:000000000000:certificate/22222222-2222-2222-2222-222222222222";
        // (case, the declared `Certificates`, the certificates to attach)
        let cases: &[(&str, Value, Vec<&str>)] = &[
            (
                "one certificate",
                serde_json::json!([{ "CertificateArn": leaf }]),
                vec![leaf],
            ),
            (
                "two certificates",
                serde_json::json!([{ "CertificateArn": leaf }, { "CertificateArn": other }]),
                vec![leaf, other],
            ),
            (
                "the same certificate twice",
                serde_json::json!([{ "CertificateArn": leaf }, { "CertificateArn": leaf }]),
                vec![leaf],
            ),
            (
                "an entry carrying no ARN beside one that does",
                serde_json::json!([{ "IsDefault": false }, { "CertificateArn": other }]),
                vec![other],
            ),
            (
                "an empty ARN",
                serde_json::json!([{ "CertificateArn": "" }]),
                vec![],
            ),
            ("no entries at all", serde_json::json!([]), vec![]),
        ];
        for (case, declared, expected) in cases {
            let properties = serde_json::json!({
                "ListenerArn": "arn:aws:elasticloadbalancing:us-west-2:000000000000:listener/app/web/50dc6c495c0c9188/f2f7dc8efc522ab2",
                "Certificates": declared,
            });
            let read = attached_certificates(&properties);
            assert_eq!(
                read,
                expected
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<String>>(),
                "case {case:?}: expected the declared certificates once each, got {read:?}"
            );
        }
    }

    /// RULE: a declaration that names no listener, no certificate, or a property this module
    /// does not apply is refused before anything is asked of the owning service.
    #[test]
    fn an_incomplete_declaration_is_refused() {
        let listener = "arn:aws:elasticloadbalancing:us-west-2:000000000000:listener/app/web/50dc6c495c0c9188/f2f7dc8efc522ab2";
        let leaf = "arn:aws:acm:us-west-2:000000000000:certificate/11111111-1111-1111-1111-111111111111";
        let cases: &[(&str, Value)] = &[
            ("no properties at all", Value::Null),
            (
                "no listener",
                serde_json::json!({ "Certificates": [{ "CertificateArn": leaf }] }),
            ),
            (
                "an empty listener",
                serde_json::json!({
                    "ListenerArn": "",
                    "Certificates": [{ "CertificateArn": leaf }],
                }),
            ),
            (
                "no certificates",
                serde_json::json!({ "ListenerArn": listener }),
            ),
            (
                "certificates naming none",
                serde_json::json!({
                    "ListenerArn": listener,
                    "Certificates": [{ "CertificateArn": "" }],
                }),
            ),
            (
                "a property this module does not apply",
                serde_json::json!({
                    "ListenerArn": listener,
                    "Certificates": [{ "CertificateArn": leaf }],
                    "Tags": [],
                }),
            ),
        ];
        for (case, properties) in cases {
            let read = Declaration::read(properties);
            assert!(
                read.is_err(),
                "case {case:?}: expected a refusal naming what is missing, got a declaration"
            );
        }
        let complete = Declaration::read(&serde_json::json!({
            "ListenerArn": listener,
            "Certificates": [{ "CertificateArn": leaf }],
        }));
        let read = complete
            .as_ref()
            .ok()
            .map(|declaration| (declaration.listener.clone(), declaration.certificates.clone()));
        assert_eq!(
            read,
            Some((listener.to_string(), vec![leaf.to_string()])),
            "case \"a complete declaration\": expected the listener and its certificate, got \
             {read:?}"
        );
    }

    /// RULE: of what the owning service reports a listener presenting, the certificate LIST is
    /// every entry that is not the listener's own default certificate.
    #[test]
    fn the_default_certificate_is_no_list_entry() {
        let document = "<DescribeListenerCertificatesResult><Certificates>\
             <member><CertificateArn>arn:aws:acm:us-west-2:000000000000:certificate/a</CertificateArn>\
             <IsDefault>true</IsDefault></member>\
             <member><CertificateArn>arn:aws:acm:us-west-2:000000000000:certificate/b</CertificateArn>\
             <IsDefault>false</IsDefault></member>\
             </Certificates></DescribeListenerCertificatesResult>";
        let listed = listed_certificates(document);
        assert_eq!(
            listed,
            vec!["arn:aws:acm:us-west-2:000000000000:certificate/b".to_string()],
            "case \"a default certificate beside a list entry\": expected only the list entry, \
             got {listed:?}"
        );
        let empty = listed_certificates("<DescribeListenerCertificatesResult/>");
        assert_eq!(
            empty,
            Vec::<String>::new(),
            "case \"an answer carrying no certificates\": expected no entries, got {empty:?}"
        );
    }
}
