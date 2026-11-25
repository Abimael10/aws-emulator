Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: kafka__lambda.msk-lambda-esm-filter-criteria-delivers-only-matching-records-and-a-cleared-filter-releases-the-rest kafka__lambda.msk-lambda-event-source-mapping-disabled-delivers-nothing-then-reenabled-delivers-the-missed-records
Issue-Key: kafka__lambda.msk-lambda-esm-filter-criteria-delivers-only-matching-records-and-a-cleared-filter-releases-the-rest|_await_items#5|Timeout|time.sleep(5)
Issue-Key: kafka__lambda.msk-lambda-event-source-mapping-disabled-delivers-nothing-then-reenabled-delivers-the-missed-records|_await_items#3|Timeout|return items
Issue-Commit: 8450e81d4129f303aa1cfd932c016ce4cf12d7e7

# kafka__lambda: an MSK event source mapping never delivers a record, so neither FilterCriteria selection nor the disable/re-enable catch-up ever reaches a verdict (both checks time out at 60 s)

> _Note: this issue was identified against the code base at commit `8450e81d4129f303aa1cfd932c016ce4cf12d7e7`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateEventSourceMapping

## Description
One net-new root cause for this service; both of this service's failing checks are grounded in it and neither is marked OWNED or ALREADY FILED by the suite.

Cause 1 of 1: an Amazon MSK event source mapping on a Lambda function never hands any record to the function. Both checks produce records to a topic on the cluster's advertised SASL/IAM bootstrap endpoint and then wait for the mapping to deliver them; no delivery and no committed-offset progress is ever observable, so each check is killed by the harness's 60-second budget without producing any assertion result at all. Failing checks: kafka__lambda.msk-lambda-esm-filter-criteria-delivers-only-matching-records-and-a-cleared-filter-releases-the-rest and kafka__lambda.msk-lambda-event-source-mapping-disabled-delivers-nothing-then-reenabled-delivers-the-missed-records.

AWS: Smithy (com.amazonaws.lambda) models CreateEventSourceMapping as POST /2015-03-31/event-source-mappings (code 202) and UpdateEventSourceMapping as PUT /2015-03-31/event-source-mappings/{UUID} (code 202); both return EventSourceMappingConfiguration carrying UUID, EventSourceArn, Topics (@length min 1 max 1), AmazonManagedKafkaEventSourceConfig, FilterCriteria, FilterCriteriaError, State, StateTransitionReason and LastProcessingResult, and both model InvalidParameterValueException (400), ResourceConflictException (409), ResourceNotFoundException (404), TooManyRequestsException (429) and ServiceException (500). The model describes only the control plane; what the two recordings add is the delivery contract they assert: for an MSK source Lambda polls the cluster, parses each record's VALUE as JSON for filter evaluation, invokes the function with the matching records and commits the offsets of discarded records too; FilterCriteria={"Filters": []} is accepted and discards every record (an OR over zero filters is never satisfied) while an empty FilterCriteria object clears the criteria; and a mapping updated to Enabled=false delivers nothing while records accumulate, then delivers the accumulated records once it is re-enabled. The filter-update path is asynchronous, which is why each check decides a post-clear record by delivery OR by the committed offset passing it.

Emulator: nothing is ever delivered and nothing ever advances, so the assertions are never reached — the only observable is the harness verdict 'Check timed out after 60.0s' for both checks. The suite prints no intermediate status for either, so the stage at which the pipeline stops is not stated by the recording. Hypothesis: the MSK poller is inert for the same reason several already-filed kafka causes describe — nothing serves the advertised SASL/IAM bootstrap endpoint on port 9098 and the native broker's ApiVersions omits CreateTopics (api key 19), so no topic exists for the mapping to poll; that is a hypothesis, not something either the model or these recordings state.

Why: a client that wires an MSK topic to a Lambda function gets a mapping whose control-plane reads look healthy but whose data plane is silent. Nothing arrives, so the two behaviors most consumers depend on when sizing and gating a Kafka consumer — FilterCriteria pre-filtering (and the fact that a discarded record's offset is still committed, so a cleared filter does NOT re-deliver it) and the disable/re-enable catch-up (no loss while disabled, accumulated records delivered on re-enable) — are both unobservable and silently wrong. There is no error anywhere for the client to act on: no FilterCriteriaError, no LastProcessingResult, no State transition away from Enabled.

Fix: the lambda crate's event-source-mapping poller for Amazon MSK sources (the component that consumes from the cluster named by EventSourceArn/Topics, evaluates FilterCriteria against the record value, invokes the function and commits offsets), together with whatever in the kafka crate must stand up the advertised bootstrap listener and the CreateTopics API the poller needs; the Enabled flag must gate delivery without dropping records, and the mapping's State/LastProcessingResult must report progress.

Traces: kafka__lambda.msk-lambda-esm-filter-criteria-delivers-only-matching-records-and-a-cleared-filter-releases-the-rest, kafka__lambda.msk-lambda-event-source-mapping-disabled-delivers-nothing-then-reenabled-delivers-the-missed-records. Related open issues that do NOT carry either replay key, so neither covers these checks: 20261104T044826Z-kafka-lambda-msk-event-source-mappings-never-deliver-filter-criteria.md and 20261114T205343Z-kafka-lambda-msk-lambda-event-source-mapping-checks-cannot-set-up.md (the latter names the missing CreateTopics api key as a setup blocker).

## Evidence
Suite run: 1982/2850 passed (69.5%). Service kafka__lambda: both of its failing checks fail identically and neither is marked OWNED or ALREADY FILED.

[mismatch] kafka__lambda.msk-lambda-esm-filter-criteria-delivers-only-matching-records-and-a-cleared-filter-releases-the-rest
  Error: Check timed out after 60.0s

[mismatch] kafka__lambda.msk-lambda-event-source-mapping-disabled-delivers-nothing-then-reenabled-delivers-the-missed-records
  Error: Check timed out after 60.0s

Expected (AWS):
- filter check: with FilterCriteria set, only records whose JSON value matches are delivered to the function; non-matching records are discarded but their offsets are committed; UpdateEventSourceMapping with FilterCriteria={"Filters": []} is accepted (202) and then discards every record; an empty FilterCriteria object clears the criteria and subsequent records are delivered (or are decided by the committed offset passing them).
- disable check: UpdateEventSourceMapping Enabled=false -> 202, and records produced while disabled are delivered to nobody; Enabled=true -> 202, and exactly those accumulated records are then delivered, none lost and none duplicated.

Actual (emulator): in both checks no record is ever delivered and no offset ever advances, so neither check reaches any of those assertions; the harness kills each at its 60-second budget. The cluster name, topic name, function name and mapping UUID in any one run are per-run values and are not part of the requirement — the requirement is that records produced to the mapped topic reach the function and that Enabled/FilterCriteria gate that delivery.

## Steps to Reproduce
1. kafka:CreateClusterV2 for an MSK Serverless cluster with SASL/IAM enabled; wait for ACTIVE and read kafka:GetBootstrapBrokers.
2. From inside the cluster's VPC, create a topic on the advertised SASL/IAM endpoint (TLS, port 9098) with a Kafka client (kafka-python), then produce JSON-valued records to it.
3. lambda:CreateFunction (python3.12) plus an execution role granting kafka-cluster:* on the cluster/topic/group and lambda:InvokeFunction to the mapping.
4. lambda:CreateEventSourceMapping with EventSourceArn = the cluster ARN, Topics=[<topic>], StartingPosition=TRIM_HORIZON, AmazonManagedKafkaEventSourceConfig.ConsumerGroupId set, and FilterCriteria selecting a subset of the record values. Wait for State=Enabled.
5. Produce one matching and one non-matching record.
   -> AWS: the function is invoked with the matching record only; the non-matching record's offset is committed.
   -> Emulator: no invocation ever happens; the check waits and is killed at 60 s.
6. lambda:UpdateEventSourceMapping with FilterCriteria={"Filters": []} (accepted, discards everything), then with an empty FilterCriteria object (clears it) and produce again.
   -> AWS: the post-clear record is delivered, or the committed offset passes it.
   -> Emulator: unreachable.
7. Second scenario: lambda:UpdateEventSourceMapping Enabled=false, produce N records, then Enabled=true.
   -> AWS: nothing is delivered while disabled; all N are delivered after re-enable.
   -> Emulator: nothing is delivered in either state; the check is killed at 60 s.
