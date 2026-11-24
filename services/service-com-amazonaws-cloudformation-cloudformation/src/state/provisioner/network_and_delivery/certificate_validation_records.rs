// SPDX-License-Identifier: CC0-1.0

//! The DNS validation records CloudFormation itself publishes for an
//! `AWS::CertificateManager::Certificate`: the challenge CNAME of each covered name, written
//! into the hosted zone that name's `DomainValidationOptions` entry names.
//!
//! `HostedZoneId` is no member of ACM's `DomainValidationOption` at all — that shape carries
//! the domain and the mail suffix ACM validates it through
//! (`smithy_query(shape="com.amazonaws.acm#DomainValidationOption")`) — it is CloudFormation's
//! own instruction: the Route 53 zone the provider is to publish the certificate's validation
//! record in, so the name is proven without anybody writing the record by hand. ACM hands the
//! record out on the certificate it minted (`DomainValidation$ResourceRecord` "contains the
//! CNAME record that you add to your DNS database for domain validation"), so the record's
//! name, type and value are READ off the certificate and never composed here.
//!
//! The record is published while the certificate is still waiting on proof of domain control,
//! which is the only window in which it proves anything — and it is published with
//! `UPSERT`, so the same instruction applied twice leaves the zone holding exactly one
//! record set, whatever the zone already held for that name. A zone that cannot hold the
//! record (one Route 53 does not hold, one that is not authoritative for the name) refuses
//! the change batch, and that refusal is the resource's own failure: the certificate ACM
//! minted stays, under the identifier the stack already published for it.
//!
//! Removing the resource withdraws what this published: the record set belongs to the
//! provider that wrote it, so a zone the same stack declares is left able to be deleted.

use super::*;

/// The `TTL` the challenge record is published with, in seconds: the value AWS's own
/// documented example of the ACM validation CNAME carries, and the one Route 53 offers for a
/// `CNAME` record set. `ResourceRecordSet$TTL` is required beside `ResourceRecords` ("all of
/// [TTL, and ResourceRecords]"), so a record set is published with one or not at all.
pub(crate) const VALIDATION_RECORD_TTL: u64 = 300;

/// One covered name whose validation record a declaration asks CloudFormation to publish, and
/// the hosted zone it is to be published in.
///
/// The zone id is kept in the BARE form every Route 53 `ResourceId` member is bounded for
/// ([`bare_hosted_zone_id`]), which is what a template that states a zone id literally writes
/// and what a `Ref` to a zone of the same template resolves to.
pub(crate) type DeclaredValidationZone = (String, String);

/// The covered names a declaration asks CloudFormation to publish a validation record for,
/// in the order the declaration states them.
///
/// Only an entry that names a zone is one: an entry naming a mail suffix alone is ACM's own
/// ([`super::ACM_DOMAIN_VALIDATION_OPTION_MEMBERS`]), and a declaration with no
/// `DomainValidationOptions` at all asks for nothing to be published.
pub(crate) fn declared_validation_zones(declared: &Value) -> Vec<DeclaredValidationZone> {
    let Some(Value::Array(options)) = declared.get("DomainValidationOptions") else {
        return Vec::new();
    };
    options
        .iter()
        .filter_map(|option| {
            let domain = option.get("DomainName").and_then(scalar_text)?;
            let zone = option.get(super::ACM_HOSTED_ZONE_ID).and_then(scalar_text)?;
            (!domain.is_empty() && !zone.is_empty())
                .then(|| (domain, bare_hosted_zone_id(&zone).to_string()))
        })
        .collect()
}

/// The challenge record ACM hands out for `domain` on the certificate it reported, as the
/// `(name, type, value)` a change batch carries it as.
///
/// `None` where that certificate's validation of the name publishes no DNS record: a name the
/// certificate does not cover at all, or one ACM validates by a method whose evidence is not a
/// DNS record ("`ResourceRecord` … is only populated for `DNS` validation"). Nothing is
/// invented for either — what a client publishes is exactly what ACM asked for.
///
/// The names are compared as DNS names ([`fully_qualified`]), so the trailing dot and the
/// letter case the template wrote the domain with do not decide which entry it is.
fn challenge_record(detail: Option<&Value>, domain: &str) -> Option<(String, String, String)> {
    let record = detail?
        .get("DomainValidationOptions")?
        .as_array()?
        .iter()
        .find(|validation| {
            validation
                .get("DomainName")
                .and_then(Value::as_str)
                .is_some_and(|name| fully_qualified(name) == fully_qualified(domain))
        })?
        .get("ResourceRecord")?;
    let name = record.get("Name").and_then(scalar_text)?;
    let record_type = record.get("Type").and_then(scalar_text)?;
    let value = record.get("Value").and_then(scalar_text)?;
    (!name.is_empty() && !record_type.is_empty() && !value.is_empty())
        .then_some((name, record_type, value))
}

/// One `Change` element carrying `action` over the challenge record, rendered through the
/// same `ResourceRecordSet` writer a declared record set goes through
/// ([`record_set_element_of`]) so a record this provider publishes and one a template declares
/// cannot be written differently.
///
/// `ttl` is the cache lifetime the record set carries: the one this provider publishes it
/// with, and — for the DELETE that withdraws it — the one the zone REPORTS holding it under,
/// since Route 53 removes a record set by its own values and "if the current value does not
/// match the actual value, an error is returned".
fn validation_change(
    action: &str,
    (name, record_type, value): &(String, String, String),
    ttl: &str,
) -> Result<String, Refusal> {
    let element = record_set_element_of(
        super::ACM_CERTIFICATE,
        &serde_json::json!({
            "Name": name,
            "Type": record_type,
            "TTL": ttl,
            "ResourceRecords": [value],
        }),
    )?;
    Ok(format!(
        "<Change>{}{element}</Change>",
        xml_leaf("Action", action)
    ))
}

/// Apply one change batch to the zone `zone`, which is how Route 53 writes record sets:
/// either every change in it is applied or none is.
async fn change_validation_records(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    zone: &str,
    changes: &str,
) -> Result<(), Refusal> {
    call_route53(
        ctx,
        region,
        "POST",
        &format!("/hostedzone/{}/rrset", percent_encoded(zone)),
        Some(&route53_request(
            "ChangeResourceRecordSetsRequest",
            &format!("<ChangeBatch><Changes>{changes}</Changes></ChangeBatch>"),
        )),
    )
    .await
    .map(|_| ())
}

/// Publish the challenge record of every covered name in `wanted` into the zone that entry
/// names, so ACM's own resolution of the challenge proves the name and the certificate reaches
/// the state it is usable in.
///
/// One change batch PER ZONE, in the order the declaration names them: two covered names that
/// live in different zones each get their record in their own zone, and two that share a zone
/// are written in the one batch that zone takes.
///
/// `Ok(None)` is every record published. `Ok(Some(reason))` is a declaration asking for a
/// record the certificate publishes none of ([`challenge_record`]), which no zone could be
/// made to hold — the resource's own failure, in the words of what ACM reports about it.
/// `Err` is Route 53's refusal of the batch, which is the resource's failure in that
/// service's own words.
pub(crate) async fn publish_validation_records(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
    detail: Option<&Value>,
    wanted: &[DeclaredValidationZone],
) -> Result<Option<String>, Refusal> {
    let mut batches: Vec<(String, String)> = Vec::new();
    for (domain, zone) in wanted {
        let Some(record) = challenge_record(detail, domain) else {
            return Ok(Some(format!(
                "ACM reports no DNS validation record for {domain} on certificate \
                 {physical_resource_id}"
            )));
        };
        let change = validation_change(
            RECORD_SET_UPSERT,
            &record,
            &VALIDATION_RECORD_TTL.to_string(),
        )?;
        match batches.iter_mut().find(|(held, _)| held == zone) {
            Some((_, changes)) => changes.push_str(&change),
            None => batches.push((zone.clone(), change)),
        }
    }
    for (zone, changes) in batches {
        change_validation_records(ctx, region, &zone, &changes).await?;
    }
    Ok(None)
}

/// Withdraw the challenge records this provider published, before the certificate they prove
/// is deleted.
///
/// Route 53 removes a record set by its own values ("if the current value does not match the
/// actual value, an error is returned"), so the delete carries the record ACM still reports —
/// the one the publish wrote — under the `TTL` the ZONE reports holding it with, which is the
/// value that batch has to match whether or not it is still the one this provider published.
/// It models no not-found error for a record set and reports a change batch it cannot apply as
/// one refusal class whatever the problem was, so a record set the zone no longer holds is
/// read back rather than deleted blind ([`read_record_set`]); a zone Route 53 no longer holds
/// holds none of them either.
pub(crate) async fn withdraw_validation_records(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    detail: Option<&Value>,
    wanted: &[DeclaredValidationZone],
) -> Result<(), Refusal> {
    for (domain, zone) in wanted {
        let Some(record) = challenge_record(detail, domain) else {
            continue;
        };
        let (name, record_type, _) = &record;
        let held = match read_record_set(ctx, region, zone, name, record_type, None).await {
            Ok(Some(held)) => held,
            Ok(None) => continue,
            Err(refusal) if refusal.code == NO_SUCH_HOSTED_ZONE => continue,
            Err(refusal) => return Err(refusal),
        };
        let ttl = held
            .get("TTL")
            .and_then(scalar_text)
            .unwrap_or_else(|| VALIDATION_RECORD_TTL.to_string());
        let change = validation_change(RECORD_SET_DELETE, &record, &ttl)?;
        match change_validation_records(ctx, region, zone, &change).await {
            Ok(()) => {}
            Err(refusal) if refusal.code == NO_SUCH_HOSTED_ZONE => {}
            Err(refusal) => return Err(refusal),
        }
    }
    Ok(())
}
