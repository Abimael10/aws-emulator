Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: codecommit__events__sqs.a-commit-refused-for-a-stale-parent-moves-no-reference-and-emits-no-state-change-event
Issue-Key: codecommit__events__sqs.a-commit-refused-for-a-stale-parent-moves-no-reference-and-emits-no-state-change-event|_drain#45|Timeout|resp = sqs.receive_message(QueueUrl=url, MaxNumberOfMessages=10, WaitTimeSeconds=5)
Issue-Commit: 814109981af2f415bf041bb2099ef5ad4140d55d

# codecommit__events__sqs: an accepted PutFile publishes no 'CodeCommit Repository State Change' event to the default EventBridge bus

> _Note: this issue was identified against the code base at commit `814109981af2f415bf041bb2099ef5ad4140d55d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutFile

## Description
One root cause.

=== Cause 1 of 1: a CodeCommit write that moves a reference emits no repository-state-change event, so the positive control of the negative-delivery check can never be satisfied ===
AWS: recorded behavior only — the EventBridge envelope a service emits is not part of the Smithy model (com-amazonaws-codecommit-codecommit-20150413 models PutFile's request/response and its ParentCommitIdOutdatedException only). CodeCommit publishes, with no opt-in, to the DEFAULT event bus an envelope with source 'aws.codecommit', detail-type 'CodeCommit Repository State Change', the repository ARN in the envelope's resources array, and a detail carrying event = 'referenceCreated' for the write that creates the branch and 'referenceUpdated' for a later write to an existing branch, referenceType 'branch', referenceName = the BARE branch name (e.g. main), referenceFullName = the full ref (e.g. refs/heads/main) and commitId = the commit id PutFile returned. A PutFile refused for a stale parentCommitId moves no reference and emits nothing — that absence is the property under test, and it is only gradable once the positive deliveries are observed.
Emulator: an EventBridge rule on {"source":["aws.codecommit"],"detail-type":["CodeCommit Repository State Change"],"resources":[<repo ARN>]} with an SQS target whose queue policy admits events.amazonaws.com receives nothing after an accepted PutFile that creates refs/heads/main and after a second accepted PutFile that moves it. The check long-polls that target and burns the whole harness budget: 'Error: Check timed out after 60.0s'. Hypothesis: the codecommit write path publishes no EventBridge events at all, so the branch-creating poll (the check's own 240s budget) can never be satisfied and the refusal leg is never reached.
Why: every event-driven CodeCommit integration — a build triggered by a push, a mirroring consumer, an audit pipeline — never fires. Worse, a client cannot distinguish 'no event because the write was refused' (AWS's behavior, and the property this check exists to assert) from 'this service never emits events', so a consumer that is supposed to react exactly once per accepted reference change can never be validated against the emulator.
Fix: event publication on reference-moving writes in the codecommit crate's write path (the PutFile / DeleteFile / CreateCommit handlers), emitting the 'CodeCommit Repository State Change' envelope onto the EventBridge default bus only after the write is accepted, with the bare branch name in detail.referenceName and the full ref in detail.referenceFullName.
Related filing: the open issue 20261022T171422Z-codecommit-events-sqs-codecommit-createbranch-is-a-501-stub-and-no.md already names this exact check id in its Issue-Trace-Ids and carries an Issue-Key for this same timeout, but this run's report did not mark the failure as owned, so it is clustered and filed here; a fixer should treat the two as ONE defect and close them together.
Traces: codecommit__events__sqs.a-commit-refused-for-a-stale-parent-moves-no-reference-and-emits-no-state-change-event.

## Evidence
Failing check codecommit__events__sqs.a-commit-refused-for-a-stale-parent-moves-no-reference-and-emits-no-state-change-event (mismatch):

Error: Check timed out after 60.0s

Expected (AWS): within seconds of the branch-creating PutFile, the SQS target of the rule receives one envelope with source 'aws.codecommit', detail-type 'CodeCommit Repository State Change', the repository ARN in resources, detail.event == 'referenceCreated', detail.referenceName == 'main', detail.referenceFullName == 'refs/heads/main' and detail.commitId == the commitId PutFile returned; the second accepted PutFile produces the same envelope with detail.event == 'referenceUpdated'; the PutFile refused with ParentCommitIdOutdatedException produces none. (The branch name, ref, repository ARN and commit ids above are the shape of the template — the concrete repository name and commit ids are per-run values.)
Actual (emulator): no message ever arrives at the target, the first (positive-control) poll runs until the harness kills the check at 60s, and the refusal leg — the actual negative assertion — is never reached.

## Steps to Reproduce
1. Build and boot the emulator from this checkout and point botocore at it.
2. codecommit.create_repository(repositoryName=R).
3. Create an SQS queue whose policy allows sqs:SendMessage for Service events.amazonaws.com; PutRule on the DEFAULT bus with pattern {"source":["aws.codecommit"],"detail-type":["CodeCommit Repository State Change"],"resources":[<repository ARN>]} and PutTargets with that queue.
4. codecommit.put_file(repositoryName=R, branchName='main', filePath='seed.txt', fileContent=b'seed\n', commitMessage='seed') — this creates the branch.
5. Long-poll the queue. Expect one 'CodeCommit Repository State Change' envelope with detail.event == 'referenceCreated' within seconds; observe nothing.
6. Equivalently: replay check codecommit__events__sqs.a-commit-refused-for-a-stale-parent-moves-no-reference-and-emits-no-state-change-event.
