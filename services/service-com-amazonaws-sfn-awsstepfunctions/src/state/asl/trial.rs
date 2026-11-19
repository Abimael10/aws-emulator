// SPDX-License-Identifier: CC0-1.0

//! The single-state trial `TestState` runs: ONE state of a definition, with no
//! execution around it.
//!
//! One concern in a file of its own, reached from [`super`]: the request fixes the
//! state, its input, the attempt it is on and the mock that stands in for its work, and
//! the walk reports what that one state computed.

use super::*;

/// One `TestState` call: the state under test, and everything the request fixed
/// about it.
pub struct StateTest<'a> {
    /// The state's name, which `$$.State.Name` reports. Empty for a `definition`
    /// that is the state alone, which carries no name — only a `stateName` naming a
    /// state of a full definition gives it one.
    pub name: &'a str,
    /// The state's own definition: the object the Amazon States Language declares it
    /// as.
    pub state: &'a Map<String, Value>,
    /// "A string that contains the JSON input data for the state", as the caller
    /// wrote it.
    pub input: &'a str,
    /// When the state is being tested, which `$$.State.EnteredTime` reports.
    pub start: SystemTime,
    /// What the context object reports about this test, for the members the request
    /// fixes.
    pub context: &'a Context,
    /// "A JSON string representing a valid Context object for the state under test":
    /// the whole `$$` document, which replaces the one built from `context`.
    pub context_override: Option<&'a Value>,
    pub mock: Option<&'a Mock>,
    /// `stateConfiguration.retrierRetryCount`: "The number of retry attempts that
    /// have occurred for the state's Retry that applies to the mocked error", which
    /// is what decides whether the state's failure is still RETRIABLE.
    pub retries_taken: u64,
    /// "JSON object literal that sets variables used in the state under test. Object
    /// keys are the variable names and values are the variable values": the scope the
    /// state's own expressions read, before its `Assign` writes to it.
    pub variables: Option<&'a Value>,
}

/// Run the ONE state `test` names over its input and report what it produced.
///
/// This is `TestState`'s walk: a single state, with no state machine around it, so
/// nothing is transitioned to and nothing is persisted. The state's data flow, its
/// `Choice` rules, its `Wait` members and its `Fail` members are evaluated by the
/// same code an execution's walk uses; an invocation this process cannot make is
/// answered by the request's `mock`, and without one the state fails with ASL's own
/// [`RUNTIME_ERROR`] rather than with a fabricated result.
pub fn test_state(test: &StateTest<'_>) -> Trial {
    // The trial is a caller-sized walk too, so it runs under the same work allowance an
    // execution's does ([`work_meter`]).
    let _allowance = WorkAllowance::of(MAX_INTERPRET_WORK);
    let inspection = Inspection {
        input: Some(test.input.to_string()),
        ..Inspection::default()
    };
    let raw: Value = match serde_json::from_str(test.input) {
        Ok(value) => value,
        Err(why) => {
            return Trial::failed(
                inspection,
                runtime(format!(
                    "the state input is not valid JSON: {}",
                    crate::state::json_parse_diagnostic(test.input, &why)
                )),
            );
        }
    };
    // The input is a caller's document too, so it is measured against the ceiling
    // every document a state produces is measured against: an input larger than a
    // state's output may carry is answered with ASL's own data-limit error rather
    // than evaluated, which is also what bounds what one path or one `StringMatches`
    // rule can be asked to walk.
    if let Err(failure) = measure_payload(&raw, UNNAMED_PRODUCER) {
        // The document measured here is the state's INPUT, which the request supplied:
        // the quota sentence is about a result a state RETURNED, so an input past the
        // ceiling says what it is instead.
        return Trial::failed(
            inspection,
            Failure {
                error: failure.error,
                cause: Some(format!(
                    "the state input is larger than the {MAX_PAYLOAD_BYTES} bytes a state's \
                     document may carry"
                )),
            },
        );
    }
    let base_context = match test.context_override {
        Some(context) => context.clone(),
        None => test.context.value(&raw, test.start),
    };
    let mut interpreter = Interpreter {
        // `TestState` keeps no history of its own, so the allowance it runs under is a
        // whole execution's: what bounds the ONE state it tests is the work that state
        // asks for and not a history it writes nothing into.
        events_left: MAX_HISTORY_EVENTS,
        // The one state under test records what it records for the trial to report; no
        // child workflow execution's events are counted here, because `TestState` starts
        // none.
        unheld: None,
        depth: 0,
        call_wait_left: MAX_CALL_WAIT_DURATION,
        // `TestState` tests ONE state, with no execution and so no definition-level
        // `TimeoutSeconds` around it: nothing here bounds the span the state runs in.
        timeout_left: None,
        timed_out: false,
        // The state is tested as the attempt the request says it is on, which is what
        // it reads through `$$.State.RetryCount`.
        retry_count: test.retries_taken,
        steps: Vec::new(),
        // `TestState` keeps no history: the state under test is the first thing this
        // walk would record, so a diagnosis it raises names the id the first event of
        // a history carries.
        first_event_id: FIRST_EVENT_ID,
        start: test.start,
        // `TestState` owes the clock nothing: it reports what one state computes in the
        // call that asked, and the child executions a span like this would date belong
        // to an execution.
        settle: Duration::ZERO,
        base_context,
        // `TestState` tests ONE state, with no execution around it: a state whose
        // work is an invocation is tested through its `mock`, so nothing is called
        // for this walk and there is nowhere for it to wait.
        tasks: &super::super::integration::Unserved,
        waiting: None,
        parked: None,
        // `TestState` tests one state, so there is no concurrent state around it to
        // park inside, no job of its own to abandon and no sibling branch whose
        // failure could stop it.
        abandoned: Vec::new(),
        cancel_after: None,
        resume_frames: Vec::new(),
        map_depth: 0,
        // `TestState` starts nothing: it reports what one state computes, and the
        // Map Runs a Distributed Map starts — and the child executions its items run
        // as — belong to an execution.
        map_runs: Vec::new(),
        map_runs_entered: 0,
        map_runs_entered_by_label: BTreeMap::new(),
        // `TestState` continues no execution: a state whose work is an invocation is
        // answered by the request's own `mock`, not by a worker's reply.
        reply: None,
        job_reply: None,
        callback_reply: None,
        resumed_attempts: None,
        attempts_spent: Vec::new(),
        resumed: None,
        machine_name: test.context.state_machine_name.clone(),
        region: super::super::arn_region(&test.context.state_machine_arn),
        execution_arn: test.context.execution_arn.clone(),
        child_counter: 0,
        // The state under test belongs to no state machine, so no workflow type
        // refuses it: the context the request supplies says nothing of one.
        express: test.context.express,
        // `TestState` redrives nothing: the step a redrive would continue from
        // belongs to an execution.
        failed_at: None,
        // The state under test is written in the language it declares; there is no
        // definition around it to inherit one from, so the other language's default
        // stands in for the absent member.
        language: JSONPATH,
        // "The set will include variables assigned in the state and variables set up
        // as test state input": the request's own set is what the state's expressions
        // read, and its `Assign` writes into it.
        variables: test
            .variables
            .and_then(Value::as_object)
            .map(|variables| {
                variables
                    .iter()
                    .map(|(name, value)| (name.clone(), value.clone()))
                    .collect()
            })
            .unwrap_or_default(),
        // Nothing has been assigned BY the state under test yet; what the request set
        // up is the scope around it.
        assigned: BTreeMap::new(),
        // The request's own input is the caller's document here, so what the trial
        // reports of it is the bytes the request carried. The state under test
        // arrives PARSED, so the text the definition wrote its own documents in is
        // not one this walk was given.
        supplied: {
            let mut supplied = Supplied::default();
            supplied.supplied(test.input);
            supplied
        },
        // `TestState` continues no execution either, so the state under test is entered
        // by this walk and names the event this walk numbers for it.
        continued_entry: None,
        skipping_entry: false,
    };
    // And the same document for the diagnoses the trial's own expressions word, which
    // quote the document they could not be applied to ([`GivenDocuments`]).
    let _given = GivenDocuments::of(&interpreter.supplied);
    interpreter.trial(test, &raw, inspection)
}

impl Interpreter<'_> {
    /// [`test_state`]'s walk of the one state under test.
    pub(super) fn trial(&mut self, test: &StateTest<'_>, raw: &Value, mut inspection: Inspection) -> Trial {
        let state = test.state;
        let state_type = state.get("Type").and_then(Value::as_str).unwrap_or_default();
        let context = self.context_for(&Scope::default(), test.name, Duration::ZERO);

        if let Some(language) = state.get("QueryLanguage").and_then(Value::as_str) {
            if !QUERY_LANGUAGES.contains(&language) {
                return Trial::failed(
                    inspection,
                    runtime(format!(
                        "{SINGLE_STATE_LABEL} declares the query language '{}', which is not one \
                         of {}, so nothing was run for it",
                        echoed(language),
                        QUERY_LANGUAGES.join(", ")
                    )),
                );
            }
        }
        let language = match self.language_of(state) {
            Ok(language) => language,
            Err(why) => return Trial::failed(inspection, runtime(why)),
        };
        // A member this emulator does not interpret changes what the state does, so
        // the state is not run for it — the rule the walk applies at every state. A
        // MOCKED state is exempt: the members that describe the invocation the mock
        // stands in for are the mock's business, and nothing is invoked for them; a
        // type this emulator does not run at all is reported by `produce` as the
        // invocation it is, not as a member.
        if test.mock.is_none() && EXECUTED_STATE_TYPES.contains(&state_type) {
            if let Some(member) = unexecutable_member(state_type, state, language) {
                return Trial::failed(
                    inspection,
                    runtime(format!(
                        "{SINGLE_STATE_LABEL} carries the member '{member}', which this emulator \
                         does not interpret, so nothing was run for it"
                    )),
                );
            }
        }
        // The input side: `InputPath`, then the `Parameters` payload template — or, in
        // JSONata, the `Arguments` the state hands its work. "Not populated when the
        // QueryLanguage is JSONata" is why each stage is reported only for the
        // language that has it.
        let effective = match self.input_value(state, raw, &context) {
            Ok(value) => value,
            Err(failure) => return self.classify(test, language, raw, &context, failure, inspection),
        };
        if language != JSONATA {
            inspection.after_input_path = Some(self.echo(&effective));
        }
        let templated = TEMPLATED_STATE_TYPES.contains(&state_type);
        let after_parameters = match templated {
            true => match self.task_request(state, language, &effective, &context) {
                Ok(value) => value,
                Err(failure) => {
                    return self.classify(test, language, raw, &context, failure, inspection)
                }
            },
            false => effective.clone(),
        };
        if templated {
            match language {
                JSONATA => inspection.after_arguments = Some(self.echo(&after_parameters)),
                _ => inspection.after_parameters = Some(self.echo(&after_parameters)),
            }
        }

        // A `Choice` state's transition is what the state DOES, and the walk decides
        // it before the state's output is built, so a rule that cannot be evaluated
        // is what such a state fails with.
        let chosen = if state_type == "Choice" {
            match self.choose(state, language, &effective, &context) {
                Ok(chosen) => Some(chosen),
                Err(failure) => {
                    return self.classify(test, language, raw, &context, failure, inspection)
                }
            }
        } else {
            None
        };

        let produced = self.produce(
            state,
            state_type,
            language,
            &after_parameters,
            &effective,
            &context,
            test.mock,
        );
        let placed = match produced {
            Ok(result) => match language {
                // A JSONata state's output is its `Output`, over the variables its
                // own `Assign` has just written; there are no result-side filters to
                // record stages for. A `Choice` state's own `Assign` and the SELECTED
                // rule's decide it together, exactly as they do in the walk.
                JSONATA => {
                    if let Some(result) = result.as_ref() {
                        inspection.result = Some(self.echo(result));
                    }
                    match chosen.as_ref() {
                        Some(chosen) => {
                            self.choice_output(state, chosen.rule.as_ref(), &effective, &context)
                        }
                        None => {
                            self.jsonata_output(state, &effective, result.as_ref(), None, &context)
                        }
                    }
                }
                _ => match result {
                    Some(result) => self.place_recorded(
                        state,
                        state_type,
                        raw,
                        result,
                        &context,
                        &mut inspection,
                    ),
                    None => self.output_value(state, effective.clone(), &context),
                },
            },
            Err(failure) => Err(failure),
        };
        let output = match placed {
            Ok(output) => output,
            Err(failure) => return self.classify(test, language, raw, &context, failure, inspection),
        };
        // The document the state produced is measured here, at the state that
        // produced it, against what a state's output may carry — as the walk measures
        // every state's.
        if let Err(failure) = measure_payload(&output, SINGLE_STATE_LABEL) {
            return self.classify(test, language, raw, &context, failure, inspection);
        }
        inspection.variables = self.assigned_variables();

        let next_state = match chosen {
            // The rule that matched names the next state; a JSONata rule may also
            // declare the state's own output.
            Some(chosen) => Some(chosen.next),
            None if TERMINAL_STATE_TYPES.contains(&state_type) => None,
            None if state.get("End").and_then(Value::as_bool) == Some(true) => None,
            // A single state's `Next` names a state no definition around it declares,
            // so it is REPORTED rather than resolved; a state that names none has no
            // next state at all.
            None => state.get("Next").and_then(Value::as_str).map(str::to_string),
        };
        Trial::succeeded(Some(self.echo(&output)), next_state, inspection)
    }

    /// The variables this walk has assigned, as the one JSON document `TestState`
    /// reports them through — `None` where nothing assigned any, so the member is
    /// absent rather than an empty object.
    pub(super) fn assigned_variables(&self) -> Option<String> {
        if self.variables.is_empty() {
            return None;
        }
        let variables: Map<String, Value> = self
            .variables
            .iter()
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect();
        Some(Value::Object(variables).to_string())
    }

    /// The RAW result the state's own work produced: the document the result-side
    /// filters place, `None` for a state that produces no result of its own (its
    /// effective input is what `OutputPath` reads), or the failure it ended in.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn produce(
        &mut self,
        state: &Map<String, Value>,
        state_type: &str,
        language: &'static str,
        after_parameters: &Value,
        effective: &Value,
        context: &Value,
        mock: Option<&Mock>,
    ) -> Result<Option<Value>, Failure> {
        // The mock IS the outcome of the invocation it stands in for, so nothing is
        // invoked for it and the state's result is the one the request named.
        match mock {
            Some(Mock::Result(result)) => return Ok(Some(result.clone())),
            Some(Mock::Error(failure)) => return Err(failure.clone()),
            None => {}
        }
        match state_type {
            // "Parameters ... Used to create a collection of key-value pairs that
            // will be passed as input": it transforms the state's effective INPUT,
            // while `Result` is the state's RESULT — the rule the walk applies. A
            // JSONata `Pass` has no `Result`: what it puts out is its `Output`, so it
            // produces no result of its own.
            "Pass" => match language {
                JSONATA => Ok(None),
                _ => Ok(Some(
                    state
                        .get("Result")
                        .cloned()
                        .unwrap_or_else(|| after_parameters.clone()),
                )),
            },
            // A `Wait` state produces no result of its own. Its members are still
            // evaluated, so a malformed one is reported as the failure it is, but
            // nothing sleeps for the span it declares.
            "Wait" => {
                self.wait_duration(state, language, effective, context, Duration::ZERO)
                    .map_err(|failure| Failure {
                        error: failure.error,
                        // A diagnosis this module worded about the state's own
                        // expression already names the field it is written in, so it
                        // is carried exactly as raised — the rule the walk applies
                        // around every such diagnosis ([`diagnosed`]). Every other
                        // reason a `Wait`'s members could not be read names the state
                        // under test, which is the only name this trial has for it.
                        cause: failure.cause.map(|why| match names_its_state(&why) {
                            true => why,
                            false => format!("{SINGLE_STATE_LABEL}: {why}"),
                        }),
                    })?;
                Ok(None)
            }
            "Choice" | "Succeed" => Ok(None),
            // A `Fail` state ends with the error and cause it declares, either of
            // which ASL may instead read from the state's input through a `*Path`.
            "Fail" => {
                let error =
                    self.fail_member(state, language, "Error", "ErrorPath", effective, context)?;
                let cause =
                    self.fail_member(state, language, "Cause", "CausePath", effective, context)?;
                Err(Failure { error, cause })
            }
            // Every remaining type's work is an invocation this process does not
            // make: a service integration, an iteration over items, a set of
            // branches. Without a mock to stand in for it, nothing ran — which is
            // what the state fails with, rather than a synthesised result.
            other => Err(runtime(format!(
                "a '{other}' state's own work is an invocation this emulator does not make, and \
                 the request names no mock to stand in for it, so nothing was run for it"
            ))),
        }
    }

    /// The result-side filters over a state's raw result — `ResultSelector`,
    /// `ResultPath` against the state's own raw input, then `OutputPath` — recording
    /// what the state carried after each one.
    pub(super) fn place_recorded(
        &self,
        state: &Map<String, Value>,
        state_type: &str,
        raw: &Value,
        result: Value,
        context: &Value,
        inspection: &mut Inspection,
    ) -> Result<Value, Failure> {
        inspection.result = Some(self.echo(&result));
        let with_selector = SELECTOR_STATE_TYPES.contains(&state_type);
        let selected = select_result(state, result, context, with_selector)?;
        // "The effective result after Step Functions applies the ResultSelector
        // filter": the stage is the state's result as the result-side data flow
        // RECEIVES it, so a state whose type has no such filter — or that declares
        // none — reports its raw result there rather than nothing at all.
        inspection.after_result_selector = Some(self.echo(&selected));
        let placed = path_result(state, raw, selected)?;
        inspection.after_result_path = Some(self.echo(&placed));
        self.output_value(state, placed, context)
    }

    /// What `TestState` reports for a state whose work failed: the attempt one of its
    /// `Retry` retriers still owes (RETRIABLE), the catcher that takes the failure
    /// over (CAUGHT_ERROR), or the failure itself (FAILED).
    ///
    /// The two are the state's OWN error handling, read exactly as the walk reads it:
    /// the first retrier whose `ErrorEquals` names the error decides, and `Catch` is
    /// consulted only once no retrier has an attempt left.
    pub(super) fn classify(
        &mut self,
        test: &StateTest<'_>,
        language: &'static str,
        raw: &Value,
        context: &Value,
        failure: Failure,
        inspection: Inspection,
    ) -> Trial {
        let state = test.state;
        let name = failure.name().to_string();
        // Which `Retry` entry answers for this error, whether or not it still has an
        // attempt left: "the array index of the Retry which handled the exception" is
        // what the trial reports for a failure the state's own error handling took
        // over, and a state with no retrier for the error has none to name.
        let retrier = retry_entries(state, "Retry")
            .iter()
            .enumerate()
            .filter_map(|(index, retrier)| retrier.as_object().map(|retrier| (index, retrier)))
            .find(|(_, retrier)| errors_match(retrier, &name));
        let retry_index = Some(match retrier {
            Some((index, _)) => i32::try_from(index).unwrap_or(i32::MAX),
            None => NO_RETRIER_INDEX,
        });
        if let Some((_, retrier)) = retrier {
            let max_attempts = retrier
                .get("MaxAttempts")
                .and_then(Value::as_u64)
                .unwrap_or(DEFAULT_RETRY_MAX_ATTEMPTS);
            if test.retries_taken < max_attempts {
                let mut trial = Trial::failed(inspection, failure);
                trial.status = TEST_STATUS_RETRIABLE;
                trial.retry_index = retry_index;
                trial.retry_backoff_seconds =
                    Some(retry_delay_seconds(retrier, test.retries_taken).round() as i32);
                return trial;
            }
        }
        for (index, catcher) in retry_entries(state, "Catch").iter().enumerate() {
            let Some(catcher) = catcher.as_object() else {
                continue;
            };
            if !errors_match(catcher, &name) {
                continue;
            }
            let Some(next) = catcher.get("Next").and_then(Value::as_str) else {
                return Trial::failed(
                    inspection,
                    runtime("a 'Catch' catcher must carry a string 'Next'".to_string()),
                );
            };
            // "When a state reports an error and it matches a catcher, the state's
            // output is the error output", placed through the catcher's own
            // `ResultPath` — or, in JSONata, through its own `Output`, which is one
            // of the two places `$states.errorOutput` is in scope.
            let error_output = failure.payload();
            let effective = match self.input_value(state, raw, context) {
                Ok(effective) => effective,
                Err(failure) => return Trial::failed(inspection, failure),
            };
            let output = match language {
                JSONATA => {
                    self.jsonata_output(catcher, &effective, None, Some(&error_output), context)
                }
                _ => self.place_result(catcher, raw, error_output, context, true),
            };
            let output = match output {
                Ok(output) => output,
                Err(failure) => return Trial::failed(inspection, failure),
            };
            let mut trial = Trial::failed(inspection, failure);
            trial.status = TEST_STATUS_CAUGHT_ERROR;
            trial.output = Some(self.echo(&output));
            trial.next_state = Some(next.to_string());
            trial.catch_index = Some(i32::try_from(index).unwrap_or(i32::MAX));
            // A caught failure reports both handler positions: the catcher that took it
            // over, and the retrier whose attempts preceded it.
            trial.retry_index = retry_index;
            return trial;
        }
        Trial::failed(inspection, failure)
    }
}
