Issue-Source: trace-sweep-divergence
Issue-Trace: glue-trigger-schedule-activate-update-deactivate
Issue-Step: setup#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: glue-trigger-schedule-activate-update-deactivate
Issue-Shape-Id: com.amazonaws.glue#AWSGlue
Issue-Key: glue|CreateTrigger|2xx->4xx|status
Issue-Key: glue|GetTrigger|2xx->4xx|$.Trigger
Issue-Key: glue|GetTrigger|2xx->4xx|status
Issue-Key: glue|StartTrigger|2xx->4xx|$.Name
Issue-Key: glue|StartTrigger|2xx->4xx|status
Issue-Key: glue|GetTrigger|2xx->4xx|$.Trigger.State
Issue-Key: glue|UpdateTrigger|2xx->4xx|$.Trigger
Issue-Key: glue|UpdateTrigger|2xx->4xx|status
Issue-Key: glue|StopTrigger|2xx->4xx|$.Name
Issue-Key: glue|StopTrigger|2xx->4xx|status
Issue-Commit: b8e5c8616008a33e0a34d66e8eacd10130426f37

# Glue CreateTrigger refuses a scheduled trigger, so the whole trigger lifecycle fails

> _Note: this issue was identified against the code base at commit `b8e5c8616008a33e0a34d66e8eacd10130426f37`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateTrigger

## Description
A SCHEDULED trigger whose single action names a job that does not exist is created by AWS (200) but refused by the emulator (400), and because the trigger is never stored every subsequent call in the lifecycle refuses too. Six causes, one root plus five dependent read/mutate paths that must each answer correctly once the trigger exists.

Cause 1 of 6: CreateTrigger refuses the request AWS accepts.
AWS: Smithy: CreateTrigger input Name, Type (TriggerType enum) and Actions (ActionList) @required, Schedule a GenericString; modeled errors AlreadyExistsException, EntityNotFoundException, InvalidInputException, ResourceNumberLimitExceededException (all 400). Recorded behavior: AWS returns 200 for Type=SCHEDULED with a cron Schedule and Actions=[{JobName:<a job that does not exist>}] — it does NOT resolve the action's job at create time, and the trigger is created in State CREATED.
Emulator: 400.
Why: AWS lets a trigger be declared before its target job exists (the normal IaC ordering); refusing it makes that ordering impossible and aborts the whole lifecycle.
Fix: the CreateTrigger handler's validation (drop the existence check on Actions[].JobName) and its trigger state insert.
Traces: glue-trigger-schedule-activate-update-deactivate#setup#1.

Cause 2 of 6: GetTrigger on the created trigger must return it in State CREATED.
AWS: Smithy: GetTrigger output Trigger; the Trigger shape carries Name, Type, State (TriggerState enum), Description, Schedule, Actions. Recorded: 200 with Type 'SCHEDULED', State 'CREATED', the Schedule and Description as supplied, and Actions echoing the JobName.
Emulator: 400 and no Trigger member.
Fix: the GetTrigger handler / trigger state.
Traces: glue-trigger-schedule-activate-update-deactivate#task#2 (cluster also covers #task#6).

Cause 3 of 6: StartTrigger must activate it and echo the name.
AWS: recorded: 200 with Name = the trigger's name.
Emulator: 400, no Name.
Fix: the StartTrigger handler and the trigger's state transition to ACTIVATED.
Traces: glue-trigger-schedule-activate-update-deactivate#task#3.

Cause 4 of 6: GetTrigger after StartTrigger must report State ACTIVATED.
AWS: recorded: 200 with Trigger.State 'ACTIVATED' (the fixture polls for it).
Emulator: 400; the poll exhausts 30 attempts with no Trigger at all.
Fix: the trigger state machine (CREATED -> ACTIVATED on StartTrigger) and GetTrigger.
Traces: glue-trigger-schedule-activate-update-deactivate#task#4 (cluster also covers #task#8).

Cause 5 of 6: UpdateTrigger must apply the new Description/Schedule and return the updated Trigger.
AWS: recorded: 200 with the full updated Trigger (new Description, new cron Schedule, State still ACTIVATED).
Emulator: 400, no Trigger.
Fix: the UpdateTrigger handler and trigger state update.
Traces: glue-trigger-schedule-activate-update-deactivate#task#5.

Cause 6 of 6: StopTrigger must deactivate it and echo the name.
AWS: recorded: 200 with Name = the trigger's name.
Emulator: 400, no Name.
Fix: the StopTrigger handler and the ACTIVATED -> DEACTIVATED transition.
Traces: glue-trigger-schedule-activate-update-deactivate#task#7.

## Evidence
1. glue-trigger-schedule-activate-update-deactivate#setup#1 CreateTrigger — status expected 200, got 400.
2. #task#2 GetTrigger — status expected 200, got 400; $.Trigger expected {Actions:[{JobName:<job>}], Description:'initial description', Name:<trigger>, Schedule:'cron(0 0 1 1 ? 2099)', State:'CREATED', Type:'SCHEDULED'}, got absent.
3. #task#3 StartTrigger — status expected 200, got 400; $.Name expected the trigger name, got absent.
4. #task#4 GetTrigger — status expected 200, got 400; $.Trigger.State poll: not in ['ACTIVATED'] after 30 attempts, got None.
5. #task#5 UpdateTrigger — status expected 200, got 400; $.Trigger expected the updated trigger (Description 'updated description', Schedule 'cron(0 0 2 1 ? 2099)', State 'ACTIVATED'), got absent.
6. #task#7 StopTrigger — status expected 200, got 400; $.Name expected the trigger name, got absent.
(The trigger/job names and cron values above are one run's values; the requirement is the status, the state transitions and the echoed members.)

## Steps to Reproduce
1. Replay the wire corpus (replay_wire_corpus).
2. Call CreateTrigger with Type='SCHEDULED', Schedule='cron(0 0 1 1 ? 2099)', Actions=[{'JobName':'<a job name that does not exist>'}] and a Description — the emulator answers 400 where AWS answers 200.
3. Then call GetTrigger, StartTrigger, GetTrigger (poll for State ACTIVATED), UpdateTrigger (new description/schedule) and StopTrigger — each answers 400 because the trigger was never stored.
