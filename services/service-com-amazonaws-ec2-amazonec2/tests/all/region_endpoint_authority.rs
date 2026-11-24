// SPDX-License-Identifier: CC0-1.0

//! `DescribeRegions` announces each Region at an endpoint that leads back to this emulator.
//!
//! THE RULE: `Region$Endpoint` is "The Region service endpoint"
//! (`smithy_query(query="doc",shape="Region",member="Endpoint")`), so every row of a
//! `DescribeRegions` report carries a name a multi-region client RESOLVES and then sends its
//! next requests to. This emulator's answer is therefore derived, per row, from the row's OWN
//! Region code — this service's endpoint prefix and that code are its leading labels, which
//! is what lets a client tell one Region's endpoint from another's — in the DNS zone THIS
//! emulator answers in, read off the address the request being served arrived at. No row
//! carries a name in a zone Amazon Web Services serves, and no row carries another row's
//! Region.
//!
//! Each row of the table below differs in the FORM of the address the request reached: an
//! address with a port, a bracketed IPv6 address with a port, a name in the loopback zone
//! with a port, an address naming no port, and a request addressed to one of Amazon Web
//! Services' own hosts — which is not this emulator's zone to announce in, so the address
//! this process is served at answers for it. Each form is driven for more than one signed
//! Region, because the rule is about every row of every report rather than about one Region.
//!
//! Region ORDER is deliberately not asserted: "the order of the elements in the response,
//! including those within nested structures, might vary. Applications should not assume the
//! elements appear in a particular order" (`DescribeRegions`).

use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{API_VERSION, filters, form_encode};

/// The emulator the tests drive, as [`super::ec2_query::emulator`] builds it. This
/// operation reports facts of a partition and stores nothing, so the engine handle it
/// hands back is not needed here.
fn emulator() -> MultiServiceRouter {
    super::ec2_query::emulator().0
}

/// The zones Amazon Web Services publishes its OWN service endpoints in, as the partition
/// metadata gives them: a `regionEndpoint` under any of these is a name this emulator does not
/// answer, so a client that resolved it would leave the emulated environment.
const AWS_ENDPOINT_ZONES: [&str; 4] = [
    "amazonaws.com",
    "amazonaws.com.cn",
    "api.aws",
    "c2s.ic.gov",
];

/// The DNS zone this process is SERVED in for a request that addressed one of Amazon Web
/// Services' own hosts: the host of the base a later request addresses
/// (`smithy_server_runtime::UNSERVED_URL_BASE` for a suite that runs the service in process on
/// no port at all — a host RFC 6761 reserves so that nothing resolves it), without its scheme
/// or its port.
fn served_zone() -> &'static str {
    let base = smithy_server_runtime::UNSERVED_URL_BASE;
    let authority = base
        .split_once("://")
        .map_or(base, |(_scheme, authority)| authority);
    authority
        .split(':')
        .next()
        .expect("the served base names a host")
}

/// One `DescribeRegions` body: the action, the API version a client sends, and `params`.
fn form_body(params: &[(String, String)]) -> String {
    let mut body = format!("Action=DescribeRegions&Version={API_VERSION}");
    for (name, value) in params {
        body.push('&');
        body.push_str(name);
        body.push('=');
        body.push_str(&form_encode(value));
    }
    body
}

/// One `DescribeRegions` call signed for `region` that ARRIVED at `authority` — the `Host` a
/// client addressed this emulator by — or carrying no `Host` at all, which is the request
/// whose absolute-form target names one of Amazon Web Services' own endpoints.
async fn describe_reached_at(
    router: &MultiServiceRouter,
    authority: Option<&str>,
    region: &str,
    params: &[(String, String)],
) -> (u16, String) {
    let headers: Vec<(&str, &str)> = authority
        .map(|host| vec![("host", host)])
        .unwrap_or_default();
    let (status, _content_type, body) =
        super::ec2_query::call_raw(router, region, &headers, &form_body(params)).await;
    (status, body)
}

/// One Region of a `DescribeRegions` response: its code and the endpoint it was announced at.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Announced {
    name: String,
    endpoint: String,
}

/// Every Region a `DescribeRegions` body reports, in the order the response carried them.
///
/// A Region is an element that has a `<regionName>` child, read through the document rather
/// than off the text, so a member is never read out of a shape that merely shares its name.
fn announced(case: &str, body: &str) -> Vec<Announced> {
    let parsed = super::wire::document(case, body);
    let rows: Vec<Announced> = parsed
        .descendants()
        .filter(|node| node.is_element() && super::wire::child(*node, "regionName").is_some())
        .map(|node| {
            let member = |name: &str| {
                super::wire::child_text(node, name)
                    .unwrap_or_else(|| panic!("case {case:?}: no <{name}> in a Region: {body}"))
            };
            Announced {
                name: member("regionName"),
                // `Region$Endpoint` is on the wire as `regionEndpoint`.
                endpoint: member("regionEndpoint"),
            }
        })
        .collect();
    assert!(
        !rows.is_empty(),
        "case {case:?}: expected a Region in the report, got {body}"
    );
    rows
}

/// One row of the table: the case, the address the request arrived at, and the zone every
/// announced endpoint of that request must be a name in.
struct Reached {
    case: &'static str,
    authority: Option<&'static str>,
    /// `None` for the row whose zone is the address this process is served at
    /// ([`served_zone`]), which is not a literal.
    zone: Option<&'static str>,
}

/// The address forms a request reaches this emulator at. An ADDRESS names no DNS zone and
/// names the interface the loopback zone resolves to, which RFC 6761 reserves `localhost` and
/// every name under it for — so an emulator reached at an address announces its Regions there;
/// a host that names a SERVICE is one of Amazon Web Services' own endpoint names, which is not
/// this emulator's zone to announce in.
const REACHED: [Reached; 6] = [
    Reached {
        case: "an address and the port this emulator accepts on",
        authority: Some("127.0.0.1:9297"),
        zone: Some("localhost"),
    },
    Reached {
        case: "a bracketed IPv6 address and a port",
        authority: Some("[::1]:8443"),
        zone: Some("localhost"),
    },
    Reached {
        case: "a name in the loopback zone and a port",
        authority: Some("localhost:41234"),
        zone: Some("localhost"),
    },
    Reached {
        case: "an address naming no port at all",
        authority: Some("127.0.0.1"),
        zone: Some("localhost"),
    },
    Reached {
        case: "one of Amazon Web Services' own regional endpoint names",
        authority: Some("ec2.eu-central-1.amazonaws.com"),
        zone: None,
    },
    Reached {
        case: "a request stating no host at all",
        authority: None,
        zone: None,
    },
];

impl Reached {
    /// The zone this row's announced endpoints are names in.
    fn expected_zone(&self) -> &str {
        self.zone.unwrap_or_else(served_zone)
    }
}

/// The Regions every call below is signed for: two Regions of the commercial partition, so
/// each claim is about the whole report rather than about the one Region a request was signed
/// for.
const SIGNED: [&str; 2] = ["us-east-1", "eu-west-3"];

/// Every Region is announced at a name in the zone this emulator answers in, with its OWN
/// Region in the endpoint's leading labels — and never in a zone Amazon Web Services serves.
#[tokio::test]
async fn a_region_is_announced_in_the_zone_this_emulator_answers_in() {
    let router = emulator();
    for reached in &REACHED {
        let zone = reached.expected_zone();
        for signed in SIGNED {
            let case = format!("{} signed for {signed}", reached.case);
            let (status, body) = describe_reached_at(&router, reached.authority, signed, &[]).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected 200, got {status} {body}"
            );
            let reported = announced(&case, &body);
            for region in &reported {
                assert_eq!(
                    region.endpoint,
                    format!("ec2.{}.{zone}", region.name),
                    "case {case:?}: expected this service's prefix and the Region's own code \
                     in the {zone:?} zone, got {region:?}"
                );
                for aws_zone in AWS_ENDPOINT_ZONES {
                    assert!(
                        !region.endpoint.contains(aws_zone),
                        "case {case:?}: a Region must not be announced in the {aws_zone:?} \
                         zone, got {region:?}"
                    );
                }
            }
            // Two Regions never share an announced endpoint: the Region in the leading
            // labels is what a client tells one Region's endpoint from another's by.
            let mut endpoints: Vec<String> = reported
                .iter()
                .map(|region| region.endpoint.clone())
                .collect();
            let announced_count = endpoints.len();
            endpoints.sort();
            endpoints.dedup();
            assert_eq!(
                endpoints.len(),
                announced_count,
                "case {case:?}: every Region must be announced at its own endpoint, got \
                 {endpoints:?}"
            );
        }
    }
}

/// The value a Region was announced at selects that Region through the `endpoint` filter, so
/// a client can ask about the endpoint it was handed and be answered the one Region it
/// belongs to.
#[tokio::test]
async fn the_announced_endpoint_selects_its_own_region() {
    let router = emulator();
    for reached in &REACHED {
        for signed in SIGNED {
            let case = format!("{} signed for {signed}", reached.case);
            let (status, body) = describe_reached_at(&router, reached.authority, signed, &[]).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected 200, got {status} {body}"
            );
            // The Region the report announces LAST, so the row under test is not the one a
            // request signed for that Region would report first.
            let row = announced(&case, &body)
                .into_iter()
                .next_back()
                .expect("a reported Region");
            let (status, filtered) = describe_reached_at(
                &router,
                reached.authority,
                signed,
                &filters(&[("endpoint", &[row.endpoint.as_str()])]),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected 200 from the endpoint filter, got {status} {filtered}"
            );
            assert_eq!(
                announced(&case, &filtered),
                vec![row.clone()],
                "case {case:?}: the announced endpoint {:?} must select exactly its own \
                 Region, got {filtered}",
                row.endpoint
            );
        }
    }
}
