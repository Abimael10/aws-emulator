// SPDX-License-Identifier: CC0-1.0

//! The SCALARS a `PipelineSummary` carries beside its pipeline's name, read over the wire
//! this service speaks (`awsJson1_1`, `POST /` with
//! `X-Amz-Target: CodePipeline_20150709.<Operation>`), after EVERY write of a pipeline's
//! lifetime rather than after its creation alone.
//!
//! The rule under test: the `pipelineType` a listing reports for a pipeline is the type the
//! declaration of its LAST write resolved to — the type that write states, else the
//! service's default type, which is the newer of the two the model declares — and the
//! instants it reports are the stored ones serialized as epoch-seconds numbers at
//! millisecond resolution, whatever precision the clock that wrote them reads.
//!
//! Where the other summary suites vary the declaration a pipeline is CREATED with, this one
//! varies the declaration it is UPDATED with: a service that resolves the type once, at the
//! create, or that lets a pipeline keep a type a later write replaced, serves a summary its
//! own `GetPipeline` structure contradicts and fails here. The instants are the ones the
//! HOST clock wrote, so the resolution assertions run against the live encoder rather than
//! against an instant a case placed in the store.
//!
//! Every assertion goes through the router, so nothing here can pass by reading a handler's
//! internals.

use serde_json::{json, Value};

use crate::pipeline_declaration_wire_form::{epoch_millis, whole_listing};
use crate::pipeline_lifecycle::{declaration, Emulator};

/// The `PipelineType` values the model declares
/// (`smithy_query query="shape" shape="PipelineType"` -> `V1`, `V2`).
const PIPELINE_TYPES: [&str; 2] = ["V1", "V2"];

/// The `PipelineType` a declaration stating none resolves to: the newer of the two the
/// model declares, which `PipelineDeclaration$pipelineType` describes as having "the same
/// structure as a V1 type, along with additional parameters for release safety and trigger
/// configuration".
const DEFAULT_PIPELINE_TYPE: &str = PIPELINE_TYPES[1];

/// The version a pipeline one edit has changed is left at: a create stores version 1 and an
/// edit that really changes the structure advances it by one
/// (`PipelineDeclaration$version`, "incremented when a pipeline is updated").
const VERSION_AFTER_ONE_EDIT: i64 = 2;

/// One pipeline of the fleet, written twice: created with one declaration and then updated
/// with another.
struct WrittenTwice {
    /// What makes this row different from the others.
    case: &'static str,
    /// The pipeline's name, which is also the key the listing orders by.
    name: &'static str,
    /// The `pipelineType` the CREATE states, `None` for a declaration stating none.
    created_type: Option<&'static str>,
    /// The `pipelineType` the UPDATE states, `None` for a declaration stating none.
    updated_type: Option<&'static str>,
    /// The `pipelineType` every projection must report once both writes have landed.
    pipeline_type: &'static str,
}

/// The summary a listing serves reports the type the pipeline's LAST write resolved to, and
/// its stored instants at millisecond resolution, on every page.
///
/// Rows differ along what the rule generalises over: which of the model's two types each of
/// the two writes states, including the write that states none — the case where the
/// service's own default decides the type, at the update as much as at the create — and
/// including a write that replaces the newer type with the older one and a write that
/// leaves the type as it was.
#[tokio::test]
async fn every_summary_reports_the_type_its_last_write_resolved_to() {
    let fleet: [WrittenTwice; 6] = [
        WrittenTwice {
            case: "created stating no type, updated stating no type",
            name: "golf-none-then-none",
            created_type: None,
            updated_type: None,
            pipeline_type: DEFAULT_PIPELINE_TYPE,
        },
        WrittenTwice {
            case: "created stating the older type, updated stating no type",
            name: "hotel-older-then-none",
            created_type: Some(PIPELINE_TYPES[0]),
            updated_type: None,
            pipeline_type: DEFAULT_PIPELINE_TYPE,
        },
        WrittenTwice {
            case: "created stating the older type, updated stating the newer",
            name: "india-older-then-newer",
            created_type: Some(PIPELINE_TYPES[0]),
            updated_type: Some(PIPELINE_TYPES[1]),
            pipeline_type: PIPELINE_TYPES[1],
        },
        WrittenTwice {
            case: "created stating the newer type, updated stating the older",
            name: "juliett-newer-then-older",
            created_type: Some(PIPELINE_TYPES[1]),
            updated_type: Some(PIPELINE_TYPES[0]),
            pipeline_type: PIPELINE_TYPES[0],
        },
        WrittenTwice {
            case: "created stating no type, updated stating the older type",
            name: "kilo-none-then-older",
            created_type: None,
            updated_type: Some(PIPELINE_TYPES[0]),
            pipeline_type: PIPELINE_TYPES[0],
        },
        WrittenTwice {
            case: "created stating the older type, updated stating it again",
            name: "lima-older-then-older",
            created_type: Some(PIPELINE_TYPES[0]),
            updated_type: Some(PIPELINE_TYPES[0]),
            pipeline_type: PIPELINE_TYPES[0],
        },
    ];

    let emulator = Emulator::new();
    // What each pipeline's own write answered with, so the listing is read against the
    // structure the service answered the update with as well as against the stored one.
    let mut written: Vec<Value> = Vec::new();
    for member in &fleet {
        let mut declared = declaration(member.name);
        if let Some(created_type) = member.created_type {
            declared["pipelineType"] = json!(created_type);
        }
        emulator
            .ok("CreatePipeline", &json!({"pipeline": declared.clone()}))
            .await;

        // A structure that really DIFFERS from the stored one — another artifact store
        // location — so the write lands whether or not it also changes the type, and the
        // version it leaves the pipeline at is the same for every row. No stage changes, so
        // the pipeline keeps declaring each action name once.
        let mut edit = declaration(member.name);
        edit["artifactStore"]["location"] = json!("another-bucket");
        // A row whose update states no type sends the declaration without that member at
        // all, which is the request whose type the SERVICE decides.
        if let Some(updated_type) = member.updated_type {
            edit["pipelineType"] = json!(updated_type);
        }
        let updated = emulator
            .ok("UpdatePipeline", &json!({"pipeline": edit}))
            .await;
        written.push(updated["pipeline"].clone());
    }

    // The stored pipeline each summary has to agree with, read once: nothing writes these
    // pipelines again, so the pages read below see the same stored structures.
    let mut fetched: Vec<Value> = Vec::new();
    for member in &fleet {
        fetched.push(
            emulator
                .ok("GetPipeline", &json!({"name": member.name}))
                .await,
        );
    }

    // One summary per page, a size that divides the fleet unevenly, the whole fleet in one
    // page, and no `maxResults` at all: the same scalars whichever page a summary arrives
    // on.
    let fleet_size = fleet.len() as i64;
    for page_size in [Some(1), Some(4), Some(fleet_size), None] {
        let summaries = whole_listing(&emulator, page_size, fleet.len()).await;
        for ((member, written), fetched) in fleet.iter().zip(&written).zip(&fetched) {
            let case = format!("{} at page size {page_size:?}", member.case);
            let summary = summaries
                .iter()
                .find(|summary| summary["name"] == json!(member.name))
                .unwrap_or_else(|| {
                    panic!(
                        "case {case:?}: the listing must serve this pipeline's summary, got \
                         {summaries:?}"
                    )
                });

            assert_eq!(
                summary["pipelineType"],
                json!(member.pipeline_type),
                "case {case:?}: the summary must report the type the last write's \
                 declaration resolved to, got {summary}"
            );
            assert_eq!(
                summary["pipelineType"], written["pipelineType"],
                "case {case:?}: the summary must report the same type the update answered \
                 with ({written}), got {summary}"
            );
            assert_eq!(
                summary["pipelineType"], fetched["pipeline"]["pipelineType"],
                "case {case:?}: the summary must report the same type the stored structure \
                 carries ({}), got {summary}",
                fetched["pipeline"]
            );
            assert_eq!(
                summary["version"],
                json!(VERSION_AFTER_ONE_EDIT),
                "case {case:?}: the summary must report the version the two writes left the \
                 pipeline at, got {summary}"
            );
            assert_eq!(
                summary["version"], fetched["pipeline"]["version"],
                "case {case:?}: the summary must report the same version the stored \
                 structure carries ({}), got {summary}",
                fetched["pipeline"]
            );

            // The instants the HOST clock wrote, each serialized at millisecond resolution
            // (`epoch_millis` asserts that) and each the number the pipeline's own metadata
            // reports.
            let mut reported = Vec::new();
            for instant in ["created", "updated"] {
                let actual = epoch_millis(
                    &case,
                    &format!("the summary of {}.{instant}", member.name),
                    &summary[instant],
                );
                let metadata = epoch_millis(
                    &case,
                    &format!("the metadata of {}.{instant}", member.name),
                    &fetched["metadata"][instant],
                );
                assert_eq!(
                    actual, metadata,
                    "case {case:?}: the summary must report {instant} as the pipeline's own \
                     metadata reports it ({}), got {summary}",
                    fetched["metadata"]
                );
                reported.push(actual);
            }
            let (created, updated) = (reported[0], reported[1]);
            assert!(
                updated >= created,
                "case {case:?}: the instant an edit left the pipeline updated at is never \
                 before the one it was created at, got {summary}"
            );
        }
    }
}
