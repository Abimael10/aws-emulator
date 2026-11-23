// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of `ListJobTemplates`: the paginated reader over the job
//! templates an account holds.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a restJson1
//! `PUT /job-templates/{jobTemplateId}` and `GET /job-templates[/{jobTemplateId}]` request
//! signed for the `iot` signing name — so what is asserted is what an AWS SDK client would
//! see, not an internal function's return value.
//!
//! The rules under test:
//!
//! * the listing reports ONE ENTRY PER job template the account holds, in `jobTemplateId`
//!   order, each entry projected from the record `CreateJobTemplate` persisted — its ARN,
//!   its id, its description and the instant it was created — so a listed entry and the
//!   `DescribeJobTemplate` of the template it names cannot disagree;
//! * an account that holds no template answers 200 with an EMPTY list and no continuation
//!   token: "the account holds none" is an answer, not a missing one;
//! * `maxResults` bounds the page and `nextToken` is issued only while entries remain and
//!   resumes the SAME listing, so a walk visits every template exactly once whatever the
//!   page size;
//! * a `nextToken` this listing did not issue is refused `InvalidRequestException` (HTTP
//!   400) rather than answered from the middle of some other listing;
//! * the listing is a READ: no request to it creates or changes a template, so a later
//!   caller sees what an earlier one saw.
//!
//! The tables span what the rules generalise over — the characters and length a
//! `jobTemplateId` admits (`^[a-zA-Z0-9_-]+$`, len 1..64,
//! `smithy_query(query="shape",shape="JobTemplateId")`), whether a template names its
//! document inline or by S3 link, how many templates the account holds, and every page
//! size from one to past the number of templates — rather than one recorded request.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The account id every emulated ARN carries (`emulator_server::EMULATOR_ACCOUNT_ID`).
const ACCOUNT_ID: &str = "000000000000";
/// The region the harness signs for.
const REGION: &str = "us-east-1";

/// `InvalidRequestException` as this service NAMES it in `x-amzn-errortype`: the shape
/// name, a `:`, and the service's API doc namespace.
const QUALIFIED_INVALID_REQUEST: &str =
    "InvalidRequestException:http://iot.amazonaws.com/doc/2015-10-02/";

/// A SigV4 `Authorization` header whose credential scope names the `iot` signing name:
/// the scope is how the router picks the claiming service, and how the service picks the
/// region its ARNs are scoped to. The signature itself is never checked here — the harness
/// turns request authentication off, exactly as an unauthenticated emulator runs.
fn signed_for_iot() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDTESTIOT/20260101/{REGION}/iot/aws4_request, \
         SignedHeaders=host;x-amz-date, Signature=0000"
    )
}

/// One response, reduced to what a client branches on.
#[derive(Debug)]
struct Reply {
    status: u16,
    error_code: Option<String>,
    body: Value,
}

impl Reply {
    /// The `jobTemplates` member as the list it is, or an empty list when it is absent —
    /// which the cases below distinguish with [`Self::has_job_templates`].
    fn job_templates(&self) -> Vec<Value> {
        self.body
            .get("jobTemplates")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    }

    /// Whether the answer carries the `jobTemplates` member at all.
    fn has_job_templates(&self) -> bool {
        self.body
            .get("jobTemplates")
            .is_some_and(|templates| templates.is_array())
    }

    /// The `jobTemplateId` of every entry, in the order the response carries them.
    fn ids(&self) -> Vec<String> {
        self.job_templates()
            .iter()
            .map(|entry| member(entry, "jobTemplateId"))
            .collect()
    }

    fn next_token(&self) -> Option<String> {
        self.body
            .get("nextToken")
            .and_then(Value::as_str)
            .map(str::to_string)
    }

    fn message(&self) -> String {
        self.body
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }
}

/// One string member of a JSON object, or the empty string when it is absent.
fn member(value: &Value, name: &str) -> String {
    value
        .get(name)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The emulator under test: ONE shared engine with the IoT service mounted on it, reached
/// the way the production router reaches it.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(engine));
        Self { router }
    }

    async fn call(&self, method: &str, path: &str, body: Option<&Value>) -> Reply {
        let payload = body.map_or_else(Vec::new, |value| {
            serde_json::to_vec(value).expect("a request body serializes")
        });
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://iot.{REGION}.amazonaws.com{path}"))
            .header("authorization", signed_for_iot())
            .header("content-type", "application/json")
            .body(bytes::Bytes::from(payload))
            .expect("a well-formed request");

        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let error_code = response
            .headers()
            .get("x-amzn-errortype")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let bytes = response.into_body();
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        Reply {
            status,
            error_code,
            body,
        }
    }

    async fn create(&self, job_template_id: &str, request: &Value) -> Reply {
        self.call(
            "PUT",
            &format!("/job-templates/{job_template_id}"),
            Some(request),
        )
        .await
    }

    async fn describe(&self, job_template_id: &str) -> Reply {
        self.call("GET", &format!("/job-templates/{job_template_id}"), None)
            .await
    }

    /// `GET /job-templates` with the already-encoded query string `query`.
    async fn list(&self, query: &str) -> Reply {
        let path = if query.is_empty() {
            "/job-templates".to_string()
        } else {
            format!("/job-templates?{query}")
        };
        self.call("GET", &path, None).await
    }
}

/// The job-template ARN this plane mints for `job_template_id`: the `jobtemplate/`
/// resource type AWS IoT names a job template by, under the region the request was signed
/// for and the account this emulator serves.
fn job_template_arn(job_template_id: &str) -> String {
    format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:jobtemplate/{job_template_id}")
}

/// The longest `jobTemplateId` the model admits: 64 characters of `^[a-zA-Z0-9_-]+$`
/// (`smithy_query(query="shape",shape="JobTemplateId")` gives the length as 1..64).
const LONGEST_ID: &str = "a012345678901234567890123456789012345678901234567890123456789_AB";

/// One row of the table: a template to create, and what its listed entry must report.
struct TemplateCase {
    job_template_id: &'static str,
    description: &'static str,
    /// Set when the template names its document inline; otherwise it names an S3 link.
    document: Option<&'static str>,
}

/// The templates every listing case below is built from.
///
/// The rows differ along what a `jobTemplateId` may be — `^[a-zA-Z0-9_-]+$` with length
/// 1..64, so each of its character classes, both cases, the shortest id and one of the
/// longest — and along how the template names the document it deploys, because neither
/// is a member a listing entry carries and so neither may change the listing. They are
/// deliberately NOT in id order: the listing's order is the service's, not the order the
/// creates arrived in.
fn templates() -> Vec<TemplateCase> {
    // The longest row really is the bound: the table spans the length constraint rather
    // than claiming to.
    assert_eq!(
        LONGEST_ID.len(),
        64,
        "case longest id: expected the greatest length JobTemplateId admits, got {}",
        LONGEST_ID.len()
    );
    vec![
        TemplateCase {
            job_template_id: "reboot-template",
            description: "reboot the fleet",
            document: Some(r#"{"operation":"reboot"}"#),
        },
        TemplateCase {
            job_template_id: "A",
            description: "the shortest id an account can take",
            document: Some(r#"{"operation":"ping"}"#),
        },
        TemplateCase {
            job_template_id: "Firmware_Update_2",
            description: "install firmware 2.0",
            document: None,
        },
        TemplateCase {
            job_template_id: "0000",
            description: "digits only",
            document: Some(r#"{"operation":"noop"}"#),
        },
        TemplateCase {
            job_template_id: "zzzz-last-of-the-listing",
            description: "the id that sorts last",
            document: Some(r#"{"operation":"reset"}"#),
        },
        TemplateCase {
            job_template_id: LONGEST_ID,
            description: "an id of the greatest length the model admits",
            document: None,
        },
    ]
}

/// The create request body for one row.
fn create_request(case: &TemplateCase) -> Value {
    match case.document {
        Some(document) => json!({
            "description": case.description,
            "document": document,
        }),
        None => json!({
            "description": case.description,
            "documentSource": "https://s3.us-east-1.amazonaws.com/example-firmware/device-firmware.1.0",
        }),
    }
}

/// An emulator holding every row of [`templates`], created through the wire path.
async fn emulator_with_templates() -> Emulator {
    let emulator = Emulator::new();
    for case in templates() {
        let reply = emulator
            .create(case.job_template_id, &create_request(&case))
            .await;
        assert_eq!(
            reply.status,
            200,
            "case {:?}: expected the create to answer 200, got {reply:?}",
            case.job_template_id
        );
    }
    emulator
}

/// The ids of [`templates`] in the order the listing must report them: ascending by
/// `jobTemplateId`, the identifier the store keys a template by.
fn expected_ids() -> Vec<String> {
    let mut ids: Vec<String> = templates()
        .iter()
        .map(|case| case.job_template_id.to_string())
        .collect();
    ids.sort();
    ids
}

/// Walks the listing with `page_size` entries per page, following `nextToken` until the
/// listing stops issuing one, and answers the ids it reported in order.
async fn walk(emulator: &Emulator, page_size: usize) -> Vec<String> {
    let mut walked: Vec<String> = Vec::new();
    let mut token: Option<String> = None;
    // One request per template at most, plus one: a listing that issued a token for every
    // entry would still terminate, and a loop that did not would be a defect this bound
    // catches.
    for _ in 0..=expected_ids().len() {
        let query = match token.as_deref() {
            None => format!("maxResults={page_size}"),
            Some(token) => format!("maxResults={page_size}&nextToken={token}"),
        };
        let reply = emulator.list(&query).await;
        assert_eq!(
            reply.status, 200,
            "case page size {page_size}: expected 200, got {reply:?}"
        );
        let ids = reply.ids();
        assert!(
            ids.len() <= page_size,
            "case page size {page_size}: expected at most {page_size} entries, got {ids:?}"
        );
        walked.extend(ids);
        token = reply.next_token();
        if token.is_none() {
            break;
        }
    }
    assert_eq!(
        token, None,
        "case page size {page_size}: expected the walk to end without a token, got {token:?}"
    );
    walked
}

/// RULE: the listing reports one entry per template the account holds, in `jobTemplateId`
/// order, each projected from the record the create persisted.
#[tokio::test]
async fn the_listing_reports_every_template_the_account_holds_in_id_order() {
    let emulator = emulator_with_templates().await;
    let reply = emulator.list("").await;
    assert_eq!(reply.status, 200, "listing: expected 200, got {reply:?}");
    assert_eq!(
        reply.ids(),
        expected_ids(),
        "listing: expected every template once in id order, got {:?}",
        reply.ids()
    );

    for case in templates() {
        let id = case.job_template_id;
        let entry = reply
            .job_templates()
            .into_iter()
            .find(|entry| member(entry, "jobTemplateId") == id)
            .unwrap_or_else(|| panic!("case {id:?}: expected the listing to carry an entry"));
        assert_eq!(
            member(&entry, "jobTemplateArn"),
            job_template_arn(id),
            "case {id:?}: expected the ARN the create minted, got {:?}",
            member(&entry, "jobTemplateArn")
        );
        assert_eq!(
            member(&entry, "description"),
            case.description,
            "case {id:?}: expected the stored description, got {:?}",
            member(&entry, "description")
        );
        assert!(
            entry.get("createdAt").is_some_and(|created| created.is_number()),
            "case {id:?}: expected the instant of the create as a timestamp, got {:?}",
            entry.get("createdAt")
        );
    }
}

/// RULE: a listed entry and the describe of the template it names report the same stored
/// record, so the two cannot drift.
#[tokio::test]
async fn a_listed_entry_agrees_with_the_describe_of_the_template_it_names() {
    let emulator = emulator_with_templates().await;
    let listing = emulator.list("").await;
    assert_eq!(listing.status, 200, "listing: expected 200, got {listing:?}");

    for entry in listing.job_templates() {
        let id = member(&entry, "jobTemplateId");
        let described = emulator.describe(&id).await;
        assert_eq!(
            described.status, 200,
            "case {id:?}: expected the describe to answer 200, got {described:?}"
        );
        // Every member `JobTemplateSummary` declares
        // (`smithy_query(query="shape",shape="JobTemplateSummary")`).
        for name in ["jobTemplateArn", "jobTemplateId", "description"] {
            assert_eq!(
                member(&entry, name),
                member(&described.body, name),
                "case {id:?}/{name}: expected the listing and the describe to agree, got {:?} \
                 against {:?}",
                member(&entry, name),
                member(&described.body, name)
            );
        }
        assert_eq!(
            entry.get("createdAt"),
            described.body.get("createdAt"),
            "case {id:?}/createdAt: expected the listing and the describe to agree, got {:?} \
             against {:?}",
            entry.get("createdAt"),
            described.body.get("createdAt")
        );
    }
}

/// RULE: an account holding no template answers with an EMPTY list and no continuation
/// token, whatever page the request asks for.
#[tokio::test]
async fn an_account_holding_no_template_is_answered_with_an_empty_list() {
    let emulator = Emulator::new();
    // The rows differ along what the request asks for: nothing, the smallest page and the
    // largest the model admits (`LaserMaxResults`, range 1..250).
    for query in ["", "maxResults=1", "maxResults=250"] {
        let reply = emulator.list(query).await;
        assert_eq!(
            reply.status, 200,
            "case {query:?}: expected 200, got {reply:?}"
        );
        assert!(
            reply.has_job_templates(),
            "case {query:?}: expected the jobTemplates member to be present, got {reply:?}"
        );
        assert!(
            reply.job_templates().is_empty(),
            "case {query:?}: expected no entries, got {:?}",
            reply.job_templates()
        );
        assert_eq!(
            reply.next_token(),
            None,
            "case {query:?}: expected no continuation token, got {:?}",
            reply.next_token()
        );
    }
}

/// RULE: `maxResults` bounds the page and `nextToken` resumes the same listing, so a walk
/// visits every template exactly once whatever the page size.
#[tokio::test]
async fn pages_of_every_size_walk_the_templates_exactly_once() {
    let emulator = emulator_with_templates().await;
    let expected = expected_ids();
    // Every page size from one to one past the number of templates: the page boundary has
    // to fall inside, exactly on, and past the end of the listing.
    for page_size in 1..=expected.len() + 1 {
        let walked = walk(&emulator, page_size).await;
        assert_eq!(
            walked, expected,
            "case page size {page_size}: expected every template once in order, got {walked:?}"
        );
    }
}

/// RULE: `maxResults` outside the range the model fixes (`LaserMaxResults`, 1..250) is
/// REFUSED rather than answered from a coerced page — the constraint is the model's, so a
/// request that breaks it never reaches the listing.
#[tokio::test]
async fn a_max_results_outside_the_modelled_range_is_refused() {
    let emulator = emulator_with_templates().await;
    // One row either side of each published bound, and the bounds themselves, which are
    // answered.
    let cases: [(&str, i64, bool); 6] = [
        ("below the minimum", 0, false),
        ("negative", -1, false),
        ("the minimum", 1, true),
        ("the maximum", 250, true),
        ("past the maximum", 251, false),
        ("far past the maximum", 10_000, false),
    ];
    for (case, max_results, admitted) in cases {
        let reply = emulator.list(&format!("maxResults={max_results}")).await;
        if admitted {
            assert_eq!(
                reply.status, 200,
                "case {case:?} ({max_results}): expected 200, got {reply:?}"
            );
        } else {
            assert_eq!(
                reply.status, 400,
                "case {case:?} ({max_results}): expected the request to be refused, got {reply:?}"
            );
            assert!(
                reply.job_templates().is_empty(),
                "case {case:?} ({max_results}): expected no page on a refused request, got {:?}",
                reply.job_templates()
            );
        }
    }
}

/// RULE: a `nextToken` this listing did not issue is refused `InvalidRequestException`
/// (HTTP 400) rather than answered from the middle of another listing.
#[tokio::test]
async fn a_token_this_listing_did_not_issue_is_refused() {
    let emulator = emulator_with_templates().await;
    // The rows span the ways a token can fail to be one of this listing's: not hex at all,
    // hex of text carrying no position, and hex of a position issued under another
    // listing's scope.
    let cases = [
        ("not a token", "zzzz".to_string()),
        ("hex of no position", ::hex::encode("job-templates")),
        (
            "a position of another listing",
            ::hex::encode(format!("things\u{1f}{}", expected_ids()[0])),
        ),
    ];
    for (case, token) in cases {
        let reply = emulator.list(&format!("nextToken={token}")).await;
        assert_eq!(
            reply.status, 400,
            "case {case:?}: expected 400, got {reply:?}"
        );
        assert_eq!(
            reply.error_code.as_deref(),
            Some(QUALIFIED_INVALID_REQUEST),
            "case {case:?}: expected InvalidRequestException, got {reply:?}"
        );
        assert_eq!(
            reply.message(),
            "nextToken is not a continuation token of this listing",
            "case {case:?}: expected the sentence about the token, got {:?}",
            reply.message()
        );
    }
}

/// RULE: the listing is a READ — no request to it, answered or refused, creates or changes
/// a template, so a later caller sees what an earlier one saw.
#[tokio::test]
async fn listing_changes_nothing_a_later_caller_sees() {
    let emulator = emulator_with_templates().await;
    let before = emulator.list("").await;

    // Every way the listing can be reached, including the ones that are refused.
    let _ = emulator.list("maxResults=1").await;
    let _ = emulator.list("maxResults=2").await;
    let _ = emulator.list("nextToken=zzzz").await;
    let token = emulator
        .list("maxResults=1")
        .await
        .next_token()
        .expect("a store of more than one template issues a token at page size one");
    let _ = emulator.list(&format!("maxResults=1&nextToken={token}")).await;

    let after = emulator.list("").await;
    assert_eq!(
        after.job_templates(),
        before.job_templates(),
        "expected the listing to report the same entries as before it was read, got {:?} \
         against {:?}",
        after.job_templates(),
        before.job_templates()
    );
}
