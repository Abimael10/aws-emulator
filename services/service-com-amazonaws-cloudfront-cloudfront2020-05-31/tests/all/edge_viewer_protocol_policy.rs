// SPDX-License-Identifier: CC0-1.0

//! What the matched cache behaviour's `ViewerProtocolPolicy` does with the scheme a viewer
//! DIALLED, end to end over the address the distribution published.
//!
//! The rule: the member is "the protocol that viewers can use to access the files in the
//! origin specified by `TargetOriginId`", and each of its three values states its own answer
//! for a viewer that dialled the plaintext half of that address — `allow-all`: "viewers can
//! use HTTP or HTTPS", so the request is served as it arrived; `redirect-to-https`:
//! "CloudFront returns an HTTP status code of 301 (Moved Permanently) to the viewer along
//! with the HTTPS URL. The viewer then resubmits the request using the new URL", which is the
//! SAME request's URL over `https` — the host the viewer addressed, its path and its query;
//! `https-only`: "CloudFront returns an HTTP status code of 403 (Forbidden)". A viewer that
//! dialled `https` is served under every one of them, and the behaviour that decides is the
//! one the viewer PATH matched, the ordered behaviours first and the default behaviour when
//! none matches.
//!
//! The rows differ along what the rule generalises over: each of the three policy values, the
//! two schemes a viewer may dial the one published address with, the shape of the URL the
//! redirect has to carry back (the root, a nested path, a path with a query string), and
//! which behaviour matched (the default one, and an ordered one whose policy differs from
//! it).
//!
//! The harness is the lifecycle suites': the wire client is
//! [`crate::distribution_lifecycle`]'s, the viewer dial [`crate::published_edge_address`]'s.

use smithy_server_runtime::MultiServiceRouter;

use crate::distribution_lifecycle::{
    awaited_deployment, create_distribution, emulator, is_reachable_authority, API_VERSION, HOST,
};
use crate::published_edge_address::dial;

/// A viewer path the origin these rows point at — this service's own control-plane endpoint —
/// answers with a `DistributionList`, so a row that is SERVED carries the origin's own bytes.
const SERVED_PATH: &str = "/2020-05-31/distribution";

/// The AWS-managed cache policy `Managed-CachingDisabled`, whose TTLs are all zero: nothing
/// is cached, so every row is answered by the same path however many rows ran before it.
const CACHING_DISABLED: &str = "4135ea2d-6df8-44a3-9df3-4b5a84be39ad";

/// The path pattern the ordered cache behaviour below claims: it matches [`SERVED_PATH`] and
/// nothing else a row dials, so which behaviour matched is what the rows cross.
const ORDERED_PATTERN: &str = "/2020-05-31/*";

/// A viewer path under no ordered behaviour's pattern, so the DEFAULT behaviour matches it.
/// The origin answers it with a refusal of its own rather than an object, which is still the
/// origin answering: what a served row asserts is that CloudFront did not answer it itself.
const UNMATCHED_PATH: &str = "/index.html";

/// `X-Cache` for an answer the ORIGIN produced for this request, which is what an answer
/// CloudFront did not produce itself carries.
const MISS: &str = "Miss from cloudfront";

/// One `DistributionConfig`: one origin, a default cache behaviour under `default_policy`,
/// and — where `ordered_policy` is stated — one ordered behaviour under [`ORDERED_PATTERN`]
/// with a policy of its own.
fn config_xml(reference: &str, default_policy: &str, ordered_policy: Option<&str>) -> String {
    let ordered = ordered_policy
        .map(|policy| {
            format!(
                "<CacheBehaviors><Quantity>1</Quantity><Items><CacheBehavior>\
                 <PathPattern>{ORDERED_PATTERN}</PathPattern>\
                 <TargetOriginId>primary</TargetOriginId>\
                 <ViewerProtocolPolicy>{policy}</ViewerProtocolPolicy>\
                 <CachePolicyId>{CACHING_DISABLED}</CachePolicyId>\
                 </CacheBehavior></Items></CacheBehaviors>"
            )
        })
        .unwrap_or_default();
    format!(
        "<DistributionConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <CallerReference>{reference}</CallerReference>\
         <Origins><Quantity>1</Quantity><Items>\
         <Origin><Id>primary</Id><DomainName>{HOST}</DomainName></Origin>\
         </Items></Origins>\
         <DefaultCacheBehavior><TargetOriginId>primary</TargetOriginId>\
         <ViewerProtocolPolicy>{default_policy}</ViewerProtocolPolicy>\
         <CachePolicyId>{CACHING_DISABLED}</CachePolicyId>\
         </DefaultCacheBehavior>{ordered}\
         <Comment>{reference}</Comment>\
         <Enabled>true</Enabled>\
         </DistributionConfig>"
    )
}

/// Create one deployed distribution and answer the address it published, which is what a
/// viewer dials.
async fn deployed(
    router: &MultiServiceRouter,
    case: &str,
    reference: &str,
    default_policy: &str,
    ordered_policy: Option<&str>,
) -> String {
    let created =
        create_distribution(router, &config_xml(reference, default_policy, ordered_policy)).await;
    assert_eq!(
        created.status, 201,
        "case {case}: expected the create to be accepted, got {} with {}",
        created.status, created.raw
    );
    let id = created
        .root()
        .find_text("Id")
        .unwrap_or_default()
        .to_string();
    let domain = created
        .root()
        .find_text("DomainName")
        .unwrap_or_default()
        .to_string();
    assert!(
        is_reachable_authority(&domain),
        "case {case}: expected an address a viewer can dial, got {domain:?}"
    );
    // A configuration that has not reached the edge locations serves no viewer.
    awaited_deployment(router, case, &id).await;
    domain
}

/// What one viewer request was answered with: the status, and the headers read by name
/// without regard to case (a header name is case-insensitive, RFC 9110).
struct Answered {
    status: u16,
    headers: Vec<(String, String)>,
    raw: String,
}

impl Answered {
    /// One answer as it came off the connection.
    fn read(case: &str, raw: String) -> Self {
        let status = raw
            .split_whitespace()
            .nth(1)
            .and_then(|code| code.parse::<u16>().ok())
            .unwrap_or_else(|| panic!("case {case}: no status line in {raw:?}"));
        let head = raw.split_once("\r\n\r\n").map_or("", |(head, _body)| head);
        let headers = head
            .split("\r\n")
            .skip(1)
            .filter_map(|line| line.split_once(':'))
            .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_string()))
            .collect();
        Self {
            status,
            headers,
            raw,
        }
    }

    /// One response header's value, or `None` when the answer carries none under that name.
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(held, _)| held == name)
            .map(|(_, value)| value.as_str())
    }
}

/// The scheme a row's viewer dials the one published address with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dialled {
    /// The `http://` half of the address — the only scheme either policy acts on.
    Plaintext,
    /// The `https://` half, which the same address serves.
    Tls,
}

/// Send one viewer request to `domain` over `dialled`, with no AWS credential.
async fn view(case: &str, domain: &str, dialled: Dialled, path_and_query: &str) -> Answered {
    match dialled {
        Dialled::Plaintext => Answered::read(case, dial(domain, "GET", path_and_query).await),
        Dialled::Tls => {
            let url = format!("https://{domain}{path_and_query}");
            let owned = case.to_string();
            ::tokio::task::spawn_blocking(move || {
                let client = ::reqwest::blocking::Client::builder()
                    .danger_accept_invalid_certs(true)
                    .redirect(::reqwest::redirect::Policy::none())
                    // Generous: nothing here measures the wait, only what came back.
                    .timeout(::std::time::Duration::from_secs(120))
                    .build()
                    .expect("a TLS client");
                let answered = client.get(&url).send().unwrap_or_else(|error| {
                    panic!("case {owned}: the published address did not answer {url}: {error}")
                });
                let status = answered.status().as_u16();
                let headers = answered
                    .headers()
                    .iter()
                    .map(|(name, value)| {
                        (
                            name.as_str().to_ascii_lowercase(),
                            value.to_str().unwrap_or_default().to_string(),
                        )
                    })
                    .collect();
                let raw = answered.text().unwrap_or_default();
                Answered {
                    status,
                    headers,
                    raw,
                }
            })
            .await
            .unwrap_or_else(|error| panic!("case {case}: the dial did not finish: {error}"))
        }
    }
}

/// What a row expects of the answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expected {
    /// The request is served as it arrived: the ORIGIN's own answer, with no redirect and
    /// nothing CloudFront decided itself.
    Served,
    /// CloudFront answers `301` itself, with the same URL over `https` in `Location`.
    Redirected,
    /// CloudFront answers `403` itself, and names no location to go to.
    Forbidden,
}

/// One row: the policy the matched behaviour states, the scheme the viewer dialled, the URL
/// it asked for and the answer it must get.
struct Row {
    case: &'static str,
    default_policy: &'static str,
    ordered_policy: Option<&'static str>,
    dialled: Dialled,
    path_and_query: &'static str,
    expected: Expected,
}

/// The table: every policy value against both schemes, the shapes of URL a redirect has to
/// carry back, and a distribution whose ordered behaviour states a policy its default does
/// not — which is what makes the MATCHED behaviour the thing deciding.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "allow-all, dialled in the clear",
            default_policy: "allow-all",
            ordered_policy: None,
            dialled: Dialled::Plaintext,
            path_and_query: SERVED_PATH,
            expected: Expected::Served,
        },
        Row {
            case: "allow-all, dialled over TLS",
            default_policy: "allow-all",
            ordered_policy: None,
            dialled: Dialled::Tls,
            path_and_query: SERVED_PATH,
            expected: Expected::Served,
        },
        Row {
            case: "redirect-to-https, an object path dialled in the clear",
            default_policy: "redirect-to-https",
            ordered_policy: None,
            dialled: Dialled::Plaintext,
            path_and_query: "/index.html",
            expected: Expected::Redirected,
        },
        Row {
            case: "redirect-to-https, the root dialled in the clear",
            default_policy: "redirect-to-https",
            ordered_policy: None,
            dialled: Dialled::Plaintext,
            path_and_query: "/",
            expected: Expected::Redirected,
        },
        Row {
            case: "redirect-to-https, a nested path with a query dialled in the clear",
            default_policy: "redirect-to-https",
            ordered_policy: None,
            dialled: Dialled::Plaintext,
            path_and_query: "/a/b/c.txt?one=1&two=2",
            expected: Expected::Redirected,
        },
        Row {
            case: "redirect-to-https, dialled over TLS",
            default_policy: "redirect-to-https",
            ordered_policy: None,
            dialled: Dialled::Tls,
            path_and_query: SERVED_PATH,
            expected: Expected::Served,
        },
        Row {
            case: "https-only, dialled in the clear",
            default_policy: "https-only",
            ordered_policy: None,
            dialled: Dialled::Plaintext,
            path_and_query: "/index.html",
            expected: Expected::Forbidden,
        },
        Row {
            case: "https-only, dialled over TLS",
            default_policy: "https-only",
            ordered_policy: None,
            dialled: Dialled::Tls,
            path_and_query: SERVED_PATH,
            expected: Expected::Served,
        },
        Row {
            case: "the ordered behaviour redirects the path it matched",
            default_policy: "allow-all",
            ordered_policy: Some("redirect-to-https"),
            dialled: Dialled::Plaintext,
            path_and_query: SERVED_PATH,
            expected: Expected::Redirected,
        },
        Row {
            case: "the default behaviour serves the path the ordered one does not match",
            default_policy: "allow-all",
            ordered_policy: Some("redirect-to-https"),
            dialled: Dialled::Plaintext,
            path_and_query: UNMATCHED_PATH,
            expected: Expected::Served,
        },
        Row {
            case: "the ordered behaviour serves the path the default one would redirect",
            default_policy: "redirect-to-https",
            ordered_policy: Some("allow-all"),
            dialled: Dialled::Plaintext,
            path_and_query: SERVED_PATH,
            expected: Expected::Served,
        },
        Row {
            case: "the default behaviour redirects the path the ordered one does not match",
            default_policy: "redirect-to-https",
            ordered_policy: Some("allow-all"),
            dialled: Dialled::Plaintext,
            path_and_query: UNMATCHED_PATH,
            expected: Expected::Redirected,
        },
    ]
}

/// Every row's distribution is created, deployed and dialled, and the answer is held to the
/// policy the behaviour its path matched states.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_matched_behaviours_viewer_protocol_policy_decides_the_answer() {
    let router = emulator();
    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        let domain = deployed(
            &router,
            case,
            &format!("vpp-{index}"),
            row.default_policy,
            row.ordered_policy,
        )
        .await;
        let answered = view(case, &domain, row.dialled, row.path_and_query).await;
        let location = answered.header("location").map(str::to_string);
        match row.expected {
            Expected::Served => {
                // The ORIGIN answered it, which is what "served as it arrived" means: the
                // edge reports the answer as one it fetched rather than one it produced.
                assert_eq!(
                    answered.header("x-cache"),
                    Some(MISS),
                    "case {case}: expected the origin's own answer, got {} with {:?}",
                    answered.status,
                    answered.raw
                );
                assert_eq!(
                    location, None,
                    "case {case}: expected no redirect target on a served answer, got \
                     {location:?}"
                );
                if row.path_and_query == SERVED_PATH {
                    assert_eq!(
                        answered.status, 200,
                        "case {case}: expected the origin's object, got {} with {:?}",
                        answered.status, answered.raw
                    );
                }
            }
            Expected::Redirected => {
                assert_eq!(
                    answered.status, 301,
                    "case {case}: expected CloudFront's own 301, got {} with {:?}",
                    answered.status, answered.raw
                );
                // The SAME request's URL over `https`: the host the viewer addressed, its
                // path and its query, unchanged.
                let expected = format!("https://{domain}{}", row.path_and_query);
                assert_eq!(
                    location.as_deref(),
                    Some(expected.as_str()),
                    "case {case}: expected the HTTPS URL of the same request in Location, got \
                     {location:?}"
                );
            }
            Expected::Forbidden => {
                assert_eq!(
                    answered.status, 403,
                    "case {case}: expected CloudFront's own 403, got {} with {:?}",
                    answered.status, answered.raw
                );
                assert_eq!(
                    location, None,
                    "case {case}: expected no redirect target on a refusal, got {location:?}"
                );
            }
        }
    }
}
