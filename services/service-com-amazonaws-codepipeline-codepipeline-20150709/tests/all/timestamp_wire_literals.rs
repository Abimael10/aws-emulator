// SPDX-License-Identifier: CC0-1.0

//! The CHARACTERS this service writes an instant as, read off the response body itself
//! over the wire it speaks (`awsJson1_1`, `POST /` with
//! `X-Amz-Target: CodePipeline_20150709.<Operation>`).
//!
//! The rule under test: an instant a read reports is written as an epoch-seconds JSON
//! number carrying at most [`SUBSECOND_DIGITS`] fractional digits — millisecond
//! resolution, the resolution AWS reports CodePipeline's instants at — whatever precision
//! the clock that wrote it, or the store that holds it, keeps.
//!
//! Where the other suites read an instant by parsing it into a double and comparing the
//! result against a whole millisecond, this one never parses it: it reads the literal
//! characters the serializer emitted, counts the fractional digits and reconstructs the
//! millisecond from the digits themselves. A body whose number carries a digit finer than
//! a millisecond — the native precision of a clock read, written straight through — fails
//! here on the digit count, which is the form the number arrives in rather than how close
//! it lands to a millisecond.
//!
//! Both the instants a case PLACES in the store and the ones the HOST clock wrote are
//! read, so the live encoder is covered as well as a chosen tail, and the listing is read
//! on every page size: a projection that encodes an instant itself on one page, or for one
//! pipeline, rather than through the one encoder every read shares, fails here.
//!
//! Every assertion goes through the router, so nothing here can pass by reading a
//! handler's internals.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_codepipeline_codepipeline_20150709::ComAmazonawsCodepipelineCodepipeline20150709State as ServiceState;

use crate::pipeline_lifecycle::declaration;

/// The `X-Amz-Target` prefix OPERATIONS.md records for this service.
const TARGET_PREFIX: &str = "CodePipeline_20150709";

/// How many SUBSECOND digits an instant this service writes may carry.
///
/// `awsJson1_1` carries a `Timestamp` as an epoch-seconds JSON number, and AWS reports
/// CodePipeline's instants at millisecond resolution — three fractional digits — so a
/// number carrying a finer digit is one the service never writes.
const SUBSECOND_DIGITS: usize = 3;

/// The members of this model's pipeline plane that carry an instant
/// (`smithy_query query="shape" shape="PipelineSummary,PipelineMetadata"` -> `created`,
/// `updated`).
const PIPELINE_INSTANTS: [&str; 2] = ["created", "updated"];

/// A ceiling on how many pages one listing of these fleets may serve, so a listing that
/// never stops handing out a cursor fails instead of looping forever. The fleets below are
/// a handful of pipelines and the smallest page size is one.
const MAX_PAGES: usize = 64;

/// One emulator whose answers are read as BYTES.
///
/// The shared harness (`pipeline_lifecycle::Emulator`) parses every answer into a
/// `serde_json::Value`, which is exactly what this suite must not do: parsing re-renders
/// the number and loses the characters the serializer wrote. So the router is driven
/// directly here, the same way `core/router` assembles the production emulator.
///
/// `pub(crate)` because the suites that read an instant as BYTES drive the service through
/// this one harness rather than each growing a copy of it.
pub(crate) struct WireEmulator {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
}

impl WireEmulator {
    pub(crate) fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(
            service_com_amazonaws_codepipeline_codepipeline_20150709::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        Self { router, engine }
    }

    /// The one shared engine this emulator's service reads and writes, for a case that
    /// places an instant in the store the handlers read. Every assertion still goes
    /// through the router.
    pub(crate) fn engine(&self) -> &Arc<engine::InMemoryEngine> {
        &self.engine
    }

    /// Call `operation` and hand back the response body EXACTLY as the bytes arrived.
    pub(crate) async fn raw(&self, operation: &str, body: &Value) -> String {
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("content-type", "application/x-amz-json-1.1")
            .header("x-amz-target", format!("{TARGET_PREFIX}.{operation}"))
            .header(
                "authorization",
                "AWS4-HMAC-SHA256 \
                 Credential=AKIDEXAMPLE/20260101/us-east-1/codepipeline/aws4_request, \
                 SignedHeaders=host;x-amz-date, Signature=0123456789abcdef",
            )
            .body(bytes::Bytes::from(body.to_string()))
            .expect("a valid request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let received = response.into_body();
        let received = String::from_utf8(received.to_vec())
            .unwrap_or_else(|e| panic!("{operation}: response body is not UTF-8 ({e})"));
        assert_eq!(
            status, 200,
            "{operation}: expected HTTP 200, got {status} with {received}"
        );
        received
    }
}

/// Every value `body` writes for the member `name`, as the literal characters the
/// serializer wrote — the one reader in this suite, so no case can slip into parsing.
///
/// The scan is deliberately textual: it takes what follows `"<name>":` up to the next
/// value delimiter. The callers below check the COUNT of what it found against the number
/// of structures the answer carries, so a member this scan missed, or one it found
/// somewhere unexpected, fails rather than passing vacuously.
pub(crate) fn literal_members(body: &str, name: &str) -> Vec<String> {
    let key = format!("\"{name}\":");
    let mut found = Vec::new();
    let mut rest = body;
    while let Some(at) = rest.find(&key) {
        let after = &rest[at + key.len()..];
        let end = after.find([',', '}', ']']).unwrap_or(after.len());
        found.push(after[..end].trim().to_string());
        rest = &after[end..];
    }
    found
}

/// The epoch-MILLISECONDS a literal names, read from its DIGITS rather than through a
/// float, asserting on the way that it is one of the forms AWS writes: an epoch-seconds
/// number with at most [`SUBSECOND_DIGITS`] fractional digits and no exponent.
pub(crate) fn epoch_millis_of_literal(case: &str, path: &str, literal: &str) -> i64 {
    let (seconds, fraction) = literal.split_once('.').unwrap_or((literal, ""));
    assert!(
        !seconds.is_empty() && seconds.bytes().all(|byte| byte.is_ascii_digit()),
        "case {case:?}: {path} must be written as an epoch-seconds number, got {literal:?}"
    );
    assert!(
        fraction.bytes().all(|byte| byte.is_ascii_digit()),
        "case {case:?}: {path} must be written as a plain decimal with no exponent, got \
         {literal:?}"
    );
    assert!(
        fraction.len() <= SUBSECOND_DIGITS,
        "case {case:?}: {path} must carry at most {SUBSECOND_DIGITS} fractional digits, got \
         {literal:?} with {} of them",
        fraction.len()
    );
    let whole: i64 = seconds.parse().unwrap_or_else(|e| {
        panic!("case {case:?}: {path} must be an epoch second ({e}): {literal:?}")
    });
    // The fractional digits are milliseconds once padded out to three, whichever of the
    // three the serializer had to write (a trailing zero is not written).
    let millis: i64 = match fraction.is_empty() {
        true => 0,
        false => format!("{fraction:0<width$}", width = SUBSECOND_DIGITS)
            .parse()
            .unwrap_or_else(|e| {
                panic!("case {case:?}: {path} must carry millisecond digits ({e}): {literal:?}")
            }),
    };
    whole * 1_000 + millis
}

/// The raw body of every page of one `ListPipelines`, read at `page_size`.
async fn listed_pages(emulator: &WireEmulator, page_size: Option<i64>) -> Vec<String> {
    let mut pages = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let mut request = json!({});
        if let Some(size) = page_size {
            request["maxResults"] = json!(size);
        }
        if let Some(cursor) = &cursor {
            request["nextToken"] = json!(cursor);
        }
        let body = emulator.raw("ListPipelines", &request).await;
        let parsed: Value = serde_json::from_str(&body)
            .unwrap_or_else(|e| panic!("ListPipelines: the answer must be JSON ({e}): {body}"));
        cursor = parsed["nextToken"].as_str().map(str::to_string);
        pages.push(body);
        if cursor.is_none() {
            break;
        }
        assert!(
            pages.len() <= MAX_PAGES,
            "page size {page_size:?}: the listing kept handing out a cursor after \
             {MAX_PAGES} pages"
        );
    }
    pages
}

/// The name of every summary one page carries, in the order the page carries them.
fn page_names(page: &str) -> Vec<String> {
    let parsed: Value = serde_json::from_str(page)
        .unwrap_or_else(|e| panic!("ListPipelines: the answer must be JSON ({e}): {page}"));
    parsed["pipelines"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|summary| {
            summary["name"]
                .as_str()
                .unwrap_or_else(|| panic!("every summary names its pipeline, got {summary}"))
                .to_string()
        })
        .collect()
}

/// One pipeline of the fleet, with the sub-second tail its stored instants carry.
struct StoredTail {
    /// What makes this row different from the others.
    case: &'static str,
    /// The pipeline's name, which is also the key the listing orders by.
    name: &'static str,
    /// The whole seconds both stored instants sit at (`created`; `updated` is a hundred
    /// seconds later, so a projection reporting one for the other fails).
    created_secs: u64,
    /// The sub-second NANOseconds both stored instants carry.
    nanos: u32,
    /// The sub-second MILLIseconds both written numbers must carry.
    millis: i64,
}

/// How far apart a row's two stored instants sit, so no two instants of this fleet share a
/// value.
const UPDATED_OFFSET_SECS: u64 = 100;

/// Every instant the listing writes for a stored tail is written with at most millisecond
/// digits, and names the millisecond the stored instant fell in, on every page.
///
/// Rows differ along what the rule generalises over, the sub-second part of the stored
/// instant: none at all, the nanosecond above a whole second, the last nanosecond below a
/// millisecond, exactly one millisecond, a tail whose first dropped digit would ROUND the
/// millisecond up, a tail that ends in a zero millisecond digit (so the serializer writes
/// fewer than three digits), and the last nanosecond below a whole second, which rounding
/// would carry into the next second.
#[tokio::test]
async fn every_listed_instant_is_written_with_at_most_millisecond_digits() {
    let fleet: [StoredTail; 7] = [
        StoredTail {
            case: "a whole second",
            name: "alfa-whole-second",
            created_secs: 1_700_000_001,
            nanos: 0,
            millis: 0,
        },
        StoredTail {
            case: "one nanosecond past the second",
            name: "bravo-one-nanosecond",
            created_secs: 1_700_000_002,
            nanos: 1,
            millis: 0,
        },
        StoredTail {
            case: "the last nanosecond below a millisecond",
            name: "charlie-below-a-millisecond",
            created_secs: 1_700_000_003,
            nanos: 999_999,
            millis: 0,
        },
        StoredTail {
            case: "exactly one millisecond",
            name: "delta-one-millisecond",
            created_secs: 1_700_000_004,
            nanos: 1_000_000,
            millis: 1,
        },
        StoredTail {
            case: "a tail whose dropped digits would round the millisecond up",
            name: "echo-rounding-tail",
            created_secs: 1_700_000_005,
            nanos: 272_999_999,
            millis: 272,
        },
        StoredTail {
            case: "a tail ending in a zero millisecond digit",
            name: "foxtrot-trailing-zero",
            created_secs: 1_700_000_006,
            nanos: 650_000_000,
            millis: 650,
        },
        StoredTail {
            case: "the last nanosecond below a whole second",
            name: "golf-below-a-second",
            created_secs: 1_700_000_007,
            nanos: 999_999_999,
            millis: 999,
        },
    ];

    let emulator = WireEmulator::new();
    for member in &fleet {
        emulator
            .raw(
                "CreatePipeline",
                &json!({"pipeline": declaration(member.name)}),
            )
            .await;
    }

    // The instants the store holds, written into the one store every handler reads, so the
    // number each answer must carry is exact rather than a shape. Nothing writes these
    // pipelines again, so the reads below see these instants.
    {
        let state = emulator.engine().state::<ServiceState>();
        let mut state = state.lock();
        for member in &fleet {
            let pipeline = state.pipelines.get_mut(member.name).unwrap_or_else(|| {
                panic!(
                    "case {:?}: the created pipeline must be in the store",
                    member.case
                )
            });
            pipeline.created =
                SystemTime::UNIX_EPOCH + Duration::new(member.created_secs, member.nanos);
            pipeline.updated = SystemTime::UNIX_EPOCH
                + Duration::new(member.created_secs + UPDATED_OFFSET_SECS, member.nanos);
        }
    }

    // The millisecond each member's two instants must be written as, by pipeline name.
    let expected: BTreeMap<&str, [i64; 2]> = fleet
        .iter()
        .map(|member| {
            let created = member.created_secs as i64 * 1_000 + member.millis;
            let updated = created + UPDATED_OFFSET_SECS as i64 * 1_000;
            (member.name, [created, updated])
        })
        .collect();

    // One summary per page, a size that divides the fleet unevenly, the whole fleet in one
    // page, and no `maxResults` at all: the same characters whichever page a summary is
    // served on.
    let fleet_size = fleet.len() as i64;
    for page_size in [Some(1), Some(3), Some(fleet_size), None] {
        let pages = listed_pages(&emulator, page_size).await;
        let mut seen = 0;
        for page in &pages {
            let names = page_names(page);
            for (index, member) in PIPELINE_INSTANTS.iter().enumerate() {
                let literals = literal_members(page, member);
                assert_eq!(
                    literals.len(),
                    names.len(),
                    "page size {page_size:?}: the page must write {member} once per summary \
                     it carries ({} of them), got {literals:?} in {page}",
                    names.len()
                );
                for (name, literal) in names.iter().zip(&literals) {
                    let case = format!("{name} at page size {page_size:?}");
                    let path = format!("the summary of {name}.{member}");
                    let actual = epoch_millis_of_literal(&case, &path, literal);
                    let expected = expected
                        .get(name.as_str())
                        .unwrap_or_else(|| {
                            panic!("case {case:?}: the listing served an unknown pipeline")
                        })[index];
                    assert_eq!(
                        actual, expected,
                        "case {case:?}: {path} must name the millisecond the stored instant \
                         fell in, got {literal:?}"
                    );
                }
            }
            seen += names.len();
        }
        assert_eq!(
            seen,
            fleet.len(),
            "page size {page_size:?}: the listing must serve every pipeline of the fleet \
             once, got {seen} summaries over {} page(s)",
            pages.len()
        );
    }
}

/// The instants the HOST clock wrote are written with at most millisecond digits too, on
/// every read that reports one, so the rule holds against the live encoder and not only
/// against a tail a case placed in the store.
///
/// The clock reads at the precision the platform offers — nanoseconds on a typical host —
/// so an encoder that wrote the read straight through is what this test catches, which is
/// the whole of what the recorded divergence showed. Several pipelines are created, one
/// after another, so the tails differ between them without anything fixing what they are.
#[tokio::test]
async fn an_instant_the_clock_wrote_is_written_with_at_most_millisecond_digits() {
    const NAMES: [&str; 3] = ["hotel-live-clock", "india-live-clock", "juliett-live-clock"];

    let emulator = WireEmulator::new();
    for name in NAMES {
        emulator
            .raw("CreatePipeline", &json!({"pipeline": declaration(name)}))
            .await;
    }

    // Every read of the pipeline plane that reports one of these instants, so the rule is
    // asserted per ANSWER rather than on the listing alone.
    let mut reads: Vec<(String, String)> = Vec::new();
    for page in listed_pages(&emulator, Some(2)).await {
        reads.push(("the ListPipelines summaries".to_string(), page));
    }
    for name in NAMES {
        reads.push((
            format!("the GetPipeline metadata of {name}"),
            emulator.raw("GetPipeline", &json!({"name": name})).await,
        ));
        reads.push((
            format!("the GetPipelineState answer for {name}"),
            emulator
                .raw("GetPipelineState", &json!({"name": name}))
                .await,
        ));
    }

    let mut asserted = 0;
    for (case, body) in &reads {
        for member in PIPELINE_INSTANTS {
            let literals = literal_members(body, member);
            assert!(
                !literals.is_empty(),
                "case {case:?}: the answer must report {member}, got {body}"
            );
            for literal in &literals {
                let millis = epoch_millis_of_literal(case, member, literal);
                assert!(
                    millis > 0,
                    "case {case:?}: {member} must be the instant the clock read, got \
                     {literal:?}"
                );
                asserted += 1;
            }
        }
    }
    // Without this the loop above would satisfy the rule by reading nothing: each of the
    // pipelines is reported by the listing and by both of its own reads, for both members.
    assert_eq!(
        asserted,
        NAMES.len() * 3 * PIPELINE_INSTANTS.len(),
        "the reads must together report every pipeline's instants, read {asserted}"
    );
}
