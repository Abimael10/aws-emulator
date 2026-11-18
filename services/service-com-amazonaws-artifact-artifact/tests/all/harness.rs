// SPDX-License-Identifier: CC0-1.0

//! The one harness the report suites drive the artifact emulator through.
//!
//! Every suite goes over the router, so each case exercises routing, deserialization,
//! the handler, the persisted state and serialization — the same path a client takes.
//! The request helpers and the expected catalog live here rather than in one suite, so
//! two suites cannot disagree about what the service publishes.
//!
//! `report_workflow.rs` and `report_projection.rs` use it. The crate's other suites still
//! carry their own copies of the router bootstrap: each covers another resource and they
//! are being changed concurrently by other issues, so they are left to migrate with their
//! next edit rather than be rewritten from under their owners here. (No list of them is
//! kept here, so this comment cannot rot as suites are added or migrated — `tests/all.rs`
//! is the register.)

// Each suite uses the part of the harness its own rules need; an item no suite has
// reached yet is still part of the contract this file states.
#![allow(dead_code)]

use std::sync::Arc;

use aws_smithy_types::date_time::Format;
use aws_smithy_types::DateTime;
use bytes::Bytes;
use serde_json::Value;
use service_com_amazonaws_artifact_artifact::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// How many digits wide the fraction of a rendered date-time is: the millisecond
/// resolution this service reports an instant at, whatever the value of the milliseconds
/// (a recorded catalog entry carries `2024-11-01T00:00:00.000Z`).
pub const MILLISECOND_DIGITS: usize = 3;

/// Assert one `TimestampAttribute` member's wire form and answer the instant it names.
///
/// The ONE date-time form assertion of this crate's suites — the form is one service-wide
/// rule, so a suite checking a member's wire form builds on this rather than on a copy of
/// it: a JSON STRING (never restJson1's default epoch-seconds number) that the `date-time`
/// format parses, closed by `Z`, carrying a fraction exactly [`MILLISECOND_DIGITS`] wide.
/// A suite's own extra expectations about the instant are asserted by the suite, over the
/// value this hands back.
pub fn assert_rendered_date_time(case: &str, member: &str, value: &Value) -> DateTime {
    assert!(
        !value.is_number(),
        "case {case:?}: {member} targets TimestampAttribute (@timestampFormat date-time), so it \
         must be an RFC 3339 string and never an epoch-seconds number, got {value:?}"
    );
    let text = value.as_str().unwrap_or_else(|| {
        panic!("case {case:?}: {member} must be an RFC 3339 date-time string, got {value:?}")
    });
    let parsed = DateTime::from_str(text, Format::DateTime).unwrap_or_else(|error| {
        panic!("case {case:?}: {member} must parse as a date-time ({error}), got {text:?}")
    });
    assert!(
        text.contains('T'),
        "case {case:?}: {member} must be an ISO-8601 date-time, got {text:?}"
    );
    let fraction = text
        .strip_suffix('Z')
        .and_then(|civil| civil.split_once('.').map(|(_, fraction)| fraction))
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: {member} must be a UTC date-time carrying a fraction of a \
                 second, got {text:?}"
            )
        });
    assert!(
        fraction.len() == MILLISECOND_DIGITS
            && fraction.chars().all(|digit| digit.is_ascii_digit()),
        "case {case:?}: {member} must carry a {MILLISECOND_DIGITS}-digit millisecond fraction, \
         got {text:?}"
    );
    parsed
}

/// The endpoint host every request of this harness is addressed to — and therefore the
/// host an announced document URL names, since a document URL names the endpoint the
/// caller reached the emulator on.
pub const ENDPOINT: &str = "artifact.eu-west-1.amazonaws.com";

/// A SigV4 credential scope naming this service. It is what routes a request to the
/// artifact service; its region is the one the CALLER presented, which is not what an ARN
/// this service mints names (see [`HOME_REGION`]).
pub const SIGNED_FOR_EU_WEST_1: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260101/eu-west-1/artifact/aws4_request, \
     SignedHeaders=host, Signature=abc";

/// The region [`SIGNED_FOR_EU_WEST_1`] signs for.
pub const REGION: &str = "eu-west-1";

/// A credential scope whose region field does not have an AWS region's shape. It still
/// names the `artifact` signing name, so the request routes and the handler runs.
pub const SIGNED_FOR_A_MALFORMED_REGION: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260101/not_a_region/artifact/aws4_request, \
     SignedHeaders=host, Signature=abc";

/// The Region every ARN this service mints names, whatever Region a request was signed
/// for: this service's endpoints are partitional, so a call reaches it in the partition's
/// one Region and the resources it reports are homed there
/// (`crate::state::SERVICE_REGION`).
pub const HOME_REGION: &str = "us-east-1";

/// A credential scope for `region`, in the shape SigV4 declares.
pub fn signed_for(region: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{region}/artifact/aws4_request, \
         SignedHeaders=host, Signature=abc"
    )
}

/// `AcceptanceType$EXPLICIT`, whose member doc in this service's Smithy model
/// (`smithy_query` shape=AcceptanceType) is "Require explicit click-through acceptance of
/// the Term associated with this Report."
pub const ACCEPTANCE_EXPLICIT: &str = "EXPLICIT";

/// `AcceptanceType$PASSTHROUGH`, whose member doc in the same model is "Do not require
/// explicit click-through acceptance of the Term associated with this Report".
pub const ACCEPTANCE_PASSTHROUGH: &str = "PASSTHROUGH";

/// One row of the service-owned catalog this emulator publishes.
pub struct Expected {
    pub id: &'static str,
    pub name: &'static str,
    pub series: &'static str,
    pub category: &'static str,
    /// One of `AcceptanceType`'s values (`EXPLICIT` | `PASSTHROUGH`).
    pub acceptance_type: &'static str,
    /// Every published version of this report, ascending.
    pub versions: &'static [i64],
}

impl Expected {
    /// The newest version this report publishes — the one `ListReports` projects.
    pub fn latest(&self) -> i64 {
        self.versions
            .iter()
            .copied()
            .next_back()
            .expect("every catalog row publishes a version")
    }

    /// Whether downloading this report requires the explicit click-through acceptance of
    /// the term governing it — what its `AcceptanceType` says
    /// ([`ACCEPTANCE_EXPLICIT`] / [`ACCEPTANCE_PASSTHROUGH`]).
    pub fn requires_explicit_acceptance(&self) -> bool {
        self.acceptance_type == ACCEPTANCE_EXPLICIT
    }
}

/// A catalog row whose download requires the explicit acceptance of its term: the reports
/// a term grant is the receipt for, which is what a suite asserting that gate names.
pub fn a_report_requiring_acceptance() -> &'static Expected {
    CATALOG
        .iter()
        .find(|row| row.requires_explicit_acceptance())
        .expect("the catalog publishes a report requiring explicit acceptance")
}

/// The catalog the emulator serves, in the ascending report-id order `ListReports` uses.
///
/// AWS Artifact has no `CreateReport`: the catalog is service-owned, so these rows are
/// the published report set. They are spelled out here, independently of the seed in
/// `src/state.rs`, so a changed or fabricated catalog fails these suites rather than
/// quietly redefining what they assert. The ids and names AWS publishes are the ones a
/// recording of a live `ListReports` shows (`report-073F0fUCdVVNKJrF`,
/// `report-0c1IXmudeXuTlHJJ`, `report-5eevElBv6MyRmQsD`, and `report-7g5j2DTVeNXWx7St`
/// and `report-0QjvdIs6Bsw4uPxb` of the accessibility-conformance class); every other row
/// is a report AWS publishes in Artifact under an id this emulator assigns (see `SEED` and
/// `accessibility_reports` in `src/state.rs`).
///
/// A published title is spelled as it is published, punctuation included: the recorded
/// entry of the accessibility-conformance class is titled
/// "Accessibility Conformance Report (VPAT) - <product>", which `ShortStringAttribute`'s
/// `@pattern` (a bound on what a REQUEST may send) does not admit.
///
/// The rows differ along everything the rules generalise over: report series, category,
/// `AcceptanceType`, and how many versions a report has (one, two, three and seven). The
/// catalog holds both of the classes AWS publishes: one row per compliance programme, and
/// one row per product evaluated for accessibility conformance (the `ACR` series).
pub const CATALOG: &[Expected] = &[
    Expected {
        id: "report-073F0fUCdVVNKJrF",
        name: "AWS Outposts Rack Information Security FAQ",
        series: "AWS Services",
        category: "Alignment Documents",
        acceptance_type: "PASSTHROUGH",
        versions: &[1, 2, 3],
    },
    Expected {
        id: "report-0QjvdIs6Bsw4uPxb",
        name: "Accessibility Conformance Report (VPAT) - Amazon Connect",
        series: "ACR",
        category: "Certifications and Attestations",
        acceptance_type: "PASSTHROUGH",
        versions: &[1, 2],
    },
    Expected {
        id: "report-0c1IXmudeXuTlHJJ",
        name: "ISO 27001 2022 Certification",
        series: "ISO",
        category: "Certifications and Attestations",
        acceptance_type: "PASSTHROUGH",
        versions: &[1, 2, 3],
    },
    Expected {
        id: "report-5eevElBv6MyRmQsD",
        name: "UAE IAR Compliance Report",
        series: "UAE TDRA IAR",
        category: "Certifications and Attestations",
        acceptance_type: "PASSTHROUGH",
        versions: &[1, 2, 3, 4, 5, 6, 7],
    },
    Expected {
        id: "report-7g5j2DTVeNXWx7St",
        name: "Accessibility Conformance Report (VPAT) - AWS Transfer Family",
        series: "ACR",
        category: "Certifications and Attestations",
        acceptance_type: "PASSTHROUGH",
        versions: &[1],
    },
    Expected {
        id: "report-9pQ2mT8xLbV4sZ1c",
        name: "SOC 2 Type 2 Report",
        series: "SOC",
        category: "Certifications and Attestations",
        acceptance_type: "EXPLICIT",
        versions: &[1, 2],
    },
    Expected {
        id: "report-Af3KdN7yRw2HqS5e",
        name: "PCI DSS Attestation of Compliance",
        series: "PCI",
        category: "Certifications and Attestations",
        acceptance_type: "EXPLICIT",
        versions: &[1, 2],
    },
    Expected {
        id: "report-Bq7WmF3xTk9ZpL2a",
        name: "SOC 1 Report",
        series: "SOC",
        category: "Certifications and Attestations",
        acceptance_type: "EXPLICIT",
        versions: &[1, 2],
    },
    Expected {
        id: "report-C4tYhR8nJd6VxQ3b",
        name: "SOC 3 Report",
        series: "SOC",
        category: "Certifications and Attestations",
        acceptance_type: "PASSTHROUGH",
        versions: &[1],
    },
    Expected {
        id: "report-D9kPzX2mLb7TnR4c",
        name: "ISO 27017 Certification",
        series: "ISO",
        category: "Certifications and Attestations",
        acceptance_type: "PASSTHROUGH",
        versions: &[1, 2],
    },
    Expected {
        id: "report-E2nVbK6yQw8MsZ5d",
        name: "ISO 27018 Certification",
        series: "ISO",
        category: "Certifications and Attestations",
        acceptance_type: "PASSTHROUGH",
        versions: &[1],
    },
    Expected {
        id: "report-F6sJdL9xRt3WqN7e",
        name: "ISO 27701 Certification",
        series: "ISO",
        category: "Certifications and Attestations",
        acceptance_type: "PASSTHROUGH",
        versions: &[1],
    },
    Expected {
        id: "report-G3mQfT7bXz5KpV8h",
        name: "ISO 9001 Certification",
        series: "ISO",
        category: "Certifications and Attestations",
        acceptance_type: "PASSTHROUGH",
        versions: &[1],
    },
    Expected {
        id: "report-H8wRnP4cYk2JsL6m",
        name: "FedRAMP Moderate Customer Package",
        series: "FedRAMP",
        category: "Certifications and Attestations",
        acceptance_type: "EXPLICIT",
        versions: &[1],
    },
    Expected {
        id: "report-J5zXkM9dVb3QtR7n",
        name: "IRAP Assessment Report",
        series: "IRAP",
        category: "Certifications and Attestations",
        acceptance_type: "EXPLICIT",
        versions: &[1],
    },
    Expected {
        id: "report-K7bNqS2fWx8LmT4p",
        name: "C5 Attestation Report",
        series: "C5",
        category: "Certifications and Attestations",
        acceptance_type: "EXPLICIT",
        versions: &[1, 2],
    },
    Expected {
        id: "report-L4dTvJ8hZn6RkQ9s",
        name: "Cloud Security Alliance CAIQ",
        series: "CSA",
        category: "Alignment Documents",
        acceptance_type: "PASSTHROUGH",
        versions: &[1],
    },
    Expected {
        id: "report-M2hXsL5bQw9TnF4j",
        name: "Accessibility Conformance Report (VPAT) - Amazon WorkSpaces",
        series: "ACR",
        category: "Certifications and Attestations",
        acceptance_type: "PASSTHROUGH",
        versions: &[1, 2],
    },
    Expected {
        id: "report-N7kDvR3mYt8ZpB6c",
        name: "Accessibility Conformance Report (VPAT) - Amazon QuickSight",
        series: "ACR",
        category: "Certifications and Attestations",
        acceptance_type: "PASSTHROUGH",
        versions: &[1],
    },
    Expected {
        id: "report-P5qZmJ8hXc2VtL9d",
        name: "Accessibility Conformance Report (VPAT) - Amazon Chime SDK",
        series: "ACR",
        category: "Certifications and Attestations",
        acceptance_type: "PASSTHROUGH",
        versions: &[1, 2, 3],
    },
    Expected {
        id: "report-Q8tBnW4fKz6RmS3h",
        name: "Accessibility Conformance Report (VPAT) - AWS CloudShell",
        series: "ACR",
        category: "Certifications and Attestations",
        acceptance_type: "PASSTHROUGH",
        versions: &[1],
    },
    Expected {
        id: "report-R3vYkT9pLd5XnQ7b",
        name: "Accessibility Conformance Report (VPAT) - Amazon SageMaker Studio",
        series: "ACR",
        category: "Certifications and Attestations",
        acceptance_type: "PASSTHROUGH",
        versions: &[1, 2],
    },
    Expected {
        id: "report-S6mFqH2xVb8JtZ4n",
        name: "Accessibility Conformance Report (VPAT) - Amazon Lex",
        series: "ACR",
        category: "Certifications and Attestations",
        acceptance_type: "PASSTHROUGH",
        versions: &[1],
    },
];

/// One emulator over `engine`, so a suite can reach the same store the handler reads —
/// the secret its pagination cursors are sealed under, for instance.
pub fn emulator_on(engine: Arc<engine::InMemoryEngine>) -> MultiServiceRouter {
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(engine));
    router
}

/// One emulator: a router carrying only this service, over one shared engine.
pub fn emulator() -> MultiServiceRouter {
    emulator_on(Arc::new(engine::InMemoryEngine::new()))
}

/// What the emulator answered: the HTTP status, the `x-amzn-errortype` header a restJson1
/// client resolves an error shape from, and the JSON body.
pub struct Answer {
    pub status: u16,
    /// The `x-amzn-errortype` header, or the empty string when the answer carries none —
    /// which is every success. Carried here so a suite asserting the SHAPE a refusal
    /// travels in does not need a sender of its own.
    pub error_type: String,
    pub body: Value,
}

impl Answer {
    /// The wire error code (`__type`), or the empty string on a success.
    pub fn error_code(&self) -> &str {
        self.body["__type"].as_str().unwrap_or_default()
    }

    /// The `reports` list a paginated report operation returns.
    pub fn reports(&self) -> &[Value] {
        self.body["reports"].as_array().map_or(&[], |items| items)
    }

    /// The `nextToken` the answer carries, if any.
    pub fn next_token(&self) -> Option<String> {
        self.body["nextToken"].as_str().map(str::to_string)
    }
}

pub async fn get(router: &MultiServiceRouter, uri: &str) -> Answer {
    get_signed(router, uri, SIGNED_FOR_EU_WEST_1).await
}

/// One request under ANY method, with the body that method carries: the sender a suite
/// needs for a write, a removal or a read alike, so no suite grows a second copy of the
/// request builder. `get` above is this one specialised to a read.
pub async fn call(router: &MultiServiceRouter, method: &str, uri: &str, body: Bytes) -> Answer {
    send_signed(router, method, uri, body, Some(SIGNED_FOR_EU_WEST_1)).await
}

/// The ONE request builder of this harness: `method` and `uri`, the body that method
/// carries, and the credential scope to present — `None` for a request that presents none
/// at all, which is a request a client can make and which the service still has to answer
/// from its own state.
pub async fn send_signed(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Bytes,
    authorization: Option<&str>,
) -> Answer {
    let mut builder = ::http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", ENDPOINT);
    if !body.is_empty() {
        builder = builder.header("content-type", "application/json");
    }
    if let Some(scope) = authorization {
        builder = builder.header("authorization", scope);
    }
    let request = builder
        .body(body)
        .expect("the test request is well formed");

    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let error_type = error_type_of(&response);
    let bytes = response.into_body();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or_else(|error| {
            panic!(
                "{method} {uri} answered a body that is not JSON ({error}): {:?}",
                String::from_utf8_lossy(&bytes)
            )
        })
    };
    Answer {
        status,
        error_type,
        body,
    }
}

/// The `x-amzn-errortype` header of one answer, or the empty string when it carries none.
fn error_type_of(response: &::http::Response<Bytes>) -> String {
    response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string()
}

pub async fn get_signed(router: &MultiServiceRouter, uri: &str, authorization: &str) -> Answer {
    send_signed(router, "GET", uri, Bytes::new(), Some(authorization)).await
}

pub async fn list_reports(router: &MultiServiceRouter, query: &str) -> Answer {
    get(router, &format!("/v1/report/list{query}")).await
}

/// A case label for one page size, for a suite walking the catalog at several of them.
pub fn page_size_case(max_results: Option<i32>) -> String {
    match max_results {
        None => "maxResults absent".to_string(),
        Some(size) => format!("maxResults={size}"),
    }
}

/// Every entry of ONE whole walk of `ListReports` at `max_results`, in the order the pages
/// reported them, each paired with the 1-based page it came on.
///
/// The ONE catalog walker of this crate's suites: a suite asserting about the entries, the
/// pages they arrive on, or how a page size changes them builds on this rather than on a
/// copy of the paging loop. It asserts only what makes the walk a walk — every page is
/// served, a page holds at most `maxResults` entries, and the walk terminates — so each
/// suite's own rule is asserted over what it hands back.
pub async fn walk_reports(
    router: &MultiServiceRouter,
    case: &str,
    max_results: Option<i32>,
) -> Vec<(usize, Value)> {
    let mut entries: Vec<(usize, Value)> = Vec::new();
    let mut token: Option<String> = None;
    let mut page = 0usize;
    loop {
        page += 1;
        assert!(
            page <= CATALOG.len() + 1,
            "case {case:?}: a walk of {} reports must terminate, got {page} pages",
            CATALOG.len()
        );
        let mut query = String::new();
        if let Some(size) = max_results {
            query.push_str(&format!("?maxResults={size}"));
        }
        if let Some(value) = token.as_deref() {
            query.push(if query.is_empty() { '?' } else { '&' });
            query.push_str(&format!("nextToken={value}"));
        }
        let answer = list_reports(router, &query).await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: page {page} of the enumeration must be served, got {} with {:?}",
            answer.status, answer.body
        );
        if let Some(size) = max_results {
            assert!(
                i32::try_from(answer.reports().len()).unwrap_or(i32::MAX) <= size,
                "case {case:?}: page {page} is cut at maxResults, got {} entries",
                answer.reports().len()
            );
        }
        for summary in answer.reports() {
            entries.push((page, summary.clone()));
        }
        match answer.next_token() {
            None => break,
            Some(issued) => token = Some(issued),
        }
    }
    entries
}

pub async fn list_report_versions(router: &MultiServiceRouter, query: &str) -> Answer {
    get(router, &format!("/v1/report/listVersions{query}")).await
}

pub async fn get_report_metadata(router: &MultiServiceRouter, query: &str) -> Answer {
    get(router, &format!("/v1/report/getMetadata{query}")).await
}

pub async fn get_term_for_report(router: &MultiServiceRouter, query: &str) -> Answer {
    get(router, &format!("/v1/report/getTermForReport{query}")).await
}

pub async fn get_report(router: &MultiServiceRouter, query: &str) -> Answer {
    get(router, &format!("/v1/report/get{query}")).await
}

/// What following an announced document URL served: the status, the content type and the
/// raw bytes — a document is not JSON, so it is kept as bytes rather than parsed.
pub struct Served {
    pub status: u16,
    pub content_type: String,
    pub bytes: Bytes,
}

/// Follow a `documentPresignedUrl` the service announced, exactly as a client does:
/// addressed to the endpoint that URL names ([`ENDPOINT`]) and with NO credential of any
/// kind, since the URL carries the token that authorises the fetch.
///
/// Asserts the announcement is absolute and names this service's own endpoint, so the URL a
/// suite follows is the one a client could follow; `case` names the row that announced it.
pub async fn follow_announced_url(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
) -> Served {
    let rest = url
        .strip_prefix("https://")
        .unwrap_or_else(|| panic!("case {case}: an announced URL is absolute, got {url:?}"));
    let (host, path_and_query) = rest
        .split_once('/')
        .unwrap_or_else(|| panic!("case {case}: an announced URL carries a path, got {url:?}"));
    assert_eq!(
        host, ENDPOINT,
        "case {case}: an announced URL must name the endpoint the caller reached, got {url:?}"
    );

    let request = ::http::Request::builder()
        .method("GET")
        .uri(format!("/{path_and_query}"))
        .header("host", ENDPOINT)
        .body(Bytes::new())
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    Served {
        status,
        content_type,
        bytes: response.into_body(),
    }
}
