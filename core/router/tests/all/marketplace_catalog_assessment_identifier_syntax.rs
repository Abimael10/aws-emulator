// SPDX-License-Identifier: CC0-1.0

//! What a Marketplace Catalog refusal publishes as the required syntax of
//! `DescribeAssessmentRequest$AssessmentIdentifier` is the SERVICE's own fixed text — the
//! same string for every rejected value — and never the shape's modelled `@pattern`
//! rendered out of the emulator's model data.
//!
//! One rule is under test: a refusal that quotes the syntax a member must match states
//! that syntax as fixed text the service publishes, with only the submitted value
//! interpolated, so a caller that pre-validates against the advertised string validates
//! against the grammar the service advertises. For this member the two strings differ:
//! the published one is two SEPARATELY anchored alternatives joined by `|`, the ARN branch
//! carrying a bare `::` where the modelled pattern carries an `[0-9]*` account segment
//! inside one anchored group, so a message built from the model is detectable here.
//!
//! The rows span what the rule generalises over — the submitted value's form (a bare word,
//! a near-miss of either accepted form, an ARN naming another namespace, catalog or
//! resource type), its length, its character classes and the empty value — and the fixed
//! tail of every answer is asserted to be the one published string, byte for byte. The
//! neighbouring half of the contract is pinned beside it: an identifier INSIDE the accepted
//! forms reaches the store and is reported missing (404) rather than refused for its
//! syntax, which is what keeps the refusal from swallowing well-formed reads.
//!
//! It is driven through the ASSEMBLED emulator, so the answer is the one a client of the
//! booted router sees. Assembling the router costs stack (see `boot_serve.rs`), so it is
//! built — and the requests are driven — on an explicitly sized thread.

use std::sync::Arc;

use aws_emulator_router::{build_router, InMemoryEngine};
use serde_json::{json, Value};

/// Keep this at or above `boot_serve.rs`'s stack: the same assembly runs here.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// A SigV4 credential scope naming this service's signing name (`aws-marketplace`), which
/// is what routes the request to the Marketplace family.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260205/us-east-1/aws-marketplace/aws4_request, \
     SignedHeaders=host, Signature=deadbeef";

/// The catalog every operation's `Catalog` member documents as its fixed value.
const CATALOG: &str = "AWSMarketplace";

/// The syntax the service publishes for an `AssessmentIdentifier` and reports in the
/// refusal for a value outside it: the two forms the member's own doc documents ("You can
/// provide either the assessment ID … or the full assessment ARN",
/// `smithy_query(query="doc", shape="DescribeAssessmentRequest",
/// member="AssessmentIdentifier")`), written as two separately anchored alternatives
/// joined by `|` with no account field in the ARN branch.
///
/// Written out here rather than read from the crate so a message built from the crate's
/// own data cannot make this suite pass by quoting it back.
const PUBLISHED_SYNTAX: &str = concat!(
    r"(^assessment-[a-zA-Z0-9]+$)|(^arn:[a-zA-Z0-9-]+:[a-zA-Z0-9-]+:[a-zA-Z0-9-]*::",
    r"[a-zA-Z0-9-]+/Assessment/assessment-[a-zA-Z0-9]+$)"
);

/// The MODELLED constraint of the same member (`smithy_query(query="shape",
/// shape="AssessmentIdentifier")`): one anchored group holding both alternatives, with an
/// `[0-9]*` account segment in the ARN one. It is the shape's `@pattern`, not the string
/// the service publishes, so no refusal may carry it.
const MODELLED_PATTERN: &str = concat!(
    r"^(assessment-[a-zA-Z0-9]+|arn:[a-zA-Z0-9-]+:[a-zA-Z0-9-]+:[a-zA-Z0-9-]*:[0-9]*:",
    r"[a-zA-Z0-9-]+/Assessment/assessment-[a-zA-Z0-9]+)$"
);

/// The account-id segment the modelled `@pattern` advertises in its ARN alternative and
/// the published syntax does not: the part of a model-rendered message that survives any
/// re-spelling of the anchors, so it is asserted against on its own.
const MODELLED_ACCOUNT_SEGMENT: &str = ":[0-9]*:";

/// What the emulator answered: the HTTP status and the JSON body.
struct Answer {
    status: u16,
    body: Value,
}

impl Answer {
    /// The wire error code (`__type`), or the empty string on a success.
    fn error_code(&self) -> &str {
        let raw = self.body["__type"].as_str().unwrap_or_default();
        raw.rsplit('#').next().unwrap_or(raw)
    }

    /// The fault this answer carries: every error of this service spells it `Message`.
    fn message(&self) -> String {
        self.body["Message"].as_str().unwrap_or_default().to_string()
    }
}

/// `DescribeAssessment` over the assembled router, for `identifier`.
async fn describe_assessment(
    router: &smithy_server_runtime::MultiServiceRouter,
    identifier: &str,
) -> Answer {
    let payload = json!({"Catalog": CATALOG, "AssessmentIdentifier": identifier}).to_string();
    let request = http::Request::builder()
        .method("POST")
        .uri("http://localhost/DescribeAssessment")
        .header("host", "catalog.marketplace.us-east-1.amazonaws.com")
        .header("authorization", AUTHORIZATION)
        .header("content-type", "application/json")
        .body(bytes::Bytes::from(payload))
        .expect("a well-formed request");

    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let rendered = response.body().clone();
    let body = if rendered.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&rendered).unwrap_or_else(|error| {
            panic!(
                "DescribeAssessment answered a body that is not JSON ({error}): {:?}",
                String::from_utf8_lossy(&rendered)
            )
        })
    };
    Answer { status, body }
}

/// The sentence this service states for an identifier outside the syntax it publishes,
/// built from the request: the bracketed finding, the value quoted, and the published
/// syntax as its fixed tail.
fn refusal(identifier: &str) -> String {
    format!(
        "[Requested assessment identifier '{identifier}' is invalid. \
         It should match with {PUBLISHED_SYNTAX}.]"
    )
}

/// The fixed part of a refusal: everything from the end of the quoted value on. Two
/// refusals of two different values must carry the same one — that is what makes the
/// syntax published text rather than something rendered per request.
fn fixed_tail(message: &str) -> String {
    message
        .rsplit_once("' is invalid. ")
        .map(|(_, tail)| tail.to_string())
        .unwrap_or_else(|| message.to_string())
}

/// RULE: a member refused for its syntax is refused with the syntax the SERVICE publishes
/// for it, as fixed text identical across every rejected value and never the shape's
/// modelled `@pattern`; a value inside that syntax is not refused at all but reaches the
/// store, where an identifier naming no assessment is reported missing.
#[test]
fn an_assessment_identifier_refusal_publishes_the_services_own_syntax() {
    std::thread::Builder::new()
        .name("marketplace-catalog-assessment-identifier".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
            let mut router = build_router(engine);
            router.set_request_authentication(false);

            let runtime = tokio::runtime::Builder::new_current_thread()
                .build()
                .expect("a current-thread runtime");

            // (case, identifier) — values outside BOTH forms the service publishes. The
            // rows differ in form, length and character class: a plain word, nothing at
            // all, each accepted form missing one of its parts, a character neither form
            // admits, and an ARN whose shape is right while the namespace, catalog,
            // resource type or trailing path is not.
            let malformed: Vec<(&str, String)> = vec![
                ("a plain word", "not-a-valid-identifier".to_string()),
                ("nothing at all", String::new()),
                ("the id prefix with no id", "assessment-".to_string()),
                ("the id prefix capitalised", "Assessment-1".to_string()),
                ("the id prefix upper cased", "ASSESSMENT-1".to_string()),
                (
                    "an id carrying a character neither form admits",
                    "assessment-ab_cd".to_string(),
                ),
                ("an id carrying a space", "assessment-12 34".to_string()),
                ("an id carrying a non-ascii letter", "assessment-é1".to_string()),
                ("an id with no prefix", "12345".to_string()),
                (
                    "a long value in neither form",
                    "z".repeat(300),
                ),
                (
                    "an arn naming another service's namespace",
                    format!("arn:aws:ec2:us-east-1::{CATALOG}/Assessment/assessment-1"),
                ),
                (
                    "an arn naming another catalog",
                    "arn:aws:aws-marketplace:us-east-1::OtherCatalog/Assessment/assessment-1"
                        .to_string(),
                ),
                (
                    "an arn naming another resource type",
                    format!("arn:aws:aws-marketplace:us-east-1::{CATALOG}/Entity/assessment-1"),
                ),
                (
                    "an arn with more path past the assessment",
                    format!(
                        "arn:aws:aws-marketplace:us-east-1::{CATALOG}/Assessment/assessment-1/x"
                    ),
                ),
                (
                    "an arn whose assessment id is empty",
                    format!("arn:aws:aws-marketplace:us-east-1::{CATALOG}/Assessment/assessment-"),
                ),
            ];

            let mut tails: Vec<(String, String)> = Vec::new();
            for (case, identifier) in &malformed {
                let answer = runtime.block_on(describe_assessment(&router, identifier));
                assert_eq!(
                    (answer.status, answer.error_code()),
                    (422, "ValidationException"),
                    "case {case:?}: an identifier outside the published syntax is the \
                     modelled ValidationException and its 422, got {} {:?} with {:?}",
                    answer.status,
                    answer.error_code(),
                    answer.body
                );
                let message = answer.message();
                assert_eq!(
                    message,
                    refusal(identifier),
                    "case {case:?}: the refusal must quote the submitted value inside the \
                     sentence the service states, with the syntax it publishes as the \
                     fixed tail, got {message:?}"
                );
                assert!(
                    !message.contains(MODELLED_PATTERN),
                    "case {case:?}: the refusal must not publish the shape's modelled \
                     @pattern, got {message:?}"
                );
                assert!(
                    !message.contains(MODELLED_ACCOUNT_SEGMENT),
                    "case {case:?}: the published syntax advertises no account-id segment \
                     in its ARN branch, got {message:?}"
                );
                tails.push(((*case).to_string(), fixed_tail(&message)));
            }

            // The tail is FIXED text: every rejected value, whatever its form or length,
            // is answered with the same published syntax.
            let expected_tail = format!("It should match with {PUBLISHED_SYNTAX}.]");
            for (case, tail) in &tails {
                assert_eq!(
                    *tail, expected_tail,
                    "case {case:?}: every refusal of this member must carry the one \
                     published syntax, got {tail:?}"
                );
            }

            // (case, identifier, the assessment it names) — values INSIDE the accepted
            // forms, which are not refused for their syntax but looked up. The ARN form
            // is admitted whatever partition, region and account it names: those say
            // where an assessment lives, not which one it is.
            let well_formed: [(&str, String, &str); 6] = [
                ("the shortest id", "assessment-1".to_string(), "assessment-1"),
                (
                    "an alphanumeric id",
                    "assessment-zzz000notheld".to_string(),
                    "assessment-zzz000notheld",
                ),
                (
                    "the arn form with no account",
                    format!("arn:aws:aws-marketplace:us-east-1::{CATALOG}/Assessment/assessment-7"),
                    "assessment-7",
                ),
                (
                    "the arn form carrying an account",
                    format!(
                        "arn:aws:aws-marketplace:us-east-1:000000000000:{CATALOG}/Assessment/\
                         assessment-8"
                    ),
                    "assessment-8",
                ),
                (
                    "the arn form with no region",
                    format!("arn:aws:aws-marketplace:::{CATALOG}/Assessment/assessment-9"),
                    "assessment-9",
                ),
                (
                    "the arn form in another partition",
                    format!(
                        "arn:aws-cn:aws-marketplace:cn-north-1::{CATALOG}/Assessment/\
                         assessment-10"
                    ),
                    "assessment-10",
                ),
            ];

            for (case, identifier, named) in &well_formed {
                let answer = runtime.block_on(describe_assessment(&router, identifier));
                assert_eq!(
                    (answer.status, answer.error_code()),
                    (404, "ResourceNotFoundException"),
                    "case {case:?}: an identifier inside the published syntax reaches the \
                     lookup, got {} {:?} with {:?}",
                    answer.status,
                    answer.error_code(),
                    answer.body
                );
                let message = answer.message();
                assert_eq!(
                    message,
                    format!("Requested assessment '{named}' does not exist."),
                    "case {case:?}: a well-formed identifier naming no assessment is \
                     reported missing, naming the assessment it asked for, got {message:?}"
                );
                assert!(
                    !message.contains("It should match with"),
                    "case {case:?}: a missing assessment is not a syntax refusal and \
                     publishes no syntax, got {message:?}"
                );
            }
        })
        .expect("spawn the assessment-identifier thread")
        .join()
        .expect("the refusal must publish the syntax the service publishes");
}
