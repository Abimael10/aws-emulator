// SPDX-License-Identifier: CC0-1.0

//! What `ChangeResourceRecordSets` accepts INSIDE a change batch whose request
//! shape the model already admits.
//!
//! Five rules are pinned here, each asserted over a TABLE that spans the class
//! it generalises over rather than over the one request that exposed it:
//!
//! 1. A record set's record data is parsed against the syntax of the TYPE it
//!    was submitted for — an address, a character string, a domain name — and a
//!    value that does not parse refuses the batch (`InvalidChangeBatch`),
//!    quoting the value.
//! 2. A record set whose routing policy is not simple is one of several at its
//!    name and type, so it must carry the `SetIdentifier` that tells it from
//!    its siblings; a change that omits it is a REQUEST fault
//!    (`InvalidInput`).
//! 3. A record set answers from EXACTLY ONE source of record data: its own
//!    `TTL` and `ResourceRecords`, an `AliasTarget`, or the traffic policy
//!    instance that generated it. Naming more than one, or none, is the same
//!    request fault.
//! 4. A `CNAME` says its name IS another name, so it stands neither at a zone's
//!    apex nor beside a record of another type at the same name — symmetrically,
//!    whichever of the two the batch writes.
//! 5. A latency record set routes by an Amazon EC2 Region, so it needs both the
//!    identifier that tells it from its siblings and a `Region` that names a
//!    region: two requirements refused by two different stages.
//! 6. An alias routes to the resource its target names, so the target has to lie
//!    within the hosted zone it names — checkable for the target zones whose id
//!    the owning service fixes, and refused (`InvalidChangeBatch`) when it does
//!    not.
//! 7. That same rule over every zone whose contents are decidable here: the
//!    target zones AWS publishes ONE ID PER REGION for (the Amazon S3 website
//!    endpoints), read from both sides of the pair, and a hosted zone of the
//!    account's own.
//!
//! Every test drives the service through the real router, using the SAME
//! harness `hosted_zone_records` uses, so what is asserted is the HTTP/restXml
//! behaviour a Route 53 client sees.

use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_route53_awsdnsv20130401::state::alias_target_zones::{
    s3_website_domains, S3_WEBSITE_TARGET_ZONE_REGIONS,
};

use crate::hosted_zone_records::{
    call, change_batch_body, create_zone_ok, escape_xml, router, Reply,
};

/// The zone every table below works inside.
const ZONE: &str = "example.com";

/// The zone's apex, in the canonical form a response reports it in.
const APEX: &str = "example.com.";

// ---------------------------------------------------------------------------
// Request builders
// ---------------------------------------------------------------------------

/// One record set, as the members a `<Change>` carries for it.
///
/// `pub(crate)` so a sibling suite in this test target writes its changes with
/// the SAME builder rather than growing a second copy of it.
#[derive(Debug, Clone, Default)]
pub(crate) struct Rrset {
    name: String,
    rr_type: String,
    set_identifier: Option<String>,
    /// Already-rendered routing-policy element(s), in model order.
    routing: String,
    ttl: Option<i64>,
    values: Vec<String>,
    /// The `(HostedZoneId, DNSName)` of an `AliasTarget`.
    alias: Option<(String, String)>,
}

impl Rrset {
    pub(crate) fn new(name: &str, rr_type: &str) -> Self {
        Self {
            name: name.to_string(),
            rr_type: rr_type.to_string(),
            ..Self::default()
        }
    }

    pub(crate) fn set_identifier(mut self, identifier: &str) -> Self {
        self.set_identifier = Some(identifier.to_string());
        self
    }

    pub(crate) fn routing(mut self, elements: &str) -> Self {
        self.routing = elements.to_string();
        self
    }

    pub(crate) fn ttl(mut self, ttl: i64) -> Self {
        self.ttl = Some(ttl);
        self
    }

    pub(crate) fn values(mut self, values: &[&str]) -> Self {
        self.values = values.iter().map(|value| (*value).to_string()).collect();
        self
    }

    pub(crate) fn alias(mut self, hosted_zone_id: &str, dns_name: &str) -> Self {
        self.alias = Some((hosted_zone_id.to_string(), dns_name.to_string()));
        self
    }

    /// The name in the canonical form a refusal and a listing report it in.
    pub(crate) fn canonical(&self) -> String {
        format!("{}.", self.name.trim_end_matches('.'))
    }
}

/// A `<Change>` element over one record set.
///
/// The members appear in the order the model declares them (`Name`, `Type`,
/// `SetIdentifier`, the routing policy, `TTL`, `ResourceRecords`,
/// `AliasTarget`), which is the order the operation's documentation requires of
/// the XML elements.
pub(crate) fn change(action: &str, record: &Rrset) -> String {
    let set_identifier = record.set_identifier.as_deref().map_or_else(
        String::new,
        |identifier| format!("<SetIdentifier>{}</SetIdentifier>", escape_xml(identifier)),
    );
    let ttl = record
        .ttl
        .map_or_else(String::new, |ttl| format!("<TTL>{ttl}</TTL>"));
    let records = if record.values.is_empty() {
        String::new()
    } else {
        let members: String = record
            .values
            .iter()
            .map(|value| {
                format!(
                    "<ResourceRecord><Value>{}</Value></ResourceRecord>",
                    escape_xml(value)
                )
            })
            .collect();
        format!("<ResourceRecords>{members}</ResourceRecords>")
    };
    let alias = record.alias.as_ref().map_or_else(String::new, |(zone, dns)| {
        format!(
            "<AliasTarget><HostedZoneId>{}</HostedZoneId><DNSName>{}</DNSName>\
             <EvaluateTargetHealth>false</EvaluateTargetHealth></AliasTarget>",
            escape_xml(zone),
            escape_xml(dns)
        )
    });
    format!(
        "<Change><Action>{action}</Action><ResourceRecordSet><Name>{}</Name>\
         <Type>{}</Type>{set_identifier}{}{ttl}{records}{alias}</ResourceRecordSet></Change>",
        escape_xml(&record.name),
        record.rr_type,
        record.routing
    )
}

/// Submit already-rendered `<Change>` elements as one batch.
pub(crate) async fn submit(router: &MultiServiceRouter, zone_id: &str, changes: &str) -> Reply {
    call(
        router,
        "POST",
        &format!("/2013-04-01/hostedzone/{zone_id}/rrset"),
        &change_batch_body(changes, None),
    )
    .await
}

/// Submit one change as a batch of its own.
async fn submit_one(
    router: &MultiServiceRouter,
    zone_id: &str,
    action: &str,
    record: &Rrset,
) -> Reply {
    submit(router, zone_id, &change(action, record)).await
}

/// List the zone's record sets.
pub(crate) async fn list(router: &MultiServiceRouter, zone_id: &str) -> Reply {
    call(
        router,
        "GET",
        &format!("/2013-04-01/hostedzone/{zone_id}/rrset"),
        "",
    )
    .await
}

/// The `Error/Message` of a refusal, or the whole body when there is none.
pub(crate) fn message(reply: &Reply) -> String {
    reply
        .first("Error/Message")
        .unwrap_or(&reply.body)
        .to_string()
}

/// Whether the zone holds a record set at `name`.
async fn holds(router: &MultiServiceRouter, zone_id: &str, name: &str) -> bool {
    list(router, zone_id)
        .await
        .all("ResourceRecordSet/Name")
        .contains(&name)
}

/// Assert one refusal: the status, the error code, the whole message, and that
/// the batch left the zone alone.
async fn assert_refused(
    router: &MultiServiceRouter,
    zone_id: &str,
    case: &str,
    reply: &Reply,
    code: &str,
    expected: &str,
    untouched: &str,
) {
    assert_eq!(
        reply.status, 400,
        "case {case:?}: expected 400, got {} with body {}",
        reply.status, reply.body
    );
    assert_eq!(
        reply.error_code(),
        code,
        "case {case:?}: expected {code}, got {}",
        reply.body
    );
    let actual = message(reply);
    assert_eq!(
        actual, expected,
        "case {case:?}: expected the refusal {expected:?}, got {actual:?}"
    );
    assert!(
        !holds(router, zone_id, untouched).await,
        "case {case:?}: a refused batch must land nothing, but {untouched} is listed"
    );
}

// ---------------------------------------------------------------------------
// 1. Record data is parsed against the syntax of its type
// ---------------------------------------------------------------------------

/// One record data value submitted for one record type, and the syntax rule it
/// breaks (`None` when it breaks none).
struct ValueCase {
    label: &'static str,
    rr_type: &'static str,
    value: String,
    fault: Option<&'static str>,
}

/// A label of 64 octets — one past the 63 the DNS domain-name format allows.
fn overlong_label() -> String {
    "a".repeat(64)
}

#[tokio::test]
async fn record_data_is_parsed_against_the_syntax_of_its_type() {
    // The rule: a submitted value is parsed against the syntax of the record
    // type it was submitted for, and a value that does not parse refuses the
    // batch with `InvalidChangeBatch`, naming the rule and quoting the value.
    // The rows span the syntax CLASSES the rule covers — an IPv4 address, an
    // IPv6 address, a character string, a domain name — the ways each can fail,
    // and the values of those same types that DO parse. The record types come
    // from the modelled `RRType` value set
    // (`smithy_query(query="shape",shape="RRType")`): SOA, A, TXT, NS, CNAME,
    // MX, NAPTR, PTR, SRV, SPF, AAAA, CAA, DS, TLSA, SSHFP, SVCB, HTTPS.
    const IPV4: &str = "ARRDATAIllegalIPv4Address (Value is not a valid IPv4 address)";
    const IPV6: &str = "AAAARRDATAIllegalIPv6Address (Value is not a valid IPv6 address)";
    const CHARACTER_STRING: &str =
        "InvalidCharacterString (Value should be enclosed in quotation marks)";
    const EMPTY_LABEL: &str = "DomainLabelEmpty (Domain label is empty)";
    const LABEL_TOO_LONG: &str = "DomainLabelTooLong (Domain label is too long)";

    let cases = vec![
        ValueCase {
            label: "an A record holding text",
            rr_type: "A",
            value: "not-an-ip-address".to_string(),
            fault: Some(IPV4),
        },
        ValueCase {
            label: "an A record whose last octet is out of range",
            rr_type: "A",
            value: "192.0.2.300".to_string(),
            fault: Some(IPV4),
        },
        ValueCase {
            label: "an A record with only three octets",
            rr_type: "A",
            value: "192.0.2".to_string(),
            fault: Some(IPV4),
        },
        ValueCase {
            label: "an A record holding an IPv6 address",
            rr_type: "A",
            value: "2001:db8::1".to_string(),
            fault: Some(IPV4),
        },
        ValueCase {
            label: "an AAAA record with a non-hex group",
            rr_type: "AAAA",
            value: "2001:db8::zz".to_string(),
            fault: Some(IPV6),
        },
        ValueCase {
            label: "an AAAA record holding an IPv4 address",
            rr_type: "AAAA",
            value: "192.0.2.1".to_string(),
            fault: Some(IPV6),
        },
        ValueCase {
            label: "a TXT record whose unquoted value carries spaces",
            rr_type: "TXT",
            value: "v=spf1 include:example.net ~all".to_string(),
            fault: Some(CHARACTER_STRING),
        },
        ValueCase {
            label: "a TXT record whose unquoted value carries no space",
            rr_type: "TXT",
            value: "unquoted".to_string(),
            fault: Some(CHARACTER_STRING),
        },
        ValueCase {
            label: "a TXT record whose quotation mark is never closed",
            rr_type: "TXT",
            value: "\"unterminated".to_string(),
            fault: Some(CHARACTER_STRING),
        },
        ValueCase {
            label: "an SPF record with an unquoted value",
            rr_type: "SPF",
            value: "v=spf1 -all".to_string(),
            fault: Some(CHARACTER_STRING),
        },
        ValueCase {
            label: "a CNAME whose target has an empty label",
            rr_type: "CNAME",
            value: "target..example.net.".to_string(),
            fault: Some(EMPTY_LABEL),
        },
        ValueCase {
            label: "a CNAME with no target at all",
            rr_type: "CNAME",
            value: String::new(),
            fault: Some(EMPTY_LABEL),
        },
        ValueCase {
            label: "a CNAME whose target has a 64-octet label",
            rr_type: "CNAME",
            value: format!("{}.example.net.", overlong_label()),
            fault: Some(LABEL_TOO_LONG),
        },
        ValueCase {
            label: "an NS record whose target has an empty label",
            rr_type: "NS",
            value: "ns1..example.net.".to_string(),
            fault: Some(EMPTY_LABEL),
        },
        ValueCase {
            label: "a PTR record whose target has a 64-octet label",
            rr_type: "PTR",
            value: format!("{}.example.net.", overlong_label()),
            fault: Some(LABEL_TOO_LONG),
        },
        ValueCase {
            label: "an A record holding an address",
            rr_type: "A",
            value: "192.0.2.1".to_string(),
            fault: None,
        },
        ValueCase {
            label: "an AAAA record holding an address",
            rr_type: "AAAA",
            value: "2001:db8::1".to_string(),
            fault: None,
        },
        ValueCase {
            label: "a TXT record holding one quoted string",
            rr_type: "TXT",
            value: "\"v=spf1 include:example.net ~all\"".to_string(),
            fault: None,
        },
        ValueCase {
            label: "a TXT record holding two quoted strings",
            rr_type: "TXT",
            value: "\"first string\" \"second string\"".to_string(),
            fault: None,
        },
        ValueCase {
            label: "a CNAME holding a domain name",
            rr_type: "CNAME",
            value: "target.example.net.".to_string(),
            fault: None,
        },
        ValueCase {
            label: "an NS record holding a domain name",
            rr_type: "NS",
            value: "ns1.example.net.".to_string(),
            fault: None,
        },
        ValueCase {
            // A type whose record data is a COMPOUND of several fields is not
            // judged value by value, so its data passes this stage.
            label: "an MX record holding a preference and an exchange",
            rr_type: "MX",
            value: "10 mail.example.net.".to_string(),
            fault: None,
        },
    ];

    let router = router();
    let zone_id = create_zone_ok(&router, ZONE, "ref-rdata-syntax").await;

    for (index, case) in cases.iter().enumerate() {
        let record = Rrset::new(&format!("v{index}.{ZONE}"), case.rr_type)
            .ttl(300)
            .values(&[case.value.as_str()]);
        let reply = submit_one(&router, &zone_id, "CREATE", &record).await;
        let canonical = record.canonical();
        match case.fault {
            Some(fault) => {
                let expected = format!(
                    "[Invalid Resource Record: 'FATAL problem: {fault} encountered with '{}'']",
                    case.value
                );
                assert_refused(
                    &router,
                    &zone_id,
                    case.label,
                    &reply,
                    "InvalidChangeBatch",
                    &expected,
                    &canonical,
                )
                .await;
                // The problem is reported ONCE: the flat message asserted above
                // is the whole report, and the modelled list member is not on
                // the wire beside it.
                assert!(
                    reply.all("messages/member").is_empty(),
                    "case {:?}: expected no `messages` entries beside the flat message, got {}",
                    case.label,
                    reply.body
                );
            }
            None => {
                assert_eq!(
                    reply.status, 200,
                    "case {:?}: expected 200, got {} with body {}",
                    case.label, reply.status, reply.body
                );
                // Round trip: the accepted value is readable back.
                let listed = list(&router, &zone_id).await;
                assert!(
                    listed.all("ResourceRecordSet/Name").contains(&&*canonical),
                    "case {:?}: expected {canonical} in the listing, got {}",
                    case.label,
                    listed.body
                );
                assert!(
                    listed
                        .all("ResourceRecord/Value")
                        .contains(&case.value.as_str()),
                    "case {:?}: expected the stored value {:?} in the listing, got {:?}",
                    case.label,
                    case.value,
                    listed.all("ResourceRecord/Value")
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 2. A routing policy other than simple needs a SetIdentifier
// ---------------------------------------------------------------------------

/// One routing policy, as the member that selects it.
struct PolicyCase {
    label: &'static str,
    /// The element(s) that give the record set this policy.
    elements: &'static str,
}

/// Every member of `ResourceRecordSet` that gives a record set a routing policy
/// other than simple, from the shape's own member list
/// (`smithy_query(query="shape",shape="ResourceRecordSet")`): `Weight`,
/// `Region`, `GeoLocation`, `Failover`, `MultiValueAnswer`,
/// `CidrRoutingConfig`, `GeoProximityLocation`.
const POLICIES: &[PolicyCase] = &[
    PolicyCase {
        label: "a weighted record set",
        elements: "<Weight>10</Weight>",
    },
    PolicyCase {
        label: "a latency record set",
        elements: "<Region>us-west-2</Region>",
    },
    PolicyCase {
        label: "a geolocation record set",
        elements: "<GeoLocation><CountryCode>US</CountryCode></GeoLocation>",
    },
    PolicyCase {
        label: "a failover record set",
        elements: "<Failover>PRIMARY</Failover>",
    },
    PolicyCase {
        label: "a multivalue answer record set",
        elements: "<MultiValueAnswer>true</MultiValueAnswer>",
    },
    PolicyCase {
        label: "an IP-based record set",
        elements: "<CidrRoutingConfig>\
                   <CollectionId>0123abcd-4567-89ef-0123-456789abcdef</CollectionId>\
                   <LocationName>office</LocationName></CidrRoutingConfig>",
    },
    PolicyCase {
        label: "a geoproximity record set",
        elements: "<GeoProximityLocation><AWSRegion>us-west-2</AWSRegion></GeoProximityLocation>",
    },
];

#[tokio::test]
async fn a_routing_policy_other_than_simple_requires_a_set_identifier() {
    // The rule: `SetIdentifier` belongs to "resource record sets that have a
    // routing policy other than simple" and "differentiates among multiple
    // resource record sets that have the same combination of name and type", so
    // a change that selects such a policy without it is refused as a REQUEST
    // fault — `InvalidInput`, naming the missing field and the change — while
    // the same change carrying one is applied. The rows are every policy member
    // the shape declares, each tried as a CREATE and as an UPSERT.
    let router = router();
    let zone_id = create_zone_ok(&router, ZONE, "ref-set-identifier").await;

    for (index, policy) in POLICIES.iter().enumerate() {
        for action in ["CREATE", "UPSERT"] {
            let case = format!("{} ({action})", policy.label);
            // A name of its own per row AND action, so the record set an
            // accepted row lands never stands in for the one a refused row
            // must not land.
            let lonely = Rrset::new(
                &format!("p{index}-{}.{ZONE}", action.to_lowercase()),
                "A",
            )
            .routing(policy.elements)
            .ttl(60)
            .values(&["192.0.2.10"]);
            let reply = submit_one(&router, &zone_id, action, &lonely).await;
            let expected = format!(
                "Invalid request: Missing field 'SetIdentifier' in Change with [Action={action}, \
                 Name={}, Type=A, SetIdentifier=null]",
                lonely.canonical()
            );
            assert_refused(
                &router,
                &zone_id,
                &case,
                &reply,
                "InvalidInput",
                &expected,
                &lonely.canonical(),
            )
            .await;

            // The same change, with the identifier the policy needs, lands.
            let identified = lonely.clone().set_identifier("blue");
            let accepted = submit_one(&router, &zone_id, action, &identified).await;
            assert_eq!(
                accepted.status, 200,
                "case {case:?}: with a SetIdentifier the change must be applied, got {} with \
                 body {}",
                accepted.status, accepted.body
            );
            assert!(
                holds(&router, &zone_id, &identified.canonical()).await,
                "case {case:?}: the applied record set must be listed"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// 3. Exactly one source of record data
// ---------------------------------------------------------------------------

/// One record set shape, and how many sources of record data it names.
struct SourceCase {
    label: &'static str,
    /// The members under test, applied to a per-row record set.
    shape: fn(Rrset) -> Rrset,
    /// `Some("more than one")` / `Some("none")` for a refusal, `None` when the
    /// shape names exactly one source.
    found: Option<&'static str>,
}

/// The hosted zone id of the CloudFront distribution an alias row routes to,
/// from `AliasTarget$HostedZoneId`'s own documentation ("CloudFront
/// distribution: Specify `Z2FDTNDATAQYW2`").
const CLOUDFRONT_ZONE: &str = "Z2FDTNDATAQYW2";

#[tokio::test]
async fn a_record_set_names_exactly_one_source_of_record_data() {
    // The rule: a record set answers from exactly one source of record data —
    // its own `TTL` with `ResourceRecords`, an `AliasTarget` ("If you're
    // creating an alias resource record set, omit `ResourceRecords`" / "omit
    // `TTL`"), or the traffic policy instance that generated it — so a change
    // naming more than one source, naming none, or naming only half of the
    // `TTL`-and-`ResourceRecords` pair is refused as a REQUEST fault. The rows
    // span the ways to name more than one, the ways to name none, and the two
    // shapes that name exactly one.
    let cases: &[SourceCase] = &[
        SourceCase {
            label: "an alias record that also specifies a TTL",
            shape: |record| record.alias(CLOUDFRONT_ZONE, "d111111abcdef8.cloudfront.net").ttl(60),
            found: Some("more than one"),
        },
        SourceCase {
            label: "an alias record that also specifies ResourceRecords",
            shape: |record| {
                record
                    .alias(CLOUDFRONT_ZONE, "d111111abcdef8.cloudfront.net")
                    .values(&["192.0.2.1"])
            },
            found: Some("more than one"),
        },
        SourceCase {
            label: "an alias record that also specifies a TTL and ResourceRecords",
            shape: |record| {
                record
                    .alias(CLOUDFRONT_ZONE, "d111111abcdef8.cloudfront.net")
                    .ttl(60)
                    .values(&["192.0.2.1"])
            },
            found: Some("more than one"),
        },
        SourceCase {
            label: "ResourceRecords with no TTL",
            shape: |record| record.values(&["192.0.2.1"]),
            found: Some("none"),
        },
        SourceCase {
            label: "a TTL with no ResourceRecords",
            shape: |record| record.ttl(60),
            found: Some("none"),
        },
        SourceCase {
            label: "neither record data nor an alias",
            shape: |record| record,
            found: Some("none"),
        },
        SourceCase {
            label: "an alias target alone",
            shape: |record| record.alias(CLOUDFRONT_ZONE, "d111111abcdef8.cloudfront.net"),
            found: None,
        },
        SourceCase {
            label: "a TTL with ResourceRecords",
            shape: |record| record.ttl(60).values(&["192.0.2.1"]),
            found: None,
        },
    ];

    let router = router();
    let zone_id = create_zone_ok(&router, ZONE, "ref-one-source").await;

    for (index, case) in cases.iter().enumerate() {
        let record = (case.shape)(Rrset::new(&format!("s{index}.{ZONE}"), "A"));
        let reply = submit_one(&router, &zone_id, "CREATE", &record).await;
        match case.found {
            Some(found) => {
                let expected = format!(
                    "Invalid request: Expected exactly one of [AliasTarget, all of [TTL, and \
                     ResourceRecords], or TrafficPolicyInstanceId], but found {found} in Change \
                     with [Action=CREATE, Name={}, Type=A, SetIdentifier=null]",
                    record.canonical()
                );
                assert_refused(
                    &router,
                    &zone_id,
                    case.label,
                    &reply,
                    "InvalidInput",
                    &expected,
                    &record.canonical(),
                )
                .await;
            }
            None => {
                assert_eq!(
                    reply.status, 200,
                    "case {:?}: expected 200, got {} with body {}",
                    case.label, reply.status, reply.body
                );
                assert!(
                    holds(&router, &zone_id, &record.canonical()).await,
                    "case {:?}: the applied record set must be listed",
                    case.label
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 4. A CNAME stands alone, and never at the apex
// ---------------------------------------------------------------------------

/// One name, the record set already there, and the record set that may not join
/// it.
struct ExclusivityCase {
    label: &'static str,
    /// The type and a suitable value of the record set created first, or `None`
    /// to leave the name empty.
    seeded: Option<(&'static str, &'static str)>,
    /// The type and value of the record set written next.
    written: (&'static str, &'static str),
    /// Whether the two may not stand together.
    refused: bool,
}

/// How a refusal is worded for the record set the change WROTE: a `CNAME`
/// conflicts with whatever else its name answers for, while a record of another
/// type collides with the one `CNAME` already there, which the refusal names.
fn exclusivity_refusal(rr_type: &str, name: &str) -> String {
    if rr_type == "CNAME" {
        format!(
            "[RRSet of type CNAME with DNS name {name} is not permitted as it conflicts with \
             other records with the same DNS name in zone {APEX}]"
        )
    } else {
        format!(
            "[RRSet of type {rr_type} with DNS name {name} is not permitted because a \
             conflicting RRSet of type CNAME with the same DNS name already exists in zone \
             {APEX}]"
        )
    }
}

#[tokio::test]
async fn a_cname_stands_alone_at_its_name() {
    // The rule: a CNAME says that its name IS another name, so every query for
    // that name is answered from the name it points at and no data of another
    // type can live beside it. One name therefore holds either a CNAME or
    // records of other types, and the refusal is symmetric — the CNAME beside
    // an existing record of another type is refused exactly as a record of
    // another type beside an existing CNAME — though each direction states it
    // from the record set the change wrote. The rows span which of the two was
    // there first, the OTHER type involved, and the pairs that may stand
    // together.
    let cases: &[ExclusivityCase] = &[
        ExclusivityCase {
            label: "a CNAME at a name that already holds an A",
            seeded: Some(("A", "192.0.2.1")),
            written: ("CNAME", "target.example.net."),
            refused: true,
        },
        ExclusivityCase {
            label: "a CNAME at a name that already holds a TXT",
            seeded: Some(("TXT", "\"owned\"")),
            written: ("CNAME", "target.example.net."),
            refused: true,
        },
        ExclusivityCase {
            label: "an A at a name that already holds a CNAME",
            seeded: Some(("CNAME", "target.example.net.")),
            written: ("A", "192.0.2.2"),
            refused: true,
        },
        ExclusivityCase {
            label: "an AAAA at a name that already holds a CNAME",
            seeded: Some(("CNAME", "target.example.net.")),
            written: ("AAAA", "2001:db8::1"),
            refused: true,
        },
        ExclusivityCase {
            label: "an MX at a name that already holds a CNAME",
            seeded: Some(("CNAME", "target.example.net.")),
            written: ("MX", "10 mail.example.net."),
            refused: true,
        },
        ExclusivityCase {
            label: "a CNAME at a name of its own",
            seeded: None,
            written: ("CNAME", "target.example.net."),
            refused: false,
        },
        ExclusivityCase {
            // The rule is about the CNAME, not about two types at one name.
            label: "a TXT at a name that already holds an A",
            seeded: Some(("A", "192.0.2.3")),
            written: ("TXT", "\"beside an A\""),
            refused: false,
        },
    ];

    let router = router();
    let zone_id = create_zone_ok(&router, ZONE, "ref-cname-alone").await;

    for (index, case) in cases.iter().enumerate() {
        let name = format!("c{index}.{ZONE}");
        if let Some((rr_type, value)) = case.seeded {
            let seed = Rrset::new(&name, rr_type).ttl(300).values(&[value]);
            assert_eq!(
                submit_one(&router, &zone_id, "CREATE", &seed).await.status,
                200,
                "case {:?}: the record set that is there first must be created",
                case.label
            );
        }

        let written = Rrset::new(&name, case.written.0)
            .ttl(300)
            .values(&[case.written.1]);
        let reply = submit_one(&router, &zone_id, "CREATE", &written).await;
        if case.refused {
            let expected = exclusivity_refusal(case.written.0, &written.canonical());
            assert_eq!(
                reply.status, 400,
                "case {:?}: expected 400, got {} with body {}",
                case.label, reply.status, reply.body
            );
            assert_eq!(
                reply.error_code(),
                "InvalidChangeBatch",
                "case {:?}: expected InvalidChangeBatch, got {}",
                case.label,
                reply.body
            );
            let actual = message(&reply);
            assert_eq!(
                actual, expected,
                "case {:?}: expected the refusal {expected:?}, got {actual:?}",
                case.label
            );
            // The refused batch landed nothing: the name still holds only the
            // type that was there first.
            let listed = list(&router, &zone_id).await;
            let types: Vec<&str> = listed
                .all("ResourceRecordSet/Name")
                .into_iter()
                .zip(listed.all("ResourceRecordSet/Type"))
                .filter(|(listed_name, _)| *listed_name == written.canonical())
                .map(|(_, rr_type)| rr_type)
                .collect();
            assert_eq!(
                types,
                vec![case.seeded.expect("a conflict needs a seeded record").0],
                "case {:?}: the refused change must not have landed, got {types:?}",
                case.label
            );
        } else {
            assert_eq!(
                reply.status, 200,
                "case {:?}: expected 200, got {} with body {}",
                case.label, reply.status, reply.body
            );
        }
    }
}

#[tokio::test]
async fn one_batch_may_replace_a_cname_with_another_type() {
    // The other edge of the same rule: it is judged against the state the batch
    // would LEAVE, so a batch that removes the CNAME and writes another type at
    // the same name is accepted whatever order its changes are written in —
    // while the batch that only adds the other type is not.
    for (case, changes_order) in [("the delete first", false), ("the create first", true)] {
        let router = router();
        let zone_id = create_zone_ok(&router, ZONE, "ref-cname-replace").await;
        let name = format!("swap.{ZONE}");
        let cname = Rrset::new(&name, "CNAME")
            .ttl(300)
            .values(&["target.example.net."]);
        let address = Rrset::new(&name, "A").ttl(300).values(&["192.0.2.4"]);
        assert_eq!(
            submit_one(&router, &zone_id, "CREATE", &cname).await.status,
            200,
            "case {case:?}: the CNAME must be created first"
        );

        let delete = change("DELETE", &cname);
        let create = change("CREATE", &address);
        let batch = if changes_order {
            format!("{create}{delete}")
        } else {
            format!("{delete}{create}")
        };
        let reply = submit(&router, &zone_id, &batch).await;
        assert_eq!(
            reply.status, 200,
            "case {case:?}: replacing the CNAME in one batch must be accepted, got {} with \
             body {}",
            reply.status, reply.body
        );

        let listed = list(&router, &zone_id).await;
        let types: Vec<&str> = listed
            .all("ResourceRecordSet/Name")
            .into_iter()
            .zip(listed.all("ResourceRecordSet/Type"))
            .filter(|(listed_name, _)| *listed_name == address.canonical())
            .map(|(_, rr_type)| rr_type)
            .collect();
        assert_eq!(
            types,
            vec!["A"],
            "case {case:?}: the name must hold only the A after the batch, got {types:?}"
        );
    }
}

#[tokio::test]
async fn a_cname_is_not_permitted_at_the_zone_apex() {
    // The rule: the apex answers for the zone itself and holds the zone's own
    // SOA and NS records, so a CNAME there — which would redirect every query
    // for the zone, its delegation included — is refused. The rows span the
    // spellings of the apex (with and without the trailing dot, in mixed case)
    // and the actions, against the same CNAME one label down, which IS the
    // caller's to create.
    let router = router();
    let zone_id = create_zone_ok(&router, ZONE, "ref-apex-cname").await;

    for spelling in [ZONE, APEX, "EXAMPLE.COM", "Example.Com."] {
        for action in ["CREATE", "UPSERT"] {
            let case = format!("{spelling} ({action})");
            let apex_cname = Rrset::new(spelling, "CNAME")
                .ttl(300)
                .values(&["target.example.net."]);
            let reply = submit_one(&router, &zone_id, action, &apex_cname).await;
            let expected = format!(
                "[RRSet of type CNAME with DNS name {APEX} is not permitted at apex in zone \
                 {APEX}]"
            );
            assert_eq!(
                reply.status, 400,
                "case {case:?}: expected 400, got {} with body {}",
                reply.status, reply.body
            );
            assert_eq!(
                reply.error_code(),
                "InvalidChangeBatch",
                "case {case:?}: expected InvalidChangeBatch, got {}",
                reply.body
            );
            let actual = message(&reply);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected the refusal {expected:?}, got {actual:?}"
            );
            // The apex still holds exactly the zone's own records.
            let listed = list(&router, &zone_id).await;
            assert_eq!(
                listed.all("ResourceRecordSet/Type"),
                vec!["NS", "SOA"],
                "case {case:?}: a refused batch must leave the apex alone, got {}",
                listed.body
            );
        }
    }

    // One label down, the same CNAME is the caller's to create.
    let below = Rrset::new(&format!("www.{ZONE}"), "CNAME")
        .ttl(300)
        .values(&["target.example.net."]);
    assert_eq!(
        submit_one(&router, &zone_id, "CREATE", &below).await.status,
        200,
        "a CNAME one label below the apex must be created"
    );
    assert!(
        holds(&router, &zone_id, &below.canonical()).await,
        "the CNAME below the apex must be listed"
    );
}

// ---------------------------------------------------------------------------
// 5. A latency record set routes by a region that exists
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_latency_record_set_is_refused_in_two_stages() {
    // The rule: a latency record set routes by "the Amazon EC2 Region where you
    // created the resource that this resource record set refers to", so a
    // change that selects latency routing must carry both the `SetIdentifier`
    // that tells it from its siblings and a `Region` that resolves to a region.
    // The two are refused by DIFFERENT stages: the missing identifier by the
    // request-shape stage (`InvalidInput`, naming the field), the unresolvable
    // region by the lookup a change batch does (`InvalidChangeBatch`, naming the
    // region). The region rows come from the modelled `ResourceRecordSetRegion`
    // value set (`smithy_query(query="shape",shape="ResourceRecordSetRegion")`)
    // and from values outside it.
    let router = router();
    let zone_id = create_zone_ok(&router, ZONE, "ref-latency").await;

    // Stage one: the identifier is missing, so the region is never reached.
    let lonely = Rrset::new(&format!("lonely.{ZONE}"), "A")
        .routing("<Region>us-west-2</Region>")
        .ttl(60)
        .values(&["192.0.2.20"]);
    let refused_shape = submit_one(&router, &zone_id, "CREATE", &lonely).await;
    assert_refused(
        &router,
        &zone_id,
        "a Region with no SetIdentifier",
        &refused_shape,
        "InvalidInput",
        &format!(
            "Invalid request: Missing field 'SetIdentifier' in Change with [Action=CREATE, \
             Name={}, Type=A, SetIdentifier=null]",
            lonely.canonical()
        ),
        &lonely.canonical(),
    )
    .await;

    // Stage two: the identifier is there, so the region itself is judged. The
    // accepted rows include an entry the recorded case did not cite.
    for (index, region) in ["us-west-2", "ap-southeast-4", "eusc-de-east-1"]
        .into_iter()
        .enumerate()
    {
        let record = Rrset::new(&format!("lat{index}.{ZONE}"), "A")
            .set_identifier(region)
            .routing(&format!("<Region>{region}</Region>"))
            .ttl(60)
            .values(&["192.0.2.21"]);
        let reply = submit_one(&router, &zone_id, "CREATE", &record).await;
        assert_eq!(
            reply.status, 200,
            "case {region}: a modelled region must be accepted, got {} with body {}",
            reply.status, reply.body
        );
        assert!(
            holds(&router, &zone_id, &record.canonical()).await,
            "case {region}: the latency record set must be listed"
        );
    }

    for (index, region) in ["xx-fake-1", "us-west-3", "US-WEST-2"]
        .into_iter()
        .enumerate()
    {
        let record = Rrset::new(&format!("bad{index}.{ZONE}"), "A")
            .set_identifier("primary")
            .routing(&format!("<Region>{region}</Region>"))
            .ttl(60)
            .values(&["192.0.2.22"]);
        let reply = submit_one(&router, &zone_id, "CREATE", &record).await;
        assert_refused(
            &router,
            &zone_id,
            region,
            &reply,
            "InvalidChangeBatch",
            &format!("[Cannot find region '{region}'. No changes made.]"),
            &record.canonical(),
        )
        .await;
    }

    // A batch that ALSO breaks a rule of its own shape is still a request
    // fault: the region lookup is not what refused it.
    let two_faults = format!(
        "{}{}",
        change(
            "CREATE",
            &Rrset::new(&format!("mixed.{ZONE}"), "A")
                .set_identifier("primary")
                .routing("<Region>xx-fake-1</Region>")
                .ttl(60)
                .values(&["192.0.2.23"])
        ),
        change(
            "CREATE",
            &Rrset::new(&format!("long.{ZONE}"), "A")
                .set_identifier(&"i".repeat(129))
                .ttl(60)
                .values(&["192.0.2.24"])
        )
    );
    let mixed = submit(&router, &zone_id, &two_faults).await;
    assert_eq!(
        mixed.status, 400,
        "case a region beside a shape fault: expected 400, got {} with body {}",
        mixed.status, mixed.body
    );
    assert_eq!(
        mixed.error_code(),
        "InvalidInput",
        "case a region beside a shape fault: expected InvalidInput, got {}",
        mixed.body
    );
}

// ---------------------------------------------------------------------------
// 6. An alias target lies within the hosted zone it names
// ---------------------------------------------------------------------------

/// The hosted zone id AWS fixes for a Global Accelerator accelerator, from the
/// same documentation `CLOUDFRONT_ZONE` is read off ("Global Accelerator
/// accelerator: Specify `Z2BJ6XQ5FK7U4H`"). Exercised here because the rule is
/// about every target zone whose domain is fixed with its id, not about one of
/// them.
const GLOBAL_ACCELERATOR_ZONE: &str = "Z2BJ6XQ5FK7U4H";

/// A target zone whose id a caller reads off the RESOURCE instead — a load
/// balancer's `CanonicalHostedZoneId`, per the same documentation — so no domain
/// is fixed with it and a target naming it is not checked against one.
///
/// `pub(crate)` so a sibling suite writing an alias that is about something OTHER
/// than where its target lies names the same zone rather than a second copy of it.
pub(crate) const RESOURCE_ZONE: &str = "Z35SXDOTRQ7X7K";

/// One alias target and the zone it names, with what the service must do with
/// it.
struct AliasZoneCase {
    label: &'static str,
    /// The `AliasTarget.HostedZoneId` the change names.
    zone: &'static str,
    /// The `AliasTarget.DNSName` the change names.
    target: &'static str,
    /// That target as the service reports it: lower-cased and fully qualified.
    reported: &'static str,
    /// Whether the batch must be refused.
    refused: bool,
}

#[tokio::test]
async fn an_alias_target_must_lie_within_the_hosted_zone_it_names() {
    // The rule: an alias routes to the resource its target names, and the target
    // names it by the hosted zone it lives in plus its name INSIDE that zone, so
    // a change writing an alias whose target lies outside the zone it names is
    // refused `InvalidChangeBatch`. The zones this is decidable for are the ones
    // whose id the owning service FIXES — CloudFront's and Global Accelerator's,
    // the two `AliasTarget$HostedZoneId` states by id — because the domain their
    // names lie in is fixed with the id; a zone id read off a resource fixes no
    // domain, so a target naming one is accepted whatever it is.
    //
    // The rows span what the rule generalises over: the two fixed zones, a
    // target inside the zone and one outside it, the case and trailing dot it is
    // written in, a name whose last labels only LOOK like the zone's, a name that
    // merely ends with the zone's text inside a longer label, each fixed zone
    // against the OTHER one's domain, a target that names the address an endpoint
    // of this emulator is published at, and a zone id no domain is fixed with.
    let router = router();
    let zone_id = create_zone_ok(&router, ZONE, "alias-target-zone").await;

    let rows: &[AliasZoneCase] = &[
        AliasZoneCase {
            label: "a distribution's own domain",
            zone: CLOUDFRONT_ZONE,
            target: "d111111abcdef8.cloudfront.net",
            reported: "d111111abcdef8.cloudfront.net.",
            refused: false,
        },
        AliasZoneCase {
            label: "the same domain in mixed case, already qualified",
            zone: CLOUDFRONT_ZONE,
            target: "D111111ABCDEF8.CloudFront.NET.",
            reported: "d111111abcdef8.cloudfront.net.",
            refused: false,
        },
        AliasZoneCase {
            label: "a name in the caller's own domain",
            zone: CLOUDFRONT_ZONE,
            target: "cdn.example.net",
            reported: "cdn.example.net.",
            refused: true,
        },
        AliasZoneCase {
            label: "a name whose last labels only look like the zone's",
            zone: CLOUDFRONT_ZONE,
            target: "notcloudfront.net",
            reported: "notcloudfront.net.",
            refused: true,
        },
        AliasZoneCase {
            label: "a name that carries the zone's text in a longer suffix",
            zone: CLOUDFRONT_ZONE,
            target: "d111111abcdef8.cloudfront.net.example.com",
            reported: "d111111abcdef8.cloudfront.net.example.com.",
            refused: true,
        },
        AliasZoneCase {
            label: "an accelerator's own DNS name",
            zone: GLOBAL_ACCELERATOR_ZONE,
            target: "a1234567890abcdef.awsglobalaccelerator.com",
            reported: "a1234567890abcdef.awsglobalaccelerator.com.",
            refused: false,
        },
        AliasZoneCase {
            label: "the other fixed zone's domain under the accelerator zone",
            zone: GLOBAL_ACCELERATOR_ZONE,
            target: "d111111abcdef8.cloudfront.net",
            reported: "d111111abcdef8.cloudfront.net.",
            refused: true,
        },
        AliasZoneCase {
            label: "the accelerator domain under the CloudFront zone",
            zone: CLOUDFRONT_ZONE,
            target: "a1234567890abcdef.awsglobalaccelerator.com",
            reported: "a1234567890abcdef.awsglobalaccelerator.com.",
            refused: true,
        },
        AliasZoneCase {
            // An endpoint this emulator serves is published as the address a
            // client dials rather than as a name in the owning service's zone,
            // so a target that names one names an endpoint of that zone.
            label: "the address an endpoint of this emulator is published at",
            zone: CLOUDFRONT_ZONE,
            target: "127.0.0.1:34865",
            reported: "127.0.0.1:34865.",
            refused: false,
        },
        AliasZoneCase {
            label: "a zone id read off the resource, with a load balancer's name",
            zone: RESOURCE_ZONE,
            target: "my-lb-123456.us-east-1.elb.amazonaws.com",
            reported: "my-lb-123456.us-east-1.elb.amazonaws.com.",
            refused: false,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let record = Rrset::new(&format!("alias{index}.{ZONE}"), "A").alias(row.zone, row.target);
        let reply = submit_one(&router, &zone_id, "CREATE", &record).await;
        if row.refused {
            assert_refused(
                &router,
                &zone_id,
                row.label,
                &reply,
                "InvalidChangeBatch",
                &format!(
                    "[Tried to create an alias that targets {}, type A in zone {}, but the \
                     alias target name does not lie within the target zone]",
                    row.reported, row.zone
                ),
                &record.canonical(),
            )
            .await;
            continue;
        }
        assert_eq!(
            reply.status, 200,
            "case {:?}: expected the alias to be accepted, got {} with body {}",
            row.label, reply.status, reply.body
        );
        let listed = list(&router, &zone_id).await;
        let targets = listed.all("AliasTarget/DNSName");
        assert!(
            targets.contains(&row.reported),
            "case {:?}: expected the alias target {:?} in the listing, got {targets:?}",
            row.label,
            row.reported
        );
    }

    // The rule is about what a change WRITES, not about the action it writes
    // with: an UPSERT carrying an out-of-zone target is refused exactly as the
    // CREATE above is.
    let upserted = Rrset::new(&format!("upsert.{ZONE}"), "A")
        .alias(CLOUDFRONT_ZONE, "cdn.example.net");
    let reply = submit_one(&router, &zone_id, "UPSERT", &upserted).await;
    assert_refused(
        &router,
        &zone_id,
        "an UPSERT with an out-of-zone target",
        &reply,
        "InvalidChangeBatch",
        &format!(
            "[Tried to create an alias that targets cdn.example.net., type A in zone \
             {CLOUDFRONT_ZONE}, but the alias target name does not lie within the target zone]"
        ),
        &upserted.canonical(),
    )
    .await;
}

// ---------------------------------------------------------------------------
// 7. The same rule over the target zones AWS publishes ONE ID PER REGION for
// ---------------------------------------------------------------------------

/// The problem a refused alias is reported with, for a target already in the
/// form the service reports it (lower-cased and fully qualified).
fn out_of_zone_problem(reported_target: &str, rr_type: &str, zone_id: &str) -> String {
    format!(
        "[Tried to create an alias that targets {reported_target}, type {rr_type} in zone \
         {zone_id}, but the alias target name does not lie within the target zone]"
    )
}

#[tokio::test]
async fn an_alias_target_must_lie_within_the_region_zone_of_the_id_it_names() {
    // The rule: an alias target lies within the hosted zone its `HostedZoneId`
    // names, and for the target zones AWS publishes the id of, the domain that
    // zone holds names in is published with the id — one id for every CloudFront
    // distribution and every accelerator, and ONE PER REGION for the Amazon S3
    // website endpoints ("Specify the hosted zone ID for the region that you
    // created the bucket in"). So a Region's endpoint host under another
    // Region's published id names a zone it does not lie within, and the batch
    // is refused `InvalidChangeBatch`.
    //
    // Asserted over the WHOLE published catalog
    // (`service_com_amazonaws_route53_awsdnsv20130401::state::alias_target_zones`,
    // read from the *Amazon S3 Website Endpoints* table
    // `smithy_query(query="doc",shape="AliasTarget",member="HostedZoneId")`
    // links), not over one Region: every entry's own id accepts the endpoint
    // domain in both spellings Amazon S3 publishes and a bucket host under it,
    // and the NEXT entry's id refuses the same name.
    let router = router();
    let zone_id = create_zone_ok(&router, ZONE, "s3-website-alias-zone").await;
    let catalog = S3_WEBSITE_TARGET_ZONE_REGIONS;
    assert!(
        catalog.len() > 1,
        "the published catalog must hold more than one Region, got {catalog:?}"
    );

    for (index, (id, region)) in catalog.iter().enumerate() {
        let [hyphenated, dotted] = s3_website_domains(region);
        // The endpoint domain itself, which is what `AliasTarget$DNSName`
        // documents for a bucket website ("the domain name of the Amazon S3
        // website endpoint that you created the bucket in"), in both spellings
        // Amazon S3 publishes, and a bucket's own host under it.
        let targets = [
            dotted.clone(),
            hyphenated.clone(),
            format!("{ZONE}.{hyphenated}"),
        ];
        for (spelling, target) in targets.iter().enumerate() {
            let case = format!("{region}: {target:?} under its own published zone {id}");
            let record = Rrset::new(&format!("s3-{index}-{spelling}.{ZONE}"), "A").alias(id, target);
            let reply = submit_one(&router, &zone_id, "CREATE", &record).await;
            assert_eq!(
                reply.status, 200,
                "case {case:?}: expected the alias to be accepted, got {} with body {}",
                reply.status, reply.body
            );
            let listed = list(&router, &zone_id).await;
            let reported = format!("{target}.");
            let targets_listed = listed.all("AliasTarget/DNSName");
            assert!(
                targets_listed.contains(&reported.as_str()),
                "case {case:?}: expected the alias target {reported:?} in the listing, got \
                 {targets_listed:?}"
            );
        }

        // The same endpoint host under the NEXT Region's published id: one
        // Region's endpoints do not lie within another Region's zone.
        let (other_id, other_region) = catalog[(index + 1) % catalog.len()];
        let case = format!("{region} endpoint {dotted:?} under {other_region}'s zone {other_id}");
        let record =
            Rrset::new(&format!("s3-mismatch-{index}.{ZONE}"), "A").alias(other_id, &dotted);
        let reply = submit_one(&router, &zone_id, "CREATE", &record).await;
        assert_refused(
            &router,
            &zone_id,
            &case,
            &reply,
            "InvalidChangeBatch",
            &out_of_zone_problem(&format!("{dotted}."), "A", other_id),
            &record.canonical(),
        )
        .await;
    }
}

/// One alias target submitted under one published hosted zone id, and whether
/// the batch must be refused.
struct PublishedZoneCase {
    label: &'static str,
    /// The `AliasTarget.HostedZoneId` the change names.
    zone: &'static str,
    /// The `AliasTarget.DNSName` the change names.
    target: &'static str,
    /// Whether the batch must be refused.
    refused: bool,
}

#[tokio::test]
async fn an_s3_website_alias_target_is_judged_by_the_endpoint_domain_of_its_own_region() {
    // The same rule, over what the published per-Region zones DO and do not
    // hold: a Region's website zone holds the names under that Region's
    // endpoint domain and nothing else, so a name that merely carries the
    // endpoint label, one under another Region's domain, and one under the
    // wrong DNS suffix all lie outside it.
    //
    // The ids and Regions below are entries of the published catalog
    // (`state::alias_target_zones::S3_WEBSITE_TARGET_ZONE_REGIONS`, read from
    // the *Amazon S3 Website Endpoints* table
    // `smithy_query(query="doc",shape="AliasTarget",member="HostedZoneId")`
    // links); the China Region is there because its endpoints are published
    // under their own DNS suffix.
    let router = router();
    let zone_id = create_zone_ok(&router, ZONE, "s3-website-domain-zone").await;

    /// The published zone id of the Amazon S3 website endpoints of `eu-west-1`.
    const EU_WEST_1: &str = "Z1BKCTXD74EZPE";
    /// The published zone id of the Amazon S3 website endpoints of `cn-north-1`.
    const CN_NORTH_1: &str = "Z5CN8UMXT92WN";

    let rows: &[PublishedZoneCase] = &[
        PublishedZoneCase {
            label: "the Region's own endpoint domain",
            zone: EU_WEST_1,
            target: "s3-website.eu-west-1.amazonaws.com",
            refused: false,
        },
        PublishedZoneCase {
            label: "a bucket host under the Region's own endpoint domain",
            zone: EU_WEST_1,
            target: "reports.s3-website-eu-west-1.amazonaws.com",
            refused: false,
        },
        PublishedZoneCase {
            label: "a bucket host under ANOTHER Region's endpoint domain",
            zone: EU_WEST_1,
            target: "reports.s3-website-us-east-2.amazonaws.com",
            refused: true,
        },
        PublishedZoneCase {
            label: "a name that only carries the endpoint label",
            zone: EU_WEST_1,
            target: "s3-website.example.net",
            refused: true,
        },
        PublishedZoneCase {
            label: "a name that carries the endpoint domain in a longer suffix",
            zone: EU_WEST_1,
            target: "site.s3-website-eu-west-1.amazonaws.com.example.net",
            refused: true,
        },
        PublishedZoneCase {
            label: "a China Region's endpoint domain, under its own DNS suffix",
            zone: CN_NORTH_1,
            target: "s3-website.cn-north-1.amazonaws.com.cn",
            refused: false,
        },
        PublishedZoneCase {
            label: "the same Region's endpoint under the wrong DNS suffix",
            zone: CN_NORTH_1,
            target: "s3-website.cn-north-1.amazonaws.com",
            refused: true,
        },
        PublishedZoneCase {
            // An endpoint this emulator serves is published as the address a
            // client dials rather than as a name in the owning service's zone,
            // so a target that names one names an endpoint of that zone.
            label: "the address an endpoint of this emulator is published at",
            zone: EU_WEST_1,
            target: "127.0.0.1:41000",
            refused: false,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let record = Rrset::new(&format!("held{index}.{ZONE}"), "A").alias(row.zone, row.target);
        let reply = submit_one(&router, &zone_id, "CREATE", &record).await;
        if row.refused {
            assert_refused(
                &router,
                &zone_id,
                row.label,
                &reply,
                "InvalidChangeBatch",
                &out_of_zone_problem(&format!("{}.", row.target), "A", row.zone),
                &record.canonical(),
            )
            .await;
            continue;
        }
        assert_eq!(
            reply.status, 200,
            "case {:?}: expected the alias to be accepted, got {} with body {}",
            row.label, reply.status, reply.body
        );
    }
}

/// One alias whose `HostedZoneId` names a hosted zone of the account's own, and
/// whether the target it names lies within that zone.
struct OwnZoneCase {
    label: &'static str,
    /// The `AliasTarget.DNSName` the change names.
    target: &'static str,
    /// Whether the batch must be refused.
    refused: bool,
}

#[tokio::test]
async fn an_alias_target_must_lie_within_the_callers_own_hosted_zone() {
    // The rule, for the other kind of zone whose contents are known here: a
    // hosted zone of the account's own holds exactly the names inside it —
    // "Another Route 53 resource record set in your hosted zone: Specify the
    // hosted zone ID of your hosted zone. (An alias resource record set can't
    // reference a resource record set in a different hosted zone.)", whose
    // `DNSName` is "the value of the `Name` element for a resource record set
    // in the current hosted zone" — so a target outside that zone's own name is
    // refused.
    //
    // The rows span a name inside the zone, the zone apex itself, a subdomain
    // of a subdomain, a name whose last labels only LOOK like the zone's, a
    // name in another zone THIS ACCOUNT holds, and a name of a published
    // endpoint zone.
    let router = router();
    let zone_id = create_zone_ok(&router, ZONE, "own-zone-alias").await;
    let other_zone_id = create_zone_ok(&router, "example.net", "own-zone-alias-other").await;

    let rows: &[OwnZoneCase] = &[
        OwnZoneCase {
            label: "a name inside the zone",
            target: "www.example.com",
            refused: false,
        },
        OwnZoneCase {
            label: "the zone apex itself",
            target: APEX,
            refused: false,
        },
        OwnZoneCase {
            label: "a name several labels inside the zone",
            target: "blue.www.example.com",
            refused: false,
        },
        OwnZoneCase {
            label: "a name whose last labels only look like the zone's",
            target: "notexample.com",
            refused: true,
        },
        OwnZoneCase {
            label: "a name in another zone the account holds",
            target: "www.example.net",
            refused: true,
        },
        OwnZoneCase {
            label: "a published endpoint zone's own name",
            target: "d111111abcdef8.cloudfront.net",
            refused: true,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let record = Rrset::new(&format!("own{index}.{ZONE}"), "A").alias(&zone_id, row.target);
        let reply = submit_one(&router, &zone_id, "CREATE", &record).await;
        let reported = format!("{}.", row.target.trim_end_matches('.'));
        if row.refused {
            assert_refused(
                &router,
                &zone_id,
                row.label,
                &reply,
                "InvalidChangeBatch",
                &out_of_zone_problem(&reported, "A", &zone_id),
                &record.canonical(),
            )
            .await;
            continue;
        }
        assert_eq!(
            reply.status, 200,
            "case {:?}: expected the alias to be accepted, got {} with body {}",
            row.label, reply.status, reply.body
        );
    }

    // The zone the batch is submitted to is not what decides this: an alias in
    // the OTHER zone naming the first zone's id is held against the first
    // zone's names just the same.
    let crossed = Rrset::new("cross.example.net", "A").alias(&zone_id, "www.example.net");
    let reply = submit_one(&router, &other_zone_id, "CREATE", &crossed).await;
    assert_refused(
        &router,
        &other_zone_id,
        "a target in the submitting zone, under another zone's id",
        &reply,
        "InvalidChangeBatch",
        &out_of_zone_problem("www.example.net.", "A", &zone_id),
        &crossed.canonical(),
    )
    .await;
}
