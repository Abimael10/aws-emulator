// SPDX-License-Identifier: CC0-1.0

//! `ListPrompts`, end to end over HTTP: the listing enumerates either the WORKING DRAFT
//! of every prompt the account holds, or every version of the one prompt a
//! `promptIdentifier` names, and reports each of them from the stored record the matching
//! `GetPrompt` read reports.
//!
//! The rules under test, each asserted over a table of inputs rather than one row:
//!
//! * with no `promptIdentifier`, one summary per stored prompt, that prompt's working
//!   draft, ordered by the instant the prompt was created;
//! * with a `promptIdentifier` in any form the modelled shape admits, the versions of
//!   the prompt it names — the working draft first, then the numbered snapshots in the
//!   order they were cut — and no other prompt's;
//! * a page carries at most `maxResults` summaries and never more than the ten the
//!   modelled `promptSummaries` list admits, and paging with the `nextToken` a page hands
//!   out reports every summary exactly once;
//! * an identifier of neither modelled form is a bad request, a well-formed identifier
//!   the store does not hold is a missing resource, and a `nextToken` this service never
//!   issued is a bad request;
//! * the listing is a READ: it leaves the prompts, their versions and the next version
//!   number exactly where they stood.
//!
//! Every assertion goes through the router, so it exercises the real wire path (route
//! matching, model constraint validation, serialization) rather than calling a handler
//! directly. The one thing these tests reach into the store for is SPREADING THE CREATION
//! INSTANTS the prompts carry, so the order the listing is asserted in is a property of
//! the stored records and not of how fast the host ran the creates.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_bedrockagent_amazonbedrockagentbuildtimelambda::state::service_state;
use service_com_amazonaws_bedrockagent_amazonbedrockagentbuildtimelambda::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// A SigV4 `Authorization` header scoped to the `bedrock` signing name: the routing key
/// this service claims on, and the source of the region its ARNs carry.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260101/eu-west-1/bedrock/aws4_request, \
     SignedHeaders=host;x-amz-date, Signature=0123456789abcdef";

/// The most summaries one page may carry: the modelled `promptSummaries` is
/// `length ..10` (`smithy_query(query="operation",operation="ListPrompts")`).
const MOST_SUMMARIES_PER_PAGE: usize = 10;

/// How many pages a listing of this test's inputs can possibly need before the paging
/// under test is broken rather than merely long.
const PAGE_BUDGET: usize = 64;

/// One value as a percent-encoded URI component, as a client encodes a `@httpQuery`
/// value or a non-greedy `@httpLabel`.
fn encoded(value: &str) -> String {
    value
        .replace('%', "%25")
        .replace(':', "%3A")
        .replace('/', "%2F")
}

/// One HTTP answer from the emulator, parsed.
#[derive(Debug)]
struct Answer {
    status: u16,
    /// The restJson1 error shape name, from `x-amzn-errortype` or the body's `__type`;
    /// `None` on a success.
    error_type: Option<String>,
    body: Value,
}

impl Answer {
    fn member(&self, path: &[&str]) -> &Value {
        let mut at = &self.body;
        for key in path {
            at = &at[*key];
        }
        at
    }

    fn string(&self, path: &[&str]) -> String {
        self.member(path)
            .as_str()
            .unwrap_or_else(|| panic!("{path:?} is not a string in {:?}", self.body))
            .to_string()
    }

    /// The `promptSummaries` this answer reports.
    fn summaries(&self, case: &str) -> Vec<Value> {
        self.member(&["promptSummaries"])
            .as_array()
            .unwrap_or_else(|| {
                panic!(
                    "case {case}: expected a promptSummaries list, got {:?}",
                    self.body
                )
            })
            .clone()
    }

    /// The `nextToken` this answer hands out, if any.
    fn next_token(&self) -> Option<String> {
        self.member(&["nextToken"]).as_str().map(ToString::to_string)
    }
}

/// The emulator under test: one router over one shared engine, exactly as the production
/// router mounts this service.
struct Emulator {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        Self { router, engine }
    }

    async fn call(&self, method: &str, uri: &str, body: Option<Value>) -> Answer {
        let bytes = body
            .map(|value| Bytes::from(serde_json::to_vec(&value).expect("serialize request body")))
            .unwrap_or_default();
        let request = http::Request::builder()
            .method(method)
            .uri(uri)
            .header("host", "bedrock-agent.eu-west-1.amazonaws.com")
            .header("authorization", AUTHORIZATION)
            .header("content-type", "application/json")
            .body(bytes)
            .expect("build the request");

        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let header_error = response
            .headers()
            .get("x-amzn-errortype")
            .and_then(|value| value.to_str().ok())
            .map(ToString::to_string);
        let raw = response.into_body();
        let body: Value = if raw.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&raw).unwrap_or_else(|error| {
                panic!(
                    "{method} {uri} -> {status} body is not JSON ({error}): {}",
                    String::from_utf8_lossy(&raw)
                )
            })
        };
        let error_type = header_error.or_else(|| {
            body.get("__type")
                .and_then(Value::as_str)
                .map(ToString::to_string)
        });
        Answer {
            status,
            error_type,
            body,
        }
    }

    /// `CreatePrompt`, which mints the prompt and the working draft the listing reports.
    async fn create_prompt(&self, case: &str, request: Value) -> Answer {
        let created = self.call("POST", "/prompts/", Some(request)).await;
        assert_eq!(
            created.status, 201,
            "case {case}: expected CreatePrompt -> 201, got {created:?}"
        );
        created
    }

    async fn create_prompt_version(&self, case: &str, identifier: &str, request: Value) -> Answer {
        let cut = self
            .call(
                "POST",
                &format!("/prompts/{}/versions", encoded(identifier)),
                Some(request),
            )
            .await;
        assert_eq!(
            cut.status, 201,
            "case {case}: expected CreatePromptVersion -> 201, got {cut:?}"
        );
        cut
    }

    /// `GetPrompt` on any `PromptIdentifier` form, optionally naming a version.
    async fn get_prompt(&self, identifier: &str, prompt_version: Option<&str>) -> Answer {
        let query = prompt_version
            .map(|version| format!("?promptVersion={version}"))
            .unwrap_or_default();
        self.call(
            "GET",
            &format!("/prompts/{}/{query}", encoded(identifier)),
            None,
        )
        .await
    }

    /// `ListPrompts`, with whichever of its three `@httpQuery` members the case names.
    async fn list_prompts(
        &self,
        identifier: Option<&str>,
        max_results: Option<i32>,
        next_token: Option<&str>,
    ) -> Answer {
        let mut query: Vec<String> = Vec::new();
        if let Some(identifier) = identifier {
            query.push(format!("promptIdentifier={}", encoded(identifier)));
        }
        if let Some(max_results) = max_results {
            query.push(format!("maxResults={max_results}"));
        }
        if let Some(next_token) = next_token {
            query.push(format!("nextToken={}", encoded(next_token)));
        }
        let uri = if query.is_empty() {
            "/prompts/".to_string()
        } else {
            format!("/prompts/?{}", query.join("&"))
        };
        self.call("GET", &uri, None).await
    }

    /// Every summary the listing reports for `identifier`, read page by page with the
    /// `nextToken` each page hands out. Asserts along the way that no page is longer than
    /// the bound in force.
    async fn drain(&self, case: &str, identifier: Option<&str>, max_results: Option<i32>) -> Vec<Value> {
        let bound = max_results
            .map(|requested| usize::try_from(requested).unwrap_or(MOST_SUMMARIES_PER_PAGE))
            .unwrap_or(MOST_SUMMARIES_PER_PAGE)
            .min(MOST_SUMMARIES_PER_PAGE);
        let mut collected: Vec<Value> = Vec::new();
        let mut token: Option<String> = None;
        for page in 0..PAGE_BUDGET {
            let answer = self
                .list_prompts(identifier, max_results, token.as_deref())
                .await;
            assert_eq!(
                answer.status, 200,
                "case {case}: expected ListPrompts -> 200 on page {page}, got {answer:?}"
            );
            let summaries = answer.summaries(case);
            assert!(
                summaries.len() <= bound,
                "case {case}: expected at most {bound} summaries on page {page}, \
                 got {} in {answer:?}",
                summaries.len()
            );
            collected.extend(summaries.iter().cloned());
            match answer.next_token() {
                None => return collected,
                Some(next) => {
                    assert!(
                        !summaries.is_empty(),
                        "case {case}: a page that hands out {next:?} reported something, \
                         got {answer:?}"
                    );
                    token = Some(next);
                }
            }
        }
        panic!("case {case}: the listing still had not finished after {PAGE_BUDGET} pages");
    }

    /// Spread the creation instants of the prompts `prompt_ids` names, in the order
    /// given: the first carries the earliest instant. The order the listing reports is a
    /// property of those stored instants, so setting them is what makes the expected
    /// order of a case exact rather than a race against the host's clock resolution.
    fn spread_creation_instants(&self, case: &str, prompt_ids: &[String]) {
        let state = service_state(&*self.engine).expect("the bedrock-agent store");
        let mut state = state.lock();
        let base = SystemTime::UNIX_EPOCH + Duration::from_secs(1_767_225_600);
        for (position, prompt_id) in prompt_ids.iter().enumerate() {
            let prompt = state
                .prompts
                .get_mut(prompt_id)
                .unwrap_or_else(|| panic!("case {case}: expected the stored prompt {prompt_id}"));
            let created_at = base + Duration::from_secs(u64::try_from(position).unwrap_or(0) + 1);
            prompt.created_at = created_at;
            prompt.updated_at = created_at;
        }
    }
}

/// One variant of a prompt: the modelled `PromptVariant` requires `name`,
/// `templateType` (the modelled `PromptTemplateType`) and `templateConfiguration` (the
/// modelled `PromptTemplateConfiguration` union, whose `text` member carries a
/// `TextPromptTemplateConfiguration`).
fn text_variant(name: &str, text: &str) -> Value {
    json!({
        "name": name,
        "templateType": "TEXT",
        "templateConfiguration": { "text": { "text": text } },
    })
}

/// The `CreatePrompt` request for the `index`th prompt of a case. The rows a case creates
/// differ in what the summary has to carry: a described prompt with a default variant, a
/// prompt with neither a description nor variants, and one carrying a variant but naming
/// no default.
fn prompt_request(index: usize) -> Value {
    match index % 3 {
        0 => json!({
            "name": format!("listed-prompt-{index}"),
            "description": format!("the {index}th listed prompt"),
            "defaultVariant": "summary",
            "variants": [text_variant("summary", "Summarise {{input}}")],
        }),
        1 => json!({ "name": format!("bare-listed-prompt-{index}") }),
        _ => json!({
            "name": format!("undefaulted-listed-prompt-{index}"),
            "variants": [text_variant("translate", "Translate {{input}}")],
        }),
    }
}

/// The members a `PromptSummary` carries, as the modelled shape declares them
/// (`smithy_query(query="shape",shape="PromptSummary")`): the required ones plus the
/// optional `description`.
const SUMMARY_MEMBERS: [&str; 7] = [
    "name",
    "description",
    "id",
    "arn",
    "version",
    "createdAt",
    "updatedAt",
];

/// Assert that `summary` reports the same values the `GetPrompt` read of the same version
/// reports — which is what makes a summary a projection of the stored record rather than
/// a second account of it — and that every member the modelled shape requires is there.
async fn assert_summary_matches_the_read(
    emulator: &Emulator,
    case: &str,
    summary: &Value,
    identifier: &str,
    version: Option<&str>,
) {
    let read = emulator.get_prompt(identifier, version).await;
    assert_eq!(
        read.status, 200,
        "case {case}: expected GetPrompt -> 200 for {identifier} at {version:?}, got {read:?}"
    );
    for member in SUMMARY_MEMBERS {
        assert_eq!(
            summary.get(member).unwrap_or(&Value::Null),
            read.member(&[member]),
            "case {case}: the summary's {member} is the one the read reports, \
             got {summary:?} against {read:?}"
        );
    }
    for member in ["name", "id", "arn", "version", "createdAt", "updatedAt"] {
        assert!(
            summary.get(member).and_then(Value::as_str).is_some(),
            "case {case}: expected the required member {member} in {summary:?}"
        );
    }
    // Nothing beyond the modelled shape's members is reported.
    let object = summary
        .as_object()
        .unwrap_or_else(|| panic!("case {case}: expected a summary object, got {summary:?}"));
    for member in object.keys() {
        assert!(
            SUMMARY_MEMBERS.contains(&member.as_str()),
            "case {case}: {member} is not a member of the modelled PromptSummary, \
             got {summary:?}"
        );
    }
}

/// With no `promptIdentifier`, the listing reports the working draft of every prompt the
/// account holds, in the order the prompts were created, and pages over them.
///
/// Rows differ in the COUNT of prompts held — none, fewer than one page, exactly one page
/// and more than one page — and in the `maxResults` the request names, including the
/// bounds of its modelled `range 1..1000`.
#[tokio::test]
async fn list_prompts_reports_the_working_draft_of_every_prompt_in_creation_order() {
    for count in [0_usize, 1, 3, 10, 12, 23] {
        for max_results in [None, Some(1), Some(3), Some(10), Some(1000)] {
            let case = format!("{count} prompt(s), maxResults {max_results:?}");
            let emulator = Emulator::new();

            let mut created: Vec<Value> = Vec::new();
            for index in 0..count {
                created.push(
                    emulator
                        .create_prompt(&case, prompt_request(index))
                        .await
                        .body,
                );
            }
            // The expected order is the order the prompts were CREATED in, which this
            // reverses relative to the order they were stored in: a listing that reported
            // insertion order, or the store's own iteration order, would not match.
            let ids: Vec<String> = created
                .iter()
                .rev()
                .map(|prompt| prompt["id"].as_str().expect("the minted id").to_string())
                .collect();
            emulator.spread_creation_instants(&case, &ids);

            let summaries = emulator.drain(&case, None, max_results).await;
            assert_eq!(
                summaries.len(),
                count,
                "case {case}: expected one summary per prompt, got {summaries:?}"
            );
            let listed: Vec<String> = summaries
                .iter()
                .map(|summary| summary["id"].as_str().unwrap_or_default().to_string())
                .collect();
            assert_eq!(
                listed, ids,
                "case {case}: the listing reports the prompts in creation order"
            );
            for summary in &summaries {
                let id = summary["id"].as_str().expect("the summary's id");
                assert_eq!(
                    summary["version"], json!("DRAFT"),
                    "case {case}: the listing reports each prompt's working draft, \
                     got {summary:?}"
                );
                assert_summary_matches_the_read(&emulator, &case, summary, id, None).await;
            }
        }
    }
}

/// With a `promptIdentifier`, the listing reports the versions of the prompt it names —
/// the working draft first, then the numbered snapshots in the order they were cut — and
/// no other prompt's.
///
/// Rows differ in the COUNT of versions cut (none, one, more than one page's worth), in
/// WHICH form of the modelled `PromptIdentifier` names the prompt, and in `maxResults`.
#[tokio::test]
async fn list_prompts_for_one_prompt_reports_its_draft_and_every_version_cut_from_it() {
    for cuts in [0_u32, 1, 2, 10, 12] {
        let case = format!("{cuts} version(s) cut");
        let emulator = Emulator::new();

        let draft = emulator.create_prompt(&case, prompt_request(0)).await;
        let prompt_id = draft.string(&["id"]);
        let prompt_arn = draft.string(&["arn"]);

        // A second prompt, with a cut of its own: one prompt's listing never reports
        // another's version.
        let other = emulator.create_prompt(&case, prompt_request(1)).await;
        let other_id = other.string(&["id"]);
        emulator
            .create_prompt_version(&case, &other_id, json!({}))
            .await;

        for expected in 1..=cuts {
            let cut = emulator
                .create_prompt_version(
                    &case,
                    &prompt_id,
                    json!({ "description": format!("cut number {expected}") }),
                )
                .await;
            assert_eq!(
                cut.string(&["version"]),
                expected.to_string(),
                "case {case}: versions are numbered incrementally from 1, got {cut:?}"
            );
        }

        let expected_versions: Vec<String> = ::std::iter::once("DRAFT".to_string())
            .chain((1..=cuts).map(|version| version.to_string()))
            .collect();

        for (form, identifier) in identifier_forms(&prompt_id, &prompt_arn, cuts) {
            for max_results in [None, Some(1), Some(4), Some(1000)] {
                let case = format!("{case}, {form}, maxResults {max_results:?}");
                let summaries = emulator
                    .drain(&case, Some(identifier.as_str()), max_results)
                    .await;
                let listed: Vec<String> = summaries
                    .iter()
                    .map(|summary| summary["version"].as_str().unwrap_or_default().to_string())
                    .collect();
                assert_eq!(
                    listed, expected_versions,
                    "case {case}: the listing reports the working draft and then every \
                     version cut from it, in cut order"
                );
                for summary in &summaries {
                    let version = summary["version"].as_str().expect("the summary's version");
                    assert_eq!(
                        summary["id"], json!(prompt_id),
                        "case {case}: every summary names the prompt that was listed, \
                         got {summary:?}"
                    );
                    let expected_arn = if version == "DRAFT" {
                        prompt_arn.clone()
                    } else {
                        format!("{prompt_arn}:{version}")
                    };
                    assert_eq!(
                        summary["arn"], json!(expected_arn),
                        "case {case}: a numbered version is addressed by the prompt's ARN \
                         qualified with its number, got {summary:?}"
                    );
                    if version != "DRAFT" {
                        assert_eq!(
                            summary["description"], json!(format!("cut number {version}")),
                            "case {case}: a version reports the snapshot it was cut as, \
                             got {summary:?}"
                        );
                    }
                    assert_summary_matches_the_read(
                        &emulator,
                        &case,
                        summary,
                        &prompt_id,
                        Some(version),
                    )
                    .await;
                }
            }
        }

        // The other prompt's own listing reports its own versions and nothing else, so
        // neither listing reaches into the other.
        let other_versions: Vec<String> = emulator
            .drain(&case, Some(other_id.as_str()), None)
            .await
            .iter()
            .map(|summary| summary["version"].as_str().unwrap_or_default().to_string())
            .collect();
        assert_eq!(
            other_versions,
            vec!["DRAFT".to_string(), "1".to_string()],
            "case {case}: one prompt's version listing reports only its own versions"
        );
    }
}

/// Every form of the modelled `PromptIdentifier` that names one prompt: the bare id, the
/// prompt's ARN, and — once a version has been cut — that ARN qualified with a version,
/// which names the prompt whose versions are listed all the same.
fn identifier_forms(prompt_id: &str, prompt_arn: &str, cuts: u32) -> Vec<(String, String)> {
    let mut forms = vec![
        ("a bare promptId".to_string(), prompt_id.to_string()),
        ("a prompt ARN".to_string(), prompt_arn.to_string()),
    ];
    if cuts >= 1 {
        forms.push((
            "a version-qualified ARN".to_string(),
            format!("{prompt_arn}:1"),
        ));
    }
    forms
}

/// A well-formed identifier the store does not hold names a missing resource; an
/// identifier of neither modelled form is a bad request; and a `nextToken` this service
/// never issued is a bad request in either mode of the listing.
#[tokio::test]
async fn list_prompts_refuses_an_unknown_prompt_a_malformed_identifier_and_an_unissued_token() {
    let case = "refused listings";
    let emulator = Emulator::new();
    let draft = emulator.create_prompt(case, prompt_request(0)).await;
    let prompt_id = draft.string(&["id"]);
    let prompt_arn = draft.string(&["arn"]);

    // Rows: both modelled identifier forms, over a well-formed id the store does not
    // hold. A prompt that exists always has its working draft to report, so an
    // identifier naming none is a missing resource rather than an empty page.
    let unknown = "0123456789";
    for (form, identifier) in [
        ("bare promptId", unknown.to_string()),
        ("prompt ARN", prompt_arn.replace(prompt_id.as_str(), unknown)),
        (
            "version-qualified ARN",
            format!("{}:1", prompt_arn.replace(prompt_id.as_str(), unknown)),
        ),
    ] {
        let refused = emulator.list_prompts(Some(&identifier), None, None).await;
        assert_eq!(
            refused.status, 404,
            "case {case} ({form}): expected ListPrompts -> 404, got {refused:?}"
        );
        assert_eq!(
            refused.error_type.as_deref(),
            Some("ResourceNotFoundException"),
            "case {case} ({form}): expected ResourceNotFoundException, got {refused:?}"
        );
        let message = refused.string(&["message"]);
        assert!(
            message.contains("ListPrompts") && message.contains("Prompt"),
            "case {case} ({form}): the refusal names the operation and the resource type, \
             got {message:?}"
        );
    }

    // Rows differ in HOW the identifier misses the modelled `PromptIdentifier` forms: a
    // prompt name, ids of the wrong length or alphabet, and ARNs wrong in the service,
    // the resource type and the partition.
    for (form, identifier) in [
        ("a prompt name, not an identifier", "listed-prompt-0".to_string()),
        ("nine characters", "012345678".to_string()),
        ("eleven characters", "0123456789a".to_string()),
        ("a character outside the id alphabet", "0123_56789".to_string()),
        ("an ARN naming another service", prompt_arn.replace(":bedrock:", ":s3:")),
        ("an ARN naming another resource type", prompt_arn.replace(":prompt/", ":flow/")),
        ("an ARN in another partition", prompt_arn.replace("arn:aws:", "arn:aws-cn:")),
    ] {
        let refused = emulator.list_prompts(Some(&identifier), None, None).await;
        assert_eq!(
            refused.status, 400,
            "case {case} ({form}, {identifier}): expected ListPrompts -> 400, got {refused:?}"
        );
        assert_eq!(
            refused.error_type.as_deref(),
            Some("ValidationException"),
            "case {case} ({form}, {identifier}): expected ValidationException, got {refused:?}"
        );
    }

    // Rows differ in HOW the token misses the one spelling the listing in force hands
    // out: the other mode's spelling, a position before the first, a padded number, a
    // number too wide for a version, and a value this service never writes at all.
    for (mode, identifier, tokens) in [
        (
            "the account's prompts",
            None,
            vec!["DRAFT", "1", "0", "01", "not-a-token", "12345-short", "0-abcdefghij"],
        ),
        (
            "one prompt's versions",
            Some(prompt_id.as_str()),
            vec!["0", "01", "000001", "999999", "draft", "not-a-token", "17-abcdefghij"],
        ),
    ] {
        for token in tokens {
            let refused = emulator.list_prompts(identifier, None, Some(token)).await;
            assert_eq!(
                refused.status, 400,
                "case {case} (over {mode}, token {token:?}): expected ListPrompts -> 400, \
                 got {refused:?}"
            );
            assert_eq!(
                refused.error_type.as_deref(),
                Some("ValidationException"),
                "case {case} (over {mode}, token {token:?}): expected ValidationException, \
                 got {refused:?}"
            );
        }
    }
}

/// The listing is a READ: it reports the same page however often it is called, and
/// leaves the prompts, their versions and the next version number exactly where they
/// stood.
#[tokio::test]
async fn listing_prompts_changes_nothing_a_later_caller_sees() {
    let case = "repeated listings";
    let emulator = Emulator::new();

    let mut ids: Vec<String> = Vec::new();
    for index in 0..4 {
        let created = emulator.create_prompt(case, prompt_request(index)).await;
        ids.push(created.string(&["id"]));
    }
    let first_id = ids[0].clone();
    emulator
        .create_prompt_version(case, &first_id, json!({}))
        .await;

    let before_account = emulator.list_prompts(None, None, None).await;
    let before_versions = emulator
        .list_prompts(Some(&first_id), None, None)
        .await;
    let before_read = emulator.get_prompt(&first_id, None).await;

    // Listing again, in either mode, reports exactly the same answer.
    for round in 0..3 {
        let account = emulator.list_prompts(None, None, None).await;
        assert_eq!(
            account.body, before_account.body,
            "case {case}: round {round} of the account listing reports the same page"
        );
        let versions = emulator
            .list_prompts(Some(&first_id), None, None)
            .await;
        assert_eq!(
            versions.body, before_versions.body,
            "case {case}: round {round} of the version listing reports the same page"
        );
    }

    assert_eq!(
        emulator.get_prompt(&first_id, None).await.body,
        before_read.body,
        "case {case}: listing leaves the prompt as it stood"
    );
    assert_eq!(
        before_account.summaries(case).len(),
        ids.len(),
        "case {case}: every prompt created is still reported, got {before_account:?}"
    );
    let reported: BTreeSet<String> = before_account
        .summaries(case)
        .iter()
        .map(|summary| summary["id"].as_str().unwrap_or_default().to_string())
        .collect();
    assert_eq!(
        reported,
        ids.iter().cloned().collect::<BTreeSet<String>>(),
        "case {case}: the account listing reports each prompt once"
    );

    // No version number was consumed by any of those reads: the next cut takes the
    // number after the one already cut.
    let next_cut = emulator
        .create_prompt_version(case, &first_id, json!({}))
        .await;
    assert_eq!(
        next_cut.string(&["version"]),
        "2",
        "case {case}: listing consumed no version number, got {next_cut:?}"
    );
}
