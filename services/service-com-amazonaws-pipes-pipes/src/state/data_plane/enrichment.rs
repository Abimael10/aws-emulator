// SPDX-License-Identifier: CC0-1.0

//! The ENRICHMENT stage of a pipe: what the records polled together are handed to it as,
//! and what ONE record it cannot handle costs the records beside it.
//!
//! The records polled together are handed to the enrichment as ONE JSON array of record
//! envelopes and what it RETURNS is what the target stage delivers, which is the whole of
//! the stage for an enrichment that answers ([`super`]'s module docs state that rule and
//! [`super::invoke_enrichment`] makes the call). This module is the other half: the
//! ACCOUNTING of a failure, which is per RECORD and not per batch.
//!
//! A record the enrichment could not handle is retained at its source — it is never
//! deleted, so its Amazon SQS visibility timeout lapses and the queue hands it over again —
//! and it is withheld from the target, while the records the enrichment DID handle go on to
//! the target stage and are finished with their source as any delivered record is. So one
//! record the enrichment raises on delays that record alone: the traffic beside it is
//! neither held back nor lost.
//!
//! Which records those are is a question only the enrichment can answer, so it is ASKED:
//! an attempt at the whole batch that the enrichment answered an error of its own on is
//! attempted again record by record, each record as the one-element array it would be in a
//! batch of its own, and the answers say which records the enrichment handles and which it
//! does not. The attempts a failure costs are therefore bounded by the records the batch
//! holds, and only a batch the enrichment ERRORED on pays them: an attempt the enrichment
//! answered is one call, as before.
//!
//! Two failures are NOT accounted per record, because neither is about the records:
//!
//! * a call the pipe could not make or the called service refused — a role that may not
//!   invoke, a resource that is not there, a throttled or erroring service — says nothing
//!   about any one record, so the batch is withheld whole and attempted again as it stands;
//! * a batch from a source whose records are not retained one by one. An Amazon SQS receive
//!   checks each message out and deletes it on its own, so leaving one behind leaves exactly
//!   that one; a stream read has ONE position per shard, so the records a read can be
//!   finished with are the run at the head of the batch and a record left behind holds every
//!   record after it ([`super::Polled::retains_each_record`]).

use std::collections::BTreeSet;

use ::serde_json::Value;

use super::{
    execution_log, invoke_enrichment, transformed, Admitted, CallFailure, Clients, Enrichment,
    FailedAttempt, LiveRecord, RunningPipe, Stage,
};

/// What the enrichment stage leaves the pass with: the events the TARGET stage delivers, the
/// records this stage withheld from it, and the failure those records are reported by.
pub(super) struct Enriched {
    /// What the target stage delivers: what the enrichment returned, or the record envelopes
    /// themselves for a pipe that configures no enrichment.
    pub(super) events: Vec<Value>,
    /// The records the enrichment could not handle, by the identifier their source names each
    /// of them by: the ones retained at that source and withheld from the target.
    pub(super) withheld: BTreeSet<String>,
    /// The failure the withheld records are reported by, so the pass that delivered the rest
    /// is still the failed attempt this stage made. `None` when the stage withheld nothing.
    pub(super) failure: Option<FailedAttempt>,
}

impl Enriched {
    /// The stage's answer when nothing was withheld: `events` is the whole of what the target
    /// stage delivers — what an enrichment returned, or the record envelopes themselves for a
    /// pipe that configures no enrichment.
    fn of(events: Vec<Value>) -> Self {
        Self {
            events,
            withheld: BTreeSet::new(),
            failure: None,
        }
    }
}

/// Hand `admitted` to the pipe's enrichment and answer what the TARGET stage delivers, with
/// the records the enrichment could not handle withheld from it.
///
/// A pipe that configures NO enrichment has this stage answer the record envelopes themselves,
/// which is the batch the target stage then delivers.
///
/// `retains_each_record` is whether the source these records came off hands them over — and
/// takes them back — one by one, which is what makes retaining one of them possible at all.
///
/// `Err` is the attempt no record of the batch got through: the whole of it stays on the
/// source and a later pass hands it over again.
pub(super) async fn enriched(
    callback: &smithy_server_runtime::EmbeddedEmulator,
    clients: &mut Clients,
    record: &LiveRecord<'_>,
    decided: &RunningPipe,
    admitted: Vec<Admitted>,
    retains_each_record: bool,
    log: &mut execution_log::Execution<'_>,
) -> Result<Enriched, FailedAttempt> {
    // The identifier of each admitted record beside what the enrichment is handed FOR it:
    // which record of the source an answer is about is what retaining that one record needs.
    let (identifiers, batch): (Vec<String>, Vec<Value>) = admitted
        .into_iter()
        .map(|record| (record.identifier, record.event))
        .unzip();
    let Some(enrichment) = &decided.enrichment else {
        return Ok(Enriched::of(batch));
    };
    // An enrichment is handed the records rendered through ITS input template, when the pipe
    // carries one, because that template is what it receives instead of the record.
    let handed = transformed(
        decided.enrichment_template.as_deref(),
        batch,
        &decided.template_variables(),
    )
    .map_err(|detail| FailedAttempt::withheld(Stage::Enrichment, detail))?;
    if let Some(detail) = record.refusal(enrichment.invocation_action(), enrichment.arn()) {
        return Err(FailedAttempt::withheld(Stage::Enrichment, detail));
    }
    log.stage_started(Stage::Enrichment, &handed);
    match invoke_enrichment(callback, clients, enrichment, &handed).await {
        Ok(events) => {
            log.stage_succeeded(Stage::Enrichment, Some(&events));
            Ok(Enriched::of(events))
        }
        Err(failure) => {
            // The service the enrichment was called on refused it, which is the one signal a
            // client has that its enrichment is failing: the record naming that refusal is
            // written before the execution's own verdict.
            if let Some(refusal) = &failure.refusal {
                log.invocation_failed(Stage::Enrichment, &handed, refusal);
            }
            if !asks_record_by_record(&failure, handed.len(), retains_each_record) {
                return Err(FailedAttempt::from_call(Stage::Enrichment, failure));
            }
            record_by_record(callback, clients, enrichment, &handed, &identifiers, log).await
        }
    }
}

/// Whether the batch a failed attempt was made at is attempted again RECORD BY RECORD, so the
/// records the enrichment handles are told from the ones it does not.
///
/// All three hold or the batch is withheld whole: the enrichment itself answered an error
/// (`function_error`, the invoked resource's own word for what went wrong — a failure no
/// service answered, or one a service refused, is not about any one record), the batch holds
/// more than one record (a batch of one is already as isolated as it gets), and the records
/// are retained one by one by the source they came off.
fn asks_record_by_record(failure: &CallFailure, held: usize, retains_each_record: bool) -> bool {
    retains_each_record && held > 1 && failure.function_error.is_some()
}

/// Attempt the enrichment once per record of `handed`, each record as the one-element array it
/// would be in a batch of its own, and answer what the target stage delivers with the records
/// the enrichment could not handle withheld from it.
///
/// `Err` when the enrichment handled NO record of the batch: there is nothing to deliver and
/// nothing is finished with the source, which is the whole batch withheld as before.
async fn record_by_record(
    callback: &smithy_server_runtime::EmbeddedEmulator,
    clients: &mut Clients,
    enrichment: &Enrichment,
    handed: &[Value],
    identifiers: &[String],
    log: &mut execution_log::Execution<'_>,
) -> Result<Enriched, FailedAttempt> {
    let mut events: Vec<Value> = Vec::new();
    let mut withheld: BTreeSet<String> = BTreeSet::new();
    // The FIRST record's failure is the one the pass reports: every record the enrichment
    // failed is withheld for it, and the stage that withheld them is the same stage.
    let mut failure: Option<FailedAttempt> = None;
    for (identifier, record) in identifiers.iter().zip(handed) {
        let one = ::std::slice::from_ref(record);
        log.stage_started(Stage::Enrichment, one);
        match invoke_enrichment(callback, clients, enrichment, one).await {
            Ok(answered) => {
                log.stage_succeeded(Stage::Enrichment, Some(&answered));
                events.extend(answered);
            }
            Err(call) => {
                if let Some(refusal) = &call.refusal {
                    log.invocation_failed(Stage::Enrichment, one, refusal);
                }
                withheld.insert(identifier.clone());
                if failure.is_none() {
                    failure = Some(FailedAttempt::from_call(Stage::Enrichment, call));
                }
            }
        }
    }
    match failure {
        // No record of the batch got through: nothing is delivered and nothing is finished
        // with the source, so the pass is the failed attempt it was.
        Some(failure) if withheld.len() >= identifiers.len() => Err(failure),
        failure => Ok(Enriched {
            events,
            withheld,
            failure,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A call failure the invoked resource named an error of its own on.
    fn errored() -> CallFailure {
        CallFailure {
            detail: "the enrichment answered an Unhandled error".to_string(),
            function_error: Some("Unhandled".to_string()),
            refusal: None,
        }
    }

    /// A failure no service answered: the call could not be made at all.
    fn unanswered() -> CallFailure {
        CallFailure::from("the enrichment could not be invoked".to_string())
    }

    /// RULE: only a failure the ENRICHMENT itself answered, over a batch of more than one
    /// record whose source retains its records one by one, is attempted record by record.
    #[test]
    fn a_failure_is_asked_about_record_by_record_only_when_it_can_be() {
        let cases = [
            (errored(), 2, true, true),
            (errored(), 1, true, false),
            (errored(), 10, false, false),
            (unanswered(), 2, true, false),
            (unanswered(), 1, false, false),
        ];
        for (failure, held, retains, expected) in cases {
            let answered = asks_record_by_record(&failure, held, retains);
            assert_eq!(
                answered, expected,
                "case {failure:?} over {held} record(s), retained one by one: {retains}: \
                 expected {expected}, got {answered}"
            );
        }
    }
}
