// SPDX-License-Identifier: CC0-1.0

//! The `PipelineSummary` a listing serves for EACH pipeline of a mixed fleet, read over
//! the wire this service speaks (`awsJson1_1`, `POST /` with
//! `X-Amz-Target: CodePipeline_20150709.<Operation>`).
//!
//! The rule under test: a summary reports the values the SERVICE resolved and stored for
//! that one pipeline — the `pipelineType` its declaration resolved to (the type it
//! declared, else the service's default type, which is the newer of the two the model
//! declares) and the instants the store holds for it, each serialized as an epoch-seconds
//! number truncated to the millisecond it fell in.
//!
//! Where the other summary suites assert that rule pipeline by pipeline, this one asserts
//! it across a fleet whose pipelines differ BOTH in declared type and in the sub-second
//! tail of their stored instants, inside ONE response and on every page of it: a
//! projection that decides a type while answering the listing, that rounds an instant
//! instead of truncating it, or that reports one pipeline's instant for another's summary
//! fails here even though each pipeline on its own would look right.
//!
//! Every assertion goes through the router, so nothing here can pass by reading a
//! handler's internals.

use std::time::{Duration, SystemTime};

use serde_json::{json, Value};

use service_com_amazonaws_codepipeline_codepipeline_20150709::ComAmazonawsCodepipelineCodepipeline20150709State as ServiceState;

use crate::pipeline_declaration_wire_form::{epoch_millis, whole_listing};
use crate::pipeline_lifecycle::{declaration, Emulator};

/// The `PipelineType` values the model declares
/// (`smithy_query query="shape" shape="PipelineType"` -> `V1`, `V2`).
const PIPELINE_TYPES: [&str; 2] = ["V1", "V2"];

/// The `PipelineType` a declaration stating none is stored as: the newer of the two the
/// model declares, which `PipelineDeclaration$pipelineType` describes as having "the same
/// structure as a V1 type, along with additional parameters for release safety and trigger
/// configuration".
const DEFAULT_PIPELINE_TYPE: &str = PIPELINE_TYPES[1];

/// One pipeline of the fleet the listing summarises.
struct FleetMember {
    /// What makes this row different from the others.
    case: &'static str,
    /// The pipeline's name, which is also the key the listing orders by.
    name: &'static str,
    /// The `pipelineType` the declaration states, `None` for one stating none.
    declared_type: Option<&'static str>,
    /// The `pipelineType` the summary must report.
    pipeline_type: &'static str,
    /// The whole seconds the store holds this pipeline's `created` at.
    created_secs: u64,
    /// The whole seconds the store holds this pipeline's `updated` at.
    updated_secs: u64,
    /// The sub-second NANOseconds both stored instants carry.
    nanos: u32,
    /// The sub-second MILLIseconds both reported instants must carry.
    sub_second_millis: i64,
}

/// Every summary of a mixed fleet reports its own pipeline's stored type and stored
/// instants, on every page of the listing.
///
/// Rows differ along what the rule generalises over: whether the declaration states a type
/// (none, the older, the newer) and the sub-second tail of the stored instants (none at
/// all, the nanosecond above a whole second, the last nanosecond below a millisecond,
/// exactly one millisecond, a millisecond trailed by finer digits that would ROUND up, and
/// the last nanosecond below a whole second, which rounding would carry into the next
/// second). Each pipeline's two instants sit at different whole seconds and no two
/// pipelines share one, so a summary carrying another member's instant — or the other
/// member's own — fails.
#[tokio::test]
async fn every_summary_of_a_mixed_fleet_reports_its_own_stored_type_and_instants() {
    let fleet: [FleetMember; 6] = [
        FleetMember {
            case: "a declaration stating no type, stored at a whole second",
            name: "alfa-whole-second",
            declared_type: None,
            pipeline_type: DEFAULT_PIPELINE_TYPE,
            created_secs: 1_700_000_001,
            updated_secs: 1_700_000_101,
            nanos: 0,
            sub_second_millis: 0,
        },
        FleetMember {
            case: "a declaration stating the older type, one nanosecond past the second",
            name: "bravo-one-nanosecond",
            declared_type: Some(PIPELINE_TYPES[0]),
            pipeline_type: PIPELINE_TYPES[0],
            created_secs: 1_700_000_002,
            updated_secs: 1_700_000_102,
            nanos: 1,
            sub_second_millis: 0,
        },
        FleetMember {
            case: "a declaration stating the newer type, the last nanosecond below a millisecond",
            name: "charlie-below-a-millisecond",
            declared_type: Some(PIPELINE_TYPES[1]),
            pipeline_type: PIPELINE_TYPES[1],
            created_secs: 1_700_000_003,
            updated_secs: 1_700_000_103,
            nanos: 999_999,
            sub_second_millis: 0,
        },
        FleetMember {
            case: "a declaration stating no type, exactly one millisecond",
            name: "delta-one-millisecond",
            declared_type: None,
            pipeline_type: DEFAULT_PIPELINE_TYPE,
            created_secs: 1_700_000_004,
            updated_secs: 1_700_000_104,
            nanos: 1_000_000,
            sub_second_millis: 1,
        },
        FleetMember {
            case: "a declaration stating the older type, a millisecond trailed by finer digits",
            name: "echo-trailing-digits",
            declared_type: Some(PIPELINE_TYPES[0]),
            pipeline_type: PIPELINE_TYPES[0],
            created_secs: 1_700_000_005,
            updated_secs: 1_700_000_105,
            nanos: 272_499_999,
            sub_second_millis: 272,
        },
        FleetMember {
            case: "a declaration stating no type, the last nanosecond below a second",
            name: "foxtrot-below-a-second",
            declared_type: None,
            pipeline_type: DEFAULT_PIPELINE_TYPE,
            created_secs: 1_700_000_006,
            updated_secs: 1_700_000_106,
            nanos: 999_999_999,
            sub_second_millis: 999,
        },
    ];

    let emulator = Emulator::new();
    for member in &fleet {
        let mut declared = declaration(member.name);
        if let Some(declared_type) = member.declared_type {
            declared["pipelineType"] = json!(declared_type);
        }
        emulator
            .ok("CreatePipeline", &json!({"pipeline": declared}))
            .await;
    }

    // The instants the store holds, written into the one store every handler reads, so the
    // number each summary must carry is exact rather than a shape. Nothing but a write to
    // the pipeline moves them, so the reads below see these instants.
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
            pipeline.updated =
                SystemTime::UNIX_EPOCH + Duration::new(member.updated_secs, member.nanos);
        }
    }

    // What each pipeline's OWN projections report, read once: the stored structure cannot
    // change between the pages read below, so the comparison does not need re-reading it.
    let mut fetched: Vec<Value> = Vec::new();
    for member in &fleet {
        fetched.push(
            emulator
                .ok("GetPipeline", &json!({"name": member.name}))
                .await,
        );
    }

    let mut ordered_names: Vec<&str> = fleet.iter().map(|member| member.name).collect();
    ordered_names.sort_unstable();

    // One summary per page, a size that divides the fleet unevenly, the whole fleet in one
    // page, and no `maxResults` at all: the same values whichever page a summary is served
    // on.
    let fleet_size = fleet.len() as i64;
    for page_size in [Some(1), Some(4), Some(fleet_size), None] {
        let summaries = whole_listing(&emulator, page_size, fleet.len()).await;
        let served: Vec<&str> = summaries
            .iter()
            .map(|summary| summary["name"].as_str().unwrap_or_default())
            .collect();
        assert_eq!(
            served, ordered_names,
            "page size {page_size:?}: the listing serves every pipeline of the fleet once, \
             in name order, got {summaries:?}"
        );

        for (member, fetched) in fleet.iter().zip(&fetched) {
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
                "case {case:?}: the summary must report the type the declaration resolved \
                 to, got {summary}"
            );

            assert_eq!(
                summary["pipelineType"], fetched["pipeline"]["pipelineType"],
                "case {case:?}: the summary must report the same type the stored structure \
                 carries ({}), got {summary}",
                fetched["pipeline"]
            );

            for (instant, secs) in [
                ("created", member.created_secs),
                ("updated", member.updated_secs),
            ] {
                let expected = secs as i64 * 1_000 + member.sub_second_millis;
                let path = format!("the summary of {}.{instant}", member.name);
                let actual = epoch_millis(&case, &path, &summary[instant]);
                assert_eq!(
                    actual, expected,
                    "case {case:?}: the summary must report {instant} as the millisecond \
                     the stored instant fell in, got {summary}"
                );
                let reported = epoch_millis(
                    &case,
                    &format!("the metadata of {}.{instant}", member.name),
                    &fetched["metadata"][instant],
                );
                assert_eq!(
                    actual, reported,
                    "case {case:?}: the summary must report {instant} as the pipeline's own \
                     metadata reports it ({}), got {summary}",
                    fetched["metadata"]
                );
            }
        }
    }
}
