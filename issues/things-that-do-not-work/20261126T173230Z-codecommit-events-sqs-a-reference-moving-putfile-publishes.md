Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: codecommit__events__sqs.a-commit-refused-for-a-stale-parent-moves-no-reference-and-emits-no-state-change-event
Issue-Key: codecommit__events__sqs.a-commit-refused-for-a-stale-parent-moves-no-reference-and-emits-no-state-change-event|_drain#45|Timeout|resp = sqs.receive_message(QueueUrl=url, MaxNumberOfMessages=10, WaitTimeSeconds=5)
Issue-Commit: 909b19ea020eb5a0e53f1bf3e57b93e5193187d7

# codecommit__events__sqs: a reference-moving PutFile publishes no 'CodeCommit Repository State Change' event, so the stale-parent non-delivery check never gets past its positive control

> _Note: this issue was identified against the code base at commit `909b19ea020eb5a0e53f1bf3e57b93e5193187d7`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutFile

## Description
1 net-new cause behind the codecommit__events__sqs failure. Related open issue: 20261022T171422Z-codecommit-events-sqs-codecommit-createbranch-is-a-501-stub-and-no.md describes the same missing event publication as its Cause 2 and even lists this check in its Issue-Trace-Ids, but the replay report did not mark this failure OWNED by it (its recorded replay key names a different step), so the failure is re-filed here with the key it now fails on; the two should be closed together.

Cause 1 of 1: an accepted CodeCommit write that moves a branch reference emits no EventBridge event, so a consumer of 'CodeCommit Repository State Change' on the default bus never sees anything and a check that must first observe delivery (to then prove NON-delivery for a refused write) burns its whole deadline.

AWS: recorded behavior only — CodeCommit eventing is not in the Smithy model (com-amazonaws-codecommit-codecommit-20150413 models PutFile/GetBranch/GetFile but no events). With no opt-in, CodeCommit publishes to the account's DEFAULT event bus, for every write that moves a reference, an event with source 'aws.codecommit', detail-type 'CodeCommit Repository State Change' and the repository ARN in resources; its detail names event = 'referenceCreated' when the write creates the branch and 'referenceUpdated' when the branch already existed, referenceName = the BARE branch name ('main'), referenceFullName = the full ref ('refs/heads/main') and commitId = the id PutFile returned. A PutFile refused with ParentCommitIdOutdatedException moves no reference and must emit nothing.
Emulator: no such event is ever delivered to an EventBridge rule matching {source: [aws.codecommit], detail-type: [CodeCommit Repository State Change], resources: [<repo arn>]} with an SQS target whose policy allows events.amazonaws.com. The check long-polls the queue after an accepted PutFile and is killed by the harness: 'Check timed out after 60.0s' (the check's own first poll allows 240s). Hypothesis: the codecommit write path publishes no events at all, so the positive-control poll can never be satisfied.
Why: every event-driven CodeCommit integration is dead — a build or mirroring consumer triggered by a push, an audit pipeline, a notification rule. Worse for correctness testing, a consumer cannot distinguish 'no event because the write was refused' (the property under test, and the behavior AWS guarantees) from 'this service never emits events', so the refusal semantics are unobservable.
Fix: event publication in the codecommit crate's reference-moving write path (PutFile, DeleteFile, CreateCommit and the merge operations), emitted onto the EventBridge default bus only AFTER the write is accepted, carrying source/detail-type/resources and the detail members above. The refusal path must emit nothing.
Traces: codecommit__events__sqs.a-commit-refused-for-a-stale-parent-moves-no-reference-and-emits-no-state-change-event.

## Evidence
Check codecommit__events__sqs.a-commit-refused-for-a-stale-parent-moves-no-reference-and-emits-no-state-change-event.

Expected (AWS): after cc.put_file on branch 'main' returns a commitId, at least one SQS message arrives on the rule's target queue holding an EventBridge envelope with source 'aws.codecommit', detail-type 'CodeCommit Repository State Change', the repository ARN in resources, and detail {event: 'referenceCreated' (first commit) / 'referenceUpdated' (subsequent), referenceName: 'main', referenceFullName: 'refs/heads/main', commitId: <the id PutFile returned>}.

Actual (emulator): nothing is ever delivered; the check never passes its positive control and the suite reports 'Error: Check timed out after 60.0s'. Consequently the refusal leg (PutFile with a stale parentCommitId -> ParentCommitIdOutdatedException, branch tip unchanged, no event during a hold of 3x the measured latency) is never reached and is unverified.

## Steps to Reproduce
1. Boot the emulator and point botocore at it.
2. cc.create_repository(repositoryName=R) and note repositoryMetadata.Arn.
3. Create an SQS queue and set a Policy allowing Principal {Service: events.amazonaws.com} sqs:SendMessage on its ARN.
4. events.put_rule(Name=X, EventPattern={'source': ['aws.codecommit'], 'detail-type': ['CodeCommit Repository State Change'], 'resources': [<repo arn>]}, State='ENABLED') and events.put_targets(Rule=X, Targets=[{Id: 'q', Arn: <queue arn>}]) -> FailedEntryCount 0.
5. cc.put_file(repositoryName=R, branchName='main', filePath='seed.txt', fileContent=b'seed\n', commitMessage='seed').
6. sqs.receive_message on the queue (long poll) -> expect an envelope naming the returned commitId within seconds; observe no message at all until the deadline.
7. Re-replay the check id in trace_ids.
