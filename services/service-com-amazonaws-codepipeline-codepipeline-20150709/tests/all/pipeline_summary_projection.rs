// SPDX-License-Identifier: CC0-1.0

//! The `PipelineSummary` projection `ListPipelines` serves, read against the OTHER
//! projections of the same stored pipeline, over the wire this service speaks
//! (`awsJson1_1`, `POST /` with `X-Amz-Target: CodePipeline_20150709.<Operation>`).
//!
//! The rule under test: every member of the summary a listing serves for a pipeline is the
//! value the SERVICE resolved and stored for that pipeline — the `pipelineType` its
//! declaration resolved to (the declared one, else the service's default type), the
//! `executionMode` it resolved to, the `version` its writes left it at, and the instants
//! the store holds for it — so the summary reports exactly what the pipeline's own
//! `CreatePipeline`, `UpdatePipeline` and `GetPipeline` answers report for those members,
//! with each instant serialized as an epoch-seconds number at millisecond resolution.
//!
//! Where `tests/all/pipeline_declaration_wire_form.rs` asserts each projection against the
//! values it must carry, this suite asserts the projections AGAINST EACH OTHER: a summary
//! computed from anything but the stored pipeline — a default decided at the listing
//! rather than at the write, or an instant encoded at another precision — disagrees with
//! the structure and metadata the same store answers with and fails here.
//!
//! Every assertion goes through the router, so nothing here can pass by reading a
//! handler's internals.

use serde_json::{json, Value};

use crate::pipeline_declaration_wire_form::epoch_millis;
use crate::pipeline_lifecycle::{declaration, Emulator};

/// The `PipelineType` values the model declares
/// (`smithy_query query="shape" shape="PipelineType"` -> `V1`, `V2`).
const PIPELINE_TYPES: [&str; 2] = ["V1", "V2"];

/// The `PipelineType` a declaration stating none resolves to: the newer of the two the
/// model declares, which `PipelineDeclaration$pipelineType` describes as having "the same
/// structure as a V1 type, along with additional parameters for release safety and trigger
/// configuration".
const DEFAULT_PIPELINE_TYPE: &str = PIPELINE_TYPES[1];

/// The `ExecutionMode` a declaration stating none resolves to
/// (`PipelineDeclaration$executionMode`: "The default mode is SUPERSEDED").
const DEFAULT_EXECUTION_MODE: &str = "SUPERSEDED";

/// The `ExecutionMode` values the model declares
/// (`smithy_query query="shape" shape="ExecutionMode"` -> `QUEUED`, `SUPERSEDED`,
/// `PARALLEL`).
const EXECUTION_MODES: [&str; 3] = ["QUEUED", "SUPERSEDED", "PARALLEL"];

/// One pipeline of the fleet the listing summarises.
struct SummaryRow {
    /// What makes this row different from the others.
    case: &'static str,
    /// The pipeline's name, which is also the key the listing orders by.
    name: &'static str,
    /// The `pipelineType` the declaration states, `None` for one stating none.
    declared_type: Option<&'static str>,
    /// The `executionMode` the declaration states, `None` for one stating none.
    declared_mode: Option<&'static str>,
    /// Whether a later `UpdatePipeline` edits the structure, which advances the version.
    edited: bool,
    /// The `pipelineType` the summary must report.
    pipeline_type: &'static str,
    /// The `executionMode` the summary must report.
    execution_mode: &'static str,
    /// The `version` the summary must report: the number the writes left the pipeline at.
    version: i64,
}

/// What one pipeline's own projections report for the members a summary carries.
struct Projections {
    /// The structure the write (`CreatePipeline`, else `UpdatePipeline`) answered with.
    written: Value,
    /// The structure `GetPipeline` answers with.
    fetched: Value,
    /// The `PipelineMetadata` `GetPipeline` answers with.
    metadata: Value,
}

/// Every member of a `PipelineSummary` is the one the pipeline's OWN structure and
/// metadata report: a listing serves the stored pipeline, not a value decided while
/// answering the listing.
///
/// Rows differ along what the rule generalises over: whether the declaration states a
/// type (neither of the model's two values, the older, the newer), whether it states a
/// mode (none, and two of the model's three values), and whether a later edit moved the
/// pipeline to another version — the member a summary would get wrong if it read the
/// first stored declaration rather than the current one.
#[tokio::test]
async fn every_summary_reports_what_the_pipelines_own_projections_report() {
    let rows: [SummaryRow; 6] = [
        SummaryRow {
            case: "a declaration stating neither a type nor a mode",
            name: "sierra-plain",
            declared_type: None,
            declared_mode: None,
            edited: false,
            pipeline_type: DEFAULT_PIPELINE_TYPE,
            execution_mode: DEFAULT_EXECUTION_MODE,
            version: 1,
        },
        SummaryRow {
            case: "a declaration stating the older type",
            name: "oscar-older",
            declared_type: Some(PIPELINE_TYPES[0]),
            declared_mode: None,
            edited: false,
            pipeline_type: PIPELINE_TYPES[0],
            execution_mode: DEFAULT_EXECUTION_MODE,
            version: 1,
        },
        SummaryRow {
            case: "a declaration stating the newer type",
            name: "november-newer",
            declared_type: Some(PIPELINE_TYPES[1]),
            declared_mode: None,
            edited: false,
            pipeline_type: PIPELINE_TYPES[1],
            execution_mode: DEFAULT_EXECUTION_MODE,
            version: 1,
        },
        SummaryRow {
            case: "a declaration stating a mode but no type",
            name: "quebec-queued",
            declared_type: None,
            declared_mode: Some(EXECUTION_MODES[0]),
            edited: false,
            pipeline_type: DEFAULT_PIPELINE_TYPE,
            execution_mode: EXECUTION_MODES[0],
            version: 1,
        },
        SummaryRow {
            case: "a declaration stating the newer type and a parallel mode",
            name: "papa-parallel",
            declared_type: Some(PIPELINE_TYPES[1]),
            declared_mode: Some(EXECUTION_MODES[2]),
            edited: false,
            pipeline_type: PIPELINE_TYPES[1],
            execution_mode: EXECUTION_MODES[2],
            version: 1,
        },
        SummaryRow {
            case: "a pipeline an edit moved to the next version",
            name: "echo-edited",
            declared_type: None,
            declared_mode: None,
            edited: true,
            pipeline_type: DEFAULT_PIPELINE_TYPE,
            execution_mode: DEFAULT_EXECUTION_MODE,
            version: 2,
        },
    ];

    let emulator = Emulator::new();
    let mut projections: Vec<Projections> = Vec::new();
    for row in &rows {
        let mut declared = declaration(row.name);
        if let Some(declared_type) = row.declared_type {
            declared["pipelineType"] = json!(declared_type);
        }
        if let Some(declared_mode) = row.declared_mode {
            declared["executionMode"] = json!(declared_mode);
        }
        let created = emulator
            .ok("CreatePipeline", &json!({"pipeline": declared.clone()}))
            .await;
        let mut written = created["pipeline"].clone();
        if row.edited {
            // A structure that really DIFFERS from the stored one — another artifact
            // store location — so the write advances the version the summary has to
            // report. The edit changes no stage, so the pipeline keeps declaring each
            // action name once.
            let mut edit = declared.clone();
            edit["artifactStore"]["location"] = json!("another-bucket");
            let updated = emulator
                .ok("UpdatePipeline", &json!({"pipeline": edit}))
                .await;
            written = updated["pipeline"].clone();
        }
        let fetched = emulator.ok("GetPipeline", &json!({"name": row.name})).await;
        projections.push(Projections {
            written,
            fetched: fetched["pipeline"].clone(),
            metadata: fetched["metadata"].clone(),
        });
    }

    let listed = emulator.ok("ListPipelines", &json!({})).await;
    let summaries: Vec<Value> = listed["pipelines"].as_array().cloned().unwrap_or_default();
    for (row, projection) in rows.iter().zip(&projections) {
        let summary = summaries
            .iter()
            .find(|summary| summary["name"] == json!(row.name))
            .cloned()
            .unwrap_or_else(|| {
                panic!(
                    "case {:?}: the listing must serve the pipeline's summary, got {listed}",
                    row.case
                )
            });
        let case = row.case;

        // The members the structure also carries: the summary and the structure are two
        // projections of the one stored pipeline, so they cannot disagree.
        for (member, expected) in [
            ("pipelineType", row.pipeline_type),
            ("executionMode", row.execution_mode),
        ] {
            assert_eq!(
                summary[member],
                json!(expected),
                "case {case:?}: the summary must report {member} as the value the \
                 declaration resolved to, got {summary}"
            );
            assert_eq!(
                summary[member], projection.written[member],
                "case {case:?}: the summary must report the same {member} the write \
                 answered with ({}), got {summary}",
                projection.written
            );
            assert_eq!(
                summary[member], projection.fetched[member],
                "case {case:?}: the summary must report the same {member} GetPipeline \
                 answers with ({}), got {summary}",
                projection.fetched
            );
        }
        assert_eq!(
            summary["version"],
            json!(row.version),
            "case {case:?}: the summary must report the version the writes left the \
             pipeline at, got {summary}"
        );
        assert_eq!(
            summary["version"], projection.fetched["version"],
            "case {case:?}: the summary must report the same version GetPipeline answers \
             with ({}), got {summary}",
            projection.fetched
        );

        // The instants: the same numbers the metadata reports, each an epoch-seconds
        // number at millisecond resolution (`epoch_millis` asserts the resolution). Each is
        // decoded once, so the ordering check below reads the values the assertions above
        // compared.
        let mut summarised = Vec::new();
        for member in ["created", "updated"] {
            let instant = epoch_millis(case, &format!("the summary's {member}"), &summary[member]);
            let reported = epoch_millis(
                case,
                &format!("the metadata's {member}"),
                &projection.metadata[member],
            );
            assert_eq!(
                instant, reported,
                "case {case:?}: the summary must report {member} as the metadata reports \
                 it ({}), got {summary}",
                projection.metadata
            );
            summarised.push(instant);
        }
        let (created, updated) = (summarised[0], summarised[1]);
        assert!(
            updated >= created,
            "case {case:?}: a pipeline is never updated before it was created, got \
             {summary}"
        );
    }
}
