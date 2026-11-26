// SPDX-License-Identifier: CC0-1.0

//! The CHARACTERS this service writes the WEBHOOK plane's instant as, read off the
//! response body itself over the wire it speaks (`awsJson1_1`, `POST /` with
//! `X-Amz-Target: CodePipeline_20150709.<Operation>`).
//!
//! The rule under test is the one every instant this service reports is held to: an
//! instant a read reports is written as an epoch-seconds JSON number carrying at most
//! three fractional digits — millisecond resolution, the resolution AWS reports
//! CodePipeline's instants at — naming the millisecond the stored instant fell in,
//! whatever precision the clock that wrote it or the store that holds it keeps.
//!
//! The member here is `ListWebhookItem$lastTriggered`, "the date and time a webhook was
//! last successfully triggered, in timestamp format"
//! (`smithy_query query="shape" shape="ListWebhookItem"`) — the one instant the webhook
//! plane reports, and the one no other suite of this target holds to that rule. It is read
//! through BOTH calls that project a stored webhook, `ListWebhooks` and the `PutWebhook`
//! that redefines one, so one instant is one number wherever it is reported.
//!
//! Nothing here parses the number out of the body before checking it: the literal
//! characters the serializer wrote are read, their fractional digits counted, and the
//! millisecond reconstructed from the digits themselves (the readers come from
//! `tests/all/timestamp_wire_literals.rs`, with the raw-bytes harness they belong to).
//!
//! Every assertion goes through the router, so nothing here can pass by reading a
//! handler's internals.

use std::collections::BTreeMap;
use std::time::{Duration, SystemTime};

use serde_json::{json, Value};

use service_com_amazonaws_codepipeline_codepipeline_20150709::ComAmazonawsCodepipelineCodepipeline20150709State as ServiceState;

// The millisecond a stored instant fell in, as every projection of it must report it.
use crate::pipeline_declaration_wire_form::millis_of;
// The raw-bytes harness and the two literal readers this target shares.
use crate::timestamp_wire_literals::{
    epoch_millis_of_literal, literal_members, WireEmulator,
};

/// The pipeline every webhook of this suite targets. `WebhookDefinition$targetPipeline`
/// is a NAME the definition carries rather than a reference `PutWebhook` resolves, so no
/// pipeline has to be stored for these definitions to be accepted and reported.
const TARGET_PIPELINE: &str = "demo";

/// A ceiling on how many pages one listing of these webhooks may serve, so a listing that
/// never stops handing out a cursor fails instead of looping forever. The fleets below are
/// a handful of webhooks and the smallest page size is one.
const MAX_PAGES: usize = 64;

/// The smallest definition that is a webhook: the authentication type that takes no
/// configuration property at all, and one filter over the payload
/// (`smithy_query query="shape" shape="WebhookDefinition"` — `name`, `targetPipeline`,
/// `targetAction`, `filters`, `authentication` and `authenticationConfiguration` are all
/// required).
fn definition(name: &str) -> Value {
    json!({
        "name": name,
        "targetPipeline": TARGET_PIPELINE,
        "targetAction": "Src",
        "filters": [{"jsonPath": "$.ref", "matchEquals": "refs/heads/main"}],
        "authentication": "UNAUTHENTICATED",
        "authenticationConfiguration": {},
    })
}

/// Define `name`, and answer the webhook item `PutWebhook` reported for it, as bytes.
async fn put_webhook(emulator: &WireEmulator, name: &str) -> String {
    emulator
        .raw("PutWebhook", &json!({"webhook": definition(name)}))
        .await
}

/// Record `instant` as the webhook's last successful trigger, in the one store every
/// handler reads — the instant a delivered trigger request writes there
/// (`crate::service::webhook_trigger`), which no API call can be made to put a chosen
/// sub-second tail on.
fn set_last_triggered(emulator: &WireEmulator, name: &str, instant: SystemTime, case: &str) {
    let state = emulator.engine().state::<ServiceState>();
    let mut state = state.lock();
    let webhook = state
        .webhooks
        .get_mut(name)
        .unwrap_or_else(|| panic!("case {case:?}: the defined webhook must be in the store"));
    webhook.last_triggered = Some(instant);
}

/// The raw body of every page of one `ListWebhooks`, read at `page_size`.
///
/// The paging members are the model's own, which this listing spells with a leading
/// capital (`smithy_query query="operation" operation="ListWebhooks"` — `MaxResults`,
/// `NextToken`).
async fn listed_pages(emulator: &WireEmulator, page_size: Option<i64>) -> Vec<String> {
    let mut pages = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let mut request = json!({});
        if let Some(size) = page_size {
            request["MaxResults"] = json!(size);
        }
        if let Some(cursor) = &cursor {
            request["NextToken"] = json!(cursor);
        }
        let body = emulator.raw("ListWebhooks", &request).await;
        let parsed: Value = serde_json::from_str(&body)
            .unwrap_or_else(|e| panic!("ListWebhooks: the answer must be JSON ({e}): {body}"));
        cursor = parsed["NextToken"].as_str().map(str::to_string);
        pages.push(body);
        if cursor.is_none() {
            return pages;
        }
        assert!(
            pages.len() <= MAX_PAGES,
            "page size {page_size:?}: the listing kept handing out a cursor after \
             {MAX_PAGES} pages"
        );
    }
}

/// The name of every webhook one page carries, in the order the page carries them.
fn page_names(page: &str) -> Vec<String> {
    let parsed: Value = serde_json::from_str(page)
        .unwrap_or_else(|e| panic!("ListWebhooks: the answer must be JSON ({e}): {page}"));
    parsed["webhooks"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|item| {
            item["definition"]["name"]
                .as_str()
                .unwrap_or_else(|| panic!("every reported webhook names itself, got {item}"))
                .to_string()
        })
        .collect()
}

/// One webhook of the fleet, with the sub-second tail its stored trigger instant carries.
struct TriggeredTail {
    /// What makes this row different from the others.
    case: &'static str,
    /// The webhook's name, which is also the key the listing orders by.
    name: &'static str,
    /// The whole seconds the stored instant sits at, one per row so no two instants of
    /// the fleet share a value and a projection reporting another webhook's instant
    /// fails.
    secs: u64,
    /// The sub-second NANOseconds the stored instant carries.
    nanos: u32,
    /// The sub-second MILLIseconds the written number must carry.
    millis: i64,
}

/// RULE: every `lastTriggered` the webhook plane writes is written with at most
/// millisecond digits, and names the millisecond the stored instant fell in — on every
/// page of the listing, and in the answer of the call that redefines the webhook.
///
/// Rows differ along what the rule generalises over, the sub-second part of the stored
/// instant: none at all, the nanosecond above a whole second, the last nanosecond below a
/// millisecond, exactly one millisecond, a tail whose first dropped digit would ROUND the
/// millisecond up, a tail ending in a zero millisecond digit (so the serializer writes
/// fewer than three digits), and the last nanosecond below a whole second, which rounding
/// would carry into the next second.
#[tokio::test]
async fn every_reported_trigger_instant_is_written_with_at_most_millisecond_digits() {
    let fleet: [TriggeredTail; 7] = [
        TriggeredTail {
            case: "a whole second",
            name: "alfa-whole-second",
            secs: 1_700_000_001,
            nanos: 0,
            millis: 0,
        },
        TriggeredTail {
            case: "one nanosecond past the second",
            name: "bravo-one-nanosecond",
            secs: 1_700_000_002,
            nanos: 1,
            millis: 0,
        },
        TriggeredTail {
            case: "the last nanosecond below a millisecond",
            name: "charlie-below-a-millisecond",
            secs: 1_700_000_003,
            nanos: 999_999,
            millis: 0,
        },
        TriggeredTail {
            case: "exactly one millisecond",
            name: "delta-one-millisecond",
            secs: 1_700_000_004,
            nanos: 1_000_000,
            millis: 1,
        },
        TriggeredTail {
            case: "a tail whose dropped digits would round the millisecond up",
            name: "echo-rounding-tail",
            secs: 1_700_000_005,
            nanos: 272_999_999,
            millis: 272,
        },
        TriggeredTail {
            case: "a tail ending in a zero millisecond digit",
            name: "foxtrot-trailing-zero",
            secs: 1_700_000_006,
            nanos: 650_000_000,
            millis: 650,
        },
        TriggeredTail {
            case: "the last nanosecond below a whole second",
            name: "golf-below-a-second",
            secs: 1_700_000_007,
            nanos: 999_999_999,
            millis: 999,
        },
    ];

    let emulator = WireEmulator::new();
    for member in &fleet {
        put_webhook(&emulator, member.name).await;
        set_last_triggered(
            &emulator,
            member.name,
            SystemTime::UNIX_EPOCH + Duration::new(member.secs, member.nanos),
            member.case,
        );
    }

    // The number each answer must carry for each webhook, which is exact rather than a
    // shape because the store holds an instant this suite chose.
    let expected: BTreeMap<&str, i64> = fleet
        .iter()
        .map(|member| (member.name, member.secs as i64 * 1_000 + member.millis))
        .collect();

    // One item per page, a size that divides the fleet unevenly, the whole fleet in one
    // page, and no `MaxResults` at all: the same characters whichever page an item is
    // served on.
    let fleet_size = fleet.len() as i64;
    for page_size in [Some(1), Some(3), Some(fleet_size), None] {
        let pages = listed_pages(&emulator, page_size).await;
        let mut seen = 0;
        for page in &pages {
            let names = page_names(page);
            let literals = literal_members(page, "lastTriggered");
            assert_eq!(
                literals.len(),
                names.len(),
                "page size {page_size:?}: the page must write lastTriggered once per \
                 webhook it carries ({} of them), got {literals:?} in {page}",
                names.len()
            );
            for (name, literal) in names.iter().zip(&literals) {
                let case = format!("{name} at page size {page_size:?}");
                let path = format!("the listed {name}.lastTriggered");
                let actual = epoch_millis_of_literal(&case, &path, literal);
                let expected = *expected.get(name.as_str()).unwrap_or_else(|| {
                    panic!("case {case:?}: the listing served an unknown webhook")
                });
                assert_eq!(
                    actual, expected,
                    "case {case:?}: {path} must name the millisecond the stored instant \
                     fell in, got {literal:?}"
                );
            }
            seen += names.len();
        }
        assert_eq!(
            seen,
            fleet.len(),
            "page size {page_size:?}: the listing must serve every webhook of the fleet \
             once, got {seen} item(s) over {} page(s)",
            pages.len()
        );
    }

    // The other call that projects a stored webhook: a redefinition keeps the instant a
    // trigger recorded, so it reports the same number the listing wrote.
    for member in &fleet {
        let body = put_webhook(&emulator, member.name).await;
        let literals = literal_members(&body, "lastTriggered");
        assert_eq!(
            literals.len(),
            1,
            "case {:?}: the redefinition must report lastTriggered once, got {body}",
            member.case
        );
        let actual = epoch_millis_of_literal(
            member.case,
            "the redefined webhook's lastTriggered",
            &literals[0],
        );
        assert_eq!(
            actual, expected[member.name],
            "case {:?}: the redefinition must report the millisecond the stored instant \
             fell in, got {body}",
            member.case
        );
    }
}

/// RULE, against the live encoder: an instant the HOST clock read is written with at most
/// millisecond digits too, and is the millisecond that reading fell in.
///
/// The clock reads at the precision the platform offers — nanoseconds on a typical host —
/// so an encoder that wrote the reading straight through is what this test catches, which
/// is the whole of what the recorded divergence showed. Both calls that project the stored
/// webhook are read, and they must agree: one instant is one number.
#[tokio::test]
async fn a_trigger_instant_the_clock_read_is_written_with_at_most_millisecond_digits() {
    const NAME: &str = "hotel-live-clock";
    let case = "the instant the host clock read";

    let emulator = WireEmulator::new();
    put_webhook(&emulator, NAME).await;

    // The reading itself, kept so the expected number is this instant's millisecond
    // rather than a shape.
    let read = SystemTime::now();
    set_last_triggered(&emulator, NAME, read, case);
    let expected = millis_of(read);

    let listed = listed_pages(&emulator, None).await;
    let redefined = put_webhook(&emulator, NAME).await;
    let mut asserted = 0;
    for (path, body) in [
        ("the listed lastTriggered", listed.concat()),
        ("the redefined webhook's lastTriggered", redefined),
    ] {
        let literals = literal_members(&body, "lastTriggered");
        assert_eq!(
            literals.len(),
            1,
            "case {case:?}: {path} must be written once, got {literals:?} in {body}"
        );
        let actual = epoch_millis_of_literal(case, path, &literals[0]);
        assert_eq!(
            actual, expected,
            "case {case:?}: {path} must name the millisecond the clock's reading fell in, \
             got {:?}",
            literals[0]
        );
        asserted += 1;
    }
    // Without this the loop above would satisfy the rule by reading nothing.
    assert_eq!(
        asserted, 2,
        "both projections of the stored webhook must report its instant, read {asserted}"
    );
}
