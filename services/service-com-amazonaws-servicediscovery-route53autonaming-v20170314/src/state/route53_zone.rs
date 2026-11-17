// SPDX-License-Identifier: CC0-1.0

//! The Route 53 hosted zone a DNS namespace IS, and the record sets Cloud Map publishes
//! into it.
//!
//! A DNS namespace is not a label on a record: "When you create a private DNS namespace,
//! Cloud Map automatically creates an Amazon Route 53 private hosted zone that has the same
//! name as the namespace" (`CreatePrivateDnsNamespace$Name`), and the records of the
//! services in it are record sets of THAT zone — which is what lets a client resolve
//! `<service>.<namespace>` through DNS instead of calling `DiscoverInstances`. Two things
//! follow, and this module is both of them:
//!
//! * the id a namespace reports as `Properties.DnsProperties.HostedZoneId` is the id of a
//!   hosted zone Route 53 HOLDS — created when the namespace is created, associated with
//!   the VPC a private namespace named, and deleted with the namespace;
//! * the record sets under `<serviceName>.<namespaceName>.` are kept in step with this
//!   service's own state: one record set per registered instance per record the service's
//!   `DnsConfig` declares, carrying that record's TTL and the instance's address, written
//!   when an instance is registered, rewritten when the service's TTLs change and removed
//!   when the instance is deregistered.
//!
//! Route 53 is reached the way any emulated service reaches another: the AWS SDK driven
//! over the emulator's OWN in-process transport, through the operation callback
//! ([`smithy_server_runtime::OperationContext::callback`]), so no socket is opened, no name
//! is resolved and nothing outside this process is reachable from it.
//!
//! Every hop here FAILS OPEN, for the reason the other cross-service pre-flights in this
//! workspace fail open: an emulator that mounts no Route 53 at all, or a namespace name
//! Route 53 will not hold a zone for (Cloud Map's own `Name` pattern admits names that are
//! not domain names), must not make a Cloud Map call fail that the real service serves. What
//! is never done is reporting a zone that was not created: a namespace whose zone Route 53
//! declined holds NO hosted zone id, so every id a read reports resolves in Route 53.

use std::collections::{BTreeMap, BTreeSet};

use smithy_server_runtime::EmbeddedEmulator;

use super::{ComAmazonawsServicediscoveryRoute53autonamingV20170314State, ServiceRecord};

/// Route 53 is a global service: one endpoint, signed in one Region, whatever Region the
/// Cloud Map request that drives this hop was signed for. The Region a VPC lives in is
/// passed separately, where a zone association needs it.
const ROUTE53_SIGNING_REGION: &str = "us-east-1";

/// The instance attributes a record's value is read from, and the record types a service
/// declares: `super::dns_records` is the one home for both, so the keys this module reads an
/// instance's address out of are the keys `RegisterInstance` requires it to carry.
use super::dns_records::{ATTRIBUTE_CNAME, ATTRIBUTE_IPV4, ATTRIBUTE_IPV6, ATTRIBUTE_PORT};

/// The `RoutingPolicy` value under which each instance's record set is one answer among
/// several ("Route 53 returns the applicable value for up to eight instances").
const ROUTING_POLICY_MULTIVALUE: &str = "MULTIVALUE";

/// The `priority` and `weight` fields of an `SRV` record's data, which `DnsRecord$Type`
/// fixes: "The values of `priority` and `weight` are both set to `1` and can't be changed."
const SRV_PRIORITY_AND_WEIGHT: &str = "1 1";

/// The weight each record set of a `WEIGHTED` service carries. `DnsConfig$RoutingPolicy`
/// states it: "Currently, all records have the same weight, so you can't route more or less
/// traffic to any instances" — and the API has no per-instance weight to vary it by.
const WEIGHTED_RECORD_WEIGHT: i64 = 1;

/// How many record sets one page of a zone's listing asks for. The top of Route 53's own
/// `maxitems` for `ListResourceRecordSets`, so a zone is read in as few round trips as it
/// can be.
const RECORD_PAGE_SIZE: i32 = 300;

/// How many pages one reconciliation reads before it stops. The listing is bounded by the
/// zone, which is bounded by Route 53's own per-zone record-set quota; this is the backstop
/// that keeps one Cloud Map call's work finite whatever the zone holds.
const MAX_RECORD_PAGES: usize = 64;

/// How many changes one `ChangeResourceRecordSets` request carries. A reconciliation of a
/// service with many instances is split into batches of this size, so no request grows
/// without bound.
const MAX_CHANGES_PER_BATCH: usize = 100;

/// How many record sets one service publishes at once.
///
/// An EMULATOR ceiling on the WHOLE publication, not a quota AWS documents: the record sets
/// of a service are its instances (bounded by
/// [`super::MAX_INSTANCES_PER_SERVICE`]) times the records its `DnsConfig` declares, and the
/// second factor is a list a caller chose. One record set per instance per modelled
/// `RecordType`, plus the `SRV` service-hostname's own address records, is what a service
/// can legitimately publish, so this cap sits an order of magnitude above that and bounds
/// the collection a reconciliation builds whatever the request asked for. A publication at
/// the cap is TRUNCATED rather than refused: the Cloud Map call that triggered it is served
/// either way, and the cap is never reached by a service the API's own rules admit.
const MAX_PUBLISHED_RECORDS_PER_SERVICE: usize = 8_000;

/// One record set Cloud Map publishes for one instance, as this service decides it: the
/// name it answers for, its type, the TTL the service's `DnsConfig` declares for that type,
/// the identifier that tells it from its siblings and the record data read off the instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishedRecord {
    /// The DNS name, canonical (lowercase, trailing dot).
    pub name: String,
    /// An `RRType`: `A`, `AAAA`, `CNAME` or `SRV`.
    pub record_type: String,
    /// The TTL of the service's `DnsRecord` of this type.
    pub ttl: i64,
    /// The `SetIdentifier` that tells one instance's record set from its siblings' —
    /// the instance id — or `None` for a record set that is the only one at its name.
    pub set_identifier: Option<String>,
    /// The record data.
    pub value: String,
    /// `MultiValueAnswer`, for a service whose routing policy is `MULTIVALUE`.
    pub multi_value_answer: bool,
    /// `Weight`, for a service whose routing policy is `WEIGHTED`.
    pub weight: Option<i64>,
}

/// What identifies a record set in a zone: its name, its type and its set identifier —
/// the three members Route 53 itself keys a record set by.
type RecordKey = (String, String, Option<String>);

/// The zone-side state of ONE service: the zone its records live in, the name they answer
/// for, the names a sibling service in the same namespace owns, and the record sets this
/// service's own state says the zone should hold.
///
/// Computed under this service's state lock and published after it is released, because the
/// hop into Route 53 is `await`ed and the state guard is not held across it.
#[derive(Debug, Clone)]
pub struct ZonePublication {
    /// The hosted zone the namespace holds.
    pub zone_id: String,
    /// `<serviceName>.<namespaceName>.`, canonical.
    pub record_name: String,
    /// The same name for every OTHER service of the namespace, so a service whose name
    /// ends in another service's name never reconciles away its sibling's records.
    pub sibling_record_names: BTreeSet<String>,
    /// Every record set this service's state asks the zone to hold.
    pub desired: Vec<PublishedRecord>,
}

/// A DNS name in the one canonical form a zone's record sets are keyed by: lowercase, with
/// the trailing dot of a fully qualified name.
#[must_use]
pub fn canonical_name(name: &str) -> String {
    let trimmed = name.trim_end_matches('.').to_ascii_lowercase();
    format!("{trimmed}.")
}

/// The name the record sets of `service_name` in `namespace_name` answer for:
/// `<serviceName>.<namespaceName>.`, which is the DNS name a client resolves.
#[must_use]
pub fn service_record_name(service_name: &str, namespace_name: &str) -> String {
    canonical_name(&format!("{service_name}.{namespace_name}"))
}

/// The record sets one instance of `service` contributes, one per record its `DnsConfig`
/// declares.
///
/// The rule is `DnsRecord$Type`'s own: an `A` answers with `AWS_INSTANCE_IPV4`, an `AAAA`
/// with `AWS_INSTANCE_IPV6`, a `CNAME` with `AWS_INSTANCE_CNAME`, and an `SRV` with
/// `priority weight port service-hostname` — priority and weight both `1`, the port from
/// `AWS_INSTANCE_PORT` and the service-hostname the concatenation of the instance id, the
/// service name and the namespace name. An `SRV` service that was given an address also
/// gets the `A`/`AAAA` record at that service-hostname the same documentation describes.
/// A record whose attribute the registration did not carry contributes nothing: there is no
/// address to answer with, and nothing is invented for it.
#[must_use]
pub fn instance_records(
    service: &ServiceRecord,
    namespace_name: &str,
    instance_id: &str,
    attributes: &std::collections::HashMap<String, String>,
) -> Vec<PublishedRecord> {
    let Some(dns_config) = service.dns_config.as_ref() else {
        return Vec::new();
    };
    let multi_value_answer = dns_config
        .routing_policy
        .as_deref()
        .unwrap_or(super::DEFAULT_ROUTING_POLICY)
        == ROUTING_POLICY_MULTIVALUE;
    let weight = (!multi_value_answer).then_some(WEIGHTED_RECORD_WEIGHT);
    let record_name = service_record_name(&service.name, namespace_name);
    // The `service-hostname` of an `SRV` record's data: "a concatenation of … the value
    // that you specify for `InstanceId` …, the name of the service [and] the name of the
    // namespace", as that documentation spells it.
    let instance_host = format!("{instance_id}.{}.{namespace_name}", service.name);
    let instance_record_name = canonical_name(&instance_host);
    let attribute = |key: &str| attributes.get(key).filter(|value| !value.is_empty());

    let mut records = Vec::new();
    for declared in &dns_config.dns_records {
        let ttl = declared.ttl;
        match declared.r#type.as_str() {
            "A" => {
                if let Some(address) = attribute(ATTRIBUTE_IPV4) {
                    records.push(PublishedRecord {
                        name: record_name.clone(),
                        record_type: "A".to_string(),
                        ttl,
                        set_identifier: Some(instance_id.to_string()),
                        value: address.clone(),
                        multi_value_answer,
                        weight,
                    });
                }
            }
            "AAAA" => {
                if let Some(address) = attribute(ATTRIBUTE_IPV6) {
                    records.push(PublishedRecord {
                        name: record_name.clone(),
                        record_type: "AAAA".to_string(),
                        ttl,
                        set_identifier: Some(instance_id.to_string()),
                        value: address.clone(),
                        multi_value_answer,
                        weight,
                    });
                }
            }
            "CNAME" => {
                if let Some(target) = attribute(ATTRIBUTE_CNAME) {
                    records.push(PublishedRecord {
                        name: record_name.clone(),
                        record_type: "CNAME".to_string(),
                        ttl,
                        set_identifier: Some(instance_id.to_string()),
                        // "the domain name that you want Route 53 to return in response
                        // to DNS queries" — the instance's own value, answered as it was
                        // registered.
                        value: target.clone(),
                        multi_value_answer,
                        weight,
                    });
                }
            }
            "SRV" => {
                if let Some(port) = attribute(ATTRIBUTE_PORT) {
                    records.push(PublishedRecord {
                        name: record_name.clone(),
                        record_type: "SRV".to_string(),
                        ttl,
                        set_identifier: Some(instance_id.to_string()),
                        value: format!("{SRV_PRIORITY_AND_WEIGHT} {port} {instance_host}"),
                        multi_value_answer,
                        weight,
                    });
                }
                // "If you specify values for AWS_INSTANCE_IPV4, AWS_INSTANCE_IPV6, or
                // both in the RegisterInstance request, Cloud Map automatically creates
                // A and/or AAAA records that have the same name as the value of
                // service-hostname in the SRV record." That name holds one instance's
                // address alone, so each is the single record set at its own name and
                // carries no routing policy.
                for (key, record_type) in [(ATTRIBUTE_IPV4, "A"), (ATTRIBUTE_IPV6, "AAAA")] {
                    if let Some(address) = attribute(key) {
                        records.push(PublishedRecord {
                            name: instance_record_name.clone(),
                            record_type: record_type.to_string(),
                            ttl,
                            set_identifier: None,
                            value: address.clone(),
                            multi_value_answer: false,
                            weight: None,
                        });
                    }
                }
            }
            // `DnsRecord$Type` targets the model's `RecordType` enum, whose value set is
            // exactly the four types above, so nothing else reaches this.
            _ => {}
        }
    }
    records
}

/// What the zone should hold for `service`, and what tells those record sets from a
/// sibling's — or `None` when the service registers no DNS records at all (its namespace
/// holds no hosted zone, or it has no `DnsConfig`).
#[must_use]
pub fn publication(
    state: &ComAmazonawsServicediscoveryRoute53autonamingV20170314State,
    service: &ServiceRecord,
) -> Option<ZonePublication> {
    let namespace = state.namespaces.get(&service.namespace_id)?;
    let zone_id = namespace.hosted_zone_id.clone()?;
    let record_name = service_record_name(&service.name, &namespace.name);
    let sibling_record_names = state
        .services
        .values()
        .filter(|sibling| {
            sibling.namespace_id == service.namespace_id && sibling.id != service.id
        })
        .map(|sibling| service_record_name(&sibling.name, &namespace.name))
        .collect();
    // The record sets this service's state asks for, keyed as Route 53 keys a record set so
    // that two declarations of one record type — which the API's own rule forbids and
    // `CreateService`/`UpdateService` refuse — collapse onto the one record set they name
    // rather than multiplying the collection, and BOUNDED by
    // [`MAX_PUBLISHED_RECORDS_PER_SERVICE`], so the product of the instances stored and the
    // records a request declared cannot size this allocation.
    let mut desired: BTreeMap<RecordKey, PublishedRecord> = BTreeMap::new();
    for instance in service.instances.values() {
        for record in instance_records(
            service,
            &namespace.name,
            &instance.instance_id,
            &instance.attributes,
        ) {
            if desired.len() >= MAX_PUBLISHED_RECORDS_PER_SERVICE
                && !desired.contains_key(&desired_key(&record))
            {
                break;
            }
            desired.insert(desired_key(&record), record);
        }
    }
    let desired = desired.into_values().collect();
    Some(ZonePublication {
        zone_id,
        record_name,
        sibling_record_names,
        desired,
    })
}

/// The in-process Route 53 client this service drives.
///
/// The transport is the emulator's own, so there is nothing to retry, time out or watch for
/// a stalled stream — each of those would need a sleep implementation this client is not
/// given — and the credentials the callback hands over are the emulator's own, used
/// directly: the caller's `Authorization` header is never forwarded.
fn route53_client(callback: &EmbeddedEmulator) -> ::aws_sdk_route53::Client {
    let config = ::aws_sdk_route53::Config::builder()
        .behavior_version(::aws_sdk_route53::config::BehaviorVersion::latest())
        .http_client(callback.http_client())
        .credentials_provider(callback.aws_credentials_provider())
        .region(::aws_sdk_route53::config::Region::new(
            ROUTE53_SIGNING_REGION.to_string(),
        ))
        .endpoint_url("https://route53.amazonaws.com")
        .retry_config(::aws_sdk_route53::config::retry::RetryConfig::disabled())
        .timeout_config(::aws_sdk_route53::config::timeout::TimeoutConfig::disabled())
        .identity_cache(::aws_sdk_route53::config::IdentityCache::no_cache())
        .stalled_stream_protection(
            ::aws_sdk_route53::config::StalledStreamProtectionConfig::disabled(),
        )
        .build();
    ::aws_sdk_route53::Client::from_conf(config)
}

/// A hosted zone id as Cloud Map reports it: Route 53 answers a zone's `Id` in its
/// `/hostedzone/<id>` resource form, while `DnsProperties.HostedZoneId` is the bare id —
/// the form every Route 53 call also accepts back.
fn bare_zone_id(id: &str) -> String {
    id.rsplit('/').next().unwrap_or(id).to_string()
}

/// Create the hosted zone a DNS namespace is backed by, and answer its id.
///
/// `vpc` is the VPC a PRIVATE namespace is created with — naming one is what makes the
/// zone private and associates it, which is the whole of a private namespace's visibility
/// ("visible only inside a specified Amazon VPC") — and `vpc_region` the Region that VPC was
/// created in. A public namespace names none and gets a public zone.
///
/// `None` when Route 53 did not create a zone, for any reason at all; the caller then
/// reports no hosted zone id rather than one that resolves to nothing.
pub async fn create_namespace_zone<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &smithy_server_runtime::OperationContext<'_>,
    zone: NamespaceZone<'_>,
) -> Option<String> {
    let client = route53_client(ctx.callback());
    let mut request = client
        .create_hosted_zone()
        .name(zone.namespace_name)
        // The zone belongs to this namespace and to no other call, so the namespace's own
        // id is what identifies the request that created it.
        .caller_reference(format!("servicediscovery-{}", zone.namespace_id))
        .hosted_zone_config(
            ::aws_sdk_route53::types::HostedZoneConfig::builder()
                .private_zone(zone.vpc.is_some())
                .build(),
        );
    if let Some(vpc_id) = zone.vpc {
        request = request.vpc(
            ::aws_sdk_route53::types::Vpc::builder()
                .vpc_id(vpc_id)
                .vpc_region(::aws_sdk_route53::types::VpcRegion::from(zone.vpc_region))
                .build(),
        );
    }
    let created = match request.send().await {
        Ok(created) => created.hosted_zone().map(|zone| bare_zone_id(zone.id())),
        Err(error) => {
            tracing::debug!(
                namespace = zone.namespace_id,
                error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                "Route 53 created no hosted zone for this namespace"
            );
            None
        }
    }?;
    // The zone was created BY this service, for the namespace it belongs to, and this
    // service manages it from here on — which is what Route 53 reports as the zone's
    // `LinkedService`. Recorded on the zone whose id Route 53 just minted, so nothing a
    // request carries decides it.
    ::service_com_amazonaws_route53_awsdnsv20130401::state::managed_zone::link_zone_to_service(
        engine,
        &created,
        SERVICE_PRINCIPAL,
        zone.namespace_arn,
    );
    Some(created)
}

/// The hosted zone ONE namespace asks Route 53 for: the namespace it belongs to, and the
/// VPC a private namespace is visible inside.
#[derive(Debug, Clone, Copy)]
pub struct NamespaceZone<'a> {
    /// The namespace's `Name`, which is the zone's name.
    pub namespace_name: &'a str,
    /// The namespace's `Id`.
    pub namespace_id: &'a str,
    /// The namespace's ARN: the resource the create is made on behalf of, which Route 53
    /// reports as the zone's `LinkedService` description.
    pub namespace_arn: &'a str,
    /// The VPC a PRIVATE namespace is created with — naming one is what makes the zone
    /// private and associates it, which is the whole of a private namespace's visibility
    /// ("visible only inside a specified Amazon VPC"). A public namespace names none and
    /// gets a public zone.
    pub vpc: Option<&'a str>,
    /// The Region that VPC was created in.
    pub vpc_region: &'a str,
}

/// This service's own service principal, which is what it creates and manages a DNS
/// namespace's hosted zone as.
const SERVICE_PRINCIPAL: &str = "servicediscovery.amazonaws.com";

/// Delete the hosted zone a namespace held, so the zone goes away with the namespace it
/// belonged to.
pub async fn delete_namespace_zone(callback: &EmbeddedEmulator, zone_id: &str) {
    if let Err(error) = route53_client(callback)
        .delete_hosted_zone()
        .id(zone_id)
        .send()
        .await
    {
        tracing::debug!(
            zone = zone_id,
            error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
            "Route 53 did not delete the hosted zone of this namespace"
        );
    }
}

/// Make the zone hold exactly the record sets `publication` asks for: write the ones it is
/// missing or holds differently, and remove the ones of this service it should no longer
/// hold.
///
/// This ONE reconciliation serves every write path — a registration adds record sets, a
/// deregistration leaves them out, a TTL change rewrites them, a record type the service no
/// longer declares disappears — because what is published is always recomputed from this
/// service's own state rather than from the call that changed it.
pub async fn publish(callback: &EmbeddedEmulator, publication: &ZonePublication) {
    let client = route53_client(callback);
    let Some(held) = owned_record_sets(&client, publication).await else {
        return;
    };

    let mut changes = Vec::new();
    for record in &publication.desired {
        let key = desired_key(record);
        if held.get(&key).is_some_and(|current| holds(current, record)) {
            continue;
        }
        if let Some(change) = upsert_change(record) {
            changes.push(change);
        }
    }
    let desired: BTreeSet<RecordKey> = publication.desired.iter().map(desired_key).collect();
    for (key, current) in &held {
        if desired.contains(key) {
            continue;
        }
        if let Some(change) = delete_change(current) {
            changes.push(change);
        }
    }
    if changes.is_empty() {
        return;
    }

    for batch in changes.chunks(MAX_CHANGES_PER_BATCH) {
        let batch = match ::aws_sdk_route53::types::ChangeBatch::builder()
            .set_changes(Some(batch.to_vec()))
            .build()
        {
            Ok(batch) => batch,
            Err(error) => {
                tracing::debug!(error = %error, "the record-set changes did not form a batch");
                return;
            }
        };
        if let Err(error) = client
            .change_resource_record_sets()
            .hosted_zone_id(&publication.zone_id)
            .change_batch(batch)
            .send()
            .await
        {
            tracing::debug!(
                zone = publication.zone_id,
                record_name = publication.record_name,
                error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                "Route 53 did not apply the record sets of this service"
            );
            return;
        }
    }
}

/// The record sets the zone currently holds FOR THIS SERVICE, keyed as Route 53 keys them.
///
/// A record set belongs to the service when it answers for the service's own name, or for a
/// single further label under it — the service-hostname an `SRV` record's `A`/`AAAA` record
/// carries. A name another service of the same namespace owns is never claimed, so a
/// service whose name ends with a sibling's name reconciles only its own records.
///
/// `None` when the zone could not be read at all, which is also the answer when no Route 53
/// is mounted: nothing is then written either, since the state to reconcile against is
/// unknown.
async fn owned_record_sets(
    client: &::aws_sdk_route53::Client,
    publication: &ZonePublication,
) -> Option<BTreeMap<RecordKey, ::aws_sdk_route53::types::ResourceRecordSet>> {
    let mut held = BTreeMap::new();
    let mut start: Option<(String, ::aws_sdk_route53::types::RrType, Option<String>)> = None;
    for _ in 0..MAX_RECORD_PAGES {
        let mut request = client
            .list_resource_record_sets()
            .hosted_zone_id(&publication.zone_id)
            .max_items(RECORD_PAGE_SIZE);
        if let Some((name, record_type, identifier)) = start.clone() {
            request = request
                .start_record_name(name)
                .start_record_type(record_type)
                .set_start_record_identifier(identifier);
        }
        let page = match request.send().await {
            Ok(page) => page,
            Err(error) => {
                tracing::debug!(
                    zone = publication.zone_id,
                    error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                    "the hosted zone of this namespace could not be read"
                );
                return None;
            }
        };
        for record in page.resource_record_sets() {
            if !owns(publication, record.name()) {
                continue;
            }
            held.insert(
                (
                    canonical_name(record.name()),
                    record.r#type().as_str().to_string(),
                    record.set_identifier().map(str::to_string),
                ),
                record.clone(),
            );
        }
        let next = page.next_record_name().map(str::to_string);
        match (page.is_truncated(), next, page.next_record_type()) {
            (true, Some(name), Some(record_type)) => {
                start = Some((
                    name,
                    record_type.clone(),
                    page.next_record_identifier().map(str::to_string),
                ));
            }
            _ => return Some(held),
        }
    }
    Some(held)
}

/// Whether a record set the zone holds answers for a name this service owns.
fn owns(publication: &ZonePublication, name: &str) -> bool {
    let name = canonical_name(name);
    if name == publication.record_name {
        return true;
    }
    let Some(label) = name.strip_suffix(&format!(".{}", publication.record_name)) else {
        return false;
    };
    !label.is_empty() && !label.contains('.') && !publication.sibling_record_names.contains(&name)
}

/// How a record set this service wants published is keyed.
fn desired_key(record: &PublishedRecord) -> RecordKey {
    (
        record.name.clone(),
        record.record_type.clone(),
        record.set_identifier.clone(),
    )
}

/// Whether the record set the zone holds is already the one this service wants published:
/// the same record data, the same TTL and the same routing policy. Anything else is
/// rewritten.
fn holds(
    current: &::aws_sdk_route53::types::ResourceRecordSet,
    wanted: &PublishedRecord,
) -> bool {
    let values: Vec<&str> = current
        .resource_records()
        .iter()
        .map(::aws_sdk_route53::types::ResourceRecord::value)
        .collect();
    values == [wanted.value.as_str()]
        && current.ttl() == Some(wanted.ttl)
        && current.multi_value_answer() == wanted.multi_value_answer.then_some(true)
        && current.weight() == wanted.weight
}

/// The `UPSERT` that writes one record set: "If a resource set doesn't exist, Route 53
/// creates it. If a resource set exists Route 53 updates it with the values in the request."
fn upsert_change(record: &PublishedRecord) -> Option<::aws_sdk_route53::types::Change> {
    let value = ::aws_sdk_route53::types::ResourceRecord::builder()
        .value(&record.value)
        .build()
        .ok()?;
    let mut record_set = ::aws_sdk_route53::types::ResourceRecordSet::builder()
        .name(&record.name)
        .r#type(::aws_sdk_route53::types::RrType::from(
            record.record_type.as_str(),
        ))
        .ttl(record.ttl)
        .resource_records(value);
    if let Some(identifier) = record.set_identifier.as_deref() {
        record_set = record_set.set_identifier(identifier);
    }
    if record.multi_value_answer {
        record_set = record_set.multi_value_answer(true);
    }
    if let Some(weight) = record.weight {
        record_set = record_set.weight(weight);
    }
    ::aws_sdk_route53::types::Change::builder()
        .action(::aws_sdk_route53::types::ChangeAction::Upsert)
        .resource_record_set(record_set.build().ok()?)
        .build()
        .ok()
}

/// The `DELETE` that removes one record set. "To delete a resource record set, you must
/// specify all the same values that you specified when you created it", so the change
/// carries the record set the zone itself answered with.
fn delete_change(
    current: &::aws_sdk_route53::types::ResourceRecordSet,
) -> Option<::aws_sdk_route53::types::Change> {
    ::aws_sdk_route53::types::Change::builder()
        .action(::aws_sdk_route53::types::ChangeAction::Delete)
        .resource_record_set(current.clone())
        .build()
        .ok()
}
