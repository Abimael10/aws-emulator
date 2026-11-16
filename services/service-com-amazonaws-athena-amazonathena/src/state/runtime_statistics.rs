// SPDX-License-Identifier: CC0-1.0

//! The RUNTIME STATISTICS of one query execution: the spans each phase of it took, the
//! rows and bytes its run read and returned, and the stage it ran as.
//!
//! Every number here is one the execution's own record already holds. The TIMELINE is the
//! same set of spans the execution reports as its `QueryExecutionStatistics`
//! (`statistics_at`), member for member, so the two reads of one run can never tell a
//! client two different stories about it. The ROW and BYTE counters are what the run
//! measured: the rows and bytes it read out of the objects its statement named, and the
//! rows and bytes it returned. The OUTPUT STAGE reports those same measures for the one
//! stage this engine runs a statement as, with the engine's own span as its execution time.
//!
//! Nothing here writes: a read derives what it reports from the stored outcome and the
//! clock, exactly as every other read of an execution does, so two reads at one instant
//! report the same statistics and neither moves the execution along.

use std::time::SystemTime;

use crate::ops::types::{
    QueryRuntimeStatistics, QueryRuntimeStatisticsRows, QueryRuntimeStatisticsTimeline, QueryStage,
};

use super::query_engine::Outcome;
use super::{query_results, QueryExecutionRecord};

/// The `StageId` of the stage a statement runs as here.
///
/// This engine runs a statement as ONE stage over the objects it reads itself — the same
/// fact that makes a run's input bytes everything it read rather than anything another
/// stage handed it — so the output stage, the stage whose output is the query's own, is
/// also the first one.
const OUTPUT_STAGE_ID: i64 = 0;

/// The `State` the output stage of a run that completed is in: its work is over, which is
/// what the query engine's terminal stage state reports.
const STAGE_STATE_FINISHED: &str = "FINISHED";

impl QueryExecutionRecord {
    /// The `QueryRuntimeStatistics` this execution reports at `now`.
    ///
    /// The timeline is reported from the first read on, as the spans measured so far; the
    /// counters and the output stage are reported once the run has REPORTED its results and
    /// the execution is terminal, which is when the statistics beside the timeline exist at
    /// all (a query still queued or running, one whose statement failed and one a caller
    /// cancelled measured no rows and no bytes, so none are reported for it).
    pub fn to_runtime_statistics(&self, now: SystemTime) -> QueryRuntimeStatistics {
        let statistics = self.statistics_at(now);
        // Each span is the one the execution's own statistics report under the same name,
        // taken from that view rather than measured again here.
        let timeline = QueryRuntimeStatisticsTimeline {
            query_queue_time_in_millis: statistics.query_queue_time_in_millis,
            service_pre_processing_time_in_millis: statistics.service_pre_processing_time_in_millis,
            query_planning_time_in_millis: statistics.query_planning_time_in_millis,
            engine_execution_time_in_millis: statistics.engine_execution_time_in_millis,
            service_processing_time_in_millis: statistics.service_processing_time_in_millis,
            total_execution_time_in_millis: statistics.total_execution_time_in_millis,
        };
        let rows = self.runtime_rows_at(now);
        let output_stage = rows.as_ref().map(|rows| QueryStage {
            stage_id: Some(OUTPUT_STAGE_ID),
            state: Some(STAGE_STATE_FINISHED.to_string()),
            // The stage's counters are the run's own: there is one stage, so what the
            // query read and returned is what that stage read and returned.
            input_rows: rows.input_rows,
            input_bytes: rows.input_bytes,
            output_rows: rows.output_rows,
            output_bytes: rows.output_bytes,
            // What the engine spent on the statement, the same span the timeline reports.
            execution_time: timeline.engine_execution_time_in_millis,
            // This emulator publishes no PLAN for a statement — it compiles and runs it
            // rather than describing how — so the plan tree is absent rather than
            // carrying nodes nothing produced, and a single-stage run feeds no substage.
            query_stage_plan: None,
            sub_stages: None,
        });
        QueryRuntimeStatistics {
            timeline: Some(timeline),
            rows,
            output_stage,
        }
    }

    /// The rows and bytes this run read and returned, or `None` while it has reported none.
    fn runtime_rows_at(&self, now: SystemTime) -> Option<QueryRuntimeStatisticsRows> {
        let Some(Outcome::Succeeded {
            result,
            data_scanned_bytes,
            ..
        }) = self.outcome.as_ref()
        else {
            return None;
        };
        if !self.is_terminal_at(now) {
            return None;
        }
        Some(QueryRuntimeStatisticsRows {
            // What the run READ: the rows materialised for it out of the objects its
            // statement named, and the bytes those values held.
            input_rows: Some(self.input_rows_at(now)),
            input_bytes: Some(*data_scanned_bytes),
            // What it RETURNED: the data rows of the result set it produced, and the bytes
            // those rows hold as the results it saved write them.
            output_rows: Some(i64::try_from(result.rows.len()).unwrap_or(i64::MAX)),
            output_bytes: Some(query_results::returned_bytes(self)),
        })
    }
}
