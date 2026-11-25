Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ssm.send-automation-signal-invalid-state-rejected
Issue-Shape-Id: com.amazonaws.ssm#AmazonSSM
Issue-Key: ssm.send-automation-signal-invalid-state-rejected|run#13|AssertionError|assert early_resume == (
Issue-Related: 20261105T123903Z-ssm-sendautomationsignal-stub.md

# ssm SendAutomationSignal: the refusal MESSAGE for a signal the execution's state does not admit is emulator-authored

> _Filed by the fixer of `20261105T123903Z-ssm-sendautomationsignal-stub.md`, which carried this
> check's replay key. That issue's own cause is resolved — `SendAutomationSignal` is implemented and
> state-backed, it is no longer a 501 scaffold, and the sibling unit
> `ssm.send-automation-signal-nonexistent-step-rejected` passes. This cause is NOT resolved: it needs
> the recorded sentence, which no artifact available to a fixer discloses, so it is filed on its own
> rather than lost when that report moved._

## Operation
SendAutomationSignal

## Description
The emulator refuses a signal the execution's own state does not admit with the right status and the
right error class and an emulator-authored sentence.

- AWS: `SendAutomationSignal` models `AutomationExecutionNotFoundException` (400),
  `AutomationStepNotFoundException` (400), `InvalidAutomationSignalException` (400) and
  `InternalServerError` (500). A `Resume` whose `Payload.StepName` names a step the runbook DECLARES
  but the walk has not reached, sent while the execution is `InProgress` at an earlier step, is
  refused — the recorded unit asserts a refusal, and the `(Error.Code, Error.Message)` pair it
  compares against is recorded behavior. The code the emulator answers is not known to diverge; the
  MESSAGE does. Sibling refusals of this operation show the shapes this subsystem's sentences take:
  "The input automation execution does not exist." (unknown execution id),
  "Failed to process signal Resume. Step <StepName> is not defined in document: <document> (version
  <version>)." (a step the runbook does not declare), "One StepName is required when signal type is
  <SignalType>." (a step-naming signal whose payload names none) and "The step you are trying to stop
  doesn't exist." (a `StopStep` naming a step execution the execution does not hold).
- Emulator: `crate::state::automation::signal_not_valid_message` renders
  "Failed to process signal <SignalType>. The signal isn't valid for the current Automation
  execution." — the `InvalidAutomationSignalException` DOC sentence from the Smithy model, prefixed
  with the recorded leading text of the step-not-defined refusal. The doc sentence is a documentation
  artifact: the wire corpus already showed this subsystem does not emit it for the neighbouring
  precondition (`20261114T164648Z-ssm-2-net-new-causes-sendautomationsignal-stopstep-precondition.md`,
  cause 1), so it is not AWS's wording here either.
- Why: the message is the whole content of this refusal. A client that branches on it cannot tell
  "the automation is not parked at a step yet, retry later" from any other invalid-signal condition,
  and message-keyed tooling never matches.
- Fix: `crate::state::automation::signal_not_valid_message`
  (`services/service-com-amazonaws-ssm-amazonssm/src/state/automation.rs`), the one place
  `send_automation_signal.rs` words this class from — render AWS's sentence for this class, with the
  variable parts taken from the request and from the execution's own state. The refusal class, the
  400 status and the untouched-execution effect are already right and are pinned by
  `services/service-com-amazonaws-ssm-amazonssm/tests/all/automation_signal_state_admission.rs`;
  nothing in that suite asserts the sentence, so only the message function needs to change.
- Traces: ssm.send-automation-signal-invalid-state-rejected.

## Evidence
Replaying the behavioral unit `ssm.send-automation-signal-invalid-state-rejected`, its check fails at
`check_ssm_send_automation_signal_invalid_state_rejected.py` line 72,
`assert early_resume == (...), early_resume`, with the emulator's own pair as the assertion text:

```
AssertionError: ('InvalidAutomationSignalException', "Failed to process signal Resume. The signal
isn't valid for the current Automation execution.")
```

What the check does, established by instrumenting the handler's refusal and reading the replayed
AssertionError back: it creates an Automation runbook of two steps — `sleepStep` (`aws:sleep`) then
`pauseStep` (`aws:pause`) — starts it in `Auto` mode, waits until the execution is `InProgress` at
`sleepStep` (so `pauseStep` is still `Pending`), and sends
`SendAutomationSignal(SignalType='Resume', Payload={'StepName': ['pauseStep']})`.

The recorded answer is a REFUSAL, not a 200: when the emulator was made to serve that call, the same
unit failed instead with `expected the signal to be refused, but it succeeded: {...200...}` (the
check's own `_error` helper, which returns `(Error.Code, Error.Message)` for a refusal). So status and
class are settled and the message text is the whole remaining divergence.

The recorded sentence is disclosed nowhere a fixer can read: the parent issue report does not quote
it, the replay withholds `expected` for a check unit, and no file in the checkout carries it. These 15
candidate `(code, message)` pairs were each built and re-replayed, and every one diverges:

- `InvalidAutomationSignalException` with: the doc sentence alone; the doc sentence with the
  "Failed to process signal Resume." prefix (today's behaviour); "… Automation execution is in
  InProgress status."; "… Automation execution is not in Waiting status."; "… The automation execution
  is not in Waiting status."; "… Automation execution is not waiting for a signal."; "… Automation
  execution is not in a waiting state."; "… Automation execution is not in a valid state.";
  "… Step pauseStep is not in Waiting status."; "Signal Resume is not valid for automation execution
  in InProgress status."; "Signal Resume is not valid for the current Automation execution."; "The
  signal is not valid for the current Automation execution." (prefixed); and no `Message` member at
  all.
- `AutomationStepNotFoundException` with: the recorded "Failed to process signal Resume. Step
  pauseStep is not defined in document: <document> (version 1)." template; and the modelled "The
  specified step name and execution ID don't exist. Verify the information and try again." sentence.

So this cause needs the recorded response quoted in the report, the way cause 5 of
`20261115T084659Z-ssm-10-net-new-behavioral-causes-maintenance-window.md` quoted the `StopStep`
sentence. With the sentence in hand the fix is a one-function change.

## Steps to Reproduce
1. CreateDocument an `Automation` runbook with two steps: `sleepStep` (`aws:sleep`, a `Duration` long
   enough to observe, e.g. `PT5M`) and `pauseStep` (`aws:pause`).
2. StartAutomationExecution it (default `Auto` mode) and poll GetAutomationExecution until
   `AutomationExecutionStatus` is `InProgress` with `CurrentStepName` `sleepStep`.
3. Call SendAutomationSignal with that `AutomationExecutionId`, `SignalType='Resume'` and
   `Payload={'StepName': ['pauseStep']}`.
4. Observe HTTP 400 `InvalidAutomationSignalException` with `Message` "Failed to process signal
   Resume. The signal isn't valid for the current Automation execution." — the Smithy doc sentence,
   not the sentence Systems Manager words this refusal with.
